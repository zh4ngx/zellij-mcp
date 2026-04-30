# Changelog

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
