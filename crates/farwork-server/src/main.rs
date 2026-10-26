use std::net::SocketAddr;

use farwork_server::cli::{Command, Config, USAGE};
use farwork_server::{serve, AppState};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = match Config::parse(std::env::args().skip(1)) {
        Ok(Command::Run(config)) => config,
        Ok(Command::Help) => {
            println!("{USAGE}");
            return Ok(());
        }
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    init_tracing();

    let token = std::env::var("FARWORK_TOKEN")
        .ok()
        .filter(|token| !token.is_empty());
    if token.is_none() {
        tracing::warn!("FARWORK_TOKEN is not set: accepting unauthenticated connections");
    }

    let listener = match farwork_server::listen::bind(config.port).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("cannot start server: {error}");
            std::process::exit(1);
        }
    };
    let addr: SocketAddr = listener.local_addr()?;
    tracing::info!(%addr, auth = token.is_some(), "farwork server listening");

    serve(listener, AppState::new(token)).await?;
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
