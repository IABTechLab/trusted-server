//! Asking a service whether a signature verifies, through `POST /verify-signature`.

use serde::Deserialize;

use crate::ad_templates::output::escape_terminal_text;
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
            format!(
                "verified: signature is valid for kid {}",
                escape_terminal_text(&response.kid)
            ),
            RunOutcome::Success,
        ));
    }
    // The service folds an unknown kid, a malformed signature, and an unreadable signing
    // store into this one sanitized value, so name the possibilities instead of echoing it.
    let detail = match response.error.as_deref() {
        Some("internal verification error") => " (the service could not check it: the kid \
             may not exist, the signature may not be 64 bytes of base64, or the service could \
             not read its keys)"
            .to_owned(),
        Some(error) => format!(" ({})", escape_terminal_text(error)),
        None => String::new(),
    };
    Ok((
        format!(
            "not verified: {} for kid {}{detail}",
            escape_terminal_text(&response.message),
            escape_terminal_text(&response.kid)
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
    fn the_sanitized_internal_error_names_its_possible_causes() {
        let body = json!({
            "verified": false,
            "kid": "ts-2099-01-01",
            "message": "Verification error",
            "error": "internal verification error",
        })
        .to_string();

        let (line, outcome) = interpret(&body).expect("should interpret");

        assert_eq!(outcome, RunOutcome::AssertionFailed, "should exit non-zero");
        assert!(
            line.contains("the kid may not exist"),
            "should name what the service could not check: {line}"
        );
    }

    #[test]
    fn service_controlled_fields_are_escaped() {
        let body = json!({
            "verified": false,
            "kid": "ts-2024-01-15\u{1b}[2K",
            "message": "Signature verification failed\u{1b}[1G",
            "error": "Invalid signature\u{1b}]0;title\u{7}",
        })
        .to_string();

        let (line, _) = interpret(&body).expect("should interpret");

        assert!(
            !line.contains('\u{1b}') && !line.contains('\u{7}'),
            "should not pass control characters to the terminal: {line:?}"
        );

        let verified = json!({
            "verified": true,
            "kid": "ts-2024-01-15\u{1b}[2K",
            "message": "Signature verified successfully",
        })
        .to_string();

        let (line, _) = interpret(&verified).expect("should interpret");

        assert!(
            !line.contains('\u{1b}'),
            "should not pass control characters to the terminal: {line:?}"
        );
    }

    #[test]
    fn an_unexpected_body_is_an_error() {
        assert!(interpret("not json").is_err(), "should reject non-JSON");
    }
}
