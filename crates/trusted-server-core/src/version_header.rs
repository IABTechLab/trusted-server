//! `x-ts-version`: the deployed git version compiled in by `build.rs`.

use edgezero_core::http::{HeaderValue, Response};

use crate::constants::{HEADER_X_TS_VERSION, TS_GIT_VERSION};

/// Returns the compiled-in git version as a header value.
///
/// `None` when no version is known or the value is not a valid header value.
#[must_use]
pub fn git_version_header_value() -> Option<HeaderValue> {
    header_value_from(TS_GIT_VERSION)
}

/// Converts `version` to a header value, logging and returning `None` when it
/// is not a valid header value.
#[must_use]
fn header_value_from(version: Option<&str>) -> Option<HeaderValue> {
    let version = version?;
    match HeaderValue::from_str(version) {
        Ok(value) => Some(value),
        Err(_) => {
            log::warn!("Skipping invalid TRUSTED_SERVER__GIT_VERSION response header value");
            None
        }
    }
}

/// Sets `x-ts-version` to the compiled-in git version, if one is known.
pub fn apply_git_version_header(response: &mut Response) {
    apply_git_version_header_from(TS_GIT_VERSION, response);
}

/// Sets `x-ts-version` to `version`, or removes it when `version` is unknown
/// or invalid, so an origin's `x-ts-version` is never reported as ours.
fn apply_git_version_header_from(version: Option<&str>, response: &mut Response) {
    if let Some(value) = header_value_from(version) {
        response.headers_mut().insert(HEADER_X_TS_VERSION, value);
    } else {
        response.headers_mut().remove(HEADER_X_TS_VERSION);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use edgezero_core::body::Body;
    use edgezero_core::http::response_builder;

    fn empty_response() -> Response {
        response_builder()
            .body(Body::empty())
            .expect("should build empty test response")
    }

    fn response_with_upstream_version() -> Response {
        response_builder()
            .header(HEADER_X_TS_VERSION, "upstream-v1")
            .body(Body::empty())
            .expect("should build test response with an upstream x-ts-version")
    }

    fn version_of(response: &Response) -> Option<&str> {
        response
            .headers()
            .get(HEADER_X_TS_VERSION)
            .and_then(|v| v.to_str().ok())
    }

    #[test]
    fn header_value_from_valid_version() {
        assert_eq!(
            header_value_from(Some("v1.2.3")),
            Some(HeaderValue::from_static("v1.2.3")),
            "should convert a valid version"
        );
    }

    #[test]
    fn header_value_from_unknown_or_invalid_version() {
        assert_eq!(header_value_from(None), None, "should be None when unknown");
        assert_eq!(
            header_value_from(Some("bad\nvalue")),
            None,
            "should be None for a non-header-safe version"
        );
    }

    #[test]
    fn git_version_header_value_matches_compiled_in_version() {
        assert_eq!(
            git_version_header_value()
                .as_ref()
                .and_then(|v| v.to_str().ok()),
            TS_GIT_VERSION,
            "should convert the compiled-in TS_GIT_VERSION"
        );
    }

    #[test]
    fn sets_header_from_version() {
        let mut response = empty_response();
        apply_git_version_header_from(Some("v1.2.3"), &mut response);
        assert_eq!(
            version_of(&response),
            Some("v1.2.3"),
            "should set x-ts-version"
        );
    }

    #[test]
    fn omits_header_when_version_unknown() {
        let mut response = empty_response();
        apply_git_version_header_from(None, &mut response);
        assert_eq!(
            version_of(&response),
            None,
            "should omit x-ts-version when unknown"
        );
    }

    #[test]
    fn skips_invalid_header_value() {
        let mut response = empty_response();
        apply_git_version_header_from(Some("bad\nvalue"), &mut response);
        assert_eq!(
            version_of(&response),
            None,
            "should skip a non-header-safe version"
        );
    }

    #[test]
    fn replaces_upstream_header_with_version() {
        let mut response = response_with_upstream_version();
        apply_git_version_header_from(Some("v2.0.0"), &mut response);
        assert_eq!(
            version_of(&response),
            Some("v2.0.0"),
            "should replace an upstream x-ts-version with ours"
        );
    }

    #[test]
    fn removes_upstream_header_when_version_unknown() {
        let mut response = response_with_upstream_version();
        apply_git_version_header_from(None, &mut response);
        assert_eq!(
            version_of(&response),
            None,
            "should remove an upstream x-ts-version when ours is unknown"
        );
    }

    #[test]
    fn removes_upstream_header_when_version_invalid() {
        let mut response = response_with_upstream_version();
        apply_git_version_header_from(Some("bad\nvalue"), &mut response);
        assert_eq!(
            version_of(&response),
            None,
            "should remove an upstream x-ts-version when ours is invalid"
        );
    }

    #[test]
    fn default_uses_compiled_in_version() {
        let mut response = empty_response();
        apply_git_version_header(&mut response);
        assert_eq!(
            version_of(&response),
            TS_GIT_VERSION,
            "should report the compiled-in TS_GIT_VERSION"
        );
    }
}
