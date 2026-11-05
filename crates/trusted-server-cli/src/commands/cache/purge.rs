//! Calling a deployed service's cache-purge endpoint.

use serde::Deserialize;

use crate::admin_client::{AdminClient, AdminRequest, AdminResponse, UNAUTHORIZED_HINT};
use crate::commands::cache::PurgeArgs;
use crate::error::{CliResult, cli_error};

const PURGE_PATH: &str = "/_ts/admin/cache/purge";

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
    let client = AdminClient::new(&args.service, &args.username)?;
    let AdminResponse {
        status,
        body: response_body,
    } = client.send(AdminRequest::PostJson {
        path: PURGE_PATH,
        body,
    })?;

    if status.is_success() {
        let acknowledgment: PurgeAcknowledgment = serde_json::from_str(&response_body)
            .map_err(|error| format!("invalid purge acknowledgment: {error}"))?;
        let expected_scope = if args.all { "all" } else { "url" };
        if !acknowledgment.purged || acknowledgment.scope != expected_scope {
            return cli_error(format!(
                "service did not acknowledge a successful {expected_scope} purge"
            ));
        }
        if !args.all
            && acknowledgment
                .surrogate_key
                .as_deref()
                .is_none_or(|key| key.trim().is_empty())
        {
            return cli_error("URL purge acknowledgment is missing its surrogate key");
        }
        writeln!(out, "{response_body}")
            .map_err(|error| format!("failed to write the purge result: {error}"))?;
        return Ok(());
    }

    // Named individually, because each one sends the operator somewhere different and a
    // purge is usually run mid-incident.
    let hint = match status.as_u16() {
        401 => UNAUTHORIZED_HINT,
        404 => " — this service may predate the purge endpoint",
        501 => " — the template cache is Fastly-backed; this adapter cannot purge",
        _ => "",
    };
    cli_error(format!(
        "purge failed: {status}{hint}\n{}",
        response_body.trim()
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
}
