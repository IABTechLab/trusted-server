//! `ts preparation` — operator control of the publisher's preparation
//! worker: warm targets, retire an identity, read status.
//!
//! Every command goes through the worker's own authenticated HTTP routes,
//! never through a second open of its store, with the operator scope's
//! secret from [`OPERATOR_SECRET_VARIABLE`].

use std::io::Write;
use std::time::Duration;

use clap::Subcommand;
use serde_json::{Value, json};

/// The environment variable holding the operator scope's secret.
pub(crate) const OPERATOR_SECRET_VARIABLE: &str = "TS_PREPARATION_OPERATOR_SECRET";

/// The header the worker authenticates with.
const TOKEN_HEADER: &str = "x-encypher-preparation-token";

/// Subcommands under `ts preparation`.
#[derive(Debug, Subcommand)]
pub(crate) enum PreparationCommand {
    /// Queue configured targets for preparation now, as a publication event.
    Warm(WarmArgs),
    /// Retire one prepared identity: its edge marker is published until the
    /// delivery instance acknowledges it.
    Retire(RetireArgs),
    /// Print the worker's queue, artifact, retirement and readiness state.
    Status(WorkerArgs),
}

/// The worker's address.
#[derive(Debug, clap::Args)]
pub(crate) struct WorkerArgs {
    /// The worker's origin, such as `http://127.0.0.1:8112`.
    #[arg(long, value_name = "URL")]
    pub(crate) worker_url: String,
}

/// Arguments for `ts preparation warm`.
#[derive(Debug, clap::Args)]
pub(crate) struct WarmArgs {
    #[command(flatten)]
    pub(crate) worker: WorkerArgs,
    /// A target path, such as `/news/harbor-vote`; repeat for several.
    #[arg(long = "target", value_name = "PATH", required = true)]
    pub(crate) targets: Vec<String>,
    /// The admitted variant query of every target, such as `edition=uk`.
    #[arg(long, default_value = "")]
    pub(crate) variant: String,
    /// Also register the targets for periodic reconciliation.
    #[arg(long)]
    pub(crate) register: bool,
    /// Clear the targets' damped outcomes: an explicit operator retry.
    #[arg(long)]
    pub(crate) retry: bool,
}

/// Arguments for `ts preparation retire`.
#[derive(Debug, clap::Args)]
pub(crate) struct RetireArgs {
    #[command(flatten)]
    pub(crate) worker: WorkerArgs,
    /// The identity retired, 64 lower case hex characters.
    #[arg(long, value_name = "IDENTITY")]
    pub(crate) identity: String,
}

/// The method, path and body one command sends.
fn request(
    command: &PreparationCommand,
) -> (&WorkerArgs, reqwest::Method, &'static str, Option<Value>) {
    match command {
        PreparationCommand::Warm(args) => (
            &args.worker,
            reqwest::Method::POST,
            "/v1/preparation/warm",
            Some(json!({
                "targets": args
                    .targets
                    .iter()
                    .map(|path| json!({"path": path, "variant": args.variant}))
                    .collect::<Vec<_>>(),
                "register": args.register,
                "retry": args.retry,
            })),
        ),
        PreparationCommand::Retire(args) => (
            &args.worker,
            reqwest::Method::POST,
            "/v1/preparation/retire",
            Some(json!({"identity": args.identity})),
        ),
        PreparationCommand::Status(args) => {
            (args, reqwest::Method::GET, "/v1/preparation/status", None)
        }
    }
}

/// Runs a `ts preparation` subcommand, printing the worker's answer.
///
/// # Errors
///
/// A message where the secret is unset, the worker cannot be reached or it
/// refuses the command.
pub(crate) fn run(command: &PreparationCommand, out: &mut impl Write) -> Result<(), String> {
    let secret = std::env::var(OPERATOR_SECRET_VARIABLE)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{OPERATOR_SECRET_VARIABLE} is not set"))?;
    run_with_secret(command, &secret, out)
}

fn run_with_secret(
    command: &PreparationCommand,
    secret: &str,
    out: &mut impl Write,
) -> Result<(), String> {
    let (worker, method, path, body) = request(command);
    let origin =
        reqwest::Url::parse(&worker.worker_url).map_err(|_| "worker URL is invalid".to_owned())?;
    let secure = origin.scheme() == "https"
        || (origin.scheme() == "http"
            && matches!(origin.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")));
    if !secure
        || origin.host_str().is_none()
        || !origin.username().is_empty()
        || origin.password().is_some()
        || !matches!(origin.path(), "" | "/")
        || origin.query().is_some()
        || origin.fragment().is_some()
    {
        return Err(
            "worker URL must be an HTTPS origin (HTTP is allowed only on loopback)".to_owned(),
        );
    }
    let url = format!("{}{path}", worker.worker_url.trim_end_matches('/'));
    crate::tls::install_crypto_provider();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let (status, answer) = runtime.block_on(async {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|error| error.to_string())?;
        let mut builder = client.request(method, &url).header(TOKEN_HEADER, secret);
        if let Some(body) = body {
            builder = builder.json(&body);
        }
        let mut response = builder
            .send()
            .await
            .map_err(|_| format!("the worker at {} did not answer", worker.worker_url))?;
        let status = response.status();
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "worker response read failed".to_owned())?
        {
            if body.len().saturating_add(chunk.len()) > 8 * 1024 * 1024 {
                return Err("worker response exceeds 8 MiB".to_owned());
            }
            body.extend_from_slice(&chunk);
        }
        let text =
            String::from_utf8(body).map_err(|_| "worker response is not UTF-8".to_owned())?;
        Ok::<_, String>((status, text))
    })?;
    writeln!(out, "{answer}").map_err(|error| error.to_string())?;
    if status.is_success() {
        let value: Value =
            serde_json::from_str(&answer).map_err(|_| "worker response is not JSON".to_owned())?;
        match command {
            PreparationCommand::Retire(args) => {
                if value.get("identity").and_then(Value::as_str) != Some(args.identity.as_str())
                    || value.get("epoch").and_then(Value::as_u64).is_none()
                    || !matches!(
                        value.get("marker").and_then(Value::as_str),
                        Some("pending" | "acknowledged")
                    )
                {
                    return Err("worker retirement acknowledgement is malformed".to_owned());
                }
            }
            PreparationCommand::Warm(args) => {
                if value.get("protocol").and_then(Value::as_str) != Some("encypher-preparation/1") {
                    return Err("worker preparation protocol does not match".to_owned());
                }
                let targets = value
                    .get("targets")
                    .and_then(Value::as_array)
                    .filter(|targets| targets.len() == args.targets.len())
                    .ok_or_else(|| "worker omitted target outcomes".to_owned())?;
                let mut refused = 0;
                for target in targets {
                    if target.get("outcome").and_then(Value::as_str) == Some("not_admitted") {
                        refused += 1;
                        continue;
                    }
                    match target.pointer("/admission/outcome").and_then(Value::as_str) {
                        Some("queued" | "coalesced" | "damped") => {}
                        Some("queue_full") => refused += 1,
                        _ => return Err("worker returned an unknown admission outcome".to_owned()),
                    }
                }
                if refused > 0 {
                    return Err(format!(
                        "the worker refused {refused} target(s); see outcomes above"
                    ));
                }
            }
            PreparationCommand::Status(_) => {
                if value.get("protocol").and_then(Value::as_str) != Some("encypher-preparation/1")
                    || value.get("ready").and_then(Value::as_bool).is_none()
                    || !value.get("counts").is_some_and(Value::is_object)
                {
                    return Err("worker status response is malformed or incompatible".to_owned());
                }
            }
        }
        Ok(())
    } else {
        Err(format!("the worker refused the command: {status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insecure_or_credentialed_worker_origins_refuse_before_network() {
        for url in [
            "http://worker.example.com",
            "https://user:password@worker.example.com",
            "https://worker.example.com/path",
            "https://worker.example.com?token=secret",
        ] {
            assert!(
                run_with_secret(
                    &PreparationCommand::Status(WorkerArgs {
                        worker_url: url.to_owned()
                    }),
                    "operator",
                    &mut Vec::new()
                )
                .is_err()
            );
        }
    }
}
