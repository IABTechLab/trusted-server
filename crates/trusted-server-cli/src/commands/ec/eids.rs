//! `ts ec eids` — preview cookie EID ingestion through `GET /_ts/admin/eids`.

use serde::Deserialize;
use trusted_server_core::constants::{COOKIE_SHAREDID, COOKIE_TS_EIDS};

use crate::admin_client::{AdminClient, AdminRequest, UNAUTHORIZED_HINT};
use crate::commands::ec::show::{EidView, render_eids};
use crate::commands::ec::{EidsArgs, json_passthrough, unexpected_answer, write_report};
use crate::error::{CliResult, cli_error};

const ADMIN_EIDS_PATH: &str = "/_ts/admin/eids";

/// The fields of the EIDs response this command summarizes. Unknown fields are ignored.
#[derive(Debug, Deserialize)]
struct EidsResponse {
    cookie_present: bool,
    eids: Option<Vec<EidView>>,
    parse_error: Option<String>,
    sharedid_present: bool,
    partners_configured: usize,
    ingest: IngestPreview,
}

#[derive(Debug, Deserialize)]
struct IngestPreview {
    matched: Vec<MatchedPartnerId>,
    unmatched: Vec<DroppedEidSource>,
}

#[derive(Debug, Deserialize)]
struct MatchedPartnerId {
    source_domain: String,
    uid: String,
}

#[derive(Debug, Deserialize)]
struct DroppedEidSource {
    source: String,
    reason: String,
}

/// Execute `ts ec eids`.
///
/// # Errors
///
/// Returns an error when no cookie value is given, a value would split the `Cookie`
/// header, the admin password is missing, the service cannot be reached, it answers with
/// a non-success status, or its answer is not an EIDs preview.
pub fn run_eids(args: &EidsArgs, out: &mut impl std::io::Write) -> CliResult<()> {
    let cookie = cookie_header(args.ts_eids.as_deref(), args.shared_id.as_deref())?;
    let client = AdminClient::new(&args.service, &args.username)?;
    let response = client.send(AdminRequest::Get {
        path: ADMIN_EIDS_PATH,
        cookie: Some(&cookie),
    })?;

    if !response.status.is_success() {
        let explanation = match response.status.as_u16() {
            401 => UNAUTHORIZED_HINT,
            404 => " — this service may predate the EIDs diagnostic endpoint",
            _ => "",
        };
        return cli_error(format!(
            "EIDs preview failed: {}{explanation}\n{}",
            response.status,
            response.error_text()
        ));
    }

    let preview: EidsResponse = serde_json::from_str(&response.body)
        .map_err(|error| unexpected_answer("an EIDs preview", &error))?;

    let report = if args.json {
        json_passthrough(&response.body)
    } else {
        render(&preview)
    };
    write_report(out, &report)
}

/// Build the `Cookie` header carrying the given values.
///
/// The service splits the header on `;`, so a value containing one would arrive as a
/// second, different cookie. Whitespace and control characters are refused for the same
/// reason, and non-ASCII characters cannot be sent in a header at all: none can appear in
/// a cookie value a browser sends. The values themselves are never echoed in an error.
fn cookie_header(ts_eids: Option<&str>, shared_id: Option<&str>) -> CliResult<String> {
    let mut pairs = Vec::new();
    for (flag, name, value) in [
        ("--ts-eids", COOKIE_TS_EIDS, ts_eids),
        ("--shared-id", COOKIE_SHAREDID, shared_id),
    ] {
        let Some(value) = value else { continue };
        if value.is_empty() {
            return cli_error(format!("{flag} must not be empty"));
        }
        if value.chars().any(|character| {
            character == ';'
                || character.is_whitespace()
                || character.is_control()
                || !character.is_ascii()
        }) {
            return cli_error(format!(
                "{flag} must be the cookie value alone, as the browser sends it: it must not \
                 contain `;`, whitespace, control, or non-ASCII characters"
            ));
        }
        pairs.push(format!("{name}={value}"));
    }
    if pairs.is_empty() {
        return cli_error("specify --ts-eids <value>, --shared-id <value>, or both");
    }
    Ok(pairs.join("; "))
}

/// Render the preview as an operator-readable summary.
fn render(preview: &EidsResponse) -> String {
    let ts_eids = match (preview.cookie_present, &preview.parse_error, &preview.eids) {
        (false, _, _) => "not received by the service".to_owned(),
        (true, Some(error), _) => format!("received, not parsed: {error}"),
        (true, None, eids) => format!(
            "received, {} EID source(s) parsed",
            eids.as_ref().map_or(0, Vec::len)
        ),
    };
    let mut lines = vec![
        format!("ts-eids cookie:  {ts_eids}"),
        format!(
            "sharedId cookie: {}",
            if preview.sharedid_present {
                "received"
            } else {
                "not received by the service"
            }
        ),
        format!("Partners configured: {}", preview.partners_configured),
    ];

    if let Some(eids) = preview.eids.as_deref().filter(|eids| !eids.is_empty()) {
        lines.push(String::new());
        lines.push(format!("Parsed EIDs ({}):", eids.len()));
        lines.extend(render_eids(eids));
    }

    lines.push(String::new());
    lines.push(format!(
        "Matched partner IDs ({}):",
        preview.ingest.matched.len()
    ));
    lines.extend(
        preview
            .ingest
            .matched
            .iter()
            .map(|matched| format!("  {}: {}", matched.source_domain, matched.uid)),
    );
    lines.push(String::new());
    lines.push(format!(
        "Unmatched EID sources ({}):",
        preview.ingest.unmatched.len()
    ));
    lines.extend(preview.ingest.unmatched.iter().map(|dropped| {
        format!(
            "  {}: {}",
            dropped.source,
            unmatched_reason(&dropped.reason)
        )
    }));
    lines.push(String::new());
    lines.push(
        "This preview reads the cookies alone: it does not apply the consent gate on \
         identity-graph writes, see /auction request-body EIDs, or account for UIDs already \
         stored."
            .to_owned(),
    );

    let mut report = lines.join("\n");
    report.push('\n');
    report
}

/// Explain an unmatched reason the server reports, keeping its code for searching.
fn unmatched_reason(reason: &str) -> String {
    let explanation = match reason {
        "no_partner" => "no configured partner recognizes this source",
        "no_valid_uid" => "every UID is empty or exceeds the storage limit",
        _ => return reason.to_owned(),
    };
    format!("{reason} ({explanation})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_header_sends_each_given_value_under_its_cookie_name() {
        assert_eq!(
            cookie_header(Some("eyJ4Ijoi=="), Some("shared-1")).expect("should build"),
            "ts-eids=eyJ4Ijoi==; sharedId=shared-1"
        );
        assert_eq!(
            cookie_header(None, Some("shared-1")).expect("should build"),
            "sharedId=shared-1"
        );
        assert_eq!(
            cookie_header(Some("abc"), None).expect("should build"),
            "ts-eids=abc"
        );
    }

    #[test]
    fn cookie_header_requires_a_value() {
        assert!(
            cookie_header(None, None).is_err(),
            "should require a cookie"
        );
        assert!(
            cookie_header(Some(""), None).is_err(),
            "should refuse empty"
        );
    }

    #[test]
    fn values_that_would_inject_another_cookie_are_refused_without_echoing_them() {
        for value in [
            "abc; ts-ec=other",
            "abc def",
            "abc\r\nx: y",
            "abc\t",
            "abcé",
        ] {
            let error = cookie_header(Some(value), None)
                .expect_err("should refuse a value that splits the header");
            assert!(
                !error.contains("abc"),
                "should not echo the cookie value, got: {error}"
            );
        }
    }

    #[test]
    fn summary_lists_matched_and_unmatched_partners_with_reasons() {
        let preview: EidsResponse = serde_json::from_value(serde_json::json!({
            "cookie_present": true,
            "eids": [{"source": "configured.example", "uids": [{"id": "uid-1", "atype": 1}]}],
            "sharedid_present": false,
            "partners_configured": 2,
            "ingest": {
                "matched": [{"source_domain": "configured.example", "uid": "uid-1"}],
                "unmatched": [
                    {"source": "unknown.example", "reason": "no_partner"},
                    {"source": "empty.example", "reason": "no_valid_uid"}
                ]
            }
        }))
        .expect("should parse preview");

        let report = render(&preview);

        assert!(report.contains("ts-eids cookie:  received, 1 EID source(s) parsed"));
        assert!(report.contains("sharedId cookie: not received by the service"));
        assert!(report.contains("Matched partner IDs (1):\n  configured.example: uid-1"));
        assert!(report.contains("unknown.example: no_partner (no configured partner"));
        assert!(report.contains("empty.example: no_valid_uid (every UID is empty"));
    }

    #[test]
    fn a_cookie_parse_error_is_shown() {
        let preview: EidsResponse = serde_json::from_value(serde_json::json!({
            "cookie_present": true,
            "parse_error": "failed to parse ts-eids cookie: bad base64",
            "sharedid_present": true,
            "partners_configured": 0,
            "ingest": {"matched": [], "unmatched": []}
        }))
        .expect("should parse preview");

        let report = render(&preview);

        assert!(
            report.contains("received, not parsed: failed to parse ts-eids cookie: bad base64"),
            "got: {report}"
        );
        assert!(report.contains("Matched partner IDs (0):"));
    }
}
