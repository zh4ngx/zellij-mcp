//! MCP server: 9 tools wrapping zellij CLI primitives.
//!
//! Design rules (lifted from the audit of bnomei/tmux-mcp + GitJuhb/zellij-mcp-server):
//!  - `pane_id` is REQUIRED on every per-pane tool. No "current focus" fallbacks.
//!  - `list-panes` returns typed JSON (`output_schema` set on the tool).
//!  - `spawn-pane` exposes `keep_focus_on` so callers can spawn background panes
//!    without losing their seat in the foreground pane.
//!  - `spawn-pane-with-target` anchors a new split at a specific existing pane.
//!  - Tool descriptions steer the agent toward correct usage.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, Content, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo,
};
use rmcp::schemars::{self, JsonSchema};
use rmcp::{ErrorData as McpError, ServerHandler, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};

use crate::zellij;

// ----------------------------------------------------------------------------
// Input schemas
// ----------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListSessionsInput {
    /// Optional zellij session name. Ignored because zellij session listing is
    /// global, but accepted for input-shape consistency with the other tools.
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListPanesInput {
    /// Optional zellij session name. If omitted, the zellij CLI uses the
    /// current session from its inherited zellij environment. For caller-side
    /// session detection, use `ZELLIJ_SESSION_NAME`; `ZELLIJ` is only a
    /// presence marker.
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SpawnPaneInput {
    /// Working directory for the new pane. Required — zellij's `new-pane` defaults
    /// to the parent process cwd otherwise, which is rarely what the agent wants.
    pub cwd: String,
    /// Optional command to run in the new pane (argv split). If omitted, the pane
    /// runs the user's default shell.
    pub command: Option<Vec<String>>,
    /// Optional split direction relative to the focused pane: `"right"` or `"down"`.
    /// If omitted, zellij chooses the largest available space.
    pub direction: Option<String>,
    /// If true, open the pane as a floating overlay rather than splitting.
    pub floating: Option<bool>,
    /// Optional human-readable name for the new pane.
    pub name: Option<String>,
    /// If set, after spawning, focus is restored to this pane id (e.g. the
    /// caller's own pane). Use this to spawn background panes without losing focus.
    pub keep_focus_on: Option<String>,
    /// If set, focus this pane before spawning so the new split is anchored next
    /// to it rather than the current focus.
    pub target_pane_id: Option<String>,
    /// Optional zellij session name (see list-panes).
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SendTextInput {
    /// Pane ID, e.g. `terminal_3` or `plugin_1`. Bare integers are accepted by
    /// zellij as `terminal_<n>`.
    pub pane_id: String,
    /// Text to write to the pane. Sent via `zellij action write-chars`.
    pub text: String,
    /// If true, follow the text with a submit key. Default: false (just type).
    pub submit: Option<bool>,
    /// Submit key when `submit: true`. `"enter"` (default) sends byte 13 (CR);
    /// `"shift_enter"` sends byte 10 (LF), useful for Claude Code-style multi-line
    /// composers that treat shift+enter as newline-without-submit.
    pub newline_mode: Option<String>,
    /// Optional zellij session name.
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadPaneInput {
    /// Pane ID, e.g. `terminal_3`.
    pub pane_id: String,
    /// If true, include scrollback buffer. Default: viewport only.
    pub full: Option<bool>,
    /// Optional zellij session name.
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PaneTargetInput {
    /// Pane ID.
    pub pane_id: String,
    /// Optional zellij session name.
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ResizePaneInput {
    /// Pane ID.
    pub pane_id: String,
    /// Resize operation. Must be `"increase"` or `"decrease"`.
    pub direction: String,
    /// Optional zellij session name.
    pub session: Option<String>,
}

// ----------------------------------------------------------------------------
// Output schemas
// ----------------------------------------------------------------------------

fn unsigned_integer_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "integer",
        "minimum": 0
    })
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SessionSummary {
    /// Zellij session name.
    pub name: String,
    /// Session age in whole seconds, parsed from zellij's "Created ... ago" text.
    #[schemars(schema_with = "unsigned_integer_schema")]
    pub created_age_seconds: u64,
    /// True if zellij marks this session as current/attached.
    pub is_attached: bool,
    /// True if zellij marks this session as exited.
    pub is_exited: bool,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PaneSummary {
    /// Stable pane id in the form `terminal_<n>` or `plugin_<n>`.
    pub id: String,
    /// User-visible pane title.
    pub title: String,
    /// True if this is the focused pane in its tab.
    pub is_focused: bool,
    /// True if this is a plugin pane (vs a regular terminal pane).
    pub is_plugin: bool,
    /// True if floating.
    pub is_floating: bool,
    /// Has the underlying command exited?
    pub exited: bool,
    /// Tab id this pane lives in.
    #[schemars(schema_with = "unsigned_integer_schema")]
    pub tab_id: u64,
    /// Tab name.
    pub tab_name: String,
    /// The command running in the pane, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Working directory of the running command, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListPanesOutput {
    pub panes: Vec<PaneSummary>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListSessionsOutput {
    pub sessions: Vec<SessionSummary>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SpawnPaneOutput {
    /// The id of the newly created pane.
    pub pane_id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct OkOutput {
    pub ok: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ReadPaneOutput {
    pub text: String,
}

// ----------------------------------------------------------------------------
// Server
// ----------------------------------------------------------------------------

#[derive(Clone)]
pub struct ZellijMcpServer {
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl ZellijMcpServer {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

fn structured<T: Serialize>(value: &T) -> CallToolResult {
    match serde_json::to_value(value) {
        Ok(v) => CallToolResult::structured(v),
        Err(e) => CallToolResult::error(vec![Content::text(format!(
            "internal: failed to serialize tool output: {e}"
        ))]),
    }
}

fn err(msg: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![Content::text(msg.into())])
}

async fn spawn_pane_impl(input: SpawnPaneInput, tool_name: &str) -> CallToolResult {
    let session = input.session.as_deref();
    let direction_norm = input.direction.as_deref().map(|d| d.to_lowercase());
    let direction = direction_norm.as_deref();
    if let Some(d) = direction
        && !matches!(d, "right" | "down" | "left" | "up")
    {
        return err(format!(
            "{tool_name}: direction must be one of right|down|left|up, got `{d}`"
        ));
    }

    if let Some(target) = input.target_pane_id.as_deref()
        && let Err(e) = zellij::focus_pane_id(session, target).await
    {
        return err(format!(
            "{tool_name}: focusing target pane `{target}` failed: {e}"
        ));
    }

    let pane_id = match zellij::new_pane(
        session,
        &input.cwd,
        direction,
        input.floating.unwrap_or(false),
        input.name.as_deref(),
        input.command.as_deref(),
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            if let Some(target) = input.keep_focus_on.as_deref() {
                let _ = zellij::focus_pane_id(session, target).await;
            }
            return err(format!("{tool_name}: {e}"));
        }
    };

    if let Some(target) = input.keep_focus_on.as_deref()
        && let Err(e) = zellij::focus_pane_id(session, target).await
    {
        return err(format!(
            "{tool_name}: pane `{pane_id}` created, but restoring focus to `{target}` failed: {e}"
        ));
    }

    structured(&SpawnPaneOutput { pane_id })
}

#[tool_router]
impl ZellijMcpServer {
    #[tool(
        name = "list-sessions",
        description = "List all zellij sessions as structured JSON. Use this when you need to discover available session names before calling list-panes with a session argument.",
        annotations(read_only_hint = true, idempotent_hint = true),
        output_schema = rmcp::handler::server::common::schema_for_type::<ListSessionsOutput>()
    )]
    async fn list_sessions(
        &self,
        Parameters(input): Parameters<ListSessionsInput>,
    ) -> Result<CallToolResult, McpError> {
        let ListSessionsInput { session: _session } = input;
        let raw = match zellij::list_sessions().await {
            Ok(s) => s,
            Err(e) => return Ok(err(format!("list-sessions: {e}"))),
        };
        match parse_sessions(&raw) {
            Ok(sessions) => Ok(structured(&ListSessionsOutput { sessions })),
            Err(e) => Ok(err(format!("list-sessions: {e}\nraw: {raw}"))),
        }
    }

    #[tool(
        name = "list-panes",
        description = "List all panes in a zellij session as structured JSON. Always call this FIRST to discover pane ids before any send-text/read-pane/focus-pane/kill-pane call. Pane ids look like `terminal_3` or `plugin_1` and are stable for the lifetime of the pane.",
        annotations(read_only_hint = true, idempotent_hint = true),
        output_schema = rmcp::handler::server::common::schema_for_type::<ListPanesOutput>()
    )]
    async fn list_panes(
        &self,
        Parameters(input): Parameters<ListPanesInput>,
    ) -> Result<CallToolResult, McpError> {
        let raw = match zellij::list_panes_json(input.session.as_deref()).await {
            Ok(s) => s,
            Err(e) => return Ok(err(format!("list-panes: {e}"))),
        };
        let parsed: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                return Ok(err(format!(
                    "list-panes: failed to parse zellij JSON: {e}\nraw: {raw}"
                )));
            }
        };
        let panes = match parsed.as_array() {
            Some(arr) => arr.iter().map(parse_pane).collect::<Vec<_>>(),
            None => return Ok(err("list-panes: zellij output was not a JSON array")),
        };
        Ok(structured(&ListPanesOutput { panes }))
    }

    #[tool(
        name = "spawn-pane",
        description = "Spawn a new pane in the zellij session. Returns the new pane id. Pass `keep_focus_on` with the caller's own pane id to launch background panes without stealing focus — zellij's new-pane always focuses the new pane, so the MCP issues a follow-up focus-pane-id to restore your seat. Use `floating: true` for transient overlays; otherwise the pane splits the focused tile.",
        output_schema = rmcp::handler::server::common::schema_for_type::<SpawnPaneOutput>()
    )]
    async fn spawn_pane(
        &self,
        Parameters(input): Parameters<SpawnPaneInput>,
    ) -> Result<CallToolResult, McpError> {
        Ok(spawn_pane_impl(input, "spawn-pane").await)
    }

    #[tool(
        name = "spawn-pane-with-target",
        description = "Spawn a new pane anchored at a specific existing pane. Pass `target_pane_id` to focus that pane before spawning, `direction` for the split direction, and `keep_focus_on` to restore focus after the new pane is created.",
        output_schema = rmcp::handler::server::common::schema_for_type::<SpawnPaneOutput>()
    )]
    async fn spawn_pane_with_target(
        &self,
        Parameters(input): Parameters<SpawnPaneInput>,
    ) -> Result<CallToolResult, McpError> {
        Ok(spawn_pane_impl(input, "spawn-pane-with-target").await)
    }

    #[tool(
        name = "send-text",
        description = "Type text into a specific pane. With `submit: true` (and default newline_mode `enter`), follows the text with byte 13 (CR) so prompts/REPLs execute. Use `newline_mode: shift_enter` (byte 10, LF) for tools like Claude Code's prompt composer that treat enter as submit and shift+enter as newline. Always pass `pane_id`; this MCP intentionally has no `send to focused pane` shortcut.",
        output_schema = rmcp::handler::server::common::schema_for_type::<OkOutput>()
    )]
    async fn send_text(
        &self,
        Parameters(input): Parameters<SendTextInput>,
    ) -> Result<CallToolResult, McpError> {
        let session = input.session.as_deref();
        if let Err(e) = zellij::write_chars(session, &input.pane_id, &input.text).await {
            return Ok(err(format!("send-text: write-chars failed: {e}")));
        }
        if input.submit.unwrap_or(false) {
            let mode = input
                .newline_mode
                .as_deref()
                .unwrap_or("enter")
                .to_lowercase();
            let byte = match mode.as_str() {
                "enter" => 13u8,
                "shift_enter" => 10u8,
                other => {
                    return Ok(err(format!(
                        "send-text: newline_mode must be `enter` or `shift_enter`, got `{other}`"
                    )));
                }
            };
            if let Err(e) = zellij::write_byte(session, &input.pane_id, byte).await {
                return Ok(err(format!("send-text: submit byte write failed: {e}")));
            }
        }
        Ok(structured(&OkOutput { ok: true }))
    }

    #[tool(
        name = "read-pane",
        description = "Capture the visible viewport (or full scrollback with `full: true`) of a pane as plain text. Use this to poll a background pane's output after send-text, or to confirm a spawned pane has reached an expected state.",
        annotations(read_only_hint = true),
        output_schema = rmcp::handler::server::common::schema_for_type::<ReadPaneOutput>()
    )]
    async fn read_pane(
        &self,
        Parameters(input): Parameters<ReadPaneInput>,
    ) -> Result<CallToolResult, McpError> {
        let session = input.session.as_deref();
        let full = input.full.unwrap_or(false);
        match zellij::dump_screen(session, &input.pane_id, full).await {
            Ok(text) => Ok(structured(&ReadPaneOutput { text })),
            Err(e) => Ok(err(format!("read-pane: {e}"))),
        }
    }

    #[tool(
        name = "focus-pane",
        description = "Move focus to a specific pane by id. Note: `spawn-pane` already exposes `keep_focus_on` — prefer that to avoid focus-bounce. Use this tool only for explicit user-driven focus changes.",
        output_schema = rmcp::handler::server::common::schema_for_type::<OkOutput>()
    )]
    async fn focus_pane(
        &self,
        Parameters(input): Parameters<PaneTargetInput>,
    ) -> Result<CallToolResult, McpError> {
        let session = input.session.as_deref();
        match zellij::focus_pane_id(session, &input.pane_id).await {
            Ok(()) => Ok(structured(&OkOutput { ok: true })),
            Err(e) => Ok(err(format!("focus-pane: {e}"))),
        }
    }

    #[tool(
        name = "resize-pane",
        description = "Resize a specific pane by id. `direction` is the resize operation and must be `increase` or `decrease`.",
        output_schema = rmcp::handler::server::common::schema_for_type::<OkOutput>()
    )]
    async fn resize_pane(
        &self,
        Parameters(input): Parameters<ResizePaneInput>,
    ) -> Result<CallToolResult, McpError> {
        let session = input.session.as_deref();
        let direction = input.direction.to_lowercase();
        if !matches!(direction.as_str(), "increase" | "decrease") {
            return Ok(err(format!(
                "resize-pane: direction must be `increase` or `decrease`, got `{}`",
                input.direction
            )));
        }
        match zellij::resize_pane(session, &input.pane_id, &direction).await {
            Ok(()) => Ok(structured(&OkOutput { ok: true })),
            Err(e) => Ok(err(format!("resize-pane: {e}"))),
        }
    }

    #[tool(
        name = "kill-pane",
        description = "Close a specific pane by id. Permanent — the pane and its running command are terminated.",
        annotations(destructive_hint = true),
        output_schema = rmcp::handler::server::common::schema_for_type::<OkOutput>()
    )]
    async fn kill_pane(
        &self,
        Parameters(input): Parameters<PaneTargetInput>,
    ) -> Result<CallToolResult, McpError> {
        let session = input.session.as_deref();
        match zellij::close_pane(session, &input.pane_id).await {
            Ok(()) => Ok(structured(&OkOutput { ok: true })),
            Err(e) => Ok(err(format!("kill-pane: {e}"))),
        }
    }
}

#[tool_handler]
impl ServerHandler for ZellijMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_instructions(
                "zellij-mcp wraps the zellij CLI for agent orchestration. \
             Call list-sessions when you need to discover available session names. \
             ALWAYS call list-panes first within the target session to discover pane ids — \
             pane ids look like `terminal_3` and are stable for the pane's lifetime. \
             ALWAYS pass an explicit pane_id; this server has no 'send to current focus' \
             shortcut by design. To spawn a background pane without losing focus, call \
             spawn-pane with keep_focus_on set to your own pane id (read it from the \
             ZELLIJ_PANE_ID env). To anchor a split next to an existing pane, pass \
             target_pane_id or use spawn-pane-with-target."
                    .to_string(),
            )
    }
}

// ----------------------------------------------------------------------------
// Helpers
// ----------------------------------------------------------------------------

/// Convert one entry from `zellij action list-panes -j -a` into our `PaneSummary`.
fn parse_pane(v: &serde_json::Value) -> PaneSummary {
    let id_int = v.get("id").and_then(|x| x.as_u64()).unwrap_or(0);
    let is_plugin = v
        .get("is_plugin")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    let prefix = if is_plugin { "plugin" } else { "terminal" };
    PaneSummary {
        id: format!("{prefix}_{id_int}"),
        title: v
            .get("title")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        is_focused: v
            .get("is_focused")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        is_plugin,
        is_floating: v
            .get("is_floating")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        exited: v.get("exited").and_then(|x| x.as_bool()).unwrap_or(false),
        tab_id: v.get("tab_id").and_then(|x| x.as_u64()).unwrap_or(0),
        tab_name: v
            .get("tab_name")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        command: v
            .get("pane_command")
            .or_else(|| v.get("terminal_command"))
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        cwd: v
            .get("pane_cwd")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
    }
}

fn parse_sessions(raw: &str) -> Result<Vec<SessionSummary>, String> {
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(parse_session_line)
        .collect()
}

fn parse_session_line(line: &str) -> Result<SessionSummary, String> {
    let (name, after_created) = line
        .split_once(" [Created ")
        .ok_or_else(|| format!("failed to parse session line `{line}`: missing `[Created ...]`"))?;
    let (created, suffix) = after_created
        .split_once(']')
        .ok_or_else(|| format!("failed to parse session line `{line}`: missing closing `]`"))?;
    let age = created.strip_suffix(" ago").unwrap_or(created).trim();
    Ok(SessionSummary {
        name: name.trim().to_string(),
        created_age_seconds: parse_age_seconds(age)?,
        is_attached: suffix.contains("(current)"),
        is_exited: suffix.contains("EXITED"),
    })
}

fn parse_age_seconds(age: &str) -> Result<u64, String> {
    let mut total = 0u64;
    let mut parsed_any = false;

    for part in age.split_whitespace() {
        let (number, multiplier) = if let Some(number) = part.strip_suffix('h') {
            (number, 3600u64)
        } else if let Some(number) = part.strip_suffix('m') {
            (number, 60u64)
        } else if let Some(number) = part.strip_suffix('s') {
            (number, 1u64)
        } else {
            return Err(format!("unsupported age component `{part}`"));
        };
        let value = number
            .parse::<u64>()
            .map_err(|e| format!("invalid age component `{part}`: {e}"))?;
        let seconds = value
            .checked_mul(multiplier)
            .ok_or_else(|| format!("age component `{part}` overflowed"))?;
        total = total
            .checked_add(seconds)
            .ok_or_else(|| "age total overflowed".to_string())?;
        parsed_any = true;
    }

    if parsed_any {
        Ok(total)
    } else {
        Err("age was empty".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_list_sessions_output() {
        let sessions = parse_sessions(
            "main [Created 1h 41m 1s ago] (current)\n\
             mz [Created 7m 41s ago]\n\
             old [Created 2h ago] EXITED\n",
        )
        .expect("sessions parse");

        assert_eq!(
            sessions,
            vec![
                SessionSummary {
                    name: "main".to_string(),
                    created_age_seconds: 6061,
                    is_attached: true,
                    is_exited: false,
                },
                SessionSummary {
                    name: "mz".to_string(),
                    created_age_seconds: 461,
                    is_attached: false,
                    is_exited: false,
                },
                SessionSummary {
                    name: "old".to_string(),
                    created_age_seconds: 7200,
                    is_attached: false,
                    is_exited: true,
                },
            ]
        );
    }
}
