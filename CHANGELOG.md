# Changelog

## v0.2.0

- Added the `list-sessions` MCP tool, backed by `zellij list-sessions -n`, with structured session summaries.
- Removed non-standard `format: "uint64"` from exposed unsigned integer schemas while preserving `u64` values.

## v0.1.0

- Initial MCP server with six pane-id-addressed zellij tools: `list-panes`, `spawn-pane`, `send-text`, `read-pane`, `focus-pane`, and `kill-pane`.
