pub const DEFAULT_PORT: u16 = 7300;

pub const USAGE: &str = r#"usage: farwork-server [--port <PORT>]

options:
  --port <PORT>  TCP port to listen on (default 7300)

environment:
  FARWORK_TOKEN  when set, REST calls need "Authorization: Bearer <token>"
                 and WebSocket calls need the "?token=<token>" query parameter
  RUST_LOG       log filter (default "info")"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Run(Config),
    Help,
}

impl Config {
    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Command, String> {
        let mut args = args.into_iter();
        let mut port = DEFAULT_PORT;

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--help" | "-h" => return Ok(Command::Help),
                "--port" => {
                    let value = args
                        .next()
                        .ok_or_else(|| format!("--port needs a value\n{USAGE}"))?;
                    port = parse_port(&value)?;
                }
                other => match other.strip_prefix("--port=") {
                    Some(value) => port = parse_port(value)?,
                    None => return Err(format!("unexpected argument \"{other}\"\n{USAGE}")),
                },
            }
        }

        Ok(Command::Run(Config { port }))
    }
}

fn parse_port(value: &str) -> Result<u16, String> {
    value
        .parse::<u16>()
        .map_err(|_| format!("invalid port \"{value}\"\n{USAGE}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn defaults_to_port_7300() {
        assert_eq!(
            Config::parse(args(&[])).unwrap(),
            Command::Run(Config { port: DEFAULT_PORT })
        );
    }

    #[test]
    fn accepts_separate_and_inline_port_values() {
        let expected = Command::Run(Config { port: 8123 });
        assert_eq!(Config::parse(args(&["--port", "8123"])).unwrap(), expected);
        assert_eq!(Config::parse(args(&["--port=8123"])).unwrap(), expected);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(Config::parse(args(&["--port"])).is_err());
        assert!(Config::parse(args(&["--port", "abc"])).is_err());
        assert!(Config::parse(args(&["--nope"])).is_err());
    }

    #[test]
    fn help_wins_over_port() {
        assert_eq!(
            Config::parse(args(&["--help", "--port", "9000"])).unwrap(),
            Command::Help
        );
    }
}
