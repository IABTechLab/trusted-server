//! Experimental PBS operator commands, independent of Trusted Server deployment.

mod aws;
mod config;
mod inspect;
mod secrets;
mod status;

use std::fs::File;
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use derive_more::Display;
use error_stack::Report;
use serde_json::Value;

use aws::AwsCli;
use config::Deployment;

/// Arguments for the experimental `ts prebid server` namespace.
#[derive(Debug, Args)]
pub(crate) struct PbsArgs {
    /// Emit a nonsecret JSON report instead of a human summary.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: PbsCommand,
}

#[derive(Debug, Subcommand)]
enum PbsCommand {
    /// Inspect local Trusted Server configuration without modifying it or contacting AWS.
    Inspect {
        /// Explicit source file; omitted fields/defaults and remote overrides remain unresolved.
        #[arg(long, default_value = "trusted-server.toml")]
        config: PathBuf,
    },
    /// Validate declared PBS inputs and regional YAML merges locally, without AWS access.
    Check(TargetArgs),
    /// Read EC2 infrastructure status, not PBS health or the installed release.
    Status(TargetArgs),
    /// Manage values for existing, explicitly declared Secrets Manager secrets.
    #[command(subcommand)]
    Secrets(SecretCommand),
}

#[derive(Debug, Args)]
struct TargetArgs {
    /// Deployment descriptor; paths inside it are relative to this file.
    #[arg(long)]
    deployment: PathBuf,
}

#[derive(Debug, Subcommand)]
enum SecretCommand {
    /// Write a complete JSON credential payload; does not deploy or rotate partner credentials.
    Set(secrets::SetArgs),
}

/// Sanitized errors: source parser output and AWS stderr can contain credential values.
#[derive(Debug, Display)]
pub(crate) enum PbsError {
    #[display("invalid PBS input: {_0}")]
    Input(&'static str),
    #[display("PBS I/O failed: {_0}")]
    Io(&'static str),
    #[display("PBS I/O failed: cannot open input file {path:?}")]
    InputFile { path: PathBuf },
    #[display("AWS operation failed: {_0}; provider output withheld")]
    Aws(&'static str),
    #[display("AWS account does not match the deployment descriptor; no further calls made")]
    AccountMismatch,
    #[display("operation was not confirmed; no secret value was written")]
    NotConfirmed,
}

impl std::error::Error for PbsError {}

pub(super) type Result<T> = std::result::Result<T, Report<PbsError>>;

pub(super) struct Output {
    summary: String,
    details: Vec<String>,
    data: Value,
    failure: Option<PbsError>,
}

impl Output {
    /// Write only explicitly constructed nonsecret report fields.
    ///
    /// # Errors
    /// Returns a sanitized error if output cannot be written or serialized.
    fn write(&self, json: bool, out: &mut dyn Write) -> Result<()> {
        if json {
            serde_json::to_writer_pretty(&mut *out, &self.data)
                .map_err(|_| Report::new(PbsError::Io("cannot write JSON report")))?;
            writeln!(out).map_err(|_| Report::new(PbsError::Io("cannot write report")))?;
        } else {
            writeln!(out, "{}", self.summary)
                .map_err(|_| Report::new(PbsError::Io("cannot write report")))?;
            for detail in &self.details {
                writeln!(out, "  {detail}")
                    .map_err(|_| Report::new(PbsError::Io("cannot write report")))?;
            }
        }
        Ok(())
    }
}

/// Execute PBS commands. Only explicitly selected cloud commands construct an AWS client.
///
/// # Errors
/// Returns sanitized validation, I/O, identity, confirmation, or AWS errors.
pub(crate) fn run(args: &PbsArgs) -> Result<()> {
    let output = match &args.command {
        PbsCommand::Inspect { config } => inspect::inspect(config)?,
        PbsCommand::Check(target) => config::check(&Deployment::load(&target.deployment)?)?,
        PbsCommand::Status(target) => {
            let deployment = Deployment::load(&target.deployment)?;
            Terminal.notice(&format!(
                "Read EC2 status: environment={}; account={}; profile={}; regions={}",
                deployment.environment,
                deployment.aws.account_id,
                deployment.aws.profile,
                deployment
                    .regions
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ))?;
            status::status(&deployment, &AwsCli)?
        }
        PbsCommand::Secrets(SecretCommand::Set(set)) => {
            let deployment = Deployment::load(&set.deployment)?;
            secrets::set(set, &deployment, &AwsCli, &mut Terminal)?
        }
    };
    output.write(args.json, &mut io::stdout().lock())?;
    match output.failure {
        Some(error) => Err(Report::new(error)),
        None => Ok(()),
    }
}

/// Read bounded local input without including file contents in error reports.
///
/// # Errors
/// Returns an I/O error or rejects oversized/non-UTF-8 input.
pub(super) fn read_text(path: &Path, limit: usize) -> Result<String> {
    let file = File::open(path).map_err(|_| {
        Report::new(PbsError::InputFile {
            path: path.to_path_buf(),
        })
    })?;
    read_bounded(file, limit)
}

/// Read bounded input, including piped credential JSON.
///
/// # Errors
/// Rejects oversized/non-UTF-8 input and read failures without disclosing contents.
pub(super) fn read_bounded(input: impl Read, limit: usize) -> Result<String> {
    let mut text = String::new();
    input
        .take(limit as u64 + 1)
        .read_to_string(&mut text)
        .map_err(|_| Report::new(PbsError::Io("cannot read UTF-8 input")))?;
    if text.len() > limit {
        return Err(Report::new(PbsError::Input("input exceeds size limit")));
    }
    Ok(text)
}

/// Keep report identifiers bounded and free of terminal control sequences.
pub(super) fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn is_confirmation(input: &str) -> bool {
    input.len() <= 16 && input.trim() == "yes"
}

/// Operator interaction is injectable so tests cannot accidentally read a terminal.
pub(super) trait Interaction {
    /// Read a complete secret JSON object without echoing it.
    ///
    /// # Errors
    /// Returns an error when an interactive terminal is unavailable.
    fn secret(&mut self) -> Result<String>;
    /// Request approval of a nonsecret target description.
    ///
    /// # Errors
    /// Returns an error on unavailable terminal or failed I/O.
    fn confirm(&mut self, target: &str) -> Result<bool>;
    /// Display nonsecret target and retry information separately from JSON stdout.
    ///
    /// # Errors
    /// Returns an error if the notice cannot be written.
    fn notice(&mut self, message: &str) -> Result<()>;
}

struct Terminal;

impl Interaction for Terminal {
    fn secret(&mut self) -> Result<String> {
        if !io::stdin().is_terminal() {
            return Err(Report::new(PbsError::Input(
                "use --file or --stdin for noninteractive secret input",
            )));
        }
        rpassword::prompt_password("Secret JSON (hidden): ")
            .map_err(|_| Report::new(PbsError::Io("cannot read hidden secret input")))
    }

    fn confirm(&mut self, target: &str) -> Result<bool> {
        if !io::stdin().is_terminal() {
            return Err(Report::new(PbsError::Input(
                "automation requires --yes and --request-token",
            )));
        }
        self.notice(target)?;
        self.notice("Write this secret version? Type yes to confirm:")?;
        let line = io::stdin()
            .lock()
            .lines()
            .next()
            .transpose()
            .map_err(|_| Report::new(PbsError::Io("cannot read confirmation")))?
            .unwrap_or_default();
        Ok(is_confirmation(&line))
    }

    fn notice(&mut self, message: &str) -> Result<()> {
        writeln!(io::stderr().lock(), "{message}")
            .map_err(|_| Report::new(PbsError::Io("cannot write operator notice")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_bounded_preserves_size_utf8_and_error_redaction_guards() {
        assert_eq!(
            read_bounded(&b"abcd"[..], 4).expect("should accept limit"),
            "abcd"
        );
        for (input, limit) in [(&b"DUMMY_SECRET"[..], 4), (&b"\xffDUMMY_SECRET"[..], 64)] {
            let error = read_bounded(input, limit).expect_err("should reject invalid input");
            assert!(!format!("{error:?}").contains("DUMMY_SECRET"));
        }
        struct FailedRead;
        impl Read for FailedRead {
            fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("DUMMY_SECRET"))
            }
        }
        let error = read_bounded(FailedRead, 4).expect_err("should reject read failure");
        assert!(!format!("{error:?}").contains("DUMMY_SECRET"));
    }

    #[test]
    fn confirmation_accepts_only_bounded_yes() {
        assert!(is_confirmation("yes"));
        assert!(!is_confirmation("no"));
        assert!(!is_confirmation("yes-but-with-more-than-sixteen-bytes"));
    }
}
