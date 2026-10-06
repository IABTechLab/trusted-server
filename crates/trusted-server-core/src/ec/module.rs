//! Edge Cookie identity modules.
//!
//! An [`EdgeCookieModule`] derives an Edge Cookie identifier. The module is
//! selected by configuration, with no default, and [`build_module`] is the
//! composition root that builds the selected one. A built-in module is
//! constructed from its `[ec.<name>]` block, and a vendor module is taken
//! from the adapter that injected it. A built-in module that also needs a
//! host service, as the host-signal module needs the [`HostSignals`]
//! service, is built only on a host that supplies that service. Construction
//! reads configuration and long-lived services, so a selection this deployment
//! cannot satisfy fails at startup rather than leaving it running without an
//! identity. Fastly, Cloudflare and Spin resolve the module once per
//! application state and thread the result. Axum and embedders resolve per
//! request. The host-signal module is the exception, being built per request
//! from that request's TLS and HTTP/2 signals (see
//! [`is_request_scoped`](EdgeCookieModule::is_request_scoped)).
//!
//! Request evidence reaches a module at call time rather than at
//! construction. [`EdgeCookieModule::generate`] borrows a [`RequestInfo`],
//! which carries the normalized client IP, the User-Agent and the request
//! headers, for the life of the call, alongside an [`IdentityInput`] holding
//! the request's gating context. A module reads what it needs and retains
//! nothing. Core snapshots the headers, path and query it lends to the module
//! at generate time, and the module itself keeps none of it.
//!
//! [`HmacModule`] is the built-in server-side implementation. It derives the
//! identifier from the client IP using HMAC over the configured passphrase.

use std::sync::Arc;

use error_stack::Report;
use serde::{Deserialize, Serialize};

use crate::consent::ConsentContext;
use crate::error::TrustedServerError;
use crate::evidence::{HostSignals, RequestInfo};
use crate::permissions::{Permission, PermissionSet, PermissionState};
use crate::redacted::Redacted;
use crate::settings::{Ec, EcModuleBlock};

use super::cookies::ec_id_has_only_allowed_chars;
use super::generation;

/// The Edge Cookie identity module a deployment has selected.
///
/// Deserialized from the `[ec] module` string and serialized back to the
/// same string. Module names are open-ended (a vendor crate names its own),
/// so every name other than the explicit `"none"` becomes
/// [`Named`](Self::Named) rather than a parse failure, and whether the
/// deployment can actually supply that module is decided by
/// [`build_module`].
///
/// No module has a variant of its own, so every module is selected the
/// same way, by name, and no caller can be written around one module being
/// different.
///
/// This is the one place the selector is spelled. Everything that needs to ask
/// which module is selected matches on this rather than comparing string
/// literals.
#[derive(Debug, Clone, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(from = "String", into = "String")]
pub enum EcModuleSelection {
    /// Explicit statelessness, spelled `"none"`. The same meaning as omitting
    /// the selector: no Edge Cookie is created and no module block may be
    /// configured.
    None,

    /// A module selected by name, configured by the matching `[ec.<name>]`
    /// block when it has settings. The name is the implementation unless that
    /// block names one, and [`build_module`] resolves the implementation,
    /// whether it is built into core or injected by the adapter.
    Named(String),
}

impl EcModuleSelection {
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

impl From<&str> for EcModuleSelection {
    fn from(key: &str) -> Self {
        match key {
            EcModuleSelection::NONE_KEY => Self::None,
            other => Self::Named(other.to_owned()),
        }
    }
}

impl From<String> for EcModuleSelection {
    fn from(key: String) -> Self {
        match key.as_str() {
            EcModuleSelection::NONE_KEY => Self::None,
            _ => Self::Named(key),
        }
    }
}

impl From<EcModuleSelection> for String {
    fn from(selection: EcModuleSelection) -> Self {
        match selection {
            EcModuleSelection::None => EcModuleSelection::NONE_KEY.to_owned(),
            EcModuleSelection::Named(key) => key,
        }
    }
}

/// The implementation id of the HMAC module built into core.
///
/// It is also [`HmacModule::id`]'s return value and the text of
/// [`HMAC_MODULE_CODE`].
pub const HMAC_MODULE_KEY: &str = "hmac";

/// The implementation id of the host-signal module still built into core.
///
/// An ordinary id in the same open-ended namespace as [`HMAC_MODULE_KEY`],
/// spelled exactly the way a vendor crate spells its own, and nothing branches
/// on it outside the resolution in [`build_module`]. It is also
/// [`HostSignalModule::id`]'s return value, and it goes with that resolution
/// arm when the host-signal module becomes a module of its own.
pub const HOST_SIGNALS_MODULE_KEY: &str = "host_signals";

/// The implementation id of the `client_fixed` demonstration module.
///
/// Defined in every build, though the module is compiled in only under the
/// `client-fixed-demo` cargo feature, so a build without it still recognizes
/// the name and refuses the selection at startup. The resolution in
/// [`build_module`] matches it, as does its startup counterpart
/// `check_named_provider_configuration`, and the integration registry adds the
/// client-cycle page-script module when the name is selected. It is also
/// `ClientFixedProvider`'s `id`.
pub const CLIENT_FIXED_PROVIDER_KEY: &str = "client_fixed";

/// The implementation ids core supplies itself, one per resolution arm in
/// [`resolve_named_module`].
///
/// [`build_module`] refuses an injected module under one of these ids
/// rather than picking one of the two. [`CLIENT_FIXED_PROVIDER_KEY`] is listed
/// whether or not the demonstration module is compiled in, because a build
/// without it still owns the name.
const BUILTIN_MODULE_KEYS: &[&str] = &[
    HMAC_MODULE_KEY,
    HOST_SIGNALS_MODULE_KEY,
    CLIENT_FIXED_PROVIDER_KEY,
];

/// The registry code of the built-in HMAC module.
///
/// The same text as [`HMAC_MODULE_KEY`], but a different role: this is the
/// `{code}~` namespace stamped on every identifier the built-in module
/// creates, and it is what [`generation`] matches when it decides whether an
/// enveloped identifier is one of its own.
pub const HMAC_MODULE_CODE: ModuleCode = crate::module_code!(HMAC_MODULE_KEY);

/// The request-scoped gating context passed to [`EdgeCookieModule::generate`].
///
/// Request data reaches a module through the `request_info` parameter of
/// [`EdgeCookieModule::generate`], not through this struct and not through
/// anything injected into the module's constructor. This struct carries only
/// the per-request gating context a module may read for behavior beyond
/// gating. On the organic request path the gate has confirmed the module's
/// required permissions are set before `generate` is called. A test calling
/// `edge_cookie::generate_ec_id` reaches `generate` without that gate.
#[derive(Default)]
pub struct IdentityInput<'a> {
    /// The permissions resolved for this request, when the calling path carries
    /// them. A module reads this only for behavior beyond gating. The main
    /// organic path supplies them; the publisher path passes `None`.
    pub permissions: Option<&'a PermissionState>,

    /// The request's consent context, when available, for module-specific
    /// logic. The core gates on permissions, not consent, so a module reads
    /// this only to forward or record consent. [`HmacModule`] ignores it.
    pub consent: Option<&'a ConsentContext>,
}

/// Inputs available to [`EdgeCookieModule::resolve_from_client`].
///
/// Carries the value a client produced and posted to the Edge Cookie resolve
/// endpoint, alongside the same gating context as [`IdentityInput`]. The posted
/// value reaches the module as [`payload`](Self::payload), not through
/// anything injected into the module. Unlike trusted edge-derived
/// data, [`payload`](Self::payload) arrives from the browser, so an
/// implementation must verify it before deriving an identifier from it.
pub struct ClientResolveInput<'a> {
    /// The raw body the client posted to the resolve endpoint. For a vendor
    /// module this is its own JSON envelope; for the built-in
    /// `ClientFixedProvider` demo it is the fixed known word the page script
    /// posts.
    pub payload: &'a [u8],

    /// The permissions resolved for the resolve request. The endpoint has
    /// already confirmed the module's required permissions are set, so a
    /// module reads this only for behavior beyond gating.
    pub permissions: Option<&'a PermissionState>,

    /// The resolve request's consent context, for module-specific logic. The
    /// core gates on permissions, not consent.
    pub consent: Option<&'a ConsentContext>,
}

/// The outcome of [`EdgeCookieModule::generate`].
///
/// Carries the derived identifier, if any, and any response headers the module
/// needs set on the outbound response.
#[derive(Debug, Default)]
pub struct GeneratedEdgeCookie {
    /// The derived Edge Cookie identifier, or `None` when the module produced
    /// none for this request.
    pub id: Option<String>,

    /// Response headers the module needs set on the outbound response, for
    /// example to request additional client evidence on later requests. Empty
    /// for modules that set no headers, such as [`HmacModule`].
    ///
    /// Core checks every header here against its own reserved response surface
    /// (see [`reserved_response_effect`]) before it is applied, so a module
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
/// whether the response may be cached. A module that set any of these would
/// be rewriting the response envelope rather than adding evidence to it, and a
/// module setting `cache-control` could make an identity-bearing response
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

/// Why one module response header falls inside core's reserved surface.
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

/// Classifies one module response header against core's reserved surface.
///
/// Returns `Some` when the header would reach into what core manages, and
/// `None` for everything else, including a module's own cookie. Modules
/// legitimately need to set cookies of their own (an evidence cookie for a
/// later request, for example), so the rule reserves core's namespace rather
/// than banning `Set-Cookie` outright.
///
/// A rejected effect is not simply dropped while the rest of the module
/// response goes ahead. Generation returns an error instead, as it does for a
/// module creating an identifier outside the cookie-safe alphabet, because a
/// module reaching into the reserved surface has broken its contract in the
/// same way. The check runs before anything from that module response is
/// kept, so neither its identifier nor any of its headers is kept. The
/// publisher proxy and integration proxy log the error and serve the response
/// without an Edge Cookie, and orphan recovery in EC finalization leaves the
/// visitor's existing cookie in place. Applying the header instead would let a
/// module set `ts-ec` directly, bypassing core's identifier validation and
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

/// Appends a module's response headers to a response that already carries
/// the publisher origin's own.
///
/// Appending keeps the origin's `Set-Cookie` and `Vary` lines, and lets a
/// module set more than one cookie of its own.
/// [`reserved_response_effect`] has already refused the single-valued headers
/// core owns, so nothing a module may set here needs to replace a value.
pub(crate) fn apply_module_response_headers<I>(headers: &mut http::HeaderMap, module_headers: I)
where
    I: IntoIterator<Item = (http::HeaderName, http::HeaderValue)>,
{
    for (name, value) in module_headers {
        headers.append(name, value);
    }
}

/// The registered short code that namespaces one Edge Cookie module's
/// identifiers.
///
/// Exactly four characters from `[a-z0-9]`, allocated append-only in the
/// module-code registry and never reused. The code appears as the
/// `{code}~` prefix of every identifier the module creates, so identifiers
/// from different modules can never collide in the cookie, the identity
/// graph, or a withdrawal, and each identifier records which module
/// created it.
#[derive(Debug, Copy, Clone, Eq, Hash, PartialEq, derive_more::Display)]
pub struct ModuleCode(&'static str);

impl ModuleCode {
    /// Creates a module code when `code` matches the registry format.
    ///
    /// Returns `None` when `code` is not exactly four characters of `[a-z0-9]`,
    /// so a caller that assembles a code from anything other than a literal is
    /// handed an answer it has to deal with rather than a panic. Nothing in
    /// this function can panic, whatever it is called with and wherever it is
    /// called from.
    ///
    /// Use [`module_code!`](crate::module_code) for a literal. That macro
    /// runs this check while the crate is compiled, so a malformed code is a
    /// build failure and the resulting value needs no unwrapping.
    ///
    /// # Examples
    ///
    /// ```
    /// use trusted_server_core::ec::module::ModuleCode;
    ///
    /// assert_eq!(ModuleCode::new("t0ac").map(ModuleCode::as_str), Some("t0ac"));
    /// assert_eq!(ModuleCode::new("nope!"), None);
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

/// Builds a [`ModuleCode`] from a constant, checked while the crate is
/// compiled.
///
/// The check runs inside a `const` block, so a code that is not exactly four
/// characters of `[a-z0-9]` fails the build instead of panicking at run time,
/// and the value the macro produces needs no unwrapping. Every module code in
/// this workspace is written through this macro, which is what makes
/// [`ModuleCode::new`]'s fallible form safe to hand to anyone else.
///
/// # Examples
///
/// ```
/// use trusted_server_core::module_code;
///
/// assert_eq!(module_code!("t0ac").as_str(), "t0ac");
/// ```
#[macro_export]
macro_rules! module_code {
    ($code:expr) => {
        const {
            match $crate::ec::module::ModuleCode::new($code) {
                Some(code) => code,
                None => panic!("module code must be exactly four characters of [a-z0-9]"),
            }
        }
    };
}

/// The separator between a module code and the module's identifier value.
///
/// The tilde is inside the cookie-safe identifier alphabet and outside the
/// built-in HMAC identifier's own characters, so a legacy bare identifier can
/// never be misread as a coded one.
pub const MODULE_CODE_SEPARATOR: char = '~';

/// Splits a full identifier into its module-code prefix and value.
///
/// Returns `(Some(code), value)` when the identifier starts with a well-formed
/// `{code}~` prefix, and `(None, full)` for a legacy bare identifier. The code
/// here is the raw string, not a validated [`ModuleCode`]: an unknown code
/// simply fails the ownership check against the selected module.
#[must_use]
pub fn split_module_code(full: &str) -> (Option<&str>, &str) {
    if let Some((code, value)) = full.split_once(MODULE_CODE_SEPARATOR)
        && code.len() == 4
        && code
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    {
        return (Some(code), value);
    }
    (None, full)
}

/// Whether the selected module owns `full` as one of its identifiers.
///
/// A coded identifier belongs to the module whose registered code it
/// carries, with the value part accepted by that module's
/// [`accepts_id`](EdgeCookieModule::accepts_id). An identifier with no code
/// prefix is the built-in HMAC module's older form, still held in browsers,
/// so the HMAC module alone owns it.
#[must_use]
pub fn module_owns_id(module: &dyn EdgeCookieModule, full: &str) -> bool {
    match split_module_code(full) {
        (Some(code), value) => code == module.code().as_str() && module.accepts_id(value),
        (None, value) => module.id() == HMAC_MODULE_KEY && module.accepts_id(value),
    }
}

/// The full created identifier for `value` under `module`'s code.
#[must_use]
pub fn apply_module_code(module: &dyn EdgeCookieModule, value: &str) -> String {
    format!("{}{MODULE_CODE_SEPARATOR}{value}", module.code())
}

/// The KV-key form of a full identifier under `module`.
///
/// The code prefix is preserved verbatim and the module normalizes only its
/// own value part, so distinct modules' rows can never share a key and a
/// module never sees another module's syntax.
#[must_use]
pub fn module_kv_key(module: &dyn EdgeCookieModule, full: &str) -> String {
    match split_module_code(full) {
        (Some(code), value) => format!(
            "{code}{MODULE_CODE_SEPARATOR}{}",
            module.normalize_id_for_kv(value)
        ),
        (None, value) => module.normalize_id_for_kv(value),
    }
}

/// The modules whose identifiers a partner or diagnostic path accepts.
///
/// Pull sync, batch sync, and the admin lookup each take an identifier from
/// outside the organic request path and have to decide whether Trusted Server
/// issued it. The answer is in two parts. The **global cookie bounds** (the
/// length cap and the cookie-safe alphabet, see `ec_id_has_only_allowed_chars`)
/// apply to every identifier whichever module created it. The rest is
/// **dispatched by the `{code}~` prefix** to the module that owns that code,
/// which canonicalizes its own value part and decides whether the canonical
/// form is one of its own. A code no module in the set owns is rejected, so a
/// second module's identifiers can never be adopted or written under this
/// deployment's keys.
///
/// All three look rows up under the key
/// [`canonical_kv_key`](Self::canonical_kv_key) returns rather than under the
/// identifier as given, and pull sync and batch sync also write under that
/// key, so a module whose canonical form differs from the cookie value still
/// reaches the row it created. Batch sync and the admin lookup call
/// `canonical_kv_key` directly. Pull sync calls `canonical_kv_key` through
/// `EcContext::kv_key_for` and still sends partners the identifier as issued.
///
/// The set holds the deployment's active module, so an identifier another
/// module created is rejected, one created under an earlier selection
/// included. A stateless deployment, with no module in the set, falls back
/// to the built-in HMAC grammar (see
/// [`canonical_kv_key`](Self::canonical_kv_key)).
pub struct AcceptedModules<'a> {
    readers: Vec<&'a dyn EdgeCookieModule>,
}

impl<'a> AcceptedModules<'a> {
    /// The set holding only the deployment's active module.
    ///
    /// `None` means no module is selected, so the deployment is stateless.
    #[must_use]
    pub fn active(module: Option<&'a dyn EdgeCookieModule>) -> Self {
        Self {
            readers: module.into_iter().collect(),
        }
    }

    /// The module in the set that owns `full`'s code.
    ///
    /// Dispatch is on the code alone, before any module looks at a value, so
    /// an identifier a partner echoed back in a different case still reaches
    /// its own module to be canonicalized rather than being rejected first.
    /// A legacy bare identifier predates the envelope and belongs to the
    /// built-in HMAC module alone.
    fn owner(&self, full: &str) -> Option<&'a dyn EdgeCookieModule> {
        let (code, _) = split_module_code(full);
        self.readers.iter().copied().find(|module| match code {
            Some(code) => module.code().as_str() == code,
            None => module.id() == HMAC_MODULE_KEY,
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
    /// The owning module supplies the canonical form of its own value part
    /// and the code prefix is preserved verbatim, so two modules' rows can
    /// never share a key.
    #[must_use]
    pub fn canonical_kv_key(&self, full: &str) -> Option<String> {
        if !ec_id_has_only_allowed_chars(full) {
            return None;
        }
        match self.owner(full) {
            Some(owner) => {
                let key = module_kv_key(owner, full);
                module_owns_id(owner, &key).then_some(key)
            }
            // No module is selected, so there is no code to dispatch on and
            // the built-in HMAC grammar is the fallback for a stateless
            // deployment.
            None if self.readers.is_empty() => {
                let key = generation::normalize_ec_id_for_kv(full);
                generation::is_valid_ec_id(&key).then_some(key)
            }
            // A code that belongs to some other deployment's module.
            None => None,
        }
    }
}

/// A strategy for deriving an Edge Cookie identifier.
///
/// Implementations are selected by configuration and come in two types, which
/// reach the same outcome (a `ts-ec` cookie) by different routes:
///
/// - **Server-side** (for example [`HmacModule`]): derives the identifier at
///   the edge in [`generate`](Self::generate), and the page response sets the
///   cookie. Nothing client-side is involved.
/// - **Client-side** (for example `ClientFixedProvider`): defers in
///   [`generate`](Self::generate) (returns `id: None`), runs its own JavaScript
///   in the browser, and creates the identifier from the value the page posts
///   back in
///   [`resolve_from_client`](Self::resolve_from_client), whose response sets the
///   cookie.
///
/// A module that cannot derive an identifier at the edge returns a
/// [`GeneratedEdgeCookie`] whose [`id`](GeneratedEdgeCookie::id) is `None`, so
/// the request proceeds without an Edge Cookie rather than failing.
pub trait EdgeCookieModule: Send + Sync + core::fmt::Debug {
    /// Returns the stable implementation id for this module, used in
    /// configuration and logs.
    ///
    /// This is what `[ec] module` selects the module by, or what an
    /// `[ec.<name>] implementation` names when the module is configured
    /// under a label of the operator's choosing.
    fn id(&self) -> &'static str;

    /// The module's registered code, the `{code}~` namespace of every
    /// identifier it creates.
    ///
    /// Mandatory, with no default: a module must allocate a unique code in
    /// the module-code registry before it can exist, so no two modules
    /// can ever create colliding identifiers. Core applies the code at
    /// creation and checks it at read-back, and the module itself only ever
    /// sees its own value part.
    fn code(&self) -> ModuleCode;

    /// Whether this module was built from evidence about one request.
    ///
    /// Almost every module is built from configuration and services that are
    /// the same for every request, so one instance can be resolved once and
    /// handed to all of them. [`HostSignalModule`] is the exception, because
    /// it is built from the TLS and HTTP/2 signals of a single request and
    /// answers `true` here. A composition root reads this through
    /// [`build_reusable_module`] to decide whether keeping the instance is
    /// safe, and keeping a request-scoped one would serve every later request
    /// from the first request's evidence.
    ///
    /// The default is `false`, which is right for a module whose constructor
    /// takes only configuration and long-lived services.
    fn is_request_scoped(&self) -> bool {
        false
    }

    /// Derives an Edge Cookie identifier from the request evidence in
    /// `request_info` and the gating context in `input`.
    ///
    /// A server-side provider creates here. A client-side provider defers here
    /// (returns `id: None`) and creates later in
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

    /// Returns whether `value` is a well-formed identifier this module issues.
    ///
    /// Core calls this to decide whether an incoming `ts-ec` cookie value is a
    /// usable Edge Cookie identifier before reading it back, keying the KV
    /// identity graph, or withdrawing it. Core strips the module's `{code}~`
    /// prefix first, so this receives only the module's own value part.
    /// This keeps the identifier opaque to
    /// core: a module whose identifiers are not the built-in shape (for
    /// example an opaque signed envelope) accepts its own format here, so its
    /// identifier round-trips instead of being silently dropped on read-back.
    ///
    /// The default accepts the built-in HMAC identifier shape
    /// (`<64 hex>.<6 alphanumeric>`), which is correct for [`HmacModule`], the
    /// one module core builds in.
    fn accepts_id(&self, value: &str) -> bool {
        generation::is_valid_ec_id(value)
    }

    /// Returns the KV-key form of `value` for this module's identifiers.
    ///
    /// Core keys the identity graph by the returned string, so a module whose
    /// identifiers are case-sensitive or carry no separable segments returns the
    /// value unchanged to avoid collapsing distinct identifiers into one key.
    ///
    /// The default lowercases the leading HMAC hash segment and preserves the
    /// suffix, matching the built-in identifier shape.
    fn normalize_id_for_kv(&self, value: &str) -> String {
        generation::normalize_ec_id_for_kv(value)
    }

    /// The permissions this module's data use requires.
    ///
    /// Trusted Server executes the module only when every permission returned
    /// here is set. The default is empty, so a vendor-neutral module requires
    /// no permission. A module that stores identity on the device, or shares it
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
    /// returns no identifier, so a provider that creates entirely server-side
    /// (such as [`HmacModule`]) need not implement it.
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

/// The built-in HMAC Edge Cookie module.
///
/// Derives the identifier from the client IP (read from the [`RequestInfo`]
/// passed at call time) and the configured passphrase via
/// [`generation::generate_ec_id`].
///
/// The client IP is this module's only input, so it is this module that
/// requires one. On a host that cannot supply one, [`RequestInfo::client_ip`]
/// is the empty string and [`generate`](Self::generate) fails rather than
/// hashing the empty string into an identifier every visitor on that host
/// would share. The failure is returned to the caller. The publisher proxy and
/// integration proxy log it and serve the response without an Edge Cookie. A
/// module that reads other evidence makes its own decision and is unaffected.
#[derive(Debug, Clone)]
pub struct HmacModule {
    passphrase: Redacted<String>,
}

impl HmacModule {
    /// Creates an HMAC module with the given passphrase.
    #[must_use]
    pub fn new(passphrase: Redacted<String>) -> Self {
        Self { passphrase }
    }
}

impl EdgeCookieModule for HmacModule {
    fn id(&self) -> &'static str {
        HMAC_MODULE_KEY
    }

    fn code(&self) -> ModuleCode {
        HMAC_MODULE_CODE
    }

    fn generate(
        &self,
        request_info: &dyn RequestInfo,
        _input: &IdentityInput<'_>,
    ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
        let client_ip = request_info.client_ip();
        if client_ip.is_empty() {
            return Err(Report::new(TrustedServerError::EdgeCookie {
                message: "Edge Cookie module `hmac` requires the client IP, and this host \
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

    fn required_permissions(&self) -> PermissionSet {
        // The HMAC module writes the Edge Cookie to the device, so it requires
        // permission to store on the device (TCF Purpose 1). Whether that needs a
        // signal is decided by the country rules, not by the module.
        PermissionSet::none().with(Permission::StoreOnDevice)
    }
}

/// The built-in host-signal Edge Cookie module.
///
/// Derives the identifier from the host signals (TLS JA4 and HTTP/2, read
/// from the injected [`HostSignals`]) plus the client IP (from [`RequestInfo`]),
/// keyed by the configured passphrase. It is host-agnostic: it depends on the
/// `HostSignals` capability, so any host that supplies one can use it. A host
/// that supplies no `HostSignals` cannot build it, and the request stops.
#[derive(Debug, Clone)]
pub struct HostSignalModule {
    passphrase: Redacted<String>,
    host_signals: Arc<dyn HostSignals>,
}

impl HostSignalModule {
    /// Creates the module with the passphrase and its injected host signals.
    #[must_use]
    pub fn new(passphrase: Redacted<String>, host_signals: Arc<dyn HostSignals>) -> Self {
        Self {
            passphrase,
            host_signals,
        }
    }
}

impl EdgeCookieModule for HostSignalModule {
    fn id(&self) -> &'static str {
        HOST_SIGNALS_MODULE_KEY
    }

    // Built from the signals of one request, so it is only ever valid for
    // that request and must never be kept and reused.
    fn is_request_scoped(&self) -> bool {
        true
    }

    fn code(&self) -> ModuleCode {
        crate::module_code!("hs00")
    }

    fn generate(
        &self,
        request_info: &dyn RequestInfo,
        _input: &IdentityInput<'_>,
    ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
        let ja4 = self.host_signals.ja4().unwrap_or_default();
        let h2 = self.host_signals.h2().unwrap_or_default();
        // With no signal at all, creating an identifier would silently degrade
        // to an IP-only identifier under the `host_signals` name. Defer
        // instead, meaning no identity this request, and the request proceeds.
        if ja4.is_empty() && h2.is_empty() {
            log::warn!("The host_signals EC module found no TLS/HTTP-2 signals and is deferring");
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
        // (TCF Purpose 1), the same gate as the HMAC module.
        PermissionSet::none().with(Permission::StoreOnDevice)
    }
}

/// The fixed, known word shared by [`ClientFixedProvider`] and its page script.
///
/// Kept cookie-safe (no characters [`set_ec_cookie`] would reject) so
/// it can be used as the Edge Cookie value verbatim. The page script posts this
/// exact string; the module creates only when the posted value matches. The
/// client copy lives in
/// `crates/trusted-server-js/lib/src/integrations/ec_client_fixed`.
///
/// [`set_ec_cookie`]: super::cookies::set_ec_cookie
#[cfg(any(test, feature = "client-fixed-demo"))]
const EXPECTED_VALUE: &str = "an-ec";

/// A demonstration client-side module, with no vendor coupling.
///
/// Client and server share one fixed, known word (`EXPECTED_VALUE`). When no
/// Edge Cookie is present the page script (delivered through the tsjs bundle)
/// posts that word to `POST /_ts/api/v1/ec/resolve`, and this module creates the
/// Edge Cookie only when the posted value matches. It defers from
/// [`generate`](EdgeCookieModule::generate) so the page renders with no Edge
/// Cookie until the client reports back, then verifies and creates in
/// [`resolve_from_client`](EdgeCookieModule::resolve_from_client).
///
/// The value is verifiable precisely because it is a known constant, which is
/// the point of the demo: it exercises verify-before-create. It is useless in
/// production, because a fixed value is not an identity and every client posts
/// the same word, so it is for demonstration and testing only. A real
/// client-side module verifies a real payload (for example an OWID signature)
/// instead of a shared constant.
#[derive(Debug, Clone)]
#[cfg(any(test, feature = "client-fixed-demo"))]
pub struct ClientFixedProvider;

#[cfg(any(test, feature = "client-fixed-demo"))]
impl EdgeCookieModule for ClientFixedProvider {
    fn id(&self) -> &'static str {
        CLIENT_FIXED_PROVIDER_KEY
    }

    fn code(&self) -> ModuleCode {
        crate::module_code!("cfix")
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
        // Verify the posted value against the known shared word, then create it as
        // the Edge Cookie. A value that does not match yields no Edge Cookie.
        // This stands in for a real module's verification (for example
        // checking a signature) before it trusts a client-supplied value.
        let matches = core::str::from_utf8(input.payload)
            .map(str::trim)
            .is_ok_and(|value| value == EXPECTED_VALUE);

        Ok(GeneratedEdgeCookie {
            id: matches.then(|| EXPECTED_VALUE.to_owned()),
            response_headers: Vec::new(),
        })
    }

    fn normalize_id_for_kv(&self, value: &str) -> String {
        // The fixed word has no dot separator, so the built-in default (which
        // normalizes the HMAC `<hash>.<suffix>` shape) would corrupt it as a
        // KV key. Like any opaque-identifier module, the value is the key.
        value.to_owned()
    }

    fn required_permissions(&self) -> PermissionSet {
        // The module writes the resolved value to the device as the Edge
        // Cookie, so it requires necessary.operations.storage (TCF Purpose 1), the same gate
        // as the HMAC module.
        PermissionSet::none().with(Permission::StoreOnDevice)
    }
}

/// Refuses an injected module that claims an implementation core supplies
/// itself.
///
/// An adapter may inject a module whose id is also a built-in id, and the
/// resolution order alone would silently prefer the built-in one and drop the
/// injected module, so the pair is refused and the error names both
/// claimants. The check runs before the selection is read, so the clash is
/// reported at startup whatever the selector says, and selecting something
/// else cannot hide it.
///
/// # Errors
///
/// Returns [`TrustedServerError::EdgeCookie`] when the injected module's id
/// is one of [`BUILTIN_MODULE_KEYS`].
fn ensure_no_name_collision(
    injected: Option<&dyn EdgeCookieModule>,
) -> Result<(), Report<TrustedServerError>> {
    let Some(injected) = injected else {
        return Ok(());
    };
    let Some(claimed) = BUILTIN_MODULE_KEYS
        .iter()
        .find(|key| **key == injected.id())
    else {
        return Ok(());
    };
    Err(Report::new(TrustedServerError::EdgeCookie {
        message: format!(
            "Edge Cookie module implementation `{claimed}` is claimed twice, by the \
             module built into Trusted Server core and by the module this \
             deployment's adapter injects. Give the injected module an implementation \
             of its own and select it under that, because `{claimed}` cannot mean both \
             of them."
        ),
    }))
}

/// Builds the Edge Cookie module named by the `[ec] module` selector.
///
/// This is the composition root for the built-in modules: the adapter supplies
/// the [`HostSignals`] when the host can produce them, and this constructs the
/// selected module. The per-request [`RequestInfo`] is passed borrowed to
/// [`generate`](EdgeCookieModule::generate) at call time rather than stored, so
/// no request snapshot is cloned here. Returns `Ok(None)` when no module is
/// selected, so the caller stays stateless.
///
/// # Errors
///
/// Returns [`TrustedServerError::EdgeCookie`] when the named module cannot be
/// built, which is a built-in implementation whose configuration block is
/// missing, a built-in implementation whose host capability this host does not
/// supply, or an implementation this deployment's adapter does not inject. All
/// fail loudly rather than leaving the deployment running stateless under a
/// selector that says otherwise.
pub fn build_module(
    ec: &Ec,
    host_signals: Option<Arc<dyn HostSignals>>,
    injected: Option<Arc<dyn EdgeCookieModule>>,
) -> Result<Option<Box<dyn EdgeCookieModule>>, Report<TrustedServerError>> {
    ensure_no_name_collision(injected.as_deref())?;
    let Some(selection) = ec.module.as_ref() else {
        return Ok(None);
    };
    let module: Option<Box<dyn EdgeCookieModule>> = match selection {
        // Explicit statelessness: the same meaning as omitting the selector.
        EcModuleSelection::None => None,
        EcModuleSelection::Named(name) => {
            Some(resolve_named_module(name, ec, host_signals, injected)?)
        }
    };
    Ok(module)
}

/// Resolves one module name to its implementation.
///
/// The implementation is the one the name's `[ec.<name>]` block names, or the
/// name itself. It is looked for among the modules built into core first,
/// and is otherwise the module the adapter injects through
/// [`RuntimeServices`](crate::platform::RuntimeServices) when the
/// implementation names that module's id, so resolving an injected module
/// needs no vendor name in core. The adapter reads the injected module's
/// block when it builds it. Looking at core first cannot shadow an injected
/// module, because [`ensure_no_name_collision`] has already refused one that
/// claims a built-in implementation.
///
/// # Errors
///
/// Returns [`TrustedServerError::EdgeCookie`] when the implementation matches
/// no module this deployment can build, naming the implementations it has,
/// when a built-in implementation has no configuration block, or when a
/// built-in implementation needs a host capability this host does not supply.
fn resolve_named_module(
    name: &str,
    ec: &Ec,
    host_signals: Option<Arc<dyn HostSignals>>,
    injected: Option<Arc<dyn EdgeCookieModule>>,
) -> Result<Box<dyn EdgeCookieModule>, Report<TrustedServerError>> {
    let implementation = ec.module_blocks.implementation(name);

    // Settings validation rejects a built-in implementation with no block
    // before this runs, so reaching the error means the two checks have
    // drifted apart. Stopping is the only safe answer, because returning no
    // module would run the deployment stateless under a selector that says
    // it has an identity module.
    if implementation == HMAC_MODULE_KEY {
        let config = ec
            .module_blocks
            .get(name)
            .and_then(EcModuleBlock::hmac_settings)
            .ok_or_else(|| {
                Report::new(TrustedServerError::EdgeCookie {
                    message: format!(
                        "Edge Cookie module `{name}` uses the `hmac` implementation but \
                         has no `[ec.{name}]` configuration"
                    ),
                })
            })?;
        return Ok(Box::new(HmacModule::new(config.passphrase.clone())));
    }

    // The host-signal module needs signals only some hosts supply, and
    // that check cannot be made in settings validation at all, so it is made
    // here rather than creating a degraded identifier under this name.
    if implementation == HOST_SIGNALS_MODULE_KEY {
        let config = ec
            .module_blocks
            .get(name)
            .and_then(EcModuleBlock::host_signals_settings)
            .ok_or_else(|| {
                Report::new(TrustedServerError::EdgeCookie {
                    message: format!(
                        "Edge Cookie module `{name}` uses the `host_signals` implementation \
                         but has no `[ec.{name}]` configuration"
                    ),
                })
            })?;
        let signals = host_signals.ok_or_else(|| {
            Report::new(TrustedServerError::EdgeCookie {
                message: "The host_signals Edge Cookie module requires a host that supplies \
                          TLS/HTTP-2 signals, which this host does not"
                    .to_owned(),
            })
        })?;
        return Ok(Box::new(HostSignalModule::new(
            config.passphrase.clone(),
            signals,
        )));
    }

    // The `client_fixed` demonstration provider takes no configuration block and
    // no services, so it is built whenever it is selected. A fixed shared word
    // is not an identity, so it is compiled only into test and demonstration
    // builds and a build without it refuses the name rather than substituting
    // anything. `check_named_provider_configuration` refuses the same name at
    // startup, so reaching this error means the two have drifted apart.
    if implementation == CLIENT_FIXED_PROVIDER_KEY {
        #[cfg(any(test, feature = "client-fixed-demo"))]
        return Ok(Box::new(ClientFixedProvider));
        #[cfg(not(any(test, feature = "client-fixed-demo")))]
        return Err(Report::new(TrustedServerError::EdgeCookie {
            message: "The `client_fixed` demo Edge Cookie provider is not compiled into this \
                      build. It is for demonstration and testing only; enable the \
                      trusted-server-core `client-fixed-demo` cargo feature to use it"
                .to_owned(),
        }));
    }

    let known = known_implementations(injected.as_deref());
    injected
        .filter(|module| module.id() == implementation)
        .map(|module| Box::new(SharedModule(module)) as Box<dyn EdgeCookieModule>)
        .ok_or_else(|| {
            Report::new(TrustedServerError::EdgeCookie {
                message: format!(
                    "Edge Cookie module `{name}` is selected, but its implementation \
                     `{implementation}` is not one this deployment has. Known \
                     implementations: {known}"
                ),
            })
        })
}

/// Checks that `implementation` names a provider this build compiles in, as
/// far as the configuration on its own can answer.
///
/// The startup counterpart to [`resolve_named_module`], and the reason
/// configuration validation does not decide on its own whether every selection
/// can be honored. Whether a name is compiled into this build is the
/// resolution's knowledge, not the settings', because a build that does not
/// compile the `client_fixed` demonstration provider in cannot honor that name
/// however it is configured. The arm lives here beside the resolution it
/// belongs to, and goes with it when that provider becomes a module.
///
/// Whether a selection needs a settings block is answered by
/// [`Ec::validate_module_selection`], which knows which implementations
/// built into core take settings. The demonstration provider takes none, so it
/// is configured correctly with no block at all.
///
/// Whether the host supplies a capability a provider needs is not answerable
/// from configuration, so it is not asked here. [`ensure_module_available`]
/// asks that, with the services the adapter injects.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when `implementation` is not
/// compiled into this build.
pub(crate) fn check_named_provider_configuration(
    implementation: &str,
) -> Result<(), Report<TrustedServerError>> {
    // The one name a production build does not supply at all, which no amount
    // of configuration can fix. Rejecting it here rather than when the
    // provider is built means an operator finds out at startup instead of on
    // the first request.
    if implementation == CLIENT_FIXED_PROVIDER_KEY {
        #[cfg(any(test, feature = "client-fixed-demo"))]
        return Ok(());
        #[cfg(not(any(test, feature = "client-fixed-demo")))]
        return Err(Report::new(TrustedServerError::Configuration {
            message: "[ec] provider = \"client_fixed\" selects the demonstration provider, \
                      which is not compiled into this build. Enable the trusted-server-core \
                      `client-fixed-demo` cargo feature for demonstrations"
                .to_owned(),
        }));
    }

    Ok(())
}

/// The implementations this deployment could build, for an error that has just
/// refused one it could not.
///
/// The modules built into core, plus the one the adapter injects when there
/// is one, which is the whole set [`resolve_named_module`] chooses from.
fn known_implementations(injected: Option<&dyn EdgeCookieModule>) -> String {
    BUILTIN_MODULE_KEYS
        .iter()
        .copied()
        .chain(injected.map(EdgeCookieModule::id))
        .map(|implementation| format!("`{implementation}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Checks once, at startup, that this deployment can build the module named
/// by the `[ec] module` selector.
///
/// The composition root calls this while it builds application state, passing
/// the same services it will put into
/// [`RuntimeServices`](crate::platform::RuntimeServices) on every request.
/// [`build_module`] reads no request data, so the answer is the same for
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
/// Returns [`TrustedServerError::EdgeCookie`] when the selected module cannot
/// be built from the services this deployment injects.
pub fn ensure_module_available(
    ec: &Ec,
    host_signals: Option<Arc<dyn HostSignals>>,
    injected: Option<Arc<dyn EdgeCookieModule>>,
) -> Result<(), Report<TrustedServerError>> {
    build_shared_module(ec, host_signals, injected)?;
    Ok(())
}

/// Resolves the selected module into a shared handle.
///
/// The same resolution as [`build_module`], returned as an `Arc` rather than
/// a `Box` so one instance can be held in
/// [`RuntimeServices`](crate::platform::RuntimeServices) and read by every
/// request. Use [`build_reusable_module`] at a composition root, which adds
/// the one check that decides whether keeping the instance is safe.
///
/// # Errors
///
/// The same errors as [`build_module`].
pub fn build_shared_module(
    ec: &Ec,
    host_signals: Option<Arc<dyn HostSignals>>,
    injected: Option<Arc<dyn EdgeCookieModule>>,
) -> Result<Option<Arc<dyn EdgeCookieModule>>, Report<TrustedServerError>> {
    Ok(build_module(ec, host_signals, injected)?.map(Arc::from))
}

/// The module a composition root may keep and hand to every request, when
/// the selection is one that can be kept at all.
///
/// Resolving is also the startup check, so a selection this deployment cannot
/// satisfy fails here rather than on the first request, exactly as
/// [`ensure_module_available`] makes it fail. What this adds is the answer to
/// a second question, which is whether the module that came back is the same
/// for every request. Most are, because they are built from configuration
/// alone, and keeping one saves resolving the same settings again on every
/// request.
///
/// [`HostSignalModule`] is not, because it is built from the signals of
/// one request and reports
/// [`is_request_scoped`](EdgeCookieModule::is_request_scoped). Keeping that
/// one would freeze the signals captured while application state was built,
/// which on every adapter here are empty, so every later request would find no
/// signals and defer. `Ok(None)` comes back for it, the adapter threads
/// nothing, and the request path resolves it per request against that request's
/// own signals.
///
/// `Ok(None)` therefore means "nothing to keep", which covers both a stateless
/// deployment and a module that must be resolved per request. Both leave the
/// request path resolving for itself, which is what it did before anything was
/// kept.
///
/// # Errors
///
/// The same errors as [`build_module`].
pub fn build_reusable_module(
    ec: &Ec,
    host_signals: Option<Arc<dyn HostSignals>>,
    injected: Option<Arc<dyn EdgeCookieModule>>,
) -> Result<Option<Arc<dyn EdgeCookieModule>>, Report<TrustedServerError>> {
    let Some(module) = build_shared_module(ec, host_signals, injected)? else {
        return Ok(None);
    };
    if module.is_request_scoped() {
        log::debug!(
            "Edge Cookie module `{}` is built from request evidence, so it is resolved per              request rather than kept",
            module.id(),
        );
        return Ok(None);
    }
    Ok(Some(module))
}

/// The Edge Cookie module to use for this request.
///
/// A module reaches the request path through one seam only. An adapter
/// resolves `[ec] module` once while it builds application state and threads
/// the answer into
/// [`RuntimeServices::resolved_ec_module`](crate::platform::RuntimeServices::resolved_ec_module),
/// and that same instance comes back here with nothing resolved or constructed
/// again on the request path. When nothing was threaded, this builds from
/// `[ec]` settings alone, which is what a deployment selecting only a built-in
/// module does.
///
/// # Errors
///
/// The same errors as [`build_module`], and only when nothing was threaded,
/// because a threaded module has already been resolved successfully.
pub fn request_module(
    ec: &Ec,
    services: &crate::platform::RuntimeServices,
) -> Result<Option<Arc<dyn EdgeCookieModule>>, Report<TrustedServerError>> {
    if let Some(resolved) = services.resolved_ec_module() {
        return Ok(Some(resolved));
    }
    build_shared_module(ec, services.host_signals(), None)
}

/// Adapts an injected, shared [`EdgeCookieModule`] to the owned `Box` that
/// [`build_module`] returns.
///
/// A vendor or host module is injected as an `Arc` so it can live in
/// [`RuntimeServices`](crate::platform::RuntimeServices) and be cloned per
/// request. Every method delegates to the inner module, so its behavior is
/// unchanged.
#[derive(Debug)]
struct SharedModule(Arc<dyn EdgeCookieModule>);

impl EdgeCookieModule for SharedModule {
    fn code(&self) -> ModuleCode {
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

    fn required_permissions(&self) -> PermissionSet {
        self.0.required_permissions()
    }

    fn resolve_from_client(
        &self,
        input: &ClientResolveInput<'_>,
    ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
        self.0.resolve_from_client(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::OwnedRequestInfo;
    use crate::test_support::tests::{select_hmac_module, select_host_signals_module};
    use http::HeaderMap;

    /// Settings selecting the built-in HMAC module under `name`, which is a
    /// label whenever it is not the implementation's own name.
    fn selected_hmac(name: &str) -> Ec {
        let mut ec = Ec::default();
        select_hmac_module(&mut ec, name, test_passphrase().expose());
        ec
    }

    /// Settings selecting the built-in host-signal module under its own
    /// name, with `passphrase` in its block.
    fn selected_host_signals(passphrase: &str) -> Ec {
        let mut ec = Ec::default();
        select_host_signals_module(&mut ec, passphrase);
        ec
    }

    #[test]
    fn a_malformed_module_code_is_refused_rather_than_panicking() {
        // `ModuleCode::new` is public, so a vendor crate can reach it with a
        // value it assembled rather than a literal. Every rejected shape has to
        // come back as `None`, because a panic here would take down whatever
        // request the caller was serving.
        for malformed in ["", "abc", "abcde", "AB12", "t0a_", "t0a-", "t0a ", "t.ac"] {
            assert_eq!(
                ModuleCode::new(malformed),
                None,
                "`{malformed}` is outside the registry format and should be refused"
            );
        }

        assert_eq!(
            ModuleCode::new("t0ac").map(ModuleCode::as_str),
            Some("t0ac"),
            "a well-formed code should still be accepted"
        );
    }

    #[test]
    fn the_module_code_macro_keeps_the_compile_time_guarantee() {
        // The macro checks a literal while the crate is compiled and yields the
        // code itself, so the codes written across this workspace stay as
        // strong as the old panicking constructor made them, with none of the
        // run-time risk.
        assert_eq!(
            crate::module_code!("t0ac").as_str(),
            "t0ac",
            "the macro should yield the code it was given"
        );
        assert_eq!(
            HMAC_MODULE_CODE.as_str(),
            HMAC_MODULE_KEY,
            "the built-in code should still be the built-in key"
        );
    }

    #[test]
    fn split_module_code_separates_coded_and_legacy_forms() {
        assert_eq!(
            split_module_code("hmac~abc.DEF123"),
            (Some("hmac"), "abc.DEF123"),
            "a four-character code before the first tilde splits off"
        );
        assert_eq!(
            split_module_code("51dd~value~with~tildes"),
            (Some("51dd"), "value~with~tildes"),
            "only the first tilde splits, so a value may contain tildes"
        );
        assert_eq!(
            split_module_code("abcdef.XYZ"),
            (None, "abcdef.XYZ"),
            "no tilde means the legacy bare form"
        );
        assert_eq!(
            split_module_code("toolong~x"),
            (None, "toolong~x"),
            "a prefix that is not exactly four characters is not a code"
        );
        assert_eq!(
            split_module_code("AB12~x"),
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
    fn reserved_response_effect_allows_module_owned_effects() {
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
                "`{name}` is the module's own and should be allowed"
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

    /// A stand-in for a vendor module an adapter injects.
    #[derive(Debug)]
    struct VendorModule;

    impl EdgeCookieModule for VendorModule {
        fn id(&self) -> &'static str {
            "acme"
        }

        fn code(&self) -> ModuleCode {
            crate::module_code!("t0ac")
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
    fn accepted_modules_splits_global_bounds_from_module_dispatch() {
        let hmac = HmacModule::new(Redacted::new("test-secret-key-32-bytes-minimum".to_owned()));
        let hmac_value = format!("{}.ABC123", "a".repeat(64));
        let active = AcceptedModules::active(Some(&hmac));

        // The global bounds come first and apply whoever created the value. A
        // character outside the cookie-safe alphabet, or a value over the
        // length cap, never reaches a module.
        assert!(
            !active.accepts(&format!("hmac~{hmac_value} with spaces")),
            "the cookie-safe alphabet is a global bound"
        );
        assert!(
            !active.accepts(&format!("hmac~{}", "a".repeat(300))),
            "the length cap is a global bound"
        );

        // Then dispatch by code to the module that owns it.
        assert!(
            active.accepts(&format!("hmac~{hmac_value}")),
            "the active module's own code is accepted"
        );
        assert!(
            active.accepts(&hmac_value),
            "the legacy bare form belongs to the built-in module"
        );
        assert!(
            !active.accepts(&format!("t0ac~{hmac_value}")),
            "a code no configured module reads is rejected even in the HMAC shape"
        );

        // A vendor module's own identifiers are accepted when it is the
        // active one, and the built-in bare form then belongs to nobody.
        let vendor = AcceptedModules::active(Some(&VendorModule));
        assert!(
            vendor.accepts(&format!("t0ac~{hmac_value}")),
            "the vendor module's code is accepted when it is active"
        );
        assert!(
            !vendor.accepts(&hmac_value),
            "the legacy bare form is the built-in module's alone"
        );

        // With no module selected the deployment is stateless, so the
        // built-in grammar is the fallback, as it has always been.
        let stateless = AcceptedModules::active(None);
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
            (EcModuleSelection::NONE_KEY, EcModuleSelection::None),
            (
                HMAC_MODULE_KEY,
                EcModuleSelection::Named(HMAC_MODULE_KEY.to_owned()),
            ),
            ("acme", EcModuleSelection::Named("acme".to_owned())),
        ] {
            let ec: Ec = toml::from_str(&format!("module = \"{key}\""))
                .expect("should parse the [ec] section");
            assert_eq!(
                ec.module.as_ref(),
                Some(&expected),
                "`{key}` should select the module it names"
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
                written.contains(&format!("module = \"{key}\"")),
                "`{key}` should be written back unchanged, got: {written}"
            );

            // A full round trip through the document leaves the same choice.
            let reparsed: Ec = toml::from_str(&written).expect("should reparse the [ec] section");
            assert_eq!(
                reparsed.module.as_ref(),
                Some(&expected),
                "`{key}` should survive a serialize and parse round trip"
            );
        }
    }

    #[test]
    fn each_selection_builds_what_its_string_key_built_before() {
        // `none` is stateless, exactly as omitting the selector is.
        let none = Ec {
            module: Some(EcModuleSelection::None),
            ..Ec::default()
        };
        assert!(
            build_module(&none, None, None)
                .expect("explicit statelessness should build")
                .is_none(),
            "`none` should select no module"
        );

        // `hmac` with its block builds the built-in module.
        let hmac = selected_hmac(HMAC_MODULE_KEY);
        let built = build_module(&hmac, None, None)
            .expect("the hmac selection should build")
            .expect("the hmac selection should yield a module");
        assert_eq!(
            built.id(),
            HMAC_MODULE_KEY,
            "`hmac` should select the built-in module"
        );
        assert_eq!(
            built.code(),
            HMAC_MODULE_CODE,
            "the built-in module should carry the built-in code"
        );

        // An arbitrary vendor name selects the module the adapter injected
        // under that same implementation.
        let vendor = Ec {
            module: Some(EcModuleSelection::Named("acme".to_owned())),
            ..Ec::default()
        };
        let built = build_module(&vendor, None, Some(Arc::new(VendorModule)))
            .expect("the vendor selection should build")
            .expect("the vendor selection should yield a module");
        assert_eq!(
            built.id(),
            "acme",
            "a vendor name should select the injected module of that id"
        );
    }

    #[test]
    fn a_label_builds_the_implementation_its_block_names() {
        // The selector names a block, and the block names the implementation,
        // so everything that resolves the selection has to read the
        // implementation rather than the label the operator chose.
        let labeled_hmac = selected_hmac("primary");
        let built = build_module(&labeled_hmac, None, None)
            .expect("a labeled hmac block should build")
            .expect("a labeled hmac block should yield a module");
        assert_eq!(
            built.id(),
            HMAC_MODULE_KEY,
            "the label should build the implementation its block names"
        );
        assert_eq!(
            built.code(),
            HMAC_MODULE_CODE,
            "the identifiers it creates carry the implementation's own code"
        );

        // The same for a module the adapter injects, which is matched on the
        // implementation its block names and not on the label.
        let labeled_vendor: Ec = toml::from_str(
            "module = \"main\"\n\n[main]\nimplementation = \"acme\"\nendpoint = \"https://ec.acme.example.com\"\n",
        )
        .expect("should parse a labeled vendor block");
        let built = build_module(&labeled_vendor, None, Some(Arc::new(VendorModule)))
            .expect("a labeled vendor block should build")
            .expect("a labeled vendor block should yield a module");
        assert_eq!(
            built.id(),
            "acme",
            "the label should build the injected module its block names"
        );
    }

    #[test]
    fn module_ownership_follows_the_code() {
        let module = HmacModule::new(test_passphrase());
        let legacy = format!("{}.ABC123", "a".repeat(64));
        let coded = format!("hmac~{legacy}");
        let foreign = format!("zz00~{legacy}");
        assert!(
            module_owns_id(&module, &coded),
            "the module owns identifiers carrying its own code"
        );
        assert!(
            module_owns_id(&module, &legacy),
            "the built-in hmac module dual-reads the legacy bare form"
        );
        assert!(
            !module_owns_id(&module, &foreign),
            "an identifier with another module's code is never owned"
        );
    }
    use crate::permissions::PermissionMaps;
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
        let module = HmacModule::new(test_passphrase());

        // The default `accepts_id` accepts the built-in HMAC shape and rejects
        // anything else, so a built-in module's identifiers round-trip while an
        // opaque value is left to a module that overrides the check.
        let valid = format!("{}.{}", "a".repeat(64), "abc123");
        assert!(module.accepts_id(&valid), "should accept the HMAC shape");
        assert!(
            !module.accepts_id("not-hmac-shaped"),
            "should reject a non-HMAC identifier by default"
        );

        // The default `normalize_id_for_kv` lowercases the hash segment. This is
        // exactly the transform that would corrupt an opaque case-sensitive
        // identifier, which is why such a module overrides it.
        let mixed = format!("{}.{}", "A".repeat(64), "abc123");
        assert_eq!(
            module.normalize_id_for_kv(&mixed),
            format!("{}.{}", "a".repeat(64), "abc123"),
            "the default should lowercase the hash segment"
        );
    }

    #[test]
    fn shared_module_delegates_id_semantics_to_the_inner_module() {
        // `SharedModule` wraps an adapter-injected module. It must forward
        // every trait method to the inner module, including `accepts_id` and
        // `normalize_id_for_kv`; a wrapper that silently used the defaults would
        // drop an opaque vendor identifier on read-back. This guards that
        // delegation directly.
        #[derive(Debug)]
        struct Inner;

        impl EdgeCookieModule for Inner {
            fn id(&self) -> &'static str {
                "inner"
            }

            fn code(&self) -> ModuleCode {
                crate::module_code!("t0in")
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

        let shared = SharedModule(Arc::new(Inner));

        assert_eq!(shared.id(), "inner", "should delegate id");
        assert!(
            shared.accepts_id("opaque-ok"),
            "should delegate accepts_id acceptance to the inner module"
        );
        assert!(
            !shared.accepts_id("something-else"),
            "should delegate accepts_id rejection to the inner module"
        );
        assert_eq!(
            shared.normalize_id_for_kv("x"),
            "kv:x",
            "should delegate normalize_id_for_kv to the inner module"
        );
    }

    /// A vendor module that claims the name core already uses for its
    /// built-in HMAC module.
    #[derive(Debug)]
    struct VendorNamedHmacModule;

    impl EdgeCookieModule for VendorNamedHmacModule {
        fn id(&self) -> &'static str {
            HMAC_MODULE_KEY
        }

        fn code(&self) -> ModuleCode {
            crate::module_code!("t0vh")
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
    fn two_modules_claiming_one_name_are_refused_and_both_are_named() {
        // An adapter may inject a module called `hmac` while core supplies
        // one of its own. Resolution order alone would prefer the built-in one
        // and drop the injected one with nothing said, which is the fault this
        // guards.
        let hmac = selected_hmac(HMAC_MODULE_KEY);

        let err = build_module(&hmac, None, Some(Arc::new(VendorNamedHmacModule)))
            .expect_err("two modules claiming `hmac` should be refused");
        let message = err.to_string();
        assert!(
            message.contains(HMAC_MODULE_KEY),
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
            module: Some(EcModuleSelection::None),
            ..Ec::default()
        };
        let err = ensure_module_available(
            &selected_elsewhere,
            None,
            Some(Arc::new(VendorNamedHmacModule)),
        )
        .expect_err("the clash should be refused whatever the selector says");
        assert!(
            err.to_string().contains(HMAC_MODULE_KEY),
            "the startup check should name the contested name too, got: {err}"
        );

        // A vendor name of its own is unaffected.
        let vendor = Ec {
            module: Some(EcModuleSelection::Named("acme".to_owned())),
            ..Ec::default()
        };
        build_module(&vendor, None, Some(Arc::new(VendorModule)))
            .expect("a vendor module under its own name should still build");
    }

    #[test]
    fn hmac_module_requires_store_on_device() {
        let module = HmacModule::new(test_passphrase());
        let required = module.required_permissions();
        assert!(
            required.contains(Permission::StoreOnDevice),
            "the HMAC module writes a cookie, so it requires necessary.operations.storage"
        );
        assert!(
            !required.contains(Permission::SelectPersonalisedAds),
            "the HMAC module requires no advertising permissions"
        );
    }

    #[test]
    fn host_signal_module_mints_from_fingerprints_and_requires_store_on_device() {
        let signals = Arc::new(TestHostSignals {
            ja4: Some("t13d1516h2_8daaf6152771_e5627efa2ab1".to_owned()),
            h2: Some("1:65536;4:6291456".to_owned()),
        });
        let module = HostSignalModule::new(test_passphrase(), signals);
        let request_info = test_request_info();
        let generated = module
            .generate(&request_info, &IdentityInput::default())
            .expect("should generate");
        assert!(
            generated.id.is_some(),
            "the host-signal module should create an identifier from the signals"
        );
        assert!(
            module
                .required_permissions()
                .contains(Permission::StoreOnDevice),
            "the host-signal module writes a cookie, so it requires necessary.operations.storage"
        );
    }

    /// A minimal module that overrides nothing optional, used to prove the
    /// trait defaults.
    #[derive(Debug)]
    struct MinimalModule;

    impl EdgeCookieModule for MinimalModule {
        fn id(&self) -> &'static str {
            "minimal"
        }

        fn code(&self) -> ModuleCode {
            crate::module_code!("t0mi")
        }

        fn generate(
            &self,
            _request_info: &dyn RequestInfo,
            _input: &IdentityInput<'_>,
        ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
            Ok(GeneratedEdgeCookie::default())
        }

        fn accepts_id(&self, _value: &str) -> bool {
            true
        }
    }

    #[test]
    fn a_neutral_module_requires_no_permissions_by_default() {
        // MinimalModule does not override required_permissions, so it
        // inherits the trait default of none and requires no permission.
        assert!(
            MinimalModule.required_permissions().is_empty(),
            "a vendor-neutral module requires nothing by default"
        );
    }

    #[test]
    fn the_edge_cookie_gate_blocks_until_the_permission_is_set() {
        let required = HmacModule::new(test_passphrase()).required_permissions();
        // Empty maps with no default: every permission is the requires-signal
        // floor.
        let maps = PermissionMaps::empty();

        // No signal: the module's required permission is not set, so Trusted
        // Server would not commit the Edge Cookie.
        assert!(
            !maps.resolve(None, |_| false).all_set(required),
            "the floor should not run the Edge Cookie module without the permission set"
        );

        // A grant signal for necessary.operations.storage: the module's permission is now set.
        assert!(
            maps.resolve(None, |p| p == Permission::StoreOnDevice)
                .all_set(required),
            "the Edge Cookie module runs once necessary.operations.storage is set"
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
            "`client_fixed` should defer in generate, deriving no edge identifier"
        );
    }

    #[test]
    fn resolve_from_client_creates_only_from_a_verified_payload() {
        // `client_fixed` creates the Edge Cookie only from the known shared
        // word. `HmacModule` does not override `resolve_from_client`, so it
        // inherits the no-op default, because a server-side module takes no
        // part in the client cycle.
        /// One case: its name, the module, the posted payload and the
        /// identifier the module should create.
        type Case<'a> = (&'a str, &'a dyn EdgeCookieModule, &'a [u8], Option<&'a str>);

        let hmac = HmacModule::new(test_passphrase());
        let cases: [Case<'_>; 3] = [
            (
                "client_fixed with the known word",
                &ClientFixedProvider,
                EXPECTED_VALUE.as_bytes(),
                Some(EXPECTED_VALUE),
            ),
            (
                "client_fixed with another word",
                &ClientFixedProvider,
                b"not-the-word",
                None,
            ),
            ("hmac with any payload", &hmac, b"anything", None),
        ];
        for (case, module, payload, expected) in cases {
            let input = ClientResolveInput {
                payload,
                permissions: None,
                consent: None,
            };
            let generated = module
                .resolve_from_client(&input)
                .unwrap_or_else(|err| panic!("{case}: should resolve, got: {err}"));
            assert_eq!(
                generated.id.as_deref(),
                expected,
                "{case}: should create exactly what the module verifies"
            );
        }
    }

    #[test]
    fn client_fixed_requires_store_on_device() {
        assert!(
            ClientFixedProvider
                .required_permissions()
                .contains(Permission::StoreOnDevice),
            "`client_fixed` writes a cookie, so it requires necessary.operations.storage"
        );
    }

    #[test]
    fn the_fixed_word_and_marker_name_match_the_page_script() {
        // The demo page script and this module share the fixed word by
        // convention; the resolved-marker cookie name is likewise shared with
        // the script. Assert against the script source so a rename on either
        // side fails this test instead of silently breaking the round trip.
        let script = include_str!(
            "../../../trusted-server-js/lib/src/integrations/ec_client_fixed/index.ts"
        );
        assert!(
            script.contains(&format!("const FIXED_WORD = '{EXPECTED_VALUE}'")),
            "the page script's FIXED_WORD should match EXPECTED_VALUE"
        );
        assert!(
            script.contains(&format!(
                "const MARKER_COOKIE_NAME = '{}'",
                crate::constants::COOKIE_TS_EC_RESOLVED
            )),
            "the page script's marker cookie name should match COOKIE_TS_EC_RESOLVED"
        );
        // The page module declares the same permission the server-side
        // module requires, and checks it against the state the page is
        // handed, so the two declarations must name the same Data Use.
        assert!(
            script.contains(&format!(
                "const REQUIRED_PERMISSION = '{}'",
                Permission::StoreOnDevice.as_str()
            )),
            "the page script's REQUIRED_PERMISSION should match the module's declaration"
        );
    }

    #[test]
    fn host_signal_provider_defers_without_fingerprints() {
        let signals = Arc::new(TestHostSignals {
            ja4: None,
            h2: None,
        });
        let module = HostSignalModule::new(test_passphrase(), signals);
        let request_info = test_request_info();
        let generated = module
            .generate(&request_info, &IdentityInput::default())
            .expect("should generate");
        assert!(
            generated.id.is_none(),
            "with no host signals the module should defer rather than create an IP-only identifier"
        );
    }

    #[test]
    fn an_unknown_implementation_fails_naming_the_known_ones() {
        // A label hands the choice of implementation to its block, so a
        // mistyped implementation has to be refused by name, alongside the
        // implementations this deployment could have used instead.
        let ec: Ec =
            toml::from_str("module = \"primary\"\n\n[primary]\nimplementation = \"hmca\"\n")
                .expect("should parse a labeled module block");

        let err = build_module(&ec, None, None)
            .expect_err("an implementation this deployment lacks should be refused");
        let message = err.to_string();
        assert!(
            message.contains("`hmca`"),
            "the error should name the unknown implementation, got: {message}"
        );
        assert!(
            message.contains("`hmac`"),
            "the error should name the built-in implementation, got: {message}"
        );

        let err = build_module(&ec, None, Some(Arc::new(VendorModule)))
            .expect_err("an injected module of another implementation should not stand in");
        let message = err.to_string();
        assert!(
            message.contains("`hmac`") && message.contains("`acme`"),
            "the error should name every implementation this deployment has, got: {message}"
        );
    }

    #[test]
    fn selecting_hmac_without_its_block_fails_loudly() {
        // `Ec::validate_module_selection` rejects this pair before settings
        // reach the composition root, so the state is built directly here to
        // reach the seam. If the two checks ever drift apart, `build_module`
        // must still stop rather than hand back a stateless deployment.
        let ec = Ec {
            module: Some(EcModuleSelection::from(HMAC_MODULE_KEY)),
            ..Ec::default()
        };

        let err = build_module(&ec, None, None)
            .expect_err("selecting hmac with no [ec.hmac] block should error");
        assert!(
            err.to_string().contains("[ec.hmac]"),
            "the error should name the missing block, got: {err}"
        );
    }

    #[test]
    fn a_module_built_from_request_evidence_is_never_kept_and_reused() {
        // The host-signal module captures the signals of the request it
        // was built for. A composition root builds application state with an
        // empty host-signal service, because there is no request yet, so
        // keeping that instance would serve every later request from empty
        // signals and the module would defer forever. It must come back
        // as nothing to keep, leaving the request path to resolve it against
        // the signals each request actually carried.
        let host_signals_selected = selected_host_signals(test_passphrase().expose());
        let startup_signals: Arc<dyn HostSignals> = Arc::new(TestHostSignals {
            ja4: None,
            h2: None,
        });

        // It resolves, so the startup check still passes on a host that
        // supplies the service.
        let resolved = build_shared_module(
            &host_signals_selected,
            Some(Arc::clone(&startup_signals)),
            None,
        )
        .expect("the host-signal selection should resolve on a host that supplies signals")
        .expect("the selection should yield a module");
        assert!(
            resolved.is_request_scoped(),
            "the host-signal module should declare itself built from request evidence"
        );

        // It is not offered for reuse.
        assert!(
            build_reusable_module(
                &host_signals_selected,
                Some(Arc::clone(&startup_signals)),
                None
            )
            .expect("the host-signal selection should still pass the startup check")
            .is_none(),
            "a module built from request evidence must never be kept for later requests"
        );

        // A module built from configuration alone is still kept, so the
        // saving stands for every selection that can take it.
        let hmac_selected = selected_hmac(HMAC_MODULE_KEY);
        let kept = build_reusable_module(&hmac_selected, None, None)
            .expect("the hmac selection should resolve")
            .expect("a module built from configuration alone should be kept");
        assert_eq!(
            kept.id(),
            HMAC_MODULE_KEY,
            "the kept module should be the selected one"
        );
    }

    #[test]
    fn the_request_path_reuses_the_module_the_composition_root_resolved() {
        // A composition root resolves the selection once while it builds
        // application state, which is the same work `build_module` does on a
        // request, so doing both means doing it twice for every request. The
        // resolved module is threaded into `RuntimeServices`, and this is the
        // assertion that the request path takes it rather than resolving again:
        // the same allocation, not merely an equal one.
        let ec = Ec {
            module: Some(EcModuleSelection::Named("acme".to_owned())),
            ..Ec::default()
        };
        let resolved = build_reusable_module(&ec, None, Some(Arc::new(VendorModule)))
            .expect("the composition root should resolve the selection")
            .expect("the selection should yield a module");

        let services =
            crate::platform::test_support::noop_services_with_ec_module(Arc::clone(&resolved));
        let for_request = request_module(&ec, &services)
            .expect("the request path should take the resolved module")
            .expect("the resolved module should be there");

        assert!(
            Arc::ptr_eq(&resolved, &for_request),
            "the request path should reuse the resolved module, not build a second one"
        );

        // With nothing threaded the request path builds the selection from the
        // settings. No module is injected on that path, so only a built-in
        // selection can be built there.
        let hmac = selected_hmac(HMAC_MODULE_KEY);
        let built = request_module(&hmac, &crate::platform::test_support::noop_services())
            .expect("an unthreaded request path should build a built-in selection")
            .expect("the selection should yield a module");
        assert_eq!(
            built.id(),
            HMAC_MODULE_KEY,
            "the request path should build the built-in module the settings select"
        );
    }

    #[test]
    fn an_uninjected_module_is_refused_and_statelessness_is_allowed() {
        // A selection the adapter cannot supply is knowable without a request,
        // so the composition root rejects it while application state is built.
        // The startup check wraps `build_module`, so both refuse it.
        let selected = Ec {
            module: Some(EcModuleSelection::from("acme")),
            ..Ec::default()
        };
        for (case, outcome) in [
            (
                "build_module",
                build_module(&selected, None, None).map(|_| ()),
            ),
            (
                "the startup check",
                ensure_module_available(&selected, None, None),
            ),
        ] {
            let Err(err) = outcome else {
                panic!("{case} should refuse a module the adapter does not inject");
            };
            assert!(
                err.to_string().contains("acme"),
                "{case}: the error should name the selected module, got: {err}"
            );
        }

        // Statelessness is a supported deployment, spelled either way, and must
        // never be turned into a startup error.
        ensure_module_available(&Ec::default(), None, None)
            .expect("should allow a deployment that selects no module");
        let explicit_none = Ec {
            module: Some(EcModuleSelection::None),
            ..Ec::default()
        };
        ensure_module_available(&explicit_none, None, None)
            .expect("should allow the explicit `none` selection");
    }

    #[test]
    fn the_startup_check_rejects_host_signals_on_a_host_that_supplies_none() {
        // Whether the adapter injects a host-signal service is fixed per
        // deployment, so selecting the host-signal module on an adapter that
        // injects none is knowable without a request.
        let selected = selected_host_signals(test_passphrase().expose());

        let err = ensure_module_available(&selected, None, None).expect_err(
            "the host-signal module should fail the startup check with no host signals",
        );
        assert!(
            err.to_string().contains("TLS/HTTP-2 signals"),
            "the error should say the host supplies no signals, got: {err}"
        );

        let signals: Arc<dyn HostSignals> = Arc::new(TestHostSignals {
            ja4: None,
            h2: None,
        });
        ensure_module_available(&selected, Some(signals), None).expect(
            "should pass on a host that injects host signals, whatever this request's signals are",
        );
    }

    #[test]
    fn the_configuration_check_and_the_resolution_agree_on_a_provider_with_no_block() {
        // The demonstration provider is configured correctly with no
        // `[ec.<name>]` block at all, so a check that demanded a block for
        // every selection rejected a valid deployment. The settings ask for a
        // block only from the implementations they know take settings, and
        // that has to agree with the resolution, because a startup check that
        // passes what the construction then refuses leaves the deployment
        // failing on its first request.
        let ec = Ec {
            provider: Some(EcModuleSelection::from(CLIENT_FIXED_PROVIDER_KEY)),
            ..Ec::default()
        };

        ec.validate_module_selection()
            .expect("`client_fixed` should validate with no configuration block");

        let built = build_module(&ec, None, None)
            .expect("`client_fixed` should build with no configuration and no services")
            .expect("`client_fixed` should yield a provider");
        assert_eq!(
            built.id(),
            CLIENT_FIXED_PROVIDER_KEY,
            "the built provider should be the one the selector names"
        );
    }

    #[test]
    fn a_name_that_needs_a_block_still_fails_without_one() {
        // Taking the block question out of the settings must not weaken it for
        // the names that do need a block.
        let ec = Ec {
            provider: Some(EcModuleSelection::from(HMAC_MODULE_KEY)),
            ..Ec::default()
        };

        let err = ec
            .validate_module_selection()
            .expect_err("hmac with no block should still fail at startup");
        assert!(
            err.to_string().contains("[ec.hmac]"),
            "the error should name the missing block, got: {err}"
        );
    }

    #[test]
    fn a_block_left_configured_alongside_a_blockless_provider_is_still_rejected() {
        // The unreferenced-block rule does not soften for a provider that
        // needs no block of its own, because a stale block is still a mistake.
        let mut ec = selected_hmac(HMAC_MODULE_KEY);
        ec.provider = Some(EcModuleSelection::from(CLIENT_FIXED_PROVIDER_KEY));

        let err = ec
            .validate_module_selection()
            .expect_err("a stray hmac block should still be rejected");
        assert!(
            err.to_string().contains("hmac"),
            "the error should name the unreferenced block, got: {err}"
        );
    }
}
