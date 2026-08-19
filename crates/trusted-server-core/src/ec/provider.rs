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
///
/// A provider returns `Ok(None)` from [`generate`](Self::generate) when it
/// cannot derive an identifier at the edge, so the request proceeds without an
/// Edge Cookie rather than failing.
pub trait EdgeCookieProvider: Send + Sync + core::fmt::Debug {
    /// Returns the stable identifier for this provider, used in configuration
    /// and logs.
    fn id(&self) -> &'static str;

    /// Derives an Edge Cookie identifier from the provider's injected services
    /// and the request's gating context.
    ///
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

    /// Returns whether `value` is a well-formed identifier this provider issues.
    ///
    /// Core calls this to decide whether an incoming `ts-ec` cookie value is a
    /// usable Edge Cookie identifier before reading it back, keying the KV
    /// identity graph, or withdrawing it. This keeps the identifier opaque to
    /// core: a provider whose identifiers are not the built-in shape (for
    /// example an opaque signed envelope) accepts its own format here, so its
    /// identifier round-trips instead of being silently dropped on read-back.
    ///
    /// The default accepts the built-in HMAC identifier shape
    /// (`<64 hex>.<6 alphanumeric>`), which is correct for [`HmacProvider`] and
    /// the other core providers.
    fn accepts_id(&self, value: &str) -> bool {
        generation::is_valid_ec_id(value)
    }

    /// Returns the KV-key form of `value` for this provider's identifiers.
    ///
    /// Core keys the identity graph by the returned string, so a provider whose
    /// identifiers are case-sensitive or carry no separable segments returns the
    /// value unchanged to avoid collapsing distinct identifiers into one key.
    ///
    /// The default lowercases the leading HMAC hash segment and preserves the
    /// suffix, matching the built-in identifier shape.
    fn normalize_id_for_kv(&self, value: &str) -> String {
        generation::normalize_ec_id_for_kv(value)
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
        // With no fingerprint at all, minting would silently degrade to an
        // IP-only identifier under the host-signals name. Defer instead: no
        // identity this request, and the request proceeds.
        if ja4.is_empty() && h2.is_empty() {
            log::warn!("Host-signal EC provider found no TLS/HTTP-2 fingerprints; deferring");
            return Ok(GeneratedEdgeCookie::default());
        }
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
        // Writes the Edge Cookie to the device, so it requires necessary.operations.storage
        // (TCF Purpose 1), the same gate as the HMAC provider.
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
    injected: Option<Arc<dyn EdgeCookieProvider>>,
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
        // Any other key names a vendor or host provider the adapter injects
        // through [`RuntimeServices`](crate::platform::RuntimeServices), the same
        // seam the device and geo providers use, so core never names a vendor.
        // The injected provider is used when its own id matches the selected key,
        // and its `[ec.providers.<key>]` block is read by the adapter that built
        // it. A selected key with no matching injected provider is a deployment
        // error: fail loudly rather than silently running stateless.
        other => {
            let provider = injected
                .filter(|provider| provider.id() == other)
                .map(|provider| Box::new(SharedProvider(provider)) as _);
            if provider.is_none() {
                return Err(Report::new(TrustedServerError::EdgeCookie {
                    message: format!(
                        "Edge Cookie provider `{other}` is selected but this deployment's \
                         adapter does not provide it"
                    ),
                }));
            }
            provider
        }
    };
    Ok(provider)
}

/// Adapts an injected, shared [`EdgeCookieProvider`] to the owned `Box` that
/// [`build_provider`] returns.
///
/// A vendor or host provider is injected as an `Arc` so it can live in
/// [`RuntimeServices`](crate::platform::RuntimeServices) and be cloned per
/// request. Every method delegates to the inner provider, so its behavior is
/// unchanged.
#[derive(Debug)]
struct SharedProvider(Arc<dyn EdgeCookieProvider>);

impl EdgeCookieProvider for SharedProvider {
    fn id(&self) -> &'static str {
        self.0.id()
    }

    fn generate(
        &self,
        request_info: &dyn RequestInfo,
        input: &IdentityInput<'_>,
    ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
        self.0.generate(request_info, input)
    }

    fn keys_equal(&self, left: &str, right: &str) -> bool {
        self.0.keys_equal(left, right)
    }

    fn accepts_id(&self, value: &str) -> bool {
        self.0.accepts_id(value)
    }

    fn normalize_id_for_kv(&self, value: &str) -> String {
        self.0.normalize_id_for_kv(value)
    }

    fn required_permissions(&self) -> PermissionSet {
        self.0.required_permissions()
    }
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
    fn default_id_semantics_match_the_builtin_shape() {
        let provider = HmacProvider::new(test_passphrase());

        // The default `accepts_id` accepts the built-in HMAC shape and rejects
        // anything else, so a built-in provider's identifiers round-trip while an
        // opaque value is left to a provider that overrides the check.
        let valid = format!("{}.{}", "a".repeat(64), "abc123");
        assert!(provider.accepts_id(&valid), "should accept the HMAC shape");
        assert!(
            !provider.accepts_id("not-hmac-shaped"),
            "should reject a non-HMAC identifier by default"
        );

        // The default `normalize_id_for_kv` lowercases the hash segment. This is
        // exactly the transform that would corrupt an opaque case-sensitive
        // identifier, which is why such a provider overrides it.
        let mixed = format!("{}.{}", "A".repeat(64), "abc123");
        assert_eq!(
            provider.normalize_id_for_kv(&mixed),
            format!("{}.{}", "a".repeat(64), "abc123"),
            "the default should lowercase the hash segment"
        );
    }

    #[test]
    fn shared_provider_delegates_id_semantics_to_the_inner_provider() {
        // `SharedProvider` wraps an adapter-injected provider. It must forward
        // every trait method to the inner provider, including `accepts_id` and
        // `normalize_id_for_kv`; a wrapper that silently used the defaults would
        // drop an opaque vendor identifier on read-back. This guards that
        // delegation directly.
        #[derive(Debug)]
        struct Inner;

        impl EdgeCookieProvider for Inner {
            fn id(&self) -> &'static str {
                "inner"
            }

            fn generate(
                &self,
                _request_info: &dyn RequestInfo,
                _input: &IdentityInput<'_>,
            ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
                Ok(GeneratedEdgeCookie::default())
            }

            fn accepts_id(&self, value: &str) -> bool {
                value == "opaque-ok"
            }

            fn normalize_id_for_kv(&self, value: &str) -> String {
                format!("kv:{value}")
            }
        }

        let shared = SharedProvider(Arc::new(Inner));

        assert_eq!(shared.id(), "inner", "should delegate id");
        assert!(
            shared.accepts_id("opaque-ok"),
            "should delegate accepts_id acceptance to the inner provider"
        );
        assert!(
            !shared.accepts_id("something-else"),
            "should delegate accepts_id rejection to the inner provider"
        );
        assert_eq!(
            shared.normalize_id_for_kv("x"),
            "kv:x",
            "should delegate normalize_id_for_kv to the inner provider"
        );
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
            "the HMAC provider writes a cookie, so it requires necessary.operations.storage"
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
            "the host-signal provider writes a cookie, so it requires necessary.operations.storage"
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

        // A grant signal for necessary.operations.storage: the provider's permission is now set.
        assert!(
            maps.resolve(None, None, |p| p == Permission::StoreOnDevice)
                .all_set(required),
            "the Edge Cookie provider runs once necessary.operations.storage is set"
        );
    }

    #[test]
    fn a_selected_but_uninjected_vendor_provider_fails_loudly() {
        let ec = Ec {
            provider: Some("acme".to_owned()),
            ..Ec::default()
        };

        let err = build_provider(&ec, None, None)
            .expect_err("selecting a provider the adapter does not inject should error");
        assert!(
            err.to_string().contains("acme"),
            "the error should name the selected provider, got: {err}"
        );
    }

    #[test]
    fn host_signal_provider_defers_without_fingerprints() {
        let signals = Arc::new(TestHostSignals {
            ja4: None,
            h2: None,
        });
        let provider = HostSignalProvider::new(test_passphrase(), signals);
        let request_info = test_request_info();
        let generated = provider
            .generate(&request_info, &IdentityInput::default())
            .expect("should generate");
        assert!(
            generated.id.is_none(),
            "with no host fingerprints the provider should defer rather than              mint an IP-only identifier"
        );
    }
}
