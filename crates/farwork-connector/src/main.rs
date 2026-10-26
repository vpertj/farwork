mod cli;
mod client;
mod pty;

use farwork_protocol::{DeviceInfo, Platform};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use crate::cli::{Command, Config, USAGE};
use crate::client::{INITIAL_BACKOFF, MAX_BACKOFF};

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

    // Identity is per process run: a restart shows up as a fresh device.
    let device = DeviceInfo {
        id: Uuid::new_v4().to_string(),
        name: config.name.clone(),
        platform: Platform::current(),
    };
    tracing::info!(
        device_id = %device.id,
        device_name = %device.name,
        platform = ?device.platform,
        server = %config.server,
        "farwork connector starting"
    );

    let mut backoff = INITIAL_BACKOFF;
    loop {
        match client::run_session(&config, &device, &mut backoff).await {
            Ok(()) => tracing::info!("session closed by server"),
            Err(error) => tracing::warn!(
                %error,
                retry_in_ms = backoff.as_millis() as u64,
                "session failed, reconnecting"
            ),
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
