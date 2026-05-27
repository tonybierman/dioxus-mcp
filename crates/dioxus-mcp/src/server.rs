use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    tool, tool_handler, tool_router,
};

use crate::state::State;
use crate::tools;

#[derive(Clone)]
pub struct DioxusMcp {
    pub state: Arc<State>,
    #[allow(dead_code)]
    tool_router: ToolRouter<DioxusMcp>,
}

#[tool_router]
impl DioxusMcp {
    pub fn new(state: Arc<State>) -> Self {
        Self {
            state,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Audit Cargo.toml + dioxus.toml for misconfigurations (conflicting platform features, fullstack mis-wiring, version mismatches)."
    )]
    async fn audit_feature_flags(
        &self,
        Parameters(p): Parameters<tools::audit::audit_feature_flags::AuditFeatureFlagsParams>,
    ) -> Result<CallToolResult, McpError> {
        let report = tools::audit::audit_feature_flags::audit_feature_flags(&self.state, p).await;
        ok_json(&report)
    }

    #[tool(
        description = "Lint Rust file(s)' rsx! blocks for common 0.7 mistakes (missing keys on iterators, parameter-less event handlers, attribute writes that trigger E0034 ambiguity — e.g. `autofocus: true` on `input`/`button`/`textarea`/`select`). The response includes a `checks_run` list naming the lints that fired so a clean `issues: []` is distinguishable from an empty/skipped scan. Pass `file` for a single file (single-file response shape: `file`, `rsx_block_count`, `checks_run`, `issues`). Pass `files: [...]` for batch mode (adds `per_file: [...]`; top-level `issues` is the flat merge with each issue tagged by file)."
    )]
    async fn check_rsx(
        &self,
        Parameters(p): Parameters<tools::lints::check_rsx::CheckRsxParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::lints::check_rsx::check_rsx(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Server-fn call graph: for every #[server] fn, list every call site (caller_file, caller_line, enclosing_fn) and emit an orphan list of server fns nobody calls. Cross-crate callers not detected."
    )]
    async fn server_fn_call_graph(
        &self,
        Parameters(p): Parameters<tools::inspect::server_fn_call_graph::ServerFnCallGraphParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::inspect::server_fn_call_graph::server_fn_call_graph(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Audit assets/: list files under the assets dir(s) not referenced by any `asset!(\"...\")` macro, and `asset!()` references to files that don't exist on disk. Dynamic (non-string-literal) args are counted but skipped."
    )]
    async fn asset_audit(
        &self,
        Parameters(p): Parameters<tools::audit::asset_audit::AssetAuditParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::audit::asset_audit::asset_audit(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "List components defined but never used in any rsx! block. Components reachable from the Routable enum (route targets + layouts) plus `App` are treated as roots."
    )]
    async fn dead_components(
        &self,
        Parameters(p): Parameters<tools::inspect::dead_components::DeadComponentsParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::inspect::dead_components::dead_components(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Find props passed unchanged from a parent component into a child (drilling). Matches bare ident and one-level wrappers `.clone()`, `.into()`, `.to_owned()`, `.read()`, `.peek()`, `.cloned()`; each finding tagged with a `via` field."
    )]
    async fn prop_drill(
        &self,
        Parameters(p): Parameters<tools::inspect::prop_drill::PropDrillParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::inspect::prop_drill::prop_drill(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Lint signals: flag `use_signal` / `use_memo` / `use_resource` / `use_effect` calls inside `for` / `while` / `loop` bodies in component fns — a new hook is created on every iteration."
    )]
    async fn signal_lint(
        &self,
        Parameters(p): Parameters<tools::lints::signal_lint::SignalLintParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::lints::signal_lint::signal_lint(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Flag a `Signal<T>` prop passed unchanged through 2 or more parents — the canonical \"missing `use_context_provider`\" shape that `prop_drill` only sees as a single-level passthrough at each hop. Walks `prop_drill`'s state_passthrough edges, keeps only those where the parent-side prop type is `Signal<T>` / `ReadSignal<T>` / `WriteSignal<T>`, and emits a finding for every two-hop forwarding chain (A → B → C). Returns the full chain, the signal type, and a copy-pasteable `use_context_provider` / `use_context::<…>()` fix snippet. Severity is `warning` — Signals flowing unmodified through multiple hops almost always want a context provider."
    )]
    async fn signal_drilled_2_levels(
        &self,
        Parameters(p): Parameters<tools::lints::signal_drilled_2_levels::SignalDrilledParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::lints::signal_drilled_2_levels::signal_drilled_2_levels(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Lint Props structs: flag `#[derive(Props, ...)]` structs that don't also derive `PartialEq`. Dioxus needs PartialEq on Props for memoization."
    )]
    async fn props_lint(
        &self,
        Parameters(p): Parameters<tools::lints::props_lint::PropsLintParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::lints::props_lint::props_lint(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Hint when a component hand-rolls a catalog widget. Detects: the HTML5 drag/drop triplet (`ondragstart` + `ondragover` + `ondrop` on the same component, `confidence: high`); the drop-target half alone (`confidence: low`); and bare DOM elements with catalog equivalents (`<select>`, `<dialog>`, `<textarea>`, `<input>`, `confidence: low`). Skips catalog wrapper files (`src/components/<catalog_name>/`). Findings are hints, not errors — the drag/drop catalog widget is single-list, and specialised forms (e.g. `<input type=\"file\">`) have no catalog equivalent, so verify the use case before swapping."
    )]
    async fn reinvented_widget(
        &self,
        Parameters(p): Parameters<tools::lints::reinvented_widget::ReinventedWidgetParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::lints::reinvented_widget::reinvented_widget(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Flag `std::sync::Mutex` / `RwLock` (and similar sync locks) called from inside `async` server-fn bodies. Currently safe if the critical section stays fully synchronous, but the first `.await` added while holding the guard will block the Tokio worker thread for the duration of that await. Suggests switching to `tokio::sync::Mutex` / `RwLock` (lock-across-await safe) or moving the section into `tokio::task::spawn_blocking`. Calls inside an existing `spawn_blocking { … }` body are silently skipped — that's the recommended escape hatch."
    )]
    async fn server_state_blocking_locks(
        &self,
        Parameters(p): Parameters<
            tools::lints::server_state_blocking_locks::ServerStateBlockingLocksParams,
        >,
    ) -> Result<CallToolResult, McpError> {
        match tools::lints::server_state_blocking_locks::server_state_blocking_locks(&self.state, p)
            .await
        {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Flag the hand-rolled \"optimistic-lock staleness gate\" pattern in a Dioxus component: a `Signal<integer>` that is snapshotted (`let snap = sig();`), bumped (`sig += 1`), and then compared back against the snapshot (`if sig() == snap { … }`) inside an `async` / `spawn` tail to gate reconciliation. The shape is correct but it has recurred verbatim across multiple generated apps — suggests extracting into a Store generation method (e.g. `let rev = store.bump_revision(); … if store.matches(rev) { … }`) so the invariant lives in one place. Confidence: medium — requires all three shapes to co-occur on the same signal in the same write source."
    )]
    async fn optimistic_lock_gate(
        &self,
        Parameters(p): Parameters<tools::lints::optimistic_lock_gate::OptimisticLockGateParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::lints::optimistic_lock_gate::optimistic_lock_gate(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Flag `Set-Cookie` header values built inside server fns that lack the `Secure` attribute. Scans every string literal in a server-fn body (including `format!(...)` format strings and `HeaderValue::from_static(\"...\")` bare literals) and treats it as a cookie value when it contains at least one recognised cookie attribute (`HttpOnly`, `SameSite=`, `Path=`, `Max-Age=`, `Domain=`, `Expires=`, `Partitioned`). Findings: `SameSite=None` without `Secure` → severity `error` (browsers reject the cookie outright); any other cookie value missing `Secure` → severity `warning`. Suggests `Secure` + the `__Host-` prefix for session cookies."
    )]
    async fn insecure_set_cookie(
        &self,
        Parameters(p): Parameters<tools::lints::insecure_set_cookie::InsecureSetCookieParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::lints::insecure_set_cookie::insecure_set_cookie(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Flag a `static <MAP>: Lazy<Mutex<HashMap<…>>>` (or `Lazy<RwLock<…>>` / `Lazy<…<BTreeMap<…>>>` / `OnceLock<…<DashMap<…>>>`) that server fns insert into but never evict from. Walks every `#[server]` / `#[get/post/put/delete/patch]` body, accumulates `.insert(...)` sites per static binding, and emits an `info`-severity finding when no `.retain()` / `.remove()` / `.clear()` / `.drain()` / `.extract_if()` call is reachable from any server fn. Long-running servers will accumulate entries forever — the lint nudges towards a TTL sweep or a TTL-aware map crate (`dashmap` + `mini-moka` is the canonical drop-in). Many app-internal caches are deliberately append-only, so the lint is a reviewer hint, not a hard error."
    )]
    async fn presence_map_unbounded(
        &self,
        Parameters(p): Parameters<tools::lints::presence_map_unbounded::PresenceMapUnboundedParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::lints::presence_map_unbounded::presence_map_unbounded(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Hint when client and server independently validate the same string-literal set — the canonical 'shared enum, please' shape generators hit when one side has `const COLUMNS: [(&str, …); 3] = [(\"todo\", …), …]` and the server `match column { \"todo\" | \"doing\" | \"done\" => … }` re-pattern-matches the same values. Severity `info`, confidence `low` — both sides currently agree, but adding a value to one half silently desyncs from the other. Fix suggestion is a shared `enum` under `src/model/` with serde + strum derives so the client `for`-loop and the server pattern match both drive off one source of truth."
    )]
    async fn shared_enum_validation(
        &self,
        Parameters(p): Parameters<tools::lints::shared_enum_validation::SharedEnumValidationParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::lints::shared_enum_validation::shared_enum_validation(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Hint when a model's `id` field encodes optimistic-placeholder state via a literal string prefix (`\"tmp-\"`, `\"pending-\"`, `\"local-\"`, …). Detects two complementary shapes: `<expr>.id.starts_with(\"tmp-\")` (read site — consumer branches on the magic prefix) and `format!(\"tmp-{…}\", …)` (write site — optimistic-create path forges an ID). Severity `info`, confidence `low` — the pattern works but is brittle, and a real ID that happens to start with `tmp-` would be silently mis-classified. Fix suggestion is a typed `pending: bool` field on the model or a sidecar `pending: HashSet<Id>` signal."
    )]
    async fn magic_id_prefix_for_optimistic(
        &self,
        Parameters(p): Parameters<tools::lints::magic_id_prefix::MagicIdPrefixParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::lints::magic_id_prefix::magic_id_prefix_for_optimistic(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Hint when a `#[component] fn` takes an owned non-Signal arg type (`Vec<T>` or a user struct) that will be re-cloned on every parent render. Skips `Signal<…>` / `ReadOnlySignal<…>` / `EventHandler<…>` / `Memo<…>` / `Resource<…>` / `Callback<…>` (reactive handles, not value clones) and stdlib types we don't want to nag about (`String`, `Option`, `HashMap`, primitives, …). Emits one `info`-severity finding per qualifying arg with `confidence: medium` when a *reactive* parent caller is observed (its body has `.set()` / `.with_mut()` / `+=` writes), or `confidence: low` otherwise. `reactive_callers` lists the parents whose user-driven re-renders trigger the reclone. `fix` suggests `ReadOnlySignal<Vec<T>>` / `Rc<[T]>` for Vecs and `ReadOnlySignal<T>` / `Rc<T>` for owned structs."
    )]
    async fn vec_or_owned_prop_passthrough(
        &self,
        Parameters(p): Parameters<
            tools::lints::vec_or_owned_prop_passthrough::VecOrOwnedPropParams,
        >,
    ) -> Result<CallToolResult, McpError> {
        match tools::lints::vec_or_owned_prop_passthrough::vec_or_owned_prop_passthrough(
            &self.state,
            p,
        )
        .await
        {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Flag pure helper fns whose body is byte-identical across `src/components/` and `src/server/`. Generators copy-paste shared logic into both halves of the app instead of lifting it into `src/model/`; the two impls then drift when one side patches a bug and the other doesn't. Skips `async fn` (server fns themselves), `#[component]` fns, and bodies with fewer than 2 statements (wrappers that legitimately differ). Emits `warning`-severity findings with `sites: [{file, line, side}]` so the reviewer sees every duplicate at once, plus a paste-ready fix recommending `src/model/<name>.rs`. Use this on generated apps before they accumulate two-way drift."
    )]
    async fn duplicate_helper_client_server(
        &self,
        Parameters(p): Parameters<
            tools::lints::duplicate_helper_client_server::DuplicateHelperParams,
        >,
    ) -> Result<CallToolResult, McpError> {
        match tools::lints::duplicate_helper_client_server::duplicate_helper_client_server(
            &self.state,
            p,
        )
        .await
        {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Flag an auth-shaped helper fn that's called from ≥3 distinct server fn bodies — `user_from_cookies(&cookies)`, `cookies.get(\"sid\")`, etc. The same identity check repeated across the server surface is the signal that the app wants an Axum `FromRequestParts` extractor: write the auth logic once, get a `Session` / `User` type-level guarantee everywhere it's needed. Emits `info`-severity findings; `who_am_i` / `login` / `logout` endpoints are exempt because those ARE the auth surface. `min_call_sites` (default 3) lowers the threshold for early surfacing."
    )]
    async fn repeated_auth_extractor(
        &self,
        Parameters(p): Parameters<
            tools::lints::repeated_auth_extractor::RepeatedAuthExtractorParams,
        >,
    ) -> Result<CallToolResult, McpError> {
        match tools::lints::repeated_auth_extractor::repeated_auth_extractor(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Flag a `use_future` / `spawn` polling loop whose tick interval is a constant literal — no error-path backoff, no jitter. Detects `loop { … <server_fn>().await … <sleep>(<int_literal>).await; }` patterns where the sleep is `TimeoutFuture::new`, `tokio::time::sleep`, `gloo_timers::sleep`, or any of the timer-typed constructors. Stays silent when the delay expression is a variable (presumed to encode backoff already) and in sync contexts. Emits `warning` severity with `delay_ms` and `sleep_call` so reviewers can grep the rest of the codebase for the same pattern."
    )]
    async fn polling_future_no_backoff(
        &self,
        Parameters(p): Parameters<
            tools::lints::polling_future_no_backoff::PollingFutureNoBackoffParams,
        >,
    ) -> Result<CallToolResult, McpError> {
        match tools::lints::polling_future_no_backoff::polling_future_no_backoff(&self.state, p)
            .await
        {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Flag an `Err(_) => {}` arm (or `if let Err(_) = … {}`) inside an `async` context — `use_future(|| async move { … })`, `spawn(async move { … })`, an `async fn` body, or any inline `async { … }` block. Swallowing errors here turns a polling loop into a silent failure: the UI looks live, the server is broken. Emits one `warning`-severity finding per offending arm with `shape: match_arm` or `if_let_block`. Stays silent in sync contexts and when the Err arm has any non-empty body."
    )]
    async fn empty_async_error_arm(
        &self,
        Parameters(p): Parameters<tools::lints::empty_async_error_arm::EmptyAsyncErrorArmParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::lints::empty_async_error_arm::empty_async_error_arm(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Flag a pure derivation fn — one that takes `&[T]` and returns `Vec<T>` (or any owned `Vec<U>`) — when it's invoked from inside an `rsx!` body without `use_memo(…)`. Each render reruns the filter/sort/clone even when neither the source signal nor the selector changed. iter03's `column_cards(&cards.read(), col_id)` called three times per render is the canonical shape. Emits one `warning`-severity finding per (component, callee) pair with `calls_in_rsx_block` so reviewers see the per-render multiplier. Fix is mechanical: `use_memo(move || callee(args))()`. False-positive shape: a deliberately-fresh recomputation (rare); reviewer overrides."
    )]
    async fn derived_view_no_memo(
        &self,
        Parameters(p): Parameters<tools::lints::derived_view_no_memo::DerivedViewNoMemoParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::lints::derived_view_no_memo::derived_view_no_memo(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Hint when a component hand-rolls a catalog widget via class-attribute conventions. Scans every `#[component] fn` rsx for `class: \"<literal>\"` strings and flags tokens that map to a catalog widget (`modal`/`dialog` → dialog, `tabs`/`tab-strip`/`tablist` → tabs, `accordion` → accordion, `popover` → popover, `tooltip` → tooltip, `calendar` → calendar, `datepicker`/`date-picker` → date_picker, `dropdown`/`dropdown-menu` → dropdown_menu, `toast`/`snackbar` → toast, `sidebar` → sidebar, `drawer` → sheet, `pagination` → pagination, `avatar` → avatar, `badge` → badge, `progress`/`progress-bar` → progress). Skips catalog wrapper files (`src/components/<catalog_name>/`). Dedupes per-component. All findings are `confidence: low` — class names are conventions, not contracts. Complements `reinvented_widget` (bare DOM tags + drag/drop) and `suggest_components` (the pre-write counterpart)."
    )]
    async fn components_audit(
        &self,
        Parameters(p): Parameters<tools::lints::components_audit::ComponentsAuditParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::lints::components_audit::components_audit(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Run every project-wide lint (`check_rsx`, `dead_components`, `prop_drill`, `signal_lint`, `props_lint`, `reinvented_widget`, `optimistic_lock_gate`, `components_audit`) over the crate's `src/` tree and merge the results. Returns a markdown summary, per-lint issue counts (`issues_by_lint`), the raw report from each lint under its name, deduplicated `parse_errors`, and a `total_issues` count. `reinvented_widget` and `components_audit` findings are hints — counted in `issues_by_lint` but not surfaced as errors. Use `include` / `exclude` to scope (e.g. `include: [\"check_rsx\", \"signal_lint\"]`), and `dead_component_roots` to mark extra components alive."
    )]
    async fn lint_project(
        &self,
        Parameters(p): Parameters<tools::lints::lint_project::LintProjectParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::lints::lint_project::lint_project(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "One-shot project tour: feature-flag audit + route map + component/server-fn index + asset audit, plus a pre-rendered markdown summary. Use `include`/`exclude` to scope, `max_items_per_section` to cap output."
    )]
    async fn project_tour(
        &self,
        Parameters(p): Parameters<tools::inspect::project_tour::ProjectTourParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::inspect::project_tour::project_tour(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Index every #[component] and #[server] function in the crate. Returns each symbol's name, file:line, signature (props/args + types, optional flag), and for server fns the unwrapped ServerFnResult<T> return type."
    )]
    async fn project_index(
        &self,
        Parameters(p): Parameters<tools::inspect::project_index::ProjectIndexParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::inspect::project_index::project_index(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "List every route in the project's #[derive(Routable)] enum: URL path (raw + nest-prefixed), target component, params, and any #[layout(...)] / #[nest(...)] it's nested under."
    )]
    async fn route_map(
        &self,
        Parameters(p): Parameters<tools::inspect::route_map::RouteMapParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::inspect::route_map::route_map(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Explain the reactive graph of a Dioxus component: which use_signal / use_memo / use_resource / use_effect bindings exist and which signals each one reads."
    )]
    async fn explain_signal_graph(
        &self,
        Parameters(p): Parameters<tools::inspect::explain_signal_graph::ExplainSignalGraphParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::inspect::explain_signal_graph::explain_signal_graph(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    // create_component / create_route / create_server_fn used to be exposed
    // as MCP tools but were almost always the wrong call — `get_dsl_spec` +
    // `execute_code` are the supported scaffold path. Removed from the tool
    // surface; the underlying `tools::scaffold::*` functions are still used
    // by `execute_code` internally to materialize each DSL primitive.

    #[tool(
        description = "Call this BEFORE `execute_code` whenever the user asks to build, scaffold, add, or create anything in a Dioxus 0.7 project — a model, a screen, a server fn, a full CRUD slice, or a whole app. Returns the YAML DSL vocabulary used by `execute_code`. Pass `extensions: [\"crud\", \"realtime\", \"auth\"]` to include extra primitive groups; empty / omitted returns core only (Model, Store, ClientStore, Resource, Component, Screen, ServerFn). Each primitive lists its fields and a runnable example. The Resource primitive expands into a model+store+server-fn+screens slice in one entry — prefer it for server-backed features. ClientStore + Screen `kind: client_crud` covers client-only in-memory state with no server fn round-trip. For a non-CRUD screen the templates don't cover (markdown editor, dashboard, custom canvas), use Screen `kind: freeform` and write the rsx body in `template.body` — don't shoehorn it into `client_crud` (that yields a todo-shaped app)."
    )]
    async fn get_dsl_spec(
        &self,
        Parameters(p): Parameters<tools::dsl::GetDslSpecParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::dsl::get_dsl_spec(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "List the official Dioxus 0.7 component catalog (45 widgets installable via `dx components add <name>`). Returns each entry's snake_case name, one-line description, and `use crate::components::...;` import path. Pass `query` to filter by case-insensitive substring match against name OR description (e.g. `query: \"date\"` returns calendar + date_picker). Cheaper than calling `get_dsl_spec { sections: [components] }` when you just want to pick a widget; the spec section wraps the same catalog in authoring guidance you don't need here."
    )]
    async fn list_components(
        &self,
        Parameters(p): Parameters<tools::dsl::ListComponentsParams>,
    ) -> Result<CallToolResult, McpError> {
        let reg = self.state.registry();
        match tools::dsl::list_components(&reg.components, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Return the merged theme/component/layout registry (built-in defaults overlaid by runtime descriptors) as JSON. Primarily for the embedded cockpit: drives the theme selector, the navigator's per-layout labels/ranks, and the generic screen preview. Loaded fresh from disk on every call, so descriptors hot-reload (no server restart). Add `*.toml` descriptors under `~/.config/dioxus-mcp/registry/{themes,components,layouts}/` (canonical — applies to every project regardless of cwd) or `<project_root>/.dioxus-mcp/registry/...` (project-specific, highest precedence; or set `DIOXUS_MCP_REGISTRY_DIR`)."
    )]
    async fn get_registry(
        &self,
        Parameters(p): Parameters<tools::dsl::GetRegistryParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::dsl::get_registry(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Map a user prompt to catalog widgets. Pass the user's verbatim ask as `prompt` — the matcher scans for UI-primitive keywords (drag/dialog/combobox/calendar/toast/menu/tabs/etc.) and returns the canonical Dioxus 0.7 catalog entries that cover the request. Use this BEFORE writing event handlers for anything that looks like a UI primitive: a positive hit avoids hand-rolling drag listeners, modal trap-focus, autocomplete logic, etc. Empty `components` means no keywords matched — fall back to `list_components`."
    )]
    async fn suggest_components(
        &self,
        Parameters(p): Parameters<tools::dsl::SuggestComponentsParams>,
    ) -> Result<CallToolResult, McpError> {
        let reg = self.state.registry();
        match tools::dsl::suggest_components(&reg.components, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Verify a project's `dx components add` wiring. Reports which one-time setup steps are still missing (`mod components;` in src/main.rs or src/lib.rs, `asset!(\"/assets/dx-components-theme.css\")` mounted in the App, `src/components/` directory present). Returns `fully_wired: bool`, a `missing: [step_id]` summary, and a `steps: [...]` list with each step's `ok`, the paths it looked at (`looked_in`), and the exact fix line + paste location when `ok: false`. Use this after `dx components add` (or after the user reports compile errors about an unresolved `crate::components` path) to finish wiring without re-running the CLI."
    )]
    async fn verify_install(
        &self,
        Parameters(p): Parameters<tools::dsl::VerifyInstallParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::dsl::verify_install(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Full prop / event surface for a Dioxus 0.7 catalog component (the data needed to author rsx! against it). Returns the component fn signature, every prop (name, type, optional?, has_default, default expression, extends targets, doc comment), every variant enum (e.g. ButtonVariant + its #[default]), aggregated `extends` and `event_handlers` lists, plus `ambiguous_attributes` (E0034 setters that need the literal-string form) and `referenced_enums` (variants for enum types referenced inside any prop type, e.g. `CheckboxState`). When the wrapper just forwards `props: SomeProps` the primitive's props are promoted to the top-level `props` list and `props_source: \"primitive\"` is set so the first read isn't misleadingly empty. Reads from the upstream cargo git checkout (~/.cargo/git/checkouts/components-*) when available, otherwise falls back to the project-local install at `src/components/<name>/component.rs`. Call this BEFORE writing rsx! that uses a catalog widget — saves 5+ file reads per widget."
    )]
    async fn describe_component(
        &self,
        Parameters(p): Parameters<tools::dsl::DescribeComponentParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::dsl::describe_component(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Use this whenever the user asks to build, scaffold, add, or create anything in a Dioxus 0.7 project — a model, a screen, a server fn, a full CRUD slice, or a whole app. Materializes a file set from a single YAML DSL doc (see `get_dsl_spec`). For a non-CRUD UI the templates don't cover, use a Screen with `kind: freeform` and write the body in `template.body`. Pre-flights name collisions across the whole doc; rejects unknown fields, multi-document YAML, and missing cross-refs (List/Table → ServerFn, Feed → Socket). On success returns the merged ScaffoldResult with files_created, files_modified, next_steps, and (when applicable) collisions. \
\
Flags: pass `dry_run: true` to compute a plan (`would_create` / `would_modify`) without writing anything. Pass `if_missing: true` to skip primitives whose target leaf file already exists (reported in `collisions`) instead of erroring — makes re-runs during iteration safe."
    )]
    async fn execute_code(
        &self,
        Parameters(p): Parameters<tools::dsl::ExecuteCodeParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::dsl::execute_code(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Human-in-the-loop alternative to execute_code: submit a DSL doc as a PROPOSAL instead of writing files. It's previewed in the dx-playground cockpit where a human can preview, EDIT the DSL, and Approve or Reject. Blocks up to `wait_secs` (default 300, max 540) for the decision; on timeout returns `{status:\"pending\", proposal_id}` to poll with check_proposal. On approval returns the REAL ScaffoldResult plus `executed_code` — the DSL that ACTUALLY ran, which may differ from yours if the human edited it; treat `executed_code` as ground truth, not your original proposal. Use this when the user asks you to propose changes for their approval / review before writing."
    )]
    async fn propose_scaffold(
        &self,
        Parameters(p): Parameters<tools::dsl::ProposeScaffoldParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::dsl::propose_scaffold(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "List scaffold proposals awaiting a human decision (the cockpit inbox). Returns each proposal's id, created_at, original DSL `code`, and dry-run `preview`. Pass `include_resolved: true` to also see resolved ones. Primarily called by the dx-playground UI."
    )]
    async fn list_proposals(
        &self,
        Parameters(p): Parameters<tools::dsl::ListProposalsParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::dsl::list_proposals(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Resolve a scaffold proposal (called by the human via the cockpit). action=\"approve\" runs execute_code(dry_run:false) on `edited_code` if given (the round-trip edit) else the original; action=\"reject\" discards it. Wakes any blocked propose_scaffold call with the outcome."
    )]
    async fn resolve_proposal(
        &self,
        Parameters(p): Parameters<tools::dsl::ResolveProposalParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::dsl::resolve_proposal(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Poll a scaffold proposal's status/result by id — the non-blocking counterpart to propose_scaffold when it returns `pending`. Returns the same applied/rejected/failed/pending shapes (incl. `executed_code` on success)."
    )]
    async fn check_proposal(
        &self,
        Parameters(p): Parameters<tools::dsl::CheckProposalParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::dsl::check_proposal(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Live-search dioxuslabs.com docs (scoped to the project's Dioxus version) and return ranked snippets. 15-min cache."
    )]
    async fn search_docs(
        &self,
        Parameters(p): Parameters<tools::docs::search_docs::SearchDocsParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::docs::search_docs::search_docs(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Find Dioxus examples — official ones in DioxusLabs/dioxus on GitHub, merged with a small local registry of pattern examples that the upstream repo doesn't ship a folder for (e.g. `optimistic-with-reconcile`). Pass `concept` to rank by name + blurb match ('router', 'fullstack', 'use_signal'); omit it for an alphabetically-sorted listing of every example. Each hit carries `kind: \"upstream\"` (browsable via `url` / `raw_url`) or `kind: \"local\"` (inline `body:` field with paste-ready Rust source; no follow-up fetch needed). `limit` defaults to 3 with a concept, 100 without."
    )]
    async fn find_example(
        &self,
        Parameters(p): Parameters<tools::docs::find_example::FindExampleParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::docs::find_example::find_example(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Generate an OpenAPI 3.1 spec from #[server] functions (POST endpoints) and, optionally, router routes (GET). Schemas for arg/return types are walked from local #[derive(Serialize)] / #[derive(Deserialize)] structs and enums; unresolved type names are reported. Server fns with a `cookies:` extractor emit `parameters[in: cookie]` plus a `security` ref to a `sessionCookie` scheme (named via `session_cookie_name`, default `session_id`). Defaults: server_fn_prefix=\"/api\", include_routes=false."
    )]
    async fn openapi_spec(
        &self,
        Parameters(p): Parameters<tools::audit::openapi_spec::OpenapiSpecParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::audit::openapi_spec::openapi_spec(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Cross-references `route_map` guards with server-fn cookie extractors and reports per-route + per-server-fn auth status. Surfaces likely mismatches: gated routes whose backing handlers don't check cookies, or cookie-gated handlers behind unguarded routes. Returns `routes[]` (with `guards`, `gated`), `server_fns[]` (with `cookie_gated`), headline counts, and a `mismatches[]` block. A clean report (empty `mismatches`) means client + server agree about which slices need a session."
    )]
    async fn auth_map(
        &self,
        Parameters(p): Parameters<tools::audit::auth_map::AuthMapParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::audit::auth_map::auth_map(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Read runtime events captured by the dioxus-mcp-probe crate. Tails target/dioxus-mcp/events.jsonl and returns events matching the filters: kind (render | signal_write | signal_read | server_fn | route | panic | event), since (RFC 3339 cutoff, default last 5 min), component, signal, server_fn, limit (default 200, hard cap 2000). Returns an empty list with a clear note if the probe hasn't been installed yet. \
\
USE THIS (don't ask the user to paste logs) when they ask things like: \"Was there a panic? Where did it happen?\", \"Did the app crash?\", \"Which signals wrote in the past minute?\", \"Show the last few renders of <Component>\", \"List server-fn calls for <name>\", \"What navigations happened?\", \"Tail the runtime log\". \
\
If the user references \"the last run\" or a specific log file, pass `log_path` and widen `since` (default cutoff is only 5 min back). On \"no Cargo.toml from project root\", set `project_root` to the actual Dioxus app directory."
    )]
    async fn runtime_events(
        &self,
        Parameters(p): Parameters<tools::runtime::runtime_events::RuntimeEventsParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::runtime::runtime_events::runtime_events(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Per-server-fn latency summary derived from the dioxus-mcp-probe log. Pairs phase=start with phase=end by call_id and returns count, ok/err, and min/p50/p95/max latency in microseconds for each #[server] fn called in the window. Filters: since (RFC 3339, default last 5 min), server_fn (one name only), log_path (override). \
\
USE THIS when the user asks: \"What's the latency distribution for <fn>?\", \"Which server fns are slowest?\", \"Are any server fns erroring?\", \"How many <fn> calls ran and how many failed?\", \"What's still pending mid-flight?\", \"Summary of server-fn activity over the last N minutes\", \"Show p95 latency for every server fn\"."
    )]
    async fn server_fn_summary(
        &self,
        Parameters(p): Parameters<tools::runtime::server_fn_summary::ServerFnSummaryParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::runtime::server_fn_summary::server_fn_summary(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Run `cargo check` against the Dioxus project with a structured diagnostic shape — the closing-the-loop step after `execute_code`. Auto-picks a sensible feature combo (no extras when `fullstack` is already on the dep, `server` for the canonical 0.7 `default=[\"web\"]` + opt-in `server` sibling, `web,server` for older layouts) or accepts an explicit `features:` list. **Runs BOTH legs by default** (host check + `--target wasm32-unknown-unknown`) so `dx serve`-only wasm errors don't slip past a green host build; pass `target_wasm: false` to run only the host leg (faster, fine for pure-server changes) or `target_wasm: true` to run only the wasm leg. **Quick mode** (`quick: true`) keeps both legs scheduled but short-circuits the wasm leg when the host leg fails — fast-fail without the caller having to remember to flip `target_wasm: false` on subsequent runs. Per-leg detail lands in `legs[]`; `status` aggregates worst-of (host pass + wasm fail → `failed`). Parses `--message-format=json` and returns separate `errors` / `warnings` lists with file/line/column/code + cargo's pre-rendered diagnostic text; both lists are capped via `max_messages` (default 20), and `truncated: true` signals when caps fired. `status` is one of `passed | failed | timed_out | spawn_failed`. Default timeout 300s per leg (override with `timeout_secs`). Does NOT shell out to `dx serve`; this is a static compile check, not an end-to-end serve probe."
    )]
    async fn build_and_smoke(
        &self,
        Parameters(p): Parameters<tools::build::build_and_smoke::BuildAndSmokeParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::build::build_and_smoke::build_and_smoke(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }
}

#[tool_handler]
impl ServerHandler for DioxusMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_instructions(
                "Dioxus 0.7 project assistant. When a question maps to a tool, call it.\n\
                 \n\
                 Routing:\n\
                 - Scaffold STRUCTURED slices (model / store / server-fn-backed Resource / \
                   client_crud / whole-app skeleton): `get_dsl_spec` then `execute_code`. \
                   Use `Resource` for a full server-backed slice; `ClientStore` + \
                   `kind: client_crud` for in-memory state. For a non-CRUD screen the \
                   templates don't cover (markdown editor, dashboard, canvas), use Screen \
                   `kind: freeform` and author the body in `template.body` — you still get \
                   the route / module / App wiring. For a single-component edit, skip the \
                   DSL and write the file directly; `execute_code` is for multi-file, \
                   cross-wired primitives.\n\
                 - UI primitive widgets (button / dialog / date-picker / drag-to-reorder / \
                   combobox / toast / etc.): BEFORE writing any handler code, \
                   `list_components` (or `suggest_components { prompt: \"...\" }` with the \
                   user's verbatim ask) to scan the catalog, then `dx components add <name>` \
                   from the project root. Call `describe_component` for the full prop / \
                   event surface before authoring rsx! that uses it. If you find yourself \
                   hand-rolling event listeners for a UI primitive that the catalog likely \
                   covers (drag, sortable, autocomplete, calendar, modal, toast), stop and \
                   check the catalog first.\n\
                 - Runtime behavior (panics, renders, signal writes, navigations) -> \
                   runtime_events. Server-fn latency / errors -> server_fn_summary.\n\
                 - Project structure (what routes / components / server fns exist) -> \
                   route_map, project_index, project_tour, server_fn_call_graph.\n\
                 - Static analysis (dead code, prop drilling, signal/props lints, \
                   reinvented widgets, optimistic-lock gates, hand-rolled catalog \
                   class-shapes, asset audit, feature flags, OpenAPI) -> dead_components, \
                   prop_drill, signal_lint, props_lint, reinvented_widget, \
                   optimistic_lock_gate, components_audit, asset_audit, audit_feature_flags, \
                   openapi_spec, explain_signal_graph, lint_project.\n\
                 - Docs / canonical examples -> search_docs, find_example. RSX check -> \
                   check_rsx. Catalog widget prop / event surface -> describe_component.\n\
                 \n\
                 Probe note: runtime_events + server_fn_summary read a JSONL log written \
                 by the dioxus-mcp-probe crate. Pass `project_root` when cwd isn't the app; \
                 widen `since` (default 5 min) for older runs."
                    .to_string(),
            )
    }
}

pub fn ok_json<T: serde::Serialize>(value: &T) -> Result<CallToolResult, McpError> {
    let s = serde_json::to_string_pretty(value)
        .map_err(|e| McpError::internal_error(format!("serialize: {e}"), None))?;
    Ok(CallToolResult::success(vec![Content::text(s)]))
}

pub fn err(msg: impl Into<String>) -> McpError {
    McpError::invalid_request(msg.into(), None)
}
