# Changelog

## v0.3.1

- Fixed immediate `spawn-pane` -> `resize-pane` failure by verifying that `spawn-pane`'s returned pane id appears in `list-panes` before exposing it. Root cause: zellij 0.44.1 can return a `terminal_N` id for directed non-floating splits in detached sessions without actually inserting an addressable pane, so subsequent pane-id actions (resize, send-text, focus, kill) all fail with "Pane with id Terminal(N) not found".
- When a directed `spawn-pane` returns a phantom id, retry the spawn without `--direction` and return the materialized pane id. This preserves a usable pane for follow-up pane-id tools in the affected detached-session path.
- Defense-in-depth: `resize-pane` also retries on the transient "not found" error string in case the spawn-verification path ever misses an edge case.
- Extended `scripts/smoke.py` to cover the no-delay spawn → resize sequence and updated the expected v0.3 tool list.

## v0.3.0

- Added the `resize-pane` MCP tool, wrapping `zellij action resize` with `direction: "increase" | "decrease"`. Useful when programmatic pane spawning needs post-spawn ratio adjustment.
- Added the `spawn-pane-with-target` MCP tool, a variant of `spawn-pane` that focuses a specified `target_pane_id` before spawning so the new split anchors next to the target rather than the caller's current focus. `spawn-pane` itself also gains an optional `target_pane_id` field.
- `focus-pane` now treats zellij's "already focused" error as success rather than bubbling it as a tool error.
- Tool surface grows from 7 → 9 tools.

## v0.2.0

- Added the `list-sessions` MCP tool, backed by `zellij list-sessions -n`, with structured session summaries.
- Removed non-standard `format: "uint64"` from exposed unsigned integer schemas while preserving `u64` values.

## v0.1.0

- Initial MCP server with six pane-id-addressed zellij tools: `list-panes`, `spawn-pane`, `send-text`, `read-pane`, `focus-pane`, and `kill-pane`.
