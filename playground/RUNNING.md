# Running the dioxus-mcp cockpit

`playground/` is a wasm web app (the "cockpit") that talks to the `dioxus-mcp`
server. As of M7 the **stdio MCP server embeds the cockpit** — there's nothing
extra to run.

## The common case: nothing extra to run

Your `dioxus` MCP server (stdio, in Claude Code config) points at
`target/debug/dioxus-mcp`. When Claude Code spawns it for a session, that same
process **also serves the cockpit** at <http://127.0.0.1:8731/> (auto-scoped to
the project you're in, same origin so no CORS). Just open that URL in a browser
while a session is live. `propose_scaffold` proposals show up in the Proposals
tab; approving (with your edits) returns the result to the agent.

To (re)build the embedded UI bundle after changing `playground/`:
```sh
crates/dioxus-mcp/scripts/build-ui.sh && cargo build -p dioxus-mcp
```
Proposals persist to `<project>/target/dioxus-mcp/proposals.json`, so they
survive a server respawn. Pass `--no-cockpit` to suppress the embedded HTTP, or
`--bind` to change the cockpit address.

## Standalone (durable / shared) mode

For a cockpit that outlives any one session, isn't tied to a project, or is
shared, run it as its own daemon instead — same binary, `--transport http`:

```sh
./target/debug/dioxus-mcp --transport http --bind 127.0.0.1:8731 \
    --project-root /home/tony/src/dx-playground-scratch
```

Open <http://127.0.0.1:8731/>. During UI iteration, skip the embed step and
point the server at a live build dir instead:

```sh
DIOXUS_MCP_UI_DIR=playground/target/dx/dx-playground/release/web/public \
  ./target/debug/dioxus-mcp --transport http --project-root ...
# (or `cd playground && dx serve` on :8080 for hot reload during UI dev)
```

## Cockpit modes

- **Author** — edit a DSL doc; the **Approximate** tab renders instantly from a
  local parse; **Generated source** / **Validation** update ~300ms after typing
  (a `dry_run` round-trip). Header presets load `client_crud` / `resources`.
- **Author → Compiled tab** — **Apply** writes the slice into the scratch crate
  for real, then reloads an iframe pointed at that crate's `dx serve`
  (run `cd dx-playground-scratch && dx serve --port 8081`, set the URL in the box).
- **Proposals** — the human side of the approval gate (see below).

## Approval gate (M6): agent proposes, you approve

Point Claude Code (or any MCP client) at the **same running instance** over HTTP:

```jsonc
// .mcp.json in your target Dioxus app
{ "mcpServers": { "dioxus-mcp": { "type": "http", "url": "http://127.0.0.1:8731" } } }
```

Then, when you want review-before-write, ask the agent to *propose* changes. It
calls `propose_scaffold` instead of `execute_code`; the call **blocks** (up to
~5 min, then degrades to a pollable handle). The proposal appears in the
cockpit's **Proposals** tab, where you preview it, optionally **edit the DSL**,
and **Approve** or **Reject**. The approved (possibly edited) doc is what
actually runs, and that result returns to the agent — your edit is ground truth.

`execute_code` is unchanged and still writes directly; the gate is opt-in.

## Notes
- Apply / approve use `if_missing: true`, so re-applying a *changed* screen skips
  the existing file. To iterate on compiled output, reset the scratch crate
  between Applies. The Approximate preview is the iteration tool.
- Resource slices (`resources:`) generate `#[server]` fns → the scratch crate is
  fullstack.
- `playground/` lives in this repo but is **excluded from the workspace** (it's a
  wasm bin built via `dx`, not `cargo build`).
