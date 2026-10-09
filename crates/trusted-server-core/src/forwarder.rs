//! Authenticated browser-facing origin metadata, separate from transport evidence.

use edgezero_core::body::Body;
use http::{HeaderMap, HeaderName, Request};
use url::Url;

use crate::settings::Settings;

/// The default credential header emitted by the development proxy.
pub const FORWARDER_AUTH_HEADER: &str = "x-ts-forwarder-auth";

/// A frozen forwarding decision captured before transport header sanitation.
///
/// A rejected decision carries no origin or authentication material. Adapters
/// may carry this decision across conversion without changing runtime ingress.
#[derive(Clone, Debug, Default)]
pub struct ForwarderPreparation {
    origin: Option<PublicOrigin>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PublicOrigin {
    pub(crate) scheme: String,
    pub(crate) authority: String,
}

impl PublicOrigin {
    pub(crate) fn serialized(&self) -> String {
        format!("{}://{}", self.scheme, self.authority)
    }
}

impl ForwarderPreparation {
    /// Capture only authenticated, publisher-bounded forwarding metadata.
    ///
    /// Invalid or missing inputs produce an empty decision. This does not
    /// change the supplied fields or retain a credential.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let prepared = ForwarderPreparation::capture(request.headers(), &settings);
    /// request.extensions_mut().insert(prepared);
    /// ```
    #[must_use]
    pub fn capture(headers: &HeaderMap, settings: &Settings) -> Self {
        Self {
            origin: authenticated_public_origin(headers, settings),
        }
    }
}

/// Freeze forwarding metadata and remove forwarding credentials before routing.
///
/// Repeated calls retain the first decision, including a rejected decision.
/// Runtime ingress and browser Origin remain untouched.
///
/// # Examples
///
/// ```ignore
/// prepare_trusted_forwarder(&mut request, &settings);
/// ```
pub fn prepare_trusted_forwarder(request: &mut Request<Body>, settings: &Settings) {
    if request.extensions().get::<ForwarderPreparation>().is_none() {
        let prepared = ForwarderPreparation::capture(request.headers(), settings);
        request.extensions_mut().insert(prepared);
    }
    request.headers_mut().remove(FORWARDER_AUTH_HEADER);
    if let Some(config) = &settings.trusted_forwarder {
        request.headers_mut().remove(config.auth_header.as_str());
    }
    // The decision is now typed metadata. Raw forwarding fields must not
    // influence later consumers or escape to publishers and vendors.
    for name in ["forwarded", "x-forwarded-host", "x-forwarded-proto"] {
        request.headers_mut().remove(name);
    }
}

fn authenticated_public_origin(headers: &HeaderMap, settings: &Settings) -> Option<PublicOrigin> {
    let config = settings.trusted_forwarder.as_ref()?;
    let credential = single_field(headers, &config.auth_header)?;
    if !config.authenticates(credential) {
        return None;
    }
    let scheme = single_field(headers, "x-forwarded-proto")?;
    let authority = single_field(headers, "x-forwarded-host")?;
    let origin = parse_public_origin(scheme, authority)?;
    let url = Url::parse(&origin.serialized()).ok()?;
    let hostname = url.host_str()?;
    let publisher = settings.publisher.domain.to_ascii_lowercase();
    if hostname != publisher
        && !hostname
            .strip_suffix(&publisher)
            .is_some_and(|prefix| prefix.ends_with('.'))
    {
        return None;
    }
    Some(origin)
}

fn single_field<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let name = HeaderName::from_bytes(name.as_bytes()).ok()?;
    let mut fields = headers.get_all(name).iter();
    let field = fields.next()?;
    if fields.next().is_some() {
        return None;
    }
    core::str::from_utf8(field.as_bytes()).ok()
}

fn parse_public_origin(scheme: &str, authority: &str) -> Option<PublicOrigin> {
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https")
        || authority.is_empty()
        || !authority
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-:[]".contains(&byte))
    {
        return None;
    }
    let port = if authority.starts_with('[') {
        let end = authority.find(']')?;
        let suffix = &authority[end + 1..];
        if suffix.is_empty() {
            None
        } else {
            Some(suffix.strip_prefix(':')?)
        }
    } else {
        let (hostname, port) = authority
            .split_once(':')
            .map_or((authority, None), |(host, port)| (host, Some(port)));
        if hostname.len() > 253
            || hostname.split('.').any(|label| {
                label.is_empty()
                    || label.len() > 63
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            })
        {
            return None;
        }
        port
    };
    if let Some(port) = port
        && (port.is_empty()
            || !port.bytes().all(|byte| byte.is_ascii_digit())
            || port.parse::<u16>().ok().is_none_or(|port| port == 0))
    {
        return None;
    }
    let url = Url::parse(&format!("{}://{authority}", scheme.to_ascii_lowercase())).ok()?;
    let serialized = url.origin().ascii_serialization();
    let (scheme, authority) = serialized.split_once("://")?;
    Some(PublicOrigin {
        scheme: scheme.to_owned(),
        authority: authority.to_owned(),
    })
}

pub(crate) fn public_origin<B>(request: &Request<B>) -> Option<&PublicOrigin> {
    request
        .extensions()
        .get::<ForwarderPreparation>()
        .and_then(|prepared| prepared.origin.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use edgezero_core::request::{
        CapturedTarget, HeaderFidelity, InboundOrigin, OriginSource, RequestIngress,
        TargetUnavailable,
    };
    use http::{HeaderValue, header};

    use crate::http_util::RequestInfo;
    use crate::platform::ClientInfo;
    use crate::test_support::tests::create_test_settings;
    use crate::trace::inspect_cookies;

    const SECRET: &str = "fictional-forwarder-secret-0123456789";

    fn settings() -> Settings {
        let mut value = serde_json::to_value(create_test_settings())
            .expect("should serialize example settings");
        value["publisher"]["domain"] = serde_json::json!("publisher.example.com");
        value["trusted_forwarder"] = serde_json::json!({
            "auth_header": FORWARDER_AUTH_HEADER,
            "shared_secret": SECRET,
        });
        Settings::from_json_value(value).expect("should parse forwarder configuration")
    }

    fn request() -> Request<Body> {
        Request::builder()
            .uri("http://upstream.example.com/path")
            .header(header::HOST, "upstream.example.com")
            .header(header::ORIGIN, "https://publisher.example.com:8443")
            .header(FORWARDER_AUTH_HEADER, SECRET)
            .header("x-forwarded-host", "PUBLISHER.example.com:8443")
            .header("x-forwarded-proto", "https")
            .body(Body::empty())
            .expect("should construct forwarded request")
    }

    #[test]
    fn authenticated_origin_overrides_transport_without_mutating_it() {
        let mut request = request();
        let original_uri = request.uri().clone();
        prepare_trusted_forwarder(&mut request, &settings());

        let info = RequestInfo::from_request(&request, &ClientInfo::default());
        assert_eq!(
            info.host, "publisher.example.com:8443",
            "should retain the public port"
        );
        assert_eq!(info.scheme, "https", "should use the public scheme");
        assert_eq!(request.uri(), &original_uri, "should retain transport URI");
        assert_eq!(
            request.headers()[header::HOST],
            "upstream.example.com",
            "should retain transport Host"
        );
        assert_eq!(
            request.headers()[header::ORIGIN],
            "https://publisher.example.com:8443",
            "should retain browser Origin"
        );
        for name in [
            FORWARDER_AUTH_HEADER,
            "forwarded",
            "x-forwarded-host",
            "x-forwarded-proto",
        ] {
            assert!(
                !request.headers().contains_key(name),
                "should remove forwarding field {name}"
            );
        }
    }

    #[test]
    fn capture_accepts_canonical_publisher_origins_and_subdomains() {
        for (host, scheme, expected) in [
            (
                "publisher.example.com:443",
                "HTTPS",
                "https://publisher.example.com",
            ),
            (
                "a.publisher.example.com:8443",
                "https",
                "https://a.publisher.example.com:8443",
            ),
            (
                "publisher.example.com:80",
                "http",
                "http://publisher.example.com",
            ),
        ] {
            let mut request = request();
            request
                .headers_mut()
                .insert("x-forwarded-host", HeaderValue::from_static(host));
            request
                .headers_mut()
                .insert("x-forwarded-proto", HeaderValue::from_static(scheme));
            let prepared = ForwarderPreparation::capture(request.headers(), &settings());
            assert_eq!(
                prepared
                    .origin
                    .as_ref()
                    .map(PublicOrigin::serialized)
                    .as_deref(),
                Some(expected),
                "should canonicalize an allowed public origin"
            );
            assert!(
                !format!("{prepared:?}").contains(SECRET),
                "should not retain authentication material"
            );
        }
    }

    #[test]
    fn capture_rejects_unauthenticated_ambiguous_and_malformed_forwarding() {
        for (name, values) in [
            (FORWARDER_AUTH_HEADER, vec![]),
            (
                FORWARDER_AUTH_HEADER,
                vec!["fictional-wrong-secret-0123456789"],
            ),
            (FORWARDER_AUTH_HEADER, vec![SECRET, SECRET]),
            ("x-forwarded-host", vec![]),
            (
                "x-forwarded-host",
                vec!["publisher.example.com", "publisher.example.com"],
            ),
            (
                "x-forwarded-host",
                vec!["publisher.example.com, publisher.example.com"],
            ),
            ("x-forwarded-host", vec!["other.example.com"]),
            ("x-forwarded-host", vec!["evilpublisher.example.com"]),
            (
                "x-forwarded-host",
                vec!["publisher.example.com.evil.example.com"],
            ),
            ("x-forwarded-host", vec!["publisher.example.com/"]),
            ("x-forwarded-host", vec!["publisher.example.com?"]),
            ("x-forwarded-host", vec!["publisher.example.com#"]),
            ("x-forwarded-host", vec!["@publisher.example.com"]),
            ("x-forwarded-host", vec![" publisher.example.com"]),
            ("x-forwarded-host", vec!["publisher.example.com "]),
            ("x-forwarded-host", vec!["publisher.example.com:"]),
            ("x-forwarded-host", vec!["publisher.example.com:0"]),
            ("x-forwarded-host", vec!["publisher.example.com:65536"]),
            ("x-forwarded-host", vec!["publisher.example.com:+443"]),
            ("x-forwarded-proto", vec![]),
            ("x-forwarded-proto", vec!["https", "https"]),
            ("x-forwarded-proto", vec!["https, http"]),
            ("x-forwarded-proto", vec![" https"]),
            ("x-forwarded-proto", vec!["ftp"]),
        ] {
            let mut request = request();
            request.headers_mut().remove(name);
            for value in values {
                request
                    .headers_mut()
                    .append(name, HeaderValue::from_static(value));
            }
            prepare_trusted_forwarder(&mut request, &settings());
            assert!(
                public_origin(&request).is_none(),
                "should reject invalid {name}"
            );
            assert!(
                !request.headers().contains_key(FORWARDER_AUTH_HEADER),
                "should strip credentials on rejection"
            );
            let info = RequestInfo::from_request(&request, &ClientInfo::default());
            assert_eq!(
                info.host, "upstream.example.com",
                "should fall back to received Host"
            );
            assert_eq!(info.scheme, "http", "should fall back to transport scheme");
        }
    }

    #[test]
    fn preparation_is_frozen_even_when_the_first_decision_rejects() {
        for valid in [true, false] {
            let mut request = request();
            if !valid {
                request.headers_mut().remove(FORWARDER_AUTH_HEADER);
            }
            prepare_trusted_forwarder(&mut request, &settings());
            let first = public_origin(&request).cloned();
            request
                .headers_mut()
                .insert(FORWARDER_AUTH_HEADER, HeaderValue::from_static(SECRET));
            request.headers_mut().insert(
                "x-forwarded-host",
                HeaderValue::from_static("a.publisher.example.com"),
            );
            request
                .headers_mut()
                .insert("x-forwarded-proto", HeaderValue::from_static("http"));
            prepare_trusted_forwarder(&mut request, &settings());
            assert_eq!(
                public_origin(&request),
                first.as_ref(),
                "should retain the first forwarding decision"
            );
            assert!(
                !request.headers().contains_key(FORWARDER_AUTH_HEADER),
                "should strip credentials on repeated preparation"
            );
        }
    }

    #[test]
    fn disabled_forwarding_ignores_and_strips_spoofed_metadata() {
        let mut request = request();
        prepare_trusted_forwarder(&mut request, &create_test_settings());
        assert!(
            public_origin(&request).is_none(),
            "should require explicit configuration"
        );
        assert!(
            !request.headers().contains_key(FORWARDER_AUTH_HEADER),
            "should remove the known credential header even when disabled"
        );
        let info = RequestInfo::from_request(&request, &ClientInfo::default());
        assert_eq!(
            info.host, "upstream.example.com",
            "should ignore spoofed forwarding"
        );
    }

    #[test]
    fn custom_credentials_are_consumed_without_leaving_default_copies() {
        let mut settings = settings();
        settings
            .trusted_forwarder
            .as_mut()
            .expect("should configure forwarding")
            .auth_header = "x-example-forwarder-auth".to_owned();
        let mut request = request();
        request
            .headers_mut()
            .insert("x-example-forwarder-auth", HeaderValue::from_static(SECRET));

        prepare_trusted_forwarder(&mut request, &settings);

        assert!(
            public_origin(&request).is_some(),
            "should authenticate the configured credential field"
        );
        for name in ["x-example-forwarder-auth", FORWARDER_AUTH_HEADER] {
            assert!(
                !request.headers().contains_key(name),
                "should strip both configured and known credential fields"
            );
        }
    }

    #[test]
    fn forwarding_cannot_upgrade_ingress_or_cookie_fidelity() {
        for accepted in [true, false] {
            let mut request = request();
            let ingress = RequestIngress::new(
                CapturedTarget::Unavailable(TargetUnavailable::NotExposed),
                Some(
                    InboundOrigin::parse("http", "upstream.example.com", OriginSource::RuntimeUri)
                        .expect("should construct immutable transport origin"),
                ),
                HeaderFidelity::default(),
                vec![],
            )
            .expect("should retain unavailable target and fidelity");
            let original_fidelity = ingress.header_fidelity(&header::COOKIE);
            request.extensions_mut().insert(ingress);
            request.headers_mut().insert(
                header::COOKIE,
                HeaderValue::from_static("__Host-ts-console=1, unrelated=1"),
            );
            let health = inspect_cookies(
                request.headers(),
                request.extensions().get::<RequestIngress>(),
            );
            if !accepted {
                request.headers_mut().remove(FORWARDER_AUTH_HEADER);
                request
                    .headers_mut()
                    .insert(header::HOST, HeaderValue::from_static("other.example.com"));
            }

            prepare_trusted_forwarder(&mut request, &settings());

            let retained = request
                .extensions()
                .get::<RequestIngress>()
                .expect("should retain runtime ingress");
            assert!(
                matches!(
                    retained.target(),
                    CapturedTarget::Unavailable(TargetUnavailable::NotExposed)
                ),
                "should retain unavailable target evidence"
            );
            let origin = retained.origin().expect("should retain transport origin");
            assert_eq!(origin.scheme(), "http", "should retain transport scheme");
            assert_eq!(
                origin.authority(),
                "upstream.example.com",
                "should retain transport authority"
            );
            assert_eq!(
                retained.header_fidelity(&header::COOKIE),
                original_fidelity,
                "should retain cookie header fidelity"
            );
            assert_eq!(
                inspect_cookies(request.headers(), Some(retained)),
                health,
                "should preserve ambiguous cookie health"
            );
            assert!(
                !health.observed_active(),
                "should never treat folded cookies as a verified session"
            );
            let info = RequestInfo::from_request(&request, &ClientInfo::default());
            assert_eq!(
                info.host,
                if accepted {
                    "publisher.example.com:8443"
                } else {
                    "upstream.example.com"
                },
                "should use one trusted origin instead of mixed mutable facts"
            );
        }
    }
}
