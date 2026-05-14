//! End-to-end tests for every dioxus-mcp tool.
//!
//! Each test spawns the dioxus-mcp binary over stdio, sends an MCP
//! initialize + tools/call sequence, and asserts on the parsed JSON
//! response against fixtures under `tests/fixtures/sample-project/`.
//!
//! Live-HTTP tools (`search_docs`, `find_example`) are `#[ignore]`d so
//! `cargo test` passes offline. Run them with `cargo test -- --ignored`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

// ---------- helpers ----------

fn bin_path() -> &'static str {
    env!("CARGO_BIN_EXE_dioxus-mcp")
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample-project")
}

/// Spawn the binary, send a `tools/call`, return the parsed inner JSON.
fn call_tool_at(project_root: &Path, tool: &str, mut args: Value) -> Value {
    if args.get("project_root").is_none() {
        args["project_root"] = json!(project_root.to_string_lossy());
    }
    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "integration", "version": "0"}
        }
    });
    let init_done = json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {}
    });
    let call = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {"name": tool, "arguments": args}
    });

    let mut child = Command::new(bin_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn dioxus-mcp");

    {
        let stdin = child.stdin.as_mut().expect("stdin");
        writeln!(stdin, "{init}").unwrap();
        writeln!(stdin, "{init_done}").unwrap();
        writeln!(stdin, "{call}").unwrap();
    }
    // Closing stdin lets the server's stdio loop terminate after the response.
    drop(child.stdin.take());

    let output = child.wait_with_output().expect("wait");
    let stdout = String::from_utf8(output.stdout).expect("utf8");

    for line in stdout.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("id").and_then(|x| x.as_i64()) != Some(2) {
            continue;
        }
        let text = v
            .get("result")
            .and_then(|r| r.get("content"))
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or_else(|| panic!("no result.content[0].text in: {v}"));
        return serde_json::from_str(text)
            .unwrap_or_else(|e| panic!("text wasn't JSON: {e}\n--- text ---\n{text}"));
    }
    panic!("no response with id=2; stdout was:\n{stdout}");
}

fn call_tool(tool: &str, args: Value) -> Value {
    call_tool_at(&fixture_root(), tool, args)
}

/// Recursively copy the fixture into a fresh tempdir so scaffolding
/// tools can mutate it without dirtying the checked-in tree.
fn copy_fixture_to_temp() -> tempfile::TempDir {
    let src = fixture_root();
    let dst = tempfile::tempdir().expect("tempdir");
    copy_dir(&src, dst.path()).expect("copy fixture");
    dst
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&path, &target)?;
        } else {
            std::fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

// ---------- tests ----------

#[test]
fn tool_project_tour() {
    let r = call_tool("project_tour", json!({}));
    let summary = r["summary"].as_str().unwrap();
    assert!(summary.contains("Project tour"), "summary: {summary}");
    assert!(r["audit"].is_object(), "audit missing");
    assert!(r["routes"].is_object(), "routes missing");
    assert!(r["index"].is_object(), "index missing");
    assert!(r["assets"].is_object(), "assets missing");
}

#[test]
fn tool_route_map() {
    let r = call_tool("route_map", json!({}));
    assert_eq!(r["enum_name"], "Route");
    let routes = r["routes"].as_array().unwrap();
    let home = routes
        .iter()
        .find(|x| x["component"] == "Home")
        .expect("Home route");
    assert_eq!(home["layouts"], json!(["NavBar"]));
    let blog_post = routes
        .iter()
        .find(|x| x["component"] == "BlogPost")
        .expect("BlogPost route");
    assert_eq!(blog_post["full_path"], "/blog/:slug");
}

#[test]
fn tool_project_index() {
    let r = call_tool("project_index", json!({}));
    let names: Vec<&str> = r["components"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    for expected in ["App", "Home", "Child", "UserPage", "Unused"] {
        assert!(
            names.contains(&expected),
            "components missing {expected}: {names:?}"
        );
    }
    let server_names: Vec<&str> = r["server_fns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(server_names.contains(&"fetch_user"));
    assert!(server_names.contains(&"orphan_fn"));
}

#[test]
fn tool_server_fn_call_graph() {
    let r = call_tool("server_fn_call_graph", json!({}));
    let edges = r["edges"].as_array().unwrap();
    let fetch_edge = edges
        .iter()
        .find(|e| e["server_fn"] == "fetch_user")
        .expect("fetch_user call site");
    assert_eq!(fetch_edge["enclosing_fn"], "UserPage");
    let orphans: Vec<&str> = r["orphans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["name"].as_str().unwrap())
        .collect();
    assert!(orphans.contains(&"orphan_fn"), "orphans: {orphans:?}");
}

#[test]
fn tool_dead_components() {
    // Args exactly mirror the TOOLS.md example call. "RootLayout" is a no-op
    // extra root (not in the fixture); the tool still flags `Unused`.
    let r = call_tool("dead_components", json!({"roots": ["RootLayout"]}));
    let dead: Vec<&str> = r["dead"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    assert!(
        dead.contains(&"Unused"),
        "dead set should include Unused: {dead:?}"
    );
    assert!(!dead.contains(&"Home"), "Home is route-reachable: {dead:?}");
    assert!(
        !dead.contains(&"NavBar"),
        "NavBar is a layout root: {dead:?}"
    );
}

#[test]
fn tool_asset_audit() {
    // Args mirror the TOOLS.md example. `public` doesn't exist in the
    // fixture; the tool silently ignores missing asset dirs.
    let r = call_tool("asset_audit", json!({"assets_dirs": ["assets", "public"]}));
    let unreferenced: Vec<&str> = r["unreferenced_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect();
    assert!(
        unreferenced.contains(&"assets/orphan.css"),
        "unreferenced: {unreferenced:?}"
    );
    let missing = r["missing_assets"].as_array().unwrap();
    assert!(
        missing.iter().any(|m| m["path"] == "/assets/missing.svg"),
        "missing_assets: {missing:?}"
    );
}

#[test]
fn tool_check_rsx() {
    let r = call_tool("check_rsx", json!({"file": "src/lint_demo.rs"}));
    assert!(r["rsx_block_count"].as_u64().unwrap() > 0);
    let issues = r["issues"].as_array().unwrap();
    assert!(!issues.is_empty(), "expected issues, got {issues:?}");
    let any_key = issues
        .iter()
        .any(|i| i["message"].as_str().unwrap_or("").contains("key:"));
    let any_handler = issues.iter().any(|i| {
        i["message"]
            .as_str()
            .unwrap_or("")
            .contains("no parameters")
    });
    assert!(any_key, "expected a missing-key issue: {issues:?}");
    assert!(any_handler, "expected an empty-handler issue: {issues:?}");
}

#[test]
fn tool_signal_lint() {
    let r = call_tool("signal_lint", json!({}));
    let issues = r["issues"].as_array().unwrap();
    assert!(
        issues
            .iter()
            .any(|i| i["code"] == "hook_in_loop" && i["component"] == "Home"),
        "expected hook_in_loop for Home: {issues:?}"
    );
}

#[test]
fn tool_props_lint() {
    let r = call_tool("props_lint", json!({}));
    let issues = r["issues"].as_array().unwrap();
    assert!(
        issues
            .iter()
            .any(|i| i["struct_name"] == "ChildProps" && i["code"] == "props_missing_partial_eq"),
        "expected ChildProps in props_missing_partial_eq: {issues:?}"
    );
}

#[test]
fn tool_prop_drill() {
    let r = call_tool("prop_drill", json!({}));
    let parents = r["parents"].as_array().unwrap();
    let home = parents
        .iter()
        .find(|p| p["component"] == "Home")
        .expect("Home parent");
    let pts = home["passthroughs"].as_array().unwrap();
    assert!(
        pts.iter()
            .any(|p| p["parent_prop"] == "title" && p["via"] == "clone"),
        "title→Child via clone missing: {pts:?}"
    );
    assert!(
        pts.iter()
            .any(|p| p["parent_prop"] == "user_id" && p["via"] == "direct"),
        "user_id direct missing: {pts:?}"
    );
}

#[test]
fn tool_audit_feature_flags() {
    let r = call_tool("audit_feature_flags", json!({}));
    assert_eq!(r["ok"], true, "audit findings: {:#?}", r["findings"]);
    assert_eq!(r["dioxus_version"], "0.7");
    let features: Vec<&str> = r["dioxus_features"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect();
    assert!(features.contains(&"fullstack"));
    assert!(features.contains(&"web"));
    assert!(features.contains(&"server"));
}

#[test]
fn tool_explain_signal_graph() {
    let r = call_tool(
        "explain_signal_graph",
        json!({"file": "src/components/home.rs"}),
    );
    let comps = r["components"].as_array().unwrap();
    let home = comps
        .iter()
        .find(|c| c["component"] == "Home")
        .expect("Home graph");
    let nodes = home["nodes"].as_array().unwrap();
    assert!(
        nodes.iter().any(|n| n["kind"] == "signal"),
        "expected at least one signal node: {nodes:?}"
    );
    assert!(
        nodes.iter().any(|n| n["kind"] == "memo"),
        "expected at least one memo node: {nodes:?}"
    );
}

#[test]
fn tool_create_component() {
    // Args mirror the TOOLS.md example call exactly.
    let tmp = copy_fixture_to_temp();
    let r = call_tool_at(
        tmp.path(),
        "create_component",
        json!({
            "name": "UserCard",
            "props": [
                {"name": "id", "type": "i32"},
                {"name": "label", "type": "String", "optional": true}
            ]
        }),
    );
    let created = r["files_created"].as_array().unwrap();
    assert!(
        created
            .iter()
            .any(|p| p.as_str().unwrap().ends_with("user_card.rs")),
        "files_created: {created:?}"
    );
    assert!(tmp.path().join("src/components/user_card.rs").exists());
    let mod_rs = std::fs::read_to_string(tmp.path().join("src/components/mod.rs")).unwrap();
    assert!(mod_rs.contains("pub mod user_card"), "mod.rs: {mod_rs}");
}

#[test]
fn tool_create_route() {
    // Args mirror the TOOLS.md example call exactly.
    let tmp = copy_fixture_to_temp();
    let r = call_tool_at(
        tmp.path(),
        "create_route",
        json!({"path": "/settings", "component": "Settings"}),
    );
    let modified = r["files_modified"].as_array().unwrap();
    assert!(
        modified
            .iter()
            .any(|p| p.as_str().unwrap().ends_with("router.rs")),
        "files_modified: {modified:?}"
    );
    let router = std::fs::read_to_string(tmp.path().join("src/router.rs")).unwrap();
    assert!(router.contains("/settings"), "router.rs: {router}");
    assert!(router.contains("Settings"), "router.rs: {router}");
}

#[test]
fn tool_create_server_fn() {
    // Args mirror the TOOLS.md example call exactly.
    let tmp = copy_fixture_to_temp();
    let r = call_tool_at(
        tmp.path(),
        "create_server_fn",
        json!({
            "name": "fetch_users",
            "args": [{"name": "limit", "type": "u32"}],
            "return_type": "Vec<User>"
        }),
    );
    let created = r["files_created"].as_array().unwrap();
    assert!(
        created
            .iter()
            .any(|p| p.as_str().unwrap().ends_with("fetch_users.rs")),
        "files_created: {created:?}"
    );
    assert!(tmp.path().join("src/server/fetch_users.rs").exists());
}

#[test]
fn tool_openapi_spec() {
    let r = call_tool("openapi_spec", json!({"include_routes": true}));
    let spec = &r["spec"];
    assert_eq!(spec["openapi"], "3.1.0");

    let paths = spec["paths"].as_object().unwrap();

    // FetchUser: explicit name from #[server(FetchUser)] → /api/FetchUser.
    let fetch = paths
        .get("/api/FetchUser")
        .unwrap_or_else(|| panic!("paths: {paths:?}"));
    let fetch_req = &fetch["post"]["requestBody"]["content"]["application/json"]["schema"];
    assert_eq!(fetch_req["properties"]["id"]["type"], "integer");
    let fetch_resp = &fetch["post"]["responses"]["200"]["content"]["application/json"]["schema"];
    assert_eq!(fetch_resp["type"], "string");

    // ListPosts: input + response should $ref local structs the resolver walked.
    let list = paths
        .get("/api/ListPosts")
        .unwrap_or_else(|| panic!("paths: {paths:?}"));
    let list_req = &list["post"]["requestBody"]["content"]["application/json"]["schema"];
    assert_eq!(
        list_req["properties"]["input"]["$ref"],
        "#/components/schemas/ListPostsInput"
    );
    let list_resp = &list["post"]["responses"]["200"]["content"]["application/json"]["schema"];
    assert_eq!(list_resp["type"], "array");
    assert_eq!(list_resp["items"]["$ref"], "#/components/schemas/Post");

    let schemas = spec["components"]["schemas"].as_object().unwrap();
    let input_schema = schemas
        .get("ListPostsInput")
        .expect("ListPostsInput schema emitted");
    let required: Vec<&str> = input_schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(required.contains(&"limit"), "required: {required:?}");
    assert!(
        !required.contains(&"cursor"),
        "Option<...> should be optional: {required:?}"
    );
    assert!(
        schemas.contains_key("Post"),
        "components.schemas missing Post: {:?}",
        schemas.keys().collect::<Vec<_>>()
    );
    assert!(
        schemas.contains_key("ServerFnError"),
        "components.schemas missing ServerFnError",
    );

    // Route emission: BlogPost variant has a :slug — should become {slug} in the path.
    assert!(
        paths.contains_key("/blog/{slug}"),
        "route path keys: {:?}",
        paths.keys().collect::<Vec<_>>()
    );

    // Server fns without #[server(Name)] aren't in our fixture, so guessed_paths is empty.
    assert!(
        r["guessed_paths"].as_array().unwrap().is_empty(),
        "guessed_paths: {:?}",
        r["guessed_paths"]
    );
}

#[test]
fn tool_runtime_events() {
    // Absolute path to the hand-crafted fixture log. `since` is pinned to
    // 1970 so the default-5-minute window doesn't filter the 2099 events out.
    let log = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/runtime_events/events.jsonl");
    let log_str = log.to_string_lossy().to_string();
    let since = "1970-01-01T00:00:00Z";

    // No kind filter -> every event in the fixture (9 lines).
    let r = call_tool(
        "runtime_events",
        json!({"log_path": log_str, "since": since}),
    );
    let events = r["events"].as_array().unwrap();
    assert_eq!(events.len(), 9, "events: {events:?}");
    assert_eq!(r["truncated"], false);

    // Kind filter.
    let renders = call_tool(
        "runtime_events",
        json!({"log_path": log_str, "since": since, "kind": "render"}),
    );
    let render_events = renders["events"].as_array().unwrap();
    assert_eq!(render_events.len(), 4);
    assert!(render_events.iter().all(|e| e["kind"] == "render"));

    // Component filter narrows further.
    let home_renders = call_tool(
        "runtime_events",
        json!({"log_path": log_str, "since": since, "kind": "render", "component": "Home"}),
    );
    let hr = home_renders["events"].as_array().unwrap();
    assert_eq!(hr.len(), 2);
    assert!(hr.iter().all(|e| e["component"] == "Home"));

    // server_fn name filter.
    let sf = call_tool(
        "runtime_events",
        json!({"log_path": log_str, "since": since, "kind": "server_fn", "server_fn": "fetch_user"}),
    );
    let sf_events = sf["events"].as_array().unwrap();
    assert_eq!(sf_events.len(), 2);

    // Limit -> truncated flag set.
    let capped = call_tool(
        "runtime_events",
        json!({"log_path": log_str, "since": since, "limit": 3}),
    );
    assert_eq!(capped["events"].as_array().unwrap().len(), 3);
    assert_eq!(capped["truncated"], true);

    // since cutoff in the future -> empty + a note.
    let future = call_tool(
        "runtime_events",
        json!({"log_path": log_str, "since": "2999-01-01T00:00:00Z"}),
    );
    assert!(future["events"].as_array().unwrap().is_empty());
    assert!(
        future["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n.as_str().unwrap_or("").contains("no events matched")),
        "notes: {}",
        future["notes"]
    );

    // Missing log path -> empty list + clear note (no error).
    let missing = call_tool(
        "runtime_events",
        json!({"log_path": "/nonexistent/dioxus-mcp/events.jsonl"}),
    );
    assert!(missing["events"].as_array().unwrap().is_empty());
    assert!(
        missing["notes"].as_array().unwrap().iter().any(|n| n
            .as_str()
            .unwrap_or("")
            .contains("install dioxus-mcp-probe")),
        "notes: {}",
        missing["notes"]
    );
}

#[test]
fn tool_server_fn_summary() {
    let log = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/server_fn_summary/events.jsonl");
    let log_str = log.to_string_lossy().to_string();
    let since = "1970-01-01T00:00:00Z";

    let r = call_tool(
        "server_fn_summary",
        json!({"log_path": log_str, "since": since}),
    );
    let summaries = r["summaries"].as_array().unwrap();

    // Most-called first: fetch_user (10) then save_post (2).
    assert_eq!(summaries[0]["name"], "fetch_user");
    assert_eq!(summaries[1]["name"], "save_post");

    let fu = &summaries[0];
    assert_eq!(fu["completed"]["count"], 10);
    assert_eq!(fu["completed"]["ok"], 10);
    assert_eq!(fu["completed"]["err"], 0);
    assert_eq!(fu["completed"]["min_us"], 100);
    assert_eq!(fu["completed"]["max_us"], 1000);
    // Linear samples 100..1000 step 100. Nearest-rank: p50 -> index round(0.50*9)=5 -> 600.
    assert_eq!(fu["completed"]["p50_us"], 600);
    // p95 -> index round(0.95*9)=9 -> 1000.
    assert_eq!(fu["completed"]["p95_us"], 1000);
    // One start without an end remains pending.
    assert_eq!(fu["pending"], 1);

    let sp = &summaries[1];
    assert_eq!(sp["completed"]["count"], 2);
    assert_eq!(sp["completed"]["ok"], 1);
    assert_eq!(sp["completed"]["err"], 1);

    // Filter to one server_fn -> single-row report.
    let only = call_tool(
        "server_fn_summary",
        json!({"log_path": log_str, "since": since, "server_fn": "save_post"}),
    );
    let only_rows = only["summaries"].as_array().unwrap();
    assert_eq!(only_rows.len(), 1);
    assert_eq!(only_rows[0]["name"], "save_post");

    // Missing-log path -> empty list + clear note, not an error.
    let missing = call_tool(
        "server_fn_summary",
        json!({"log_path": "/nonexistent/path/events.jsonl"}),
    );
    assert!(missing["summaries"].as_array().unwrap().is_empty());
    assert!(missing["notes"].as_array().unwrap().iter().any(|n| {
        n.as_str()
            .unwrap_or("")
            .contains("install dioxus-mcp-probe")
    }));
}

#[test]
#[ignore = "requires network access to dioxuslabs.com"]
fn tool_search_docs() {
    let r = call_tool("search_docs", json!({"query": "use_resource"}));
    let hits = r
        .get("hits")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("expected hits array, got: {r}"));
    assert!(!hits.is_empty(), "expected at least one hit, got: {r}");
    let top = &hits[0];
    assert!(top.get("title").is_some(), "hit missing title: {top}");
    assert!(top.get("url").is_some(), "hit missing url: {top}");
    assert!(top.get("snippet").is_some(), "hit missing snippet: {top}");
}

#[test]
#[ignore = "requires network access to github.com"]
fn tool_find_example() {
    let r = call_tool("find_example", json!({"concept": "fullstack"}));
    assert!(r.is_object(), "expected object response, got: {r}");
}
