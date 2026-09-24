//! Renders an [`AuditReport`] as a human-readable table or as JSON.
//!
//! Output is written to an injected `Write` sink (rather than `println!`) so the
//! rendering is unit-testable, following the `config init` and `prebid bundle`
//! commands.

use std::io::Write;

use super::analyze::AuditReport;
use super::rules::Verdict;
use crate::error::CliResult;

/// Writes the report to `out` in the format selected by `json`.
pub(crate) fn render(report: &AuditReport, json: bool, out: &mut dyn Write) -> CliResult<()> {
    if json {
        render_json(report, out)
    } else {
        render_human(report, out)
    }
}

fn render_json(report: &AuditReport, out: &mut dyn Write) -> CliResult<()> {
    let serialized = serde_json::to_string_pretty(report)
        .map_err(|error| format!("failed to serialize audit report: {error}"))?;
    writeln!(out, "{serialized}").map_err(|error| write_error(&error))
}

fn render_human(report: &AuditReport, out: &mut dyn Write) -> CliResult<()> {
    writeln!(out, "Origin: {}", report.origin).map_err(|error| write_error(&error))?;
    writeln!(out).map_err(|error| write_error(&error))?;

    if report.groups.is_empty() {
        writeln!(out, "No auditable content types found.").map_err(|error| write_error(&error))?;
        return Ok(());
    }

    for group in &report.groups {
        writeln!(
            out,
            "{} {}",
            verdict_glyph(group.verdict),
            group.content_type.label()
        )
        .map_err(|error| write_error(&error))?;

        for verdict in &group.verdicts {
            let recommendation = if verdict.recommendation.is_empty() {
                "--"
            } else {
                &verdict.recommendation
            };
            writeln!(
                out,
                "    {} {:<18} {}",
                verdict_glyph(verdict.verdict),
                verdict.header,
                recommendation
            )
            .map_err(|error| write_error(&error))?;
        }
        writeln!(out).map_err(|error| write_error(&error))?;
    }

    writeln!(
        out,
        "Summary (per content type): {} pass, {} warn, {} fail ({} types audited)",
        report.summary.pass, report.summary.warn, report.summary.fail, report.summary.total_groups
    )
    .map_err(|error| write_error(&error))?;

    Ok(())
}

/// A terminal glyph for a verdict. ASCII-safe fallbacks are unnecessary; the
/// glyphs match the spec's sample output.
fn verdict_glyph(verdict: Verdict) -> char {
    match verdict {
        Verdict::Pass => '✓',
        Verdict::Warn => '⚠',
        Verdict::Fail => '✗',
    }
}

fn write_error(error: &std::io::Error) -> String {
    format!("failed to write command output: {error}")
}

#[cfg(test)]
mod tests {
    use url::Url;

    use super::super::analyze::{FetchedResponse, run_analysis};
    use super::super::rules::ResponseHeaders;
    use super::*;

    fn sample_report() -> AuditReport {
        let responses = vec![
            FetchedResponse {
                url: Url::parse("https://origin.example/").expect("should parse"),
                headers: ResponseHeaders {
                    content_type: Some("text/html".to_owned()),
                    cache_control: Some("public, max-age=3600".to_owned()),
                    ..ResponseHeaders::default()
                },
            },
            FetchedResponse {
                url: Url::parse("https://origin.example/app.js").expect("should parse"),
                headers: ResponseHeaders {
                    content_type: Some("application/javascript".to_owned()),
                    cache_control: Some("public, max-age=31536000, immutable".to_owned()),
                    surrogate_key: Some("js".to_owned()),
                    etag: Some("\"abc\"".to_owned()),
                    ..ResponseHeaders::default()
                },
            },
        ];
        run_analysis("https://origin.example", &responses)
    }

    #[test]
    fn human_output_includes_origin_and_summary() {
        let report = sample_report();
        let mut buffer = Vec::new();
        render(&report, false, &mut buffer).expect("should render");
        let text = String::from_utf8(buffer).expect("should be utf-8");
        assert!(
            text.contains("Origin: https://origin.example"),
            "should print the origin"
        );
        assert!(
            text.contains("Summary (per content type):"),
            "should print the summary line"
        );
        assert!(text.contains("HTML"), "should list the HTML group");
    }

    #[test]
    fn json_output_is_valid_and_stable() {
        let report = sample_report();
        let mut buffer = Vec::new();
        render(&report, true, &mut buffer).expect("should render");
        let text = String::from_utf8(buffer).expect("should be utf-8");
        let parsed: serde_json::Value =
            serde_json::from_str(&text).expect("should emit valid JSON");
        assert_eq!(
            parsed["origin"], "https://origin.example",
            "should serialize origin"
        );
        assert_eq!(
            parsed["summary"]["total_groups"], 2,
            "should count both groups"
        );
        // Verdicts serialize as bare strings, not payload-carrying objects.
        assert_eq!(
            parsed["groups"][0]["verdict"], "Fail",
            "cacheable HTML group should serialize a bare Fail verdict"
        );
    }
}
