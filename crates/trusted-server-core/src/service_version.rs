//! The deployment's versions as response headers, a middleware of core's
//! own named `service_version` in a `[[serve]]` entry. A deployment that
//! names it writes `X-TS-Version`, the git version the build compiled in,
//! when the build knows one, and `X-TS-Service-Version`, the version the
//! adapter reports for the service, when it reports one, on every page the
//! entry covers. A deployment that does not name it sends nothing about its
//! versions on a page, because no adapter writes either header.

use http::HeaderValue;

use crate::constants::{HEADER_X_TS_SERVICE_VERSION, HEADER_X_TS_VERSION, TS_GIT_VERSION};
use crate::middleware::{Middleware, MiddlewareAction, MiddlewareContext, MiddlewarePhase};
use crate::platform::UNKNOWN;

/// The name an entry writes, and the integration the registry lists the
/// middleware under.
pub const MODULE: &str = "service_version";

/// The middleware. It holds the git version the build compiled in, and reads
/// the platform's version from the context.
#[derive(Debug)]
pub struct ServiceVersion {
    /// The deployed git version, `None` when the build knew none.
    pub git_version: Option<&'static str>,
}

impl Default for ServiceVersion {
    fn default() -> Self {
        Self {
            git_version: TS_GIT_VERSION,
        }
    }
}

impl Middleware for ServiceVersion {
    fn middleware_id(&self) -> &'static str {
        MODULE
    }

    fn phases(&self) -> &[MiddlewarePhase] {
        &[MiddlewarePhase::Serve]
    }

    fn create(&self, context: &MiddlewareContext<'_>) -> MiddlewareAction {
        let mut response_headers = Vec::new();
        if let Some(value) = header_value("git version", self.git_version) {
            response_headers.push((HEADER_X_TS_VERSION, value));
        }
        let service_version =
            Some(context.platform.service_version.as_str()).filter(|version| *version != UNKNOWN);
        if let Some(value) = header_value("service version", service_version) {
            response_headers.push((HEADER_X_TS_SERVICE_VERSION, value));
        }
        MiddlewareAction {
            response_headers,
            ..MiddlewareAction::pass()
        }
    }
}

/// `version` as a header value, or `None`, with a warning, when a header
/// cannot carry it.
fn header_value(what: &str, version: Option<&str>) -> Option<HeaderValue> {
    let version = version?;
    match HeaderValue::from_str(version) {
        Ok(value) => Some(value),
        Err(_) => {
            log::warn!("the {what} {version:?} is not a header value, so it is not written");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::IntegrationDocumentState;
    use crate::middleware::test_support;
    use crate::platform::PlatformIdentity;

    fn action_for(git_version: Option<&'static str>, service_version: &str) -> MiddlewareAction {
        let platform = PlatformIdentity {
            service_version: service_version.to_owned(),
            ..PlatformIdentity::named("example-platform")
        };
        let document_state = IntegrationDocumentState::default();
        ServiceVersion { git_version }.create(&test_support::context_on(
            MiddlewarePhase::Serve,
            &document_state,
            &platform,
        ))
    }

    fn headers_of(action: &MiddlewareAction) -> Vec<(&str, &[u8])> {
        action
            .response_headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_bytes()))
            .collect()
    }

    #[test]
    fn the_git_version_the_build_compiled_in_is_written_as_x_ts_version() {
        let action = action_for(Some("v1.3.0"), UNKNOWN);

        assert_eq!(
            headers_of(&action),
            [("x-ts-version", b"v1.3.0".as_slice())],
            "should write the git version alone on a platform reporting no version"
        );
    }

    #[test]
    fn the_version_the_platform_reports_is_written_as_x_ts_service_version() {
        let action = action_for(None, "52");

        assert_eq!(
            headers_of(&action),
            [("x-ts-service-version", b"52".as_slice())],
            "should write the platform's version alone when the build knew no git version"
        );
    }

    #[test]
    fn both_versions_are_written_when_both_are_known() {
        let action = action_for(Some("v1.3.0"), "52");

        assert_eq!(
            headers_of(&action),
            [
                ("x-ts-version", b"v1.3.0".as_slice()),
                ("x-ts-service-version", b"52".as_slice()),
            ],
            "should write the git version and then the platform's"
        );
    }

    #[test]
    fn a_build_knowing_no_git_version_on_a_platform_reporting_none_writes_nothing() {
        assert!(
            action_for(None, UNKNOWN).is_pass(),
            "should write nothing when neither version is known"
        );
    }

    #[test]
    fn a_version_that_is_not_a_header_value_is_not_written() {
        assert!(
            action_for(Some("v1\n"), "52\n").is_pass(),
            "should write nothing for versions a header cannot carry"
        );
    }

    #[test]
    fn the_default_carries_the_git_version_the_build_compiled_in() {
        assert_eq!(
            ServiceVersion::default().git_version,
            TS_GIT_VERSION,
            "should carry the compiled-in TS_GIT_VERSION"
        );
    }

    #[test]
    fn it_runs_in_the_serve_phase_under_its_name() {
        assert_eq!(ServiceVersion::default().phases(), [MiddlewarePhase::Serve]);
        assert_eq!(ServiceVersion::default().middleware_id(), "service_version");
    }
}
