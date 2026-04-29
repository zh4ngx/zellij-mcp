//! zellij-mcp: small MCP server wrapping the zellij CLI.
//!
//! Exposes six tools (`list-panes`, `spawn-pane`, `send-text`, `read-pane`,
//! `focus-pane`, `kill-pane`), all addressing panes by zellij's stable
//! `terminal_<n>` / `plugin_<n>` IDs. Designed for agents that orchestrate
//! background panes without disturbing user focus.

mod server;
mod zellij;

use rmcp::ServiceExt;
use rmcp::transport::stdio;
use server::ZellijMcpServer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    tracing::info!("zellij-mcp starting on stdio");

    let server = ZellijMcpServer::new();
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
