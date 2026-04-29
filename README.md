# zellij-mcp

A small purpose-built [Model Context Protocol][mcp] server in Rust that wraps
the [zellij][zellij] CLI for agent-driven multiplexer control. Exposes six
pane-id-addressed tools so an LLM agent can spawn, drive, and clean up
background panes without disturbing the user's focus.

[mcp]: https://modelcontextprotocol.io
[zellij]: https://zellij.dev

Designed and verified against **zellij 0.44.x** and **rmcp 1.5.x**.

## Why another zellij MCP?

[`GitJuhb/zellij-mcp-server`][gitjuhb] (the breadth-first option, ~60 tools) and
[`bnomei/tmux-mcp`][bnomei] (the only other Rust MCP for a multiplexer) were
audited as candidates. Findings:

- `GitJuhb/zellij-mcp-server` operates on **the focused pane** for every
  mutation. There is no `pane_id` argument on `write-chars`, `dump-screen`, or
  `close-pane`, and `list-panes --json` is not exposed at all. That is exactly
  the wrong abstraction for an orchestrator that needs to drive *specific*
  background panes.
- `bnomei/tmux-mcp` has the right shape (every mutating tool requires
  `pane_id`, `list-panes` returns typed JSON, output schemas everywhere) but
  targets tmux and has fanned out to ~55 tools with a buffer subsystem, search
  primitives, and a per-key dispatch surface.

This server picks up bnomei's discipline (mandatory `pane_id`, typed output,
descriptive tool docstrings) and trims to the smallest set that lets an agent
orchestrate panes for sub-agent work: **6 tools, ~470 LOC**.

[gitjuhb]: https://github.com/GitJuhb/zellij-mcp-server
[bnomei]: https://github.com/bnomei/tmux-mcp

## Tools

All tools accept an optional `session` argument; if omitted, zellij defaults to
the session inherited from `$ZELLIJ_SESSION_NAME`.

| Tool          | Required input                                      | Returns                                                        |
| ------------- | --------------------------------------------------- | -------------------------------------------------------------- |
| `list-panes`  | (none)                                              | `{ panes: [{id, title, is_focused, is_plugin, ...}] }`         |
| `spawn-pane`  | `cwd`                                               | `{ pane_id }`                                                  |
| `send-text`   | `pane_id`, `text`                                   | `{ ok }`                                                       |
| `read-pane`   | `pane_id`                                           | `{ text }`                                                     |
| `focus-pane`  | `pane_id`                                           | `{ ok }`                                                       |
| `kill-pane`   | `pane_id`                                           | `{ ok }`                                                       |

Optional inputs of note:

- `spawn-pane`: `command` (argv), `direction` (`right|down|left|up`),
  `floating: bool`, `name`, and **`keep_focus_on: pane_id`** — when set, the
  server issues a follow-up `focus-pane-id` after spawning so a background
  pane never steals focus.
- `send-text`: `submit: bool` (default `false`) and
  `newline_mode: "enter"|"shift_enter"`. With `submit: true, newline_mode: "enter"`
  the server writes byte 13 (CR) to submit. With `newline_mode: "shift_enter"`
  it writes byte 10 (LF) — useful for tools like Claude Code's prompt composer
  that treat enter as submit and shift+enter as newline.
- `read-pane`: `full: true` to include scrollback (default: viewport only).

Pane ids look like `terminal_3` or `plugin_1` — zellij's stable form. Bare
integers are accepted by zellij as `terminal_<n>`.

### A note on error surfacing

`zellij action focus-pane-id` errors loudly when given a bogus id, but
`write-chars`, `close-pane`, and `dump-screen` silently succeed (exit 0,
empty output) for nonexistent ids — a zellij CLI quirk. This MCP forwards
zellij's exit codes faithfully and **does not pre-validate** pane ids; do a
`list-panes` first if your agent loop needs strict id checks.

## Build

Requires `cargo` 1.70+ and the `zellij` binary on PATH.

```bash
cargo build --release
# binary lands at target/release/zellij-mcp
```

On NixOS:

```bash
nix shell nixpkgs#cargo nixpkgs#rustc nixpkgs#gcc -c cargo build --release
```

## Wire into an MCP-aware agent

### Claude Code

`~/.claude/mcp-config.json` (or via your settings template):

```json
{
  "mcpServers": {
    "zellij": {
      "command": "/home/you/dev/zellij-mcp/target/release/zellij-mcp"
    }
  }
}
```

### OpenCode / Codex / Qwen Code / etc.

Same shape — the binary speaks MCP over stdio, takes no flags, and reads no
config files. Drop the path into the agent's MCP server table.

## Smoke test

A small Python driver exercises every tool against a real zellij session:

```bash
# from inside an existing zellij session, or pass --session NAME
python3 scripts/smoke.py
python3 scripts/smoke.py --session some-other-session
python3 scripts/smoke.py --no-mutating-tests   # read-only
```

It walks: `initialize` → `tools/list` (verifies all 6 tools and their
`outputSchema`) → `list-panes` → `spawn-pane` (floating, with `keep_focus_on`)
→ `read-pane` (verifies the spawned process's output) → `send-text` →
`focus-pane` round-trip → `kill-pane` → bogus-id error path.

## Design notes

### Pane ids must be passed explicitly

Every per-pane tool requires `pane_id` — there is no "send to current focus"
fallback. This is the bug that makes `GitJuhb/zellij-mcp-server` unusable for
orchestrators: when the agent spawns a background pane and tries to drive it,
"send to focus" silently writes to whatever pane the human happened to click
on. Mandatory ids eliminate that class of bug.

### Output is structured JSON, not text blobs

Every tool sets `output_schema` via `schemars::schema_for_type::<...>()`, so
clients can validate responses strictly. The MCP protocol surfaces this as
`outputSchema` in `tools/list`.

### `keep_focus_on` is the focus-stealing fix

`zellij action new-pane` *always* focuses the newly created pane, and zellij
0.44.x has no `--no-focus` flag. The server papers over this in `spawn-pane`:
if the caller passes `keep_focus_on`, the new pane is created and then a
follow-up `focus-pane-id` restores the caller's seat. The caller's own pane id
is normally read from `$ZELLIJ_PANE_ID` in the agent's environment.

### What is *not* here

- No buffer/copy-paste primitives.
- No layout/tab management.
- No security policy / allow-list (assume single-user, single-session
  orchestration).
- No async command tracking (`execute-command` / `get-command-result`). If
  agents need exit codes, add them as a 7th/8th tool — they're a clean
  extension and the bnomei tmux-mcp pattern is a good reference.

## License

MIT — see [LICENSE](LICENSE).
