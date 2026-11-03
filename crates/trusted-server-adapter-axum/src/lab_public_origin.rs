//! Fixed HTTPS URL facts for the opt-in native lab behind TLS termination.

use edgezero_core::error::EdgeError;
use edgezero_core::http::{Request, Uri, header};
use trusted_server_core::http_util::RequestInfo;

use crate::platform::install_public_url;

/// Validated operator authority, bound to the adopted publisher configuration.
pub(crate) struct LabPublicOrigin {
    authority: String,
}

impl LabPublicOrigin {
    pub(crate) fn from_env(publisher_domain: &str) -> Result<Option<Self>, EdgeError> {
        match std::env::var("TS_LAB_PUBLIC_ORIGIN") {
            Ok(value) => Self::from_value(Some(&value), publisher_domain),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => Err(invalid_origin()),
        }
    }

    pub(crate) fn from_value(
        value: Option<&str>,
        publisher_domain: &str,
    ) -> Result<Option<Self>, EdgeError> {
        let Some(value) = value else {
            return Ok(None);
        };
        let uri: Uri = value.parse().map_err(|_| invalid_origin())?;
        let authority = uri.authority().ok_or_else(invalid_origin)?.as_str();
        // Check the original spelling too. URI parsers can discard fragments or
        // normalize a missing path to "/". No extra URL components are allowed.
        if uri.scheme_str() != Some("https")
            || authority != publisher_domain
            || value != format!("https://{authority}")
            || authority.contains('@')
        {
            return Err(invalid_origin());
        }
        Ok(Some(Self {
            authority: authority.to_owned(),
        }))
    }

    pub(crate) fn apply(&self, request: &mut Request) -> Result<(), EdgeError> {
        let mut hosts = request.headers().get_all(header::HOST).iter();
        let host = hosts.next().and_then(|value| value.to_str().ok());
        // Authorize the received authority before installing the fixed scheme.
        // Never repair an attacker-controlled Host or absolute request target.
        if host != Some(self.authority.as_str())
            || hosts.next().is_some()
            || request.uri().authority().is_some_and(|authority| {
                authority.as_str() != self.authority || request.uri().scheme_str() != Some("https")
            })
        {
            return Err(EdgeError::bad_request(
                "request authority does not match the configured lab public origin",
            ));
        }
        install_public_url(
            request,
            RequestInfo {
                host: self.authority.clone(),
                scheme: "https".to_owned(),
            },
        )
    }
}

fn invalid_origin() -> EdgeError {
    EdgeError::validation(
        "TS_LAB_PUBLIC_ORIGIN must be https:// followed by exactly the adopted publisher.domain, without userinfo, path, query or fragment",
    )
}

#[cfg(test)]
mod tests {
    use edgezero_core::body::Body;
    use edgezero_core::http::request_builder;
    use trusted_server_core::platform::ClientInfo;

    use super::*;

    const DOMAIN: &str = "lab.example.com";

    fn origin() -> LabPublicOrigin {
        LabPublicOrigin::from_value(Some("https://lab.example.com"), DOMAIN)
            .expect("should validate HTTPS origin")
            .expect("should select fixed origin")
    }

    #[test]
    fn fixed_origin_is_default_off_and_rejects_malformed_startup_values() {
        assert!(
            LabPublicOrigin::from_value(None, DOMAIN)
                .expect("should accept absent option")
                .is_none()
        );
        for value in [
            "",
            "http://lab.example.com",
            "https://other.example.com",
            "https://user@lab.example.com",
            "https://user:password@lab.example.com",
            "https://lab.example.com:443",
            "https://lab.example.com/",
            "https://lab.example.com/path",
            "https://lab.example.com?query",
            "https://lab.example.com#fragment",
            "https://lab.example.com/#",
            " https://lab.example.com",
            "https://lab.example.com\n",
            "https://lab.example.com\\evil",
        ] {
            assert!(
                LabPublicOrigin::from_value(Some(value), DOMAIN).is_err(),
                "should reject malformed or mismatched startup origin {value:?}"
            );
        }
    }

    #[test]
    fn fixed_origin_requires_exact_authority_agreement_including_any_port() {
        assert!(
            LabPublicOrigin::from_value(
                Some("https://lab.example.com:8443"),
                "lab.example.com:8443",
            )
            .expect("should accept matching authority")
            .is_some()
        );
        assert!(
            LabPublicOrigin::from_value(Some("https://lab.example.com:8443"), DOMAIN,).is_err()
        );
    }

    #[test]
    fn fixed_https_updates_all_consumers_without_changing_body_or_client_facts() {
        for target in [
            "/article?one=two",
            "https://lab.example.com/article?one=two",
        ] {
            let mut request = request_builder()
                .uri(target)
                .header(header::HOST, DOMAIN)
                .header(
                    "forwarded",
                    "for=192.0.2.1;host=evil.example.com;proto=http",
                )
                .header("x-forwarded-host", "evil.example.com")
                .header("x-forwarded-proto", "http")
                .body(Body::from("body"))
                .expect("should build lab request");
            let client = ClientInfo {
                client_ip: Some("192.0.2.42".parse().expect("should parse fixture address")),
                ..Default::default()
            };
            request.extensions_mut().insert(client.clone());
            origin()
                .apply(&mut request)
                .expect("should approve fixed origin");
            let info = RequestInfo::from_request(&request, &client);
            assert_eq!(info.scheme, "https");
            assert_eq!(info.host, DOMAIN);
            assert_eq!(
                request.uri().to_string(),
                "https://lab.example.com/article?one=two"
            );
            assert_eq!(request.headers()[header::HOST], DOMAIN);
            assert_eq!(request.body().as_bytes(), Some(b"body".as_slice()));
            assert_eq!(
                request
                    .extensions()
                    .get::<ClientInfo>()
                    .expect("should retain client facts")
                    .client_ip,
                client.client_ip
            );
        }
    }

    #[test]
    fn fixed_origin_rejects_wrong_host_and_absolute_target_without_repairing_them() {
        for (host, target) in [
            (Some("evil.example.com"), "/article"),
            (None, "/article"),
            (Some("lab.example.com:443"), "/article"),
            (Some(DOMAIN), "https://evil.example.com/article"),
            (Some("evil.example.com"), "https://lab.example.com/article"),
            (Some(DOMAIN), "http://lab.example.com/article"),
            (Some(DOMAIN), "https://user@lab.example.com/article"),
        ] {
            let mut builder = request_builder().uri(target);
            if let Some(host) = host {
                builder = builder.header(header::HOST, host);
            }
            let mut request = builder
                .header("x-forwarded-host", DOMAIN)
                .header("x-forwarded-proto", "https")
                .body(Body::empty())
                .expect("should build hostile request");
            assert!(origin().apply(&mut request).is_err());
            assert_eq!(request.uri().to_string(), target);
            assert!(request.extensions().get::<RequestInfo>().is_none());
        }
        let mut duplicate = request_builder()
            .uri("/")
            .header(header::HOST, DOMAIN)
            .header(header::HOST, DOMAIN)
            .body(Body::empty())
            .expect("should build duplicate Host request");
        assert!(origin().apply(&mut duplicate).is_err());
    }
}
