# dioxus-mcp TODO

Improvement checklist derived from a real-world build (an inventory management app, May 2026). Each item lists the friction observed and a concrete shape for the fix. Ordered roughly by impact.

## High impact — turn the DSL from "file generator" into "feature generator"

- [x] **Parameterized stub bodies (components).** Component primitives now accept `template:` (`empty | form | list | crud_table | resource_view`); each picks a structural skeleton (form-with-submit, ul-with-empty-state, table+toolbar, article+actions) instead of the placeholder `"X component"` text. Wired through both the `create_component` MCP tool and the DSL `components:` primitive. Server-fn bodies still use `Ok(Default::default())` for plain `server_fns:`; the resource expansion already emits store-bound bodies, so the friction the TODO described is now confined to ad-hoc server fns — track separately if it bites.
- [x] **`models:` (or `types:`) primitive.** A DSL doc can declare server fns that reference `crate::model::Product`, but the model itself has to be hand-written first. Add a top-level `models:` section that emits Rust structs with serde derives and shared types between client and server.
- [x] **`store:` primitive.** Top-level `stores:` emits an in-memory CRUD helper under `src/state/{snake}.rs` (server-feature gated) with list/get/create/update/delete methods. Pair with server fns that call into `{Pascal}Store::global()`. SQLite backend is reserved for a follow-up.
- [x] **Client-side hook scaffolding.** Screens accept `template: { kind: resource_list | resource_form, endpoint, ... }`. `resource_list` emits `use_resource` + the loading/error/empty/data match ladder bound to a server fn. `resource_form` emits a controlled form with one signal per declared field plus a submit handler.

## Idempotency & re-runs

- [x] **`screens:` is not idempotent.** Re-running blindly appends to the `Route` enum and creates duplicate variants. Detect existing variants by name and skip/update, or own the route file outright and require the enum to live there.
- [x] **`components:` / `server_fns:` re-creation behaviour.** Currently overwrites stub files silently. Document the policy and/or add a `--if-missing` mode so re-runs are safe during iteration.
- [x] **`mod.rs` insertion order.** Entries are appended in DSL order. Sort alphabetically so diffs are stable across re-runs.

## Iteration & safety

- [x] **`dry_run` / preview mode for `execute_code`.** Today it writes immediately, which makes probing the tool's behaviour destructive. Return a file plan + diffs without committing when `dry_run: true`.
- [x] **`modify:` primitive for editing existing items.** Top-level `modify:` accepts `kind: add_model_field | add_component_prop | add_server_fn_arg` entries that idempotently append to an existing on-disk struct / props struct / server-fn parameter list. Honors `if_missing` and `dry_run`. Renaming a Routable variant is still hand-edit territory — track in a follow-up if needed.
- [x] **Collision pre-flight surfaced to the caller.** The spec says collisions are pre-flighted, but the response doesn't enumerate what was already present or what would conflict. Return a `collisions: [...]` field even on success.

## Correctness gaps

- [x] **`return_type` double-wrapping is a footgun.** Passing `Result<T, ServerFnError>` results in `Result<Result<T, ServerFnError>, ServerFnError>`. Either:
  - reject return types that already wrap `Result<_, ServerFnError>` with a clear error, or
  - accept both forms and normalize.
- [x] **Feature-flag awareness.** Items that exist only on the server side (state stores, server fn implementations) need `#[cfg(feature = "server")]` gates to avoid dead-code warnings on the web target. The DSL knows which items are server-only — emit the cfg gates automatically. *(Done as part of `store:` / `resources:`: `STORE_TPL` emits `#![cfg(feature = "server")]` at the file level; resource-expanded server-fn bodies wrap calls into the store with `#[cfg(feature = "server")]` + an `unreachable!()` else-branch. Plain `server_fns:` need no body-level gate because their `Ok(Default::default())` body references no server-only items, and `#[get]/#[post]` handle wire-protocol gating. Asserted by tests at dsl.rs:3693 and dsl.rs:3715.)*
- [x] **`layout: sidebar|topnav|blank` is misleading.** It only stamps a CSS class on the wrapper div; no layout component is generated and no `Outlet` is wired. Either generate a real layout component or drop the field. *(Dropped: `wrap_with` already covers real layout wrappers.)*
- [x] **`wrap_with` on Screen.** Documented but not exercised here. Verify it actually wraps the screen at the route layer rather than just emitting a comment.

## Tooling ergonomics

- [x] **`check_rsx` batch mode.** Accepts only one `file` at a time. Add a `files: [...]` form (or a project-wide entrypoint) so linting the whole app is a single call.
- [x] **Whole-project lint endpoint.** `dead_components`, `prop_drill`, `signal_lint`, `props_lint`, and `check_rsx` all exist but feel siloed. A single `lint_project` that runs them all and merges output would be the natural entry point.
- [x] **`find_example` requires undocumented `concept` field.** First call returns a deserialization error with no hint about expected values. Make `concept` optional (search across concepts) or expose the enum in the error message.
- [x] **Generated tests.** Stores generated through `resources:` now append a `#[cfg(test)] mod tests` block (5 sync `#[test]` functions: create/get/update/delete/list semantics) that hits the in-memory store directly via a private `new()` constructor (not `global()`, so tests don't bleed state across `OnceLock`). Sync `#[test]` instead of `#[tokio::test]` because the store API is sync — testing the wrapping server fn would require an axum runtime and adds nothing the store tests don't cover. Plain `stores:` opt in via `emit_tests: true` (the referenced model needs `Default`).

## Documentation & spec

- [x] **Spec inconsistency on return types.** The spec example in `get_dsl_spec` shows `Vec<String>` as a return type, but doesn't call out that wrapping in `Result<_, ServerFnError>` is forbidden. Add an explicit note plus an example showing the correct form.
- [x] **Document re-run semantics.** Whether re-running `execute_code` with a previously-seen name overwrites, skips, or errors is currently learned by experiment. Document it in the spec.
- [x] **Document the file layout assumed.** `src/components/*`, `src/server/*`, `src/components/mod.rs`, `src/server/mod.rs`, plus inline edits to `src/main.rs`. New users should know the blast radius before invoking the tool.

## Nice to have

- [x] **Per-resource scaffolding macro.** `resources:` fans out into a model + store + 5 server fns (list/get/create/update/delete) + a list and new screen for a named entity. Closes the loop on the most common request: "I want a CRUD slice for X." An edit/show screen would need URL-param-aware route variants, which the DSL doesn't yet emit — those stay manual for now.
- [ ] **OpenAPI-driven generation.** `openapi_spec` already exists as a read tool. The inverse — generate server fns + types from an OpenAPI doc — would make this useful for app teams replacing an existing backend.
- [ ] **Storybook-style component preview generation.** A `--gallery` mode that emits a route showing each generated component with sample props. Doubles as a smoke test that the components render.
