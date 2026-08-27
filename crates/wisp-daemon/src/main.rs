use std::path::PathBuf;

use clap::Parser;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(version, about = "Wisp autocomplete daemon")]
struct Args {
    #[arg(long)]
    socket: Option<PathBuf>,
    #[arg(long)]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "wisp=info".into()))
        .init();
    let args = Args::parse();
    let socket = args.socket.unwrap_or_else(wisp_daemon::default_socket_path);
    let config = args.config.unwrap_or_else(wisp_daemon::default_config_path);
    let shutdown = CancellationToken::new();
    let signal = shutdown.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal.cancel();
        }
    });
    wisp_daemon::run(socket, config, shutdown).await
}
