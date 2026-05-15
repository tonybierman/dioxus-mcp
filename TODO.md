# dioxus-mcp TODO

Improvement checklist derived from a real-world build (an inventory management app, May 2026). Each item lists the friction observed and a concrete shape for the fix. Ordered roughly by impact.

## High impact — turn the DSL from "file generator" into "feature generator"

- [ ] **Parameterized stub bodies.** Today every generated component body is `div { class: "…", "X component" }` and every server fn body is `Ok(Default::default())`. Every file has to be hand-rewritten. Add a `body:` field or a `template:` enum (`form | list | crud-table | resource-view | empty`) to each primitive so the stub is at least directionally useful.
- [x] **`models:` (or `types:`) primitive.** A DSL doc can declare server fns that reference `crate::model::Product`, but the model itself has to be hand-written first. Add a top-level `models:` section that emits Rust structs with serde derives and shared types between client and server.
- [ ] **`store:` primitive.** Server fns currently scaffold as signature-only stubs. Add a store primitive (`store: { kind: in_memory|sqlite, resource: Product }`) that emits a typed CRUD helper *and* wires the matching server fns into it. Combined with `models:`, one YAML doc would describe a full resource slice.
- [ ] **Client-side hook scaffolding.** A large fraction of hand-written code was `use_resource(...)` + the `match &*res.read_unchecked() { None|Some(Err)|Some(Ok) }` ladder. A "resource-bound screen" primitive that binds a screen to a server fn and emits the loading/error/data branches would eliminate the bulk of repetitive UI plumbing.

## Idempotency & re-runs

- [x] **`screens:` is not idempotent.** Re-running blindly appends to the `Route` enum and creates duplicate variants. Detect existing variants by name and skip/update, or own the route file outright and require the enum to live there.
- [x] **`components:` / `server_fns:` re-creation behaviour.** Currently overwrites stub files silently. Document the policy and/or add a `--if-missing` mode so re-runs are safe during iteration.
- [x] **`mod.rs` insertion order.** Entries are appended in DSL order. Sort alphabetically so diffs are stable across re-runs.

## Iteration & safety

- [x] **`dry_run` / preview mode for `execute_code`.** Today it writes immediately, which makes probing the tool's behaviour destructive. Return a file plan + diffs without committing when `dry_run: true`.
- [ ] **`modify:` primitive for editing existing items.** Adding a prop, adding a server fn arg, or renaming a variant requires hand-editing. A targeted modify primitive would cover the common iteration case.
- [x] **Collision pre-flight surfaced to the caller.** The spec says collisions are pre-flighted, but the response doesn't enumerate what was already present or what would conflict. Return a `collisions: [...]` field even on success.

## Correctness gaps

- [x] **`return_type` double-wrapping is a footgun.** Passing `Result<T, ServerFnError>` results in `Result<Result<T, ServerFnError>, ServerFnError>`. Either:
  - reject return types that already wrap `Result<_, ServerFnError>` with a clear error, or
  - accept both forms and normalize.
- [ ] **Feature-flag awareness.** Items that exist only on the server side (state stores, server fn implementations) need `#[cfg(feature = "server")]` gates to avoid dead-code warnings on the web target. The DSL knows which items are server-only — emit the cfg gates automatically. *(Deferred: nothing in the current DSL emits server-only Rust items — server fns are already gated by `#[get]`/`#[post]` macro expansion. Becomes actionable once the `store:` primitive lands.)*
- [x] **`layout: sidebar|topnav|blank` is misleading.** It only stamps a CSS class on the wrapper div; no layout component is generated and no `Outlet` is wired. Either generate a real layout component or drop the field. *(Dropped: `wrap_with` already covers real layout wrappers.)*
- [x] **`wrap_with` on Screen.** Documented but not exercised here. Verify it actually wraps the screen at the route layer rather than just emitting a comment.

## Tooling ergonomics

- [x] **`check_rsx` batch mode.** Accepts only one `file` at a time. Add a `files: [...]` form (or a project-wide entrypoint) so linting the whole app is a single call.
- [ ] **Whole-project lint endpoint.** `dead_components`, `prop_drill`, `signal_lint`, `props_lint`, and `check_rsx` all exist but feel siloed. A single `lint_project` that runs them all and merges output would be the natural entry point.
- [x] **`find_example` requires undocumented `concept` field.** First call returns a deserialization error with no hint about expected values. Make `concept` optional (search across concepts) or expose the enum in the error message.
- [ ] **Generated tests.** Emit a `#[tokio::test]` stub per server fn that exercises the store. Free CI surface and a documentation surface for callers.

## Documentation & spec

- [x] **Spec inconsistency on return types.** The spec example in `get_dsl_spec` shows `Vec<String>` as a return type, but doesn't call out that wrapping in `Result<_, ServerFnError>` is forbidden. Add an explicit note plus an example showing the correct form.
- [x] **Document re-run semantics.** Whether re-running `execute_code` with a previously-seen name overwrites, skips, or errors is currently learned by experiment. Document it in the spec.
- [x] **Document the file layout assumed.** `src/components/*`, `src/server/*`, `src/components/mod.rs`, `src/server/mod.rs`, plus inline edits to `src/main.rs`. New users should know the blast radius before invoking the tool.

## Nice to have

- [ ] **Per-resource scaffolding macro.** A `resource:` primitive that fans out into `models` + `store` + `server_fns` (list/get/create/update/delete) + a screens triplet (list/new/edit) for a named entity. Closes the loop on the most common request: "I want a CRUD slice for X."
- [ ] **OpenAPI-driven generation.** `openapi_spec` already exists as a read tool. The inverse — generate server fns + types from an OpenAPI doc — would make this useful for app teams replacing an existing backend.
- [ ] **Storybook-style component preview generation.** A `--gallery` mode that emits a route showing each generated component with sample props. Doubles as a smoke test that the components render.
