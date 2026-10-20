//! Running the `fastly` CLI for the commands that administer a deployed
//! service.
//!
//! Those commands go through the hosting platform's own tool, with the
//! operator's own token. The token is read from `FASTLY_API_TOKEN` and reaches
//! the `fastly` process in its environment, never in its arguments, which
//! every other process on the machine can read.

use std::io::Write as _;
use std::process::{Command, Stdio};

/// The environment variable the Fastly API token is read from.
pub(crate) const TOKEN_VARIABLE: &str = "FASTLY_API_TOKEN";

/// Runs one invocation of the `fastly` CLI.
pub(crate) trait FastlyRunner: Send + Sync {
    /// Runs `fastly` with `args`, writing `stdin` to it when given, and
    /// returns its standard output.
    ///
    /// # Errors
    ///
    /// A message when the process cannot be run or exits unsuccessfully.
    fn run(&self, args: &[String], stdin: Option<&str>) -> Result<String, String>;
}

/// Runs the `fastly` binary on the `PATH`, with the token in its environment.
pub(crate) struct FastlyProcess {
    token: String,
}

impl FastlyProcess {
    /// Takes the token from [`TOKEN_VARIABLE`].
    ///
    /// # Errors
    ///
    /// A message naming the variable when it is unset or blank.
    pub(crate) fn from_environment() -> Result<Self, String> {
        Self::with_token(std::env::var(TOKEN_VARIABLE).ok())
    }

    fn with_token(token: Option<String>) -> Result<Self, String> {
        token
            .filter(|token| !token.trim().is_empty())
            .map(|token| Self { token })
            .ok_or_else(|| format!("set {TOKEN_VARIABLE} to a Fastly API token"))
    }
}

impl FastlyRunner for FastlyProcess {
    fn run(&self, args: &[String], stdin: Option<&str>) -> Result<String, String> {
        let mut child = Command::new("fastly")
            .args(args)
            .env(TOKEN_VARIABLE, &self.token)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("could not run `fastly`: {error}"))?;
        let written = match stdin {
            Some(input) => match child.stdin.take() {
                Some(mut pipe) => pipe
                    .write_all(input.as_bytes())
                    .map_err(|error| error.to_string()),
                None => Err("its standard input was not open".to_owned()),
            },
            None => Ok(()),
        };
        let output = child
            .wait_with_output()
            .map_err(|error| format!("`fastly` did not finish: {error}"))?;
        // What `fastly` said comes first. A process that stopped before it
        // read its input also fails the write, and its own message says why.
        if !output.status.success() {
            return Err(format!(
                "`fastly {}` failed: {}",
                command_words(args),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        written.map_err(|error| format!("could not write to `fastly`: {error}"))?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// The words of an invocation that name its command, which are those before
/// its first flag.
fn command_words(args: &[String]) -> String {
    args.iter()
        .map(String::as_str)
        .take_while(|arg| !arg.starts_with('-'))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The arguments of one `fastly` invocation, owned.
pub(crate) fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_or_blank_token_is_refused_by_name() {
        for token in [None, Some(String::new()), Some("  ".to_owned())] {
            let Err(error) = FastlyProcess::with_token(token.clone()) else {
                panic!("should refuse the token {token:?}");
            };
            assert!(
                error.contains(TOKEN_VARIABLE),
                "should name the variable to set: {error}"
            );
        }
    }

    #[test]
    fn a_token_is_kept_for_the_process_environment() {
        let process =
            FastlyProcess::with_token(Some("example-token".to_owned())).expect("should accept");

        assert_eq!(process.token, "example-token");
    }

    #[test]
    fn a_command_is_named_by_the_words_before_its_first_flag() {
        assert_eq!(
            command_words(&args(&[
                "secret-store-entry",
                "create",
                "--store-id=example",
                "--name=ts-key",
                "--stdin",
            ])),
            "secret-store-entry create"
        );
        assert_eq!(
            command_words(&args(&["service", "purge", "--key=ts-template"])),
            "service purge"
        );
    }
}
