//! The platform's service version as a response header, a middleware of
//! core's own named `service_version` in a `[[serve]]` entry. A deployment
//! that names it writes `X-TS-Version`, with the version the adapter reports
//! for the service, on every page the entry covers, and a deployment that
//! does not name it sends nothing about the service on a page. The version
//! is always in the attestation document, under `platform.serviceVersion`.

use http::HeaderValue;

use crate::attestation::UNKNOWN;
use crate::constants::HEADER_X_TS_VERSION;
use crate::middleware::{Middleware, MiddlewareAction, MiddlewareContext, MiddlewarePhase};

/// The name an entry writes, and the integration the registry lists the
/// middleware under.
pub const MODULE: &str = "service_version";

/// The middleware. It holds nothing, because the version is the platform's
/// and is read from the context.
#[derive(Debug, Default)]
pub struct ServiceVersion;

impl Middleware for ServiceVersion {
    fn middleware_id(&self) -> &'static str {
        MODULE
    }

    fn phases(&self) -> &[MiddlewarePhase] {
        &[MiddlewarePhase::Serve]
    }

    fn create(&self, context: &MiddlewareContext<'_>) -> MiddlewareAction {
        let version = &context.platform.service_version;
        if version == UNKNOWN {
            return MiddlewareAction::pass();
        }
        match HeaderValue::from_str(version) {
            Ok(value) => MiddlewareAction {
                response_headers: vec![(HEADER_X_TS_VERSION, value)],
                ..MiddlewareAction::pass()
            },
            Err(_) => {
                log::warn!(
                    "the platform's service version {version:?} is not a header value, so no \
                     X-TS-Version is written"
                );
                MiddlewareAction::pass()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attestation::PlatformIdentity;
    use crate::integrations::IntegrationDocumentState;
    use crate::middleware::test_support;

    fn action_for(version: &str) -> MiddlewareAction {
        let platform = PlatformIdentity {
            service_version: version.to_owned(),
            ..PlatformIdentity::named("example-platform")
        };
        let document_state = IntegrationDocumentState::default();
        ServiceVersion.create(&test_support::context_on(
            MiddlewarePhase::Serve,
            &document_state,
            &platform,
        ))
    }

    #[test]
    fn the_version_the_platform_reports_is_written_as_x_ts_version() {
        let action = action_for("52");

        assert_eq!(
            action
                .response_headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_bytes()))
                .collect::<Vec<_>>(),
            [("x-ts-version", b"52".as_slice())],
            "should write the version the platform reports"
        );
    }

    #[test]
    fn a_platform_reporting_no_version_gets_no_header() {
        assert!(
            action_for(UNKNOWN).is_pass(),
            "should write nothing for a version the platform did not report"
        );
    }

    #[test]
    fn a_version_that_is_not_a_header_value_gets_no_header() {
        assert!(
            action_for("52\n").is_pass(),
            "should write nothing for a version a header cannot carry"
        );
    }

    #[test]
    fn it_runs_in_the_serve_phase_under_its_name() {
        assert_eq!(ServiceVersion.phases(), [MiddlewarePhase::Serve]);
        assert_eq!(ServiceVersion.middleware_id(), "service_version");
    }
}
