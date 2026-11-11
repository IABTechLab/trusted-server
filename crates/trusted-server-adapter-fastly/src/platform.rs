//! Fastly-backed implementations of the platform traits defined in
//! `trusted-server-core::platform`.

use error_stack::Report;
use fastly::geo::{Geo, geo_lookup};
use fastly::{ConfigStore, Request, SecretStore};
use std::net::IpAddr;

use crate::management_api::FastlyManagementApiClient;
pub(crate) use trusted_server_core::platform::UnavailableKvStore;
use trusted_server_core::platform::{
    ClientInfo, GeoInfo, HttpClient, PlatformConfigStore, PlatformError, PlatformGeo,
    PlatformSecretStore, StoreId, StoreName,
};
use trusted_server_core::settings::TrustedClientIpConfig;

// ---------------------------------------------------------------------------
// FastlyPlatformConfigStore
// ---------------------------------------------------------------------------

/// Fastly [`ConfigStore`]-backed implementation of [`PlatformConfigStore`].
///
/// The store name is supplied per call, matching the trait signature.
///
/// # Writes
///
/// `put` and `delete` each send one buffered HTTPS request to the Fastly
/// management API (`api.fastly.com`) through the request's outbound
/// [`HttpClient`]. Callers that issue many writes in one request pay one
/// round trip per call. The `"api-keys"` secret store is opened per call to
/// read the management token; the Fastly Compute SDK caches the open handle so
/// that cost is negligible. A store built with
/// [`FastlyPlatformConfigStore::read_only`], as settings load uses outside a
/// request, has no client and rejects writes.
pub struct FastlyPlatformConfigStore {
    outbound_client: Option<HttpClient>,
}

impl FastlyPlatformConfigStore {
    /// Builds a store whose writes go through `client`, the request's outbound client.
    #[must_use]
    pub fn with_outbound_client(client: HttpClient) -> Self {
        Self {
            outbound_client: Some(client),
        }
    }

    /// Builds a store that only reads, for settings load outside a request.
    #[must_use]
    pub fn read_only() -> Self {
        Self {
            outbound_client: None,
        }
    }
}

#[async_trait::async_trait(?Send)]
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

    async fn put(
        &self,
        store_id: &StoreId,
        key: &str,
        value: &str,
    ) -> Result<(), Report<PlatformError>> {
        let client = write_client(self.outbound_client.as_ref(), PlatformError::ConfigStore)?;
        FastlyManagementApiClient::new()?
            .update_config_item(client, store_id.as_ref(), key, value)
            .await
    }

    async fn delete(&self, store_id: &StoreId, key: &str) -> Result<(), Report<PlatformError>> {
        let client = write_client(self.outbound_client.as_ref(), PlatformError::ConfigStore)?;
        FastlyManagementApiClient::new()?
            .delete_config_item(client, store_id.as_ref(), key)
            .await
    }
}

// ---------------------------------------------------------------------------
// FastlyPlatformSecretStore
// ---------------------------------------------------------------------------

/// Fastly [`SecretStore`]-backed implementation of [`PlatformSecretStore`].
///
/// The store name is supplied per call.
///
/// # Writes
///
/// `create` and `delete` have the same per-call
/// [`crate::management_api::FastlyManagementApiClient`] cost, and the same
/// read-only rule, described on [`FastlyPlatformConfigStore`].
pub struct FastlyPlatformSecretStore {
    outbound_client: Option<HttpClient>,
}

impl FastlyPlatformSecretStore {
    /// Builds a store whose writes go through `client`, the request's outbound client.
    #[must_use]
    pub fn with_outbound_client(client: HttpClient) -> Self {
        Self {
            outbound_client: Some(client),
        }
    }

    /// Builds a store that only reads, as settings load and the management
    /// token read use.
    #[must_use]
    pub fn read_only() -> Self {
        Self {
            outbound_client: None,
        }
    }
}

#[async_trait::async_trait(?Send)]
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

    async fn create(
        &self,
        store_id: &StoreId,
        name: &str,
        value: &str,
    ) -> Result<(), Report<PlatformError>> {
        let client = write_client(self.outbound_client.as_ref(), PlatformError::SecretStore)?;
        FastlyManagementApiClient::new()?
            .create_secret(client, store_id.as_ref(), name, value)
            .await
    }

    async fn delete(&self, store_id: &StoreId, name: &str) -> Result<(), Report<PlatformError>> {
        let client = write_client(self.outbound_client.as_ref(), PlatformError::SecretStore)?;
        FastlyManagementApiClient::new()?
            .delete_secret(client, store_id.as_ref(), name)
            .await
    }
}

/// Returns the outbound client a store write goes through.
///
/// # Errors
///
/// Returns `error` when the store was built read-only and so has no client.
fn write_client(
    client: Option<&HttpClient>,
    error: PlatformError,
) -> Result<&HttpClient, Report<PlatformError>> {
    client.ok_or_else(|| {
        Report::new(error)
            .attach("a read-only store cannot write; build it with the request's outbound client")
    })
}

// ---------------------------------------------------------------------------
// RequestGeo
// ---------------------------------------------------------------------------

/// Convert a Fastly [`Geo`] value into a platform-neutral [`GeoInfo`].
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

/// The geo result the entry point looks up once for a request's resolved client IP.
///
/// `prepare` inserts it into the request extensions. The finalize middleware,
/// the trace metadata, and the per-request services read it, so a request makes
/// one host geo lookup however many steps use geo.
#[derive(Debug, Clone, Default)]
pub(crate) struct RequestGeo {
    client_ip: Option<IpAddr>,
    geo: Option<GeoInfo>,
}

impl RequestGeo {
    /// Holds `geo` as the lookup result for `client_ip`.
    #[cfg(test)]
    pub(crate) fn new(client_ip: Option<IpAddr>, geo: Option<GeoInfo>) -> Self {
        Self { client_ip, geo }
    }

    /// Looks up `client_ip` once with Fastly's geo database.
    pub(crate) fn lookup_once(client_ip: Option<IpAddr>) -> Self {
        Self {
            client_ip,
            geo: client_ip
                .and_then(geo_lookup)
                .map(|geo| geo_from_fastly(&geo)),
        }
    }

    /// The request's geo, if the lookup found any.
    pub(crate) fn geo(&self) -> Option<&GeoInfo> {
        self.geo.as_ref()
    }
}

impl PlatformGeo for RequestGeo {
    /// Answers the request's own IP from the held result, without a host lookup.
    ///
    /// Any other IP gets `Ok(None)`: there is no data for an IP this request
    /// did not resolve.
    fn lookup(&self, client_ip: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        if client_ip == self.client_ip {
            Ok(self.geo.clone())
        } else {
            Ok(None)
        }
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
    fn request_geo_answers_only_the_ip_it_resolved() {
        let resolved_ip = IpAddr::V4(std::net::Ipv4Addr::new(198, 51, 100, 7));
        // Viceroy's default geo data answers loopback, so a restored host
        // lookup for this IP would return data and fail the test.
        let other_ip = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
        let request_geo = RequestGeo::new(
            Some(resolved_ip),
            Some(GeoInfo {
                city: "Springfield".to_owned(),
                country: "US".to_owned(),
                continent: "NorthAmerica".to_owned(),
                latitude: 39.78,
                longitude: -89.65,
                metro_code: 648,
                region: Some("IL".to_owned()),
                asn: None,
            }),
        );

        let held = request_geo
            .lookup(Some(resolved_ip))
            .expect("should answer the resolved IP");
        let other = request_geo
            .lookup(Some(other_ip))
            .expect("should answer another IP");

        assert_eq!(
            held.map(|geo| geo.city),
            Some("Springfield".to_owned()),
            "should return the held geo for the resolved IP"
        );
        assert!(
            other.is_none(),
            "should have no data for an IP this request did not resolve"
        );
    }

    #[test]
    fn read_only_stores_reject_writes() {
        let config = FastlyPlatformConfigStore::read_only();
        let secrets = FastlyPlatformSecretStore::read_only();
        let store_id = StoreId::from("example-store-id");

        let errors = futures::executor::block_on(async {
            [
                (
                    config.put(&store_id, "example-key", "example-value").await,
                    PlatformError::ConfigStore,
                ),
                (
                    config.delete(&store_id, "example-key").await,
                    PlatformError::ConfigStore,
                ),
                (
                    secrets
                        .create(&store_id, "example-name", "example-value")
                        .await,
                    PlatformError::SecretStore,
                ),
                (
                    secrets.delete(&store_id, "example-name").await,
                    PlatformError::SecretStore,
                ),
            ]
        });

        for (result, expected) in errors {
            let error = result.expect_err("should reject a write without an outbound client");
            assert_eq!(
                error.current_context().to_string(),
                expected.to_string(),
                "should report the write against its own store kind"
            );
        }
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
}
