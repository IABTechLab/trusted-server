//! Tests for `ts ec show` and `ts ec eids` against a local fixture standing in for a
//! deployed service.
//!
//! Run with `./scripts/test-cli.sh`.

// Test crates may buffer bodies with `Body::into_bytes`.
#![allow(clippy::disallowed_methods)]

mod support_origin;

use base64::Engine as _;
use edgezero_core::body::Body as EdgeBody;
use http::Request;
use support_origin::{FixtureRequest, FixtureResponse, FixtureServer};
use trusted_server_cli::commands::ec::{EcCommand, EidsArgs, ShowArgs, run};
use trusted_server_core::ec::admin::handle_admin_eids_lookup;
use trusted_server_core::ec::registry::PartnerRegistry;

/// Serializes the environment across this binary's tests; see `tests/cache_purge.rs`.
static ENVIRONMENT: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Set the admin password for one call and clear it afterwards.
fn with_password<T>(password: Option<&str>, body: impl FnOnce() -> T) -> T {
    let _guard = ENVIRONMENT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    // SAFETY: the guard above makes this the only thread touching the environment.
    unsafe {
        match password {
            Some(value) => std::env::set_var("TRUSTED_SERVER_ADMIN_PASSWORD", value),
            None => std::env::remove_var("TRUSTED_SERVER_ADMIN_PASSWORD"),
        }
    }
    let result = body();
    // SAFETY: as above; still holding the guard.
    unsafe {
        std::env::remove_var("TRUSTED_SERVER_ADMIN_PASSWORD");
    }
    result
}

/// The `user:password` pair carried by the request's basic-auth header.
fn basic_credentials(request: &FixtureRequest) -> Option<String> {
    let encoded = request.header("authorization")?.strip_prefix("Basic ")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    String::from_utf8(decoded).ok()
}

fn eids_body() -> String {
    serde_json::json!({
        "cookie_present": true,
        "eids": [],
        "sharedid_present": true,
        "partners_configured": 1,
        "ingest": {
            "matched": [{"source_domain": "configured.example", "uid": "shared-fictional"}],
            "unmatched": [{"source": "unknown.example", "reason": "no_partner"}]
        }
    })
    .to_string()
}

fn ec_id() -> String {
    format!("{}.abc123", "a".repeat(64))
}

fn show_args(service: String, ec_id: &str, json: bool) -> ShowArgs {
    ShowArgs {
        service,
        ec_id: ec_id.to_owned(),
        username: "admin".to_owned(),
        json,
    }
}

fn eids_args(service: String, ts_eids: Option<&str>, shared_id: Option<&str>) -> EidsArgs {
    EidsArgs {
        service,
        ts_eids: ts_eids.map(str::to_owned),
        shared_id: shared_id.map(str::to_owned),
        username: "admin".to_owned(),
        json: false,
    }
}

fn lookup_body() -> String {
    serde_json::json!({
        "ec_id": ec_id(),
        "store": "ec_store",
        "generation": 3,
        "tombstone": false,
        "entry": {"consent": {"tcf": "fictional-tcf-string"}},
        "metadata": {"ok": true},
        "auction": {
            "eids": [{"source": "configured.example", "uids": [{"id": "uid-live", "atype": 3}]}],
            "skipped": [{"source_domain": "unknown.example", "reason": "not_in_registry"}]
        }
    })
    .to_string()
}

fn run_to_string(command: EcCommand) -> (Result<(), String>, String) {
    let mut out = Vec::new();
    let outcome = with_password(Some("admin-pass"), || run(command, &mut out));
    (
        outcome,
        String::from_utf8(out).expect("output should be UTF-8"),
    )
}

#[test]
fn show_requests_the_lookup_route_with_credentials_and_summarizes_the_record() {
    let expected_path = format!("/_ts/admin/ec/{}", ec_id());
    let server = FixtureServer::start(move |request| {
        assert_eq!(request.method, "GET", "should use GET");
        assert_eq!(
            request.path, expected_path,
            "should request the explicit-ID route"
        );
        assert_eq!(
            basic_credentials(request).as_deref(),
            Some("admin:admin-pass"),
            "should send the default username and the environment's password"
        );
        assert!(
            request.header("cookie").is_none(),
            "should not send cookies"
        );
        FixtureResponse::html(lookup_body()).with_header("content-type", "application/json")
    });

    let (outcome, rendered) =
        run_to_string(EcCommand::Show(show_args(server.url(""), &ec_id(), false)));

    outcome.expect("a 200 lookup should exit zero");
    assert_eq!(server.request_count(), 1);
    assert!(rendered.contains("Tombstone:  no"), "got: {rendered}");
    assert!(
        rendered.contains("configured.example: uid-live (atype 3)"),
        "got: {rendered}"
    );
    assert!(
        rendered.contains("unknown.example: not_in_registry"),
        "got: {rendered}"
    );
}

#[test]
fn show_json_prints_the_full_response_unchanged() {
    let body = lookup_body();
    let served = body.clone();
    let server = FixtureServer::start(move |_request| FixtureResponse::html(served.clone()));

    let (outcome, rendered) =
        run_to_string(EcCommand::Show(show_args(server.url(""), &ec_id(), true)));

    outcome.expect("should succeed");
    assert_eq!(
        rendered,
        format!("{body}\n"),
        "--json must print the service's body byte for byte"
    );
}

#[test]
fn show_refuses_a_malformed_ec_id_before_sending_anything() {
    let server = FixtureServer::start(|_request| FixtureResponse::html(lookup_body()));
    for bad in [
        "not-an-id".to_owned(),
        format!("{}.abc123", "A".repeat(64)),
        format!("{}.abc12", "a".repeat(64)),
        format!("{}.abc123/../../keys", "a".repeat(64)),
    ] {
        let (outcome, rendered) =
            run_to_string(EcCommand::Show(show_args(server.url(""), &bad, false)));
        let error = outcome.expect_err("should refuse a malformed EC ID");
        assert!(error.contains("invalid EC ID"), "got: {error}");
        assert!(rendered.is_empty());
    }
    assert_eq!(
        server.request_count(),
        0,
        "a malformed EC ID must not reach the service"
    );
}

#[test]
fn show_explains_not_found_unsupported_and_unauthorized() {
    for (status, body, expected) in [
        (
            404u16,
            r#"{"error":"EC entry not found"}"#,
            "no record for this EC ID",
        ),
        (
            501,
            r#"{"error":"EC identity graph is not configured on this deployment"}"#,
            "only the Fastly adapter",
        ),
        (401, "Unauthorized", "TRUSTED_SERVER_ADMIN_PASSWORD"),
    ] {
        let server =
            FixtureServer::start(move |_request| FixtureResponse::html(body).with_status(status));

        let (outcome, rendered) =
            run_to_string(EcCommand::Show(show_args(server.url(""), &ec_id(), false)));

        let error = outcome.expect_err("a refused lookup must exit non-zero");
        assert!(
            error.contains(expected),
            "a {status} should explain itself, got: {error}"
        );
        assert!(rendered.is_empty(), "should print no record");
    }
}

#[test]
fn show_refuses_a_success_that_is_not_a_lookup_response() {
    let server = FixtureServer::start(|_request| FixtureResponse::html("<html>login</html>"));

    let (outcome, rendered) =
        run_to_string(EcCommand::Show(show_args(server.url(""), &ec_id(), true)));

    assert!(outcome.is_err(), "should refuse a non-lookup 200");
    assert!(rendered.is_empty(), "should not print it as the record");
}

#[test]
fn show_does_not_follow_redirects() {
    let server = FixtureServer::start(|_request| {
        FixtureResponse::html("")
            .with_status(302)
            .with_header("location", "/login")
    });

    let (outcome, _) = run_to_string(EcCommand::Show(show_args(server.url(""), &ec_id(), false)));

    assert!(outcome.is_err());
    assert_eq!(server.request_count(), 1, "should not follow the redirect");
}

#[test]
fn eids_sends_the_values_as_the_requests_cookies_and_lists_matches() {
    let server = FixtureServer::start(|request| {
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/_ts/admin/eids");
        assert_eq!(
            request.header("cookie"),
            Some("ts-eids=eyJmaWN0aW9uYWwiOnRydWV9; sharedId=shared-fictional"),
            "should send exactly the two cookies the operator gave"
        );
        assert_eq!(
            basic_credentials(request).as_deref(),
            Some("admin:admin-pass"),
            "should send the default username and the environment's password"
        );
        FixtureResponse::html(eids_body())
    });

    let (outcome, rendered) = run_to_string(EcCommand::Eids(eids_args(
        server.url(""),
        Some("eyJmaWN0aW9uYWwiOnRydWV9"),
        Some("shared-fictional"),
    )));

    outcome.expect("should succeed");
    assert!(
        rendered.contains("configured.example: shared-fictional"),
        "got: {rendered}"
    );
    assert!(
        rendered.contains("unknown.example: no_partner (no configured partner"),
        "should explain the reason, got: {rendered}"
    );
}

#[test]
fn a_non_default_username_is_sent_with_the_password() {
    let server = FixtureServer::start(|request| {
        assert_eq!(
            basic_credentials(request).as_deref(),
            Some("operator:admin-pass"),
            "should send --username"
        );
        FixtureResponse::html(lookup_body())
    });
    let mut args = show_args(server.url(""), &ec_id(), false);
    args.username = "operator".to_owned();

    let (outcome, _) = run_to_string(EcCommand::Show(args));

    outcome.expect("should succeed");
    assert_eq!(server.request_count(), 1);
}

#[test]
fn eids_json_prints_the_full_response_unchanged() {
    let body = eids_body();
    let served = body.clone();
    let server = FixtureServer::start(move |_request| FixtureResponse::html(served.clone()));
    let mut args = eids_args(server.url(""), None, Some("shared-fictional"));
    args.json = true;

    let (outcome, rendered) = run_to_string(EcCommand::Eids(args));

    outcome.expect("should succeed");
    assert_eq!(
        rendered,
        format!("{body}\n"),
        "--json must print the body unchanged"
    );
}

#[test]
fn eids_explains_a_refusal_and_prints_nothing() {
    for (status, body, expected) in [
        (401u16, "Unauthorized", "TRUSTED_SERVER_ADMIN_PASSWORD"),
        (
            404,
            r#"{"error":"admin diagnostic route not found"}"#,
            "predate the EIDs diagnostic endpoint",
        ),
        (500, "Internal Server Error", "Internal Server Error"),
    ] {
        let server =
            FixtureServer::start(move |_request| FixtureResponse::html(body).with_status(status));

        let (outcome, rendered) = run_to_string(EcCommand::Eids(eids_args(
            server.url(""),
            None,
            Some("shared-fictional"),
        )));

        let error = outcome.expect_err("a refused preview must exit non-zero");
        assert!(
            error.contains(expected),
            "a {status} should explain itself, got: {error}"
        );
        assert!(rendered.is_empty(), "should print no preview");
    }
}

#[test]
fn eids_requires_a_cookie_value_before_touching_the_service() {
    let server = FixtureServer::start(|_request| FixtureResponse::html("{}"));

    let (outcome, _) = run_to_string(EcCommand::Eids(eids_args(server.url(""), None, None)));

    assert!(outcome.is_err(), "should require --ts-eids or --shared-id");
    assert_eq!(server.request_count(), 0);
}

#[test]
fn both_commands_need_the_password_from_the_environment() {
    let server = FixtureServer::start(|_request| FixtureResponse::html(lookup_body()));
    let mut out = Vec::new();

    let show = with_password(None, || {
        run(
            EcCommand::Show(show_args(server.url(""), &ec_id(), false)),
            &mut out,
        )
    });
    let eids = with_password(None, || {
        run(
            EcCommand::Eids(eids_args(server.url(""), None, Some("shared-fictional"))),
            &mut out,
        )
    });

    for outcome in [show, eids] {
        let error = outcome.expect_err("should refuse without a password");
        assert!(
            error.contains("TRUSTED_SERVER_ADMIN_PASSWORD"),
            "got: {error}"
        );
    }
    assert_eq!(server.request_count(), 0);
}

#[test]
fn both_commands_refuse_plaintext_remote_services() {
    let show = run_to_string(EcCommand::Show(show_args(
        "http://edge.example.com".to_owned(),
        &ec_id(),
        false,
    )));
    let eids = run_to_string(EcCommand::Eids(eids_args(
        "http://edge.example.com".to_owned(),
        None,
        Some("shared-fictional"),
    )));

    for (outcome, rendered) in [show, eids] {
        let error = outcome.expect_err("should refuse plaintext remote transport");
        assert!(error.contains("HTTPS"), "got: {error}");
        assert!(rendered.is_empty());
    }
}

#[test]
fn eids_escapes_cookie_controls_through_the_real_admin_handler() {
    let cookie = base64::engine::general_purpose::STANDARD.encode(
        serde_json::json!([{
            "source": "example.com\u{1b}[2J\nsource",
            "uids": [{"id": "uid\u{1b}[H\nforged", "atype": 1}]
        }])
        .to_string(),
    );
    let server = FixtureServer::start(|request| {
        let request = Request::builder()
            .uri("https://edge.example.com/_ts/admin/eids")
            .header(
                "cookie",
                request.header("cookie").expect("should send cookies"),
            )
            .body(EdgeBody::empty())
            .expect("should build the admin request");
        let registry = PartnerRegistry::from_config(&[]).expect("should build an empty registry");
        let response = handle_admin_eids_lookup(&registry, &request)
            .expect("should decode the browser cookie");
        FixtureResponse::html(
            response
                .into_body()
                .into_bytes()
                .expect("should read the response"),
        )
    });

    let (outcome, rendered) = run_to_string(EcCommand::Eids(eids_args(
        server.url(""),
        Some(&cookie),
        None,
    )));

    outcome.expect("should inspect a valid encoded cookie");
    assert!(
        rendered.contains(r"example.com\u{1b}[2J\nsource: uid\u{1b}[H\nforged (atype 1)"),
        "should render decoded values as text, got: {rendered:?}"
    );
    assert!(
        !rendered.contains('\u{1b}'),
        "should not emit terminal escape bytes, got: {rendered:?}"
    );
}

#[test]
fn both_commands_escape_control_characters_in_refusal_messages() {
    for body in [
        serde_json::json!({"error": "refused\u{1b}[2J\nforged"}).to_string(),
        "refused\u{1b}[2J\nforged".to_owned(),
    ] {
        let server =
            FixtureServer::start(move |_| FixtureResponse::html(body.clone()).with_status(500));
        for command in [
            EcCommand::Show(show_args(server.url(""), &ec_id(), false)),
            EcCommand::Eids(eids_args(server.url(""), None, Some("shared-fictional"))),
        ] {
            let (outcome, rendered) = run_to_string(command);
            let error = outcome.expect_err("should report the refusal");
            assert!(
                error.contains(r"refused\u{1b}[2J\nforged"),
                "should escape server text before logging, got: {error:?}"
            );
            assert!(rendered.is_empty(), "should print no success report");
        }
    }
}

#[test]
fn both_json_modes_preserve_response_escaping_and_formatting() {
    for (is_show, mut body) in [
        (
            true,
            serde_json::from_str::<serde_json::Value>(&lookup_body()).expect("should parse lookup"),
        ),
        (
            false,
            serde_json::from_str::<serde_json::Value>(&eids_body()).expect("should parse preview"),
        ),
    ] {
        body["extra"] = serde_json::json!("raw\u{1b}[2J\n\tconsent");
        let body = format!(
            "{}\n\t",
            serde_json::to_string_pretty(&body).expect("should encode response")
        );
        let served = body.clone();
        let server = FixtureServer::start(move |_| FixtureResponse::html(served.clone()));
        let command = if is_show {
            EcCommand::Show(show_args(server.url(""), &ec_id(), true))
        } else {
            let mut args = eids_args(server.url(""), None, Some("shared-fictional"));
            args.json = true;
            EcCommand::Eids(args)
        };

        let (outcome, rendered) = run_to_string(command);

        outcome.expect("should preserve a valid JSON response");
        assert_eq!(
            rendered,
            format!("{body}\n"),
            "should preserve every response byte"
        );
    }
}
