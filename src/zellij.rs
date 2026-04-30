//! Thin wrappers over `zellij action` / `zellij list-sessions`.
//!
//! Every helper shells out to the `zellij` binary on PATH. Errors propagate
//! the captured stderr so the MCP can surface them verbatim to the caller.

use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;

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
    let output = cmd
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("failed to spawn zellij ({ctx}): {e}"))?;
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

/// `zellij list-sessions -n` — returns raw, no-format session text.
pub async fn list_sessions() -> anyhow::Result<String> {
    let mut cmd = zellij(None);
    cmd.args(["list-sessions", "-n"]);
    run_capturing(cmd, "list-sessions").await
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
    let pane_id = run_new_pane(session, cwd, direction, floating, name, command).await?;
    if wait_for_listed_pane(session, &pane_id).await? {
        return Ok(pane_id);
    }

    if direction.is_some() {
        // zellij 0.44.1 can return a terminal_N id for directed splits in
        // detached sessions without inserting an addressable pane. Its automatic
        // placement path does materialize the pane, so fall back before exposing
        // a phantom id to later pane-id actions.
        let fallback_pane_id = run_new_pane(session, cwd, None, floating, name, command).await?;
        if wait_for_listed_pane(session, &fallback_pane_id).await? {
            return Ok(fallback_pane_id);
        }
        anyhow::bail!(
            "zellij new-pane returned `{fallback_pane_id}` after retrying without direction, but the pane did not appear in list-panes; directed spawn had returned `{pane_id}`"
        );
    }

    anyhow::bail!(
        "zellij new-pane returned `{pane_id}`, but the pane did not appear in list-panes"
    );
}

async fn run_new_pane(
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
    if let Some(argv) = command
        && !argv.is_empty()
    {
        cmd.arg("--");
        for a in argv {
            cmd.arg(a);
        }
    }
    let stdout = run_capturing(cmd, "action new-pane").await?;
    let pane_id = stdout.trim().to_string();
    if pane_id.is_empty() {
        anyhow::bail!("zellij new-pane returned empty pane id");
    }
    Ok(pane_id)
}

async fn wait_for_listed_pane(session: Option<&str>, pane_id: &str) -> anyhow::Result<bool> {
    if pane_is_listed(session, pane_id).await? {
        return Ok(true);
    }

    for delay in [
        Duration::from_millis(20),
        Duration::from_millis(40),
        Duration::from_millis(80),
        Duration::from_millis(160),
        Duration::from_millis(320),
    ] {
        sleep(delay).await;
        if pane_is_listed(session, pane_id).await? {
            return Ok(true);
        }
    }

    Ok(false)
}

async fn pane_is_listed(session: Option<&str>, pane_id: &str) -> anyhow::Result<bool> {
    let Some((target_is_plugin, target_id)) = parse_pane_id(pane_id) else {
        anyhow::bail!("invalid pane id `{pane_id}`");
    };
    let raw = list_panes_json(session).await?;
    let parsed: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("failed to parse zellij list-panes JSON: {e}"))?;
    let panes = parsed
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("zellij list-panes output was not a JSON array"))?;

    Ok(panes.iter().any(|pane| {
        let id = pane.get("id").and_then(|id| id.as_u64());
        let is_plugin = pane
            .get("is_plugin")
            .and_then(|is_plugin| is_plugin.as_bool())
            .unwrap_or(false);
        id == Some(target_id) && is_plugin == target_is_plugin
    }))
}

fn parse_pane_id(pane_id: &str) -> Option<(bool, u64)> {
    if let Some(id) = pane_id.strip_prefix("terminal_") {
        id.parse().ok().map(|id| (false, id))
    } else if let Some(id) = pane_id.strip_prefix("plugin_") {
        id.parse().ok().map(|id| (true, id))
    } else {
        pane_id.parse().ok().map(|id| (false, id))
    }
}

/// `zellij action focus-pane-id <PANE_ID>` (positional argument).
pub async fn focus_pane_id(session: Option<&str>, pane_id: &str) -> anyhow::Result<()> {
    let mut cmd = zellij(session);
    cmd.args(["action", "focus-pane-id", pane_id]);
    let output = cmd
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("failed to spawn zellij (action focus-pane-id): {e}"))?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.contains("already focused") {
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Err(anyhow::anyhow!(
        "zellij action focus-pane-id failed (exit {:?}): {}{}",
        output.status.code(),
        stderr,
        if stderr.is_empty() && !stdout.is_empty() {
            format!(" / stdout: {stdout}")
        } else {
            String::new()
        }
    ))
}

/// `zellij action resize --pane-id <id> <increase|decrease>`.
pub async fn resize_pane(
    session: Option<&str>,
    pane_id: &str,
    direction: &str,
) -> anyhow::Result<()> {
    let retry_delays = [
        Duration::from_millis(20),
        Duration::from_millis(40),
        Duration::from_millis(80),
        Duration::from_millis(160),
    ];

    for delay in retry_delays {
        let mut cmd = zellij(session);
        cmd.args(["action", "resize", "--pane-id", pane_id, direction]);
        match run_capturing(cmd, "action resize").await {
            Ok(_) => return Ok(()),
            Err(e) if is_pane_not_found(&e.to_string()) => {
                // zellij can print a new pane id before resize's pane lookup sees it.
                // A short retry keeps immediate spawn -> resize calls reliable while
                // preserving the original error for genuinely missing pane ids.
                sleep(delay).await;
            }
            Err(e) => return Err(e),
        }
    }

    let mut cmd = zellij(session);
    cmd.args(["action", "resize", "--pane-id", pane_id, direction]);
    run_capturing(cmd, "action resize").await?;
    Ok(())
}

fn is_pane_not_found(message: &str) -> bool {
    message.contains("Pane with id") && message.contains("not found")
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
    cmd.args(["action", "write", "--pane-id", pane_id, &byte.to_string()]);
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
