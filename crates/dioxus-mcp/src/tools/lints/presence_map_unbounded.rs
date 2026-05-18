//! `presence_map_unbounded`: flag `static <MAP>: Lazy<Mutex<HashMap<…>>>`
//! that takes `.insert(...)` writes from server fn bodies but has no
//! reachable *bounded* eviction.
//!
//! Pattern: a server-side presence / session map that monotonically
//! accumulates entries on each request. Without a TTL sweep the map grows
//! forever; long-running servers will exhaust memory. The fix is usually a
//! TTL filter on read OR a periodic sweep that removes stale entries.
//!
//! Eviction taxonomy:
//! * `retain` / `extract_if` / `clear` / `drain` → TTL/sweep eviction.
//!   Treated as bounded → no finding.
//! * `.remove()` only → narrow eviction. Typical iter03 shape: only
//!   `logout_user` calls `.remove()`. Abandoned tabs that never log out
//!   accumulate forever. Fire at `warn` with code
//!   `presence_map_narrow_eviction`.
//! * No eviction at all, but a TTL filter on read (`.values().filter(|(seen, _)|
//!   *seen >= cutoff)…`) — misleading: the user-facing list looks clean
//!   while the underlying map grows forever. Fire at `warn` with code
//!   `presence_map_filter_on_read_no_evict`.
//! * No eviction at all and no read-side filter → fire at `warn` (bumped
//!   from `info` per iter03 follow-up: the failure mode is the same memory
//!   leak, just less elaborately dressed) with code
//!   `presence_map_unbounded`.

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
    /// Server fns observed calling `.remove()` on the map. Populated only
    /// for `narrow_eviction_only` findings so the reviewer can see which
    /// user-triggered fn provides the partial mitigation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove_sites: Vec<InsertSite>,
    /// Insert call sites whose KEY argument looks poisoned — extracted from
    /// a header / cookie / request part via `.unwrap_or_default()` (or
    /// `.unwrap_or(<literal>)`). When the extractor returns the default
    /// (cookie missing, header absent) the inserted key is the empty
    /// string / placeholder, which becomes a permanent slot in the map.
    /// Populated regardless of the primary finding code so the reviewer
    /// always sees the poisoned key as a sidecar.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub poisoned_key_inserts: Vec<PoisonedKeyInsert>,
    pub message: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct PoisonedKeyInsert {
    pub server_fn: String,
    pub file: PathBuf,
    pub line: usize,
    /// Source-fragment of the key expression (best-effort, single line).
    /// Surfaced so the reviewer doesn't have to navigate to file:line to
    /// see the offending extraction.
    pub key_expr: String,
    /// Name of the unwrapping method that yielded the placeholder
    /// (`unwrap_or_default`, `unwrap_or`, `unwrap_or_else`).
    pub unwrap_call: String,
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

    // Walk every server fn body, accumulate per-binding insert sites,
    // narrow remove sites, and a flag for any bounded-eviction call. We
    // use the same `is_server_fn` predicate as the blocking-locks lint —
    // both legacy `#[server]` and the verb-macro shapes count.
    let mut inserts: HashMap<String, Vec<InsertSite>> = HashMap::new();
    let mut narrow_removes: HashMap<String, Vec<InsertSite>> = HashMap::new();
    let mut bounded_evictions: HashSet<String> = HashSet::new();
    let mut poisoned_inserts: HashMap<String, Vec<PoisonedKeyInsert>> = HashMap::new();
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
                removes: HashMap::new(),
                bounded_evictions: HashSet::new(),
                read_filters: HashSet::new(),
                aliases: HashMap::new(),
                poisoned_inserts: HashMap::new(),
                poisoned_locals: HashMap::new(),
            };
            v.visit_block(&f.block);
            for (binding, lines) in v.inserts {
                for line in lines {
                    inserts
                        .entry(binding.clone())
                        .or_default()
                        .push(InsertSite {
                            server_fn: server_fn_name.clone(),
                            file: sf.path.clone(),
                            line,
                        });
                }
            }
            for (binding, lines) in v.removes {
                for line in lines {
                    narrow_removes
                        .entry(binding.clone())
                        .or_default()
                        .push(InsertSite {
                            server_fn: server_fn_name.clone(),
                            file: sf.path.clone(),
                            line,
                        });
                }
            }
            for binding in v.bounded_evictions {
                bounded_evictions.insert(binding);
            }
            for (binding, hits) in v.poisoned_inserts {
                for hit in hits {
                    poisoned_inserts
                        .entry(binding.clone())
                        .or_default()
                        .push(PoisonedKeyInsert {
                            server_fn: server_fn_name.clone(),
                            file: sf.path.clone(),
                            line: hit.line,
                            key_expr: hit.key_expr,
                            unwrap_call: hit.unwrap_call,
                        });
                }
            }
        }
    }

    // Second pass: across the WHOLE crate (not just server fns), look
    // for TTL-style filter-on-read sites — `MAP.lock().<…>.values().filter(…)`
    // or the equivalent through an alias. The TTL filter is usually
    // applied in a helper fn the server fn calls (iter03 `live_presence`)
    // so a server-fn-only walk misses it.
    let mut read_filters: HashSet<String> = HashSet::new();
    for sf in &files {
        let Ok(ast) = &sf.ast else { continue };
        let mut visitor = ReadFilterVisitor {
            targets: &binding_set,
            aliases: HashMap::new(),
            filtered: HashSet::new(),
        };
        visitor.visit_file(ast);
        for b in visitor.filtered {
            read_filters.insert(b);
        }
    }

    let mut findings: Vec<PresenceMapFinding> = Vec::new();
    for s in &statics {
        let Some(insert_sites) = inserts.get(&s.binding) else {
            continue;
        };
        if bounded_evictions.contains(&s.binding) {
            continue;
        }
        let n = insert_sites.len();
        let remove_sites = narrow_removes.get(&s.binding).cloned().unwrap_or_default();
        let has_read_filter = read_filters.contains(&s.binding);
        let (code, severity, message) = if !remove_sites.is_empty() {
            let remove_fns: Vec<String> = {
                let mut v: Vec<String> = remove_sites.iter().map(|r| r.server_fn.clone()).collect();
                v.sort();
                v.dedup();
                v
            };
            let remove_fns_str = remove_fns.join(", ");
            (
                "presence_map_narrow_eviction",
                "warning",
                format!(
                    "`{binding}: {ty}` only sheds entries via `.remove()` in \
                     {remove_fns_str} ({rn} call site{rs}). That covers users who \
                     explicitly opt out, but abandoned tabs / users who never call \
                     {remove_fns_str} accumulate forever. Add a TTL sweep \
                     (`map.retain(|_, (ts, _)| ts.elapsed() < TTL)`) on a periodic \
                     task, or filter expired entries on read AND evict them there; \
                     consider `dashmap` + `mini-moka` for a TTL-aware drop-in. \
                     Server fns insert at {n} site{s}.",
                    binding = s.binding,
                    ty = s.ty,
                    s = if n == 1 { "" } else { "s" },
                    rn = remove_sites.len(),
                    rs = if remove_sites.len() == 1 { "" } else { "s" },
                ),
            )
        } else if has_read_filter {
            (
                "presence_map_filter_on_read_no_evict",
                "warning",
                format!(
                    "`{binding}: {ty}` is read with a TTL filter (`.values().filter(|(seen, _)| \
                     *seen >= cutoff)…` or similar) but never evicted — the user-facing list \
                     looks clean while the underlying map grows forever on every insert ({n} site{s}). \
                     This is the most misleading shape: code reviewers see a TTL-aware read \
                     and assume the storage is bounded too. Convert the read filter into a \
                     two-way operation: `map.retain(|_, (ts, _)| ts.elapsed() < TTL); \
                     map.values().map(…)` — or extract a periodic sweep that calls \
                     `retain` on the same TTL. `dashmap` + `mini-moka` is a TTL-aware \
                     drop-in.",
                    binding = s.binding,
                    ty = s.ty,
                    s = if n == 1 { "" } else { "s" },
                ),
            )
        } else {
            (
                "presence_map_unbounded",
                "warning",
                format!(
                    "`{binding}: {ty}` grows on every request — server fns insert into it \
                     ({n} site{s}) but no `.retain()` / `.remove()` / `.clear()` call \
                     and no TTL-style read filter exists. Long-running servers will \
                     accumulate entries forever. Add a TTL sweep \
                     (`map.retain(|_, (ts, _)| ts.elapsed() < TTL)`) to a periodic task, \
                     or `dashmap` + `mini-moka` as a TTL-aware drop-in.",
                    binding = s.binding,
                    ty = s.ty,
                    s = if n == 1 { "" } else { "s" },
                ),
            )
        };
        findings.push(PresenceMapFinding {
            code,
            severity,
            file: s.file.clone(),
            line: s.line,
            binding: s.binding.clone(),
            map_type: s.ty.clone(),
            insert_sites: insert_sites.clone(),
            remove_sites,
            poisoned_key_inserts: poisoned_inserts.remove(&s.binding).unwrap_or_default(),
            message,
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

#[derive(Debug)]
struct PoisonedKeyHit {
    line: usize,
    key_expr: String,
    unwrap_call: String,
}

struct MapUsageVisitor<'a> {
    targets: &'a HashSet<String>,
    /// (binding_name -> [line of insert]).
    inserts: HashMap<String, Vec<usize>>,
    /// (binding_name -> [line of `.remove()`]). Narrow eviction — only
    /// shrinks the map for keys the caller passes in. Counted separately
    /// from `retain`/`extract_if`/`clear`/`drain` because a user-opt-out
    /// `.remove()` doesn't bound the map under abandoned-tab traffic.
    removes: HashMap<String, Vec<usize>>,
    /// Set of binding_names where a *bounded* eviction call appears
    /// (`retain`, `extract_if`, `clear`, `drain`). These shrink the map
    /// without needing the caller to know the key.
    bounded_evictions: HashSet<String>,
    /// Set of binding_names where a TTL-style filter-on-read call
    /// appears (`map.values().filter(…)` or
    /// `map.iter().filter(…)`). Populated by both `MapUsageVisitor`
    /// (for completeness when a server fn does the filter inline) and
    /// `ReadFilterVisitor` (cross-fn — when the filter lives in a
    /// helper the server fn calls into).
    read_filters: HashSet<String>,
    /// Local aliases for a tracked static (e.g. `let mut presence =
    /// PRESENCE.lock().unwrap();` adds `presence -> PRESENCE`, AND the
    /// if-let / while-let / let-else forms `if let Ok(mut presence) =
    /// PRESENCE.lock() { … }`). Real server fn bodies almost never call
    /// `.insert(...)` directly on the static — they go through a locked
    /// guard binding. Without this map we'd miss the canonical shape
    /// entirely.
    aliases: HashMap<String, String>,
    /// Per-binding poisoned key inserts: `<map>.insert(<extractor>.unwrap_or_default(),
    /// …)` or `<map>.insert(<extractor>.unwrap_or(<literal>), …)`. When
    /// the extractor yields the default value (missing cookie, absent
    /// header) the inserted key is the empty / placeholder, which then
    /// becomes a permanent slot.
    poisoned_inserts: HashMap<String, Vec<PoisonedKeyHit>>,
    /// Local idents whose init expression is itself a poisoned-extractor
    /// chain (`let sid = cookies.get("sid").unwrap_or_default()`). We
    /// trace these so a downstream `<map>.insert(sid, …)` still fires.
    poisoned_locals: HashMap<String, PoisonedKeyHit>,
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
            && let Some(name) = pat_alias_binding(&local.pat)
        {
            self.aliases.insert(name, target);
        }
        // Poisoned-local detection: `let sid = cookies.get("sid").unwrap_or_default()`
        // (with optional `.to_string()` / `.to_owned()` tail). We record
        // the local ident so a downstream `<map>.insert(sid, …)` still
        // fires even though the unwrap chain isn't inline at the insert.
        if let Some(init) = &local.init
            && let Some(name) = pat_alias_binding(&local.pat)
            && let Some(hit) = poisoned_key_from_expr(&init.expr)
        {
            self.poisoned_locals.insert(name, hit);
        }
        syn::visit::visit_local(self, local);
    }

    fn visit_expr_let(&mut self, e: &'ast syn::ExprLet) {
        // Covers `if let Ok(mut presence) = PRESENCE.lock() { … }` and
        // the `while let` / `let-else` analogues. The wrapper is
        // typically `Ok(...)` (LockResult) or `Some(...)` (Option).
        if let Some(target) = self.resolves_to_tracked_static(&e.expr)
            && let Some(name) = pat_alias_binding(&e.pat)
        {
            self.aliases.insert(name, target);
        }
        syn::visit::visit_expr_let(self, e);
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
                        .entry(name.clone())
                        .or_default()
                        .push(mc.method.span().start().line);
                    // Poisoned-key check: inspect the FIRST argument.
                    // Match both the inline shape
                    // (`map.insert(cookies.get("sid").unwrap_or_default(), …)`)
                    // AND the let-binding shape
                    // (`let sid = …unwrap_or_default(); map.insert(sid, …)`).
                    if let Some(key_arg) = mc.args.first() {
                        if let Some(hit) = poisoned_key_from_expr(key_arg) {
                            self.poisoned_inserts.entry(name).or_default().push(hit);
                        } else if let Some(ident) = expr_simple_ident(key_arg)
                            && let Some(src_hit) = self.poisoned_locals.get(&ident)
                        {
                            // Surface the insert's line (more actionable
                            // than the let's), but keep the rest of the
                            // hit from the original extraction so
                            // `key_expr` still names the unwrap chain.
                            self.poisoned_inserts
                                .entry(name)
                                .or_default()
                                .push(PoisonedKeyHit {
                                    line: mc.method.span().start().line,
                                    key_expr: format!("{} (via local `{ident}`)", src_hit.key_expr),
                                    unwrap_call: src_hit.unwrap_call.clone(),
                                });
                        }
                    }
                }
                "remove" => {
                    self.removes
                        .entry(name)
                        .or_default()
                        .push(mc.method.span().start().line);
                }
                "retain" | "clear" | "drain" | "extract_if" => {
                    self.bounded_evictions.insert(name);
                }
                _ => {}
            }
        }
        // Read-filter detection: any `.filter(…)` (or `.filter_map(…)`)
        // whose receiver chain bottoms out at a tracked static via
        // `.values()` / `.iter()` / `.into_iter()`. The shape is the
        // canonical TTL-on-read pattern; we don't try to inspect the
        // closure for `Instant`/`Duration` references because the
        // false-positive cost (rare filtered reads on non-TTL data) is
        // dwarfed by the false-negative cost of missing the iter03
        // PRESENCE shape.
        if matches!(method.as_str(), "filter" | "filter_map")
            && let Some(name) =
                receiver_chain_includes_values_iter(&mc.receiver, self.targets, &self.aliases)
        {
            self.read_filters.insert(name);
        }
        syn::visit::visit_expr_method_call(self, mc);
    }
}

/// Cross-fn read-filter detection. Server fns that hand the lock guard
/// to a helper fn pull the filter site out of the server fn body, so the
/// per-fn `MapUsageVisitor` doesn't see it. This visitor walks the
/// whole file (every fn, not just server fns) and records any
/// `<chain>.values().filter(...)` (or similar) that bottoms out at a
/// tracked static.
struct ReadFilterVisitor<'a> {
    targets: &'a HashSet<String>,
    aliases: HashMap<String, String>,
    filtered: HashSet<String>,
}

impl<'a, 'ast> Visit<'ast> for ReadFilterVisitor<'a> {
    fn visit_local(&mut self, local: &'ast syn::Local) {
        if let Some(init) = &local.init
            && let Some(target) = self.resolves_to_tracked_static(&init.expr)
            && let Some(name) = pat_alias_binding(&local.pat)
        {
            self.aliases.insert(name, target);
        }
        syn::visit::visit_local(self, local);
    }
    fn visit_expr_let(&mut self, e: &'ast syn::ExprLet) {
        if let Some(target) = self.resolves_to_tracked_static(&e.expr)
            && let Some(name) = pat_alias_binding(&e.pat)
        {
            self.aliases.insert(name, target);
        }
        syn::visit::visit_expr_let(self, e);
    }
    fn visit_expr_method_call(&mut self, mc: &'ast syn::ExprMethodCall) {
        let method = mc.method.to_string();
        if matches!(method.as_str(), "filter" | "filter_map")
            && let Some(name) =
                receiver_chain_includes_values_iter(&mc.receiver, self.targets, &self.aliases)
        {
            self.filtered.insert(name);
        }
        // Closure-param aliasing: when the call is `<tracked_chain>.map(|p|
        // body)` (or `.and_then(|p| body)`), bind `p` as an alias for the
        // tracked static so the body's `p.values().filter(…)` resolves.
        // We restore the binding after visiting the closure to keep the
        // alias scope correct.
        let target = if matches!(method.as_str(), "map" | "and_then" | "or_else") {
            self.resolves_to_tracked_static(&mc.receiver)
        } else {
            None
        };
        if let Some(target) = target
            && let Some(syn::Expr::Closure(c)) = mc.args.first()
            && let Some(first_input) = c.inputs.first()
            && let Some(param) = pat_alias_binding(first_input)
        {
            let saved = self.aliases.insert(param.clone(), target);
            syn::visit::visit_expr_closure(self, c);
            match saved {
                Some(prev) => {
                    self.aliases.insert(param, prev);
                }
                None => {
                    self.aliases.remove(&param);
                }
            }
            // visit the receiver chain too so its own nested filters
            // get a chance to fire (e.g. nested `.values().filter()`
            // on the receiver side, rare but possible).
            syn::visit::visit_expr(self, &mc.receiver);
            return;
        }
        syn::visit::visit_expr_method_call(self, mc);
    }
}

impl<'a> ReadFilterVisitor<'a> {
    fn resolves_to_tracked_static(&self, expr: &syn::Expr) -> Option<String> {
        let name = receiver_root_ident(expr)?;
        if self.targets.contains(&name) {
            Some(name)
        } else if let Some(t) = self.aliases.get(&name) {
            Some(t.clone())
        } else {
            None
        }
    }
}

/// Walk a receiver-chain looking for a `.values()` / `.iter()` /
/// `.into_iter()` step whose receiver bottoms out at a tracked static
/// (directly or via an alias). Returns the binding name if found.
fn receiver_chain_includes_values_iter(
    expr: &syn::Expr,
    targets: &HashSet<String>,
    aliases: &HashMap<String, String>,
) -> Option<String> {
    let mut cur = expr;
    loop {
        match cur {
            syn::Expr::MethodCall(mc) => {
                let name = mc.method.to_string();
                if matches!(
                    name.as_str(),
                    "values" | "iter" | "into_iter" | "values_mut" | "iter_mut"
                ) {
                    let root = receiver_root_ident(&mc.receiver)?;
                    if targets.contains(&root) {
                        return Some(root);
                    }
                    if let Some(t) = aliases.get(&root) {
                        return Some(t.clone());
                    }
                    return None;
                }
                cur = &mc.receiver;
            }
            syn::Expr::Paren(p) => cur = &p.expr,
            syn::Expr::Reference(r) => cur = &r.expr,
            syn::Expr::Try(t) => cur = &t.expr,
            syn::Expr::Unary(u) => cur = &u.expr,
            _ => return None,
        }
    }
}

fn expr_simple_ident(e: &syn::Expr) -> Option<String> {
    if let syn::Expr::Path(p) = e
        && p.path.segments.len() == 1
    {
        return Some(p.path.segments[0].ident.to_string());
    }
    None
}

/// Inspect a `<map>.insert(<KEY>, …)` key argument and decide whether
/// the value flowing in could be a placeholder (cookie missing / header
/// absent / parse failed). The shape we care about is a method-call
/// chain whose tail is `unwrap_or_default` / `unwrap_or` /
/// `unwrap_or_else`. The receiver of that final call is the actual
/// extractor (`cookies.get("sid")`, `headers.get("auth")`, etc.) — its
/// failure mode is the leak source.
fn poisoned_key_from_expr(e: &syn::Expr) -> Option<PoisonedKeyHit> {
    // Peel `.to_string()` / `.to_owned()` / `.clone()` etc. off the
    // tail — those are common after `unwrap_or_default()` and don't
    // change the failure mode.
    let mut cur = e;
    while let syn::Expr::MethodCall(mc) = cur {
        let m = mc.method.to_string();
        if matches!(
            m.as_str(),
            "to_string" | "to_owned" | "into" | "clone" | "as_str"
        ) {
            cur = &mc.receiver;
            continue;
        }
        if matches!(
            m.as_str(),
            "unwrap_or_default" | "unwrap_or" | "unwrap_or_else"
        ) {
            return Some(PoisonedKeyHit {
                line: mc.method.span().start().line,
                key_expr: trim_to_oneline(&e.to_token_stream().to_string()),
                unwrap_call: m,
            });
        }
        break;
    }
    None
}

fn trim_to_oneline(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = false;
    for c in s.chars() {
        let c = if c == '\n' || c == '\r' { ' ' } else { c };
        if c == ' ' {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(c);
            last_space = false;
        }
    }
    out.trim().to_string()
}

/// Walk a `Pat` to extract the inner ident binding, including `Ok(…)`,
/// `Some(…)`, `Result::Ok(…)`, etc. wrappers used in `if let` /
/// `let-else` patterns.
fn pat_alias_binding(p: &syn::Pat) -> Option<String> {
    match p {
        syn::Pat::Ident(pi) => Some(pi.ident.to_string()),
        syn::Pat::Type(t) => pat_alias_binding(&t.pat),
        syn::Pat::Reference(r) => pat_alias_binding(&r.pat),
        syn::Pat::TupleStruct(ts) => {
            let last = ts.path.segments.last()?.ident.to_string();
            if matches!(last.as_str(), "Ok" | "Some") {
                ts.elems.first().and_then(pat_alias_binding)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn receiver_root_ident(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::MethodCall(mc) => receiver_root_ident(&mc.receiver),
        syn::Expr::Paren(p) => receiver_root_ident(&p.expr),
        syn::Expr::Reference(r) => receiver_root_ident(&r.expr),
        syn::Expr::Unary(u) => receiver_root_ident(&u.expr),
        syn::Expr::Try(t) => receiver_root_ident(&t.expr),
        // Accept the LAST segment of any path. Fully-qualified accesses
        // (`crate::server::state::SESSIONS.lock()`) are common; matching
        // by last segment lets the visitor resolve the static without
        // requiring the caller to import it. Collision risk is low — the
        // tracked set is hand-built from `Lazy<Mutex<HashMap>>` statics,
        // which use distinct SCREAMING_SNAKE_CASE names.
        syn::Expr::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
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
        let mut narrow_removes: HashMap<String, Vec<InsertSite>> = HashMap::new();
        let mut bounded_evictions: HashSet<String> = HashSet::new();
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
                    removes: HashMap::new(),
                    bounded_evictions: HashSet::new(),
                    read_filters: HashSet::new(),
                    aliases: HashMap::new(),
                    poisoned_inserts: HashMap::new(),
                    poisoned_locals: HashMap::new(),
                };
                v.visit_block(&f.block);
                for (binding, lines) in v.inserts {
                    for line in lines {
                        inserts
                            .entry(binding.clone())
                            .or_default()
                            .push(InsertSite {
                                server_fn: server_fn_name.clone(),
                                file: sf.path.clone(),
                                line,
                            });
                    }
                }
                for (binding, lines) in v.removes {
                    for line in lines {
                        narrow_removes
                            .entry(binding.clone())
                            .or_default()
                            .push(InsertSite {
                                server_fn: server_fn_name.clone(),
                                file: sf.path.clone(),
                                line,
                            });
                    }
                }
                for binding in v.bounded_evictions {
                    bounded_evictions.insert(binding);
                }
            }
        }
        let mut read_filters: HashSet<String> = HashSet::new();
        for sf in &files {
            let Ok(ast) = &sf.ast else { continue };
            let mut rv = ReadFilterVisitor {
                targets: &binding_set,
                aliases: HashMap::new(),
                filtered: HashSet::new(),
            };
            rv.visit_file(ast);
            for b in rv.filtered {
                read_filters.insert(b);
            }
        }
        let mut findings = Vec::new();
        for s in &statics {
            let Some(insert_sites) = inserts.get(&s.binding) else {
                continue;
            };
            if bounded_evictions.contains(&s.binding) {
                continue;
            }
            let remove_sites = narrow_removes.get(&s.binding).cloned().unwrap_or_default();
            let has_read_filter = read_filters.contains(&s.binding);
            let (code, severity) = if !remove_sites.is_empty() {
                ("presence_map_narrow_eviction", "warning")
            } else if has_read_filter {
                ("presence_map_filter_on_read_no_evict", "warning")
            } else {
                ("presence_map_unbounded", "warning")
            };
            findings.push(PresenceMapFinding {
                code,
                severity,
                file: s.file.clone(),
                line: s.line,
                binding: s.binding.clone(),
                map_type: s.ty.clone(),
                insert_sites: insert_sites.clone(),
                remove_sites,
                poisoned_key_inserts: Vec::new(),
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
        // iter03 follow-up: bumped from `info` to `warning` — same memory
        // leak as the narrow-eviction case, just dressed plainer.
        assert_eq!(findings[0].severity, "warning");
    }

    /// iter03's `SESSIONS` / `PRESENCE` shape: only `.remove()` from
    /// `logout_user` evicts. That covers users who opt out but not abandoned
    /// tabs — fire at `warn` with the narrow-eviction code, surface the
    /// remove sites so the reviewer can audit the gate.
    #[test]
    fn narrow_remove_only_fires_as_warn() {
        let findings = scan(
            r#"use std::collections::HashMap;
use std::sync::Mutex;
use once_cell::sync::Lazy;

static SESSIONS: Lazy<Mutex<HashMap<String, String>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[post("/api/login")]
async fn login_user(name: String) -> Result<(), ServerFnError> {
    let mut sessions = SESSIONS.lock().unwrap();
    sessions.insert("sid".into(), name);
    Ok(())
}

#[post("/api/logout")]
async fn logout_user(sid: String) -> Result<(), ServerFnError> {
    let mut sessions = SESSIONS.lock().unwrap();
    sessions.remove(&sid);
    Ok(())
}
"#,
        );
        assert_eq!(findings.len(), 1, "must fire: {findings:?}");
        let f = &findings[0];
        assert_eq!(f.binding, "SESSIONS");
        assert_eq!(f.code, "presence_map_narrow_eviction");
        assert_eq!(f.severity, "warning");
        assert_eq!(f.remove_sites.len(), 1);
        assert_eq!(f.remove_sites[0].server_fn, "logout_user");
    }

    /// iter03 PRESENCE shape: inserts via `if let Ok(mut presence) =
    /// PRESENCE.lock() { presence.insert(...) }` AND a TTL filter on
    /// read in a helper fn (`.values().filter(|(seen, _)| *seen >=
    /// cutoff)`). Must fire with the new `presence_map_filter_on_read_no_evict`
    /// code — the most misleading shape because the user-facing list
    /// looks bounded.
    #[test]
    fn filter_on_read_no_evict_fires_distinctly() {
        let findings = scan(
            r#"use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use once_cell::sync::Lazy;

static PRESENCE: Lazy<Mutex<HashMap<String, (Instant, String)>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

const PRESENCE_TTL: Duration = Duration::from_secs(8);

#[post("/api/ping")]
async fn ping_presence(sid: String, label: String) -> Result<Vec<String>, ServerFnError> {
    if let Ok(mut presence) = PRESENCE.lock() {
        presence.insert(sid, (Instant::now(), label));
    }
    Ok(live_presence())
}

fn live_presence() -> Vec<String> {
    let cutoff = Instant::now() - PRESENCE_TTL;
    PRESENCE
        .lock()
        .map(|p| {
            p.values()
                .filter(|(seen, _)| *seen >= cutoff)
                .map(|(_, n)| n.clone())
                .collect()
        })
        .unwrap_or_default()
}
"#,
        );
        assert_eq!(findings.len(), 1, "must fire: {findings:?}");
        let f = &findings[0];
        assert_eq!(f.binding, "PRESENCE");
        assert_eq!(f.code, "presence_map_filter_on_read_no_evict");
        assert_eq!(f.severity, "warning");
    }

    /// Poisoned key shape: `let sid = cookies.get("sid").unwrap_or_default().to_string();`
    /// then `presence.insert(sid, …)`. The `unwrap_or_default()` returns
    /// the empty string when the cookie is missing — that becomes a
    /// permanent key in the map. We surface it as a sidecar
    /// `poisoned_key_inserts` entry on the existing finding (not a new
    /// finding — the primary issue is still the missing eviction).
    #[test]
    fn surfaces_poisoned_key_insert() {
        let dir = tempfile::TempDir::new().unwrap();
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(
            src_dir.join("state.rs"),
            r#"use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;
use once_cell::sync::Lazy;

static PRESENCE: Lazy<Mutex<HashMap<String, (Instant, String)>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[post("/api/ping")]
async fn ping_presence() -> Result<(), ServerFnError> {
    let sid = cookies.get("sid").unwrap_or_default().to_string();
    let mut presence = PRESENCE.lock().unwrap();
    presence.insert(sid, (Instant::now(), "x".into()));
    Ok(())
}
"#,
        )
        .unwrap();
        let files = walk_rs_files(&src_dir);
        // Collect statics (manual, mirroring the helper).
        let mut binding_set: HashSet<String> = HashSet::new();
        for sf in &files {
            let Ok(ast) = &sf.ast else { continue };
            for item in &ast.items {
                if let syn::Item::Static(s) = item
                    && is_lazy_mutex_hashmap(&s.ty.to_token_stream().to_string())
                {
                    binding_set.insert(s.ident.to_string());
                }
            }
        }
        let mut poisoned: Vec<PoisonedKeyHit> = Vec::new();
        for sf in &files {
            let Ok(ast) = &sf.ast else { continue };
            for item in &ast.items {
                let syn::Item::Fn(f) = item else { continue };
                if !is_server_fn(f) {
                    continue;
                }
                let mut v = MapUsageVisitor {
                    targets: &binding_set,
                    inserts: HashMap::new(),
                    removes: HashMap::new(),
                    bounded_evictions: HashSet::new(),
                    read_filters: HashSet::new(),
                    aliases: HashMap::new(),
                    poisoned_inserts: HashMap::new(),
                    poisoned_locals: HashMap::new(),
                };
                v.visit_block(&f.block);
                for (_, hits) in v.poisoned_inserts {
                    poisoned.extend(hits);
                }
            }
        }
        assert_eq!(
            poisoned.len(),
            1,
            "expected one poisoned-key hit: {poisoned:?}"
        );
        let hit = &poisoned[0];
        assert!(
            hit.key_expr.contains("unwrap_or_default"),
            "key_expr should name the unwrap call: {hit:?}",
        );
        assert_eq!(hit.unwrap_call, "unwrap_or_default");
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
