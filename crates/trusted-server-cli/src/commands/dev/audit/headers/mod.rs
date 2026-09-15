//! `ts dev audit headers` — audits origin cache-header posture per content type.
//!
//! Fetches origin responses (explicit URLs, or discovered from the origin root),
//! classifies them by content type, evaluates each against the cacheability
//! rules, and renders a per-type pass/warn/fail report. The command exits 1 when
//! any content-type group fails, 2 when only warnings are present, and 0 when
//! all pass.

mod analyze;
mod fetch;
mod output;
mod rules;

use std::io::Write;

pub use fetch::AuditHeadersArgs;

use crate::error::CliResult;
use analyze::AuditReport;
use fetch::{OriginClient, ReqwestOriginClient, collect_responses};

/// Runs the header audit against a live origin, printing to stdout and returning
/// the process exit code (0 pass, 1 fail, 2 warn-only).
pub fn run(args: &AuditHeadersArgs) -> Result<i32, String> {
    let client = ReqwestOriginClient::new()?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let report = run_audit_headers(args, &client, &mut out)?;
    Ok(report.exit_code())
}

/// Fetches, analyzes, and renders the audit, returning the report.
///
/// Kept separate from [`run`] — with the [`OriginClient`] and output sink
/// injected — so the full pipeline is unit-testable without network I/O or
/// touching stdout.
fn run_audit_headers(
    args: &AuditHeadersArgs,
    client: &dyn OriginClient,
    out: &mut dyn Write,
) -> CliResult<AuditReport> {
    let (origin, responses) = collect_responses(args, client)?;
    let report = analyze::run_analysis(&origin, &responses);
    output::render(&report, args.json, out)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::PathBuf;

    use url::Url;

    use super::fetch::OriginResponse;
    use super::rules::ResponseHeaders;
    use super::*;

    struct FakeClient {
        responses: HashMap<String, OriginResponse>,
        fetched: RefCell<Vec<String>>,
    }

    impl FakeClient {
        fn new() -> Self {
            Self {
                responses: HashMap::new(),
                fetched: RefCell::new(Vec::new()),
            }
        }

        fn insert(&mut self, url: &str, headers: ResponseHeaders, body: &str) {
            self.responses.insert(
                url.to_owned(),
                OriginResponse {
                    headers,
                    body: body.to_owned(),
                },
            );
        }
    }

    impl OriginClient for FakeClient {
        fn fetch(&self, url: &Url) -> CliResult<OriginResponse> {
            self.fetched.borrow_mut().push(url.to_string());
            self.responses
                .get(url.as_str())
                .cloned()
                .ok_or_else(|| format!("no fake response for {url}"))
        }
    }

    fn args_for(origin: &str, json: bool) -> AuditHeadersArgs {
        AuditHeadersArgs {
            urls: Vec::new(),
            config: PathBuf::from("trusted-server.toml"),
            origin: Some(origin.to_owned()),
            json,
        }
    }

    #[test]
    fn end_to_end_discovery_produces_report() {
        let html = r#"<html><head>
            <script src="/app.js"></script>
          </head><body></body></html>"#;

        let mut client = FakeClient::new();
        client.insert(
            "https://origin.example/",
            ResponseHeaders {
                content_type: Some("text/html".to_owned()),
                cache_control: Some("no-store".to_owned()),
                ..ResponseHeaders::default()
            },
            html,
        );
        client.insert(
            "https://origin.example/app.js",
            ResponseHeaders {
                content_type: Some("application/javascript".to_owned()),
                cache_control: Some("public, max-age=31536000, immutable".to_owned()),
                surrogate_key: Some("js".to_owned()),
                etag: Some("\"abc\"".to_owned()),
                ..ResponseHeaders::default()
            },
            "",
        );
        client.insert(
            "https://origin.example/favicon.ico",
            ResponseHeaders {
                content_type: Some("image/x-icon".to_owned()),
                cache_control: Some("public, max-age=86400".to_owned()),
                surrogate_key: Some("icon".to_owned()),
                ..ResponseHeaders::default()
            },
            "",
        );

        let mut buffer = Vec::new();
        let report = run_audit_headers(
            &args_for("https://origin.example", false),
            &client,
            &mut buffer,
        )
        .expect("should run end to end");

        assert!(
            report.summary.total_groups >= 2,
            "should audit at least HTML and JS groups"
        );
        assert_eq!(report.exit_code(), 0, "clean posture should exit 0");
        let text = String::from_utf8(buffer).expect("should be utf-8");
        assert!(text.contains("Origin:"), "should render the report");
    }

    #[test]
    fn failing_group_sets_exit_code_one() {
        let mut client = FakeClient::new();
        client.insert(
            "https://origin.example/",
            ResponseHeaders {
                content_type: Some("text/html".to_owned()),
                cache_control: Some("public, max-age=3600".to_owned()),
                ..ResponseHeaders::default()
            },
            "<html></html>",
        );
        client.insert(
            "https://origin.example/favicon.ico",
            ResponseHeaders {
                content_type: Some("image/x-icon".to_owned()),
                cache_control: Some("public, max-age=86400".to_owned()),
                surrogate_key: Some("icon".to_owned()),
                ..ResponseHeaders::default()
            },
            "",
        );

        let mut buffer = Vec::new();
        let report = run_audit_headers(
            &args_for("https://origin.example", true),
            &client,
            &mut buffer,
        )
        .expect("should run");
        assert_eq!(
            report.exit_code(),
            1,
            "cacheable HTML should drive a failing exit code"
        );
    }
}
