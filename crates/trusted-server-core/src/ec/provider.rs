//! Edge Cookie identity providers.
//!
//! An [`EdgeCookieProvider`] derives an Edge Cookie identifier. Providers are
//! wired by dependency injection: a provider's constructor takes the services it
//! needs (for example [`RequestInfo`] for the client IP)
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
use crate::evidence::RequestInfo;
use crate::redacted::Redacted;
use crate::settings::Ec;

use super::generation;

/// The request-scoped gating context passed to [`EdgeCookieProvider::generate`].
///
/// Request data (client IP, User-Agent, headers, host signals) reaches a
/// provider through the services injected into its constructor, not through this
/// struct. This carries only the per-request gating context a provider may read
/// for behavior beyond gating. The gate has already confirmed Edge Cookie
/// storage is allowed before `generate` is called.
#[derive(Default)]
pub struct IdentityInput<'a> {
    /// The request's consent context, when available, for provider-specific
    /// logic. The core gates generation before calling the provider, so a
    /// provider reads this only to forward or record consent. [`HmacProvider`]
    /// ignores it.
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
/// Implementations are selected by configuration. A provider derives the
/// identifier at the edge in [`generate`](Self::generate), and the page
/// response sets the `ts-ec` cookie.
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
}

/// Builds the Edge Cookie provider named by the `[ec] provider` selector,
/// injecting the services it needs.
///
/// This is the composition root for the built-in providers. The per-request
/// [`RequestInfo`] is passed borrowed to
/// [`generate`](EdgeCookieProvider::generate) at call time rather than stored, so
/// no request snapshot is cloned here. Returns `Ok(None)` when no provider is
/// selected, so the caller stays stateless.
///
/// # Errors
///
/// None of the built-in constructions fail today. The `Result` is the seam for
/// a provider whose construction can fail (for example one requiring a host
/// service the deployment does not supply), so such a misconfiguration fails
/// loudly rather than minting a degraded identifier.
pub fn build_provider(
    ec: &Ec,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redacted::Redacted;

    fn test_passphrase() -> Redacted<String> {
        Redacted::from("a-test-passphrase-32-bytes-minimum".to_owned())
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
    fn a_selected_but_uninjected_vendor_provider_fails_loudly() {
        let ec = Ec {
            provider: Some("acme".to_owned()),
            ..Ec::default()
        };

        let err = build_provider(&ec, None)
            .expect_err("selecting a provider the adapter does not inject should error");
        assert!(
            err.to_string().contains("acme"),
            "the error should name the selected provider, got: {err}"
        );
    }
}
