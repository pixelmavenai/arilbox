//! Command-line arguments. Small enough not to need an argument-parsing crate.

use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::bail;
use arilbox_core::config::DEFAULT_CONFIG_PATH;

/// Options for a normal run.
#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    /// Config file path.
    pub config: PathBuf,
}

/// Outcome of parsing.
#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    /// Start the daemon.
    Run(Args),
    /// Print this text and exit successfully (`--help`, `--version`).
    Exit(String),
}

const USAGE: &str = "\
Usage: arilboxd [--config <path>]

Options:
  -c, --config <path>  Config file (default: /etc/arilbox/config.toml)
  -h, --help           Show this help
  -V, --version        Show version";

impl Args {
    /// Parses arguments (without the program name).
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> anyhow::Result<Parsed> {
        let mut config = PathBuf::from(DEFAULT_CONFIG_PATH);
        let mut iter = args.into_iter();
        while let Some(arg) = iter.next() {
            match arg.to_str() {
                Some("-c" | "--config") => match iter.next() {
                    Some(path) => config = PathBuf::from(path),
                    None => bail!("--config needs a path\n\n{USAGE}"),
                },
                Some("-h" | "--help") => return Ok(Parsed::Exit(USAGE.into())),
                Some("-V" | "--version") => {
                    return Ok(Parsed::Exit(format!(
                        "arilboxd {}",
                        env!("CARGO_PKG_VERSION")
                    )));
                }
                _ => bail!("unexpected argument {arg:?}\n\n{USAGE}"),
            }
        }
        Ok(Parsed::Run(Args { config }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> anyhow::Result<Parsed> {
        Args::parse(args.iter().map(OsString::from))
    }

    #[test]
    fn defaults_to_etc_config() {
        assert_eq!(
            parse(&[]).unwrap(),
            Parsed::Run(Args {
                config: PathBuf::from("/etc/arilbox/config.toml")
            })
        );
    }

    #[test]
    fn config_flag() {
        for flag in ["-c", "--config"] {
            assert_eq!(
                parse(&[flag, "x.toml"]).unwrap(),
                Parsed::Run(Args {
                    config: PathBuf::from("x.toml")
                })
            );
        }
        assert!(parse(&["--config"]).is_err());
    }

    #[test]
    fn help_and_version_exit() {
        assert!(matches!(parse(&["--help"]).unwrap(), Parsed::Exit(t) if t.contains("Usage")));
        assert!(matches!(parse(&["-V"]).unwrap(), Parsed::Exit(t) if t.starts_with("arilboxd ")));
    }

    #[test]
    fn rejects_unknown() {
        assert!(parse(&["--bogus"]).is_err());
    }
}
