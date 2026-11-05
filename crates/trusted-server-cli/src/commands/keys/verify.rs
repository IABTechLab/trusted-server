//! Asking a service whether a signature verifies, through `POST /verify-signature`.

use serde::Deserialize;

use crate::commands::keys::{KeysRequest, VerifyArgs, send};
use crate::error::{CliResult, cli_error};
use crate::run::RunOutcome;

const VERIFY_PATH: &str = "/verify-signature";

/// The verification answer returned by the service.
#[derive(Deserialize)]
struct VerifyResponse {
    verified: bool,
    kid: String,
    message: String,
    error: Option<String>,
}

fn request_body(args: &VerifyArgs) -> String {
    serde_json::json!({
        "payload": args.payload,
        "signature": args.signature,
        "kid": args.kid,
    })
    .to_string()
}

/// Turn the service's answer into the line to print and the outcome to exit with.
fn interpret(body: &str) -> CliResult<(String, RunOutcome)> {
    let response: VerifyResponse = serde_json::from_str(body)
        .map_err(|error| format!("invalid verification response: {error}"))?;
    if response.verified {
        return Ok((
            format!("verified: signature is valid for kid {}", response.kid),
            RunOutcome::Success,
        ));
    }
    let detail = response
        .error
        .map(|error| format!(" ({error})"))
        .unwrap_or_default();
    Ok((
        format!(
            "not verified: {} for kid {}{detail}",
            response.message, response.kid
        ),
        RunOutcome::AssertionFailed,
    ))
}

/// Execute `ts keys verify`.
///
/// Returns [`RunOutcome::AssertionFailed`] when the service reports that the signature
/// did not verify.
///
/// # Errors
///
/// Returns an error when the service URL is unsafe or malformed, the service cannot be
/// reached, or it answers with a non-success status or an unexpected body.
pub fn run_verify(args: &VerifyArgs, out: &mut impl std::io::Write) -> CliResult<RunOutcome> {
    let endpoint = crate::url_guard::service_endpoint(&args.service, VERIFY_PATH)?;
    let body = request_body(args);
    let (status, response_body) = send(&endpoint, &KeysRequest::PostJson(&body))?;
    if !status.is_success() {
        return cli_error(format!(
            "signature verification request failed: {status}\n{}",
            response_body.trim()
        ));
    }
    let (line, outcome) = interpret(&response_body)?;
    writeln!(out, "{line}")
        .map_err(|error| format!("failed to write the verification result: {error}"))?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_request_body_carries_every_field_escaped() {
        let args = VerifyArgs {
            service: "https://edge.example.com".to_owned(),
            kid: "ts-2024-01-15".to_owned(),
            payload: r#"{"id":"example"}"#.to_owned(),
            signature: "c2lnbmF0dXJl".to_owned(),
        };

        let parsed: serde_json::Value =
            serde_json::from_str(&request_body(&args)).expect("should be valid JSON");

        assert_eq!(
            parsed,
            json!({
                "payload": r#"{"id":"example"}"#,
                "signature": "c2lnbmF0dXJl",
                "kid": "ts-2024-01-15",
            }),
            "should send the payload verbatim"
        );
    }

    #[test]
    fn a_verified_answer_succeeds() {
        let body = json!({
            "verified": true,
            "kid": "ts-2024-01-15",
            "message": "Signature verified successfully",
        })
        .to_string();

        let (line, outcome) = interpret(&body).expect("should interpret");

        assert_eq!(outcome, RunOutcome::Success, "should exit zero");
        assert!(line.starts_with("verified:"), "got: {line}");
    }

    #[test]
    fn an_unverified_answer_fails_the_assertion() {
        let body = json!({
            "verified": false,
            "kid": "ts-2024-01-15",
            "message": "Signature verification failed",
            "error": "Invalid signature",
        })
        .to_string();

        let (line, outcome) = interpret(&body).expect("should interpret");

        assert_eq!(outcome, RunOutcome::AssertionFailed, "should exit non-zero");
        assert!(line.contains("Invalid signature"), "got: {line}");
    }

    #[test]
    fn an_unexpected_body_is_an_error() {
        assert!(interpret("not json").is_err(), "should reject non-JSON");
    }
}
