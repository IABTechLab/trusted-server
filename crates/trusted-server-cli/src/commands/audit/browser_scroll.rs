//! Shared deterministic browser scrolling for audit commands.

use std::time::Duration;

use chromiumoxide::Page;

const SCROLL_STEP_DELAY: Duration = Duration::from_millis(250);
const SCROLL_OPERATION_TIMEOUT: Duration = Duration::from_secs(5);

/// A best-effort browser scroll operation that could not be completed.
#[derive(Debug, derive_more::Display)]
pub(crate) enum ScrollFailure {
    /// Chrome rejected the page evaluation.
    #[display("browser page evaluation failed: {_0}")]
    Evaluation(String),
    /// Chrome did not complete the page evaluation within the operation bound.
    #[display("browser page evaluation timed out")]
    Timeout,
}

impl core::error::Error for ScrollFailure {}

impl ScrollFailure {
    /// Stable warning code used by structured audit output.
    pub(crate) const fn code(&self) -> &'static str {
        match self {
            Self::Evaluation(_) => "page_evaluation_failed",
            Self::Timeout => "page_evaluation_timeout",
        }
    }
}

/// Scrolls a page through deterministic fractions to trigger lazy content.
pub(crate) async fn scroll_page(page: &chromiumoxide::Page) -> Vec<ScrollFailure> {
    let mut failures = Vec::new();
    for fraction in ["0.33", "0.66", "1"] {
        let script = format!(
            "window.scrollTo(0, Math.floor(Math.max(document.body.scrollHeight, \
             document.documentElement.scrollHeight) * {fraction}))"
        );
        if let Err(failure) = evaluate(page, script).await {
            failures.push(failure);
        }
        tokio::time::sleep(SCROLL_STEP_DELAY).await;
    }
    if let Err(failure) = evaluate(page, "window.scrollTo(0, 0)").await {
        failures.push(failure);
    }
    failures
}

/// Evaluates a browser expression with the shared operation bound and errors.
pub(crate) async fn evaluate(
    page: &Page,
    expression: impl Into<String>,
) -> Result<(), ScrollFailure> {
    tokio::time::timeout(SCROLL_OPERATION_TIMEOUT, page.evaluate(expression.into()))
        .await
        .map_err(|_| ScrollFailure::Timeout)?
        .map(|_| ())
        .map_err(|error| ScrollFailure::Evaluation(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::ScrollFailure;

    #[test]
    fn scroll_failures_have_stable_messages() {
        assert_eq!(
            ScrollFailure::Evaluation("execution context was destroyed".to_string()).to_string(),
            "browser page evaluation failed: execution context was destroyed"
        );
        assert_eq!(
            ScrollFailure::Timeout.to_string(),
            "browser page evaluation timed out"
        );

        fn assert_error<T: core::error::Error>() {}
        assert_error::<ScrollFailure>();
    }
}
