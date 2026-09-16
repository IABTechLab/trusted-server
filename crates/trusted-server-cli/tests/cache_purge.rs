//! Tests for `ts cache purge` against a local fixture standing in for a deployed service.
//!
//! Run with `./scripts/test-cli.sh`.

mod support_origin;

use support_origin::{FixtureResponse, FixtureServer};
use trusted_server_cli::commands::cache::{CacheCommand, PurgeArgs, run};

/// Serializes the environment across this binary's tests.
///
/// The CLI reads the password from the environment on purpose, so exercising that path
/// means writing to it — and the test harness runs these in parallel threads of one
/// process, where one test clearing the variable races another that just set it.
static ENVIRONMENT: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Set the admin password for one call and clear it afterwards.
fn with_password<T>(password: Option<&str>, body: impl FnOnce() -> T) -> T {
    // A panicking test poisons the lock; the data is `()`, so recovering it loses nothing
    // and keeps one failure from cascading into every other test in the file.
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

fn args(server: &FixtureServer, all: bool, page: Option<&str>) -> PurgeArgs {
    PurgeArgs {
        service: server.url(""),
        all,
        page: page.map(str::to_owned),
        username: "admin".to_owned(),
    }
}

#[test]
fn a_successful_purge_prints_the_services_answer_and_exits_zero() {
    let server = FixtureServer::start(|_request| {
        FixtureResponse::html(r#"{"purged":true,"scope":"all"}"#)
            .with_header("content-type", "application/json")
    });

    let mut out = Vec::new();
    let outcome = with_password(Some("admin-pass"), || {
        run(CacheCommand::Purge(args(&server, true, None)), &mut out)
    });

    assert!(outcome.is_ok(), "a 200 from the service must exit zero");
    let rendered = String::from_utf8(out).expect("output should be UTF-8");
    assert!(
        rendered.contains(r#""purged":true"#),
        "the service's own answer is what an operator needs, got: {rendered}"
    );
}

#[test]
fn the_request_carries_credentials_json_and_the_expected_body() {
    // Pins the wire format against the endpoint's guards: it requires POST, rejects any
    // Content-Type but application/json, and authenticates on ^/_ts/admin.
    let server = FixtureServer::start(|request| {
        FixtureResponse::html(format!(
            r#"{{"method":"{}","auth":{},"type":"{}","path":"{}"}}"#,
            request.method,
            request.header("authorization").is_some(),
            request.header("content-type").unwrap_or("none"),
            request.path
        ))
    });

    let mut out = Vec::new();
    with_password(Some("admin-pass"), || {
        run(CacheCommand::Purge(args(&server, true, None)), &mut out).expect("should succeed")
    });

    let rendered = String::from_utf8(out).expect("output should be UTF-8");
    assert!(rendered.contains(r#""method":"POST""#), "got: {rendered}");
    assert!(rendered.contains(r#""auth":true"#), "got: {rendered}");
    assert!(
        rendered.contains(r#""type":"application/json""#),
        "got: {rendered}"
    );
    assert!(
        rendered.contains(r#""path":"/_ts/admin/cache/purge""#),
        "got: {rendered}"
    );
}

#[test]
fn a_missing_password_fails_before_the_service_is_touched() {
    let server = FixtureServer::start(|_request| FixtureResponse::html("{}"));

    let mut out = Vec::new();
    let outcome = with_password(None, || {
        run(CacheCommand::Purge(args(&server, true, None)), &mut out)
    });

    assert!(outcome.is_err());
    assert_eq!(
        server.request_count(),
        0,
        "a purge must not be attempted without a credential"
    );
}

#[test]
fn a_rejected_purge_exits_non_zero_and_explains_the_status() {
    for (status, expected_hint) in [
        (401u16, "admin username"),
        (404, "predate the purge endpoint"),
        (501, "cannot purge"),
    ] {
        let server =
            FixtureServer::start(move |_request| FixtureResponse::html("{}").with_status(status));

        let mut out = Vec::new();
        let outcome = with_password(Some("admin-pass"), || {
            run(CacheCommand::Purge(args(&server, true, None)), &mut out)
        });

        let error = outcome.expect_err("a refused purge must not exit zero");
        let message = error.to_string();
        assert!(
            message.contains(expected_hint),
            "a {status} must say what to do next, got: {message}"
        );
    }
}

#[test]
fn a_page_purge_sends_the_url_the_operator_typed() {
    let server = FixtureServer::start(|request| {
        FixtureResponse::html(format!(r#"{{"received":{}}}"#, request.body.len()))
    });

    let mut out = Vec::new();
    with_password(Some("admin-pass"), || {
        run(
            CacheCommand::Purge(args(&server, false, Some("https://example.com/article"))),
            &mut out,
        )
        .expect("should succeed")
    });

    assert_eq!(server.request_count(), 1);
}
