//! App-level `EdgeZero` policies that every adapter installs from `Hooks::configure`.

use std::time::Duration;

use edgezero_core::app::App;
use edgezero_core::error::EdgeError;
use edgezero_core::http::HeaderMap;
use edgezero_core::response_egress::{ResponseEgressHead, ResponseEgressPolicy};
use edgezero_core::time::{Deadline, MonotonicInstant};

use crate::constants::HEADER_X_TS_VERSION;
use crate::version_header::git_version_header_value;

/// How long a response may take to reach the client once egress starts.
///
/// `EdgeZero` defaults to 30 s, which would cut a streamed publisher page short
/// whatever its origin timeout.
pub const RESPONSE_WRITE_DEADLINE: Duration = Duration::from_secs(60);

/// Installs the response write deadline and the adapter's app-level synthesized headers.
///
/// The egress policy ends each response write [`RESPONSE_WRITE_DEADLINE`] after
/// egress starts. `synthesized` is [`synthesized_response_headers`], or an
/// adapter's set built from it. `EdgeZero` adds it, where a field is absent, to
/// the responses it generates itself, such as the canonical 404 and 405. Routed
/// responses keep the headers the finalize middleware sets.
///
/// # Errors
///
/// Returns [`EdgeError::Internal`] when `EdgeZero` rejects the synthesized set.
///
/// # Examples
///
/// ```
/// use edgezero_core::app::App;
/// use edgezero_core::router::RouterService;
/// use trusted_server_core::edge_app::{install_edge_policies, synthesized_response_headers};
///
/// let mut app = App::new(RouterService::builder().build());
/// install_edge_policies(&mut app, synthesized_response_headers())
///     .expect("should install the edge policies");
/// ```
pub fn install_edge_policies(app: &mut App, synthesized: HeaderMap) -> Result<(), EdgeError> {
    app.set_response_egress_policy(response_write_policy);
    app.set_synthesized_response_headers(synthesized)
        .map_err(EdgeError::internal)
}

/// Returns the app-level synthesized response headers.
///
/// The set holds `x-ts-version` when a git version is compiled in, the value
/// the finalize middleware sets on routed responses. An adapter that adds its
/// own fields starts from this set and passes the result to
/// [`install_edge_policies`].
///
/// # Examples
///
/// ```
/// use trusted_server_core::constants::{HEADER_X_TS_VERSION, TS_GIT_VERSION};
/// use trusted_server_core::edge_app::synthesized_response_headers;
///
/// let headers = synthesized_response_headers();
/// assert_eq!(
///     headers
///         .get(HEADER_X_TS_VERSION)
///         .and_then(|value| value.to_str().ok()),
///     TS_GIT_VERSION,
///     "should hold the compiled-in git version",
/// );
/// ```
#[must_use]
pub fn synthesized_response_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Some(version) = git_version_header_value() {
        headers.insert(HEADER_X_TS_VERSION, version);
    }
    headers
}

fn response_write_policy(
    _head: &ResponseEgressHead<'_>,
    egress_started_at: MonotonicInstant,
) -> ResponseEgressPolicy {
    // An unrepresentable deadline fails closed at the egress start, as
    // EdgeZero's default policy does.
    let deadline = egress_started_at
        .checked_add(RESPONSE_WRITE_DEADLINE)
        .unwrap_or(egress_started_at);
    ResponseEgressPolicy {
        write_deadline: Deadline::at_instant(deadline),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use edgezero_core::http::{StatusCode, Version};
    use edgezero_core::router::RouterService;

    #[test]
    fn installed_egress_policy_sets_write_deadline_sixty_seconds_after_egress_start() {
        let mut app = App::new(RouterService::builder().build());
        install_edge_policies(&mut app, synthesized_response_headers())
            .expect("should install the edge policies");
        let headers = HeaderMap::new();
        let request_start = MonotonicInstant::now();
        let egress_started_at = request_start
            .checked_add(Duration::from_secs(5))
            .expect("should represent an egress start after the request start");
        let head = ResponseEgressHead::new(
            StatusCode::OK,
            Version::HTTP_11,
            &headers,
            None,
            request_start,
            None,
        );

        let policy = (app.response_egress_policy())(&head, egress_started_at);

        assert_eq!(
            policy.write_deadline.instant(),
            egress_started_at
                .checked_add(Duration::from_secs(60))
                .expect("should represent the expected deadline"),
            "should end the response write 60 s after egress starts, not after the request starts"
        );
    }
}
