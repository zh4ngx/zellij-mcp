//! Thin wrappers over `zellij action` / `zellij list-sessions`.
//!
//! Every helper shells out to the `zellij` binary on PATH. Errors propagate
//! the captured stderr so the MCP can surface them verbatim to the caller.

use std::process::Stdio;
use tokio::process::Command;

/// Build a `zellij` command, optionally pinned to a named session.
fn zellij(session: Option<&str>) -> Command {
    let mut cmd = Command::new("zellij");
    if let Some(s) = session {
        cmd.arg("--session").arg(s);
    }
    cmd.stdin(Stdio::null());
    cmd
}

async fn run_capturing(mut cmd: Command, ctx: &str) -> anyhow::Result<String> {
    let output = cmd.output().await.map_err(|e| {
        anyhow::anyhow!("failed to spawn zellij ({ctx}): {e}")
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        anyhow::bail!(
            "zellij {ctx} failed (exit {:?}): {}{}",
            output.status.code(),
            stderr,
            if stderr.is_empty() && !stdout.is_empty() {
                format!(" / stdout: {stdout}")
            } else {
                String::new()
            }
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `zellij action list-panes -j -a` — returns raw JSON text from the CLI.
pub async fn list_panes_json(session: Option<&str>) -> anyhow::Result<String> {
    let mut cmd = zellij(session);
    cmd.args(["action", "list-panes", "-j", "-a"]);
    run_capturing(cmd, "action list-panes").await
}

/// `zellij action new-pane [--cwd ...] [-d <dir>] [--floating] [--name <n>] [-- cmd...]`
///
/// Returns the new pane ID as printed by zellij (e.g. `terminal_42`).
pub async fn new_pane(
    session: Option<&str>,
    cwd: &str,
    direction: Option<&str>,
    floating: bool,
    name: Option<&str>,
    command: Option<&[String]>,
) -> anyhow::Result<String> {
    let mut cmd = zellij(session);
    cmd.args(["action", "new-pane"]);
    cmd.arg("--cwd").arg(cwd);
    if let Some(dir) = direction {
        cmd.arg("--direction").arg(dir);
    }
    if floating {
        cmd.arg("--floating");
    }
    if let Some(n) = name {
        cmd.arg("--name").arg(n);
    }
    if let Some(argv) = command {
        if !argv.is_empty() {
            cmd.arg("--");
            for a in argv {
                cmd.arg(a);
            }
        }
    }
    let stdout = run_capturing(cmd, "action new-pane").await?;
    let pane_id = stdout.trim().to_string();
    if pane_id.is_empty() {
        anyhow::bail!("zellij new-pane returned empty pane id");
    }
    Ok(pane_id)
}

/// `zellij action focus-pane-id <PANE_ID>` (positional argument).
pub async fn focus_pane_id(session: Option<&str>, pane_id: &str) -> anyhow::Result<()> {
    let mut cmd = zellij(session);
    cmd.args(["action", "focus-pane-id", pane_id]);
    run_capturing(cmd, "action focus-pane-id").await?;
    Ok(())
}

/// `zellij action write-chars --pane-id <id> <text>`.
pub async fn write_chars(session: Option<&str>, pane_id: &str, text: &str) -> anyhow::Result<()> {
    let mut cmd = zellij(session);
    cmd.args(["action", "write-chars", "--pane-id", pane_id, text]);
    run_capturing(cmd, "action write-chars").await?;
    Ok(())
}

/// `zellij action write --pane-id <id> <byte>` — send a single byte (e.g. 13 = CR).
pub async fn write_byte(session: Option<&str>, pane_id: &str, byte: u8) -> anyhow::Result<()> {
    let mut cmd = zellij(session);
    cmd.args([
        "action",
        "write",
        "--pane-id",
        pane_id,
        &byte.to_string(),
    ]);
    run_capturing(cmd, "action write").await?;
    Ok(())
}

/// `zellij action dump-screen --pane-id <id> [-f]` — captures viewport text.
pub async fn dump_screen(
    session: Option<&str>,
    pane_id: &str,
    full: bool,
) -> anyhow::Result<String> {
    let mut cmd = zellij(session);
    cmd.args(["action", "dump-screen", "--pane-id", pane_id]);
    if full {
        cmd.arg("--full");
    }
    run_capturing(cmd, "action dump-screen").await
}

/// `zellij action close-pane --pane-id <id>`.
pub async fn close_pane(session: Option<&str>, pane_id: &str) -> anyhow::Result<()> {
    let mut cmd = zellij(session);
    cmd.args(["action", "close-pane", "--pane-id", pane_id]);
    run_capturing(cmd, "action close-pane").await?;
    Ok(())
}
