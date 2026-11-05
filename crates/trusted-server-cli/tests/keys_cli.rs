//! Tests for `ts keys list` and `ts keys verify` against a local fixture standing in for
//! a deployed service.
//!
//! Run with `./scripts/test-cli.sh`.

mod support_origin;

use serde_json::json;
use support_origin::{FixtureResponse, FixtureServer};
use trusted_server_cli::RunOutcome;
use trusted_server_cli::commands::keys::{KeysCommand, ListArgs, VerifyArgs, run};

fn discovery_body() -> String {
    json!({
        "version": "1.0",
        "jwks": { "keys": [
            { "kty": "OKP", "crv": "Ed25519", "x": "example-x", "kid": "ts-2024-01-15", "alg": "EdDSA" },
            { "kty": "OKP", "crv": "Ed25519", "x": "example-y", "kid": "ts-2024-02-15", "alg": "EdDSA" },
        ]}
    })
    .to_string()
}

fn json_response(body: String) -> FixtureResponse {
    FixtureResponse::html(body).with_header("content-type", "application/json")
}

fn list_args(server: &FixtureServer, json: bool) -> ListArgs {
    ListArgs {
        service: server.url(""),
        json,
    }
}

fn verify_args(server: &FixtureServer) -> VerifyArgs {
    VerifyArgs {
        service: server.url(""),
        kid: "ts-2024-01-15".to_owned(),
        payload: "example-payload".to_owned(),
        signature: "c2lnbmF0dXJl".to_owned(),
    }
}

#[test]
fn list_prints_each_active_kid_with_type_and_curve() {
    let server = FixtureServer::start(|request| {
        assert_eq!(request.method, "GET", "should fetch the discovery document");
        assert_eq!(request.path, "/.well-known/trusted-server.json");
        assert!(
            request.header("authorization").is_none(),
            "should never send credentials"
        );
        json_response(discovery_body())
    });

    let mut out = Vec::new();
    let outcome =
        run(KeysCommand::List(list_args(&server, false)), &mut out).expect("should list the keys");

    assert_eq!(outcome, RunOutcome::Success, "should exit zero");
    let rendered = String::from_utf8(out).expect("output should be UTF-8");
    assert_eq!(
        rendered, "ts-2024-01-15\tOKP\tEd25519\nts-2024-02-15\tOKP\tEd25519\n",
        "should print one line per active key"
    );
}

#[test]
fn list_json_prints_the_discovery_document_unchanged() {
    let body = discovery_body();
    let server = {
        let body = body.clone();
        FixtureServer::start(move |_request| json_response(body.clone()))
    };

    let mut out = Vec::new();
    run(KeysCommand::List(list_args(&server, true)), &mut out).expect("should list the keys");

    let rendered = String::from_utf8(out).expect("output should be UTF-8");
    assert_eq!(
        rendered.trim_end_matches('\n'),
        body,
        "should print the service's document byte for byte"
    );
}

#[test]
fn list_reports_a_failed_fetch_as_an_error() {
    let server =
        FixtureServer::start(|_request| FixtureResponse::html("not found").with_status(404));

    let mut out = Vec::new();
    let error = run(KeysCommand::List(list_args(&server, false)), &mut out)
        .expect_err("should fail on a 404");

    assert!(error.contains("404"), "should name the status: {error}");
    assert!(out.is_empty(), "should print no keys");
}

#[test]
fn verify_sends_the_fields_and_succeeds_when_verified() {
    let server = FixtureServer::start(|request| {
        assert_eq!(request.method, "POST", "should post to the verify route");
        assert_eq!(request.path, "/verify-signature");
        assert_eq!(request.header("content-type"), Some("application/json"));
        assert!(
            request.header("authorization").is_none(),
            "should never send credentials"
        );
        let body: serde_json::Value =
            serde_json::from_slice(&request.body).expect("should send JSON");
        assert_eq!(
            body,
            json!({
                "payload": "example-payload",
                "signature": "c2lnbmF0dXJl",
                "kid": "ts-2024-01-15",
            }),
            "should send the payload, signature, and kid"
        );
        json_response(
            json!({
                "verified": true,
                "kid": "ts-2024-01-15",
                "message": "Signature verified successfully",
            })
            .to_string(),
        )
    });

    let mut out = Vec::new();
    let outcome =
        run(KeysCommand::Verify(verify_args(&server)), &mut out).expect("should get an answer");

    assert_eq!(outcome, RunOutcome::Success, "should exit zero");
    let rendered = String::from_utf8(out).expect("output should be UTF-8");
    assert!(rendered.starts_with("verified:"), "got: {rendered}");
}

#[test]
fn verify_exits_non_zero_when_the_signature_does_not_verify() {
    let server = FixtureServer::start(|_request| {
        json_response(
            json!({
                "verified": false,
                "kid": "ts-2024-01-15",
                "message": "Signature verification failed",
                "error": "Invalid signature",
            })
            .to_string(),
        )
    });

    let mut out = Vec::new();
    let outcome =
        run(KeysCommand::Verify(verify_args(&server)), &mut out).expect("should get an answer");

    assert_eq!(outcome, RunOutcome::AssertionFailed, "should exit non-zero");
    assert_ne!(outcome.exit_code(), 0, "should give scripts a failing code");
    let rendered = String::from_utf8(out).expect("output should be UTF-8");
    assert!(rendered.starts_with("not verified:"), "got: {rendered}");
}

#[test]
fn plaintext_remote_services_are_rejected() {
    for service in ["http://edge.example.com", "ftp://127.0.0.1"] {
        let mut out = Vec::new();
        let error = run(
            KeysCommand::List(ListArgs {
                service: service.to_owned(),
                json: false,
            }),
            &mut out,
        )
        .expect_err("should refuse insecure remote transport");
        assert!(
            error.contains("HTTPS"),
            "should explain the transport requirement: {error}"
        );
    }
}
