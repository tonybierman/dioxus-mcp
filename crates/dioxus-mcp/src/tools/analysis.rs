use std::path::PathBuf;
use std::sync::Arc;

use proc_macro2::TokenTree;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use syn::visit::Visit;

use crate::state::State;

// ---------- audit_feature_flags ----------

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct AuditFeatureFlagsParams {
    /// Absolute path to the Dioxus project root to inspect.
    /// Defaults to the path the MCP server was started in when omitted.
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Finding {
    pub level: &'static str, // "error" | "warning" | "info"
    pub message: String,
    pub fix: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AuditReport {
    pub ok: bool,
    pub manifest: Option<PathBuf>,
    pub dioxus_version: Option<String>,
    pub dioxus_features: Vec<String>,
    pub has_dioxus_toml: bool,
    pub findings: Vec<Finding>,
}

const PLATFORM_FEATURES: &[&str] = &[
    "web",
    "desktop",
    "mobile",
    "fullstack",
    "server",
    "static-generation",
    "ssr",
];

pub async fn audit_feature_flags(state: &Arc<State>, p: AuditFeatureFlagsParams) -> AuditReport {
    let project = match p.project_root.as_deref() {
        Some(root) => crate::project::ProjectInfo::detect(std::path::Path::new(root)),
        None => state.project.lock().await.clone(),
    };
    let mut findings = Vec::new();

    let Some(manifest) = project.manifest_path.clone() else {
        findings.push(Finding {
            level: "error",
            message: "no Cargo.toml with a `dioxus` dependency was found from the project root"
                .into(),
            fix: Some("run dioxus-mcp with --project-root pointing at a Dioxus crate".into()),
        });
        return AuditReport {
            ok: false,
            manifest: None,
            dioxus_version: None,
            dioxus_features: vec![],
            has_dioxus_toml: false,
            findings,
        };
    };

    if !project.is_dioxus_project {
        findings.push(Finding {
            level: "error",
            message: "manifest does not list `dioxus` as a dependency".into(),
            fix: None,
        });
    }

    // Version sanity
    match project.version_major_minor() {
        Some((0, 7)) => {}
        Some((maj, min)) => findings.push(Finding {
            level: "warning",
            message: format!(
                "detected Dioxus {maj}.{min}; this MCP ships templates and rules for 0.7"
            ),
            fix: Some("upgrade Dioxus to 0.7.x for best results".into()),
        }),
        None => findings.push(Finding {
            level: "warning",
            message: "could not parse the Dioxus version from Cargo.toml".into(),
            fix: None,
        }),
    }

    // Active platform features on the dioxus dep — both directly via
    // `features = [...]` on the dep line AND transitively via the project's
    // own `[features]` table (e.g. `web = ["dioxus/web"]` activated by
    // `default = ["web"]`). Without the [features] walk, projects using
    // cargo feature unification flag false positives like "fullstack
    // enabled but web is not" when web *is* enabled through default.
    let manifest_text = std::fs::read_to_string(&manifest).ok();
    let effective_dioxus_features =
        collect_effective_dioxus_features(&project.dioxus_features, manifest_text.as_deref());
    let active: Vec<&str> = effective_dioxus_features
        .iter()
        .map(|s| s.as_str())
        .filter(|f| PLATFORM_FEATURES.contains(f))
        .collect();

    let has_fullstack = active.contains(&"fullstack");
    let render_targets: Vec<&str> = active
        .iter()
        .copied()
        .filter(|f| matches!(*f, "web" | "desktop" | "mobile"))
        .collect();

    if has_fullstack {
        if !render_targets.contains(&"web") {
            findings.push(Finding {
                level: "warning",
                message: "`fullstack` is enabled but `web` is not — fullstack typically pairs `web` (client) with `server`".into(),
                fix: Some("add `web` to the dioxus features".into()),
            });
        }
        if !active.contains(&"server") {
            findings.push(Finding {
                level: "warning",
                message: "`fullstack` is enabled but `server` is not".into(),
                fix: Some("add `server` to the dioxus features".into()),
            });
        }
    } else if render_targets.len() > 1 {
        findings.push(Finding {
            level: "error",
            message: format!(
                "multiple render targets enabled simultaneously without `fullstack`: {render_targets:?}"
            ),
            fix: Some(
                "pick exactly one of web/desktop/mobile, or enable `fullstack` instead"
                    .into(),
            ),
        });
    } else if render_targets.is_empty() && !active.contains(&"server") {
        findings.push(Finding {
            level: "warning",
            message: "no platform feature enabled on the `dioxus` dep".into(),
            fix: Some(
                "select a platform: features = [\"web\"] (or desktop/mobile/fullstack)".into(),
            ),
        });
    }

    // [features] default = ["web", "server"] footgun
    if let Some(text) = manifest_text.as_deref()
        && let Ok(parsed) = text.parse::<toml::Table>()
        && let Some(features) = parsed.get("features").and_then(|v| v.as_table())
        && let Some(default) = features.get("default").and_then(|v| v.as_array())
    {
        let names: Vec<String> = default
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        let has_web = names.iter().any(|n| n == "web");
        let has_server = names.iter().any(|n| n == "server");
        if has_web && has_server {
            findings.push(Finding {
                            level: "warning",
                            message: "[features] default = [\"web\", \"server\"] activates both render targets at once — a common source of build confusion".into(),
                            fix: Some("set default = [\"web\"] (or [\"server\"]) and pass the other via --features when needed".into()),
                        });
        }
    }

    let ok = !findings.iter().any(|f| f.level == "error");
    AuditReport {
        ok,
        manifest: Some(manifest),
        dioxus_version: project.dioxus_version,
        dioxus_features: effective_dioxus_features,
        has_dioxus_toml: project.has_dioxus_toml,
        findings,
    }
}

/// Compute the effective set of dioxus features for the project, starting
/// from features set directly on the `dioxus` dep line and walking the
/// project's own `[features]` table to follow `dioxus/<name>` indirections.
///
/// Cargo activates `default` automatically (unless `default-features = false`
/// on a downstream crate — but we're inspecting the project itself, so default
/// is in scope). Any named feature reachable from `default` whose value list
/// contains `dioxus/X` contributes `X` to the effective set.
fn collect_effective_dioxus_features(
    direct_dep_features: &[String],
    manifest_text: Option<&str>,
) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut effective: BTreeSet<String> = direct_dep_features.iter().cloned().collect();
    let Some(text) = manifest_text else {
        return effective.into_iter().collect();
    };
    let Ok(parsed) = text.parse::<toml::Table>() else {
        return effective.into_iter().collect();
    };
    let Some(features) = parsed.get("features").and_then(|v| v.as_table()) else {
        return effective.into_iter().collect();
    };

    // Seed the work queue with `default` (cargo activates it by default for
    // path-level builds, which is what `dx serve` and `cargo build` do).
    let mut work: Vec<String> = Vec::new();
    if let Some(default) = features.get("default").and_then(|v| v.as_array()) {
        for v in default {
            if let Some(s) = v.as_str() {
                work.push(s.to_string());
            }
        }
    }

    let mut seen: BTreeSet<String> = BTreeSet::new();
    while let Some(name) = work.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        // Accept both `dioxus/web` and the weak-dep form `dioxus?/web`.
        if let Some(stripped) = name
            .strip_prefix("dioxus/")
            .or_else(|| name.strip_prefix("dioxus?/"))
        {
            effective.insert(stripped.to_string());
            continue;
        }
        if let Some(arr) = features.get(name.as_str()).and_then(|v| v.as_array()) {
            for v in arr {
                if let Some(s) = v.as_str() {
                    work.push(s.to_string());
                }
            }
        }
    }

    effective.into_iter().collect()
}

// ---------- check_rsx ----------

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct CheckRsxParams {
    /// Path to a single Rust file to scan (absolute, or relative to the project
    /// root). One of `file` or `files` must be provided.
    #[serde(default)]
    pub file: Option<String>,
    /// Batch form: multiple file paths. When set, each file is linted and the
    /// per-file breakdown lands in `per_file`; top-level `issues` is the flat
    /// merge across files, with each issue annotated with its `file`.
    #[serde(default)]
    pub files: Option<Vec<String>>,
    /// Absolute path to the Dioxus project root. Required when paths are
    /// relative and the server was not started in the target project directory.
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RsxIssue {
    pub line: usize,
    pub column: usize,
    pub message: String,
    /// Set in batch mode so callers can attribute a merged issue to its file.
    /// Omitted in single-file mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
pub struct CheckRsxFileReport {
    pub file: PathBuf,
    pub rsx_block_count: usize,
    pub issues: Vec<RsxIssue>,
}

#[derive(Debug, Serialize)]
pub struct CheckRsxReport {
    /// In single-file mode, the file scanned. In batch mode, the first file.
    pub file: PathBuf,
    /// Sum of rsx! blocks across all scanned files.
    pub rsx_block_count: usize,
    /// Flat list of issues. In batch mode each carries its `file`.
    pub issues: Vec<RsxIssue>,
    /// Per-file breakdown. Empty in single-file mode.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub per_file: Vec<CheckRsxFileReport>,
}

pub async fn check_rsx(state: &Arc<State>, p: CheckRsxParams) -> Result<CheckRsxReport, String> {
    let mut requested: Vec<String> = Vec::new();
    if let Some(f) = p.file.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        requested.push(f.to_owned());
    }
    if let Some(fs) = &p.files {
        for f in fs {
            let f = f.trim();
            if !f.is_empty() {
                requested.push(f.to_owned());
            }
        }
    }
    if requested.is_empty() {
        return Err("check_rsx: pass `file` (single path) or `files` (list of paths)".into());
    }

    let batch_mode = requested.len() > 1 || p.files.is_some();
    let mut per_file: Vec<CheckRsxFileReport> = Vec::with_capacity(requested.len());
    for spec in &requested {
        let path = resolve_in_project(state, spec, p.project_root.as_deref()).await;
        let report = lint_single_file(&path)?;
        per_file.push(report);
    }

    let first = per_file[0].file.clone();
    let total_blocks: usize = per_file.iter().map(|r| r.rsx_block_count).sum();
    let issues: Vec<RsxIssue> = if batch_mode {
        per_file
            .iter()
            .flat_map(|r| {
                r.issues.iter().map(|i| RsxIssue {
                    line: i.line,
                    column: i.column,
                    message: i.message.clone(),
                    file: Some(r.file.clone()),
                })
            })
            .collect()
    } else {
        per_file[0]
            .issues
            .iter()
            .map(|i| RsxIssue {
                line: i.line,
                column: i.column,
                message: i.message.clone(),
                file: None,
            })
            .collect()
    };

    Ok(CheckRsxReport {
        file: first,
        rsx_block_count: total_blocks,
        issues,
        per_file: if batch_mode { per_file } else { Vec::new() },
    })
}

fn lint_single_file(path: &std::path::Path) -> Result<CheckRsxFileReport, String> {
    let src = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let file = syn::parse_file(&src).map_err(|e| {
        let s = e.span().start();
        format!(
            "rust parse error in {} at line {} col {}: {e}",
            path.display(),
            s.line,
            s.column
        )
    })?;

    struct Visitor<'a> {
        src: &'a str,
        rsx_blocks: usize,
        issues: Vec<RsxIssue>,
    }
    impl<'a, 'ast> Visit<'ast> for Visitor<'a> {
        fn visit_macro(&mut self, m: &'ast syn::Macro) {
            let is_rsx = m
                .path
                .segments
                .last()
                .map(|s| s.ident == "rsx")
                .unwrap_or(false);
            if is_rsx {
                self.rsx_blocks += 1;
                lint_rsx_tokens(m, self.src, &mut self.issues);
            }
            syn::visit::visit_macro(self, m);
        }
    }

    let mut v = Visitor {
        src: &src,
        rsx_blocks: 0,
        issues: Vec::new(),
    };
    v.visit_file(&file);

    Ok(CheckRsxFileReport {
        file: path.to_path_buf(),
        rsx_block_count: v.rsx_blocks,
        issues: v.issues,
    })
}

fn lint_rsx_tokens(m: &syn::Macro, _src: &str, issues: &mut Vec<RsxIssue>) {
    // Heuristic linting on the raw token stream. Real parsing requires the
    // dioxus-rsx crate (which is unstable across versions); this catches the
    // most common 0.7 mistakes without bringing in a full parser.
    let tokens: Vec<TokenTree> = m.tokens.clone().into_iter().collect();
    walk_lint(&tokens, false, issues);
}

/// `parent_is_component` is true when the surrounding brace group is the body
/// of a component invocation (e.g. `MyComp { ... }`). It gates lints that only
/// make sense for HTML element attributes — most notably the `onXxx:` empty-
/// closure check, since a component prop named `onclick:` may legitimately be
/// an `EventHandler<()>` / `Callback<()>` that takes no arguments.
fn walk_lint(tokens: &[TokenTree], parent_is_component: bool, issues: &mut Vec<RsxIssue>) {
    // 1) `for ... { ... }` without a `key:` attribute somewhere in the body.
    //    Find the brace-delim body group specifically — earlier versions
    //    grabbed the first group of any delimiter, which mis-targeted
    //    `for x in xs.iter() { ... }` at the `()` of `.iter()` instead of
    //    the body block (causing false positives).
    let mut i = 0;
    while i < tokens.len() {
        if let TokenTree::Ident(id) = &tokens[i]
            && id == "for"
            && let Some(body_idx) = find_for_body(tokens, i)
        {
            let body_group = match &tokens[body_idx] {
                TokenTree::Group(g) => g,
                _ => unreachable!("find_for_body returned a non-group index"),
            };
            let body_tokens: Vec<TokenTree> = body_group.stream().clone().into_iter().collect();
            if !slice_has_key_attr(&body_tokens) {
                let s = id.span().start();
                issues.push(RsxIssue {
                            line: s.line,
                            column: s.column,
                            message: "loop in rsx! is missing a `key: ...` attribute on its child element — Dioxus needs keys for stable diffing".into(),
                            file: None,
                        });
            }
        }
        i += 1;
    }

    // 2) `onXxx: |..|` or `onXxx: move |..|` with empty closure params.
    //    Only meaningful inside an HTML element body — on component invocations
    //    the prop type may legitimately be a zero-arg callback.
    if !parent_is_component {
        let mut j = 0;
        while j + 2 < tokens.len() {
            let (a, b) = (&tokens[j], &tokens[j + 1]);
            if let (TokenTree::Ident(id), TokenTree::Punct(p)) = (a, b) {
                let name = id.to_string();
                if name.starts_with("on") && name.len() > 2 && p.as_char() == ':' {
                    let mut k = j + 2;
                    if matches!(tokens.get(k), Some(TokenTree::Ident(i)) if i == "move") {
                        k += 1;
                    }
                    let starts_pipe =
                        matches!(tokens.get(k), Some(TokenTree::Punct(q)) if q.as_char() == '|');
                    let ends_pipe = matches!(tokens.get(k + 1), Some(TokenTree::Punct(q)) if q.as_char() == '|');
                    if starts_pipe && ends_pipe {
                        let s = id.span().start();
                        issues.push(RsxIssue {
                            line: s.line,
                            column: s.column,
                            message: format!(
                                "`{name}` handler closure takes no parameters; in Dioxus it should accept an Event, e.g. `{name}: move |evt: Event<MouseData>| {{ ... }}`"
                            ),
                            file: None,
                        });
                    }
                }
            }
            j += 1;
        }
    }

    // Recurse into groups so element/component bodies are scanned. Track the
    // immediately preceding ident so the recursed body knows whether it sits
    // inside a component (uppercase) or an HTML element (lowercase).
    let mut last_ident: Option<&proc_macro2::Ident> = None;
    for tt in tokens {
        match tt {
            TokenTree::Ident(id) => last_ident = Some(id),
            TokenTree::Group(g) => {
                let is_component = last_ident
                    .map(|id| starts_uppercase(&id.to_string()))
                    .unwrap_or(false);
                let inner: Vec<TokenTree> = g.stream().clone().into_iter().collect();
                walk_lint(&inner, is_component, issues);
                last_ident = None;
            }
            _ => last_ident = None,
        }
    }
}

fn starts_uppercase(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

/// Locate the `{ ... }` body group of a `for` loop. Skips groups with other
/// delimiters (e.g. the parens from `for x in xs.iter() { ... }`) so the
/// key-attribute scan inspects the loop's actual child elements.
fn find_for_body(tokens: &[TokenTree], start_after: usize) -> Option<usize> {
    for (k, tt) in tokens.iter().enumerate().skip(start_after + 1) {
        if let TokenTree::Group(g) = tt
            && g.delimiter() == proc_macro2::Delimiter::Brace
        {
            return Some(k);
        }
    }
    None
}

fn slice_has_key_attr(slice: &[TokenTree]) -> bool {
    for w in slice.windows(2) {
        if let (TokenTree::Ident(id), TokenTree::Punct(p)) = (&w[0], &w[1])
            && id == "key"
            && p.as_char() == ':'
        {
            return true;
        }
    }
    for tt in slice {
        if let TokenTree::Group(g) = tt {
            let inner: Vec<TokenTree> = g.stream().clone().into_iter().collect();
            if slice_has_key_attr(&inner) {
                return true;
            }
        }
    }
    false
}

// ---------- explain_signal_graph ----------

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ExplainSignalGraphParams {
    pub file: String,
    /// Optional component name. If omitted, every #[component] in the file is analyzed.
    pub component: Option<String>,
    /// Absolute path to the Dioxus project root. Required when `file` is relative and the
    /// server was not started in the target project directory.
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct SignalNode {
    pub name: String,
    pub kind: String, // "signal" | "memo" | "resource" | "effect"
    pub line: usize,
    pub reads: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ComponentGraph {
    pub component: String,
    pub nodes: Vec<SignalNode>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ExplainSignalGraphReport {
    pub file: PathBuf,
    pub components: Vec<ComponentGraph>,
}

pub async fn explain_signal_graph(
    state: &Arc<State>,
    p: ExplainSignalGraphParams,
) -> Result<ExplainSignalGraphReport, String> {
    let path = resolve_in_project(state, &p.file, p.project_root.as_deref()).await;
    let src = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let file = syn::parse_file(&src).map_err(|e| format!("rust parse error: {e}"))?;

    let mut out = Vec::new();
    for item in &file.items {
        let syn::Item::Fn(f) = item else { continue };
        let is_component = f.attrs.iter().any(|a| {
            a.path()
                .segments
                .last()
                .map(|s| s.ident == "component")
                .unwrap_or(false)
        });
        if !is_component {
            continue;
        }
        let name = f.sig.ident.to_string();
        if let Some(filter) = &p.component
            && &name != filter
        {
            continue;
        }

        let nodes = analyze_component_body(&f.block);
        let warnings = lint_signal_graph(&nodes);
        out.push(ComponentGraph {
            component: name,
            nodes,
            warnings,
        });
    }

    Ok(ExplainSignalGraphReport {
        file: path,
        components: out,
    })
}

fn analyze_component_body(block: &syn::Block) -> Vec<SignalNode> {
    let mut nodes: Vec<SignalNode> = Vec::new();
    let mut known_bindings: Vec<String> = Vec::new();

    for stmt in &block.stmts {
        let syn::Stmt::Local(local) = stmt else {
            continue;
        };
        let Some(init) = &local.init else { continue };
        let kind = classify_init_call(&init.expr);
        let Some(kind) = kind else { continue };

        let binding_name = match &local.pat {
            syn::Pat::Ident(p) => p.ident.to_string(),
            syn::Pat::Type(t) => match &*t.pat {
                syn::Pat::Ident(p) => p.ident.to_string(),
                _ => "<unnamed>".into(),
            },
            _ => "<unnamed>".into(),
        };

        let line = local.let_token.span.start().line;
        let reads = collect_reads(&init.expr, &known_bindings);
        nodes.push(SignalNode {
            name: binding_name.clone(),
            kind: kind.into(),
            line,
            reads,
        });
        known_bindings.push(binding_name);
    }

    nodes
}

fn classify_init_call(expr: &syn::Expr) -> Option<&'static str> {
    let call = match expr {
        syn::Expr::Call(c) => c,
        syn::Expr::MethodCall(m) => return classify_init_call(&m.receiver),
        syn::Expr::Try(t) => return classify_init_call(&t.expr),
        syn::Expr::Await(a) => return classify_init_call(&a.base),
        _ => return None,
    };
    let syn::Expr::Path(p) = &*call.func else {
        return None;
    };
    let last = p.path.segments.last()?.ident.to_string();
    match last.as_str() {
        "use_signal" => Some("signal"),
        "use_memo" => Some("memo"),
        "use_resource" => Some("resource"),
        "use_effect" => Some("effect"),
        _ => None,
    }
}

fn collect_reads(expr: &syn::Expr, known: &[String]) -> Vec<String> {
    struct R<'a> {
        known: &'a [String],
        hits: Vec<String>,
    }
    impl<'a, 'ast> Visit<'ast> for R<'a> {
        fn visit_ident(&mut self, i: &'ast syn::Ident) {
            let s = i.to_string();
            if self.known.iter().any(|k| k == &s) && !self.hits.contains(&s) {
                self.hits.push(s);
            }
        }
    }
    let mut r = R {
        known,
        hits: Vec::new(),
    };
    r.visit_expr(expr);
    r.hits
}

fn lint_signal_graph(nodes: &[SignalNode]) -> Vec<String> {
    let mut out = Vec::new();
    for n in nodes {
        if (n.kind == "memo" || n.kind == "effect") && n.reads.is_empty() {
            out.push(format!(
                "`{}` is a {} that captures no other signals — it will never re-run on state change",
                n.name, n.kind
            ));
        }
    }
    out
}

async fn resolve_in_project(state: &Arc<State>, file: &str, project_root: Option<&str>) -> PathBuf {
    let p = PathBuf::from(file);
    if p.is_absolute() {
        return p;
    }
    let base = if let Some(root) = project_root {
        let info = crate::project::ProjectInfo::detect(std::path::Path::new(root));
        info.manifest_dir().unwrap_or_else(|| PathBuf::from(root))
    } else {
        let project = state.project.lock().await;
        project
            .manifest_dir()
            .unwrap_or_else(|| state.project_root.clone())
    };
    base.join(p)
}

#[cfg(test)]
mod audit_feature_flags_tests {
    use super::collect_effective_dioxus_features;

    #[test]
    fn picks_up_features_routed_through_project_features_table() {
        // A typical `dx new` fullstack starter: dioxus dep declares only
        // `fullstack`, with web/server enabled indirectly through the
        // project's own [features] table. The audit must follow the chain so
        // it doesn't falsely warn that web/server are missing.
        let manifest = r#"[package]
name = "starter"
version = "0.1.0"
edition = "2024"

[features]
default = ["web"]
web = ["dioxus/web"]
server = ["dioxus/server"]

[dependencies]
dioxus = { version = "0.7", features = ["fullstack"] }
"#;
        let effective =
            collect_effective_dioxus_features(&["fullstack".to_string()], Some(manifest));
        assert!(
            effective.contains(&"fullstack".to_string()),
            "{effective:?}"
        );
        assert!(effective.contains(&"web".to_string()), "{effective:?}");
        // `server` should NOT appear: cargo only activates default features
        // and `server` isn't in default. The walk is correct in skipping it.
        assert!(!effective.contains(&"server".to_string()), "{effective:?}");
    }

    #[test]
    fn handles_default_chains_with_intermediate_features() {
        // `default → ssr → dioxus/server` (intermediate feature without a
        // direct dioxus/* entry) — the walker should still resolve it.
        let manifest = r#"[features]
default = ["ssr"]
ssr = ["with_server"]
with_server = ["dioxus/server"]

[dependencies]
dioxus = "0.7"
"#;
        let effective = collect_effective_dioxus_features(&[], Some(manifest));
        assert!(effective.contains(&"server".to_string()), "{effective:?}");
    }

    #[test]
    fn handles_weak_dep_marker() {
        // `dioxus?/web` weak-feature syntax should still contribute `web`.
        let manifest = r#"[features]
default = ["web"]
web = ["dioxus?/web"]

[dependencies]
dioxus = { version = "0.7" }
"#;
        let effective = collect_effective_dioxus_features(&[], Some(manifest));
        assert!(effective.contains(&"web".to_string()), "{effective:?}");
    }

    #[test]
    fn passes_through_direct_dep_features_when_no_features_table() {
        let manifest = r#"[dependencies]
dioxus = { version = "0.7", features = ["fullstack", "web"] }
"#;
        let effective = collect_effective_dioxus_features(
            &["fullstack".to_string(), "web".to_string()],
            Some(manifest),
        );
        assert!(
            effective.contains(&"fullstack".to_string()),
            "{effective:?}"
        );
        assert!(effective.contains(&"web".to_string()), "{effective:?}");
    }
}

#[cfg(test)]
mod check_rsx_tests {
    use super::lint_single_file;

    fn lint(src: &str) -> Vec<String> {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("t.rs");
        std::fs::write(&path, src).unwrap();
        lint_single_file(&path)
            .unwrap()
            .issues
            .into_iter()
            .map(|i| i.message)
            .collect()
    }

    #[test]
    fn detects_missing_key_on_loop_child() {
        let issues = lint(
            r#"use dioxus::prelude::*;
fn t() {
    let _ = rsx! {
        ul {
            for x in vec![1, 2, 3] {
                li { "{x}" }
            }
        }
    };
}
"#,
        );
        assert!(
            issues.iter().any(|m| m.contains("missing a `key:")),
            "expected key warning, got {issues:?}"
        );
    }

    #[test]
    fn no_warning_when_key_on_element_with_other_attrs() {
        // Element body has `key:` mixed with other attributes — the lint
        // recurses into the body group and finds it.
        let issues = lint(
            r#"use dioxus::prelude::*;
fn t() {
    let _ = rsx! {
        table {
            for p in vec![1] {
                tr { key: "{p}", class: "row",
                    td { "{p}" }
                }
            }
        }
    };
}
"#,
        );
        assert!(
            issues.iter().all(|m| !m.contains("missing a `key:")),
            "did not expect a key warning, got {issues:?}"
        );
    }

    #[test]
    fn no_warning_when_for_iterates_via_method_call() {
        // Regression: `for x in xs.iter() { ... }` used to make the lint
        // target the `()` of `.iter()` instead of the body, mis-firing
        // even when the body had a proper `key:` attribute.
        let issues = lint(
            r#"use dioxus::prelude::*;
fn t() {
    let xs: Vec<i32> = vec![];
    let _ = rsx! {
        ul {
            for x in xs.iter() {
                li { key: "{x}", "{x}" }
            }
        }
    };
}
"#,
        );
        assert!(
            issues.iter().all(|m| !m.contains("missing a `key:")),
            "did not expect a key warning, got {issues:?}"
        );
    }

    #[test]
    fn no_warning_when_key_on_component_invocation() {
        // Component invocation `Row { key: "{p.id}", ... }` is also a brace
        // group; the recursion into it finds the key.
        let issues = lint(
            r#"use dioxus::prelude::*;
#[derive(Clone, PartialEq, Props)]
struct RowProps { id: i32 }
fn Row(props: RowProps) -> Element { rsx! {} }
fn t() {
    let _ = rsx! {
        for p in vec![1, 2] {
            Row { key: "{p}", id: p }
        }
    };
}
"#,
        );
        assert!(
            issues.iter().all(|m| !m.contains("missing a `key:")),
            "did not expect a key warning, got {issues:?}"
        );
    }

    #[test]
    fn flags_empty_closure_on_element_event_handler() {
        // HTML element `onclick: || { ... }` is wrong — handlers must accept
        // an Event<MouseData>.
        let issues = lint(
            r#"use dioxus::prelude::*;
fn t() {
    let _ = rsx! {
        button { onclick: |_| {}, "noop" }
        button { onclick: || {}, "bad" }
    };
}
"#,
        );
        assert!(
            issues.iter().any(|m| m.contains("takes no parameters")),
            "expected onclick warning, got {issues:?}"
        );
    }

    #[test]
    fn no_warning_for_zero_arg_closure_on_component_prop() {
        // Component props named `onXxx:` can legitimately be `EventHandler<()>`
        // or `Callback<()>` that take no args. Used to false-positive.
        let issues = lint(
            r#"use dioxus::prelude::*;
#[derive(Clone, PartialEq, Props)]
struct DialogProps { onclose: EventHandler<()> }
#[component]
fn Dialog(props: DialogProps) -> Element { rsx! {} }
fn t() {
    let _ = rsx! {
        Dialog { onclose: move || {} }
    };
}
"#,
        );
        assert!(
            issues.iter().all(|m| !m.contains("takes no parameters")),
            "did not expect onclose warning on a component prop, got {issues:?}"
        );
    }

    #[test]
    fn no_warning_when_key_on_component_with_shorthand_prop() {
        // The exact shape from TODO.md item #1: a component invocation that
        // uses `key:` plus a shorthand prop (`x` resolving to the prop named
        // `x`). The shorthand makes the body `{ key: "{x.id}", x }` — the
        // trailing bare ident must not confuse the key search.
        let issues = lint(
            r#"use dioxus::prelude::*;
#[derive(Clone, PartialEq, Props)]
struct RowProps { x: i32 }
fn Row(props: RowProps) -> Element { rsx! {} }
fn t() {
    let xs: Vec<i32> = vec![];
    let _ = rsx! {
        for x in xs {
            Row { key: "{x}", x }
        }
    };
}
"#,
        );
        assert!(
            issues.iter().all(|m| !m.contains("missing a `key:")),
            "did not expect a key warning, got {issues:?}"
        );
    }

    #[test]
    fn no_warning_when_key_on_component_with_tuple_destructure() {
        // Closer to the shape users actually write: `for (id, label) in items`
        // binding via destructure, then a component invocation with multiple
        // props including `key:`. Guards against regressions where the tuple
        // pattern's parens get mistaken for the loop body.
        let issues = lint(
            r#"use dioxus::prelude::*;
#[derive(Clone, PartialEq, Props)]
struct RowProps { id: i32, label: String }
fn Row(props: RowProps) -> Element { rsx! {} }
fn t() {
    let items: Vec<(i32, String)> = vec![];
    let _ = rsx! {
        div {
            for (id, label) in items {
                Row { key: "{id}", id: id, label: label }
            }
        }
    };
}
"#,
        );
        assert!(
            issues.iter().all(|m| !m.contains("missing a `key:")),
            "did not expect a key warning, got {issues:?}"
        );
    }
}
