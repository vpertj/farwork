//! \`farwork-hub\` entrypoint: \`--port\` overrides the default 7300.

use std::process::ExitCode;

use farwork_hub::config::{parse_port_arg, Config};
use farwork_hub::{app, db, AppState};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // Startup failures must be loud and non-zero: a hub that cannot
            // serve is worse than one that refuses to start.
            eprintln!("farwork-hub: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mut config = Config::from_env().map_err(|error| anyhow::anyhow!(error.to_string()))?;
    if let Some(port) = parse_port_arg(&args).map_err(|error| anyhow::anyhow!(error.to_string()))? {
        config = config.with_port_override(port);
    }

    farwork_hub::routes::sessions::warn_if_dev_runtimes_enabled();

    let pool = db::connect(&config).await?;
    db::migrate(&pool).await?;
    let state = AppState::new(config.clone(), pool);

    let listener = TcpListener::bind(("0.0.0.0", config.port))
        .await
        .map_err(|error| anyhow::anyhow!("cannot bind port {}: {error}", config.port))?;
    tracing::info!(
        version = farwork_hub::VERSION,
        port = config.port,
        "farwork-hub listening"
    );
    // Connect info, not a forwarded header: the pairing claim limiter keys on the
    // real peer address (see `routes::pairing`).
    axum::serve(
        listener,
        app(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}
