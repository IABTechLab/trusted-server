//! Fixed HTTPS URL facts for the opt-in native lab behind TLS termination.

use edgezero_core::error::EdgeError;
use edgezero_core::http::{Request, Uri, header};
use trusted_server_core::http_util::RequestInfo;

use crate::platform::install_public_url;

/// Fixed public authority bound to publisher configuration, and allowed ingress.
pub(crate) struct LabPublicOrigin {
    public_authority: String,
    ingress_authority: String,
}

impl LabPublicOrigin {
    pub(crate) fn from_env(publisher_domain: &str) -> Result<Option<Self>, EdgeError> {
        let public = read_origin_env("TS_LAB_PUBLIC_ORIGIN", invalid_origin)?;
        let ingress = read_origin_env("TS_LAB_INGRESS_ORIGIN", invalid_ingress_origin)?;
        Self::from_values(public.as_deref(), ingress.as_deref(), publisher_domain)
    }

    pub(crate) fn from_values(
        public: Option<&str>,
        ingress: Option<&str>,
        publisher_domain: &str,
    ) -> Result<Option<Self>, EdgeError> {
        let Some(public) = public else {
            return if ingress.is_some() {
                Err(invalid_ingress_origin())
            } else {
                Ok(None)
            };
        };
        let public_authority = https_authority(public).ok_or_else(invalid_origin)?;
        if public_authority != publisher_domain {
            return Err(invalid_origin());
        }
        let ingress_authority = match ingress {
            Some(value) => https_authority(value).ok_or_else(invalid_ingress_origin)?,
            None => public_authority.clone(),
        };
        Ok(Some(Self {
            public_authority,
            ingress_authority,
        }))
    }

    pub(crate) fn apply(&self, request: &mut Request) -> Result<(), EdgeError> {
        let mut hosts = request.headers().get_all(header::HOST).iter();
        let host = hosts.next().and_then(|value| value.to_str().ok());
        // Authorize the received authority before installing the fixed scheme.
        // Never repair an attacker-controlled Host or absolute request target.
        if host != Some(self.ingress_authority.as_str())
            || hosts.next().is_some()
            || request.uri().authority().is_some_and(|authority| {
                authority.as_str() != self.ingress_authority
                    || request.uri().scheme_str() != Some("https")
            })
        {
            return Err(EdgeError::bad_request(
                "request authority does not match the configured lab ingress origin",
            ));
        }
        install_public_url(
            request,
            RequestInfo {
                host: self.public_authority.clone(),
                scheme: "https".to_owned(),
            },
        )
    }
}

fn read_origin_env(name: &str, invalid: fn() -> EdgeError) -> Result<Option<String>, EdgeError> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(invalid()),
    }
}

fn https_authority(value: &str) -> Option<String> {
    let uri: Uri = value.parse().ok()?;
    let authority = uri.authority()?;
    // Check the original spelling too. URI parsers can discard fragments or
    // normalize a missing path to "/". No extra URL components are allowed.
    let port = authority.as_str().strip_prefix(authority.host())?;
    if uri.scheme_str() != Some("https")
        || value != format!("https://{authority}")
        || authority.host().is_empty()
        || authority.as_str().contains('@')
        || (!port.is_empty() && authority.port_u16().is_none())
    {
        return None;
    }
    Some(authority.as_str().to_owned())
}

fn invalid_ingress_origin() -> EdgeError {
    EdgeError::validation(
        "TS_LAB_INGRESS_ORIGIN requires TS_LAB_PUBLIC_ORIGIN and must be a clean HTTPS origin without userinfo, path, query or fragment",
    )
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
        LabPublicOrigin::from_values(Some("https://lab.example.com"), None, DOMAIN)
            .expect("should validate HTTPS origin")
            .expect("should select fixed origin")
    }

    #[test]
    fn fixed_origin_is_default_off_and_rejects_malformed_startup_values() {
        assert!(
            LabPublicOrigin::from_values(None, None, DOMAIN)
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
                LabPublicOrigin::from_values(Some(value), None, DOMAIN).is_err(),
                "should reject malformed or mismatched startup origin {value:?}"
            );
        }
    }

    #[test]
    fn ingress_origin_requires_public_origin_and_clean_https_authority() {
        const PUBLIC: &str = "https://golf.com";
        const INGRESS: &str = "https://ts-lab-golf-1ba0b7f5.fly.dev";
        assert!(LabPublicOrigin::from_values(None, Some(INGRESS), "golf.com").is_err());
        assert!(LabPublicOrigin::from_values(Some(PUBLIC), Some(INGRESS), "other.com").is_err());
        for value in [
            "",
            "http://ingress.example.com",
            "https://",
            "https://:443",
            "https://user@ingress.example.com",
            "https://user:pass@ingress.example.com",
            "https://ingress.example.com/",
            "https://ingress.example.com/path",
            "https://ingress.example.com?query",
            "https://ingress.example.com#fragment",
            "https://ingress.example.com/#",
            " https://ingress.example.com",
            "https://ingress.example.com\n",
            "https://ingress.example.com\\evil",
            "https://ingress.example.com:",
            "https://ingress.example.com:abc",
            "https://ingress.example.com:65536",
        ] {
            assert!(
                LabPublicOrigin::from_values(Some(PUBLIC), Some(value), "golf.com").is_err(),
                "should reject malformed ingress origin {value:?}"
            );
        }
        for ingress in [
            INGRESS,
            "https://ingress.example.com:8443",
            "https://[::1]:8443",
        ] {
            let origin = LabPublicOrigin::from_values(Some(PUBLIC), Some(ingress), "golf.com")
                .expect("should validate ingress separately")
                .expect("should select fixed origins");
            assert_eq!(origin.public_authority, "golf.com");
            assert_eq!(
                origin.ingress_authority,
                ingress.trim_start_matches("https://")
            );
        }
    }

    #[test]
    fn environment_captures_both_origins_and_rejects_dangling_ingress() {
        temp_env::with_vars(
            [
                ("TS_LAB_PUBLIC_ORIGIN", Some("https://golf.com")),
                ("TS_LAB_INGRESS_ORIGIN", Some("https://ingress.example.com")),
            ],
            || {
                let origin = LabPublicOrigin::from_env("golf.com")
                    .expect("should read both origins")
                    .expect("should enable fixed origins");
                assert_eq!(origin.public_authority, "golf.com");
                assert_eq!(origin.ingress_authority, "ingress.example.com");
            },
        );
        temp_env::with_vars(
            [
                ("TS_LAB_PUBLIC_ORIGIN", None),
                ("TS_LAB_INGRESS_ORIGIN", Some("https://ingress.example.com")),
            ],
            || assert!(LabPublicOrigin::from_env("golf.com").is_err()),
        );
    }

    #[test]
    fn fixed_origin_requires_exact_authority_agreement_including_any_port() {
        assert!(
            LabPublicOrigin::from_values(
                Some("https://lab.example.com:8443"),
                None,
                "lab.example.com:8443",
            )
            .expect("should accept matching authority")
            .is_some()
        );
        assert!(
            LabPublicOrigin::from_values(Some("https://lab.example.com:8443"), None, DOMAIN)
                .is_err()
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
    fn separate_ingress_installs_only_public_facts_and_preserves_request_metadata() {
        use trusted_server_core::platform::GeoInfo;

        const INGRESS: &str = "ts-lab-golf-1ba0b7f5.fly.dev";
        let origin = LabPublicOrigin::from_values(
            Some("https://golf.com"),
            Some("https://ts-lab-golf-1ba0b7f5.fly.dev"),
            "golf.com",
        )
        .expect("should validate separate ingress")
        .expect("should enable fixed origins");
        let geo = GeoInfo::country_region("US", Some("CA".to_owned()));
        let client = ClientInfo {
            client_ip: Some("192.0.2.42".parse().expect("should parse fixture IP")),
            tls_protocol: Some("TLSv1.3".to_owned()),
            ..Default::default()
        };
        for target in [
            "/article?one=two".to_owned(),
            format!("https://{INGRESS}/article?one=two"),
        ] {
            let mut request = request_builder()
                .uri(target)
                .header(header::HOST, INGRESS)
                .header(
                    "forwarded",
                    "for=192.0.2.1;host=evil.example.com;proto=http",
                )
                .header("x-forwarded-host", "evil.example.com")
                .header("x-forwarded-proto", "http")
                .body(Body::from("body"))
                .expect("should build ingress request");
            request.extensions_mut().insert(client.clone());
            request.extensions_mut().insert(geo.clone());
            request.extensions_mut().insert(RequestInfo {
                host: "evil.example.com".to_owned(),
                scheme: "http".to_owned(),
            });
            origin
                .apply(&mut request)
                .expect("should approve ingress authority");
            let info = RequestInfo::from_request(&request, &client);
            assert_eq!(info.host, "golf.com");
            assert_eq!(info.scheme, "https");
            assert_eq!(
                request.uri().to_string(),
                "https://golf.com/article?one=two"
            );
            assert_eq!(request.headers()[header::HOST], "golf.com");
            assert_eq!(request.body().as_bytes(), Some(b"body".as_slice()));
            let retained_client = request
                .extensions()
                .get::<ClientInfo>()
                .expect("should retain client");
            assert_eq!(retained_client.client_ip, client.client_ip);
            assert_eq!(retained_client.tls_protocol, client.tls_protocol);
            assert_eq!(request.extensions().get::<GeoInfo>(), Some(&geo));
            assert_eq!(request.headers()["x-forwarded-host"], "evil.example.com");
            assert_eq!(request.headers()["x-forwarded-proto"], "http");
            assert_eq!(
                request.headers()["forwarded"],
                "for=192.0.2.1;host=evil.example.com;proto=http"
            );
        }
        for (host, target) in [
            ("golf.com", "/article"),
            (INGRESS, "https://golf.com/article"),
            (INGRESS, "http://ts-lab-golf-1ba0b7f5.fly.dev/article"),
        ] {
            let mut request = request_builder()
                .uri(target)
                .header(header::HOST, host)
                .body(Body::from("body"))
                .expect("should build wrong-authority request");
            assert!(origin.apply(&mut request).is_err());
            assert_eq!(request.uri().to_string(), target);
            assert_eq!(request.headers()[header::HOST], host);
            assert_eq!(request.body().as_bytes(), Some(b"body".as_slice()));
            assert!(request.extensions().get::<RequestInfo>().is_none());
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
