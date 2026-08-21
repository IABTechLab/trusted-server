//! Generic `ts audit page <url>` command: a read-only page summary.

use std::io::{self, Write};

use clap::Args;

use crate::ad_templates::output::escape_terminal_text;
use crate::commands::audit::browser::BrowserCollector;
use crate::commands::audit::collector::{
    AuditCollector, BrowserCollectRequest, BrowserOpts, CollectedPage,
};

/// Arguments for `ts audit page <url>`.
#[derive(Debug, Args)]
pub(crate) struct PageAuditArgs {
    /// The page URL to audit (http or https).
    #[arg(value_parser = crate::commands::audit::parse_http_url)]
    pub url: url::Url,
    /// Perform a deterministic scroll pass after the initial settle.
    #[arg(long)]
    pub scroll: bool,
    #[command(flatten)]
    pub browser: BrowserOpts,
}

/// Runs the generic page audit for the `page` subcommand.
///
/// # Errors
///
/// Returns a user-facing string when the browser cannot collect the page.
pub(crate) fn run_page(args: &PageAuditArgs) -> Result<(), String> {
    args.browser.validate()?;
    run_with_collector(
        &BrowserCollector::from_opts(&args.browser),
        &args.url,
        args.scroll,
    )
}

fn run_with_collector(
    collector: &dyn AuditCollector,
    url: &url::Url,
    scroll: bool,
) -> Result<(), String> {
    let page = collector.collect_page(BrowserCollectRequest {
        url: url.clone(),
        init_scripts: Vec::new(),
        scroll,
        collect_ad_evidence: false,
        cookies: Vec::new(),
    })?;

    let stdout = io::stdout();
    let mut out = stdout.lock();
    write_summary(&mut out, url, &page)
}

fn write_summary(out: &mut dyn Write, url: &url::Url, page: &CollectedPage) -> Result<(), String> {
    let to_err = |error: io::Error| format!("failed to write command output: {error}");
    writeln!(out, "url: {url}").map_err(to_err)?;
    // The final URL, title, and collector warning messages are page-controlled,
    // so escape control characters before they reach the operator's terminal.
    writeln!(
        out,
        "final url: {}",
        escape_terminal_text(page.final_url.as_str())
    )
    .map_err(to_err)?;
    writeln!(out, "title: {}", escape_terminal_text(&page.title)).map_err(to_err)?;
    writeln!(out, "scripts: {}", page.script_count).map_err(to_err)?;
    writeln!(out, "resources: {}", page.resource_count).map_err(to_err)?;
    for warning in &page.warnings {
        writeln!(
            out,
            "warning [{}]: {}",
            escape_terminal_text(&warning.code),
            escape_terminal_text(&warning.message)
        )
        .map_err(to_err)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ad_templates::output::Warning;

    fn collected(final_url: &str, title: &str, warnings: Vec<Warning>) -> CollectedPage {
        CollectedPage {
            final_url: url::Url::parse(final_url).expect("should parse fixture URL"),
            title: title.to_string(),
            script_count: 3,
            resource_count: 42,
            warnings,
            ad_evidence: None,
        }
    }

    fn summary(page: &CollectedPage, requested: &str) -> String {
        let url = url::Url::parse(requested).expect("should parse requested URL");
        let mut out = Vec::new();
        write_summary(&mut out, &url, page).expect("should write summary");
        String::from_utf8(out).expect("summary should be UTF-8")
    }

    #[test]
    fn summary_reports_the_requested_and_final_urls_with_counts() {
        let page = collected(
            "https://publisher.example/news/story",
            "Example Publisher",
            Vec::new(),
        );

        let out = summary(&page, "https://publisher.example/news");

        assert!(
            out.contains("url: https://publisher.example/news\n"),
            "should echo the requested URL, got {out:?}"
        );
        assert!(
            out.contains("final url: https://publisher.example/news/story\n"),
            "should report the post-redirect URL, got {out:?}"
        );
        assert!(out.contains("scripts: 3"), "got {out:?}");
        assert!(out.contains("resources: 42"), "got {out:?}");
    }

    #[test]
    fn page_controlled_text_is_escaped_before_it_reaches_the_terminal() {
        // Title, warning text, and the post-redirect URL are all page-controlled.
        let page = collected(
            "https://publisher.example/a%1B%5B2Jb",
            "Example\u{1b}[2J",
            vec![Warning {
                code: "page_\u{1b}[31m".to_string(),
                message: "message\u{1b}[0m".to_string(),
            }],
        );

        let out = summary(&page, "https://publisher.example/");

        assert!(
            !out.contains('\u{1b}'),
            "no escape sequence may reach the terminal, got {out:?}"
        );
        assert!(
            out.contains("warning [page_"),
            "warnings should still be reported, got {out:?}"
        );
    }
}
