use std::process::Command as ProcessCommand;

pub const DEFAULT_SERVER: &str = "ws://127.0.0.1:7300/ws/connector";

pub const USAGE: &str = r#"usage: farwork-connector [--server <URL>] [--name <NAME>] [--token <TOKEN>]

options:
  --server <URL>   server connector endpoint (default ws://127.0.0.1:7300/ws/connector)
  --name <NAME>    device name shown in the device list (default: hostname)
  --token <TOKEN>  auth token (default: $FARWORK_TOKEN)"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub server: String,
    pub name: String,
    pub token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Run(Config),
    Help,
}

impl Config {
    pub fn parse<I, S>(args: I) -> Result<Command, String>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut args = args.into_iter().map(Into::into);
        let mut server = DEFAULT_SERVER.to_string();
        let mut name = default_device_name();
        let mut token = std::env::var("FARWORK_TOKEN")
            .ok()
            .filter(|token| !token.is_empty());

        while let Some(arg) = args.next() {
            let (flag, inline) = match arg.split_once('=') {
                Some((flag, value)) if flag.starts_with("--") => {
                    (flag.to_string(), Some(value.to_string()))
                }
                _ => (arg.clone(), None),
            };
            match flag.as_str() {
                "--help" | "-h" => return Ok(Command::Help),
                "--server" => server = take_value(&flag, inline, &mut args)?,
                "--name" => name = take_value(&flag, inline, &mut args)?,
                "--token" => token = Some(take_value(&flag, inline, &mut args)?),
                _ => return Err(format!("unexpected argument \"{arg}\"\n{USAGE}")),
            }
        }

        Ok(Command::Run(Config {
            server,
            name,
            token,
        }))
    }

    /// Connector endpoint including the `?token=` the server expects.
    pub fn url(&self) -> String {
        match &self.token {
            None => self.server.clone(),
            Some(token) => {
                let separator = if self.server.contains('?') { '&' } else { '?' };
                format!("{}{separator}token={token}", self.server)
            }
        }
    }
}

fn take_value<I: Iterator<Item = String>>(
    flag: &str,
    inline: Option<String>,
    args: &mut I,
) -> Result<String, String> {
    match inline {
        Some(value) if !value.is_empty() => Ok(value),
        _ => args
            .next()
            .ok_or_else(|| format!("{flag} needs a value\n{USAGE}")),
    }
}

fn default_device_name() -> String {
    for variable in ["FARWORK_DEVICE_NAME", "HOSTNAME"] {
        if let Ok(value) = std::env::var(variable) {
            if !value.trim().is_empty() {
                return value;
            }
        }
    }
    if let Ok(value) = std::fs::read_to_string("/proc/sys/kernel/hostname") {
        let value = value.trim();
        if !value.is_empty() {
            return value.to_string();
        }
    }
    if let Ok(output) = ProcessCommand::new("hostname").output() {
        if output.status.success() {
            let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !value.is_empty() {
                return value;
            }
        }
    }
    "farwork-connector".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn parse(values: &[&str]) -> Config {
        match Config::parse(args(values)).unwrap() {
            Command::Run(config) => config,
            Command::Help => panic!("expected a run command"),
        }
    }

    #[test]
    fn defaults_are_applied() {
        let config = parse(&[]);
        assert_eq!(config.server, DEFAULT_SERVER);
        assert!(!config.name.is_empty());
    }

    #[test]
    fn accepts_space_and_inline_values() {
        let config = parse(&[
            "--server",
            "ws://10.0.0.2:7300/ws/connector",
            "--name",
            "box",
        ]);
        assert_eq!(config.server, "ws://10.0.0.2:7300/ws/connector");
        assert_eq!(config.name, "box");

        let inline = parse(&["--server=ws://127.0.0.1:9999/ws/connector", "--name=box2"]);
        assert_eq!(inline.server, "ws://127.0.0.1:9999/ws/connector");
        assert_eq!(inline.name, "box2");
    }

    #[test]
    fn token_is_appended_to_the_url() {
        let config = Config {
            server: DEFAULT_SERVER.to_string(),
            name: "box".to_string(),
            token: Some("secret".to_string()),
        };
        assert_eq!(config.url(), format!("{DEFAULT_SERVER}?token=secret"));

        let config = Config {
            server: DEFAULT_SERVER.to_string(),
            name: "box".to_string(),
            token: None,
        };
        assert_eq!(config.url(), DEFAULT_SERVER);

        let config = Config {
            server: format!("{DEFAULT_SERVER}?x=1"),
            name: "box".to_string(),
            token: Some("secret".to_string()),
        };
        assert_eq!(config.url(), format!("{DEFAULT_SERVER}?x=1&token=secret"));
    }

    #[test]
    fn rejects_missing_values_and_unknown_flags() {
        assert!(Config::parse(args(&["--server"])).is_err());
        assert!(Config::parse(args(&["--name"])).is_err());
        assert!(Config::parse(args(&["--token"])).is_err());
        assert!(Config::parse(args(&["--bogus"])).is_err());
    }

    #[test]
    fn help_is_reported() {
        assert_eq!(Config::parse(args(&["--help"])).unwrap(), Command::Help);
        assert_eq!(Config::parse(args(&["-h"])).unwrap(), Command::Help);
    }
}
