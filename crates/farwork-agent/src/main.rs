//! farwork-agent entry point: parse the CLI, load the persisted identity, and
//! run the connector loop.

use std::process::ExitCode;

use anyhow::{anyhow, Result};
use farwork_agent::cli;
use farwork_agent::hub::{Connector, HubOptions};
use farwork_agent::identity;
use farwork_agent::protocol::AgentInfo;
use farwork_agent::VERSION;

#[tokio::main]
async fn main() -> ExitCode {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("farwork-agent: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let env_lookup = |key: &str| std::env::var(key).ok();
    let cli = cli::parse(std::env::args().skip(1), &env_lookup)?;
    if cli.show_help {
        print!("{}", cli::HELP);
        return Ok(());
    }
    if cli.show_version {
        println!("farwork-agent {VERSION}");
        return Ok(());
    }

    let token = cli.token.clone().ok_or_else(|| {
        anyhow!("no connector token: pass --token or set FARWORK_CONNECTOR_TOKEN")
    })?;
    let name = cli
        .name
        .clone()
        .unwrap_or_else(|| cli::default_device_name(&env_lookup));

    let identity_path = identity::default_path();
    let identity = identity::load_or_create(&identity_path, &name)?;

    // session.start carries an absolute cwd; resolve it once here.
    let workdir = match cli.workdir.clone() {
        Some(path) => path,
        None => std::env::current_dir()?,
    };
    let workdir = workdir.canonicalize().unwrap_or(workdir);
    if !workdir.is_absolute() {
        return Err(anyhow!(
            "workdir must be an absolute path, got {}",
            workdir.display()
        ));
    }

    tracing::info!(
        agent_id = %identity.agent_id,
        identity_file = %identity_path.display(),
        hub = %cli.hub,
        runtime = cli.runtime.as_str(),
        workdir = %workdir.display(),
        "configured"
    );

    let options = HubOptions {
        hub_url: cli.hub,
        token,
        agent: AgentInfo {
            name,
            platform: String::from(cli::platform()),
            version: String::from(VERSION),
        },
        runtime: cli.runtime,
        workdir,
        codex_bin: cli.codex_bin,
        shell: cli.shell,
        once: cli.once,
    };
    Connector::new(options, identity).run().await
}
