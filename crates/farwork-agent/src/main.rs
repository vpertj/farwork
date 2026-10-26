//! farwork-agent entry point: parse the CLI, load the persisted identity, and
//! run the connector loop.

use std::process::ExitCode;

use anyhow::{anyhow, Result};
use farwork_agent::cli::{self, RuntimeKind};
use farwork_agent::credentials;
use farwork_agent::dsh;
use farwork_agent::hub::{Connector, HubOptions};
use farwork_agent::identity;
use farwork_agent::pair;
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

    let name = cli
        .name
        .clone()
        .unwrap_or_else(|| cli::default_device_name(&env_lookup));

    let identity_path = identity::default_path();
    let mut identity = identity::load_or_create(&identity_path, &name)?;

    // `pair` is a one-shot exchange followed by the normal run: the connector
    // starts with the credentials it just redeemed, so a fresh install is one
    // command. The token is written and used, never logged.
    if cli.pair {
        let code = cli
            .code
            .clone()
            .ok_or_else(|| anyhow!("pair needs --code <code>"))?;
        let claimed = pair::claim(&cli.hub, &code, &name, cli::platform()).await?;
        identity.set_credentials(&cli.hub, &claimed.connector_id, &claimed.token);
        identity::save(&identity_path, &identity)?;
        tracing::info!(
            hub = %cli.hub,
            connector_id = %claimed.connector_id,
            connector_name = %claimed.name,
            identity_file = %identity_path.display(),
            "pairing code redeemed and saved; starting connector"
        );
    }

    let credentials = credentials::resolve(&cli, &identity, &identity_path)?;

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

    // The DSH bridge is located once, at startup. Starting as `--runtime dsh`
    // without one is a configuration error the operator must see before the
    // connector goes online: a session would otherwise be accepted and have
    // nothing to drive.
    let (dsh_endpoint, dsh_selections, dsh_agent_preset) = match cli.runtime {
        RuntimeKind::Dsh => {
            let endpoint = dsh::discovery::resolve(cli.dsh_endpoint.as_deref(), &env_lookup)
                .map_err(|error| anyhow!("{error}"))?;
            // A short name is resolved against the bridge right here: an unknown
            // or ambiguous permission must refuse to start, never quietly fall
            // back to DSH's default.
            let selections = dsh::selections::resolve_remote(
                &endpoint,
                &identity.agent_id,
                &dsh::session_namespace(&identity.agent_id),
                VERSION,
                cli.dsh_permission.as_deref(),
                cli.dsh_model.as_deref(),
                cli.dsh_preset.as_deref(),
            )
            .await
            .map_err(|error| anyhow!("{error}"))?;
            if !selections.is_empty() {
                tracing::info!(
                    permission = ?selections.permission,
                    model = ?selections.model,
                    preset = ?selections.preset,
                    "dsh selections resolved"
                );
            }
            // DSH refuses a session until an Agent mode is chosen, but choosing one
            // here would be a silent decision about how every turn runs. Advisory
            // only: this can never fail startup.
            if cli.dsh_preset.is_none() {
                let namespace = dsh::session_namespace(&identity.agent_id);
                let hint = dsh::selections::preset_hint_remote(
                    &endpoint,
                    &identity.agent_id,
                    &namespace,
                    VERSION,
                )
                .await;
                if let Some(hint) = hint {
                    tracing::warn!("{hint}");
                }
            }
            (Some(endpoint), selections.to_value(), selections.preset)
        }
        _ => (None, None, None),
    };

    tracing::info!(
        agent_id = %identity.agent_id,
        identity_file = %identity_path.display(),
        hub = %credentials.hub,
        credentials = if credentials.from_identity_file {
            "agent.json"
        } else {
            "flags/env"
        },
        runtime = cli.runtime.as_str(),
        workdir = %workdir.display(),
        dsh_endpoint = ?dsh_endpoint.as_ref().map(|endpoint| endpoint.address()),
        dsh_selections = ?dsh_selections,
        dsh_agent_preset = ?dsh_agent_preset,
        "configured"
    );

    let options = HubOptions {
        hub_url: credentials.hub,
        token: credentials.token,
        agent: AgentInfo {
            name,
            platform: String::from(cli::platform()),
            version: String::from(VERSION),
        },
        runtime: cli.runtime,
        workdir,
        codex_bin: cli.codex_bin,
        shell: cli.shell,
        dsh_endpoint,
        dsh_selections,
        dsh_agent_preset,
        once: cli.once,
    };
    Connector::new(options, identity).run().await
}
