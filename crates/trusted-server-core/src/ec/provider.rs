//! Edge Cookie identity providers.
//!
//! An [`EdgeCookieProvider`] derives an Edge Cookie identifier. The provider is
//! selected by configuration, with no default, and [`build_provider`] is the
//! composition root that builds the selected one. A built-in provider is
//! constructed from its `[ec.providers.<key>]` block, and a vendor provider is
//! taken from the adapter that injected it. Construction happens once, while
//! application state is built, and reads no request data, so a selection this
//! deployment cannot satisfy fails at startup rather than leaving it running
//! without an identity.
//!
//! Request evidence reaches a provider at call time rather than at
//! construction. [`EdgeCookieProvider::generate`] borrows a [`RequestInfo`],
//! which carries the normalized client IP, for the life of the call, alongside
//! an [`IdentityInput`] holding the request's gating context. A provider reads
//! what it needs and retains nothing, so no per-request snapshot is stored or
//! cloned. `RequestInfo` is the seam rather than a fixed parameter list, so a
//! provider needing further evidence gains a defaulted accessor for it in the
//! change that first reads it.
//!
//! [`HmacProvider`] is the built-in server-side implementation. It derives the
//! identifier from the client IP using HMAC over the configured passphrase, the
//! behavior Trusted Server has always shipped.

use std::sync::Arc;

use error_stack::Report;
use serde::{Deserialize, Serialize};

use crate::consent::ConsentContext;
use crate::error::TrustedServerError;
use crate::evidence::RequestInfo;
use crate::redacted::Redacted;
use crate::settings::Ec;

use super::cookies::ec_id_has_only_allowed_chars;
use super::generation;

/// The Edge Cookie identity provider a deployment has selected.
///
/// Deserialized from the `[ec] provider` string, and serialized back to the
/// same string, so the configuration surface is unchanged. Provider names are
/// open-ended (a vendor crate names its own), so every name other than the
/// explicit `"none"` becomes [`Named`](Self::Named) rather than a parse
/// failure, and whether the deployment can actually supply that provider is
/// decided by [`build_provider`].
///
/// No individual provider has a variant of its own, the one still built into
/// core included. Every provider is selected the same way, by name, so no
/// caller can be written around one provider being different, and moving the
/// built-in provider out into its own module changes nothing here.
///
/// This is the one place the selector is spelled. Everything that needs to ask
/// which provider is selected matches on this rather than comparing string
/// literals.
#[derive(Debug, Clone, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(from = "String", into = "String")]
pub enum EcProviderSelection {
    /// Explicit statelessness, spelled `"none"`. The same meaning as omitting
    /// the selector: no Edge Cookie is minted and no provider block may be
    /// configured.
    None,

    /// A provider selected by name, configured by the matching
    /// `[ec.providers.<name>]` block. [`build_provider`] resolves the name to
    /// an implementation, whether that implementation is built into core or
    /// injected by the adapter.
    Named(String),
}

impl EcProviderSelection {
    /// The configuration spelling of explicit statelessness.
    pub const NONE_KEY: &'static str = "none";

    /// The configuration key this selection is written as.
    #[must_use]
    pub fn key(&self) -> &str {
        match self {
            Self::None => Self::NONE_KEY,
            Self::Named(key) => key,
        }
    }
}

impl From<&str> for EcProviderSelection {
    fn from(key: &str) -> Self {
        match key {
            EcProviderSelection::NONE_KEY => Self::None,
            other => Self::Named(other.to_owned()),
        }
    }
}

impl From<String> for EcProviderSelection {
    fn from(key: String) -> Self {
        match key.as_str() {
            EcProviderSelection::NONE_KEY => Self::None,
            _ => Self::Named(key),
        }
    }
}

impl From<EcProviderSelection> for String {
    fn from(selection: EcProviderSelection) -> Self {
        match selection {
            EcProviderSelection::None => EcProviderSelection::NONE_KEY.to_owned(),
            EcProviderSelection::Named(key) => key,
        }
    }
}

/// The configuration name of the provider still built into core.
///
/// The name lives in the same open-ended namespace every vendor provider name
/// comes from, and nothing branches on it outside the resolution in
/// [`build_provider`]. It is also [`HmacProvider::id`]'s return value and
/// [`HMAC_PROVIDER_CODE`]'s text. It goes with that resolution arm when the
/// built-in provider becomes a module of its own.
pub const HMAC_PROVIDER_KEY: &str = "hmac";

/// The registry code of the built-in HMAC provider.
///
/// The same text as [`HMAC_PROVIDER_KEY`], but a different role: this is the
/// `{code}~` namespace stamped on every identifier the built-in provider
/// mints, and it is what [`generation`] matches when it decides whether an
/// enveloped identifier is one of its own.
pub const HMAC_PROVIDER_CODE: ProviderCode = ProviderCode::new(HMAC_PROVIDER_KEY);

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
    ///
    /// Core checks every header here against its own reserved response surface
    /// (see [`reserved_response_effect`]) before it is applied, so a provider
    /// may set its own cookies and headers but cannot reach into the surface
    /// core manages.
    pub response_headers: Vec<(http::HeaderName, http::HeaderValue)>,
}

/// The cookie-name namespace Trusted Server manages.
///
/// Every cookie core writes or reads as part of its own behavior is named
/// `ts-<something>` (`ts-ec` in [`COOKIE_TS_EC`](crate::constants::COOKIE_TS_EC),
/// `ts-eids` in [`COOKIE_TS_EIDS`](crate::constants::COOKIE_TS_EIDS), and
/// `ts-tester` in [`COOKIE_TS_TESTER`](crate::constants::COOKIE_TS_TESTER)), so
/// core defends the whole prefix rather than a list that a new managed cookie
/// would silently outgrow. `sharedId` is deliberately not reserved: core only
/// reads it, and it belongs to the page's own identity stack.
const MANAGED_COOKIE_NAME_PREFIX: &[u8] = b"ts-";

/// The response-header namespace Trusted Server reserves for itself.
///
/// Covers the fixed EC output headers and the per-partner
/// `x-ts-<source_domain>` headers, which is why the prefix is reserved rather
/// than the four names in
/// [`INTERNAL_HEADERS`](crate::constants::INTERNAL_HEADERS).
const RESERVED_RESPONSE_HEADER_PREFIX: &str = "x-ts-";

/// Response headers that frame an HTTP message or are hop-by-hop.
///
/// The hop-by-hop set is RFC 7230 §6.1, matching each adapter's
/// `is_hop_by_hop_response_header`, plus `content-length`, which frames the
/// body the adapter is about to write. A provider that set any of these would
/// be rewriting the response envelope rather than adding evidence to it.
const FRAMING_OR_HOP_BY_HOP_HEADERS: &[&str] = &[
    "connection",
    "content-length",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Why one provider response header falls inside core's reserved surface.
#[derive(Debug, Copy, Clone, Eq, PartialEq, derive_more::Display)]
pub enum ReservedResponseEffect {
    /// A `Set-Cookie` naming a cookie in the `ts-` namespace core manages.
    #[display("sets a cookie in the `ts-` namespace Trusted Server manages")]
    ManagedCookie,

    /// A header in the `x-ts-` namespace core emits and strips.
    #[display("sets a header in the reserved `x-ts-` namespace")]
    ReservedHeader,

    /// A message framing or hop-by-hop header.
    #[display("sets a message framing or hop-by-hop header")]
    FramingHeader,
}

/// The cookie name in a `Set-Cookie` value, as raw bytes.
///
/// Reads the bytes rather than a `&str` so a value that is not valid UTF-8
/// cannot smuggle a managed cookie name past the check.
fn set_cookie_name(value: &[u8]) -> &[u8] {
    let pair_end = value.iter().position(|b| *b == b';').unwrap_or(value.len());
    let pair = &value[..pair_end];
    let name_end = pair.iter().position(|b| *b == b'=').unwrap_or(pair.len());
    pair[..name_end].trim_ascii()
}

/// Classifies one provider response header against core's reserved surface.
///
/// Returns `Some` when the header would reach into what core manages, and
/// `None` for everything else, including a provider's own cookie. Providers
/// legitimately need to set cookies of their own (an evidence cookie for a
/// later request, for example), so the rule reserves core's namespace rather
/// than banning `Set-Cookie` outright.
///
/// A rejected effect fails the request rather than being dropped, because a
/// provider reaching into the reserved surface has broken its contract in the
/// same way as one minting an identifier outside the cookie-safe alphabet, and
/// that already fails the request. Serving the response instead would let a
/// provider set `ts-ec` directly, bypassing core's identifier validation and
/// its requirement that a minted identifier have an identity-graph row.
#[must_use]
pub fn reserved_response_effect(
    name: &http::HeaderName,
    value: &http::HeaderValue,
) -> Option<ReservedResponseEffect> {
    let lower = name.as_str();
    if lower == http::header::SET_COOKIE.as_str() {
        let cookie_name = set_cookie_name(value.as_bytes());
        if cookie_name.len() >= MANAGED_COOKIE_NAME_PREFIX.len()
            && cookie_name[..MANAGED_COOKIE_NAME_PREFIX.len()]
                .eq_ignore_ascii_case(MANAGED_COOKIE_NAME_PREFIX)
        {
            return Some(ReservedResponseEffect::ManagedCookie);
        }
        return None;
    }
    if lower.starts_with(RESERVED_RESPONSE_HEADER_PREFIX) {
        return Some(ReservedResponseEffect::ReservedHeader);
    }
    if FRAMING_OR_HOP_BY_HOP_HEADERS.contains(&lower) {
        return Some(ReservedResponseEffect::FramingHeader);
    }
    None
}

/// Applies a provider's response headers to a response that already carries
/// the publisher origin's own.
///
/// Every header here accumulates with what the origin returned rather than
/// replacing it, because a provider on this seam only ever adds evidence about
/// the request. It is never correcting the origin's output, so core has no
/// grounds to discard a value it did not write. Working through the headers a
/// provider can actually set:
///
/// - `Set-Cookie` can never be folded into one field line, so replacing it
///   drops every cookie the origin set, a publisher's session and sign-in
///   cookies included. This is the case the whole rule turns on, because
///   `response_headers` is a list of pairs precisely so a provider can set more
///   than one cookie of its own, and replacing collapses those too.
/// - The list-valued headers a provider realistically sets, `Vary` first among
///   them, mean the union of their field lines. Replacing the origin's
///   `Vary: Accept-Encoding` with the provider's own would break the cache
///   correctness the origin asked for.
/// - The single-valued headers where replacing would be the right answer are
///   exactly the ones a provider must not author at all, and
///   [`reserved_response_effect`] already fails the request for them: core's
///   `x-ts-` namespace, the `ts-` managed cookies, and the framing and
///   hop-by-hop set.
///
/// So nothing a provider is permitted to set here needs to replace, and
/// accumulating is the direction that cannot silently destroy someone else's
/// header. Appending where one value was wanted leaves a duplicate a reviewer
/// can see; replacing where two were wanted leaves nothing at all.
pub(crate) fn apply_provider_response_headers<I>(headers: &mut http::HeaderMap, provider_headers: I)
where
    I: IntoIterator<Item = (http::HeaderName, http::HeaderValue)>,
{
    for (name, value) in provider_headers {
        headers.append(name, value);
    }
}

/// The registered short code that namespaces one Edge Cookie provider's
/// identifiers.
///
/// Exactly four characters from `[a-z0-9]`, allocated append-only in the
/// provider-code registry and never reused. The code appears as the
/// `{code}~` prefix of every identifier the provider mints, so identifiers
/// from different providers can never collide in the cookie, the identity
/// graph, or a withdrawal, and each identifier records which provider
/// created it.
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
/// dual-reads its pre-envelope form so deployed cookies keep working across
/// the migration.
///
/// # Retiring the legacy bare reader
///
/// The reader stays until a bare identifier can no longer arrive. A returning
/// visitor's bare cookie is never rewritten into the coded form, and neither
/// the cookie nor its identity-graph row is refreshed on an ordinary page view
/// (see `ec_finalize_response` in [`finalize`](super::finalize)), so each has
/// one fixed lifetime running from the moment it was written: `COOKIE_MAX_AGE`
/// in [`cookies`](super::cookies) and `ENTRY_TTL` in [`kv`](super::kv), both
/// one year, neither operator-configurable. The earliest safe retirement is
/// therefore one year (the longer of the two, and today they are equal) after
/// the last release that could still mint a bare identifier has stopped
/// running anywhere, plus however long a deployment's own rollout takes to
/// reach every point of presence.
///
/// The other half of that condition, evidence that bare identifiers really
/// have stopped arriving, cannot be checked today. Nothing counts or logs a
/// bare-form read-back, so there is no observed legacy-reader traffic to look
/// at, and the elapsed time alone cannot tell anyone whether a deployment
/// somewhere is still serving them. Scheduling the removal needs that signal
/// to exist first. Until it does the reader stays, and keeping it costs one
/// string comparison per read-back.
#[must_use]
pub fn provider_owns_id(provider: &dyn EdgeCookieProvider, full: &str) -> bool {
    match split_provider_code(full) {
        (Some(code), value) => code == provider.code().as_str() && provider.accepts_id(value),
        (None, value) => provider.id() == HMAC_PROVIDER_KEY && provider.accepts_id(value),
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

/// The providers whose identifiers a partner or diagnostic path accepts.
///
/// Pull sync, batch sync, and the admin lookup each take an identifier from
/// outside the organic request path and have to decide whether Trusted Server
/// issued it. The answer is in two parts. The **global cookie bounds** (the
/// length cap and the cookie-safe alphabet, see `ec_id_has_only_allowed_chars`)
/// apply to every identifier whichever provider minted it. The rest is
/// **dispatched by the `{code}~` prefix** to the provider that owns that code,
/// which canonicalizes its own value part and decides whether the canonical
/// form is one of its own. A code no provider in the set owns is rejected, so a
/// second provider's identifiers can never be adopted or written under this
/// deployment's keys.
///
/// The set holds the deployment's active provider. The design's
/// `legacy_providers` reader list, the providers that never mint but must still
/// recognize identifiers a previous provider issued, is not implemented on this
/// branch, so [`active`](Self::active) fills `readers` with the one active
/// provider. That is the seam: when the configured legacy readers land they are
/// built alongside the active provider and pushed into the same list, and
/// neither [`accepts`](Self::accepts) nor
/// [`canonical_kv_key`](Self::canonical_kv_key) changes.
pub struct AcceptedProviders<'a> {
    readers: Vec<&'a dyn EdgeCookieProvider>,
}

impl<'a> AcceptedProviders<'a> {
    /// The set holding only the deployment's active provider.
    ///
    /// `None` means no provider is selected, so the deployment is stateless.
    #[must_use]
    pub fn active(provider: Option<&'a dyn EdgeCookieProvider>) -> Self {
        Self {
            readers: provider.into_iter().collect(),
        }
    }

    /// The provider in the set that owns `full`'s code.
    ///
    /// Dispatch is on the code alone, before any provider looks at a value, so
    /// an identifier a partner echoed back in a different case still reaches
    /// its own provider to be canonicalized rather than being rejected first.
    /// A legacy bare identifier predates the envelope and belongs to the
    /// built-in HMAC provider alone.
    fn owner(&self, full: &str) -> Option<&'a dyn EdgeCookieProvider> {
        let (code, _) = split_provider_code(full);
        self.readers.iter().copied().find(|provider| match code {
            Some(code) => provider.code().as_str() == code,
            None => provider.id() == HMAC_PROVIDER_KEY,
        })
    }

    /// Whether `full` is an identifier this deployment accepts.
    #[must_use]
    pub fn accepts(&self, full: &str) -> bool {
        self.canonical_kv_key(full).is_some()
    }

    /// The identity-graph key for `full`, or `None` when nothing in the set
    /// accepts it.
    ///
    /// The owning provider supplies the canonical form of its own value part
    /// and the code prefix is preserved verbatim, so two providers' rows can
    /// never share a key.
    #[must_use]
    pub fn canonical_kv_key(&self, full: &str) -> Option<String> {
        if !ec_id_has_only_allowed_chars(full) {
            return None;
        }
        match self.owner(full) {
            Some(owner) => {
                let key = provider_kv_key(owner, full);
                provider_owns_id(owner, &key).then_some(key)
            }
            // No provider is selected, so there is no code to dispatch on and
            // the built-in HMAC grammar is the fallback, the same fallback
            // `EcContext::accepts_id` has always used for a stateless
            // deployment.
            None if self.readers.is_empty() => {
                let key = generation::normalize_ec_id_for_kv(full);
                generation::is_valid_ec_id(&key).then_some(key)
            }
            // A code that belongs to some other deployment's provider.
            None => None,
        }
    }
}

/// A strategy for deriving an Edge Cookie identifier.
///
/// Implementations are selected by configuration. A provider derives the
/// identifier at the edge in [`generate`](Self::generate), and the page
/// response sets the `ts-ec` cookie.
///
/// A provider that cannot derive an identifier at the edge returns a
/// [`GeneratedEdgeCookie`] whose [`id`](GeneratedEdgeCookie::id) is `None`, so
/// the request proceeds without an Edge Cookie rather than failing.
pub trait EdgeCookieProvider: Send + Sync + core::fmt::Debug {
    /// Returns the stable identifier for this provider, used in configuration
    /// and logs.
    fn id(&self) -> &'static str;

    /// The provider's registered code, the `{code}~` namespace of every
    /// identifier it mints.
    ///
    /// Mandatory, with no default: a provider must allocate a unique code in
    /// the provider-code registry before it can exist, so no two providers
    /// can ever mint colliding identifiers. Core applies the code at mint and
    /// checks it at read-back, and the provider itself only ever sees its own
    /// value part.
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
///
/// The client IP is this provider's only input, so it is this provider that
/// requires one. On a host that cannot supply one, [`RequestInfo::client_ip`]
/// is the empty string and [`generate`](Self::generate) fails rather than
/// hashing the empty string into an identifier every visitor on that host
/// would share. The failure reaches the caller, so the request fails rather
/// than being served without identity. A provider that reads other evidence
/// makes its own decision and is unaffected.
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
        HMAC_PROVIDER_KEY
    }

    fn code(&self) -> ProviderCode {
        HMAC_PROVIDER_CODE
    }

    fn generate(
        &self,
        request_info: &dyn RequestInfo,
        _input: &IdentityInput<'_>,
    ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
        let client_ip = request_info.client_ip();
        if client_ip.is_empty() {
            return Err(Report::new(TrustedServerError::EdgeCookie {
                message: "Edge Cookie provider `hmac` requires the client IP, and this host                           could not supply one"
                    .to_owned(),
            }));
        }
        let id = generation::generate_ec_id(self.passphrase.expose(), client_ip)?;
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
/// Returns [`TrustedServerError::EdgeCookie`] when the named provider cannot be
/// built: a built-in name whose configuration block is missing, or a name this
/// deployment's adapter does not inject. Both fail loudly rather than leaving
/// the deployment running stateless under a selector that says otherwise.
pub fn build_provider(
    ec: &Ec,
    injected: Option<Arc<dyn EdgeCookieProvider>>,
) -> Result<Option<Box<dyn EdgeCookieProvider>>, Report<TrustedServerError>> {
    let Some(selection) = ec.provider.as_ref() else {
        return Ok(None);
    };
    let provider: Option<Box<dyn EdgeCookieProvider>> = match selection {
        // Explicit statelessness: the same meaning as omitting the selector.
        EcProviderSelection::None => None,
        // Every provider is named, and this is the one place a name is resolved
        // to an implementation. Nothing else in the codebase asks whether a
        // name is built in.
        EcProviderSelection::Named(key) => Some(resolve_named_provider(key, ec, injected)?),
    };
    Ok(provider)
}

/// Resolves one provider name to its implementation.
///
/// A name is looked for among the providers built into core first, and is
/// otherwise the name of a provider the adapter injects through
/// [`RuntimeServices`](crate::platform::RuntimeServices), the same seam the
/// device and geo providers use, so core never names a vendor. The injected
/// provider is used when its own id matches the name, and its
/// `[ec.providers.<name>]` block is read by the adapter that built it.
///
/// # Errors
///
/// Returns [`TrustedServerError::EdgeCookie`] when the name matches no provider
/// this deployment can build, or when a built-in name has no configuration
/// block. Both fail loudly rather than silently running stateless.
fn resolve_named_provider(
    key: &str,
    ec: &Ec,
    injected: Option<Arc<dyn EdgeCookieProvider>>,
) -> Result<Box<dyn EdgeCookieProvider>, Report<TrustedServerError>> {
    // The only place that knows a provider is built into core rather than
    // supplied as a module. It disappears, along with `HMAC_PROVIDER_KEY`,
    // when the HMAC provider becomes a module like every other provider, after
    // which `hmac` resolves through the injected path below and nothing else
    // changes.
    //
    // Settings validation rejects a built-in name with no block before this
    // runs, so reaching the error means the two checks have drifted apart.
    // Stopping is the only safe answer: returning no provider would run the
    // deployment stateless under a selector that says it has an identity
    // provider.
    if key == HMAC_PROVIDER_KEY {
        let config = ec.providers.hmac.as_ref().ok_or_else(|| {
            Report::new(TrustedServerError::EdgeCookie {
                message: "Edge Cookie provider `hmac` is selected but has no \
                          `[ec.providers.hmac]` configuration"
                    .to_owned(),
            })
        })?;
        return Ok(Box::new(HmacProvider::new(config.passphrase.clone())));
    }

    injected
        .filter(|provider| provider.id() == key)
        .map(|provider| Box::new(SharedProvider(provider)) as Box<dyn EdgeCookieProvider>)
        .ok_or_else(|| {
            Report::new(TrustedServerError::EdgeCookie {
                message: format!(
                    "Edge Cookie provider `{key}` is selected but this deployment's \
                     adapter does not provide it"
                ),
            })
        })
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
    use crate::settings::{EcProviders, HmacProviderConfig};

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

    fn header(name: &str, value: &str) -> (http::HeaderName, http::HeaderValue) {
        (
            http::HeaderName::from_bytes(name.as_bytes()).expect("should parse header name"),
            http::HeaderValue::from_str(value).expect("should parse header value"),
        )
    }

    #[test]
    fn reserved_response_effect_rejects_the_namespace_core_manages() {
        for (name, value, expected) in [
            (
                "set-cookie",
                "ts-ec=hmac~deadbeef.abc123; Path=/",
                ReservedResponseEffect::ManagedCookie,
            ),
            (
                "Set-Cookie",
                "  TS-EIDS=x; Path=/",
                ReservedResponseEffect::ManagedCookie,
            ),
            ("x-ts-ec", "spoofed", ReservedResponseEffect::ReservedHeader),
            (
                "X-TS-partner.example.com",
                "uid",
                ReservedResponseEffect::ReservedHeader,
            ),
            ("content-length", "0", ReservedResponseEffect::FramingHeader),
            (
                "Transfer-Encoding",
                "chunked",
                ReservedResponseEffect::FramingHeader,
            ),
            ("connection", "close", ReservedResponseEffect::FramingHeader),
        ] {
            let (name, value) = header(name, value);
            assert_eq!(
                reserved_response_effect(&name, &value),
                Some(expected),
                "`{name}` should be reserved"
            );
        }
    }

    #[test]
    fn reserved_response_effect_allows_provider_owned_effects() {
        for (name, value) in [
            ("set-cookie", "acme-evidence=abc; Path=/; Secure"),
            ("set-cookie", "sharedId=abc"),
            ("accept-ch", "Sec-CH-UA-Full-Version-List"),
            ("x-acme-probe", "1"),
            ("vary", "Sec-CH-UA"),
        ] {
            let (name, value) = header(name, value);
            assert_eq!(
                reserved_response_effect(&name, &value),
                None,
                "`{name}` is the provider's own and should be allowed"
            );
        }
    }

    #[test]
    fn reserved_response_effect_reads_a_non_utf8_set_cookie_as_bytes() {
        // A `Set-Cookie` carrying a byte above 127 cannot be read as a string,
        // so the cookie name is matched on raw bytes. Reading it as UTF-8 and
        // giving up on failure would let this value through.
        let name = http::header::SET_COOKIE;
        let mut bytes = b"ts-ec=value".to_vec();
        bytes.push(0xff);
        bytes.extend_from_slice(b"; Path=/");
        let value =
            http::HeaderValue::from_bytes(&bytes).expect("should build a non-utf8 header value");
        assert!(
            value.to_str().is_err(),
            "the test value should not be readable as UTF-8"
        );
        assert_eq!(
            reserved_response_effect(&name, &value),
            Some(ReservedResponseEffect::ManagedCookie),
            "a non-UTF-8 Set-Cookie should still be matched on its cookie name"
        );
    }

    /// A stand-in for a vendor provider an adapter injects.
    #[derive(Debug)]
    struct VendorProvider;

    impl EdgeCookieProvider for VendorProvider {
        fn id(&self) -> &'static str {
            "acme"
        }

        fn code(&self) -> ProviderCode {
            ProviderCode::new("t0ac")
        }

        fn generate(
            &self,
            _request_info: &dyn RequestInfo,
            _input: &IdentityInput<'_>,
        ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
            Ok(GeneratedEdgeCookie::default())
        }
    }

    #[test]
    fn accepted_providers_splits_global_bounds_from_provider_dispatch() {
        let hmac = HmacProvider::new(Redacted::new("test-secret-key-32-bytes-minimum".to_owned()));
        let hmac_value = format!("{}.ABC123", "a".repeat(64));
        let active = AcceptedProviders::active(Some(&hmac));

        // The global bounds come first and apply whoever minted the value: a
        // character outside the cookie-safe alphabet, or a value over the
        // length cap, never reaches a provider.
        assert!(
            !active.accepts(&format!("hmac~{hmac_value} with spaces")),
            "the cookie-safe alphabet is a global bound"
        );
        assert!(
            !active.accepts(&format!("hmac~{}", "a".repeat(300))),
            "the length cap is a global bound"
        );

        // Then dispatch by code to the provider that owns it.
        assert!(
            active.accepts(&format!("hmac~{hmac_value}")),
            "the active provider's own code is accepted"
        );
        assert!(
            active.accepts(&hmac_value),
            "the legacy bare form belongs to the built-in provider"
        );
        assert!(
            !active.accepts(&format!("t0ac~{hmac_value}")),
            "a code no configured provider reads is rejected even in the HMAC shape"
        );

        // A vendor provider's own identifiers are accepted when it is the
        // active one, and the built-in bare form then belongs to nobody.
        let vendor = AcceptedProviders::active(Some(&VendorProvider));
        assert!(
            vendor.accepts(&format!("t0ac~{hmac_value}")),
            "the vendor provider's code is accepted when it is active"
        );
        assert!(
            !vendor.accepts(&hmac_value),
            "the legacy bare form is the built-in provider's alone"
        );

        // With no provider selected the deployment is stateless, so the
        // built-in grammar is the fallback, as it has always been.
        let stateless = AcceptedProviders::active(None);
        assert!(
            stateless.accepts(&hmac_value),
            "a stateless deployment falls back to the built-in grammar"
        );
        assert!(
            !stateless.accepts("not-an-identifier"),
            "the fallback is still the built-in grammar, not anything goes"
        );
    }

    #[test]
    fn the_selector_round_trips_through_serialization() {
        // The typed selector must not change the configuration surface. The
        // same TOML has to parse to the same choice, and serializing has to
        // write the same key back, so an existing operator configuration keeps
        // working and a config push does not rewrite the selector.
        for (key, expected) in [
            (EcProviderSelection::NONE_KEY, EcProviderSelection::None),
            (
                HMAC_PROVIDER_KEY,
                EcProviderSelection::Named(HMAC_PROVIDER_KEY.to_owned()),
            ),
            ("acme", EcProviderSelection::Named("acme".to_owned())),
        ] {
            let ec: Ec = toml::from_str(&format!("provider = \"{key}\""))
                .expect("should parse the [ec] section");
            assert_eq!(
                ec.provider.as_ref(),
                Some(&expected),
                "`{key}` should select the provider it names"
            );
            assert_eq!(
                expected.key(),
                key,
                "`{key}` should report itself under the key it was written as"
            );

            // The serialized form is the string itself, byte for byte, so an
            // operator configuration written before the selector was typed
            // parses and is written back identically.
            let value =
                toml::Value::try_from(expected.clone()).expect("should serialize the selection");
            assert_eq!(
                value,
                toml::Value::String(key.to_owned()),
                "`{key}` should serialize to exactly its own string"
            );

            let written = toml::to_string(&ec).expect("should serialize the [ec] section");
            assert!(
                written.contains(&format!("provider = \"{key}\"")),
                "`{key}` should be written back unchanged, got: {written}"
            );

            // A full round trip through the document leaves the same choice.
            let reparsed: Ec = toml::from_str(&written).expect("should reparse the [ec] section");
            assert_eq!(
                reparsed.provider.as_ref(),
                Some(&expected),
                "`{key}` should survive a serialize and parse round trip"
            );
        }
    }

    #[test]
    fn each_selection_builds_what_its_string_key_built_before() {
        // `none` is stateless, exactly as omitting the selector is.
        let none = Ec {
            provider: Some(EcProviderSelection::None),
            ..Ec::default()
        };
        assert!(
            build_provider(&none, None)
                .expect("explicit statelessness should build")
                .is_none(),
            "`none` should select no provider"
        );

        // `hmac` with its block builds the built-in provider.
        let mut providers = EcProviders::default();
        providers.hmac = Some(HmacProviderConfig {
            passphrase: test_passphrase(),
        });
        let hmac = Ec {
            provider: Some(EcProviderSelection::from(HMAC_PROVIDER_KEY)),
            providers,
            ..Ec::default()
        };
        let built = build_provider(&hmac, None)
            .expect("the hmac selection should build")
            .expect("the hmac selection should yield a provider");
        assert_eq!(
            built.id(),
            HMAC_PROVIDER_KEY,
            "`hmac` should select the built-in provider"
        );
        assert_eq!(
            built.code(),
            HMAC_PROVIDER_CODE,
            "the built-in provider should carry the built-in code"
        );

        // An arbitrary vendor key selects the provider the adapter injected
        // under that same key.
        let vendor = Ec {
            provider: Some(EcProviderSelection::Named("acme".to_owned())),
            ..Ec::default()
        };
        let built = build_provider(&vendor, Some(Arc::new(VendorProvider)))
            .expect("the vendor selection should build")
            .expect("the vendor selection should yield a provider");
        assert_eq!(
            built.id(),
            "acme",
            "a vendor key should select the injected provider of that id"
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
            provider: Some(EcProviderSelection::from("acme")),
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
    fn selecting_hmac_without_its_block_fails_loudly() {
        // `Ec::validate_provider_selection` rejects this pair before settings
        // reach the composition root, so the state is built directly here to
        // reach the seam. If the two checks ever drift apart, `build_provider`
        // must still stop rather than hand back a stateless deployment.
        let ec = Ec {
            provider: Some(EcProviderSelection::from(HMAC_PROVIDER_KEY)),
            ..Ec::default()
        };

        let err = build_provider(&ec, None)
            .expect_err("selecting hmac with no [ec.providers.hmac] block should error");
        assert!(
            err.to_string().contains("[ec.providers.hmac]"),
            "the error should name the missing block, got: {err}"
        );
    }

    #[test]
    fn the_startup_check_rejects_an_uninjected_provider_and_allows_statelessness() {
        // A selection the adapter cannot supply is knowable without a request,
        // so the composition root rejects it while application state is built.
        let selected = Ec {
            provider: Some(EcProviderSelection::from("acme")),
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
            provider: Some(EcProviderSelection::None),
            ..Ec::default()
        };
        ensure_provider_available(&explicit_none, None)
            .expect("should allow the explicit `none` selection");
    }
}
