//! Edge Cookie identity providers.
//!
//! An [`EdgeCookieProvider`] derives an Edge Cookie identifier. The provider is
//! selected by configuration, with no default, and [`build_provider`] is the
//! composition root that builds the selected one. A built-in provider is
//! constructed from its `[ec.providers.<key>]` block, and a vendor provider is
//! taken from the adapter that injected it. A built-in provider that also needs
//! a host service, as the host-signal provider needs the [`HostSignals`]
//! service, is built only on a host that supplies that service.
//! Construction reads configuration and long-lived services, so a selection
//! this deployment cannot satisfy fails at startup rather than leaving it
//! running without an identity. The host-signal provider is the exception: it
//! is built per request from that request's TLS and HTTP/2 signals (see
//! [`is_request_scoped`](EdgeCookieProvider::is_request_scoped)).
//!
//! Request evidence reaches a provider at call time rather than at
//! construction. [`EdgeCookieProvider::generate`] borrows a [`RequestInfo`],
//! which carries the normalized client IP, the User-Agent and the request
//! headers, for the life of the call, alongside an [`IdentityInput`] holding
//! the request's gating context. A provider reads what it needs and retains
//! nothing. Core snapshots the headers, path and query it lends to the provider
//! at generate time, and the provider itself keeps none of it.
//!
//! [`HmacProvider`] is the built-in server-side implementation. It derives the
//! identifier from the client IP using HMAC over the configured passphrase, the
//! behavior Trusted Server has always shipped.

use std::sync::Arc;

use error_stack::Report;
use serde::{Deserialize, Serialize};

use crate::consent::ConsentContext;
use crate::error::TrustedServerError;
use crate::evidence::{HostSignals, RequestInfo};
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
    /// the selector: no Edge Cookie is created and no provider block may be
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

/// The configuration name of the HMAC provider still built into core.
///
/// The name lives in the same open-ended namespace every vendor provider name
/// comes from, and nothing branches on it outside the resolution in
/// [`build_provider`]. It is also [`HmacProvider::id`]'s return value and
/// [`HMAC_PROVIDER_CODE`]'s text. It goes with that resolution arm when the
/// built-in provider becomes a module of its own.
pub const HMAC_PROVIDER_KEY: &str = "hmac";

/// The configuration name of the host-signal provider still built into core.
///
/// An ordinary name in the same open-ended namespace as [`HMAC_PROVIDER_KEY`],
/// spelled exactly the way a vendor crate spells its own, and nothing branches
/// on it outside the resolution in [`build_provider`]. It is also
/// [`HostSignalProvider::id`]'s return value, and it goes with that resolution
/// arm when the host-signal provider becomes a module of its own.
pub const HOST_SIGNALS_PROVIDER_KEY: &str = "host-signals";

/// The provider names core supplies itself.
///
/// A name in this list is already taken, so an adapter that injects a provider
/// under one of them has two suppliers claiming a single name and
/// [`build_provider`] refuses the pair rather than picking one. The list holds
/// one entry per resolution arm in [`resolve_named_provider`], so it grows and
/// shrinks with them, and it empties when the providers still built into core
/// become modules like every other provider, at which point no name is
/// reserved and every provider is injected.
const BUILTIN_PROVIDER_KEYS: &[&str] = &[HMAC_PROVIDER_KEY, HOST_SIGNALS_PROVIDER_KEY];

/// The registry code of the built-in HMAC provider.
///
/// The same text as [`HMAC_PROVIDER_KEY`], but a different role: this is the
/// `{code}~` namespace stamped on every identifier the built-in provider
/// creates, and it is what [`generation`] matches when it decides whether an
/// enveloped identifier is one of its own.
pub const HMAC_PROVIDER_CODE: ProviderCode = crate::provider_code!(HMAC_PROVIDER_KEY);

/// The request-scoped gating context passed to [`EdgeCookieProvider::generate`].
///
/// Request data reaches a provider through the `request_info` parameter of
/// [`EdgeCookieProvider::generate`], not through this struct and not through
/// anything injected into the provider's constructor. This struct carries only
/// the per-request gating context a provider may read for behavior beyond
/// gating. On the organic request path the gate has confirmed Edge Cookie
/// storage is allowed before `generate` is called. A direct
/// `edge_cookie::generate_ec_id` call, test-only today, reaches `generate`
/// without that gate.
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

/// Response headers that frame an HTTP message, are hop-by-hop, or govern
/// caching.
///
/// The hop-by-hop set is RFC 7230 §6.1, plus `content-length`, which frames the
/// body the adapter is about to write, and `cache-control`, which governs
/// whether the response may be cached. A provider that set any of these would
/// be rewriting the response envelope rather than adding evidence to it, and a
/// provider setting `cache-control` could make an identity-bearing response
/// publicly cacheable, so it is reserved with the rest.
const FRAMING_OR_HOP_BY_HOP_HEADERS: &[&str] = &[
    "cache-control",
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

    /// A framing, hop-by-hop, or caching header core manages.
    #[display("sets a framing, hop-by-hop, or caching header core manages")]
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
/// same way as one creating an identifier outside the cookie-safe alphabet, and
/// that already fails the request. Serving the response instead would let a
/// provider set `ts-ec` directly, bypassing core's identifier validation and
/// its requirement that a created identifier have an identity-graph row.
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
/// `{code}~` prefix of every identifier the provider creates, so identifiers
/// from different providers can never collide in the cookie, the identity
/// graph, or a withdrawal, and each identifier records which provider
/// created it.
#[derive(Debug, Copy, Clone, Eq, Hash, PartialEq, derive_more::Display)]
pub struct ProviderCode(&'static str);

impl ProviderCode {
    /// Creates a provider code when `code` matches the registry format.
    ///
    /// Returns `None` when `code` is not exactly four characters of `[a-z0-9]`,
    /// so a caller that assembles a code from anything other than a literal is
    /// handed an answer it has to deal with rather than a panic. Nothing in
    /// this function can panic, whatever it is called with and wherever it is
    /// called from.
    ///
    /// Use [`provider_code!`](crate::provider_code) for a literal. That macro
    /// runs this check while the crate is compiled, so a malformed code is a
    /// build failure and the resulting value needs no unwrapping.
    ///
    /// # Examples
    ///
    /// ```
    /// use trusted_server_core::ec::provider::ProviderCode;
    ///
    /// assert_eq!(ProviderCode::new("t0ac").map(ProviderCode::as_str), Some("t0ac"));
    /// assert_eq!(ProviderCode::new("nope!"), None);
    /// ```
    #[must_use]
    pub const fn new(code: &'static str) -> Option<Self> {
        let bytes = code.as_bytes();
        if bytes.len() != 4 {
            return None;
        }
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if !b.is_ascii_lowercase() && !b.is_ascii_digit() {
                return None;
            }
            i += 1;
        }
        Some(Self(code))
    }

    /// The code as a string slice.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

/// Builds a [`ProviderCode`] from a constant, checked while the crate is
/// compiled.
///
/// The check runs inside a `const` block, so a code that is not exactly four
/// characters of `[a-z0-9]` fails the build instead of panicking at run time,
/// and the value the macro produces needs no unwrapping. Every provider code in
/// this workspace is written through this macro, which is what makes
/// [`ProviderCode::new`]'s fallible form safe to hand to anyone else.
///
/// # Examples
///
/// ```
/// use trusted_server_core::provider_code;
///
/// assert_eq!(provider_code!("t0ac").as_str(), "t0ac");
/// ```
#[macro_export]
macro_rules! provider_code {
    ($code:expr) => {
        const {
            match $crate::ec::provider::ProviderCode::new($code) {
                Some(code) => code,
                None => panic!("provider code must be exactly four characters of [a-z0-9]"),
            }
        }
    };
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
/// visitor's bare cookie is never rewritten into the coded form, and its
/// `COOKIE_MAX_AGE` lifetime in [`cookies`](super::cookies) (one year, not
/// operator-configurable) runs from the moment it was written. The
/// identity-graph row is not fixed the same way: an ordinary page view that
/// ingests `ts-eids` or `sharedId` cookies runs `ingest_eid_cookies` in
/// `ec_finalize_response` (see [`finalize`](super::finalize)), which rewrites
/// the bare-keyed row with a fresh `ENTRY_TTL` in [`kv`](super::kv) (also one
/// year), so the row's clock restarts on each such view. The earliest safe
/// retirement is therefore one year after the last write that could still
/// leave a bare-keyed row, which is the later of the last release that could
/// still create a bare identifier stopping everywhere and the last page view
/// that refreshed such a row, plus however long a deployment's own rollout
/// takes to reach every point of presence.
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

/// The full created identifier for `value` under `provider`'s code.
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
/// apply to every identifier whichever provider created it. The rest is
/// **dispatched by the `{code}~` prefix** to the provider that owns that code,
/// which canonicalizes its own value part and decides whether the canonical
/// form is one of its own. A code no provider in the set owns is rejected, so a
/// second provider's identifiers can never be adopted or written under this
/// deployment's keys.
///
/// Batch sync keys its rows through [`canonical_kv_key`](Self::canonical_kv_key)
/// here. Pull sync and the admin lookup still read and write rows by the raw
/// active identifier rather than the canonical form, so for a provider whose
/// canonical form differs from the cookie value they can key the wrong row.
/// That gap is recorded on `EcContext::kv_key_for` and tracked as a known
/// issue for a later change.
///
/// The set holds the deployment's active provider. The design's
/// `legacy_providers` reader list, the providers that never create but must still
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
    /// identifier it creates.
    ///
    /// Mandatory, with no default: a provider must allocate a unique code in
    /// the provider-code registry before it can exist, so no two providers
    /// can ever create colliding identifiers. Core applies the code at
    /// creation and checks it at read-back, and the provider itself only ever
    /// sees its own value part.
    fn code(&self) -> ProviderCode;

    /// Whether this provider was built from evidence about one request.
    ///
    /// Almost every provider is built from configuration and services that are
    /// the same for every request, so one instance can be resolved once and
    /// handed to all of them. [`HostSignalProvider`] is the exception, because
    /// it is built from the TLS and HTTP/2 signals of a single request and
    /// answers `true` here. A composition root reads this through
    /// [`build_reusable_provider`] to decide whether keeping the instance is
    /// safe, and keeping a request-scoped one would serve every later request
    /// from the first request's evidence.
    ///
    /// The default is `false`, which is right for a provider whose constructor
    /// takes only configuration and long-lived services.
    fn is_request_scoped(&self) -> bool {
        false
    }

    /// Derives an Edge Cookie identifier from the request evidence in
    /// `request_info` and the gating context in `input`.
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
    /// (`<64 hex>.<6 alphanumeric>`), which is correct for [`HmacProvider`], the
    /// one provider core builds in.
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
/// would share. The failure is returned to the caller. The publisher proxy and
/// integration proxy log it and serve the response without an Edge Cookie. A
/// provider that reads other evidence makes its own decision and is unaffected.
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
                message: "Edge Cookie provider `hmac` requires the client IP, and this host \
                          could not supply one"
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

/// The built-in host-signal Edge Cookie provider.
///
/// Derives the identifier from the host signals (TLS JA4 and HTTP/2, read
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
        HOST_SIGNALS_PROVIDER_KEY
    }

    // Built from the signals of one request, so it is only ever valid for
    // that request and must never be kept and reused.
    fn is_request_scoped(&self) -> bool {
        true
    }

    fn code(&self) -> ProviderCode {
        crate::provider_code!("hs00")
    }

    fn generate(
        &self,
        request_info: &dyn RequestInfo,
        _input: &IdentityInput<'_>,
    ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
        let ja4 = self.host_signals.ja4().unwrap_or_default();
        let h2 = self.host_signals.h2().unwrap_or_default();
        // With no signal at all, creating an identifier would silently degrade
        // to an IP-only identifier under the host-signals name. Defer instead,
        // meaning no identity this request, and the request proceeds.
        if ja4.is_empty() && h2.is_empty() {
            log::warn!("Host-signal EC provider found no TLS/HTTP-2 signals; deferring");
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
}

/// Refuses an injected provider that claims a name core supplies itself.
///
/// Two suppliers cannot own one name. Core ships the `hmac` provider, and once
/// this work merges IAB Tech Lab is itself a vendor shipping an HMAC provider,
/// so the two really can arrive under the same name in one deployment. The
/// resolution order alone would answer that by quietly preferring the built-in
/// one and dropping the injected provider, which an operator has no way to see,
/// so the pair is refused here and the error names both claimants.
///
/// The check runs whatever the selector says, so an operator is told at startup
/// rather than on the first request that happens to select the contested name,
/// and it runs before the selection is read so a deployment cannot hide the
/// clash by selecting something else.
///
/// # Errors
///
/// Returns [`TrustedServerError::EdgeCookie`] when the injected provider's id
/// is one of [`BUILTIN_PROVIDER_KEYS`].
fn ensure_no_name_collision(
    injected: Option<&dyn EdgeCookieProvider>,
) -> Result<(), Report<TrustedServerError>> {
    let Some(injected) = injected else {
        return Ok(());
    };
    let Some(claimed) = BUILTIN_PROVIDER_KEYS
        .iter()
        .find(|key| **key == injected.id())
    else {
        return Ok(());
    };
    Err(Report::new(TrustedServerError::EdgeCookie {
        message: format!(
            "Edge Cookie provider name `{claimed}` is claimed twice, by the provider \
             built into Trusted Server core and by the provider this deployment's \
             adapter injects. Give the injected provider a name of its own and select \
             it under that name, because `[ec] provider = \"{claimed}\"` cannot mean \
             both of them."
        ),
    }))
}

/// Builds the Edge Cookie provider named by the `[ec] provider` selector.
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
/// Returns [`TrustedServerError::EdgeCookie`] when the named provider cannot be
/// built: a built-in name whose configuration block is missing, a built-in name
/// whose host capability this host does not supply, or a name this deployment's
/// adapter does not inject. All fail loudly rather than leaving the deployment
/// running stateless under a selector that says otherwise.
pub fn build_provider(
    ec: &Ec,
    host_signals: Option<Arc<dyn HostSignals>>,
    injected: Option<Arc<dyn EdgeCookieProvider>>,
) -> Result<Option<Box<dyn EdgeCookieProvider>>, Report<TrustedServerError>> {
    ensure_no_name_collision(injected.as_deref())?;
    let Some(selection) = ec.provider.as_ref() else {
        return Ok(None);
    };
    let provider: Option<Box<dyn EdgeCookieProvider>> = match selection {
        // Explicit statelessness: the same meaning as omitting the selector.
        EcProviderSelection::None => None,
        // Every provider is named, and this is the one place a name is resolved
        // to an implementation. Nothing else in the codebase asks whether a
        // name is built in.
        EcProviderSelection::Named(key) => {
            Some(resolve_named_provider(key, ec, host_signals, injected)?)
        }
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
/// Looking at core first is safe only because
/// [`ensure_no_name_collision`] has already refused an injected provider that
/// claims a built-in name, so this order can never shadow one silently.
///
/// # Errors
///
/// Returns [`TrustedServerError::EdgeCookie`] when the name matches no provider
/// this deployment can build, when a built-in name has no configuration block,
/// or when a built-in name needs a host capability this host does not supply.
/// All fail loudly rather than silently running stateless.
fn resolve_named_provider(
    key: &str,
    ec: &Ec,
    host_signals: Option<Arc<dyn HostSignals>>,
    injected: Option<Arc<dyn EdgeCookieProvider>>,
) -> Result<Box<dyn EdgeCookieProvider>, Report<TrustedServerError>> {
    // The only place that knows a provider is built into core rather than
    // supplied as a module. Each arm disappears, along with its name constant,
    // when that provider becomes a module like every other provider, after
    // which its name resolves through the injected path below and nothing else
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

    // The host-signal provider needs signals only some hosts supply, and
    // that check cannot be made in settings validation at all, so it is made
    // here rather than creating a degraded identifier under this name.
    if key == HOST_SIGNALS_PROVIDER_KEY {
        let config = ec.providers.host_signals.as_ref().ok_or_else(|| {
            Report::new(TrustedServerError::EdgeCookie {
                message: "Edge Cookie provider `host-signals` is selected but has no \
                          `[ec.providers.host-signals]` configuration"
                    .to_owned(),
            })
        })?;
        let signals = host_signals.ok_or_else(|| {
            Report::new(TrustedServerError::EdgeCookie {
                message: "The host-signals Edge Cookie provider requires a host that supplies \
                          TLS/HTTP-2 signals, which this host does not"
                    .to_owned(),
            })
        })?;
        return Ok(Box::new(HostSignalProvider::new(
            config.passphrase.clone(),
            signals,
        )));
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
/// the same services it will put into
/// [`RuntimeServices`](crate::platform::RuntimeServices) on every request.
/// [`build_provider`] reads no request data, so the answer is the same for
/// every request and a selection the adapter can never supply fails at startup
/// rather than on the first request. A stateless deployment (no selector, or
/// `"none"`) passes.
///
/// `host_signals` answers whether this adapter supplies a [`HostSignals`]
/// service at all, which is fixed per deployment, rather than what any one
/// request's signals are. An adapter that injects host signals on every
/// request passes an instance here even though its values are empty at
/// startup, and an adapter that never injects them passes `None`.
///
/// # Errors
///
/// Returns [`TrustedServerError::EdgeCookie`] when the selected provider cannot
/// be built from the services this deployment injects.
pub fn ensure_provider_available(
    ec: &Ec,
    host_signals: Option<Arc<dyn HostSignals>>,
    injected: Option<Arc<dyn EdgeCookieProvider>>,
) -> Result<(), Report<TrustedServerError>> {
    build_shared_provider(ec, host_signals, injected)?;
    Ok(())
}

/// Resolves the selected provider into a shared handle.
///
/// The same resolution as [`build_provider`], returned as an `Arc` rather than
/// a `Box` so one instance can be held in
/// [`RuntimeServices`](crate::platform::RuntimeServices) and read by every
/// request. Use [`build_reusable_provider`] at a composition root, which adds
/// the one check that decides whether keeping the instance is safe.
///
/// # Errors
///
/// The same errors as [`build_provider`].
pub fn build_shared_provider(
    ec: &Ec,
    host_signals: Option<Arc<dyn HostSignals>>,
    injected: Option<Arc<dyn EdgeCookieProvider>>,
) -> Result<Option<Arc<dyn EdgeCookieProvider>>, Report<TrustedServerError>> {
    Ok(build_provider(ec, host_signals, injected)?.map(Arc::from))
}

/// The provider a composition root may keep and hand to every request, when
/// the selection is one that can be kept at all.
///
/// Resolving is also the startup check, so a selection this deployment cannot
/// satisfy fails here rather than on the first request, exactly as
/// [`ensure_provider_available`] makes it fail. What this adds is the answer to
/// a second question, which is whether the provider that came back is the same
/// for every request. Most are, because they are built from configuration
/// alone, and keeping one saves resolving the same settings again on every
/// request.
///
/// [`HostSignalProvider`] is not, because it is built from the signals of
/// one request and reports
/// [`is_request_scoped`](EdgeCookieProvider::is_request_scoped). Keeping that
/// one would freeze the signals captured while application state was built,
/// which on every adapter here are empty, so every later request would find no
/// signals and defer. `Ok(None)` comes back for it, the adapter threads
/// nothing, and the request path resolves it per request against that request's
/// own signals.
///
/// `Ok(None)` therefore means "nothing to keep", which covers both a stateless
/// deployment and a provider that must be resolved per request. Both leave the
/// request path resolving for itself, which is what it did before anything was
/// kept.
///
/// # Errors
///
/// The same errors as [`build_provider`].
pub fn build_reusable_provider(
    ec: &Ec,
    host_signals: Option<Arc<dyn HostSignals>>,
    injected: Option<Arc<dyn EdgeCookieProvider>>,
) -> Result<Option<Arc<dyn EdgeCookieProvider>>, Report<TrustedServerError>> {
    let Some(provider) = build_shared_provider(ec, host_signals, injected)? else {
        return Ok(None);
    };
    if provider.is_request_scoped() {
        log::debug!(
            "Edge Cookie provider `{}` is built from request evidence, so it is resolved per              request rather than kept",
            provider.id(),
        );
        return Ok(None);
    }
    Ok(Some(provider))
}

/// The Edge Cookie provider to use for this request.
///
/// A provider reaches the request path through one seam only. An adapter
/// resolves `[ec] provider` once while it builds application state and threads
/// the answer into
/// [`RuntimeServices::resolved_ec_provider`](crate::platform::RuntimeServices::resolved_ec_provider),
/// and that same instance comes back here with nothing resolved or constructed
/// again on the request path. When nothing was threaded, this builds from
/// `[ec]` settings alone, which is what a deployment selecting only a built-in
/// provider does.
///
/// # Errors
///
/// The same errors as [`build_provider`], and only when nothing was threaded,
/// because a threaded provider has already been resolved successfully.
pub fn request_provider(
    ec: &Ec,
    services: &crate::platform::RuntimeServices,
) -> Result<Option<Arc<dyn EdgeCookieProvider>>, Report<TrustedServerError>> {
    if let Some(resolved) = services.resolved_ec_provider() {
        return Ok(Some(resolved));
    }
    build_shared_provider(ec, services.host_signals(), None)
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

    fn is_request_scoped(&self) -> bool {
        self.0.is_request_scoped()
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
    use crate::evidence::OwnedRequestInfo;
    use crate::settings::{EcProviders, HmacProviderConfig, HostSignalsProviderConfig};
    use http::HeaderMap;

    #[test]
    fn a_malformed_provider_code_is_refused_rather_than_panicking() {
        // `ProviderCode::new` is public, so a vendor crate can reach it with a
        // value it assembled rather than a literal. Every rejected shape has to
        // come back as `None`, because a panic here would take down whatever
        // request the caller was serving.
        for malformed in ["", "abc", "abcde", "AB12", "t0a_", "t0a-", "t0a ", "t.ac"] {
            assert_eq!(
                ProviderCode::new(malformed),
                None,
                "`{malformed}` is outside the registry format and should be refused"
            );
        }

        assert_eq!(
            ProviderCode::new("t0ac").map(ProviderCode::as_str),
            Some("t0ac"),
            "a well-formed code should still be accepted"
        );
    }

    #[test]
    fn the_provider_code_macro_keeps_the_compile_time_guarantee() {
        // The macro checks a literal while the crate is compiled and yields the
        // code itself, so the codes written across this workspace stay as
        // strong as the old panicking constructor made them, with none of the
        // run-time risk.
        assert_eq!(
            crate::provider_code!("t0ac").as_str(),
            "t0ac",
            "the macro should yield the code it was given"
        );
        assert_eq!(
            HMAC_PROVIDER_CODE.as_str(),
            HMAC_PROVIDER_KEY,
            "the built-in code should still be the built-in key"
        );
    }

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
            (
                "cache-control",
                "public, max-age=31536000",
                ReservedResponseEffect::FramingHeader,
            ),
            (
                "Cache-Control",
                "public",
                ReservedResponseEffect::FramingHeader,
            ),
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
            crate::provider_code!("t0ac")
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

        // The global bounds come first and apply whoever created the value. A
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
            build_provider(&none, None, None)
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
        let built = build_provider(&hmac, None, None)
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
        let built = build_provider(&vendor, None, Some(Arc::new(VendorProvider)))
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

    fn test_request_info() -> OwnedRequestInfo {
        OwnedRequestInfo::new("203.0.113.1".to_owned(), HeaderMap::new())
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
                crate::provider_code!("t0in")
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

    /// A vendor provider that claims the name core already uses for its
    /// built-in HMAC provider.
    #[derive(Debug)]
    struct VendorNamedHmacProvider;

    impl EdgeCookieProvider for VendorNamedHmacProvider {
        fn id(&self) -> &'static str {
            HMAC_PROVIDER_KEY
        }

        fn code(&self) -> ProviderCode {
            crate::provider_code!("t0vh")
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
    fn two_providers_claiming_one_name_are_refused_and_both_are_named() {
        // Once this work merges, IAB Tech Lab supplies an HMAC provider as a
        // vendor module while core still supplies one of its own, so a
        // deployment really can wire two providers called `hmac`. Resolution
        // order alone would prefer the built-in one and drop the injected one
        // with nothing said, which is the fault this guards.
        let mut providers = EcProviders::default();
        providers.hmac = Some(HmacProviderConfig {
            passphrase: test_passphrase(),
        });
        let selected_hmac = Ec {
            provider: Some(EcProviderSelection::from(HMAC_PROVIDER_KEY)),
            providers,
            ..Ec::default()
        };

        let err = build_provider(
            &selected_hmac,
            None,
            Some(Arc::new(VendorNamedHmacProvider)),
        )
        .expect_err("two providers claiming `hmac` should be refused");
        let message = err.to_string();
        assert!(
            message.contains(HMAC_PROVIDER_KEY),
            "the error should name the contested name, got: {message}"
        );
        assert!(
            message.contains("core") && message.contains("adapter"),
            "the error should name both claimants, got: {message}"
        );

        // The clash is a wiring fault, not a property of the selection, so
        // selecting something else does not hide it and the operator still
        // learns at startup.
        let selected_elsewhere = Ec {
            provider: Some(EcProviderSelection::None),
            ..Ec::default()
        };
        let err = ensure_provider_available(
            &selected_elsewhere,
            None,
            Some(Arc::new(VendorNamedHmacProvider)),
        )
        .expect_err("the clash should be refused whatever the selector says");
        assert!(
            err.to_string().contains(HMAC_PROVIDER_KEY),
            "the startup check should name the contested name too, got: {err}"
        );

        // A vendor name of its own is unaffected.
        let vendor = Ec {
            provider: Some(EcProviderSelection::Named("acme".to_owned())),
            ..Ec::default()
        };
        build_provider(&vendor, None, Some(Arc::new(VendorProvider)))
            .expect("a vendor provider under its own name should still build");
    }

    #[test]
    fn host_signal_provider_creates_from_signals() {
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
            "the host-signal provider should create an identifier from the signals"
        );
    }

    #[test]
    fn host_signal_provider_defers_without_signals() {
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
            "with no host signals the provider should defer rather than create an IP-only identifier"
        );
    }

    #[test]
    fn a_selected_but_uninjected_vendor_provider_fails_loudly() {
        let ec = Ec {
            provider: Some(EcProviderSelection::from("acme")),
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
    fn selecting_hmac_without_its_block_fails_loudly() {
        // `Ec::validate_provider_selection` rejects this pair before settings
        // reach the composition root, so the state is built directly here to
        // reach the seam. If the two checks ever drift apart, `build_provider`
        // must still stop rather than hand back a stateless deployment.
        let ec = Ec {
            provider: Some(EcProviderSelection::from(HMAC_PROVIDER_KEY)),
            ..Ec::default()
        };

        let err = build_provider(&ec, None, None)
            .expect_err("selecting hmac with no [ec.providers.hmac] block should error");
        assert!(
            err.to_string().contains("[ec.providers.hmac]"),
            "the error should name the missing block, got: {err}"
        );
    }

    #[test]
    fn a_provider_built_from_request_evidence_is_never_kept_and_reused() {
        // The host-signal provider captures the signals of the request it
        // was built for. A composition root builds application state with an
        // empty host-signal service, because there is no request yet, so
        // keeping that instance would serve every later request from empty
        // signals and the provider would defer forever. It must come back
        // as nothing to keep, leaving the request path to resolve it against
        // the signals each request actually carried.
        let mut providers = EcProviders::default();
        providers.host_signals = Some(HostSignalsProviderConfig {
            passphrase: test_passphrase(),
        });
        let host_signals_selected = Ec {
            provider: Some(EcProviderSelection::from(HOST_SIGNALS_PROVIDER_KEY)),
            providers,
            ..Ec::default()
        };
        let startup_signals: Arc<dyn HostSignals> = Arc::new(TestHostSignals {
            ja4: None,
            h2: None,
        });

        // It resolves, so the startup check still passes on a host that
        // supplies the service.
        let resolved = build_shared_provider(
            &host_signals_selected,
            Some(Arc::clone(&startup_signals)),
            None,
        )
        .expect("the host-signal selection should resolve on a host that supplies signals")
        .expect("the selection should yield a provider");
        assert!(
            resolved.is_request_scoped(),
            "the host-signal provider should declare itself built from request evidence"
        );

        // It is not offered for reuse.
        assert!(
            build_reusable_provider(
                &host_signals_selected,
                Some(Arc::clone(&startup_signals)),
                None
            )
            .expect("the host-signal selection should still pass the startup check")
            .is_none(),
            "a provider built from request evidence must never be kept for later requests"
        );

        // A provider built from configuration alone is still kept, so the
        // saving stands for every selection that can take it.
        let mut providers = EcProviders::default();
        providers.hmac = Some(HmacProviderConfig {
            passphrase: test_passphrase(),
        });
        let hmac_selected = Ec {
            provider: Some(EcProviderSelection::from(HMAC_PROVIDER_KEY)),
            providers,
            ..Ec::default()
        };
        let kept = build_reusable_provider(&hmac_selected, None, None)
            .expect("the hmac selection should resolve")
            .expect("a provider built from configuration alone should be kept");
        assert_eq!(
            kept.id(),
            HMAC_PROVIDER_KEY,
            "the kept provider should be the selected one"
        );
    }

    #[test]
    fn the_request_path_reuses_the_provider_the_composition_root_resolved() {
        // A composition root resolves the selection once while it builds
        // application state, which is the same work `build_provider` does on a
        // request, so doing both means doing it twice for every request. The
        // resolved provider is threaded into `RuntimeServices`, and this is the
        // assertion that the request path takes it rather than resolving again:
        // the same allocation, not merely an equal one.
        let ec = Ec {
            provider: Some(EcProviderSelection::Named("acme".to_owned())),
            ..Ec::default()
        };
        let resolved = build_reusable_provider(&ec, None, Some(Arc::new(VendorProvider)))
            .expect("the composition root should resolve the selection")
            .expect("the selection should yield a provider");

        let services = crate::platform::test_support::noop_services_with_resolved_ec_provider(
            Arc::clone(&resolved),
        );
        let for_request = request_provider(&ec, &services)
            .expect("the request path should take the resolved provider")
            .expect("the resolved provider should be there");

        assert!(
            Arc::ptr_eq(&resolved, &for_request),
            "the request path should reuse the resolved provider, not build a second one"
        );

        // An adapter that threads nothing still resolves for itself, so core
        // driven directly behaves exactly as it did before.
        let unthreaded =
            crate::platform::test_support::noop_services_with_ec_provider(Arc::new(VendorProvider));
        let built = request_provider(&ec, &unthreaded)
            .expect("an unthreaded adapter should resolve on the request path")
            .expect("the selection should yield a provider");
        assert_eq!(
            built.id(),
            "acme",
            "resolving on the request path should still select the injected provider"
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
        let err = ensure_provider_available(&selected, None, None)
            .expect_err("an uninjected provider should fail the startup check");
        assert!(
            err.to_string().contains("acme"),
            "the error should name the selected provider, got: {err}"
        );

        // Statelessness is a supported deployment, spelled either way, and must
        // never be turned into a startup error.
        ensure_provider_available(&Ec::default(), None, None)
            .expect("should allow a deployment that selects no provider");
        let explicit_none = Ec {
            provider: Some(EcProviderSelection::None),
            ..Ec::default()
        };
        ensure_provider_available(&explicit_none, None, None)
            .expect("should allow the explicit `none` selection");
    }

    #[test]
    fn the_startup_check_rejects_host_signals_on_a_host_that_supplies_none() {
        // Whether the adapter injects a host-signal service is fixed per
        // deployment, so selecting the host-signal provider on an adapter that
        // injects none is knowable without a request.
        let mut providers = EcProviders::default();
        providers.host_signals = Some(HostSignalsProviderConfig {
            passphrase: test_passphrase(),
        });
        let selected = Ec {
            provider: Some(EcProviderSelection::from(HOST_SIGNALS_PROVIDER_KEY)),
            providers,
            ..Ec::default()
        };

        let err = ensure_provider_available(&selected, None, None).expect_err(
            "the host-signal provider should fail the startup check with no host signals",
        );
        assert!(
            err.to_string().contains("TLS/HTTP-2 signals"),
            "the error should say the host supplies no signals, got: {err}"
        );

        let signals: Arc<dyn HostSignals> = Arc::new(TestHostSignals {
            ja4: None,
            h2: None,
        });
        ensure_provider_available(&selected, Some(signals), None).expect(
            "should pass on a host that injects host signals, whatever this request's signals are",
        );
    }
}
