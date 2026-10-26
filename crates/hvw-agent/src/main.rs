//! hvw-agent: the member daemon of herdr-vm-worktree.

mod attach;
mod broker;
mod config;
mod farm;
mod gatekeeper;
mod git;
mod incus;
mod server;
mod state;

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

#[derive(Parser)]
#[command(version, about = "The member daemon of herdr-vm-worktree")]
struct Args {
    /// The configuration file of the agent.
    #[arg(long, default_value = "/etc/hvw/agent.json")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,iroh=warn,noq=warn".into()),
        )
        .without_time()
        .init();
    let args = Args::parse();
    let cfg = config::Config::load(&args.config)?;
    server::run(cfg).await
}
