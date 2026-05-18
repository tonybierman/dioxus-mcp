//! `presence_map_unbounded`: flag `static <MAP>: Lazy<Mutex<HashMap<…>>>`
//! that takes `.insert(...)` writes from server fn bodies but has no
//! reachable eviction call (`.retain()`, `.remove()`, `.clear()`).
//!
//! Pattern: a server-side presence / session map that monotonically
//! accumulates entries on each request. Without a TTL sweep the map grows
//! forever; long-running servers will exhaust memory. The fix is usually a
//! TTL filter on read OR a periodic sweep that removes stale entries.
//!
//! Severity is `info` — many maps are deliberately append-only (caches
//! with bounded keyspace, etc.). The lint is a nudge for the reviewer, not
//! a hard error.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use quote::ToTokens;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use syn::visit::Visit;

use crate::state::State;
use crate::tools::ast::{ParseError, collect_parse_errors, walk_rs_files};
use crate::tools::scaffold::crate_root;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct PresenceMapUnboundedParams {
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PresenceMapFinding {
    pub code: &'static str,
    pub severity: &'static str,
    pub file: PathBuf,
    pub line: usize,
    /// Binding name of the static map (e.g. `PRESENCE`, `SESSIONS`).
    pub binding: String,
    /// Type of the map as it appears in the static declaration — useful so
    /// the suggestion can name the right value type.
    pub map_type: String,
    /// Server fns observed inserting into the map. Surfaced so the reviewer
    /// can see at a glance where new entries land.
    pub insert_sites: Vec<InsertSite>,
    pub message: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct InsertSite {
    pub server_fn: String,
    pub file: PathBuf,
    pub line: usize,
}

#[derive(Debug, Serialize)]
pub struct PresenceMapUnboundedReport {
    pub findings: Vec<PresenceMapFinding>,
    pub parse_errors: Vec<ParseError>,
}

pub async fn presence_map_unbounded(
    state: &Arc<State>,
    p: PresenceMapUnboundedParams,
) -> Result<PresenceMapUnboundedReport, String> {
    let root = crate_root(state, p.project_root.as_deref()).await?;
    let src_root = root.join("src");
    let files = walk_rs_files(&src_root);

    struct StaticMap {
        binding: String,
        ty: String,
        file: PathBuf,
        line: usize,
    }
    let mut statics: Vec<StaticMap> = Vec::new();
    for sf in &files {
        let Ok(ast) = &sf.ast else { continue };
        for item in &ast.items {
            let syn::Item::Static(s) = item else { continue };
            let ty = s.ty.to_token_stream().to_string();
            if !is_lazy_mutex_hashmap(&ty) {
                continue;
            }
            statics.push(StaticMap {
                binding: s.ident.to_string(),
                ty: tighten_ws(&ty),
                file: sf.path.clone(),
                line: s.ident.span().start().line,
            });
        }
    }
    if statics.is_empty() {
        return Ok(PresenceMapUnboundedReport {
            findings: Vec::new(),
            parse_errors: collect_parse_errors(&files),
        });
    }

    let binding_set: HashSet<String> = statics.iter().map(|s| s.binding.clone()).collect();

    // Walk every server fn body, accumulate per-binding insert sites and
    // a flag for any eviction call on that binding. We use the same
    // `is_server_fn` predicate as the blocking-locks lint — both legacy
    // `#[server]` and the verb-macro shapes count.
    let mut inserts: HashMap<String, Vec<InsertSite>> = HashMap::new();
    let mut evictions: HashSet<String> = HashSet::new();
    for sf in &files {
        let Ok(ast) = &sf.ast else { continue };
        for item in &ast.items {
            let syn::Item::Fn(f) = item else { continue };
            if !is_server_fn(f) {
                continue;
            }
            let server_fn_name = f.sig.ident.to_string();
            let mut v = MapUsageVisitor {
                targets: &binding_set,
                inserts: HashMap::new(),
                evictions: HashSet::new(),
                aliases: HashMap::new(),
            };
            v.visit_block(&f.block);
            for (binding, lines) in v.inserts {
                for line in lines {
                    inserts.entry(binding.clone()).or_default().push(InsertSite {
                        server_fn: server_fn_name.clone(),
                        file: sf.path.clone(),
                        line,
                    });
                }
            }
            for binding in v.evictions {
                evictions.insert(binding);
            }
        }
    }

    let mut findings: Vec<PresenceMapFinding> = Vec::new();
    for s in &statics {
        let Some(insert_sites) = inserts.get(&s.binding) else {
            continue;
        };
        if evictions.contains(&s.binding) {
            continue;
        }
        let n = insert_sites.len();
        findings.push(PresenceMapFinding {
            code: "presence_map_unbounded",
            severity: "info",
            file: s.file.clone(),
            line: s.line,
            binding: s.binding.clone(),
            map_type: s.ty.clone(),
            insert_sites: insert_sites.clone(),
            message: format!(
                "`{binding}: {ty}` grows on every request — server fns insert into it \
                 ({n} site{s}) but no `.retain()` / `.remove()` / `.clear()` call is \
                 reachable from any server fn. Long-running servers will accumulate \
                 entries forever. Add a TTL sweep (`map.retain(|_, (ts, _)| ts.elapsed() < TTL)`) \
                 to a periodic task or filter expired entries on read AND evict them \
                 there; consider `dashmap` + `mini-moka` for a TTL-aware drop-in.",
                binding = s.binding,
                ty = s.ty,
                s = if n == 1 { "" } else { "s" },
            ),
        });
    }

    Ok(PresenceMapUnboundedReport {
        findings,
        parse_errors: collect_parse_errors(&files),
    })
}

/// True for `Lazy<Mutex<HashMap<…>>>` and the common `parking_lot` /
/// `RwLock` variants. We deliberately accept fully-qualified paths (e.g.
/// `once_cell::sync::Lazy<…>`) because real code often imports the
/// types fully-qualified.
fn is_lazy_mutex_hashmap(ty: &str) -> bool {
    let normalized = ty.replace(' ', "");
    let has_lazy = normalized.contains("Lazy<") || normalized.contains("OnceLock<");
    let has_lock = normalized.contains("Mutex<") || normalized.contains("RwLock<");
    let has_map = normalized.contains("HashMap<")
        || normalized.contains("BTreeMap<")
        || normalized.contains("DashMap<");
    has_lazy && has_lock && has_map
}

fn tighten_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

fn is_server_fn(f: &syn::ItemFn) -> bool {
    f.attrs.iter().any(|a| {
        let last = a.path().segments.last().map(|s| s.ident.to_string());
        matches!(
            last.as_deref(),
            Some("server" | "get" | "post" | "put" | "delete" | "patch")
        )
    })
}

struct MapUsageVisitor<'a> {
    targets: &'a HashSet<String>,
    /// (binding_name -> [line of insert]).
    inserts: HashMap<String, Vec<usize>>,
    /// Set of binding_names where an eviction call appears.
    evictions: HashSet<String>,
    /// Local aliases for a tracked static (e.g. `let mut presence =
    /// PRESENCE.lock().unwrap();` adds `presence -> PRESENCE`). Real server
    /// fn bodies almost never call `.insert(...)` directly on the static —
    /// they go through a locked guard binding. Without this map we'd miss
    /// the canonical shape entirely.
    aliases: HashMap<String, String>,
}

impl<'a> MapUsageVisitor<'a> {
    /// Walk `MAP.lock().unwrap()` / `MAP.write().unwrap()` /
    /// `MAP.lock()` / `MAP.read()` chains down to the root ident. If the
    /// root is a tracked static, return its binding name.
    fn resolves_to_tracked_static(&self, expr: &syn::Expr) -> Option<String> {
        let name = receiver_root_ident(expr)?;
        if self.targets.contains(&name) {
            Some(name)
        } else if let Some(target) = self.aliases.get(&name) {
            Some(target.clone())
        } else {
            None
        }
    }
}

impl<'a, 'ast> Visit<'ast> for MapUsageVisitor<'a> {
    fn visit_local(&mut self, local: &'ast syn::Local) {
        // Alias detection: `let [mut] X = MAP.lock().unwrap()` (or .read()
        // / .write() / no .unwrap()) — record X as an alias for MAP.
        if let Some(init) = &local.init
            && let Some(target) = self.resolves_to_tracked_static(&init.expr)
        {
            let binding_name = match &local.pat {
                syn::Pat::Ident(p) => Some(p.ident.to_string()),
                syn::Pat::Type(t) => match &*t.pat {
                    syn::Pat::Ident(p) => Some(p.ident.to_string()),
                    _ => None,
                },
                _ => None,
            };
            if let Some(name) = binding_name {
                self.aliases.insert(name, target);
            }
        }
        syn::visit::visit_local(self, local);
    }

    fn visit_expr_method_call(&mut self, mc: &'ast syn::ExprMethodCall) {
        let method = mc.method.to_string();
        // The interesting bindings appear as receivers of a chain like
        // `BINDING.lock().unwrap().insert(...)` — the call ascends, so the
        // ident we want is buried inside `mc.receiver`. Walk down to the
        // root path, then resolve through the alias map.
        if let Some(name) = self.resolves_to_tracked_static(&mc.receiver) {
            match method.as_str() {
                "insert" => {
                    self.inserts
                        .entry(name)
                        .or_default()
                        .push(mc.method.span().start().line);
                }
                "remove" | "retain" | "clear" | "drain" | "extract_if" => {
                    self.evictions.insert(name);
                }
                _ => {}
            }
        }
        syn::visit::visit_expr_method_call(self, mc);
    }
}

fn receiver_root_ident(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::MethodCall(mc) => receiver_root_ident(&mc.receiver),
        syn::Expr::Paren(p) => receiver_root_ident(&p.expr),
        syn::Expr::Reference(r) => receiver_root_ident(&r.expr),
        syn::Expr::Unary(u) => receiver_root_ident(&u.expr),
        syn::Expr::Try(t) => receiver_root_ident(&t.expr),
        syn::Expr::Path(p) if p.path.segments.len() == 1 => {
            Some(p.path.segments[0].ident.to_string())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn scan(src: &str) -> Vec<PresenceMapFinding> {
        let dir = TempDir::new().unwrap();
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("state.rs"), src).unwrap();
        let files = walk_rs_files(&src_dir);

        struct StaticMap {
            binding: String,
            ty: String,
            file: PathBuf,
            line: usize,
        }
        let mut statics: Vec<StaticMap> = Vec::new();
        for sf in &files {
            let Ok(ast) = &sf.ast else { continue };
            for item in &ast.items {
                let syn::Item::Static(s) = item else { continue };
                let ty = s.ty.to_token_stream().to_string();
                if !is_lazy_mutex_hashmap(&ty) {
                    continue;
                }
                statics.push(StaticMap {
                    binding: s.ident.to_string(),
                    ty: tighten_ws(&ty),
                    file: sf.path.clone(),
                    line: s.ident.span().start().line,
                });
            }
        }
        let binding_set: HashSet<String> = statics.iter().map(|s| s.binding.clone()).collect();

        let mut inserts: HashMap<String, Vec<InsertSite>> = HashMap::new();
        let mut evictions: HashSet<String> = HashSet::new();
        for sf in &files {
            let Ok(ast) = &sf.ast else { continue };
            for item in &ast.items {
                let syn::Item::Fn(f) = item else { continue };
                if !is_server_fn(f) {
                    continue;
                }
                let server_fn_name = f.sig.ident.to_string();
                let mut v = MapUsageVisitor {
                    targets: &binding_set,
                    inserts: HashMap::new(),
                    evictions: HashSet::new(),
                    aliases: HashMap::new(),
                };
                v.visit_block(&f.block);
                for (binding, lines) in v.inserts {
                    for line in lines {
                        inserts.entry(binding.clone()).or_default().push(InsertSite {
                            server_fn: server_fn_name.clone(),
                            file: sf.path.clone(),
                            line,
                        });
                    }
                }
                for binding in v.evictions {
                    evictions.insert(binding);
                }
            }
        }
        let mut findings = Vec::new();
        for s in &statics {
            let Some(insert_sites) = inserts.get(&s.binding) else {
                continue;
            };
            if evictions.contains(&s.binding) {
                continue;
            }
            findings.push(PresenceMapFinding {
                code: "presence_map_unbounded",
                severity: "info",
                file: s.file.clone(),
                line: s.line,
                binding: s.binding.clone(),
                map_type: s.ty.clone(),
                insert_sites: insert_sites.clone(),
                message: String::new(),
            });
        }
        findings
    }

    /// iter03's canonical shape: a session map inserted-into by an async
    /// server fn, never evicted. Must fire.
    #[test]
    fn flags_insert_only_static_map() {
        let findings = scan(
            r#"use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;
use once_cell::sync::Lazy;

static PRESENCE: Lazy<Mutex<HashMap<String, (Instant, String)>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[post("/api/ping")]
async fn ping_presence(sid: String, label: String) -> Result<(), ServerFnError> {
    let mut presence = PRESENCE.lock().unwrap();
    presence.insert(sid, (Instant::now(), label));
    Ok(())
}
"#,
        );
        assert_eq!(findings.len(), 1, "must fire: {findings:?}");
        assert_eq!(findings[0].binding, "PRESENCE");
        assert_eq!(findings[0].insert_sites.len(), 1);
        assert_eq!(findings[0].insert_sites[0].server_fn, "ping_presence");
        assert_eq!(findings[0].severity, "info");
    }

    /// A static map that has BOTH `.insert(...)` and `.retain(...)` calls
    /// from server fns is bounded — the lint must stay silent.
    #[test]
    fn silent_when_eviction_present() {
        let findings = scan(
            r#"use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use once_cell::sync::Lazy;

static PRESENCE: Lazy<Mutex<HashMap<String, (Instant, String)>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[post("/api/ping")]
async fn ping_presence(sid: String, label: String) -> Result<(), ServerFnError> {
    let mut presence = PRESENCE.lock().unwrap();
    presence.insert(sid, (Instant::now(), label));
    Ok(())
}

#[get("/api/sweep")]
async fn sweep_presence() -> Result<(), ServerFnError> {
    let mut presence = PRESENCE.lock().unwrap();
    presence.retain(|_, (ts, _)| ts.elapsed() < Duration::from_secs(60));
    Ok(())
}
"#,
        );
        assert!(
            findings.is_empty(),
            "retain() is the eviction call — must skip: {findings:?}",
        );
    }

    /// Inserts from a non-server fn (e.g. an internal helper) shouldn't
    /// drive the lint — we only count writes reachable from server fns.
    #[test]
    fn ignores_insert_from_non_server_fn() {
        let findings = scan(
            r#"use std::collections::HashMap;
use std::sync::Mutex;
use once_cell::sync::Lazy;

static CACHE: Lazy<Mutex<HashMap<String, String>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

fn seed() {
    let mut cache = CACHE.lock().unwrap();
    cache.insert("k".into(), "v".into());
}
"#,
        );
        assert!(
            findings.is_empty(),
            "only server-fn inserts count: {findings:?}",
        );
    }

    #[test]
    fn is_lazy_mutex_hashmap_recognises_canonical_shapes() {
        assert!(is_lazy_mutex_hashmap(
            "Lazy<Mutex<HashMap<String, (Instant, String)>>>"
        ));
        assert!(is_lazy_mutex_hashmap(
            "once_cell::sync::Lazy<Mutex<HashMap<u32, Session>>>"
        ));
        assert!(is_lazy_mutex_hashmap(
            "Lazy<RwLock<HashMap<String, String>>>"
        ));
        assert!(!is_lazy_mutex_hashmap("Lazy<Mutex<Vec<String>>>"));
        assert!(!is_lazy_mutex_hashmap("Mutex<HashMap<u32, u32>>")); // no Lazy
    }

    #[test]
    fn receiver_root_ident_descends_through_chain() {
        let expr: syn::Expr = syn::parse_str("PRESENCE.lock().unwrap().insert(1, 2)").unwrap();
        // expr is the .insert(...) method call; we want PRESENCE.
        let syn::Expr::MethodCall(mc) = &expr else {
            panic!("expected method call")
        };
        let name = receiver_root_ident(&mc.receiver);
        assert_eq!(name.as_deref(), Some("PRESENCE"));
    }
}
