//! Generic `ts audit page <url>` command: a read-only page summary.

use std::io::{self, Write};

use clap::Args;

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
    run_with_collector(
        &BrowserCollector::from_opts(&args.browser),
        &args.url,
        args.scroll,
    )
}

fn run_with_collector(
    collector: &BrowserCollector,
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
    writeln!(out, "final url: {}", page.final_url).map_err(to_err)?;
    writeln!(out, "title: {}", page.title).map_err(to_err)?;
    writeln!(out, "scripts: {}", page.script_count).map_err(to_err)?;
    writeln!(out, "resources: {}", page.resource_count).map_err(to_err)?;
    for warning in &page.warnings {
        writeln!(out, "warning [{}]: {}", warning.code, warning.message).map_err(to_err)?;
    }
    Ok(())
}
