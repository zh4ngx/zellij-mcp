//! MCP server: 6 tools wrapping zellij CLI primitives.
//!
//! Design rules (lifted from the audit of bnomei/tmux-mcp + GitJuhb/zellij-mcp-server):
//!  - `pane_id` is REQUIRED on every per-pane tool. No "current focus" fallbacks.
//!  - `list-panes` returns typed JSON (`output_schema` set on the tool).
//!  - `spawn-pane` exposes `keep_focus_on` so callers can spawn background panes
//!    without losing their seat in the foreground pane.
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
pub struct ListPanesInput {
    /// Optional zellij session name. If omitted, uses the session inherited from
    /// the parent process's `ZELLIJ` environment, or fails if none exists.
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

// ----------------------------------------------------------------------------
// Output schemas
// ----------------------------------------------------------------------------

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

#[tool_router]
impl ZellijMcpServer {
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
            Some(arr) => arr
                .iter()
                .map(parse_pane)
                .collect::<Vec<_>>(),
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
        let session = input.session.as_deref();
        let direction_norm = input.direction.as_deref().map(|d| d.to_lowercase());
        let direction = direction_norm.as_deref();
        if let Some(d) = direction {
            if !matches!(d, "right" | "down" | "left" | "up") {
                return Ok(err(format!(
                    "spawn-pane: direction must be one of right|down|left|up, got `{d}`"
                )));
            }
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
            Err(e) => return Ok(err(format!("spawn-pane: {e}"))),
        };
        if let Some(target) = input.keep_focus_on.as_deref() {
            if let Err(e) = zellij::focus_pane_id(session, target).await {
                return Ok(err(format!(
                    "spawn-pane: pane `{pane_id}` created, but restoring focus to `{target}` failed: {e}"
                )));
            }
        }
        Ok(structured(&SpawnPaneOutput { pane_id }))
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
            let mode = input.newline_mode.as_deref().unwrap_or("enter").to_lowercase();
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
        ServerInfo::new(
            ServerCapabilities::builder().enable_tools().build(),
        )
        .with_server_info(Implementation::from_build_env())
        .with_protocol_version(ProtocolVersion::V_2024_11_05)
        .with_instructions(
            "zellij-mcp wraps the zellij CLI for agent orchestration. \
             ALWAYS call list-panes first to discover pane ids — pane ids look like \
             `terminal_3` and are stable for the pane's lifetime. ALWAYS pass an explicit \
             pane_id; this server has no 'send to current focus' shortcut by design. \
             To spawn a background pane without losing focus, call spawn-pane with \
             keep_focus_on set to your own pane id (read it from the ZELLIJ_PANE_ID env)."
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
    let is_plugin = v.get("is_plugin").and_then(|x| x.as_bool()).unwrap_or(false);
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
