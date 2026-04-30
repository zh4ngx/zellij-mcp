# zellij-mcp

Reliable pane fabric for AI agents.

`zellij-mcp` is a small Rust [Model Context Protocol][mcp] server that wraps
the [zellij][zellij] CLI with session discovery and pane-id-addressed tools. It
gives an agent the minimum surface needed to fan out work into terminal panes,
read results, route follow-up input, and clean up without stealing the user's
focus.

[mcp]: https://modelcontextprotocol.io
[zellij]: https://zellij.dev

Designed and verified against **zellij 0.44.x** and **rmcp 1.5.x**.

## Why This Exists

Agent orchestration breaks when "the current pane" is treated as an API. If a
human clicks elsewhere, or one background worker steals focus at the wrong time,
the next write lands in the wrong terminal. That is enough to corrupt a run.

Existing multiplexer MCP experiments such as
[`GitJuhb/zellij-mcp-server`][gitjuhb] and [`bnomei/tmux-mcp`][bnomei] show the
right direction, but still leave gaps for zellij-native agent work: too many
tools, focus-coupled commands, missing structured pane discovery, or no first
class way to spawn a background pane while returning focus to the caller.

`zellij-mcp` is intentionally narrower:

- **Pane ids are required** for every per-pane operation.
- **No current-focus fallback** exists anywhere in the server.
- **`keep_focus_on` is first class** on `spawn-pane`.
- **Every tool returns structured JSON** with an MCP `outputSchema`.
- **Only seven tools** are exposed: session discovery plus the pane primitives
  needed for reliable orchestration.

[gitjuhb]: https://github.com/GitJuhb/zellij-mcp-server
[bnomei]: https://github.com/bnomei/tmux-mcp

## The Seven Tools

All tools accept optional `session`. If omitted, zellij uses the session from
the inherited zellij environment. `list-sessions` accepts the same shape but
ignores `session` because zellij session listing is global.

| Tool | Purpose | Required input | Output |
| --- | --- | --- | --- |
| `list-sessions` | Discover zellij sessions as typed JSON | none | `[{ name, created_age_seconds, ... }]` |
| `list-panes` | Discover panes as typed JSON | none | `{ panes: [...] }` |
| `spawn-pane` | Create a split or floating pane | `cwd` | `{ pane_id }` |
| `send-text` | Type into a specific pane | `pane_id`, `text` | `{ ok }` |
| `read-pane` | Capture viewport or scrollback | `pane_id` | `{ text }` |
| `focus-pane` | Move focus to a pane by id | `pane_id` | `{ ok }` |
| `kill-pane` | Close a pane by id | `pane_id` | `{ ok }` |

Important options:

- `spawn-pane.command`: argv for the process to run.
- `spawn-pane.direction`: `right`, `down`, `left`, or `up`.
- `spawn-pane.floating`: open as a floating overlay.
- `spawn-pane.keep_focus_on`: restore focus to this pane after spawn.
- `send-text.submit`: send the text and then press enter.
- `send-text.newline_mode`: `enter` sends byte 13; `shift_enter` sends byte 10.
- `read-pane.full`: include full scrollback instead of only the viewport.

Pane ids are zellij's stable ids, usually `terminal_3` or `plugin_1`. Call
`list-panes` first, keep the ids in your orchestration state, and address panes
explicitly for the rest of the run.

## Quickstart

Build the stdio MCP server:

```bash
git clone https://github.com/zh4ngx/zellij-mcp.git
cd zellij-mcp
cargo build --release
```

The binary is `target/release/zellij-mcp`. Run your MCP client inside a zellij
session, or pass `session` in tool calls when targeting a named session.

The client snippets below follow the official [Claude Code MCP][claude-mcp] and
[OpenCode MCP][opencode-mcp] local-server formats.

[claude-mcp]: https://code.claude.com/docs/en/mcp
[opencode-mcp]: https://opencode.ai/docs/mcp-servers

### Claude Code

Claude Code can add local stdio servers with `claude mcp add`:

```bash
claude mcp add --transport stdio --scope user zellij \
  -- /absolute/path/to/zellij-mcp/target/release/zellij-mcp
claude mcp list
```

Project-scoped `.mcp.json` works too:

```json
{
  "mcpServers": {
    "zellij": {
      "command": "/absolute/path/to/zellij-mcp/target/release/zellij-mcp",
      "args": [],
      "env": {}
    }
  }
}
```

### OpenCode

Add a local MCP server in `opencode.jsonc`:

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "zellij": {
      "type": "local",
      "command": ["/absolute/path/to/zellij-mcp/target/release/zellij-mcp"],
      "enabled": true
    }
  }
}
```

### Generic MCP Client

Use stdio transport. The server takes no flags and reads no config file:

```json
{
  "mcpServers": {
    "zellij": {
      "type": "stdio",
      "command": "/absolute/path/to/zellij-mcp/target/release/zellij-mcp",
      "args": []
    }
  }
}
```

## Orchestration Pattern

A robust agent loop is session-aware and pane-id-addressed:

1. `list-sessions` when the target session name is not already known.
2. `list-panes` to discover the controller pane, often also available as
   `$ZELLIJ_PANE_ID`.
3. `spawn-pane` workers with `keep_focus_on` set to the controller pane.
4. `read-pane` each worker until it reaches a terminal state.
5. `send-text` follow-up instructions to specific panes only.
6. `kill-pane` transient workers when the DAG node is done.

Here is a metastack-style fan-out / join DAG:

```yaml
root:
  pane: terminal_7
  cwd: /home/andy/dev/zellij-mcp

nodes:
  lint:
    tool: spawn-pane
    input:
      cwd: /home/andy/dev/zellij-mcp
      command: ["cargo", "clippy", "--all-targets", "--all-features"]
      keep_focus_on: terminal_7

  test:
    tool: spawn-pane
    input:
      cwd: /home/andy/dev/zellij-mcp
      command: ["cargo", "test"]
      keep_focus_on: terminal_7

  smoke:
    tool: spawn-pane
    input:
      cwd: /home/andy/dev/zellij-mcp
      command: ["python3", "scripts/smoke.py", "--no-mutating-tests"]
      floating: true
      keep_focus_on: terminal_7

  synthesize:
    after: [lint, test, smoke]
    read:
      - { tool: read-pane, pane_id: "${lint.pane_id}", full: true }
      - { tool: read-pane, pane_id: "${test.pane_id}", full: true }
      - { tool: read-pane, pane_id: "${smoke.pane_id}", full: true }
    reduce:
      tool: send-text
      input:
        pane_id: terminal_7
        text: "Summarize the three worker panes and propose the next patch."
        submit: true
```

The invariant is simple: every edge in the DAG carries a concrete `pane_id`.
Focus can move, humans can click around, and workers can finish in any order
without changing where the next tool call goes.

## Smoke Test

A small Python driver exercises the full tool surface against a real zellij
session:

```bash
python3 scripts/smoke.py
python3 scripts/smoke.py --session some-session
python3 scripts/smoke.py --no-mutating-tests
```

It walks `initialize`, `tools/list`, `list-sessions`, `list-panes`, `spawn-pane`,
`read-pane`, `send-text`, `focus-pane`, `kill-pane`, and a bogus-id error path.

## Notes

`zellij action focus-pane-id` errors loudly for bogus ids, but some zellij CLI
actions return success for nonexistent pane ids. This MCP forwards zellij's
behavior rather than hiding it. If your controller needs strict validation,
call `list-panes` and check the id before mutating.

This server assumes a trusted local user. It can type into terminals and close
panes by design.

## License

MIT, see [LICENSE](LICENSE).
