//! Edge Cookie identity providers.
//!
//! An [`EdgeCookieProvider`] derives an Edge Cookie identifier. Providers are
//! wired by dependency injection: a provider's constructor takes the services it
//! needs (for example [`RequestInfo`] for the client IP, or [`HostSignals`] for
//! the TLS/HTTP-2 fingerprints) as `Arc<dyn Trait>`, and the composition root
//! (the adapter, through [`build_provider`]) supplies instances per request. A
//! provider that needs a service the host does not supply cannot be built, so
//! the request stops rather than silently degrading.
//!
//! The provider is selected by configuration, with no default. [`HmacProvider`]
//! is the built-in server-side implementation that derives the identifier from
//! the client IP using HMAC, the behavior Trusted Server has always shipped.

use std::sync::Arc;

use error_stack::Report;

use crate::consent::ConsentContext;
use crate::error::TrustedServerError;
use crate::evidence::{HostSignals, RequestInfo};
use crate::permissions::{Permission, PermissionSet, PermissionState};
use crate::redacted::Redacted;
use crate::settings::Ec;

use super::generation;

/// The request-scoped gating context passed to [`EdgeCookieProvider::generate`].
///
/// Request data (client IP, User-Agent, headers, host signals) reaches a
/// provider through the services injected into its constructor, not through this
/// struct. This carries only the per-request gating context a provider may read
/// for behavior beyond gating. The gate has already confirmed the provider's
/// required permissions are set before `generate` is called.
#[derive(Default)]
pub struct IdentityInput<'a> {
    /// The permissions resolved for this request, when the calling path carries
    /// them. A provider reads this only for behavior beyond gating. The main
    /// organic path supplies them; the publisher path passes `None`.
    pub permissions: Option<&'a PermissionState>,

    /// The request's consent context, when available, for provider-specific
    /// logic. The core gates on permissions, not consent, so a provider reads
    /// this only to forward or record consent. [`HmacProvider`] ignores it.
    pub consent: Option<&'a ConsentContext>,
}

/// Inputs available to [`EdgeCookieProvider::resolve_from_client`].
///
/// Carries the value a client produced and posted to the Edge Cookie resolve
/// endpoint, alongside the same gating context as [`IdentityInput`]. Request data
/// reaches the provider through its injected services. Unlike trusted edge-derived
/// data, [`payload`](Self::payload) arrives from the browser, so an
/// implementation must verify it before deriving an identifier from it.
pub struct ClientResolveInput<'a> {
    /// The raw body the client posted to the resolve endpoint. For a vendor
    /// provider this is its own JSON envelope; for the built-in
    /// [`ClientFixedProvider`] demo it is the fixed known word the page script
    /// posts.
    pub payload: &'a [u8],

    /// The permissions resolved for the resolve request. The endpoint has
    /// already confirmed the provider's required permissions are set, so a
    /// provider reads this only for behavior beyond gating.
    pub permissions: Option<&'a PermissionState>,

    /// The resolve request's consent context, for provider-specific logic. The
    /// core gates on permissions, not consent.
    pub consent: Option<&'a ConsentContext>,
}

/// The outcome of [`EdgeCookieProvider::generate`].
///
/// Carries the derived identifier, if any, and any response headers the provider
/// needs set on the outbound response.
#[derive(Debug, Default)]
pub struct GeneratedEdgeCookie {
    /// The derived Edge Cookie identifier, or `None` when the provider produced
    /// none for this request.
    pub id: Option<String>,

    /// Response headers the provider needs set on the outbound response, for
    /// example to request additional client evidence on later requests. Empty
    /// for providers that set no headers, such as [`HmacProvider`].
    pub response_headers: Vec<(http::HeaderName, http::HeaderValue)>,
}

/// A strategy for deriving an Edge Cookie identifier.
///
/// Implementations are selected by configuration and come in two types, which
/// reach the same outcome (a `ts-ec` cookie) by different routes:
///
/// - **Server-side** (for example [`HmacProvider`]): derives the identifier at
///   the edge in [`generate`](Self::generate), and the page response sets the
///   cookie. Nothing client-side is involved.
/// - **Client-side** (for example [`ClientFixedProvider`]): defers in
///   [`generate`](Self::generate) (returns `id: None`), runs its own JavaScript
///   in the browser, and mints from the value the page posts back in
///   [`resolve_from_client`](Self::resolve_from_client), whose response sets the
///   cookie.
///
/// A provider returns `Ok(None)` from [`generate`](Self::generate) when it
/// cannot derive an identifier at the edge, so the request proceeds without an
/// Edge Cookie rather than failing.
pub trait EdgeCookieProvider: Send + Sync {
    /// Returns the stable identifier for this provider, used in configuration
    /// and logs.
    fn id(&self) -> &'static str;

    /// Derives an Edge Cookie identifier from the provider's injected services
    /// and the request's gating context.
    ///
    /// A server-side provider mints here. A client-side provider defers here
    /// (returns `id: None`) and mints later in
    /// [`resolve_from_client`](Self::resolve_from_client) from the value the page
    /// posts back.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::EdgeCookie`] when derivation fails.
    fn generate(
        &self,
        request_info: &dyn RequestInfo,
        input: &IdentityInput<'_>,
    ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>>;

    /// Returns whether two identifiers produced by this provider denote the same
    /// identity.
    ///
    /// Edge Cookie identifiers must not be assumed comparable by natural string
    /// equality. A provider whose identifiers can carry the same payload in
    /// different wrappers (for example a signed envelope that is re-issued with a
    /// new timestamp or signature) overrides this to compare by payload, so the
    /// system asks the provider rather than comparing the raw strings.
    ///
    /// The default compares the values for byte equality, which is correct for
    /// providers whose identifiers are canonical, such as [`HmacProvider`].
    fn keys_equal(&self, left: &str, right: &str) -> bool {
        left == right
    }

    /// The permissions this provider's data use requires.
    ///
    /// Trusted Server executes the provider only when every permission returned
    /// here is set. The default is empty, so a vendor-neutral provider requires
    /// no permission. A provider that stores identity on the device, or shares it
    /// onward, declares the matching permission so the request's country and
    /// signal rules can gate it.
    fn required_permissions(&self) -> PermissionSet {
        PermissionSet::none()
    }

    /// Derives an Edge Cookie identifier from a value the client produced and
    /// posted to the resolve endpoint (`POST /_ts/api/v1/ec/resolve`).
    ///
    /// This is the client-side counterpart to [`generate`](Self::generate). A
    /// provider that cannot derive an identifier at the edge defers from
    /// `generate` (returning `id: None`, optionally with response headers that
    /// trigger client-side work), and the page posts its result back here. The
    /// payload arrives from the browser, so an implementation MUST verify it
    /// (for example checking a signature) before trusting it. The default
    /// returns no identifier, so a provider that mints entirely server-side
    /// (such as [`HmacProvider`]) need not implement it.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::EdgeCookie`] when processing the payload
    /// fails. A payload that is merely unverified or absent yields `id: None`
    /// rather than an error, so the request proceeds without an Edge Cookie.
    fn resolve_from_client(
        &self,
        _input: &ClientResolveInput<'_>,
    ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
        Ok(GeneratedEdgeCookie::default())
    }
}

/// The built-in HMAC Edge Cookie provider.
///
/// Derives the identifier from the client IP (read from the [`RequestInfo`]
/// passed at call time) and the configured passphrase via
/// [`generation::generate_ec_id`].
#[derive(Debug, Clone)]
pub struct HmacProvider {
    passphrase: Redacted<String>,
}

impl HmacProvider {
    /// Creates an HMAC provider with the given passphrase.
    #[must_use]
    pub fn new(passphrase: Redacted<String>) -> Self {
        Self { passphrase }
    }
}

impl EdgeCookieProvider for HmacProvider {
    fn id(&self) -> &'static str {
        "hmac"
    }

    fn generate(
        &self,
        request_info: &dyn RequestInfo,
        _input: &IdentityInput<'_>,
    ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
        let id = generation::generate_ec_id(self.passphrase.expose(), request_info.client_ip())?;
        Ok(GeneratedEdgeCookie {
            id: Some(id),
            response_headers: Vec::new(),
        })
    }

    fn required_permissions(&self) -> PermissionSet {
        // The HMAC provider writes the Edge Cookie to the device, so it requires
        // permission to store on the device (TCF Purpose 1). Whether that needs a
        // signal is decided by the country rules, not by the provider.
        PermissionSet::none().with(Permission::StoreOnDevice)
    }
}

/// The built-in host-signal Edge Cookie provider.
///
/// Derives the identifier from the host fingerprints (TLS JA4 and HTTP/2, read
/// from the injected [`HostSignals`]) plus the client IP (from [`RequestInfo`]),
/// keyed by the configured passphrase. It is host-agnostic: it depends on the
/// `HostSignals` capability, so any host that supplies one can use it. A host
/// that supplies no `HostSignals` cannot build it, and the request stops.
#[derive(Debug, Clone)]
pub struct HostSignalProvider {
    passphrase: Redacted<String>,
    host_signals: Arc<dyn HostSignals>,
}

impl HostSignalProvider {
    /// Creates the provider with the passphrase and its injected host signals.
    #[must_use]
    pub fn new(passphrase: Redacted<String>, host_signals: Arc<dyn HostSignals>) -> Self {
        Self {
            passphrase,
            host_signals,
        }
    }
}

impl EdgeCookieProvider for HostSignalProvider {
    fn id(&self) -> &'static str {
        "host-signals"
    }

    fn generate(
        &self,
        request_info: &dyn RequestInfo,
        _input: &IdentityInput<'_>,
    ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
        let ja4 = self.host_signals.ja4().unwrap_or_default();
        let h2 = self.host_signals.h2().unwrap_or_default();
        let id = generation::generate_hmac_ec_id(
            self.passphrase.expose(),
            &[ja4, h2, request_info.client_ip()],
        )?;
        Ok(GeneratedEdgeCookie {
            id: Some(id),
            response_headers: Vec::new(),
        })
    }

    fn required_permissions(&self) -> PermissionSet {
        // Writes the Edge Cookie to the device, so it requires store-on-device
        // (TCF Purpose 1), the same gate as the HMAC provider.
        PermissionSet::none().with(Permission::StoreOnDevice)
    }
}

/// The fixed, known word shared by [`ClientFixedProvider`] and its page script.
///
/// Kept cookie-safe (no characters [`set_provider_ec_cookie`] would reject) so
/// it can be used as the Edge Cookie value verbatim. The page script posts this
/// exact string; the provider mints only when the posted value matches. The
/// client copy lives in
/// `crates/trusted-server-js/lib/src/integrations/ec_client_fixed`.
///
/// [`set_provider_ec_cookie`]: super::cookies::set_provider_ec_cookie
const EXPECTED_VALUE: &str = "an-ec";

/// A demonstration client-side provider, with no vendor coupling.
///
/// Client and server share one fixed, known word (`EXPECTED_VALUE`). When no
/// Edge Cookie is present the page script (delivered through the tsjs bundle)
/// posts that word to `POST /_ts/api/v1/ec/resolve`, and this provider mints the
/// Edge Cookie only when the posted value matches. It defers from
/// [`generate`](EdgeCookieProvider::generate) so the page renders with no Edge
/// Cookie until the client reports back, then verifies and mints in
/// [`resolve_from_client`](EdgeCookieProvider::resolve_from_client).
///
/// The value is verifiable precisely because it is a known constant, which is
/// the point of the demo: it exercises verify-before-mint. It is useless in
/// production, because a fixed value is not an identity and every client posts
/// the same word, so it is for demonstration and testing only. A real
/// client-side provider verifies a real payload (for example an OWID signature)
/// instead of a shared constant.
#[derive(Debug, Clone)]
pub struct ClientFixedProvider;

impl EdgeCookieProvider for ClientFixedProvider {
    fn id(&self) -> &'static str {
        "client-fixed"
    }

    fn generate(
        &self,
        _request_info: &dyn RequestInfo,
        _input: &IdentityInput<'_>,
    ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
        // No identifier is derived at the edge: the value comes from the page
        // script, which posts it to the resolve endpoint.
        Ok(GeneratedEdgeCookie::default())
    }

    fn resolve_from_client(
        &self,
        input: &ClientResolveInput<'_>,
    ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
        // Verify the posted value against the known shared word, then mint it as
        // the Edge Cookie. A value that does not match yields no Edge Cookie.
        // This stands in for a real provider's verification (for example
        // checking a signature) before it trusts a client-supplied value.
        let matches = core::str::from_utf8(input.payload)
            .map(str::trim)
            .is_ok_and(|value| value == EXPECTED_VALUE);

        Ok(GeneratedEdgeCookie {
            id: matches.then(|| EXPECTED_VALUE.to_owned()),
            response_headers: Vec::new(),
        })
    }

    fn required_permissions(&self) -> PermissionSet {
        // The provider writes the resolved value to the device as the Edge
        // Cookie, so it requires store-on-device (TCF Purpose 1), the same gate
        // as the HMAC provider.
        PermissionSet::none().with(Permission::StoreOnDevice)
    }
}

/// Builds the Edge Cookie provider named by the `[ec] provider` selector,
/// injecting the services it needs.
///
/// This is the composition root for the built-in providers: the adapter supplies
/// the [`HostSignals`] when the host can produce them, and this constructs the
/// selected provider. The per-request [`RequestInfo`] is passed borrowed to
/// [`generate`](EdgeCookieProvider::generate) at call time rather than stored, so
/// no request snapshot is cloned here. Returns `Ok(None)` when no provider is
/// selected, so the caller stays stateless.
///
/// # Errors
///
/// Returns [`TrustedServerError::EdgeCookie`] when the selected provider requires
/// a service the host did not supply (for example the host-signal provider on a
/// host that exposes no [`HostSignals`]), so a misconfigured deployment fails
/// loudly rather than minting a degraded identifier.
pub fn build_provider(
    ec: &Ec,
    host_signals: Option<Arc<dyn HostSignals>>,
) -> Result<Option<Box<dyn EdgeCookieProvider>>, Report<TrustedServerError>> {
    let Some(key) = ec.provider.as_deref() else {
        return Ok(None);
    };
    let provider: Option<Box<dyn EdgeCookieProvider>> = match key {
        "hmac" => ec
            .providers
            .hmac
            .as_ref()
            .map(|config| Box::new(HmacProvider::new(config.passphrase.clone())) as _),
        // The client-fixed demo provider takes no configuration or services, so
        // it is built whenever it is selected. For demonstration and testing
        // only (see [`ClientFixedProvider`]).
        "client-fixed" => Some(Box::new(ClientFixedProvider) as _),
        "host-signals" => {
            let signals = host_signals.ok_or_else(|| {
                Report::new(TrustedServerError::EdgeCookie {
                    message: "The host-signals Edge Cookie provider requires a host that supplies \
                              TLS/HTTP-2 fingerprints, which this host does not"
                        .to_owned(),
                })
            })?;
            ec.providers.host_signals.as_ref().map(|config| {
                Box::new(HostSignalProvider::new(config.passphrase.clone(), signals)) as _
            })
        }
        _ => None,
    };
    Ok(provider)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::OwnedRequestInfo;
    use crate::permissions::PermissionMaps;
    use crate::redacted::Redacted;

    fn test_passphrase() -> Redacted<String> {
        Redacted::from("a-test-passphrase-32-bytes-minimum".to_owned())
    }

    fn test_request_info() -> OwnedRequestInfo {
        OwnedRequestInfo::new("203.0.113.1".to_owned(), http::HeaderMap::new())
    }

    /// Test host signals with fixed JA4/H2 values.
    #[derive(Debug)]
    struct TestHostSignals {
        ja4: Option<String>,
        h2: Option<String>,
    }

    impl HostSignals for TestHostSignals {
        fn ja4(&self) -> Option<&str> {
            self.ja4.as_deref()
        }
        fn h2(&self) -> Option<&str> {
            self.h2.as_deref()
        }
    }

    /// A provider whose identifiers wrap a payload after a `:` separator, so two
    /// different wrappers of the same payload denote the same identity. Stands in
    /// for an envelope-based vendor identifier.
    #[derive(Debug)]
    struct WrappedPayloadProvider;

    impl EdgeCookieProvider for WrappedPayloadProvider {
        fn id(&self) -> &'static str {
            "wrapped"
        }

        fn generate(
            &self,
            _request_info: &dyn RequestInfo,
            _input: &IdentityInput<'_>,
        ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
            Ok(GeneratedEdgeCookie::default())
        }

        fn keys_equal(&self, left: &str, right: &str) -> bool {
            fn payload(value: &str) -> &str {
                value.split_once(':').map_or(value, |(_, payload)| payload)
            }
            payload(left) == payload(right)
        }
    }

    #[test]
    fn hmac_keys_equal_uses_natural_equality() {
        let provider = HmacProvider::new(test_passphrase());
        assert!(
            provider.keys_equal("abcd.efghij", "abcd.efghij"),
            "identical HMAC keys should be equal"
        );
        assert!(
            !provider.keys_equal("abcd.efghij", "abcd.klmnop"),
            "different HMAC keys should not be equal"
        );
    }

    #[test]
    fn keys_equal_can_compare_by_payload_ignoring_the_wrapper() {
        let provider = WrappedPayloadProvider;
        assert!(
            provider.keys_equal("wrapper-1:same-payload", "wrapper-2:same-payload"),
            "different wrappers of the same payload should be equal"
        );
        assert!(
            !provider.keys_equal("wrapper-1:payload-a", "wrapper-1:payload-b"),
            "different payloads should not be equal"
        );
    }

    #[test]
    fn hmac_provider_requires_store_on_device() {
        let provider = HmacProvider::new(test_passphrase());
        let required = provider.required_permissions();
        assert!(
            required.contains(Permission::StoreOnDevice),
            "the HMAC provider writes a cookie, so it requires store-on-device"
        );
        assert!(
            !required.contains(Permission::SelectPersonalisedAds),
            "the HMAC provider requires no advertising permissions"
        );
    }

    #[test]
    fn host_signal_provider_mints_from_fingerprints_and_requires_store_on_device() {
        let signals = Arc::new(TestHostSignals {
            ja4: Some("t13d1516h2_8daaf6152771_e5627efa2ab1".to_owned()),
            h2: Some("1:65536;4:6291456".to_owned()),
        });
        let provider = HostSignalProvider::new(test_passphrase(), signals);
        let request_info = test_request_info();
        let generated = provider
            .generate(&request_info, &IdentityInput::default())
            .expect("should generate");
        assert!(
            generated.id.is_some(),
            "the host-signal provider mints an identifier from the fingerprints"
        );
        assert!(
            provider
                .required_permissions()
                .contains(Permission::StoreOnDevice),
            "the host-signal provider writes a cookie, so it requires store-on-device"
        );
    }

    #[test]
    fn a_neutral_provider_requires_no_permissions_by_default() {
        // The wrapped-payload stub does not override required_permissions, so it
        // inherits the trait default of none and requires no permission.
        assert!(
            WrappedPayloadProvider.required_permissions().is_empty(),
            "a vendor-neutral provider requires nothing by default"
        );
    }

    #[test]
    fn the_edge_cookie_gate_blocks_until_the_permission_is_set() {
        let required = HmacProvider::new(test_passphrase()).required_permissions();
        // Empty maps with no default: every permission is the requires-signal
        // floor.
        let maps = PermissionMaps::empty();

        // No signal: the provider's required permission is not set, so Trusted
        // Server would not commit the Edge Cookie.
        assert!(
            !maps.resolve(None, None, |_| false).all_set(required),
            "the floor should not run the Edge Cookie provider without the permission set"
        );

        // A grant signal for store-on-device: the provider's permission is now set.
        assert!(
            maps.resolve(None, None, |p| p == Permission::StoreOnDevice)
                .all_set(required),
            "the Edge Cookie provider runs once store-on-device is set"
        );
    }

    #[test]
    fn client_fixed_defers_in_generate() {
        let request_info = test_request_info();
        let generated = ClientFixedProvider
            .generate(&request_info, &IdentityInput::default())
            .expect("should generate");
        assert!(
            generated.id.is_none(),
            "client-fixed should defer in generate, deriving no edge identifier"
        );
    }

    #[test]
    fn client_fixed_mints_when_posted_word_matches() {
        let input = ClientResolveInput {
            payload: EXPECTED_VALUE.as_bytes(),
            permissions: None,
            consent: None,
        };
        let generated = ClientFixedProvider
            .resolve_from_client(&input)
            .expect("should resolve");
        assert_eq!(
            generated.id.as_deref(),
            Some(EXPECTED_VALUE),
            "the known shared word should verify and mint the Edge Cookie"
        );
    }

    #[test]
    fn client_fixed_rejects_unknown_word() {
        let input = ClientResolveInput {
            payload: b"not-the-word",
            permissions: None,
            consent: None,
        };
        let generated = ClientFixedProvider
            .resolve_from_client(&input)
            .expect("should resolve");
        assert!(
            generated.id.is_none(),
            "a value that does not match the known word should mint no Edge Cookie"
        );
    }

    #[test]
    fn client_fixed_requires_store_on_device() {
        assert!(
            ClientFixedProvider
                .required_permissions()
                .contains(Permission::StoreOnDevice),
            "client-fixed writes a cookie, so it requires store-on-device"
        );
    }

    #[test]
    fn server_side_provider_inherits_no_op_resolve_from_client() {
        // HmacProvider does not override resolve_from_client, so it inherits the
        // no-op default: a server-side provider does not participate in the
        // client cycle.
        let provider = HmacProvider::new(test_passphrase());
        let input = ClientResolveInput {
            payload: b"anything",
            permissions: None,
            consent: None,
        };
        let generated = provider
            .resolve_from_client(&input)
            .expect("should resolve");
        assert!(
            generated.id.is_none(),
            "a server-side provider inherits the no-op resolve_from_client default"
        );
    }
}
