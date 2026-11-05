//! `ts keys` — request-signing key rotation through a deployed service's admin routes.
//!
//! These wrap `POST /_ts/admin/keys/rotate` and `POST /_ts/admin/keys/deactivate`. The
//! transport rules (service URL checks, admin username, password from
//! `TRUSTED_SERVER_ADMIN_PASSWORD`, 401 guidance) come from the admin client shared with
//! `ts cache purge`.

use std::io::{BufRead, IsTerminal as _, Write};

use clap::Subcommand;
use serde::Deserialize;

use crate::admin_client::{AdminResponse, UNAUTHORIZED_HINT, post_admin_json};
use crate::error::{CliResult, cli_error};

const ROTATE_PATH: &str = "/_ts/admin/keys/rotate";
const DEACTIVATE_PATH: &str = "/_ts/admin/keys/deactivate";

/// Subcommands under `ts keys`.
#[derive(Debug, Subcommand)]
pub enum KeysCommand {
    /// Generate a new request-signing key and make it the current key.
    Rotate(RotateArgs),
    /// Deactivate a request-signing key, optionally deleting it from storage.
    Deactivate(DeactivateArgs),
}

/// Arguments shared by every `ts keys` subcommand.
#[derive(Debug, clap::Args)]
pub struct ServiceArgs {
    /// HTTPS base URL of the service; HTTP is permitted only for loopback development.
    #[arg(long)]
    pub service: String,

    /// Admin username for the service's `^/_ts/admin` basic auth.
    #[arg(long, default_value = "admin")]
    pub username: String,
}

/// Arguments for `ts keys rotate`.
#[derive(Debug, clap::Args)]
pub struct RotateArgs {
    /// Target service and admin credentials.
    #[command(flatten)]
    pub service: ServiceArgs,

    /// Explicit key identifier for the new key; the service generates one when omitted.
    #[arg(long)]
    pub kid: Option<String>,
}

/// Arguments for `ts keys deactivate`.
#[derive(Debug, clap::Args)]
pub struct DeactivateArgs {
    /// Target service and admin credentials.
    #[command(flatten)]
    pub service: ServiceArgs,

    /// Key identifier to deactivate.
    #[arg(long)]
    pub kid: String,

    /// Delete the key from storage permanently instead of only deactivating it.
    #[arg(long)]
    pub delete: bool,

    /// Skip the `--delete` confirmation prompt; required for a non-interactive delete.
    #[arg(long, requires = "delete")]
    pub yes: bool,
}

/// Source of operator confirmation for destructive operations.
pub trait Confirm {
    /// Whether a person is present to answer a prompt.
    fn is_interactive(&self) -> bool;

    /// Ask `question` and return whether the operator agreed.
    ///
    /// # Errors
    ///
    /// Returns an error when the prompt cannot be written or the answer read.
    fn confirm(&mut self, question: &str) -> CliResult<bool>;
}

/// [`Confirm`] backed by the process's stdin and stderr.
pub struct TerminalConfirm;

impl Confirm for TerminalConfirm {
    fn is_interactive(&self) -> bool {
        std::io::stdin().is_terminal()
    }

    fn confirm(&mut self, question: &str) -> CliResult<bool> {
        let mut stderr = std::io::stderr();
        write!(stderr, "{question} [y/N] ")
            .and_then(|()| stderr.flush())
            .map_err(|error| format!("failed to write the confirmation prompt: {error}"))?;
        let mut answer = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut answer)
            .map_err(|error| format!("failed to read the confirmation answer: {error}"))?;
        Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "YES" | "Yes"))
    }
}

/// Run a `ts keys` subcommand.
///
/// # Errors
///
/// Returns an error when the admin password is absent from the environment, the service
/// URL is unsafe, a `--delete` is not confirmed, the service cannot be reached, or the
/// service refuses the operation.
pub fn run(
    command: KeysCommand,
    confirm: &mut impl Confirm,
    out: &mut impl Write,
) -> CliResult<()> {
    match command {
        KeysCommand::Rotate(args) => run_rotate(&args, out),
        KeysCommand::Deactivate(args) => run_deactivate(&args, confirm, out),
    }
}

/// The rotate request body, as the endpoint's parser expects it.
fn rotate_body(args: &RotateArgs) -> String {
    match args.kid.as_deref() {
        Some(kid) => serde_json::json!({ "kid": kid }).to_string(),
        None => "{}".to_owned(),
    }
}

/// The deactivate request body, as the endpoint's parser expects it.
fn deactivate_body(args: &DeactivateArgs) -> String {
    serde_json::json!({ "kid": args.kid, "delete": args.delete }).to_string()
}

#[derive(Deserialize)]
struct RotateResponse {
    success: bool,
    new_kid: String,
    previous_kid: Option<String>,
    active_kids: Vec<String>,
}

#[derive(Deserialize)]
struct DeactivateResponse {
    success: bool,
    deactivated_kid: String,
    deleted: bool,
    remaining_active_kids: Vec<String>,
}

/// The failure fields every key route returns.
#[derive(Deserialize)]
struct FailureResponse {
    error: Option<String>,
    message: Option<String>,
}

fn run_rotate(args: &RotateArgs, out: &mut impl Write) -> CliResult<()> {
    let response = post_admin_json(
        &args.service.service,
        ROTATE_PATH,
        &args.service.username,
        rotate_body(args),
    )?;
    let parsed: RotateResponse = parse_success(&response, "key rotation")?;
    writeln!(
        out,
        "new kid: {}\nprevious kid: {}\nactive kids: {}",
        parsed.new_kid,
        parsed.previous_kid.as_deref().unwrap_or("(none)"),
        join_kids(&parsed.active_kids),
    )
    .map_err(|error| format!("failed to write the rotation result: {error}"))
}

fn run_deactivate(
    args: &DeactivateArgs,
    confirm: &mut impl Confirm,
    out: &mut impl Write,
) -> CliResult<()> {
    if args.delete && !args.yes {
        if !confirm.is_interactive() {
            return cli_error(
                "--delete removes the key from storage permanently; pass --yes to confirm \
                 a non-interactive delete",
            );
        }
        let question = format!(
            "Permanently delete signing key `{}`? This cannot be undone.",
            args.kid
        );
        if !confirm.confirm(&question)? {
            return cli_error("key deletion cancelled; nothing was sent");
        }
    }

    let response = post_admin_json(
        &args.service.service,
        DEACTIVATE_PATH,
        &args.service.username,
        deactivate_body(args),
    )?;
    let operation = if args.delete {
        "key deletion"
    } else {
        "key deactivation"
    };
    let parsed: DeactivateResponse = parse_success(&response, operation)?;
    writeln!(
        out,
        "deactivated kid: {}\ndeleted: {}\nremaining active kids: {}",
        parsed.deactivated_kid,
        if parsed.deleted { "yes" } else { "no" },
        join_kids(&parsed.remaining_active_kids),
    )
    .map_err(|error| format!("failed to write the deactivation result: {error}"))
}

fn join_kids(kids: &[String]) -> String {
    if kids.is_empty() {
        "(none)".to_owned()
    } else {
        kids.join(", ")
    }
}

/// Parse a successful key-route response, or turn a failure into a readable error.
///
/// A failure is a non-2xx status, or a 2xx whose body says `success: false` or does not
/// parse. The server's `error` field is surfaced so its refusals (an invalid kid,
/// deactivating the current kid, deactivating the last active key) reach the operator.
fn parse_success<T>(response: &AdminResponse, operation: &str) -> CliResult<T>
where
    T: for<'de> Deserialize<'de> + HasSuccess,
{
    let status = response.status;
    if status.is_success() {
        match serde_json::from_str::<T>(&response.body) {
            Ok(parsed) if parsed.success() => return Ok(parsed),
            Ok(_) => {}
            Err(error) => {
                return cli_error(format!("{operation} returned an invalid response: {error}"));
            }
        }
    }

    let hint = match status.as_u16() {
        401 => UNAUTHORIZED_HINT,
        404 => " — this service may not expose the key admin routes",
        _ => "",
    };
    let detail = serde_json::from_str::<FailureResponse>(&response.body)
        .ok()
        .and_then(|failure| failure.error.or(failure.message))
        .unwrap_or_else(|| response.body.trim().to_owned());
    if detail.is_empty() {
        cli_error(format!("{operation} failed: {status}{hint}"))
    } else {
        cli_error(format!("{operation} failed: {status}{hint}\n{detail}"))
    }
}

/// Access to the `success` flag every key-route response carries.
trait HasSuccess {
    fn success(&self) -> bool;
}

impl HasSuccess for RotateResponse {
    fn success(&self) -> bool {
        self.success
    }
}

impl HasSuccess for DeactivateResponse {
    fn success(&self) -> bool {
        self.success
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> ServiceArgs {
        ServiceArgs {
            service: "https://edge.example.com".to_owned(),
            username: "admin".to_owned(),
        }
    }

    #[test]
    fn rotate_without_a_kid_sends_an_empty_object() {
        let body = rotate_body(&RotateArgs {
            service: service(),
            kid: None,
        });
        assert_eq!(body, "{}", "should let the service generate the kid");
    }

    #[test]
    fn rotate_with_a_kid_sends_it_escaped() {
        let body = rotate_body(&RotateArgs {
            service: service(),
            kid: Some(r#"kid-"a""#.to_owned()),
        });
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("should stay valid");
        assert_eq!(
            parsed,
            serde_json::json!({"kid": r#"kid-"a""#}),
            "should send the kid verbatim"
        );
    }

    #[test]
    fn deactivate_sends_kid_and_delete_flag() {
        for delete in [false, true] {
            let body = deactivate_body(&DeactivateArgs {
                service: service(),
                kid: "ts-2024-01-01".to_owned(),
                delete,
                yes: true,
            });
            let parsed: serde_json::Value =
                serde_json::from_str(&body).expect("should be valid JSON");
            assert_eq!(
                parsed,
                serde_json::json!({"kid": "ts-2024-01-01", "delete": delete}),
                "should send the delete flag explicitly"
            );
        }
    }

    #[test]
    fn failures_surface_the_servers_error_field() {
        let response = AdminResponse {
            status: reqwest::StatusCode::BAD_REQUEST,
            body: serde_json::json!({
                "success": false,
                "message": "Key deactivation failed",
                "error": "cannot deactivate the current signing key"
            })
            .to_string(),
        };
        let error = parse_success::<DeactivateResponse>(&response, "key deactivation")
            .err()
            .expect("should fail");
        assert!(
            error.contains("cannot deactivate the current signing key"),
            "should show the server's error: {error}"
        );
    }
}
