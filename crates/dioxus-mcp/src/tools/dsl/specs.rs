pub(super) const SPEC_VERSION: &str = "1";

pub(super) const CORE_PREAMBLE: &str = r#"# Dioxus-MCP DSL spec
#
# Author a YAML doc using these primitives, then call execute_code with the
# whole doc as a string. The tool parses, pre-flights collisions, and emits
# Dioxus 0.7 source files in one shot.
#
# >>> Slim-fetch hints (READ FIRST — this full spec is ~10KB):
#   - Pass `index_only: true` to get a one-line-per-primitive index of every
#     section name and its purpose. Use that to decide which sections you
#     actually need, then re-fetch only those.
#   - Pass `sections: [model, client_store, ...]` to fetch only the named
#     sections (extension blocks are auto-included). Most authoring sessions
#     touch 2–4 primitives — pulling the whole spec wastes tokens.
#   - Pass `extensions: [crud, realtime, auth]` to also include extension
#     blocks. Omit for the core surface only.
#   - Pass `include_prologue: false` to drop this preamble on follow-up calls
#     once you've already seen it (saves ~5KB per call).
#   - Pass `include_examples: false` to strip the per-primitive `example:`
#     blocks when you only need the field schema.
#
# Top-level shape:
#   version: "1"
#   <primitive_section>: [ ... ]   # see core/extensions below
#
# Data-layer-only path (no UI) — the escape hatch for "scaffold types,
# hand-write UI":
#   Every section is optional. If you only want types and state plumbing
#   generated — say, models + a client_store, or models + server_fns — omit
#   `screens:` entirely and execute_code will generate exactly the requested
#   primitives without touching the router. This is the recommended shape when
#   you want hand-rolled UI on top of generated data types: scaffold the data
#   layer here, then write your components against `crate::model::*` /
#   `crate::state::*` directly. No `screens:` means no Routable mutation and
#   no `Router::<...>` injection.
#
#   Two mutations still happen automatically so the generated code compiles
#   and is reachable from your own UI:
#     - `pub mod model;` / `pub mod state;` / etc. are added to the crate root
#       (src/main.rs or src/lib.rs) for every top-level subdir we wrote into.
#     - When `client_stores:` is declared, the matching `provide_{snake}()`
#       calls are spliced into the top of your `fn App()` body (idempotent —
#       skipped if `provide_{snake}()` already appears in the file). Without
#       this, `use_{snake}()` would panic at runtime in the UI you add later.
#       To opt out, omit `client_stores:` and call `provide_{snake}()`
#       yourself, or strip the inserted line after the run.
#
# Pruning `dx new` starter boilerplate:
#   Pass top-level `prune_dx_new_starter: true` (sibling of `screens:`, etc.)
#   to auto-remove the demo Hero component + Home route that ship with `dx
#   new`. Targets that aren't present are silent no-ops, so the flag is safe
#   to leave on across iterative re-runs. Equivalent to writing:
#     remove:
#       - {kind: remove_component, component: Hero}
#       - {kind: remove_route, variant: Home}
#   …but without forcing you to hand-author the entries when the doc's
#   intent is just "start clean from the dx new template."
#
# Auto-router bootstrap:
#   If the doc declares any `screens:` / `login_screens:` entry and no
#   `#[derive(Routable)]` enum exists anywhere under src/, execute_code
#   auto-creates `src/router.rs` (seeded with every declared route) and adds
#   `pub mod router;` to the crate root. This makes `dx new` → execute_code
#   work in one call — DO NOT pre-write a Routable enum or hand-roll a
#   `router.rs` file. The bootstrap also surfaces a `next_step` reminding you
#   to mount it (`Router::<crate::router::Route> {}` in your App body), and
#   leaves the existing enum untouched on re-runs.
#
# Picking the right tool for CRUD-like UIs:
#   - Client-only / in-memory apps (todo lists, drafts, ephemeral selections):
#     use a `client_stores:` entry + a `screens:` entry with
#     `template.kind: client_crud`. No server fns are generated and no
#     `server` feature is required. This is almost always what you want for
#     web-only / wasm-target projects without a backend.
#   - Backend-backed CRUD (a real database, REST endpoint, etc.): use a
#     `resources:` entry — it expands to a model + server-feature-gated store
#     + 5 server fns + 2 screens — or hand-author a Model + Store + server
#     fns + a `screens:` entry with `template.kind: resource_list` /
#     `resource_form`. The `crud` extension exposes Form/List/Table component
#     templates that pair with those server fns; do NOT load the `crud`
#     extension for a client-only app — `client_crud` already covers it.
#
# Official component catalog (READ BEFORE scaffolding any UI widget):
#   45 production-ready Dioxus 0.7 components ship via `dx components add
#   <name>` — button, dialog, calendar, dropdown_menu, combobox, sheet,
#   sidebar, tooltip, and more. See the `Components:` section below for the
#   full list. PREFER one of these over hand-authored rsx! or a custom
#   `Component:` entry whenever the user asks for a UI primitive that's in
#   the catalog — the official versions ship with ARIA, keyboard handling,
#   theming, and state plumbing already wired. To pull just the catalog
#   without the rest of the spec: `get_dsl_spec { sections: [components],
#   include_prologue: false }`.
#
# Keep-the-wiring, rewrite-the-body workflow:
#   When a prompt asks for a "designed" or custom-styled Screen, do NOT skip
#   the Screen primitive in favor of hand-writing the file from scratch. The
#   route variant insert, component file + mod.rs entry, App-body
#   `provide_*` + Router wiring, and (for resource templates) the server-fn
#   binding are the bulk of the cost — and they're idempotent. The rsx!
#   markup is the cheap part to rewrite. Scaffold the Screen, then open the
#   generated file (each Screen emits a `next_steps` entry naming the
#   rsx! line) and rewrite the body in place. Use `dry_run: true` to see the
#   default body via the response's `previews:` map before committing.
#
# All field names are case-sensitive. Unknown fields are rejected.
#
# File layout (the blast radius of one execute_code call):
#   src/components/{snake}.rs        Component, Form, List, Table, Feed,
#                                    LoginScreen, ProtectedRoute, Screen
#   src/components/mod.rs            entries added (sorted), file created if missing
#   src/model/{snake}.rs             Model (struct with serde derives)
#   src/model/mod.rs                 entries added (sorted)
#   src/state/{snake}.rs             Store (server-feature gated CRUD helper)
#   src/state/mod.rs                 entries added (sorted)
#   src/server/{snake}.rs            ServerFn (incl. resource-synthesized fns)
#   src/server/mod.rs                entries added (sorted)
#   src/signals/{snake}.rs           Signal
#   src/signals/mod.rs               entries added (sorted)
#   src/sockets/{snake}.rs           Socket
#   src/sockets/mod.rs               entries added (sorted)
#   src/auth/{snake}.rs              SessionState
#   src/auth/mod.rs                  entries added (sorted)
#   the project's #[derive(Routable)] enum file
#                                    new variants inserted for Screen / LoginScreen
#                                    (deduplicated by variant name; resources
#                                    emit a list + new screen per entry)
#
# Re-run semantics:
#   - By default execute_code REFUSES to overwrite an existing leaf file —
#     models, components, server fns, signals, sockets, stores, and session
#     states all hard-error with the conflicting path when their target
#     already exists. Pass `if_missing: true` to silently skip every
#     already-present primitive instead; the response lists each skipped
#     path under `collisions`. Re-runs adding one new field to a model
#     while leaving every other primitive in place are safe with
#     `if_missing: true`.
#   - Route inserts are name-keyed and idempotent: a Screen / LoginScreen whose
#     variant name already exists is skipped.
#   - mod.rs entries are inserted alphabetically; re-runs produce stable diffs.
#   - `modify:` entries are idempotent: a field/prop/arg already present in the
#     target item is skipped, and a re-run with no new additions writes nothing.
#     Missing target files error unless `if_missing: true` (then recorded under
#     `collisions`). Modify runs *after* all create steps in the same call.
#   - Pass `dry_run: true` to compute `would_create` + `would_modify` (plus any
#     `collisions`) without touching disk.
#
# Partial-failure semantics:
#   - execute_code is NOT transactional across primitives. Pre-flight catches
#     dup names / dup route paths / cross-reference errors before any write,
#     but once writes start, a mid-run error (e.g. one screen's template
#     references an undeclared field that only surfaces at render time) leaves
#     prior primitives already on disk. The response's `files_created` /
#     `files_modified` lists exactly what landed before the error.
#   - The top-level `status` field summarizes the outcome:
#       - "applied"   — every requested primitive was emitted cleanly
#       - "partial"   — some emitted, some collided (under `if_missing: true`)
#       - "no_changes" — every requested primitive collided
#       - absent      — the call errored mid-run; check the error message and
#                       inspect `files_created` to see how far the write got
#   - Recovery: re-run with `if_missing: true` to skip everything that landed,
#     fix the offending primitive, and resume. The router / App / Cargo.toml
#     wiring is idempotent and converges across re-runs.
"#;

pub(super) const CORE_MODEL: &str = r#"  Model:
    description: "A shared data type with serde derives. Generates src/model/{snake}.rs and exposes the struct as crate::model::{Pascal}. Server fns can name it in their return_type (e.g. `Vec<Product>`); forthcoming `store:` and `resource:` primitives will consume it directly. Project must depend on `serde = { version = \"1\", features = [\"derive\"] }`. AUTO-DEFAULT: when a `client_crud` Screen references this model (directly or via its `client_stores:` entry), `Default` is auto-added to `derives:` before scaffolding — the `client_crud` body uses `..Default::default()` on the push call. The patch covers both in-doc models and existing on-disk model files at `src/model/{snake}.rs`. You don't need to add `Default` yourself."
    fields:
      - {name: name, type: string, required: true}
      - {name: fields, type: "ModelField[] — each {name, type, optional?}", required: true}
      - {name: derives, type: "string[] — extra derives appended after Debug, Clone, PartialEq, Serialize, Deserialize", required: false}
    example:
      models:
        - name: Product
          fields:
            - {name: id, type: i64}
            - {name: name, type: String}
            - {name: description, type: String, optional: true}
          derives: [Eq, Hash]
"#;

pub(super) const CORE_COMPONENT: &str = r#"  Component:
    description: "A reusable UI element. Generates src/components/{snake}.rs. BEFORE scaffolding: if the user wants a button, dialog, calendar, dropdown, combobox, sheet, sidebar, tooltip, or any other widget listed in the `Components:` catalog, install that with `dx components add <name>` instead — the official version ships with accessibility and styling already wired and you'd just be reinventing it. Use this primitive for app-specific composites (UserCard, ProductTable, OrderRow). The `template` field picks a stub-body skeleton — omit (or `empty`) for the historical placeholder div. Other kinds: `form` (form + submit handler), `list` (ul with empty-state), `crud_table` (table + toolbar), `resource_view` (article with field list + edit/delete actions). Templates are structural only — they don't bind to any data; pair with `props:` and edit afterwards. For data-bound screens use `screens:` with a Screen template instead."
    fields:
      - {name: name, type: string, required: true}
      - {name: props, type: "PropDef[]", required: false}
      - {name: template, type: "empty|form|list|crud_table|resource_view (default: empty)", required: false}
    template_kinds: [empty, form, list, crud_table, resource_view]
    example:
      components:
        - name: UserCard
          props:
            - {name: id, type: i32}
            - {name: label, type: String, optional: true}
        - name: ProductTable
          template: crud_table
"#;

pub(super) const CORE_SCREEN: &str = r#"  Screen:
    description: "A top-level routed view. Generates a component file and inserts a route variant in src/router.rs. The `wrap_with` field lets a guard like ProtectedRoute sit at the route layer. The `template` field selects the emitted body — omit it for an empty placeholder; kind=empty with `store:` set wires `use_<store>()` so you get the context plumbing without committing to the stock UI; kind=resource_list / kind=resource_form bind to server fns (use these for backend-backed CRUD); kind=client_crud binds to a `client_stores:` entry and emits add/toggle/delete handlers entirely client-side (no server fn needed — ideal for in-memory apps like todo lists). All template kinds accept `class:` to override the root `div`'s class string when the host project uses a design system (e.g. Tailwind) that conflicts with the default `\"screen {name}\"` pair. WORKFLOW: scaffolding the Screen is still net-positive even when you plan to redesign the rsx! body — the route variant insert, the component file + mod.rs entry, the App-body provide_*/Router wiring, and (for resource templates) the server-fn binding are the bulk of the work. After running execute_code, open the file (next_steps gives `src/components/{snake}.rs:LINE` for the rsx! block) and rewrite the markup in place; the wiring stays correct. Use dry_run: true to preview the body via `previews:` before deciding."
    fields:
      - {name: name, type: string, required: true}
      - {name: route, type: string, required: true}
      - {name: wrap_with, type: "ComponentName (e.g. a ProtectedRoute guard)", required: false}
      - {name: template, type: "ScreenTemplate {kind, endpoint?, item_type?, on_submit?, redirect_to?, fields?, store?, label_field?, checkbox_field?, class?, body?, styled?}. `body: empty` (alias `body: stub`) on kind=empty drops the placeholder div+h1, emitting a bare `rsx! {}` so you can immediately rewrite the body without churn — imports and `use_<store>()` wiring stay intact. `styled: tailwind` on kind=client_crud emits Tailwind utility-classed defaults instead of the bare class names — pick this in projects where Tailwind is already wired up.", required: false}
      - {name: replace_route, type: "bool — when true, if `route:` is already mapped by a different variant in the on-disk Routable enum, that variant is removed first (as if you had added a matching `remove: [{kind: remove_route, ...}]` entry). Lets a fresh Screen take over a route from a `dx new` demo without a two-step edit.", required: false}
    template_kinds: [empty, resource_list, resource_form, client_crud]
    client_crud_fields:
      - {name: store, type: "ClientStore name in this doc", required: true}
      - {name: item_type, type: "Rust item type (Model in this doc or a built-in like String)", required: true}
      - {name: label_field, type: "Field on the item the add input writes / the row label reads", required: true}
      - {name: checkbox_field, type: "Optional bool field rendered as a per-row checkbox", required: false}
      - {name: styled, type: "Optional design-system preset. `tailwind` emits Tailwind-classed defaults (form, input, list, buttons, checkbox). `vanilla-css` emits semantic class names (`.compose`, `.list`, `.row`, `.field`, `.toggle`, `.delete`, `.title`, `.label`) AND writes a starter `assets/{snake}.css` stylesheet so you don't start from a blank file — mount it via `document::Stylesheet { href: asset!(\"/assets/{snake}.css\") }` in App. Omit for the historical unstyled class names.", required: false}
      - {name: compose_style, type: "Shape of the add-form's submit affordance. `submit_button` (default) keeps the visible `Add` button. `enter_only` drops the button so the form is submitted only by pressing Enter — pick this for TodoMVC-shaped apps where the row UX is type-and-press-Enter.", required: false}
    client_crud_auto_default: "When this Screen kind is used, `Default` is auto-added to the referenced Model's `derives:` (whether the Model is declared in this doc or already on disk under `src/model/{snake}.rs`) — the generated push uses `..Default::default()`. Don't pre-emptively add `Default` yourself."
    example:
      screens:
        - name: HomeScreen
          route: /
          wrap_with: Dashboard
        - name: ProductListScreen
          route: /products
          template:
            kind: resource_list
            endpoint: list_products
            item_type: Product
        - name: TodoScreen
          route: /
          template:
            kind: client_crud
            store: TodoStore
            item_type: Todo
            label_field: title
            checkbox_field: done
"#;

pub(super) const CORE_SERVER_FN: &str = r#"  ServerFn:
    description: An Axum-backed server fn using Dioxus 0.7's #[get/post("/path")] attribute. Requires fullstack feature on the dioxus dep.
    fields:
      - {name: name, type: string, required: true}
      - {name: args, type: "ArgDef[]", required: false}
      - {name: return_type, type: "string — the INNER type only; do NOT wrap in Result<_, ServerFnError> or ServerFnResult<_>, the template adds that wrapper for you. Wrapping is rejected with a clear error.", required: false}
      - {name: method, type: "get|post (defaults: post if args else get)", required: false}
      - {name: path, type: "string (default: /api/{snake_name})", required: false}
    example:
      server_fns:
        - name: fetch_users
          args:
            - {name: limit, type: u32}
          # Pass the inner type, not Result<Vec<String>, ServerFnError>.
          return_type: "Vec<String>"
          method: post
          path: /api/users
"#;

pub(super) const CORE_STORE: &str = r#"  Store:
    description: "A typed in-memory CRUD helper over a Model. Generates src/state/{snake}.rs as a server-only file (gated with `#![cfg(feature = \"server\")]`) exposing `{Pascal}Store::global()` with list/get/create/update/delete methods. The model must have an integer id field (default name `id`, default type `i64`). Pair with server fns that call into `{Pascal}Store::global()` to expose the store over the wire. The top-level `resources` primitive emits a model+store+server-fn slice in one entry — and forces `emit_tests: true` for the synthesized store."
    fields:
      - {name: name, type: string, required: true}
      - {name: resource, type: "Model name in this doc (or synthesized by resources:)", required: true}
      - {name: kind, type: "in_memory (default). sqlite is reserved.", required: false}
      - {name: id_field, type: "string (default \"id\")", required: false}
      - {name: id_type, type: "string (default \"i64\")", required: false}
      - {name: emit_tests, type: "bool (default false) — appends a `#[cfg(test)] mod tests` block exercising the CRUD methods. Requires the referenced model to derive Default. Set automatically by `resources:`.", required: false}
    example:
      models:
        - name: Product
          fields:
            - {name: id, type: i64}
            - {name: name, type: String}
          derives: [Default]
      stores:
        - name: ProductStore
          resource: Product
          emit_tests: true
"#;

pub(super) const CORE_CLIENT_STORE: &str = r#"  ClientStore:
    description: "A typed client-side reactive list. Generates `src/state/{snake}.rs` (no server feature gate) exposing a `Store<{Pascal}>`-backed store via context using Dioxus 0.7's canonical `#[derive(Store)]` + `#[store]` extension methods for path-isolated reactivity — call `provide_{snake}()` once in your root component and `use_{snake}()` from any descendant to get a `Store<{Pascal}>`. Emits `push`, `clear`, and (when `id_field` is set) `remove_by_id` and `update_by_id` helpers via the `#[store] impl` extension trait. With `auto_id: true` the store also owns a monotonic id allocator and exposes `push_new(item)` that assigns the next id before pushing — call sites can drop the id field from the struct literal. When a companion `client_crud` Screen sets `checkbox_field`, the store also gains three derived helpers keyed off that field: `clear_{field}(&mut self)` (drops every item where the bool is set — the canonical \"Clear completed\" action), `remaining(&self) -> usize` (count of items where the bool is false), and `any_{field}(&self) -> bool` (CTA gating helper). Call them straight from `rsx!` (`store.remaining()`, `if store.any_done() { ... }`) — Dioxus tracks the underlying `items()` signal automatically. Pair with a Screen template `kind: client_crud` for one-call todo-style apps. NO server fn round-trip — ideal for in-memory state, todo lists, drafts, ephemeral UI selections."
    fields:
      - {name: name, type: string, required: true}
      - {name: item_type, type: "Rust type (Model in this doc OR a built-in like String / i32). When it matches a Model name, the file emits `use crate::model::{ItemType};`.", required: true}
      - {name: initial, type: "Rust expression for the initial Vec value (default `Vec::new()`)", required: false}
      - {name: id_field, type: "Field name to use for remove_by_id / update_by_id helpers (e.g. `id`). Omit for primitive item types.", required: false}
      - {name: id_type, type: "Rust type of the id field (default `i64`)", required: false}
      - {name: auto_id, type: "bool (default false) — when true the store owns the id allocator and exposes `push_new(item)`. Requires `id_field` and a primitive-integer `id_type`. The companion client_crud screen template detects this and stops emitting its local `next_id` signal.", required: false}
    iteration_patterns: |
      Stock iteration (use this — no clones, no temporaries):
        for item in store.items().read().iter() { ... }
      Inline-filter iteration (read the signal once, filter on the borrow):
        for item in store.items().read().iter().filter(|t| !t.done) { ... }
      DO NOT write `store.items().read().clone()` + `.into_iter()` or
      `.cloned().collect::<Vec<_>>()` to filter — that double-clones every
      item every render. `.iter().filter(...)` on the read guard is what
      you want.
    example:
      models:
        - name: Todo
          fields:
            - {name: id, type: i64}
            - {name: title, type: String}
            - {name: done, type: bool}
      client_stores:
        - name: TodoStore
          item_type: Todo
          id_field: id
          id_type: i64
          auto_id: true
"#;

pub(super) const CORE_VIEW_STATE: &str = r#"  ViewState:
    description: "Local-but-shared view state — a filter enum, sort key, or search string — exposed as a `Signal<T>` via context. Generates `src/state/{snake}.rs` with `provide_{snake}()` (auto-spliced into your `fn App()` body) and `use_{snake}()`. When `enum_variants:` is set, the file also declares `pub enum {type} { Variant1, Variant2, ... }` with `#[derive(Clone, Copy, PartialEq, Eq, Debug)]` — variants are unit-only, so the signal hands them back by value. Use this for filter / sort / mode-style state that the rsx body AND a sibling button both touch; for state only one component reads, write `let mut foo = use_signal(|| ...);` inline. Contrast: `ClientStore` is a `Vec<T>`-shaped Store with push/remove/update helpers; `ViewState` is a single value in a Signal."
    fields:
      - {name: name, type: string, required: true}
      - {name: type, type: "Rust type name. When `enum_variants:` is set, this is the auto-generated enum's name; otherwise it must already resolve (Model, scalar, or imported type).", required: true}
      - {name: initial, type: "Rust expression for the starting value (e.g. `Filter::All`, `String::new()`, `0i32`).", required: true}
      - {name: enum_variants, type: "string[] — when present, generates `pub enum {type} { ... }` with these unit variants. Names are PascalCase-normalized.", required: false}
    example:
      view_states:
        - name: TodoFilter
          type: TodoFilter
          initial: "TodoFilter::All"
          enum_variants: [All, Active, Done]
        - name: SearchQuery
          type: String
          initial: "String::new()"
"#;

pub(super) const CORE_RESOURCE: &str = r#"  Resource:
    description: "A meta-primitive that fans out into a Model + Store + 5 server fns (list/get/create/update/delete) + 3 screens (list at `{route_base}`, new at `{route_base}/new`, edit at `{route_base}/:id/edit`). One entry yields a full CRUD slice. The list screen renders a rich table with edit/delete actions; the new screen submits via create_{snake} and redirects to the list; the edit screen takes an `id` URL param, fetches via get_{snake}, and submits via update_{snake}. The model must declare an integer id field (defaults to `id: i64`)."
    fields:
      - {name: name, type: "PascalCase resource name (Product, Order, …)", required: true}
      - {name: fields, type: "ModelField[] — must contain the id field", required: true}
      - {name: id_field, type: "string (default \"id\")", required: false}
      - {name: route_base, type: "string (default \"/{plural-snake}\"); plural follows `plural` if set, else the built-in algorithm (regular `+s`; `+es` for s/sh/ch/x/z endings; `y → ies` after a consonant)", required: false}
      - {name: plural, type: "string — override the auto-pluralized form for irregular nouns (Person → people, Mouse → mice). Affects the default route_base and the list_{plural} server-fn name.", required: false}
      - {name: derives, type: "string[] forwarded to the synthesized Model", required: false}
    example:
      resources:
        - name: Product
          fields:
            - {name: id, type: i64}
            - {name: name, type: String}
            - {name: description, type: String, optional: true}
        - name: Person
          plural: people
          fields:
            - {name: id, type: i64}
            - {name: name, type: String}
"#;

pub(super) const CORE_REMOVE: &str = r#"  Remove:
    description: "Delete entire on-disk items in one call. Useful when scaffolding into a starter template (`dx new`) to clear demo screens/components before adding your own. Removes run FIRST in an execute_code call (after preflight, before create/modify), so a single doc can replace a demo. Each kind is idempotent — naming a target that's already gone is a silent no-op."
    kinds:
      remove_route:
        description: "Drop a variant from the Routable enum (and its `#[route(...)]` attribute). Component file is left alone — pair with `remove_component` if you want both gone."
        fields:
          - {name: kind, type: "literal `remove_route`", required: true}
          - {name: variant, type: "Variant name (any case — normalized to PascalCase)", required: true}
      remove_component:
        description: "Delete src/components/{snake}.rs and its mod.rs entry. Does NOT touch Routable variants."
        fields:
          - {name: kind, type: "literal `remove_component`", required: true}
          - {name: component, type: "Component name (any case)", required: true}
      remove_model:
        description: "Delete src/model/{snake}.rs and its mod.rs entry."
        fields:
          - {name: kind, type: "literal `remove_model`", required: true}
          - {name: model, type: "Model name (any case)", required: true}
      remove_server_fn:
        description: "Delete src/server/{snake}.rs and its mod.rs entry."
        fields:
          - {name: kind, type: "literal `remove_server_fn`", required: true}
          - {name: server_fn, type: "Server-fn name (any case)", required: true}
    example:
      remove:
        - kind: remove_route
          variant: Home
        - kind: remove_component
          component: Hero
"#;

pub(super) const CORE_MODIFY: &str = r#"  Modify:
    description: "In-place edits to items that already exist on disk. Each entry is idempotent — fields/props/args already present are skipped on add_* kinds, names already absent are skipped on remove_* kinds, and identical re-runs produce no diff. Targets must exist on disk; pass `if_missing: true` on execute_code to skip missing targets (they are recorded under `collisions`) instead of erroring."
    kinds:
      add_model_field:
        description: "Append fields to `crate::model::{Pascal}`'s struct (src/model/{snake}.rs)."
        fields:
          - {name: kind, type: "literal `add_model_field`", required: true}
          - {name: model, type: "Model name (any case)", required: true}
          - {name: fields, type: "ModelField[] — each {name, type, optional?}", required: true}
      add_component_prop:
        description: "Append props to `{Pascal}Props` for a component (src/components/{snake}.rs). Errors if the component doesn't already declare a Props struct — recreate it with `props:` first."
        fields:
          - {name: kind, type: "literal `add_component_prop`", required: true}
          - {name: component, type: "Component name (any case)", required: true}
          - {name: props, type: "PropDef[] — each {name, type, optional?}", required: true}
      add_server_fn_arg:
        description: "Append arguments to a server fn's parameter list (src/server/{snake}.rs)."
        fields:
          - {name: kind, type: "literal `add_server_fn_arg`", required: true}
          - {name: server_fn, type: "Server-fn name (any case)", required: true}
          - {name: args, type: "ArgDef[] — each {name, type}", required: true}
      remove_model_field:
        description: "Delete named fields from `crate::model::{Pascal}`'s struct. Names already absent are silently skipped."
        fields:
          - {name: kind, type: "literal `remove_model_field`", required: true}
          - {name: model, type: "Model name (any case)", required: true}
          - {name: fields, type: "string[] — field names to drop (snake_case at compare time)", required: true}
      remove_component_prop:
        description: "Delete named props from `{Pascal}Props`. Errors only when the file or the *Props struct is missing."
        fields:
          - {name: kind, type: "literal `remove_component_prop`", required: true}
          - {name: component, type: "Component name (any case)", required: true}
          - {name: props, type: "string[] — prop names to drop", required: true}
    example:
      modify:
        - kind: add_model_field
          model: Product
          fields:
            - {name: sku, type: String}
        - kind: add_component_prop
          component: UserCard
          props:
            - {name: avatar_url, type: String, optional: true}
        - kind: add_server_fn_arg
          server_fn: fetch_users
          args:
            - {name: page, type: u32}
        - kind: remove_model_field
          model: Product
          fields: [legacy_code]
        - kind: remove_component_prop
          component: UserCard
          props: [obsolete]
"#;

pub(super) const CRUD_FORM: &str = r#"  Form:
    description: A controlled form component. One use_signal per field, oninput wires to the signal. When on_submit names a server_fn, the form spawns it with the field values; when feeds_into names a List in the same doc, success also resets the form and bumps that list's version signal so it refetches.
    fields:
      - {name: name, type: string, required: true}
      - {name: fields, type: "FieldDef[]", required: true}
      - {name: on_submit, type: "server_fn name (snake_case)", required: false}
      - {name: feeds_into, type: "List name in this doc", required: false}
    field_types: [text, email, password, number, checkbox, textarea]
    example:
      forms:
        - name: SignupForm
          fields:
            - {name: email, type: email, validation: required}
            - {name: password, type: password, validation: required}
          on_submit: handle_signup
          feeds_into: UserList
"#;

pub(super) const CRUD_LIST: &str = r#"  List:
    description: A list backed by a server fn. Uses use_resource + `match items()` and renders loading/error/empty states. If any Form in the same doc has feeds_into pointing at this list, the generator also emits provide_{snake}_version()/use_{snake}_version() helpers and re-runs the resource when the version signal bumps.
    fields:
      - {name: name, type: string, required: true}
      - {name: endpoint, type: string, required: true}
      - {name: item_type, type: string, required: true}
    example:
      lists:
        - name: UserList
          endpoint: fetch_users
          item_type: String
"#;

pub(super) const CRUD_TABLE: &str = r#"  Table:
    description: A tabular display backed by a server fn with sortable columns (sort signal scaffolded).
    fields:
      - {name: name, type: string, required: true}
      - {name: endpoint, type: string, required: true}
      - {name: item_type, type: string, required: true}
      - {name: columns, type: "ColumnDef[]", required: true}
    example:
      tables:
        - name: UserTable
          endpoint: fetch_users
          item_type: String
          columns:
            - {name: id, label: ID}
            - {name: name, label: Name}
"#;

pub(super) const REALTIME_SIGNAL: &str = r#"  Signal:
    description: A global Signal<T> exposed via context. Generates src/signals/{snake}.rs with provider + accessor.
    fields:
      - {name: name, type: string, required: true}
      - {name: type, type: string, required: true}
      - {name: initial, type: "rust expr", required: true}
    example:
      signals:
        - name: counter
          type: i32
          initial: "0"
"#;

pub(super) const REALTIME_SOCKET: &str = r#"  Socket:
    description: A WebSocket binding (web-sys based). Generates src/sockets/{snake}.rs. Add `web-sys = { version = "0.3", features = ["WebSocket", "MessageEvent", "BinaryType", "ErrorEvent"] }` to your Cargo.toml.
    fields:
      - {name: name, type: string, required: true}
      - {name: url, type: string, required: true}
    example:
      sockets:
        - name: chat
          url: wss://example.test/chat
"#;

pub(super) const REALTIME_FEED: &str = r#"  Feed:
    description: A live-updating list component subscribed to a Socket. Generates src/components/{snake}.rs with a Vec<T> signal and onmessage append.
    fields:
      - {name: name, type: string, required: true}
      - {name: socket, type: string, required: true}
      - {name: item_type, type: string, required: true}
    example:
      feeds:
        - name: ChatFeed
          socket: chat
          item_type: String
"#;

pub(super) const AUTH_SESSION: &str = r#"  SessionState:
    description: Global Signal<Option<UserType>> exposed via context for current session. Generates src/auth/{snake}.rs.
    fields:
      - {name: name, type: string, required: true}
      - {name: user_type, type: string, required: true}
    example:
      session_states:
        - name: session
          user_type: String
"#;

pub(super) const AUTH_LOGIN: &str = r#"  LoginScreen:
    description: A login form component plus a route variant. Submitting redirects to redirect_on_success.
    fields:
      - {name: name, type: string, required: true}
      - {name: route, type: string, required: true}
      - {name: redirect_on_success, type: string, required: true}
    example:
      login_screens:
        - name: Login
          route: /login
          redirect_on_success: /
"#;

pub(super) const AUTH_PROTECTED: &str = r#"  ProtectedRoute:
    description: A guard component that calls navigator()+use_effect to redirect to redirect_to when the session is None, otherwise renders children. With `requires` set (or any SessionState present in the doc) the guard imports use_{session}() automatically; otherwise it emits a TODO-comment fallback against a placeholder Signal<bool> context.
    fields:
      - {name: name, type: string, required: true}
      - {name: redirect_to, type: string, required: true}
      - {name: requires, type: "SessionState name in this doc", required: false}
    example:
      protected_routes:
        - name: Dashboard
          redirect_to: /login
          requires: session
"#;

// Informational catalog (NOT a generative primitive — there is no
// `components:` key in the DslDoc). Surfacing it via get_dsl_spec lets the
// agent discover the official Dioxus 0.7 component library without an extra
// tool call or shelling out to `dx components list`. The 45 names below are
// the upstream registry snapshot; refresh by running `dx components list`
// inside any binary crate and copy-pasting the new entries.
pub(super) const CORE_COMPONENTS: &str = r#"  Components:
    description: "Official Dioxus 0.7 component catalog — 45 pre-built, accessible widgets (button, dialog, calendar, dropdown_menu, combobox, sheet, sidebar, tooltip, …) installed via `dx components add <name>`. PREFER these over hand-authored rsx! or a custom `Component:` entry whenever the user asks for a UI primitive that appears in the catalog below — the official versions ship with ARIA, keyboard handling, theming, and state plumbing already wired. This is INFORMATIONAL ONLY: there is no `components:` key in the DSL and execute_code does not install anything; the agent (or user) runs `dx components add <name>` once per component from the project root. First-time install also requires `mod components;` in main.rs and an `asset!(\"/assets/dx-components-theme.css\")` stylesheet — `dx components add` prints both steps after writing files. After install, files live at `src/components/{name}/component.rs` with `pub use component::*;` re-exports, so use as `use crate::components::{name}::{Pascal};` and drop straight into rsx!."
    install: "dx components add <name>     # creates src/components/{name}/, updates src/components/mod.rs"
    import: "use crate::components::{name}::{Pascal};   # e.g. use crate::components::dropdown_menu::DropdownMenu;"
    catalog:
      accordion: "An accordion component for displaying collapsible content sections."
      alert_dialog: "An alert dialog component for displaying important messages and requiring user confirmation."
      aspect_ratio: "An aspect ratio component for maintaining a consistent width-to-height ratio of an element."
      avatar: "An avatar component for displaying user profile images or initials."
      badge: "A small label to display status or categorization."
      button: "A button component for triggering actions or events when clicked."
      calendar: "A calendar grid component for selecting dates."
      card: "A simple card component."
      checkbox: "A togglable checkbox component."
      collapsible: "A collapsible component for showing and hiding content sections."
      color_picker: "Allows selecting a color using a variety of input methods."
      combobox: "An autocomplete input + popover for picking a value from a filterable list of options."
      context_menu: "A context menu component for displaying a list of actions or options after right-clicking an area."
      date_picker: "A date picker component for selecting or inputting dates."
      dialog: "A dialog component for displaying modal content."
      drag_and_drop_list: "A vertically sortable list supporting drag-and-drop, touch, or keyboard input."
      dropdown_menu: "A dropdown menu component for selecting options from a list."
      form: "A form component for collecting user input."
      hover_card: "A hover card component for displaying additional information on hover."
      input: "An input field component for user text entry."
      item: "A component for displaying content."
      label: "An accessible label component for form elements."
      menubar: "A menubar component for a collection of menu items."
      navbar: "A navbar component for navigation between pages."
      pagination: "Navigation controls for paged content."
      popover: "A popover component for collapsible content."
      progress: "An accessible progress-bar indicator."
      radio_group: "A group of radio buttons for selecting one option from a set."
      scroll_area: "A scrollable area component."
      select: "A select dropdown component with typeahead support."
      separator: "A visual separator between different sections of the page."
      sheet: "A sheet component as an edge panel that complements the main content."
      sidebar: "A sidebar component as a vertical panel fixed to the screen edge for quick access to different sections."
      skeleton: "A placeholder component for all loading elements."
      slider: "An accessible slider component."
      switch: "A togglable switch component."
      tabs: "A tabbed interface component."
      textarea: "A textarea component for multi-line text input."
      toast: "A toast notification component."
      toggle: "A simple toggle button component."
      toggle_group: "A group of toggle buttons for selecting one or more options from a set."
      toolbar: "A toolbar component for grouping related inputs."
      tooltip: "A tooltip component for additional information on hover or focus."
      virtual_list: "A virtualized list component for large datasets."
    prop_hints:
      accordion: "forwards AccordionProps; on_change + on_trigger_click events"
      alert_dialog: "forwards AlertDialogRootProps; on_open_change + on_click events; modal confirm dialog"
      aspect_ratio: "forwards AspectRatioProps; layout-only wrapper, no events"
      avatar: "forwards AvatarProps; on_load + on_error + on_state_change; extends GlobalAttributes"
      badge: "forwards BadgeProps; presentational, no events; extends GlobalAttributes"
      button: "inline props (variant: ButtonVariant, size: ButtonSize); onclick + onmousedown/up + onkeydown; extends GlobalAttributes + button"
      calendar: "forwards CalendarProps; on_date_change + on_range_change + on_view_change; extends GlobalAttributes"
      card: "inline children-only wrapper; no events; extends GlobalAttributes"
      checkbox: "forwards CheckboxProps (checked: ReadSignal<Option<CheckboxState>>, default_checked, on_checked_change)"
      collapsible: "forwards CollapsibleProps; on_open_change; extends GlobalAttributes"
      color_picker: "forwards ColorPickerRootProps; on_color_change + on_value_change + on_open_change + oninput; extends GlobalAttributes"
      combobox: "wrapper defines its own ComboboxProps<T = String>; on_value_change + on_query_change + on_open_change; extends GlobalAttributes"
      context_menu: "forwards ContextMenuProps; on_open_change + on_select"
      date_picker: "forwards DatePickerProps; on_value_change + on_range_change + on_format_* placeholders; extends GlobalAttributes"
      dialog: "forwards DialogRootProps; on_open_change; pair with DialogTrigger + DialogContent children"
      drag_and_drop_list: "forwards DragAndDropListProps; pointer/drag/keyboard handlers; extends GlobalAttributes"
      dropdown_menu: "forwards DropdownMenuProps; on_open_change + on_select"
      form: "inline children-only wrapper; submit handler set by caller via ..attributes"
      hover_card: "forwards HoverCardProps; on_open_change"
      input: "inline props with 19 DOM events forwarded individually (oninput, onchange, onfocus, onkeydown, …); extends GlobalAttributes + input"
      item: "inline content wrapper; onclick + onkeydown; extends GlobalAttributes + div + p"
      label: "forwards LabelProps; accessible label for form controls; no events"
      menubar: "forwards MenubarProps; on_select"
      navbar: "forwards NavbarProps; onclick + onmounted + on_select"
      pagination: "forwards PaginationLinkProps; onclick + onmousedown/up; extends GlobalAttributes + a"
      popover: "forwards PopoverRootProps; on_open_change"
      progress: "forwards ProgressProps; presentational; value prop drives the bar"
      radio_group: "forwards RadioGroupProps; on_value_change"
      scroll_area: "forwards ScrollAreaProps; presentational; no events"
      select: "forwards SelectGroupLabelProps; on_value_change + on_values_change + on_open_change"
      separator: "forwards SeparatorProps; presentational; no events"
      sheet: "forwards DialogRootProps; on_open_change + onclick; extends GlobalAttributes"
      sidebar: "inline props; onclick + on_open_change; extends GlobalAttributes + button"
      skeleton: "presentational; no events; extends GlobalAttributes"
      slider: "forwards SliderProps; on_value_change"
      switch: "forwards SwitchProps; on_checked_change"
      tabs: "forwards TabsProps; on_value_change; extends GlobalAttributes"
      textarea: "inline props with 18 DOM events forwarded individually (oninput, onchange, onfocus, onkeydown, …); extends GlobalAttributes + textarea"
      toast: "forwards ToastProps; on_close; extends GlobalAttributes"
      toggle: "forwards ToggleProps; on_pressed_change + onfocus + onkeydown + onmounted"
      toggle_group: "forwards ToggleGroupProps; on_pressed_change emits a HashSet<usize> of the pressed indices"
      toolbar: "forwards ToolbarProps; on_click; extends GlobalAttributes + div"
      tooltip: "forwards TooltipProps; on_open_change"
      virtual_list: "forwards VirtualListProps; render-prop iterator pattern for large datasets"
    describe_component_hint: "Call `describe_component <name>` for the full prop / event surface (every prop's type, defaults, event-handler signatures, the upstream `*Props` struct when applicable, and the docs.md content) — the catalog + prop_hints above are a scan list, describe_component is the authoritative reference."
    install_via_dsl: "Top-level `dx_components: [name1, name2]` list (sibling of `screens:`, `models:`, etc.) declares which catalog entries this scaffold needs. execute_code validates each name against the catalog above and shells out to `dx components add <name>` for each valid entry (per-command 180s timeout). On failure (missing `dx`, network error, non-zero exit) it falls back to surfacing the install command on `next_steps` so the caller still sees what to run. Dry-run mode emits `would run …` previews instead of installing. The first-time `mod components;` + `asset!(\"/assets/dx-components-theme.css\")` reminders are surfaced either way. Example: `dx_components: [button, dialog, calendar]`."
"#;
