//! Fastly-backed implementations of the platform traits defined in
//! `trusted-server-core::platform`.

#[cfg(test)]
mod outbound_tests;

use error_stack::{Report, ResultExt};
use fastly::geo::{Geo, geo_lookup};
use fastly::{ConfigStore, Request, SecretStore};
use std::net::IpAddr;

use crate::backend::BackendConfig;
pub(crate) use trusted_server_core::platform::UnavailableKvStore;
use trusted_server_core::platform::{
    BackendNamingPolicy, ClientInfo, GeoInfo, PlatformBackend, PlatformBackendSpec,
    PlatformConfigStore, PlatformError, PlatformGeo, PlatformSecretStore, StoreId, StoreName,
};
use trusted_server_core::settings::TrustedClientIpConfig;

// ---------------------------------------------------------------------------
// FastlyPlatformConfigStore
// ---------------------------------------------------------------------------

/// Fastly [`ConfigStore`]-backed implementation of [`PlatformConfigStore`].
///
/// Stateless — the store name is supplied per call, matching the trait
/// signature. This replaces the store-name-at-construction pattern of
/// the legacy `FastlyConfigStore` (removed).
///
/// # Write cost
///
/// `put` and `delete` each perform a synchronous outbound HTTPS request to the
/// Fastly management API (`api.fastly.com`). Callers that issue many writes in
/// one request pay one round-trip per call. The `"api-keys"` secret store is
/// opened per call to read the management token; the Fastly Compute SDK caches
/// the open handle so that cost is negligible.
pub struct FastlyPlatformConfigStore;

impl PlatformConfigStore for FastlyPlatformConfigStore {
    fn get(&self, store_name: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
        let name = store_name.as_ref();
        let store = ConfigStore::try_open(name).map_err(|e| {
            Report::new(PlatformError::ConfigStore)
                .attach(format!("failed to open config store '{name}': {e}"))
        })?;
        store
            .try_get(key)
            .map_err(|e| {
                Report::new(PlatformError::ConfigStore).attach(format!(
                    "lookup for key '{key}' in config store '{name}' failed: {e}"
                ))
            })?
            .ok_or_else(|| {
                Report::new(PlatformError::ConfigStore)
                    .attach(format!("key '{key}' not found in config store '{name}'"))
            })
    }

    fn put(&self, store_id: &StoreId, key: &str, value: &str) -> Result<(), Report<PlatformError>> {
        let client = crate::management_api::FastlyManagementApiClient::new()?;
        client.update_config_item(store_id.as_ref(), key, value)
    }

    fn delete(&self, store_id: &StoreId, key: &str) -> Result<(), Report<PlatformError>> {
        let client = crate::management_api::FastlyManagementApiClient::new()?;
        client.delete_config_item(store_id.as_ref(), key)
    }
}

// ---------------------------------------------------------------------------
// FastlyPlatformSecretStore
// ---------------------------------------------------------------------------

/// Fastly [`SecretStore`]-backed implementation of [`PlatformSecretStore`].
///
/// Stateless — the store name is supplied per call. This replaces the
/// store-name-at-construction pattern of the legacy `FastlySecretStore`
/// (removed).
///
/// # Write cost
///
/// `create` and `delete` have the same per-call
/// [`crate::management_api::FastlyManagementApiClient`] cost described on
/// [`FastlyPlatformConfigStore`].
pub struct FastlyPlatformSecretStore;

impl PlatformSecretStore for FastlyPlatformSecretStore {
    fn get_bytes(
        &self,
        store_name: &StoreName,
        key: &str,
    ) -> Result<Vec<u8>, Report<PlatformError>> {
        let name = store_name.as_ref();
        // Unlike ConfigStore::open (which panics), SecretStore::open already
        // returns Result — there is no try_open variant on SecretStore.
        let store = SecretStore::open(name).map_err(|e| {
            Report::new(PlatformError::SecretStore)
                .attach(format!("failed to open secret store '{name}': {e}"))
        })?;
        let secret = store
            .try_get(key)
            .map_err(|e| {
                Report::new(PlatformError::SecretStore).attach(format!(
                    "lookup for key '{key}' in secret store '{name}' failed: {e}"
                ))
            })?
            .ok_or_else(|| {
                Report::new(PlatformError::SecretStore)
                    .attach(format!("key '{key}' not found in secret store '{name}'"))
            })?;
        secret
            .try_plaintext()
            .map(|bytes| bytes.to_vec())
            .map_err(|e| {
                Report::new(PlatformError::SecretStore)
                    .attach(format!("failed to decrypt secret '{key}': {e}"))
            })
    }

    fn create(
        &self,
        store_id: &StoreId,
        name: &str,
        value: &str,
    ) -> Result<(), Report<PlatformError>> {
        let client = crate::management_api::FastlyManagementApiClient::new()?;
        client.create_secret(store_id.as_ref(), name, value)
    }

    fn delete(&self, store_id: &StoreId, name: &str) -> Result<(), Report<PlatformError>> {
        let client = crate::management_api::FastlyManagementApiClient::new()?;
        client.delete_secret(store_id.as_ref(), name)
    }
}

// ---------------------------------------------------------------------------
// FastlyPlatformBackend
// ---------------------------------------------------------------------------

/// Fastly dynamic-backend implementation of [`PlatformBackend`].
///
/// Delegates name computation and registration to [`BackendConfig`], preserving
/// the existing deterministic naming scheme (scheme + host + port + cert +
/// timeout → unique name).
pub struct FastlyPlatformBackend;

#[cfg(test)]
const TRANSPORT_TIMEOUT_QUANTUM_MS: u32 = 250;
#[cfg(test)]
const SUB_QUANTUM_LADDER_MS: [u32; 4] = [200, 150, 100, 50];

fn backend_config_from_spec(spec: &PlatformBackendSpec) -> BackendConfig<'_> {
    BackendConfig::new(&spec.scheme, &spec.host)
        .port(spec.port)
        .host_header_override(spec.host_header_override.as_deref())
        .certificate_check(spec.certificate_check)
        .first_byte_timeout(spec.first_byte_timeout)
        .between_bytes_timeout(spec.between_bytes_timeout)
        .discriminator(spec.discriminator.as_deref())
}

impl PlatformBackend for FastlyPlatformBackend {
    fn naming_policy(&self) -> BackendNamingPolicy {
        BackendNamingPolicy::Fastly
    }

    fn predict_name(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        // Use the same host normalization as registration. In particular,
        // URL-derived IPv6 hosts arrive bracketed, but both forms must predict
        // the backend that `ensure` actually registers.
        backend_config_from_spec(spec)
            .predict_name()
            .change_context(PlatformError::Backend)
    }

    fn ensure(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        backend_config_from_spec(spec)
            .ensure()
            .change_context(PlatformError::Backend)
    }
}

// ---------------------------------------------------------------------------
// FastlyPlatformGeo
// ---------------------------------------------------------------------------

/// Convert a Fastly [`Geo`] value into a platform-neutral [`GeoInfo`].
///
/// Shared by `FastlyPlatformGeo::lookup` in `trusted-server-adapter-fastly` so
/// that field mapping is never duplicated.
fn geo_from_fastly(geo: &Geo) -> GeoInfo {
    GeoInfo {
        city: geo.city().to_string(),
        country: geo.country_code().to_string(),
        continent: format!("{:?}", geo.continent()),
        latitude: geo.latitude(),
        longitude: geo.longitude(),
        metro_code: geo.metro_code(),
        region: geo.region().map(str::to_string),
        asn: None,
    }
}

/// Fastly geo-lookup implementation of [`PlatformGeo`].
pub struct FastlyPlatformGeo;

impl PlatformGeo for FastlyPlatformGeo {
    fn lookup(&self, client_ip: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(client_ip
            .and_then(geo_lookup)
            .map(|geo| geo_from_fastly(&geo)))
    }
}

fn single_utf8_header<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
    let mut values = req.get_header_all(name);
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    value.to_str().ok()
}

/// Resolve the request's client IP from an authenticated forwarding header.
///
/// When no trusted-client-IP configuration is present, or when either header
/// is missing, duplicated, malformed, or unauthenticated, this returns the
/// Fastly SDK peer address unchanged.
///
/// Every fallback taken while a configuration *is* present logs at debug level
/// so a rotated secret or renamed header is diagnosable. Debug rather than warn
/// keeps a direct client from driving log volume by sending junk trust headers.
#[must_use]
pub(crate) fn resolve_client_ip(
    req: &Request,
    peer_ip: Option<IpAddr>,
    config: Option<&TrustedClientIpConfig>,
) -> Option<IpAddr> {
    let Some(config) = config else {
        return peer_ip;
    };
    let Some(auth_candidate) = single_utf8_header(req, &config.auth_header) else {
        log::debug!("Trusted client IP: auth header is missing, duplicated, or not UTF-8");
        return peer_ip;
    };
    if !config.authenticates(auth_candidate) {
        log::debug!("Trusted client IP: auth header did not match the configured shared secret");
        return peer_ip;
    }
    let Some(ip_candidate) = single_utf8_header(req, &config.ip_header) else {
        log::debug!("Trusted client IP: IP header is missing, duplicated, or not UTF-8");
        return peer_ip;
    };

    ip_candidate.parse::<IpAddr>().ok().or_else(|| {
        log::debug!("Trusted client IP: IP header is not a bare IPv4 or IPv6 address");
        peer_ip
    })
}

/// Extract [`ClientInfo`] from the original Fastly request and resolved client IP.
///
/// Fastly's TLS, JA4, and HTTP/2 fingerprint accessors only return real values
/// on the client request before it is converted to platform HTTP types. This
/// must therefore be called by the `EdgeZero` entry point while the original
/// [`fastly::Request`] is still available. The result is stored in request
/// extensions so `build_per_request_services` can read back metadata the
/// reconstructed request cannot expose.
#[must_use]
pub fn client_info_from_request(req: &Request, client_ip: Option<IpAddr>) -> ClientInfo {
    ClientInfo {
        client_ip,
        tls_protocol: req.get_tls_protocol().ok().flatten().map(str::to_string),
        tls_cipher: req
            .get_tls_cipher_openssl_name()
            .ok()
            .flatten()
            .map(str::to_string),
        tls_ja4: req.get_tls_ja4().map(str::to_string),
        h2_fingerprint: req.get_client_h2_fingerprint().map(str::to_string),
        server_hostname: std::env::var("FASTLY_HOSTNAME").ok(),
        server_region: std::env::var("FASTLY_REGION").ok(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use fastly::http::HeaderValue;
    use trusted_server_core::redacted::Redacted;
    use trusted_server_core::settings::TrustedClientIpConfig;

    const PEER_IP: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 9));

    fn trusted_client_ip_config() -> TrustedClientIpConfig {
        TrustedClientIpConfig {
            ip_header: "fastly-client-ip".to_owned(),
            auth_header: "x-trusted-client-auth".to_owned(),
            shared_secret: Redacted::new("fictional-shared-secret-0123456789".to_owned()),
        }
    }

    fn authenticated_request(ip: impl AsRef<[u8]>) -> Request {
        let mut req = Request::get("https://example.com/");
        req.set_header(
            "x-trusted-client-auth",
            "fictional-shared-secret-0123456789",
        );
        req.set_header("fastly-client-ip", ip.as_ref());
        req
    }

    #[test]
    fn resolve_client_ip_uses_peer_when_config_is_absent() {
        let req = authenticated_request("198.51.100.7");

        let resolved = resolve_client_ip(&req, Some(PEER_IP), None);

        assert_eq!(resolved, Some(PEER_IP), "should preserve the peer IP");
    }

    #[test]
    fn resolve_client_ip_accepts_authenticated_ipv4() {
        let req = authenticated_request("198.51.100.7");

        let resolved = resolve_client_ip(&req, Some(PEER_IP), Some(&trusted_client_ip_config()));

        assert_eq!(
            resolved,
            Some(IpAddr::V4(std::net::Ipv4Addr::new(198, 51, 100, 7))),
            "should use the authenticated IPv4 address"
        );
    }

    #[test]
    fn resolve_client_ip_accepts_authenticated_ipv6() {
        let req = authenticated_request("2001:db8::7");

        let resolved = resolve_client_ip(&req, Some(PEER_IP), Some(&trusted_client_ip_config()));

        assert_eq!(
            resolved,
            Some(IpAddr::V6(std::net::Ipv6Addr::new(
                0x2001, 0xdb8, 0, 0, 0, 0, 0, 7,
            ))),
            "should use the authenticated IPv6 address"
        );
    }

    #[test]
    fn resolve_client_ip_uses_peer_when_auth_is_missing_empty_or_wrong() {
        for auth_value in [None, Some(""), Some("fictional-wrong-secret")] {
            let mut req = Request::get("https://example.com/");
            if let Some(auth_value) = auth_value {
                req.set_header("x-trusted-client-auth", auth_value);
            }
            req.set_header("fastly-client-ip", "198.51.100.7");

            let resolved =
                resolve_client_ip(&req, Some(PEER_IP), Some(&trusted_client_ip_config()));

            assert_eq!(
                resolved,
                Some(PEER_IP),
                "should fall back for auth value {auth_value:?}"
            );
        }
    }

    #[test]
    fn resolve_client_ip_uses_peer_when_auth_is_duplicated() {
        let mut req = authenticated_request("198.51.100.7");
        req.append_header(
            "x-trusted-client-auth",
            "fictional-shared-secret-0123456789",
        );

        let resolved = resolve_client_ip(&req, Some(PEER_IP), Some(&trusted_client_ip_config()));

        assert_eq!(resolved, Some(PEER_IP), "should reject duplicate auth");
    }

    #[test]
    fn resolve_client_ip_uses_peer_when_auth_is_not_utf8() {
        let mut req = Request::get("https://example.com/");
        req.set_header(
            "x-trusted-client-auth",
            HeaderValue::from_bytes(b"fictional-shared-secret-0123456789\xff")
                .expect("should build non-UTF-8 auth header"),
        );
        req.set_header("fastly-client-ip", "198.51.100.7");

        let resolved = resolve_client_ip(&req, Some(PEER_IP), Some(&trusted_client_ip_config()));

        assert_eq!(resolved, Some(PEER_IP), "should reject non-UTF-8 auth");
    }

    #[test]
    fn resolve_client_ip_uses_peer_when_ip_is_missing() {
        let mut req = Request::get("https://example.com/");
        req.set_header(
            "x-trusted-client-auth",
            "fictional-shared-secret-0123456789",
        );

        let resolved = resolve_client_ip(&req, Some(PEER_IP), Some(&trusted_client_ip_config()));

        assert_eq!(resolved, Some(PEER_IP), "should require an IP header");
    }

    #[test]
    fn resolve_client_ip_uses_peer_when_ip_text_is_invalid() {
        for ip_value in [
            " 198.51.100.7",
            "198.51.100.7 ",
            "198.51.100.7:443",
            "2001:db8::7%example0",
            "198.51.100.7, 203.0.113.10",
            "",
        ] {
            let req = authenticated_request(ip_value);

            let resolved =
                resolve_client_ip(&req, Some(PEER_IP), Some(&trusted_client_ip_config()));

            assert_eq!(
                resolved,
                Some(PEER_IP),
                "should reject invalid IP value {ip_value:?}"
            );
        }
    }

    #[test]
    fn resolve_client_ip_uses_peer_when_ip_is_duplicated() {
        let mut req = authenticated_request("198.51.100.7");
        req.append_header("fastly-client-ip", "203.0.113.10");

        let resolved = resolve_client_ip(&req, Some(PEER_IP), Some(&trusted_client_ip_config()));

        assert_eq!(resolved, Some(PEER_IP), "should reject duplicate IP values");
    }

    #[test]
    fn resolve_client_ip_uses_peer_when_ip_is_not_utf8() {
        let req = authenticated_request(
            HeaderValue::from_bytes(b"198.51.100.7\xff").expect("should build non-UTF-8 IP header"),
        );

        let resolved = resolve_client_ip(&req, Some(PEER_IP), Some(&trusted_client_ip_config()));

        assert_eq!(resolved, Some(PEER_IP), "should reject non-UTF-8 IP");
    }

    #[test]
    fn client_info_from_request_preserves_supplied_client_ip() {
        let req = Request::get("https://example.com/");
        let supplied_ip = Some(IpAddr::V4(std::net::Ipv4Addr::new(198, 51, 100, 7)));

        let client_info = client_info_from_request(&req, supplied_ip);

        assert_eq!(
            client_info.client_ip, supplied_ip,
            "should preserve the supplied client IP"
        );
    }

    // --- FastlyPlatformBackend::predict_name --------------------------------

    #[test]
    fn predict_name_produces_same_name_as_backend_config() {
        let backend = FastlyPlatformBackend;
        let spec = PlatformBackendSpec {
            scheme: "https".to_string(),
            host: "origin.example.com".to_string(),
            port: None,
            host_header_override: None,
            certificate_check: true,
            first_byte_timeout: Duration::from_secs(15),
            between_bytes_timeout: Duration::from_secs(15),
            discriminator: None,
        };

        let name = backend
            .predict_name(&spec)
            .expect("should compute backend name for valid spec");

        assert!(
            name.starts_with("backend_https_origin_example_com_443_fb15000_bb15000_"),
            "should match BackendConfig naming convention, got {name}"
        );
    }

    #[test]
    fn predict_name_includes_host_header_override_suffix() {
        let backend = FastlyPlatformBackend;
        let spec = PlatformBackendSpec {
            scheme: "https".to_string(),
            host: "origin.example.com".to_string(),
            port: None,
            host_header_override: Some("www.example.com".to_string()),
            certificate_check: true,
            first_byte_timeout: Duration::from_secs(15),
            between_bytes_timeout: Duration::from_secs(15),
            discriminator: None,
        };

        let name = backend
            .predict_name(&spec)
            .expect("should compute backend name for host header override");

        assert!(
            name.starts_with(
                "backend_https_origin_example_com_443_oh_www_example_com_fb15000_bb15000_"
            ),
            "should match BackendConfig naming convention with host header override, got {name}"
        );
    }

    #[test]
    fn predict_name_includes_nocert_suffix_when_cert_check_disabled() {
        let backend = FastlyPlatformBackend;
        let spec = PlatformBackendSpec {
            scheme: "https".to_string(),
            host: "origin.example.com".to_string(),
            port: None,
            host_header_override: None,
            certificate_check: false,
            first_byte_timeout: Duration::from_secs(15),
            between_bytes_timeout: Duration::from_secs(15),
            discriminator: None,
        };

        let name = backend
            .predict_name(&spec)
            .expect("should compute name with cert check disabled");

        assert!(
            name.contains("nocert"),
            "should include nocert suffix when certificate_check is false"
        );
    }

    #[test]
    fn predict_name_returns_error_for_empty_host() {
        let backend = FastlyPlatformBackend;
        let spec = PlatformBackendSpec {
            scheme: "https".to_string(),
            host: String::new(),
            port: None,
            host_header_override: None,
            certificate_check: true,
            first_byte_timeout: Duration::from_secs(15),
            between_bytes_timeout: Duration::from_secs(15),
            discriminator: None,
        };

        let result = backend.predict_name(&spec);

        assert!(result.is_err(), "should return an error for empty host");
    }

    #[test]
    fn predict_name_encodes_custom_timeout() {
        let backend = FastlyPlatformBackend;
        let spec = PlatformBackendSpec {
            scheme: "https".to_string(),
            host: "origin.example.com".to_string(),
            port: None,
            host_header_override: None,
            certificate_check: true,
            first_byte_timeout: Duration::from_millis(2000),
            between_bytes_timeout: Duration::from_millis(2000),
            discriminator: None,
        };

        let name = backend
            .predict_name(&spec)
            .expect("should compute name with custom timeout");

        assert!(
            name.contains("_fb2000_bb2000_"),
            "should encode 2000ms first-byte and between-bytes timeouts in name, got {name}"
        );
    }

    #[test]
    fn predict_name_matches_ensured_backend_name() {
        // The auction orchestrator maps responses back to providers by the
        // predicted backend name, so predict_name and ensure must return the
        // identical string for the same spec — a divergence would make
        // responses land in the "unknown backend" branch and drop bids
        // silently.
        let backend = FastlyPlatformBackend;
        let spec = PlatformBackendSpec {
            scheme: "https".to_string(),
            host: "consistency.example.com".to_string(),
            port: None,
            host_header_override: None,
            certificate_check: true,
            first_byte_timeout: Duration::from_millis(750),
            between_bytes_timeout: Duration::from_millis(750),
            discriminator: None,
        };

        let predicted = backend
            .predict_name(&spec)
            .expect("should predict backend name");
        let ensured = backend
            .ensure(&spec)
            .expect("should register backend for valid spec");

        assert_eq!(
            predicted, ensured,
            "predicted backend name should match the registered backend name"
        );
    }

    #[test]
    fn bracketed_ipv6_predict_name_matches_bare_and_ensured_backend_name() {
        let backend = FastlyPlatformBackend;
        let bracketed = PlatformBackendSpec {
            scheme: "https".to_string(),
            host: "[2001:db8::9]".to_string(),
            port: Some(8443),
            host_header_override: None,
            certificate_check: true,
            first_byte_timeout: Duration::from_millis(750),
            between_bytes_timeout: Duration::from_millis(750),
            discriminator: Some("ipv6-provider".to_string()),
        };
        let mut bare = bracketed.clone();
        bare.host = "2001:db8::9".to_string();

        let predicted = backend
            .predict_name(&bracketed)
            .expect("should predict bracketed IPv6 backend name");
        let bare_predicted = backend
            .predict_name(&bare)
            .expect("should predict bare IPv6 backend name");
        let ensured = backend
            .ensure(&bracketed)
            .expect("should register bracketed IPv6 backend");

        assert_eq!(predicted, bare_predicted);
        assert_eq!(predicted, ensured);
    }

    // --- FastlyPlatformBackend::canonicalize_transport_timeout_ms -----------

    #[test]
    fn canonicalize_prefers_configured_timeout_when_budget_allows() {
        let backend = FastlyPlatformBackend;
        assert_eq!(
            backend.canonicalize_transport_timeout_ms(2000, 1000),
            1000,
            "should use the configured timeout verbatim when the budget allows"
        );
        assert_eq!(
            backend.canonicalize_transport_timeout_ms(2000, 100),
            100,
            "should preserve a sub-quantum configured constant — it is name-stable on its own"
        );
    }

    #[test]
    fn canonicalize_floors_budget_bound_value_to_quantum() {
        let backend = FastlyPlatformBackend;
        assert_eq!(
            backend.canonicalize_transport_timeout_ms(999, 2000),
            750,
            "should floor a 999ms budget to the 750ms quantum bucket"
        );
        assert_eq!(
            backend.canonicalize_transport_timeout_ms(300, 2000),
            250,
            "should floor a tight budget down to one quantum"
        );
        assert_eq!(
            backend.canonicalize_transport_timeout_ms(250, 2000),
            250,
            "should keep an exact quantum multiple"
        );
    }

    #[test]
    fn canonicalize_snaps_sub_quantum_budget_to_bounded_ladder() {
        let backend = FastlyPlatformBackend;
        // Exact wall-clock values in 1..250 must NOT pass through — that is the
        // unbounded-cardinality regression this ladder closes.
        assert_eq!(
            backend.canonicalize_transport_timeout_ms(249, 2000),
            200,
            "should snap a sub-quantum budget down to the greatest ladder rung, not pass 249 through"
        );
        assert_eq!(backend.canonicalize_transport_timeout_ms(200, 2000), 200);
        assert_eq!(backend.canonicalize_transport_timeout_ms(150, 2000), 150);
        assert_eq!(backend.canonicalize_transport_timeout_ms(100, 2000), 100);
        assert_eq!(backend.canonicalize_transport_timeout_ms(50, 2000), 50);
        assert_eq!(
            backend.canonicalize_transport_timeout_ms(49, 2000),
            0,
            "a budget below the smallest rung rounds to zero (launch skipped)"
        );
        assert_eq!(
            backend.canonicalize_transport_timeout_ms(0, 1000),
            0,
            "an exhausted budget canonicalizes to zero"
        );
        assert_eq!(
            backend.canonicalize_transport_timeout_ms(100, 0),
            0,
            "a zero configured timeout canonicalizes to zero"
        );
    }

    #[test]
    fn canonicalize_budget_derived_names_stay_within_a_safe_cardinality() {
        // Enumerate every reachable remaining budget for a normal 2000ms
        // ceiling and confirm the number of distinct backend-name-bearing
        // transport values an origin can mint stays far below Fastly's
        // per-service dynamic backend limit (documented default 200).
        let backend = FastlyPlatformBackend;
        let configured = 2000;
        let mut distinct = std::collections::BTreeSet::new();
        for remaining in 0..=configured {
            let value = backend.canonicalize_transport_timeout_ms(remaining, configured);
            if value > 0 {
                distinct.insert(value);
            }
            // No arbitrary clock-derived value may leak: every canonical value
            // is either a quantum multiple or one of the bounded ladder rungs.
            assert!(
                value == 0
                    || value % TRANSPORT_TIMEOUT_QUANTUM_MS == 0
                    || SUB_QUANTUM_LADDER_MS.contains(&value),
                "canonical value {value}ms (from remaining {remaining}ms) is neither a quantum \
                 multiple nor a ladder rung"
            );
            // The mediator </body> hold bound relies on canonicalization never
            // extending a transport cap past the wall-clock budget.
            assert!(
                value <= remaining,
                "canonical value {value}ms must not extend past the remaining {remaining}ms budget"
            );
        }
        assert!(
            distinct.len() <= 16,
            "budget-derived transport values should stay well under the dynamic backend limit, \
             got {} distinct values: {distinct:?}",
            distinct.len()
        );
    }

    #[test]
    fn canonicalize_budget_derived_names_stay_bounded_for_large_ceiling() {
        // A large configured ceiling (e.g. a 60s mediator budget) must not let
        // the budget-derived buckets grow with the ceiling. Without the coarse
        // ladder a 60,000ms ceiling would mint ~240 distinct 250ms buckets and
        // blow past Fastly's documented per-service dynamic backend limit (200).
        let backend = FastlyPlatformBackend;
        let configured = 60_000;
        let mut distinct = std::collections::BTreeSet::new();
        for remaining in 0..=configured {
            let value = backend.canonicalize_transport_timeout_ms(remaining, configured);
            if value > 0 {
                distinct.insert(value);
            }
            assert!(
                value == 0
                    || value % TRANSPORT_TIMEOUT_QUANTUM_MS == 0
                    || SUB_QUANTUM_LADDER_MS.contains(&value),
                "canonical value {value}ms (from remaining {remaining}ms) is neither a quantum \
                 multiple nor a ladder rung"
            );
            // The mediator </body> hold bound relies on canonicalization never
            // extending a transport cap past the wall-clock budget.
            assert!(
                value <= remaining,
                "canonical value {value}ms must not extend past the remaining {remaining}ms budget"
            );
        }
        // A budget above the top coarse rung must clamp to it, not open a new
        // bucket per 250ms step.
        assert_eq!(
            backend.canonicalize_transport_timeout_ms(120_000, 240_000),
            60_000,
            "a budget above the top coarse rung clamps to it"
        );
        assert!(
            distinct.len() <= 20,
            "large-ceiling budget-derived values must stay bounded, got {} distinct values: {distinct:?}",
            distinct.len()
        );
    }

    // --- FastlyPlatformBackend::predict_name discriminator ------------------

    #[test]
    fn predict_name_includes_provider_discriminator() {
        let backend = FastlyPlatformBackend;
        let base = PlatformBackendSpec {
            scheme: "https".to_string(),
            host: "gateway.example.com".to_string(),
            port: None,
            host_header_override: None,
            certificate_check: true,
            first_byte_timeout: Duration::from_millis(750),
            between_bytes_timeout: Duration::from_millis(750),
            discriminator: Some("prebid".to_string()),
        };
        let prebid_name = backend
            .predict_name(&base)
            .expect("should predict name with discriminator");
        assert!(
            prebid_name.contains("_p_prebid"),
            "should fold the provider discriminator into the name, got {prebid_name}"
        );

        // Same origin + same transport timeout, different provider → distinct
        // backend names, so auction response correlation cannot cross them.
        let aps = PlatformBackendSpec {
            discriminator: Some("aps".to_string()),
            ..base.clone()
        };
        let aps_name = backend
            .predict_name(&aps)
            .expect("should predict name for the second provider");
        assert_ne!(
            prebid_name, aps_name,
            "two providers on one origin must not share a backend name"
        );
    }
}
