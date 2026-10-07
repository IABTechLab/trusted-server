//! Escaped setup-page shell using only frozen, redacted request facts.

use error_stack::{Report, ResultExt as _};

use super::types::TraceRequestContextV1;

/// Bounded failure category for setup-shell projection.
#[derive(Debug, derive_more::Display)]
pub(crate) enum ShellError {
    /// The redacted request context could not be serialized.
    #[display("Trace setup facts are unavailable")]
    Serialization,
}

impl core::error::Error for ShellError {}

/// Render setup controls and frozen redacted facts using fixed external assets.
///
/// # Errors
///
/// Returns [`ShellError::Serialization`] if the context cannot be serialized.
pub(crate) fn render_setup_shell(
    context: &TraceRequestContextV1,
) -> Result<String, Report<ShellError>> {
    let context_json = serde_json::to_string(context).change_context(ShellError::Serialization)?;
    let context_text = escape_text(&context_json);
    let active = context.cookies().observed_active();
    let observed_state = if active {
        "Tracing is on — cookie observed by server"
    } else {
        "Tracing is off — no valid diagnostics session observed"
    };
    Ok(format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Trusted Server ad diagnostics</title>
<link rel="icon" href="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jXioAAAAASUVORK5CYII=">
<link rel="stylesheet" href="/_ts/trace/assets/v1.css">
<script defer src="/_ts/trace/assets/v1.js"></script>
</head>
<body>
<main>
<h1>Trusted Server ad diagnostics</h1>
<p>No previous ad failure can be recovered. Enable tracing, then reproduce the problem on a freshly reloaded page.</p>
<section aria-labelledby="trace-controls-title">
<h2 id="trace-controls-title">Tracing controls</h2>
<p id="trace-session-state" data-observed-active="{active}">{observed_state}</p>
<div class="controls">
<button class="primary" id="trace-enable" type="button">Enable tracing</button>
<button id="trace-end" type="button">End tracing</button>
<button id="trace-back" type="button">Return to previous page</button>
</div>
<p id="trace-status" role="status" aria-live="polite"></p>
<p>Return to the affected page, reload once, reproduce the problem, then select View trace results.</p>
<p>If history is not useful, reopen the affected article on the exact same hostname and in this same tab, then reload once.</p>
<noscript>JavaScript is required for these deliberate tracing actions.</noscript>
</section>
<section aria-labelledby="trace-setup-title">
<h2 id="trace-setup-title">Setup request</h2>
<p>These facts describe this setup request. They do not describe a previously affected publisher page.</p>
<h3>Approximate network facts</h3>
<p>Masked network identifiers are approximate and may still identify a network.</p>
<dl id="trace-network-facts"></dl>
<h3>Owned cookie health</h3>
<p>Only the shape of Trusted Server cookies visible in this request is inspected. Cookie values and browser cookie attributes are excluded.</p>
<dl id="trace-cookie-facts"></dl>
<pre id="trace-request-context" hidden>{context_text}</pre>
</section>
</main>
</body>
</html>"#
    ))
}

fn escape_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone as _, Utc};
    use http::{HeaderMap, HeaderValue, header};

    use crate::platform::ClientInfo;
    use crate::trace::context::project_request_context;
    use crate::trace::cookies::inspect_cookies;

    use super::*;

    fn context(cookie: &str) -> crate::trace::types::TraceRequestContextV1 {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(cookie).expect("should encode the fictional cookie fixture"),
        );
        let cookies = inspect_cookies(&headers, None);
        project_request_context(
            &ClientInfo {
                tls_cipher: Some("</pre><script>x</script>".to_owned()),
                server_hostname: Some("\" onload='x' <&>".to_owned()),
                ..ClientInfo::default()
            },
            None,
            &cookies,
            Utc.with_ymd_and_hms(2026, 10, 5, 12, 0, 0)
                .single()
                .expect("should create the fixed UTC capture clock"),
        )
        .expect("should project only bounded request facts")
    }

    #[test]
    fn trace_shell_uses_fixed_assets_and_escapes_context_as_text() {
        let html = render_setup_shell(&context(
            "unrelated=fictional-cookie-secret; __Host-ts-console=1",
        ))
        .expect("should serialize the redacted setup shell");
        assert!(
            html.contains("Trusted Server ad diagnostics"),
            "should use the approved page title"
        );
        assert!(
            html.contains("Setup request"),
            "should distinguish setup facts from a traced publisher document"
        );
        assert!(
            html.contains("data-observed-active=\"true\""),
            "should derive state from frozen cookie health"
        );
        assert!(
            html.contains("Tracing is on — cookie observed by server"),
            "should display the setup request observation"
        );
        assert!(
            html.contains("/_ts/trace/assets/v1.js"),
            "should reference the exact immutable script route"
        );
        assert!(
            html.contains("/_ts/trace/assets/v1.css"),
            "should reference the exact immutable stylesheet route"
        );
        assert_eq!(
            html.matches("<script ").count(),
            1,
            "should include only the fixed deferred external script"
        );
        assert!(
            html.contains("defer src=\"/_ts/trace/assets/v1.js\""),
            "should run the script after parsing the shell"
        );
        assert!(
            html.contains("&lt;/pre&gt;&lt;script&gt;x&lt;/script&gt;"),
            "should escape platform strings in text nodes"
        );
        assert!(
            !html.contains("</pre><script>x</script>"),
            "should never execute reflected platform facts"
        );
        assert!(
            !html.contains("fictional-cookie-secret"),
            "should never retain unrelated cookie values"
        );
        assert!(
            !html.contains("window."),
            "should not inline executable request data"
        );
        assert!(
            !html.contains("style="),
            "should respect the external-only stylesheet CSP"
        );
    }

    #[test]
    fn trace_shell_off_state_preserves_end_and_recovery_accessibility() {
        let html = render_setup_shell(&context("unrelated=comma, __Host-ts-console=1"))
            .expect("should retain setup controls when cookie inspection is ambiguous");
        assert!(
            html.contains("data-observed-active=\"false\""),
            "should never activate an ambiguous session"
        );
        assert!(
            html.contains("no valid diagnostics session observed"),
            "should describe observation without claiming cookie absence"
        );
        for id in ["trace-enable", "trace-end", "trace-back", "trace-status"] {
            assert!(
                html.contains(&format!("id=\"{id}\"")),
                "should expose all deliberate controls and status"
            );
        }
        assert!(
            html.contains("aria-live=\"polite\""),
            "should announce action results accessibly"
        );
        assert!(
            html.contains("exact same hostname and in this same tab"),
            "should retain history-independent recovery guidance"
        );
        assert!(
            html.contains("reload once"),
            "should require a fresh cookie-bearing reproduction document"
        );
        assert!(
            html.contains("data:image/png;base64,"),
            "should avoid an automatic publisher favicon request"
        );
        assert!(
            html.contains("width=device-width, initial-scale=1"),
            "should preserve mobile zoom"
        );
        assert!(
            !html.contains("maximum-scale"),
            "should not prevent browser zoom"
        );
    }
}
