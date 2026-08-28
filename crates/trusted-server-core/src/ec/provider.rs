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
/// The registered short code that namespaces one Edge Cookie provider's
/// identifiers.
///
/// Exactly four characters from `[a-z0-9]`, allocated append-only in
/// `docs/superpowers/specs/provider-code-registry.md` and never reused. The
/// code appears as the `{code}~` prefix of every identifier the provider
/// mints, so identifiers from different providers can never collide in the
/// cookie, the identity graph, or a withdrawal, and each identifier records
/// which provider created it.
#[derive(Debug, Copy, Clone, Eq, Hash, PartialEq, derive_more::Display)]
pub struct ProviderCode(&'static str);

impl ProviderCode {
    /// Creates a provider code, validating the registry format.
    ///
    /// # Panics
    ///
    /// Panics when `code` is not exactly four characters of `[a-z0-9]`. Codes
    /// are compile-time literals, so the panic fires in tests and never on a
    /// request path.
    #[must_use]
    pub const fn new(code: &'static str) -> Self {
        let bytes = code.as_bytes();
        assert!(
            bytes.len() == 4,
            "provider code must be exactly four characters"
        );
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            assert!(
                b.is_ascii_lowercase() || b.is_ascii_digit(),
                "provider code characters must be [a-z0-9]"
            );
            i += 1;
        }
        Self(code)
    }

    /// The code as a string slice.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

/// The separator between a provider code and the provider's identifier value.
///
/// The tilde is inside the cookie-safe identifier alphabet and outside the
/// built-in HMAC identifier's own characters, so a legacy bare identifier can
/// never be misread as a coded one.
pub const PROVIDER_CODE_SEPARATOR: char = '~';

/// Splits a full identifier into its provider-code prefix and value.
///
/// Returns `(Some(code), value)` when the identifier starts with a well-formed
/// `{code}~` prefix, and `(None, full)` for a legacy bare identifier. The code
/// here is the raw string, not a validated [`ProviderCode`]: an unknown code
/// simply fails the ownership check against the selected provider.
#[must_use]
pub fn split_provider_code(full: &str) -> (Option<&str>, &str) {
    if let Some((code, value)) = full.split_once(PROVIDER_CODE_SEPARATOR)
        && code.len() == 4
        && code
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    {
        return (Some(code), value);
    }
    (None, full)
}

/// Whether the selected provider owns `full` as one of its identifiers.
///
/// A coded identifier belongs to the provider whose registered code it
/// carries, with the value part accepted by that provider's
/// [`accepts_id`](EdgeCookieProvider::accepts_id). A legacy bare identifier
/// (no code prefix) belongs only to the built-in HMAC provider, which
/// dual-reads its pre-envelope form for one release cycle so deployed cookies
/// keep working across the migration.
#[must_use]
pub fn provider_owns_id(provider: &dyn EdgeCookieProvider, full: &str) -> bool {
    match split_provider_code(full) {
        (Some(code), value) => code == provider.code().as_str() && provider.accepts_id(value),
        (None, value) => provider.id() == "hmac" && provider.accepts_id(value),
    }
}

/// The full minted identifier for `value` under `provider`'s code.
#[must_use]
pub fn apply_provider_code(provider: &dyn EdgeCookieProvider, value: &str) -> String {
    format!("{}{PROVIDER_CODE_SEPARATOR}{value}", provider.code())
}

/// The KV-key form of a full identifier under `provider`.
///
/// The code prefix is preserved verbatim and the provider normalizes only its
/// own value part, so distinct providers' rows can never share a key and a
/// provider never sees another provider's syntax.
#[must_use]
pub fn provider_kv_key(provider: &dyn EdgeCookieProvider, full: &str) -> String {
    match split_provider_code(full) {
        (Some(code), value) => format!(
            "{code}{PROVIDER_CODE_SEPARATOR}{}",
            provider.normalize_id_for_kv(value)
        ),
        (None, value) => provider.normalize_id_for_kv(value),
    }
}

pub trait EdgeCookieProvider: Send + Sync + core::fmt::Debug {
    /// Returns the stable identifier for this provider, used in configuration
    /// and logs.
    fn id(&self) -> &'static str;

    /// The provider's registered code, the `{code}~` namespace of every
    /// identifier it mints.
    ///
    /// Mandatory, with no default: a provider must allocate a unique code in
    /// `docs/superpowers/specs/provider-code-registry.md` before it can exist,
    /// so no two providers can ever mint colliding identifiers. Core applies
    /// the code at mint and checks it at read-back, and the provider itself
    /// only ever sees its own value part.
    fn code(&self) -> ProviderCode;

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

    /// Returns whether `value` is a well-formed identifier this provider issues.
    ///
    /// Core calls this to decide whether an incoming `ts-ec` cookie value is a
    /// usable Edge Cookie identifier before reading it back, keying the KV
    /// identity graph, or withdrawing it. Core strips the provider's `{code}~`
    /// prefix first, so this receives only the provider's own value part.
    /// This keeps the identifier opaque to
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

    fn code(&self) -> ProviderCode {
        ProviderCode::new("hmac")
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
        // Explicit statelessness: the same meaning as omitting the selector.
        "none" => None,
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

/// Checks once, at startup, that this deployment can build the provider named
/// by the `[ec] provider` selector.
///
/// The composition root calls this while it builds application state, passing
/// the same injected provider it will put into
/// [`RuntimeServices`](crate::platform::RuntimeServices) on every request.
/// [`build_provider`] reads no request data, so the answer is the same for
/// every request and a selection the adapter can never supply fails at startup
/// rather than on the first request. A stateless deployment (no selector, or
/// `"none"`) passes.
///
/// # Errors
///
/// Returns [`TrustedServerError::EdgeCookie`] when the selected provider cannot
/// be built from the services this deployment injects.
pub fn ensure_provider_available(
    ec: &Ec,
    injected: Option<Arc<dyn EdgeCookieProvider>>,
) -> Result<(), Report<TrustedServerError>> {
    build_provider(ec, injected)?;
    Ok(())
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
    fn code(&self) -> ProviderCode {
        self.0.code()
    }

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

    #[test]
    fn split_provider_code_separates_coded_and_legacy_forms() {
        assert_eq!(
            split_provider_code("hmac~abc.DEF123"),
            (Some("hmac"), "abc.DEF123"),
            "a four-character code before the first tilde splits off"
        );
        assert_eq!(
            split_provider_code("51dd~value~with~tildes"),
            (Some("51dd"), "value~with~tildes"),
            "only the first tilde splits, so a value may contain tildes"
        );
        assert_eq!(
            split_provider_code("abcdef.XYZ"),
            (None, "abcdef.XYZ"),
            "no tilde means the legacy bare form"
        );
        assert_eq!(
            split_provider_code("toolong~x"),
            (None, "toolong~x"),
            "a prefix that is not exactly four characters is not a code"
        );
        assert_eq!(
            split_provider_code("AB12~x"),
            (None, "AB12~x"),
            "uppercase is outside the code alphabet"
        );
    }

    #[test]
    fn provider_ownership_follows_the_code() {
        let provider = HmacProvider::new(test_passphrase());
        let legacy = format!("{}.ABC123", "a".repeat(64));
        let coded = format!("hmac~{legacy}");
        let foreign = format!("zz00~{legacy}");
        assert!(
            provider_owns_id(&provider, &coded),
            "the provider owns identifiers carrying its own code"
        );
        assert!(
            provider_owns_id(&provider, &legacy),
            "the built-in hmac provider dual-reads the legacy bare form"
        );
        assert!(
            !provider_owns_id(&provider, &foreign),
            "an identifier with another provider's code is never owned"
        );
    }
    use crate::redacted::Redacted;

    fn test_passphrase() -> Redacted<String> {
        Redacted::from("a-test-passphrase-32-bytes-minimum".to_owned())
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

            fn code(&self) -> ProviderCode {
                ProviderCode::new("t0in")
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

    #[test]
    fn the_startup_check_rejects_an_uninjected_provider_and_allows_statelessness() {
        // A selection the adapter cannot supply is knowable without a request,
        // so the composition root rejects it while application state is built.
        let selected = Ec {
            provider: Some("acme".to_owned()),
            ..Ec::default()
        };
        let err = ensure_provider_available(&selected, None)
            .expect_err("an uninjected provider should fail the startup check");
        assert!(
            err.to_string().contains("acme"),
            "the error should name the selected provider, got: {err}"
        );

        // Statelessness is a supported deployment, spelled either way, and must
        // never be turned into a startup error.
        ensure_provider_available(&Ec::default(), None)
            .expect("should allow a deployment that selects no provider");
        let explicit_none = Ec {
            provider: Some("none".to_owned()),
            ..Ec::default()
        };
        ensure_provider_available(&explicit_none, None)
            .expect("should allow the explicit `none` selection");
    }
}
