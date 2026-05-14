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

    // Active platform features on the dioxus dep
    let active: Vec<&str> = project
        .dioxus_features
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
    if let Ok(text) = std::fs::read_to_string(&manifest)
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
        dioxus_features: project.dioxus_features,
        has_dioxus_toml: project.has_dioxus_toml,
        findings,
    }
}

// ---------- check_rsx ----------

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct CheckRsxParams {
    /// Path to a Rust file to scan (absolute, or relative to the project root).
    pub file: String,
    /// Absolute path to the Dioxus project root. Required when `file` is relative and the
    /// server was not started in the target project directory.
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RsxIssue {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct CheckRsxReport {
    pub file: PathBuf,
    pub rsx_block_count: usize,
    pub issues: Vec<RsxIssue>,
}

pub async fn check_rsx(state: &Arc<State>, p: CheckRsxParams) -> Result<CheckRsxReport, String> {
    let path = resolve_in_project(state, &p.file, p.project_root.as_deref()).await;
    let src = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let file = syn::parse_file(&src).map_err(|e| {
        let s = e.span().start();
        format!("rust parse error at line {} col {}: {e}", s.line, s.column)
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

    Ok(CheckRsxReport {
        file: path,
        rsx_block_count: v.rsx_blocks,
        issues: v.issues,
    })
}

fn lint_rsx_tokens(m: &syn::Macro, _src: &str, issues: &mut Vec<RsxIssue>) {
    // Heuristic linting on the raw token stream. Real parsing requires the
    // dioxus-rsx crate (which is unstable across versions); this catches the
    // most common 0.7 mistakes without bringing in a full parser.
    let tokens: Vec<TokenTree> = m.tokens.clone().into_iter().collect();
    walk_lint(&tokens, issues);
}

fn walk_lint(tokens: &[TokenTree], issues: &mut Vec<RsxIssue>) {
    // 1) `for ... { ... }` without a `key:` attribute somewhere in the body.
    let mut i = 0;
    while i < tokens.len() {
        if let TokenTree::Ident(id) = &tokens[i]
            && id == "for"
            && let Some(brace_idx) = find_matching_brace(tokens, i)
        {
            let body_slice = &tokens[i + 1..=brace_idx];
            if !slice_has_key_attr(body_slice) {
                let s = id.span().start();
                issues.push(RsxIssue {
                            line: s.line,
                            column: s.column,
                            message: "loop in rsx! is missing a `key: ...` attribute on its child element — Dioxus needs keys for stable diffing".into(),
                        });
            }
        }
        i += 1;
    }

    // 2) `onXxx: |..|` or `onXxx: move |..|` with empty closure params.
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
                let ends_pipe =
                    matches!(tokens.get(k + 1), Some(TokenTree::Punct(q)) if q.as_char() == '|');
                if starts_pipe && ends_pipe {
                    let s = id.span().start();
                    issues.push(RsxIssue {
                        line: s.line,
                        column: s.column,
                        message: format!(
                            "`{name}` handler closure takes no parameters; in Dioxus it should accept an Event, e.g. `{name}: move |evt: Event<MouseData>| {{ ... }}`"
                        ),
                    });
                }
            }
        }
        j += 1;
    }

    // Recurse into groups so element bodies are scanned.
    for tt in tokens {
        if let TokenTree::Group(g) = tt {
            let inner: Vec<TokenTree> = g.stream().clone().into_iter().collect();
            walk_lint(&inner, issues);
        }
    }
}

fn find_matching_brace(tokens: &[TokenTree], start_after: usize) -> Option<usize> {
    for (k, tt) in tokens.iter().enumerate().skip(start_after + 1) {
        if let TokenTree::Group(_) = tt {
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
