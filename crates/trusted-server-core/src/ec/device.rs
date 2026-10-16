//! Device signal derivation for bot detection and browser classification.
//!
//! The [`DeviceSignals`] derivation here is pure computation, with no KV I/O or
//! platform SDK calls. A [`DeviceModule`] is handed the request's module context
//! when `detect` is called and names what it reads from it, the
//! [`RequestInfo`] for the User-Agent among them. On a host that supplies
//! them, it also reads the [`HostSignals`](crate::evidence::HostSignals) for
//! the TLS and HTTP/2 signals injected into its constructor, then classifies
//! the request from both.
//!
//! # Signals
//!
//! - **`is_mobile`** — `0` desktop, `1` mobile, `2` unknown (rare; bots or
//!   hardened clients)
//! - **`ja4_class`** — JA4 Section 1 only (browser family identifier)
//! - **`platform_class`** — coarse OS family from UA
//! - **`h2_fp_hash`** — SHA256 prefix (12 hex chars) of raw H2 SETTINGS
//! - **`known_browser`** — `true` if `ja4_class` + `h2_fp_hash` match a known
//!   browser pattern; `false` for known bots; `None` for unknown

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::kv_types::KvDevice;
use crate::evidence::RequestInfo;
use crate::module_context::ModuleCall;
use crate::settings::Settings;

/// Device attributes for the bid request, as distinct from the gate signals.
///
/// Kept apart from [`DeviceSignals`] on purpose. Those signals answer one
/// question, whether this request looks like a browser, and are stored in the
/// identity graph. These answer a different one, what a bidder is being asked
/// to price, and are stored nowhere.
///
/// Every field is optional because every source is partial. A module that
/// resolves a device type but not a model fills what it has, and the `OpenRTB`
/// device object simply omits the rest, which is what a bidder expects.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceAttributes {
    /// `OpenRTB` device type, from the device types list of `AdCOM` 1.0,
    /// which `OpenRTB` 2.6 refers to: 1 mobile or tablet, 2 personal
    /// computer, 3 connected television, 4 phone, 5 tablet, 6 connected
    /// device, 7 set top box, 8 out-of-home device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_type: Option<i32>,
    /// Hardware vendor, the `make` field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub make: Option<String>,
    /// Hardware model, the `model` field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Operating system name, the `os` field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    /// Operating system version, the `osv` field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
    /// Screen width in pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen_width: Option<i32>,
    /// Screen height in pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen_height: Option<i32>,
}

/// The values of the device types list of `AdCOM` 1.0.
const DEVICE_TYPES: std::ops::RangeInclusive<i32> = 1..=8;

impl DeviceAttributes {
    /// Whether anything was resolved.
    ///
    /// A module that resolved nothing should not put an empty device object
    /// on the request, because an empty object and an absent one mean the same
    /// thing to a bidder and the absent one is smaller.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The same attributes without the values a bidder cannot read, being a
    /// device type outside the list, a screen size that is not positive and a
    /// blank string. Each value dropped is logged with the id of `module`, the
    /// module that answered it.
    #[must_use]
    pub(crate) fn checked(self, module: &str) -> Self {
        Self {
            device_type: kept_number(module, "device_type", self.device_type, |found| {
                DEVICE_TYPES.contains(&found)
            }),
            make: kept_text(module, "make", self.make),
            model: kept_text(module, "model", self.model),
            os: kept_text(module, "os", self.os),
            os_version: kept_text(module, "os_version", self.os_version),
            screen_width: kept_number(module, "screen_width", self.screen_width, |found| found > 0),
            screen_height: kept_number(module, "screen_height", self.screen_height, |found| {
                found > 0
            }),
        }
    }
}

/// `value` when `readable` holds for it, and otherwise nothing.
fn kept_number(
    module: &str,
    field: &str,
    value: Option<i32>,
    readable: fn(i32) -> bool,
) -> Option<i32> {
    value.filter(|&found| {
        let keep = readable(found);
        if !keep {
            log::warn!(
                "device module `{module}`: omitting {field}={found}, which a bidder cannot read"
            );
        }
        keep
    })
}

/// `value` unless it is blank, and otherwise nothing.
fn kept_text(module: &str, field: &str, value: Option<String>) -> Option<String> {
    value.filter(|found| {
        let keep = !found.trim().is_empty();
        if !keep {
            log::warn!("device module `{module}`: omitting a blank {field}");
        }
        keep
    })
}

/// Device signals derived from a single request.
///
/// Computed by the device module from the User-Agent, and from the raw TLS
/// and HTTP/2 signals where the platform supplies them, then passed to core
/// for storage and gating decisions. This type lives in core so it can be
/// used in [`KvDevice`] construction and tested without any platform.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceSignals {
    /// `0` = desktop, `1` = mobile, `2` = unknown.
    pub is_mobile: u8,
    /// JA4 Section 1 (e.g. `"t13d1516h2"`).
    pub ja4_class: Option<String>,
    /// Coarse OS family: `"mac"`, `"windows"`, `"ios"`, `"android"`,
    /// `"linux"`.
    pub platform_class: Option<String>,
    /// SHA256 prefix (12 hex chars) of the raw H2 SETTINGS signal.
    pub h2_fp_hash: Option<String>,
    /// `true` = known browser, `false` = known bot, `None` = unknown.
    pub known_browser: Option<bool>,
    /// Whether the request looks like a real browser, used to gate Edge Cookie
    /// writes. Computed by the producing module: the built-in module uses a
    /// User-Agent-only heuristic, while the platform's own module strengthens
    /// it with the TLS and HTTP/2 signals.
    ///
    /// [`human_confidence`](Self::human_confidence) is the module's answer
    /// about a person, which nothing reads yet.
    pub looks_like_browser: bool,

    /// How confident the device module is that a person is looking at the
    /// page. A module that scores the request writes it, and every other
    /// module leaves it `Unknown`. Nothing reads it yet, and it is not stored
    /// in the identity graph with the other signals.
    ///
    /// Unlike [`looks_like_browser`](Self::looks_like_browser) this is not a
    /// guess about the shape of a header. It is the module's answer from the
    /// evidence it weighs, and a person the module cannot place answers
    /// `Unknown` rather than being guessed at.
    pub human_confidence: HumanConfidence,
}

/// The confidence that a person is looking at the page.
///
/// The four answers a scoring device module can give, with `Unknown` kept
/// apart from `Low` because a request alone cannot establish a person.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum HumanConfidence {
    /// High confidence that a person is viewing the page.
    High,

    /// The evidence is mixed and the request cannot be placed either way. A
    /// person may be viewing the page.
    Medium,

    /// Almost certainly no person is directly involved in viewing the content.
    Low,

    /// Not established. **This is not the same as `Low`.** A request alone
    /// cannot establish a person, and treating it as `Low` would call every
    /// first-time visitor a machine.
    #[default]
    Unknown,
}

impl DeviceSignals {
    /// The signals of a request nothing could be read from, being an unknown
    /// device that does not look like a browser.
    #[must_use]
    pub fn unknown() -> Self {
        Self::derive_ua_only("")
    }

    /// Derives device signals from the User-Agent alone, with no
    /// host-specific TLS or HTTP/2 evidence.
    ///
    /// This is the default path. It touches no platform-specific API, so device
    /// classification stays host-neutral by default. `ja4_class` and
    /// `h2_fp_hash` are left absent, and the browser/bot decision uses a
    /// User-Agent-only heuristic (`looks_like_browser_from_ua`).
    #[must_use]
    pub fn derive_ua_only(ua: &str) -> Self {
        let platform_class = parse_platform_class(ua);
        let looks_like_browser = looks_like_browser_from_ua(ua, platform_class.as_deref());

        Self {
            is_mobile: parse_is_mobile(ua),
            ja4_class: None,
            platform_class,
            h2_fp_hash: None,
            known_browser: None,
            looks_like_browser,
            // Unknown, because the request alone cannot establish a person.
            human_confidence: HumanConfidence::Unknown,
        }
    }

    /// Derives device signals from the User-Agent strengthened with the
    /// host's TLS and HTTP/2 signals.
    ///
    /// `ua` is the `User-Agent` header value. `ja4` is the full JA4 hash and
    /// `h2_fp` the raw H2 SETTINGS string, as the platform reports them.
    /// These signals are host-specific, so only the platform's own device
    /// module, selected with `platform`, uses this path, and the browser/bot
    /// gate then requires a TLS signal.
    #[must_use]
    pub fn derive(ua: &str, ja4: Option<&str>, h2_fp: Option<&str>) -> Self {
        let is_mobile = parse_is_mobile(ua);
        let ja4_class = ja4.and_then(extract_ja4_section1);
        let platform_class = parse_platform_class(ua);
        let h2_fp_hash = h2_fp.map(compute_h2_fp_hash);
        let known_browser = evaluate_known_browser(ja4_class.as_deref(), h2_fp_hash.as_deref());
        // The gate strengthened by host signals. A real browser produces a valid
        // TLS signal and a recognizable UA platform. Raw HTTP clients
        // (curl, Python requests, Go net/http, headless scrapers) lack one or
        // both. This is intentionally aimed at filtering obvious missing-signal
        // traffic, not at resisting deliberate JA4 + UA spoofing.
        let looks_like_browser = ja4_class.is_some() && platform_class.is_some();

        Self {
            is_mobile,
            ja4_class,
            platform_class,
            h2_fp_hash,
            known_browser,
            looks_like_browser,
            // As above: the request alone cannot establish this.
            human_confidence: HumanConfidence::Unknown,
        }
    }

    /// Converts these signals into a [`KvDevice`] for KV storage.
    #[must_use]
    pub fn to_kv_device(&self) -> KvDevice {
        KvDevice {
            is_mobile: self.is_mobile,
            ja4_class: self.ja4_class.clone(),
            platform_class: self.platform_class.clone(),
            h2_fp_hash: self.h2_fp_hash.clone(),
            known_browser: self.known_browser,
        }
    }
}

/// A strategy for classifying a request into [`DeviceSignals`].
///
/// Implementations are selected by configuration. The built-in
/// [`BuiltinDeviceModule`] is the default; a deployment can switch to another
/// module without changing call sites.
///
/// These signals serve identity gating and bot detection, not bid enrichment.
/// [`DeviceSignals`] deliberately carries only the coarse browser and bot
/// classification the Edge Cookie gate needs, not a full device-detection
/// result such as make, model, OS version, or screen size. Those are
/// [`DeviceAttributes`], answered by
/// [`advertising_attributes`](DeviceModule::advertising_attributes) on this
/// same trait, and the two answers stay separate types with separate
/// lifetimes.
/// Uses `#[async_trait(?Send)]` for the same reason as
/// [`PlatformHttpClient`](crate::platform::PlatformHttpClient): the trait
/// object stays `Send + Sync` so it can be shared and run multi-threaded,
/// while the future it returns is pinned to one thread because the host SDKs
/// produce `!Send` futures on wasm32.
#[async_trait::async_trait(?Send)]
pub trait DeviceModule: Send + Sync {
    /// Returns the stable identifier for this module, used in configuration
    /// and logs.
    fn id(&self) -> &'static str;

    /// Classifies the request into [`DeviceSignals`].
    ///
    /// See [`crate::module_context`] for what an implementation may name.
    /// Classification runs before the permissions are resolved, so no value
    /// whose use needs a permission is passed here.
    ///
    /// Device signals gate identity operations and must always yield a value,
    /// so this is infallible. A module that cannot determine a signal, or
    /// whose function was not passed what it names, returns
    /// [`DeviceSignals::unknown`] rather than failing the request.
    /// Asynchronous because a device module may reach a backend, a
    /// key-value store or a secret to classify a request, and a module that
    /// cannot make those calls cannot be written at all. The built-in
    /// User-Agent module does no I/O and returns immediately.
    async fn detect(&self, call: ModuleCall<'_>) -> DeviceSignals;

    /// The permissions this module's data use requires.
    ///
    /// The default is empty, so the built-in User-Agent-only module requires
    /// no permission.
    fn required_permissions(&self) -> crate::permissions::PermissionSet {
        crate::permissions::PermissionSet::none()
    }

    /// Attributes for the bid request, when this module can resolve them.
    ///
    /// The call carries the permissions resolved for the request, so a module
    /// whose use of the evidence needs a permission the reader has not given
    /// answers `None` without using it. A backend the module registers during
    /// the call has its transport timers capped at the auction's timeout, and
    /// core drops a device type outside the list, a screen size that is not
    /// positive and a blank string before it attaches the answer.
    ///
    /// The default returns nothing, so no existing module changes. A module
    /// that resolves nothing returns `None` rather than an empty value.
    async fn advertising_attributes(&self, _call: ModuleCall<'_>) -> Option<DeviceAttributes> {
        None
    }
}

/// The built-in device module, the default.
///
/// Derives [`DeviceSignals`] from the User-Agent alone via
/// [`DeviceSignals::derive_ua_only`], touching no host-specific API. It reads
/// only [`RequestInfo::user_agent`] and never a host signal, so device
/// classification stays host-neutral by default.
#[derive(Debug, Default)]
pub struct BuiltinDeviceModule;

impl BuiltinDeviceModule {
    /// Creates the built-in module.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// The signals the User-Agent alone gives.
    fn classify(&self, request_info: &dyn RequestInfo) -> DeviceSignals {
        DeviceSignals::derive_ua_only(request_info.user_agent())
    }
}

#[async_trait::async_trait(?Send)]
impl DeviceModule for BuiltinDeviceModule {
    fn id(&self) -> &'static str {
        "builtin"
    }

    async fn detect(&self, call: ModuleCall<'_>) -> DeviceSignals {
        call.inject(self, Self::classify)
            .unwrap_or_else(|_| DeviceSignals::unknown())
    }
}

/// The device module core or the adapter supplies for the `[device] module`
/// selector.
///
/// Returns the built-in User-Agent-only module unless the `platform` selector
/// is set, in which case it builds the host platform's own module through the
/// `build_platform` factory the adapter supplies. The factory runs only when
/// that module is selected, so device classification itself reads no host
/// signals by default (see [`BuiltinDeviceModule`] for the host-neutral
/// default). An adapter's entry point may still read its platform's signals
/// on every request for other uses, such as the client info.
///
/// A module a registration supplies is resolved by the integration registry,
/// which refuses a name no module supplies when it is built. A caller uses
/// this only when the registry resolved no module, so it never sees such a
/// name.
#[must_use]
pub fn build_device_module(
    settings: &Settings,
    build_platform: impl FnOnce() -> Box<dyn DeviceModule>,
) -> Box<dyn DeviceModule> {
    match settings.device.module_key() {
        "platform" => build_platform(),
        _ => Box::new(BuiltinDeviceModule::new()),
    }
}

/// Device is a desktop (confirmed via UA platform token).
const MOBILE_DESKTOP: u8 = 0;
/// Device is a mobile (confirmed via UA mobile token).
const MOBILE_MOBILE: u8 = 1;
/// Device type is genuinely unknown (typically bots or hardened clients).
const MOBILE_UNKNOWN: u8 = 2;

/// Derives mobile signal from the User-Agent string.
///
/// Returns [`MOBILE_DESKTOP`] for confirmed desktop,
/// [`MOBILE_MOBILE`] for confirmed mobile,
/// [`MOBILE_UNKNOWN`] for genuinely unknown (typically bots or hardened clients).
#[must_use]
fn parse_is_mobile(ua: &str) -> u8 {
    // Mobile patterns checked first — more specific.
    if ua.contains("iPhone") || ua.contains("iPad") || ua.contains("Android") {
        return MOBILE_MOBILE;
    }
    if ua.contains("Macintosh") || ua.contains("Windows") || ua.contains("Linux") {
        return MOBILE_DESKTOP;
    }
    MOBILE_UNKNOWN
}

/// Parses coarse OS family from the User-Agent string.
///
/// Returns `None` when no recognized platform pattern is found.
#[must_use]
fn parse_platform_class(ua: &str) -> Option<String> {
    // Order matters: check mobile-specific patterns before generic ones.
    if ua.contains("iPhone") || ua.contains("iPad") {
        return Some("ios".to_owned());
    }
    if ua.contains("Android") {
        return Some("android".to_owned());
    }
    if ua.contains("Macintosh") {
        return Some("mac".to_owned());
    }
    if ua.contains("Windows NT") {
        return Some("windows".to_owned());
    }
    if ua.contains("Linux") {
        return Some("linux".to_owned());
    }
    None
}

/// Decides whether a request looks like a real browser from the User-Agent
/// alone, with no TLS or HTTP/2 evidence.
///
/// A real browser sends the `Mozilla/` token every major engine still emits and
/// a recognizable platform string (so `platform_class` is present), and is not
/// an obvious bot or command-line client. Raw HTTP clients (curl, Python
/// requests, Go net/http) carry no platform token, so they fail the
/// `platform_class` check; declared crawlers are caught by [`looks_like_bot_ua`].
///
/// # Threat model
///
/// This is the default, host-neutral gate. It filters obvious non-browser
/// traffic but does not resist a bot that forges a complete browser
/// User-Agent. The platform's own device module, selected with `platform`,
/// strengthens the gate with the TLS and HTTP/2 signals for deployments that
/// need it.
#[must_use]
fn looks_like_browser_from_ua(ua: &str, platform_class: Option<&str>) -> bool {
    platform_class.is_some() && ua.contains("Mozilla/") && !looks_like_bot_ua(ua)
}

/// Returns `true` when the User-Agent declares a known bot, crawler, or
/// non-browser HTTP client.
///
/// Matches common self-identifying markers case-insensitively. The `bot` marker
/// covers `Googlebot`, `bingbot`, and similar; the library markers cover HTTP
/// clients that set a recognizable platform token.
#[must_use]
fn looks_like_bot_ua(ua: &str) -> bool {
    const BOT_MARKERS: &[&str] = &[
        "bot",
        "crawl",
        "spider",
        "slurp",
        "curl",
        "wget",
        "python-requests",
        "go-http-client",
        "okhttp",
        "java/",
        "headlesschrome",
        "phantomjs",
        "scrapy",
    ];
    let lower = ua.to_ascii_lowercase();
    BOT_MARKERS.iter().any(|marker| lower.contains(marker))
}

/// Extracts Section 1 from a full JA4 string.
///
/// JA4 format: `section1_section2_section3` separated by underscores.
/// Section 1 identifies browser family (cipher count, extension count,
/// ALPN) without uniquely identifying a device.
///
/// Returns `None` if the input is empty or has no underscore-delimited
/// section.
#[must_use]
fn extract_ja4_section1(full_ja4: &str) -> Option<String> {
    let section1 = full_ja4.split('_').next()?;
    if section1.is_empty() {
        return None;
    }
    Some(section1.to_owned())
}

/// Computes a 12-hex-char prefix of the SHA256 hash of the raw H2
/// SETTINGS signal string.
///
/// The raw string looks like `"1:65536;2:0;4:6291456;6:262144"`.
#[must_use]
fn compute_h2_fp_hash(raw_h2_fp: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw_h2_fp.as_bytes());
    let digest = hasher.finalize();
    hex::encode(&digest[..6])
}

/// Known browser signal allowlist.
///
/// Each entry is `(ja4_class, h2_fp_prefix, known_browser)`.
/// `h2_fp_prefix` is the raw H2 SETTINGS string (not the hash) — we
/// compare against the hash computed from it.
///
/// Empirically derived from production traffic at the edge (2026-04-03).
const KNOWN_BROWSERS: &[(&str, &str, bool)] = &[
    // Chrome/Mac v146
    ("t13d1516h2", "1:65536;2:0;4:6291456;6:262144", true),
    // Safari/Mac v26 and Safari/iOS v26
    ("t13d2013h2", "2:0;3:100;4:2097152", true),
    // Firefox/Mac v149
    ("t13d1717h2", "1:65536;2:0;4:131072;5:16384", true),
];

/// Returns H2 signal hashes for the known browser allowlist.
///
/// Computed once on first call and cached via `OnceLock`.
fn known_browser_h2_hashes() -> &'static Vec<(&'static str, String, bool)> {
    static CACHE: std::sync::OnceLock<Vec<(&str, String, bool)>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        KNOWN_BROWSERS
            .iter()
            .map(|(ja4, h2_raw, known)| (*ja4, compute_h2_fp_hash(h2_raw), *known))
            .collect()
    })
}

/// Evaluates whether a request comes from a known browser.
///
/// Returns `Some(true)` if `ja4_class` + `h2_fp_hash` match a known
/// legitimate browser pattern. Returns `Some(false)` for known
/// bot/scraper patterns. Returns `None` for unrecognized combinations.
///
/// Both signals must be present for a match — if either is `None`,
/// returns `None`.
#[must_use]
fn evaluate_known_browser(ja4_class: Option<&str>, h2_fp_hash: Option<&str>) -> Option<bool> {
    let ja4 = ja4_class?;
    let h2_hash = h2_fp_hash?;

    for (known_ja4, known_h2_hash, is_browser) in known_browser_h2_hashes() {
        if ja4 == *known_ja4 && h2_hash == *known_h2_hash {
            return Some(*is_browser);
        }
    }

    // No match — unknown client.
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::OwnedRequestInfo;
    use crate::module_context::{ModuleContext, test_support};

    // Chrome Mac UA
    const CHROME_MAC_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
        AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36";

    // Safari iOS UA
    const SAFARI_IOS_UA: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 26_0 like Mac OS X) \
        AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Mobile/15E148 Safari/604.1";

    // Safari Mac UA
    const SAFARI_MAC_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
        AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";

    // Firefox Mac UA
    const FIREFOX_MAC_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; \
        rv:149.0) Gecko/20100101 Firefox/149.0";

    // Android Chrome UA
    const CHROME_ANDROID_UA: &str = "Mozilla/5.0 (Linux; Android 14; Pixel 8) \
        AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Mobile Safari/537.36";

    // Windows Chrome UA
    const CHROME_WINDOWS_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
        AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36";

    // Bot/empty UA
    const BOT_UA: &str = "Googlebot/2.1 (+http://www.google.com/bot.html)";

    #[test]
    fn is_mobile_desktop_browsers() {
        assert_eq!(parse_is_mobile(CHROME_MAC_UA), 0, "Chrome/Mac = desktop");
        assert_eq!(parse_is_mobile(SAFARI_MAC_UA), 0, "Safari/Mac = desktop");
        assert_eq!(parse_is_mobile(FIREFOX_MAC_UA), 0, "Firefox/Mac = desktop");
        assert_eq!(
            parse_is_mobile(CHROME_WINDOWS_UA),
            0,
            "Chrome/Windows = desktop"
        );
    }

    #[test]
    fn is_mobile_mobile_browsers() {
        assert_eq!(parse_is_mobile(SAFARI_IOS_UA), 1, "Safari/iOS = mobile");
        assert_eq!(
            parse_is_mobile(CHROME_ANDROID_UA),
            1,
            "Chrome/Android = mobile"
        );
    }

    #[test]
    fn is_mobile_unknown() {
        assert_eq!(parse_is_mobile(BOT_UA), 2, "Googlebot = unknown");
        assert_eq!(parse_is_mobile(""), 2, "empty UA = unknown");
    }

    #[test]
    fn platform_class_desktop() {
        assert_eq!(parse_platform_class(CHROME_MAC_UA).as_deref(), Some("mac"));
        assert_eq!(
            parse_platform_class(CHROME_WINDOWS_UA).as_deref(),
            Some("windows")
        );
        assert_eq!(parse_platform_class(FIREFOX_MAC_UA).as_deref(), Some("mac"));
    }

    #[test]
    fn platform_class_mobile() {
        assert_eq!(parse_platform_class(SAFARI_IOS_UA).as_deref(), Some("ios"));
        assert_eq!(
            parse_platform_class(CHROME_ANDROID_UA).as_deref(),
            Some("android")
        );
    }

    #[test]
    fn platform_class_linux() {
        let linux_ua = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36";
        assert_eq!(parse_platform_class(linux_ua).as_deref(), Some("linux"));
    }

    #[test]
    fn platform_class_unknown() {
        assert_eq!(parse_platform_class(BOT_UA), None);
        assert_eq!(parse_platform_class(""), None);
    }

    #[test]
    fn ja4_section1_extraction() {
        assert_eq!(
            extract_ja4_section1("t13d1516h2_8daaf6152771_e5627efa2ab1").as_deref(),
            Some("t13d1516h2"),
            "should extract section 1 from full JA4"
        );
    }

    #[test]
    fn ja4_section1_no_underscore() {
        // Some implementations may return just section 1
        assert_eq!(
            extract_ja4_section1("t13d1516h2").as_deref(),
            Some("t13d1516h2"),
            "should handle JA4 with no underscore"
        );
    }

    #[test]
    fn ja4_section1_empty() {
        assert_eq!(extract_ja4_section1(""), None);
    }

    #[test]
    fn h2_fp_hash_deterministic() {
        let hash1 = compute_h2_fp_hash("1:65536;2:0;4:6291456;6:262144");
        let hash2 = compute_h2_fp_hash("1:65536;2:0;4:6291456;6:262144");
        assert_eq!(hash1, hash2, "should be deterministic");
        assert_eq!(hash1.len(), 12, "should be 12 hex chars");
    }

    #[test]
    fn h2_fp_hash_different_inputs() {
        let chrome = compute_h2_fp_hash("1:65536;2:0;4:6291456;6:262144");
        let safari = compute_h2_fp_hash("2:0;3:100;4:2097152");
        assert_ne!(
            chrome, safari,
            "different inputs should produce different hashes"
        );
    }

    #[test]
    fn known_browser_chrome_match() {
        let ja4 = "t13d1516h2";
        let h2_hash = compute_h2_fp_hash("1:65536;2:0;4:6291456;6:262144");
        assert_eq!(
            evaluate_known_browser(Some(ja4), Some(&h2_hash)),
            Some(true),
            "Chrome signal should be recognized"
        );
    }

    #[test]
    fn known_browser_safari_match() {
        let ja4 = "t13d2013h2";
        let h2_hash = compute_h2_fp_hash("2:0;3:100;4:2097152");
        assert_eq!(
            evaluate_known_browser(Some(ja4), Some(&h2_hash)),
            Some(true),
            "Safari signal should be recognized"
        );
    }

    #[test]
    fn known_browser_firefox_match() {
        let ja4 = "t13d1717h2";
        let h2_hash = compute_h2_fp_hash("1:65536;2:0;4:131072;5:16384");
        assert_eq!(
            evaluate_known_browser(Some(ja4), Some(&h2_hash)),
            Some(true),
            "Firefox signal should be recognized"
        );
    }

    #[test]
    fn known_browser_unknown_combination() {
        let ja4 = "t13d9999h2";
        let h2_hash = compute_h2_fp_hash("1:1;2:2;3:3");
        assert_eq!(
            evaluate_known_browser(Some(ja4), Some(&h2_hash)),
            None,
            "unknown combination should return None"
        );
    }

    #[test]
    fn known_browser_mismatched_ja4_h2() {
        // Chrome JA4 but Safari H2
        let ja4 = "t13d1516h2";
        let h2_hash = compute_h2_fp_hash("2:0;3:100;4:2097152");
        assert_eq!(
            evaluate_known_browser(Some(ja4), Some(&h2_hash)),
            None,
            "mismatched JA4/H2 should return None"
        );
    }

    #[test]
    fn known_browser_missing_signals() {
        assert_eq!(
            evaluate_known_browser(None, Some("abcdef123456")),
            None,
            "missing JA4 should return None"
        );
        assert_eq!(
            evaluate_known_browser(Some("t13d1516h2"), None),
            None,
            "missing H2 hash should return None"
        );
        assert_eq!(
            evaluate_known_browser(None, None),
            None,
            "both missing should return None"
        );
    }

    #[test]
    fn derive_chrome_mac() {
        let signals = DeviceSignals::derive(
            CHROME_MAC_UA,
            Some("t13d1516h2_8daaf6152771_e5627efa2ab1"),
            Some("1:65536;2:0;4:6291456;6:262144"),
        );

        assert_eq!(signals.is_mobile, 0);
        assert_eq!(signals.ja4_class.as_deref(), Some("t13d1516h2"));
        assert_eq!(signals.platform_class.as_deref(), Some("mac"));
        assert!(signals.h2_fp_hash.is_some());
        assert_eq!(signals.known_browser, Some(true));
    }

    #[test]
    fn derive_safari_ios() {
        let signals = DeviceSignals::derive(
            SAFARI_IOS_UA,
            Some("t13d2013h2_abcdef123456_fedcba654321"),
            Some("2:0;3:100;4:2097152"),
        );

        assert_eq!(signals.is_mobile, 1);
        assert_eq!(signals.ja4_class.as_deref(), Some("t13d2013h2"));
        assert_eq!(signals.platform_class.as_deref(), Some("ios"));
        assert_eq!(signals.known_browser, Some(true));
    }

    #[test]
    fn derive_bot() {
        let signals = DeviceSignals::derive(BOT_UA, None, None);

        assert_eq!(signals.is_mobile, 2);
        assert!(signals.ja4_class.is_none());
        assert!(signals.platform_class.is_none());
        assert!(signals.h2_fp_hash.is_none());
        assert_eq!(signals.known_browser, None);
    }

    #[test]
    fn to_kv_device_conversion() {
        let signals = DeviceSignals::derive(
            CHROME_MAC_UA,
            Some("t13d1516h2_8daaf6152771_e5627efa2ab1"),
            Some("1:65536;2:0;4:6291456;6:262144"),
        );
        let device = signals.to_kv_device();

        assert_eq!(device.is_mobile, signals.is_mobile);
        assert_eq!(device.ja4_class, signals.ja4_class);
        assert_eq!(device.platform_class, signals.platform_class);
        assert_eq!(device.h2_fp_hash, signals.h2_fp_hash);
        assert_eq!(device.known_browser, signals.known_browser);
    }

    #[test]
    fn android_is_linux_but_platform_class_android() {
        // Android UA contains "Linux" — platform_class should be "android"
        // not "linux" because we check Android before Linux.
        assert_eq!(
            parse_platform_class(CHROME_ANDROID_UA).as_deref(),
            Some("android"),
            "Android should take precedence over Linux"
        );
        // But is_mobile should be 1 since it contains "Android".
        assert_eq!(parse_is_mobile(CHROME_ANDROID_UA), 1);
    }

    #[test]
    fn ipad_is_mobile() {
        let ipad_ua = "Mozilla/5.0 (iPad; CPU OS 26_0 like Mac OS X) \
            AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/604.1";
        assert_eq!(parse_is_mobile(ipad_ua), 1, "iPad should be mobile");
        assert_eq!(
            parse_platform_class(ipad_ua).as_deref(),
            Some("ios"),
            "iPad should be ios"
        );
    }

    #[test]
    fn looks_like_browser_with_both_signals() {
        let signals = DeviceSignals::derive(
            CHROME_MAC_UA,
            Some("t13d1516h2_8daaf6152771_e5627efa2ab1"),
            Some("1:65536;2:0;4:6291456;6:262144"),
        );
        assert!(
            signals.looks_like_browser,
            "Chrome/Mac should look like a browser"
        );
    }

    #[test]
    fn looks_like_browser_unknown_fingerprint_still_passes() {
        // Chrome/Windows with unknown JA4/H2 — still has ja4_class and platform_class
        let signals = DeviceSignals::derive(
            CHROME_WINDOWS_UA,
            Some("t13d9999h2_unknown_unknown"),
            Some("99:99;88:88"),
        );
        assert!(
            signals.looks_like_browser,
            "unknown signal with valid JA4 + platform should pass"
        );
        assert_eq!(signals.known_browser, None, "should not match allowlist");
    }

    #[test]
    fn looks_like_browser_rejects_bot() {
        let signals = DeviceSignals::derive(BOT_UA, None, None);
        assert!(
            !signals.looks_like_browser,
            "bot with no JA4 and no platform should be rejected"
        );
    }

    #[test]
    fn looks_like_browser_rejects_missing_ja4() {
        // Real UA but no TLS signal (e.g. HTTP/1.1 or missing SDK support)
        let signals = DeviceSignals::derive(CHROME_MAC_UA, None, Some("1:65536"));
        assert!(
            !signals.looks_like_browser,
            "missing JA4 should be rejected even with valid UA"
        );
    }

    #[test]
    fn looks_like_browser_rejects_missing_platform() {
        // Has JA4 but unrecognizable UA
        let signals = DeviceSignals::derive(BOT_UA, Some("t13d1516h2_abc_def"), None);
        assert!(
            !signals.looks_like_browser,
            "unrecognizable UA should be rejected even with JA4"
        );
    }

    #[test]
    fn derive_ua_only_accepts_real_browsers_without_fingerprints() {
        for ua in [
            CHROME_MAC_UA,
            SAFARI_IOS_UA,
            FIREFOX_MAC_UA,
            CHROME_ANDROID_UA,
            CHROME_WINDOWS_UA,
        ] {
            let signals = DeviceSignals::derive_ua_only(ua);
            assert!(
                signals.looks_like_browser,
                "a real browser UA should pass the UA-only gate: {ua}"
            );
            assert!(
                signals.ja4_class.is_none() && signals.h2_fp_hash.is_none(),
                "the UA-only path must not record any TLS/H2 evidence"
            );
        }
    }

    #[test]
    fn derive_ua_only_rejects_bots_and_http_clients() {
        // Declared crawlers and CLI/library clients must not pass the gate.
        for ua in [
            BOT_UA,
            "Mozilla/5.0 (compatible; bingbot/2.0; +http://www.bing.com/bingbot.htm)",
            "curl/8.4.0",
            "python-requests/2.31.0",
            "Go-http-client/2.0",
            "",
        ] {
            assert!(
                !DeviceSignals::derive_ua_only(ua).looks_like_browser,
                "a non-browser client should fail the UA-only gate: {ua:?}"
            );
        }
    }

    /// A request alone cannot establish a person, so the signals read from
    /// one say `Unknown`, which is not `Low`, and nothing calls a first-time
    /// visitor a machine.
    #[test]
    fn a_request_alone_establishes_no_person() {
        for signals in [
            DeviceSignals::derive_ua_only(CHROME_MAC_UA),
            DeviceSignals::derive_ua_only("curl/8.0"),
            DeviceSignals::derive(CHROME_MAC_UA, Some("t13d1516h2"), Some("abc123def456")),
            DeviceSignals::derive("curl/8.0", None, None),
            DeviceSignals::unknown(),
        ] {
            assert_eq!(signals.human_confidence, HumanConfidence::Unknown);
        }
        assert_eq!(HumanConfidence::default(), HumanConfidence::Unknown);
    }

    #[test]
    fn checked_keeps_only_the_values_a_bidder_can_read() {
        for (device_type, kept) in [(0, None), (1, Some(1)), (8, Some(8)), (9, None)] {
            let attributes = DeviceAttributes {
                device_type: Some(device_type),
                ..DeviceAttributes::default()
            };
            assert_eq!(
                attributes.checked("test").device_type,
                kept,
                "device type {device_type} should be kept only when it is in the list"
            );
        }
        for (size, kept) in [(-1, None), (0, None), (1, Some(1))] {
            let attributes = DeviceAttributes {
                screen_width: Some(size),
                screen_height: Some(size),
                ..DeviceAttributes::default()
            };
            let checked = attributes.checked("test");
            assert_eq!(
                (checked.screen_width, checked.screen_height),
                (kept, kept),
                "screen size {size} should be kept only when it is positive"
            );
        }
        for (text, kept) in [("", None), (" \t", None), ("Android", Some("Android"))] {
            let attributes = DeviceAttributes {
                make: Some(text.to_owned()),
                model: Some(text.to_owned()),
                os: Some(text.to_owned()),
                os_version: Some(text.to_owned()),
                ..DeviceAttributes::default()
            };
            let checked = attributes.checked("test");
            let kept = kept.map(str::to_owned);
            assert_eq!(
                [checked.make, checked.model, checked.os, checked.os_version],
                [kept.clone(), kept.clone(), kept.clone(), kept],
                "{text:?} should be kept only when it is not blank"
            );
        }
    }

    #[test]
    fn derive_ua_only_rejects_a_browser_ua_that_declares_a_bot() {
        // Newer crawlers send a full browser UA with a platform token; the bot
        // marker must still reject them.
        let googlebot_mobile = "Mozilla/5.0 (Linux; Android 6.0.1; Nexus 5X Build/MMB29P) \
            AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Mobile Safari/537.36 \
            (compatible; Googlebot/2.1; +http://www.google.com/bot.html)";
        assert!(
            !DeviceSignals::derive_ua_only(googlebot_mobile).looks_like_browser,
            "a browser-shaped UA declaring Googlebot should be rejected"
        );
    }

    #[tokio::test]
    async fn builtin_device_module_is_ua_only() {
        let module = BuiltinDeviceModule::new();
        assert_eq!(module.id(), "builtin");

        // The built-in module classifies from the User-Agent in the request's
        // evidence alone, recording no host signal.
        let request_info = request_info_with_ua(CHROME_MAC_UA);
        let context = ModuleContext::new(test_support::request("/")).with_evidence(&request_info);
        let signals = module
            .detect(context.call(module.id(), module.required_permissions()))
            .await;
        assert_eq!(
            signals,
            DeviceSignals::derive_ua_only(CHROME_MAC_UA),
            "the built-in module should classify from the User-Agent only"
        );
        assert!(
            signals.ja4_class.is_none(),
            "the built-in module must not record a JA4 class"
        );
    }

    #[tokio::test]
    async fn a_device_module_not_passed_what_it_names_answers_unknown() {
        let module = BuiltinDeviceModule::new();
        // A context with no evidence, as for work shared by every reader.
        let context = ModuleContext::new(test_support::request("/"));

        let signals = module
            .detect(context.call(module.id(), module.required_permissions()))
            .await;

        assert_eq!(
            signals,
            DeviceSignals::unknown(),
            "should answer the unknown device rather than fail the request"
        );
        assert!(
            !signals.looks_like_browser,
            "the unknown device should not look like a browser"
        );
    }

    /// Builds request info carrying the given User-Agent, for module tests.
    fn request_info_with_ua(user_agent: &str) -> OwnedRequestInfo {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::USER_AGENT,
            http::HeaderValue::from_str(user_agent)
                .expect("should build a valid User-Agent header"),
        );
        OwnedRequestInfo::new(String::new(), headers)
    }

    /// A stand-in for the platform's own module the adapter injects, so the
    /// selection logic can be tested in core without a platform's module crate.
    struct StubPlatformModule;

    #[async_trait::async_trait(?Send)]
    impl DeviceModule for StubPlatformModule {
        fn id(&self) -> &'static str {
            "stub-platform-module"
        }

        async fn detect(&self, _call: ModuleCall<'_>) -> DeviceSignals {
            DeviceSignals::unknown()
        }
    }

    #[test]
    fn builtin_device_module_requires_no_permissions() {
        assert!(
            BuiltinDeviceModule::new().required_permissions().is_empty(),
            "the built-in User-Agent-only device module requires no permissions"
        );
    }

    #[test]
    fn build_device_module_defaults_to_builtin_and_selects_injected() {
        // The default selector returns the built-in module, ignoring the
        // injected candidate.
        let settings = crate::settings::Settings::default();
        let default = build_device_module(&settings, || {
            Box::new(StubPlatformModule) as Box<dyn DeviceModule>
        });
        assert_eq!(default.id(), "builtin", "no selector should be UA-only");

        // The `platform` selector returns the module the adapter's factory
        // builds.
        let mut platform = crate::settings::Settings::default();
        platform.device.module = Some("platform".to_owned());
        let selected = build_device_module(&platform, || {
            Box::new(StubPlatformModule) as Box<dyn DeviceModule>
        });
        assert_eq!(
            selected.id(),
            "stub-platform-module",
            "the platform selector should use the injected module"
        );
    }
}
