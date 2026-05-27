mod auth;
mod client;
mod error;
mod hid;
mod image;
mod server;
mod ws;

use anyhow::{Context, Result};
use clap::Parser;
use rmcp::{ServiceExt, transport::stdio};
use tracing_subscriber::EnvFilter;

use crate::client::{Config, NanoKvmClient};
use crate::server::NanoKvmServer;

/// MCP server exposing Sipeed NanoKVM control as tools.
#[derive(Debug, Parser)]
#[command(
    name = "nanokvm-mcp",
    version,
    about = "MCP server for controlling Sipeed NanoKVM devices"
)]
struct Cli {
    /// NanoKVM IP address or hostname.
    #[arg(long, env = "NANOKVM_HOST")]
    host: String,

    /// Web UI username.
    #[arg(long, env = "NANOKVM_USER", default_value = "admin")]
    user: String,

    /// Web UI password.
    #[arg(long, env = "NANOKVM_PASS", default_value = "admin")]
    pass: String,

    /// Target screen width in pixels (used for mouse coordinate mapping).
    #[arg(long, env = "NANOKVM_SCREEN_WIDTH", default_value_t = 1920)]
    screen_width: u32,

    /// Target screen height in pixels.
    #[arg(long, env = "NANOKVM_SCREEN_HEIGHT", default_value_t = 1080)]
    screen_height: u32,

    /// Use HTTPS / WSS instead of HTTP / WS.
    #[arg(long, env = "NANOKVM_HTTPS", default_value_t = false)]
    https: bool,

    /// Verify TLS certificates. Pass `false` for self-signed devices.
    #[arg(
        long,
        env = "NANOKVM_VERIFY_SSL",
        default_value_t = true,
        action = clap::ArgAction::Set,
    )]
    verify_ssl: bool,

    /// Allowlist for nanokvm_upload_iso: a directory the tool is allowed to
    /// read ISOs from. Any path outside (after resolving `..` and symlinks)
    /// is rejected. If unset, the upload tool is disabled.
    #[arg(long, env = "NANOKVM_ISO_DIR")]
    iso_dir: Option<std::path::PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Logs go to stderr so they don't pollute the stdio MCP transport on stdout.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let cli = Cli::parse();
    tracing::info!(host = %cli.host, "starting nanokvm-mcp");

    let client = NanoKvmClient::new(Config {
        host: cli.host,
        username: cli.user,
        password: cli.pass,
        screen_width: cli.screen_width,
        screen_height: cli.screen_height,
        use_https: cli.https,
        verify_ssl: cli.verify_ssl,
        iso_dir: cli.iso_dir,
    })
    .context("failed to build NanoKVM client")?;

    let service = NanoKvmServer::new(client)
        .serve(stdio())
        .await
        .context("failed to start MCP service")?;
    service.waiting().await.context("MCP service exited with error")?;
    Ok(())
}
