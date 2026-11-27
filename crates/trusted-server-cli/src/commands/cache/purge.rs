//! Calling a deployed service's cache-purge endpoint.

use std::time::Duration;

use serde::Deserialize;

use crate::ad_templates::output::escape_terminal_text;
use crate::commands::cache::{ADMIN_PASSWORD_ENVIRONMENT_VARIABLE, PurgeArgs};
use crate::error::{CliResult, cli_error};

const PURGE_PATH: &str = "/_ts/admin/cache/purge";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The purge request body, as the endpoint's parser expects it.
///
/// `scope: "all"` must carry no `url` field: the endpoint refuses that combination, on the
/// grounds that an operator who meant one page and mistyped the scope should not be handed
/// a silent full flush.
fn request_body(args: &PurgeArgs) -> CliResult<String> {
    match (args.all, args.page.as_deref()) {
        (true, _) => Ok(r#"{"scope":"all"}"#.to_owned()),
        (false, Some(page)) => {
            if !reqwest::Url::parse(page).is_ok_and(|parsed| {
                matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some()
            }) {
                return cli_error("--page must be an absolute http(s) URL with a host");
            }
            Ok(serde_json::json!({ "scope": "url", "url": page }).to_string())
        }
        (false, None) => cli_error("specify --all or --page <url>"),
    }
}

/// Join the service base URL and the purge path without doubling or dropping a slash.
fn purge_endpoint(service: &str) -> CliResult<String> {
    let mut url =
        reqwest::Url::parse(service).map_err(|_| "--service must be an absolute HTTPS URL")?;
    crate::url_guard::require_credential_safe_transport(&url, "--service")?;
    if url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.path().trim_matches('/').is_empty()
    {
        return cli_error(
            "--service must be a base HTTPS URL without credentials, a path, query, or fragment",
        );
    }
    url.set_path(PURGE_PATH);
    Ok(url.to_string())
}

/// The acknowledgment returned by the purge endpoint.
#[derive(Deserialize)]
struct PurgeAcknowledgment {
    purged: bool,
    scope: String,
    surrogate_key: Option<String>,
}

/// Execute `ts cache purge`.
///
/// # Errors
///
/// Returns an error when no scope is given, the admin password is missing from the
/// environment, the service cannot be reached, or the service answers with a non-success
/// status, redirects, or does not acknowledge the requested purge scope.
pub fn run_purge(args: &PurgeArgs, out: &mut impl std::io::Write) -> CliResult<()> {
    let body = request_body(args)?;
    let endpoint = purge_endpoint(&args.service)?;

    let password = std::env::var(ADMIN_PASSWORD_ENVIRONMENT_VARIABLE).map_err(|_| {
        format!(
            "set {ADMIN_PASSWORD_ENVIRONMENT_VARIABLE} to the service's admin password \
             (it is read from the environment, never a flag, so it does not reach `ps` \
             output or shell history)"
        )
    })?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to start the HTTP runtime: {error}"))?;

    // Without this the first HTTPS request panics with "No provider set".
    crate::tls::install_crypto_provider();

    let (status, response_body) = runtime.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| format!("failed to build the HTTP client: {error}"))?;
        let response = client
            .post(&endpoint)
            .basic_auth(&args.username, Some(&password))
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|error| format!("could not reach {endpoint}: {error}"))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| format!("could not read the purge acknowledgment: {error}"))?;
        Ok::<_, String>((status, text))
    })?;

    report_response(args.all, status, &response_body, out)
}

/// Validate the purge endpoint's answer and report it to the operator.
///
/// Service-supplied text passes through [`escape_terminal_text`] on both paths, so a
/// compromised or misconfigured service cannot rewrite the terminal with control sequences.
fn report_response(
    all: bool,
    status: reqwest::StatusCode,
    response_body: &str,
    out: &mut impl std::io::Write,
) -> CliResult<()> {
    if status.is_success() {
        let acknowledgment: PurgeAcknowledgment = serde_json::from_str(response_body)
            .map_err(|error| format!("invalid purge acknowledgment: {error}"))?;
        let expected_scope = if all { "all" } else { "url" };
        if !acknowledgment.purged || acknowledgment.scope != expected_scope {
            return cli_error(format!(
                "service did not acknowledge a successful {expected_scope} purge"
            ));
        }
        if !all
            && acknowledgment
                .surrogate_key
                .as_deref()
                .is_none_or(|key| key.trim().is_empty())
        {
            return cli_error("URL purge acknowledgment is missing its surrogate key");
        }
        writeln!(out, "{}", escape_terminal_text(response_body))
            .map_err(|error| format!("failed to write the purge result: {error}"))?;
        return Ok(());
    }

    // Named individually, because each one sends the operator somewhere different and a
    // purge is usually run mid-incident.
    let hint = match status.as_u16() {
        401 => " — check the admin username and $TRUSTED_SERVER_ADMIN_PASSWORD",
        404 => " — this service may predate the purge endpoint",
        501 => " — the template cache is Fastly-backed; this adapter cannot purge",
        _ => "",
    };
    cli_error(format!(
        "purge failed: {status}{hint}\n{}",
        escape_terminal_text(response_body.trim())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(all: bool, page: Option<&str>) -> PurgeArgs {
        PurgeArgs {
            service: "https://edge.example.com".to_owned(),
            all,
            page: page.map(str::to_owned),
            username: "admin".to_owned(),
        }
    }

    #[test]
    fn success_output_escapes_terminal_control_sequences() {
        // JSON forbids raw control characters inside strings, so the only ones a parseable
        // acknowledgment can carry are whitespace between tokens, such as a carriage return.
        let body = "{\"purged\":true,\r\"scope\":\"all\"}";
        let mut out = Vec::new();

        report_response(true, reqwest::StatusCode::OK, body, &mut out)
            .expect("should accept a valid acknowledgment");

        let rendered = String::from_utf8(out).expect("should write UTF-8");
        assert!(
            !rendered.contains('\r'),
            "should not pass raw control characters through: {rendered:?}"
        );
        assert!(
            rendered.contains("\\u{000D}"),
            "should render the carriage return visibly: {rendered:?}"
        );
    }

    #[test]
    fn failure_output_escapes_terminal_control_sequences() {
        let mut out = Vec::new();

        let error = report_response(
            true,
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            "boom\u{1b}[1A\rpurge succeeded",
            &mut out,
        )
        .expect_err("should fail on a server error");

        assert!(
            !error.contains('\u{1b}') && !error.contains('\r'),
            "should not pass raw control characters through: {error:?}"
        );
        assert!(
            error.contains("\\u{001B}") && error.contains("\\u{000D}"),
            "should render control characters visibly: {error:?}"
        );
        assert!(out.is_empty(), "should write nothing on failure");
    }

    #[test]
    fn invalid_page_urls_are_rejected_locally() {
        for page in [
            "/article",
            "example.com/article",
            "ftp://example.com/article",
            "",
        ] {
            assert!(
                request_body(&args(false, Some(page))).is_err(),
                "should reject {page:?}"
            );
        }
        assert!(
            request_body(&args(false, Some("http://example.com/article"))).is_ok(),
            "should allow HTTP reader URLs"
        );
    }

    #[test]
    fn purge_all_sends_no_url_field() {
        // The endpoint refuses scope "all" carrying a url, so emitting one would make
        // every --all run fail.
        assert_eq!(
            request_body(&args(true, None)).expect("should build"),
            r#"{"scope":"all"}"#
        );
    }

    #[test]
    fn purge_page_sends_the_url_verbatim() {
        // Canonicalization is the endpoint's job. Normalizing here too would give the two
        // sides separate rules to drift apart.
        let body = request_body(&args(false, Some("https://example.com/a?b=2&a=1")))
            .expect("should build");
        assert_eq!(
            body,
            r#"{"scope":"url","url":"https://example.com/a?b=2&a=1"}"#
        );
    }

    #[test]
    fn a_url_with_json_punctuation_is_escaped_rather_than_breaking_the_body() {
        let body = request_body(&args(false, Some(r#"https://example.com/"; drop"#)))
            .expect("should build");
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("should stay valid");
        assert_eq!(parsed["url"], r#"https://example.com/"; drop"#);
    }

    #[test]
    fn neither_scope_is_an_error_rather_than_a_default() {
        // Defaulting to --all would make a bare `ts cache purge` flush production.
        assert!(request_body(&args(false, None)).is_err());
    }

    #[test]
    fn the_endpoint_url_survives_a_trailing_slash() {
        for service in [
            "https://edge.example.com",
            "https://edge.example.com/",
            "https://edge.example.com///",
        ] {
            assert_eq!(
                purge_endpoint(service).expect("should accept a secure base URL"),
                "https://edge.example.com/_ts/admin/cache/purge",
                "{service} must resolve to one well-formed endpoint"
            );
        }
    }

    #[test]
    fn only_loopback_services_may_use_plaintext_http() {
        for service in [
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
            "http://localhost:8080",
        ] {
            assert!(
                purge_endpoint(service).is_ok(),
                "should permit loopback development: {service}"
            );
        }
        for service in [
            "http://192.0.2.1",
            "http://[2001:db8::1]",
            "http://localhost.example.com",
        ] {
            assert!(
                purge_endpoint(service).is_err(),
                "should refuse remote plaintext transport: {service}"
            );
        }
    }

    #[test]
    fn service_urls_cannot_redirect_the_purge_path_or_embed_credentials() {
        for service in [
            "not a URL",
            "https://example.com/path",
            "https://example.com?query=1",
            "https://example.com#fragment",
            "https://user:example-password@example.com",
        ] {
            assert!(
                purge_endpoint(service).is_err(),
                "should require an unambiguous service base URL"
            );
        }
    }
}
