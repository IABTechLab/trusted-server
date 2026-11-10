//! Tests for `ts keys rotate` and `ts keys deactivate` against a local fixture standing in
//! for a deployed service.
//!
//! Run with `./scripts/test-cli.sh`.

mod support_origin;

use support_origin::{FixtureResponse, FixtureServer};
use trusted_server_cli::commands::keys::{
    Confirm, DeactivateArgs, KeysCommand, RotateArgs, ServiceArgs, run,
};

/// Serializes the environment across this binary's tests; see `tests/cache_purge.rs`.
static ENVIRONMENT: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A scripted [`Confirm`] that records whether it was asked.
struct ScriptedConfirm {
    interactive: bool,
    answer: bool,
    asked: bool,
}

impl ScriptedConfirm {
    fn new(interactive: bool, answer: bool) -> Self {
        Self {
            interactive,
            answer,
            asked: false,
        }
    }
}

impl Confirm for ScriptedConfirm {
    fn is_interactive(&self) -> bool {
        self.interactive
    }

    fn confirm(&mut self, _question: &str) -> Result<bool, String> {
        self.asked = true;
        Ok(self.answer)
    }
}

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

fn service(server: &FixtureServer) -> ServiceArgs {
    ServiceArgs {
        service: server.url(""),
        username: "admin".to_owned(),
    }
}

fn rotate(server: &FixtureServer, kid: Option<&str>) -> KeysCommand {
    KeysCommand::Rotate(RotateArgs {
        service: service(server),
        kid: kid.map(str::to_owned),
    })
}

fn deactivate(server: &FixtureServer, delete: bool, yes: bool) -> KeysCommand {
    KeysCommand::Deactivate(DeactivateArgs {
        service: service(server),
        kid: "ts-2024-01-01".to_owned(),
        delete,
        yes,
    })
}

fn run_with(command: KeysCommand, confirm: &mut ScriptedConfirm) -> (Result<(), String>, String) {
    let mut out = Vec::new();
    let outcome = with_password(Some("admin-pass"), || run(command, confirm, &mut out));
    (
        outcome,
        String::from_utf8(out).expect("should write UTF-8 output"),
    )
}

fn rotate_success() -> String {
    serde_json::json!({
        "success": true,
        "message": "Key rotated successfully",
        "new_kid": "ts-2024-02-01",
        "previous_kid": "ts-2024-01-01",
        "active_kids": ["ts-2024-01-01", "ts-2024-02-01"],
        "jwk": {}
    })
    .to_string()
}

fn deactivate_success(deleted: bool) -> String {
    serde_json::json!({
        "success": true,
        "message": "Key deactivated successfully",
        "deactivated_kid": "ts-2024-01-01",
        "deleted": deleted,
        "remaining_active_kids": ["ts-2024-02-01"]
    })
    .to_string()
}

#[test]
fn rotate_posts_json_with_credentials_and_prints_the_kids() {
    let server = FixtureServer::start(|request| {
        assert_eq!(request.method, "POST", "should POST");
        assert_eq!(request.path, "/_ts/admin/keys/rotate", "should hit rotate");
        assert_eq!(
            request.header("authorization"),
            Some("Basic YWRtaW46YWRtaW4tcGFzcw=="),
            "should send basic auth for admin and the password from the environment"
        );
        assert_eq!(
            request.header("content-type"),
            Some("application/json"),
            "should send JSON"
        );
        let body: serde_json::Value =
            serde_json::from_slice(&request.body).expect("should parse body");
        assert_eq!(body, serde_json::json!({"kid": "ts-2024-02-01"}));
        FixtureResponse::html(rotate_success())
    });

    let (outcome, rendered) = run_with(
        rotate(&server, Some("ts-2024-02-01")),
        &mut ScriptedConfirm::new(false, false),
    );

    outcome.expect("should exit zero on success");
    assert!(
        rendered.contains("new kid: ts-2024-02-01"),
        "got: {rendered}"
    );
    assert!(
        rendered.contains("previous kid: ts-2024-01-01"),
        "got: {rendered}"
    );
    assert!(
        rendered.contains("active kids: ts-2024-01-01, ts-2024-02-01"),
        "got: {rendered}"
    );
}

#[test]
fn rotate_without_a_kid_sends_an_empty_object() {
    let server = FixtureServer::start(|request| {
        let body: serde_json::Value =
            serde_json::from_slice(&request.body).expect("should parse body");
        assert_eq!(body, serde_json::json!({}), "should omit the kid");
        FixtureResponse::html(rotate_success())
    });

    let (outcome, _) = run_with(
        rotate(&server, None),
        &mut ScriptedConfirm::new(false, false),
    );

    outcome.expect("should succeed");
}

#[test]
fn deactivate_without_delete_sends_no_prompt_and_prints_the_result() {
    let server = FixtureServer::start(|request| {
        assert_eq!(request.path, "/_ts/admin/keys/deactivate");
        let body: serde_json::Value =
            serde_json::from_slice(&request.body).expect("should parse body");
        assert_eq!(
            body,
            serde_json::json!({"kid": "ts-2024-01-01", "delete": false})
        );
        FixtureResponse::html(deactivate_success(false))
    });
    let mut confirm = ScriptedConfirm::new(false, false);

    let (outcome, rendered) = run_with(deactivate(&server, false, false), &mut confirm);

    outcome.expect("should succeed without confirmation");
    assert!(!confirm.asked, "should not prompt for a plain deactivation");
    assert!(
        rendered.contains("deactivated kid: ts-2024-01-01"),
        "got: {rendered}"
    );
    assert!(rendered.contains("deleted: no"), "got: {rendered}");
    assert!(
        rendered.contains("remaining active kids: ts-2024-02-01"),
        "got: {rendered}"
    );
}

#[test]
fn a_non_interactive_delete_without_yes_is_refused_before_sending() {
    let server = FixtureServer::start(|_request| FixtureResponse::html(deactivate_success(true)));

    let (outcome, _) = run_with(
        deactivate(&server, true, false),
        &mut ScriptedConfirm::new(false, true),
    );

    let error = outcome.expect_err("should refuse an unconfirmed delete");
    assert!(error.contains("--yes"), "should name the opt-in: {error}");
    assert_eq!(server.request_count(), 0, "should send nothing");
}

#[test]
fn a_declined_interactive_delete_sends_nothing() {
    let server = FixtureServer::start(|_request| FixtureResponse::html(deactivate_success(true)));
    let mut confirm = ScriptedConfirm::new(true, false);

    let (outcome, _) = run_with(deactivate(&server, true, false), &mut confirm);

    assert!(outcome.is_err(), "should exit non-zero when declined");
    assert!(confirm.asked, "should prompt before deleting");
    assert_eq!(server.request_count(), 0, "should send nothing");
}

#[test]
fn a_confirmed_or_pre_approved_delete_sends_delete_true() {
    for (interactive, yes) in [(true, false), (false, true)] {
        let server = FixtureServer::start(|request| {
            let body: serde_json::Value =
                serde_json::from_slice(&request.body).expect("should parse body");
            assert_eq!(
                body,
                serde_json::json!({"kid": "ts-2024-01-01", "delete": true})
            );
            FixtureResponse::html(deactivate_success(true))
        });
        let mut confirm = ScriptedConfirm::new(interactive, true);

        let (outcome, rendered) = run_with(deactivate(&server, true, yes), &mut confirm);

        outcome.expect("should delete once confirmed");
        assert_eq!(confirm.asked, !yes, "should prompt only without --yes");
        assert!(rendered.contains("deleted: yes"), "got: {rendered}");
        assert_eq!(server.request_count(), 1);
    }
}

#[test]
fn minimal_2xx_failure_body_surfaces_the_error_field() {
    let server = FixtureServer::start(|_request| {
        FixtureResponse::html(
            serde_json::json!({
                "success": false,
                "error": "cannot deactivate the current signing key"
            })
            .to_string(),
        )
    });

    let (outcome, rendered) = run_with(
        deactivate(&server, false, false),
        &mut ScriptedConfirm::new(false, false),
    );

    let error = outcome.expect_err("should exit non-zero on a refusal");
    assert!(
        error.contains("cannot deactivate the current signing key"),
        "should show the server's error, not a missing-field parse error: {error}"
    );
    assert!(rendered.is_empty(), "should print no success output");
}

#[test]
fn server_refusals_exit_non_zero_with_the_error_field() {
    for (status, error_text) in [
        (400u16, "kid must start with a lowercase ASCII letter"),
        (500, "cannot deactivate the last active key"),
        (200, "cannot deactivate the current signing key"),
    ] {
        let server = FixtureServer::start(move |_request| {
            FixtureResponse::html(
                serde_json::json!({
                    "success": false,
                    "message": "Key deactivation failed",
                    "deactivated_kid": "ts-2024-01-01",
                    "deleted": false,
                    "remaining_active_kids": [],
                    "error": error_text
                })
                .to_string(),
            )
            .with_status(status)
        });

        let (outcome, rendered) = run_with(
            deactivate(&server, false, false),
            &mut ScriptedConfirm::new(false, false),
        );

        let error = outcome.expect_err("should exit non-zero on a refusal");
        assert!(
            error.contains(error_text),
            "should show the server's error for {status}: {error}"
        );
        assert!(rendered.is_empty(), "should print no success output");
    }
}

#[test]
fn unauthorized_matches_the_cache_purge_guidance() {
    let server = FixtureServer::start(|_request| FixtureResponse::html("").with_status(401));

    let (outcome, _) = run_with(
        rotate(&server, None),
        &mut ScriptedConfirm::new(false, false),
    );

    let error = outcome.expect_err("should fail on 401");
    assert!(
        error.contains("TRUSTED_SERVER_ADMIN_PASSWORD"),
        "should point at the credential: {error}"
    );
}

#[test]
fn a_missing_password_fails_before_the_service_is_touched() {
    let server = FixtureServer::start(|_request| FixtureResponse::html(rotate_success()));
    let mut out = Vec::new();

    let outcome = with_password(None, || {
        run(
            rotate(&server, None),
            &mut ScriptedConfirm::new(false, false),
            &mut out,
        )
    });

    assert!(outcome.is_err(), "should require the password");
    assert_eq!(server.request_count(), 0, "should send nothing");
}

#[test]
fn plaintext_remote_services_are_rejected() {
    let mut out = Vec::new();
    let error = with_password(Some("admin-pass"), || {
        run(
            KeysCommand::Rotate(RotateArgs {
                service: ServiceArgs {
                    service: "http://edge.example.com".to_owned(),
                    username: "admin".to_owned(),
                },
                kid: None,
            }),
            &mut ScriptedConfirm::new(false, false),
            &mut out,
        )
    })
    .expect_err("should refuse insecure remote transport");
    assert!(error.contains("HTTPS"), "should explain: {error}");
}

#[test]
fn redirects_are_not_followed() {
    let server = FixtureServer::start(|_request| {
        FixtureResponse::html("")
            .with_status(302)
            .with_header("location", "/login")
    });

    let (outcome, _) = run_with(
        rotate(&server, None),
        &mut ScriptedConfirm::new(false, false),
    );

    assert!(outcome.is_err(), "should refuse a redirected rotation");
    assert_eq!(server.request_count(), 1, "should not follow the redirect");
}

#[test]
fn a_delete_settles_local_refusals_before_prompting() {
    let server = FixtureServer::start(|_request| FixtureResponse::html(deactivate_success(true)));
    let mut missing_password = ScriptedConfirm::new(true, true);
    let mut out = Vec::new();

    let outcome = with_password(None, || {
        run(
            deactivate(&server, true, false),
            &mut missing_password,
            &mut out,
        )
    });

    let error = outcome.expect_err("should require the password");
    assert!(
        error.contains("TRUSTED_SERVER_ADMIN_PASSWORD"),
        "should name the variable: {error}"
    );
    assert!(
        !missing_password.asked,
        "should refuse before asking to confirm a missing password"
    );

    let mut insecure_service = ScriptedConfirm::new(true, true);
    let error = with_password(Some("admin-pass"), || {
        run(
            KeysCommand::Deactivate(DeactivateArgs {
                service: ServiceArgs {
                    service: "http://edge.example.com".to_owned(),
                    username: "admin".to_owned(),
                },
                kid: "ts-2024-01-01".to_owned(),
                delete: true,
                yes: false,
            }),
            &mut insecure_service,
            &mut out,
        )
    })
    .expect_err("should refuse insecure remote transport");
    assert!(error.contains("HTTPS"), "should explain: {error}");
    assert!(
        !insecure_service.asked,
        "should refuse before asking to confirm an insecure service"
    );
    assert_eq!(server.request_count(), 0, "should send nothing");
}

#[test]
fn an_empty_remaining_list_reads_as_unknown() {
    let server = FixtureServer::start(|_request| {
        FixtureResponse::html(
            serde_json::json!({
                "success": true,
                "message": "Key deactivated successfully",
                "deactivated_kid": "ts-2024-01-01",
                "deleted": false,
                "remaining_active_kids": []
            })
            .to_string(),
        )
    });

    let (outcome, rendered) = run_with(
        deactivate(&server, false, false),
        &mut ScriptedConfirm::new(false, false),
    );

    outcome.expect("should succeed");
    assert!(
        rendered.contains("remaining active kids: (unknown: the service could not list them)"),
        "should not claim that no keys remain: {rendered}"
    );
}
