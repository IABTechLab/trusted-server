use std::io::Write;
use std::process::{Command, Stdio};

use error_stack::Report;
use serde_json::{Value, json};
use tempfile::NamedTempFile;

use super::config::{AwsTarget, Deployment, invalid};
use super::{PbsError, Result};

/// An AWS call carries data separately from arguments; payloads never enter shell history or argv.
pub(super) trait Aws {
    /// Invoke one AWS API with explicit identity and region.
    ///
    /// # Errors
    /// Returns sanitized process/API failures; implementations must not expose raw stderr/payloads.
    fn call(
        &self,
        target: &AwsTarget,
        region: &str,
        service: &'static str,
        operation: &'static str,
        input: &Value,
    ) -> Result<Value>;
}

pub(super) struct AwsCli;

/// Provider output is withheld, so a rejected write cannot be distinguished from a lost response.
pub(super) const WRITE_NOT_CONFIRMED: &str = "write not confirmed; outcome may be uncertain; retain the request token and reuse it only for the original identical payload; use a new token for separately intended changed values";

impl Aws for AwsCli {
    fn call(
        &self,
        target: &AwsTarget,
        region: &str,
        service: &'static str,
        operation: &'static str,
        input: &Value,
    ) -> Result<Value> {
        if operation == "put-secret-value" {
            ensure_history_disabled(target)?;
        }
        // tempfile creates an owner-only file on Unix. No command uses a shell or logs this payload.
        let mut payload = NamedTempFile::new()
            .map_err(|_| Report::new(PbsError::Io("cannot create private AWS request file")))?;
        serde_json::to_writer(payload.as_file_mut(), input)
            .map_err(|_| Report::new(PbsError::Io("cannot encode AWS request")))?;
        payload
            .flush()
            .map_err(|_| Report::new(PbsError::Io("cannot flush AWS request")))?;
        let output = Command::new("aws")
            .args([
                "--profile",
                &target.profile,
                "--region",
                region,
                "--output",
                "json",
                "--no-cli-pager",
                "--no-cli-auto-prompt",
                "--cli-connect-timeout",
                "10",
                "--cli-read-timeout",
                "20",
                service,
                operation,
                "--cli-input-json",
            ])
            .arg(format!("file://{}", payload.path().display()))
            .env("AWS_CLI_AUTO_PROMPT", "off")
            .env("AWS_PAGER", "")
            .env("AWS_MAX_ATTEMPTS", "2")
            .env("AWS_IGNORE_CONFIGURED_ENDPOINT_URLS", "true")
            .stdin(Stdio::null())
            .output()
            .map_err(|_| Report::new(PbsError::Aws("could not execute AWS CLI v2")))?;
        // Closing removes the temporary payload, including on every earlier error path via Drop.
        drop(payload);
        if !output.status.success() {
            let message = if operation == "put-secret-value" {
                WRITE_NOT_CONFIRMED
            } else {
                operation
            };
            return Err(Report::new(PbsError::Aws(message)));
        }
        serde_json::from_slice(&output.stdout).map_err(|_| {
            let message = if operation == "put-secret-value" {
                WRITE_NOT_CONFIRMED
            } else {
                "invalid JSON response"
            };
            Report::new(PbsError::Aws(message))
        })
    }
}

/// Refuse credential writes when AWS CLI history could retain the request payload.
///
/// # Errors
/// Rejects enabled/unknown history settings and failures to inspect configuration.
fn ensure_history_disabled(target: &AwsTarget) -> Result<()> {
    for key in ["cli_history", "default.cli_history"] {
        let output = Command::new("aws")
            .args([
                "--profile",
                &target.profile,
                "--no-cli-pager",
                "--no-cli-auto-prompt",
                "configure",
                "get",
                key,
            ])
            .env("AWS_PAGER", "")
            .env("AWS_CLI_AUTO_PROMPT", "off")
            .stdin(Stdio::null())
            .output()
            .map_err(|_| Report::new(PbsError::Aws("cannot verify CLI history is disabled")))?;
        let value = String::from_utf8_lossy(&output.stdout);
        if !matches!(output.status.code(), Some(0 | 1)) || !matches!(value.trim(), "" | "disabled")
        {
            return Err(Report::new(PbsError::Aws(
                "disable AWS CLI history before writing credentials",
            )));
        }
    }
    Ok(())
}

/// Verify account identity before any deployment resource lookup or mutation.
///
/// # Errors
/// Rejects undeclared regions, failed identity reads, and mismatched accounts.
pub(super) fn verify_identity(deployment: &Deployment, region: &str, aws: &dyn Aws) -> Result<()> {
    if !deployment.regions.contains_key(region) {
        return Err(invalid(
            "select a region declared in the deployment descriptor",
        ));
    }
    let identity = aws.call(
        &deployment.aws,
        region,
        "sts",
        "get-caller-identity",
        &json!({}),
    )?;
    if identity.get("Account").and_then(Value::as_str) != Some(deployment.aws.account_id.as_str()) {
        return Err(Report::new(PbsError::AccountMismatch));
    }
    Ok(())
}

#[cfg(test)]
pub(super) mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use super::*;

    pub(crate) struct FakeAws {
        pub replies: RefCell<VecDeque<Value>>,
        pub calls: RefCell<Vec<(String, String, Value)>>,
    }

    impl FakeAws {
        pub(crate) fn new(replies: Vec<Value>) -> Self {
            Self {
                replies: RefCell::new(replies.into()),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl Aws for FakeAws {
        fn call(
            &self,
            _target: &AwsTarget,
            region: &str,
            _service: &'static str,
            operation: &'static str,
            input: &Value,
        ) -> Result<Value> {
            self.calls
                .borrow_mut()
                .push((region.to_owned(), operation.to_owned(), input.clone()));
            self.replies
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| Report::new(PbsError::Aws("simulated failure")))
        }
    }
}
