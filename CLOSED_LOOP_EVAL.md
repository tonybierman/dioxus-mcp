# Closed-loop evaluation

How we use generated Dioxus apps to drive the MCP's lint suite forward.

## The loop

1. **Generate** — feed a single user-facing description (e.g. `TODO_APPS.md` #3, "realtime standup board") to a Dioxus 0.7 fullstack generator. No incremental editing; let it ship whatever shape the model picks.
2. **Lint** — run `lint_project` against the generated `src/` tree:
   - `mcp__dioxus__lint_project { project_root: "<generated-app>" }` from the MCP, or
   - `cargo test -p dioxus-mcp --test integration tool_lint_project_iter03_baseline` to pin the fixture.
3. **Triage** — read each finding alongside the corresponding source line. Bucket into:
   - **TP** (true positive): the lint named a real shape, the suggested fix lands cleanly.
   - **FP** (false positive): the lint fired on a shape that's deliberate or doesn't actually cause the harm described.
   - **FN** (false negative): the lint stayed silent on a shape that should have triggered it.
4. **Feed back**:
   - **TP → no action** beyond reading the message. If the message was confusing, refine wording.
   - **FP → tighten the matcher.** Add a regression test that pins the shape as silent.
   - **FN → broaden the matcher.** Add a regression test that pins the shape as fired.
   - Severity drift discovered during triage → bump or demote in the lint file.
5. **Repeat** — generate iter N+1 from a fresh prompt. Each iteration should both *clear* the previous TPs and *reveal* new shapes the suite missed.

The iter01 → iter03 trajectory below is the worked example.

## Worked example: dioxus_standup (iter01 → iter03)

| Iter | Files | Issues | Notable shifts |
|------|-------|--------|----------------|
| 01   | 40    | 45     | Catalog widgets installed but unused (3 dead); deep avatar drill (chain_depth=3); 4 context-signal triads |
| 02   | 12    | 26     | Refactored to a flat board; 4 unfixed `signal_many_writers` warnings persisted |
| 03   | 22    | 34     | First app to surface `optimistic_lock_gate`, `insecure_set_cookie`, `magic_id_prefix_for_optimistic`, `shared_enum_validation`, `presence_map_unbounded` — these lints landed *during* the iter03 review cycle |

The 02 → 03 increase isn't a regression: iter02 looked best on total but had stable warnings; iter03's higher count is mostly new lint coverage hitting old shapes. Without the **severity breakdown** that `project_tour` now emits (`[N warning, M info, …]`), this trajectory reads as "got worse" instead of "we caught more."

### What iter03 surfaced as a follow-up

- **2 confirmed false negatives** against existing lints:
  - `duplicate_helper_client_server` missed `normalize_positions` when the param name differed between client (`list`) and server (`board`). Fix: tighten the matcher to rewrite param names to positional placeholders before comparing.
  - `signal_drilled_2_levels` stopped firing on `Signal<T>` chains because the signal *origin* was a `use_signal(…)` binding (no parent prop for `prop_drill` to follow). Fix: synthesize an origin edge from `use_signal` bindings into the chain graph.
- **5 new lints worth surfacing**:
  - `derived_view_no_memo` — pure derivations called from rsx without `use_memo` (iter03 `column_cards(&cards.read(), col_id)` called 3× per render).
  - `empty_async_error_arm` — `Err(_) => {}` inside a `use_future` polling loop (iter03 `ping_presence` silently swallows server failures).
  - `polling_future_no_backoff` — constant-interval polling without error-path backoff or jitter (iter03 board poll @ 2s + presence ping @ 3s).
  - `repeated_auth_extractor` — `user_from_cookies(&cookies)` repeated across ≥3 server fns (iter03: 6 of 8). Suggests an Axum `FromRequestParts` extractor.
  - `presence_map_filter_on_read_no_evict` — split out of `presence_map_unbounded` for the most misleading shape: TTL filter on read masks the memory leak.

### What iter03 surfaced as a refinement

- `presence_map_unbounded` severity bumped `info` → `warning` (the "no eviction at all" mode is the same memory leak as the narrow-eviction mode, just dressed plainer).
- `magic_id_prefix_for_optimistic` `kind: "read"` bumped `info` → `warning` (consumer mis-classifies any real id starting with the magic prefix — a correctness bug, not a smell).
- `optimistic_lock_gate` now lists every bump site by line number (iter03 had 4–6 scattered across event handlers; the consolidation refactor needs all of them).
- `signal_lint context_signal_triads` lowered to `N=2` hint when both modules share the canonical `use_context_provider(|| Signal::new(…))` + `use_context::<Signal<…>>()` boilerplate.
- `openapi_spec` auto-detects `session_cookie_name` from `cookies.get("X")` calls (iter03 uses `sid`, not the default `session_id`).

Every shift above ships with a regression test under `crates/dioxus-mcp/src/tools/lints/<lint>.rs::tests` AND a pin in the `tool_lint_project_iter03_baseline` integration test, so iter04 can't silently regress them.

## How to add the next loop

Run `lint_project` on whatever the generator produces, then:

1. Walk the **headline** rollup top-to-bottom (severity desc). Highest-leverage findings first.
2. For each surprising finding, open the source file and confirm the shape with `Read`.
3. Open the lint file (`crates/dioxus-mcp/src/tools/lints/<name>.rs`) and:
   - Add a test that pins the new shape (silent or firing, depending on triage).
   - If the matcher needs to change, change it.
4. Add a corresponding entry to `tests/fixtures/iter03_baseline/` (or add a new fixture for a different app shape) so the regression survives future refactors.
5. Bump `tool_lint_project_iter03_baseline`'s `expected_min` if the change adds a new lint code or raises the count for an existing one.

The integration test deliberately uses `expected_min` (not `==`) so a future refinement that catches more is fine. A future refinement that catches *less* fails loudly.
