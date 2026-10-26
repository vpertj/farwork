//! Which hub and connector token a run uses.
//!
//! Precedence: whatever is on the command line (or in the token env var) first,
//! then whatever `pair` wrote into agent.json. Explicit flags therefore always
//! win, and a paired device needs no arguments at all.

use std::path::Path;

use crate::cli::Cli;
use crate::identity::AgentIdentity;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub hub: String,
    pub token: String,
    /// True when the token came from agent.json rather than from --token or
    /// FARWORK_CONNECTOR_TOKEN (logged, so an operator can tell them apart).
    pub from_identity_file: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredentialError {
    #[error(
        "no connector token: agent.json ({path}) has no credentials yet \u{2014} run \
         `farwork-agent pair --hub <url> --code <code>` to pair this device, or pass \
         --token (and --hub)"
    )]
    MissingToken { path: String },
    #[error(
        "agent.json ({path}) has a hub but no connector token \u{2014} pair again with \
         `farwork-agent pair --hub <url> --code <code>`, or pass --token"
    )]
    HubWithoutToken { path: String },
    #[error(
        "agent.json ({path}) has a connector token but no hub \u{2014} re-pair with \
         `farwork-agent pair --hub <url> --code <code>`"
    )]
    TokenWithoutHub { path: String },
    #[error("--token was given but is empty; omit it to use the token in agent.json")]
    EmptyTokenFlag,
}

pub fn resolve(
    cli: &Cli,
    identity: &AgentIdentity,
    path: &Path,
) -> Result<Credentials, CredentialError> {
    let stored_hub = identity.stored_hub();
    let stored_token = identity.stored_token();
    let path = path.display().to_string();

    let hub = match (cli.hub_explicit, stored_hub) {
        (true, _) => cli.hub.trim().to_owned(),
        (false, Some(hub)) => hub.to_owned(),
        (false, None) => cli.hub.trim().to_owned(),
    };

    let token = match cli.token.as_deref() {
        Some(raw) if raw.trim().is_empty() => return Err(CredentialError::EmptyTokenFlag),
        Some(raw) => raw.trim().to_owned(),
        None => match stored_token {
            Some(token) => token.to_owned(),
            None => {
                return Err(if stored_hub.is_some() {
                    CredentialError::HubWithoutToken { path }
                } else {
                    CredentialError::MissingToken { path }
                })
            }
        },
    };
    let from_identity_file = cli.token.is_none();

    // A token read from the file belongs to the hub stored next to it. Using it
    // against the built-in localhost default would fail at the handshake with a
    // message about a bad token, not about the real mistake.
    if from_identity_file && !cli.hub_explicit && stored_hub.is_none() {
        return Err(CredentialError::TokenWithoutHub { path });
    }

    Ok(Credentials {
        hub,
        token,
        from_identity_file,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{self, Cli};
    use crate::identity::AgentIdentity;
    use std::path::PathBuf;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn cli_with(items: &[&str]) -> Cli {
        let args: Vec<String> = items.iter().map(|item| (*item).to_owned()).collect();
        cli::parse(args, &no_env).unwrap()
    }

    fn paired() -> AgentIdentity {
        AgentIdentity {
            agent_id: String::from("agt_test"),
            name: String::from("mbp"),
            created_at: String::from("2026-01-01T00:00:00Z"),
            hub: Some(String::from("https://hub.example.com")),
            connector_id: Some(String::from("cn_1")),
            connector_token: Some(String::from("cxt_file")),
        }
    }

    fn legacy() -> AgentIdentity {
        AgentIdentity {
            hub: None,
            connector_id: None,
            connector_token: None,
            ..paired()
        }
    }

    fn path() -> PathBuf {
        PathBuf::from("/tmp/farwork/agent.json")
    }

    #[test]
    fn no_arguments_reuses_the_hub_and_token_from_agent_json() {
        let resolved = resolve(&cli_with(&[]), &paired(), &path()).unwrap();
        assert_eq!(resolved.hub, "https://hub.example.com");
        assert_eq!(resolved.token, "cxt_file");
        assert!(resolved.from_identity_file);
    }

    #[test]
    fn explicit_flags_override_the_file() {
        let cli = cli_with(&["--hub", "ws://10.0.0.2:7300", "--token", "cxt_flag"]);
        let resolved = resolve(&cli, &paired(), &path()).unwrap();
        assert_eq!(resolved.hub, "ws://10.0.0.2:7300");
        assert_eq!(resolved.token, "cxt_flag");
        assert!(!resolved.from_identity_file);

        // Overriding only the hub keeps the paired token, and vice versa.
        let hub_only = resolve(&cli_with(&["--hub", "ws://h:1"]), &paired(), &path()).unwrap();
        assert_eq!(hub_only.hub, "ws://h:1");
        assert_eq!(hub_only.token, "cxt_file");
        assert!(hub_only.from_identity_file);
        let token_only = resolve(&cli_with(&["--token", "cxt_flag"]), &paired(), &path()).unwrap();
        assert_eq!(token_only.hub, "https://hub.example.com");
        assert_eq!(token_only.token, "cxt_flag");
    }

    #[test]
    fn an_unpaired_file_explains_pair_or_token() {
        let error = resolve(&cli_with(&[]), &legacy(), &path()).unwrap_err();
        let shown = error.to_string();
        assert!(shown.contains("farwork-agent pair --hub"), "{shown}");
        assert!(shown.contains("--token"), "{shown}");
        assert!(shown.contains("/tmp/farwork/agent.json"), "{shown}");
        assert!(matches!(error, CredentialError::MissingToken { .. }));
    }

    #[test]
    fn a_legacy_file_still_works_with_an_explicit_token() {
        let cli = cli_with(&["--token", "cxt_flag"]);
        let resolved = resolve(&cli, &legacy(), &path()).unwrap();
        // The pre-pairing default hub is what a bare --token has always meant.
        assert_eq!(resolved.hub, "ws://127.0.0.1:7300");
        assert_eq!(resolved.token, "cxt_flag");
    }

    #[test]
    fn half_written_files_name_the_missing_half() {
        let hub_only = AgentIdentity {
            connector_token: None,
            ..paired()
        };
        let error = resolve(&cli_with(&[]), &hub_only, &path()).unwrap_err();
        assert!(matches!(error, CredentialError::HubWithoutToken { .. }));
        assert!(error.to_string().contains("farwork-agent pair"), "{error}");

        let token_only = AgentIdentity {
            hub: None,
            ..paired()
        };
        let error = resolve(&cli_with(&[]), &token_only, &path()).unwrap_err();
        assert!(matches!(error, CredentialError::TokenWithoutHub { .. }));
    }

    #[test]
    fn an_explicit_empty_token_is_refused_instead_of_silently_ignored() {
        let error = resolve(&cli_with(&["--token", ""]), &paired(), &path()).unwrap_err();
        assert_eq!(error, CredentialError::EmptyTokenFlag);
    }
}
