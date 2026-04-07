//! Device signal derivation for bot detection and browser classification.
//!
//! All functions in this module are pure computations — no KV I/O or Fastly
//! SDK calls. The Fastly adapter extracts raw strings from the request
//! (`get_tls_ja4()`, `get_client_h2_fingerprint()`, UA header) and passes
//! them here for classification.
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

use sha2::{Digest as _, Sha256};

use super::kv_types::KvDevice;

/// Device signals derived from a single request.
///
/// Computed in the Fastly adapter from raw TLS/H2/UA data, then passed to
/// core for storage and gating decisions. This type lives in core so it
/// can be used in [`KvDevice`] construction and tested without Fastly.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceSignals {
    /// `0` = desktop, `1` = mobile, `2` = unknown.
    pub is_mobile: u8,
    /// JA4 Section 1 (e.g. `"t13d1516h2"`).
    pub ja4_class: Option<String>,
    /// Coarse OS family: `"mac"`, `"windows"`, `"ios"`, `"android"`,
    /// `"linux"`.
    pub platform_class: Option<String>,
    /// SHA256 prefix (12 hex chars) of raw H2 SETTINGS fingerprint.
    pub h2_fp_hash: Option<String>,
    /// `true` = known browser, `false` = known bot, `None` = unknown.
    pub known_browser: Option<bool>,
}

impl DeviceSignals {
    /// Derives all device signals from raw request data.
    ///
    /// `ua` is the `User-Agent` header value. `ja4` is the full JA4 hash
    /// from `req.get_tls_ja4()`. `h2_fp` is the raw H2 SETTINGS string
    /// from `req.get_client_h2_fingerprint()`.
    #[must_use]
    pub fn derive(ua: &str, ja4: Option<&str>, h2_fp: Option<&str>) -> Self {
        let is_mobile = parse_is_mobile(ua);
        let ja4_class = ja4.and_then(extract_ja4_section1);
        let platform_class = parse_platform_class(ua);
        let h2_fp_hash = h2_fp.map(compute_h2_fp_hash);
        let known_browser = evaluate_known_browser(ja4_class.as_deref(), h2_fp_hash.as_deref());

        Self {
            is_mobile,
            ja4_class,
            platform_class,
            h2_fp_hash,
            known_browser,
        }
    }

    /// Returns `true` when the request looks like a real browser.
    ///
    /// Checks for the presence of recognizable signals rather than matching
    /// against a hardcoded fingerprint allowlist. Real browsers always
    /// produce a valid TLS fingerprint (`ja4_class`) and a recognizable UA
    /// platform string (`platform_class`). Raw HTTP clients (curl, Python
    /// requests, Go net/http, headless scrapers) typically lack one or both.
    ///
    /// `known_browser` is still computed and stored on [`KvDevice`] for
    /// analytics but does not gate identity operations.
    #[must_use]
    pub fn looks_like_browser(&self) -> bool {
        self.ja4_class.is_some() && self.platform_class.is_some()
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

/// Derives mobile signal from the User-Agent string.
///
/// Returns `0` for confirmed desktop, `1` for confirmed mobile,
/// `2` for genuinely unknown (typically bots or hardened clients).
#[must_use]
pub fn parse_is_mobile(ua: &str) -> u8 {
    // Mobile patterns checked first — more specific.
    if ua.contains("iPhone") || ua.contains("iPad") || ua.contains("Android") {
        return 1;
    }
    if ua.contains("Macintosh") || ua.contains("Windows") || ua.contains("Linux") {
        return 0;
    }
    2
}

/// Parses coarse OS family from the User-Agent string.
///
/// Returns `None` when no recognized platform pattern is found.
#[must_use]
pub fn parse_platform_class(ua: &str) -> Option<String> {
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

/// Extracts Section 1 from a full JA4 fingerprint.
///
/// JA4 format: `section1_section2_section3` separated by underscores.
/// Section 1 identifies browser family (cipher count, extension count,
/// ALPN) without uniquely fingerprinting a device.
///
/// Returns `None` if the input is empty or has no underscore-delimited
/// section.
#[must_use]
pub fn extract_ja4_section1(full_ja4: &str) -> Option<String> {
    let section1 = full_ja4.split('_').next()?;
    if section1.is_empty() {
        return None;
    }
    Some(section1.to_owned())
}

/// Computes a 12-hex-char prefix of the SHA256 hash of the raw H2
/// SETTINGS fingerprint string.
///
/// The raw string looks like `"1:65536;2:0;4:6291456;6:262144"`.
#[must_use]
pub fn compute_h2_fp_hash(raw_h2_fp: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw_h2_fp.as_bytes());
    let digest = hasher.finalize();
    hex::encode(&digest[..6])
}

/// Known browser fingerprint allowlist.
///
/// Each entry is `(ja4_class, h2_fp_prefix, known_browser)`.
/// `h2_fp_prefix` is the raw H2 SETTINGS string (not the hash) — we
/// compare against the hash computed from it.
///
/// Empirically derived from Fastly Compute production responses (2026-04-03).
const KNOWN_BROWSERS: &[(&str, &str, bool)] = &[
    // Chrome/Mac v146
    ("t13d1516h2", "1:65536;2:0;4:6291456;6:262144", true),
    // Safari/Mac v26 and Safari/iOS v26
    ("t13d2013h2", "2:0;3:100;4:2097152", true),
    // Firefox/Mac v149
    ("t13d1717h2", "1:65536;2:0;4:131072;5:16384", true),
];

/// Returns H2 fingerprint hashes for the known browser allowlist.
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
pub fn evaluate_known_browser(ja4_class: Option<&str>, h2_fp_hash: Option<&str>) -> Option<bool> {
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
            "Chrome fingerprint should be recognized"
        );
    }

    #[test]
    fn known_browser_safari_match() {
        let ja4 = "t13d2013h2";
        let h2_hash = compute_h2_fp_hash("2:0;3:100;4:2097152");
        assert_eq!(
            evaluate_known_browser(Some(ja4), Some(&h2_hash)),
            Some(true),
            "Safari fingerprint should be recognized"
        );
    }

    #[test]
    fn known_browser_firefox_match() {
        let ja4 = "t13d1717h2";
        let h2_hash = compute_h2_fp_hash("1:65536;2:0;4:131072;5:16384");
        assert_eq!(
            evaluate_known_browser(Some(ja4), Some(&h2_hash)),
            Some(true),
            "Firefox fingerprint should be recognized"
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
            signals.looks_like_browser(),
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
            signals.looks_like_browser(),
            "unknown fingerprint with valid JA4 + platform should pass"
        );
        assert_eq!(signals.known_browser, None, "should not match allowlist");
    }

    #[test]
    fn looks_like_browser_rejects_bot() {
        let signals = DeviceSignals::derive(BOT_UA, None, None);
        assert!(
            !signals.looks_like_browser(),
            "bot with no JA4 and no platform should be rejected"
        );
    }

    #[test]
    fn looks_like_browser_rejects_missing_ja4() {
        // Real UA but no TLS fingerprint (e.g. HTTP/1.1 or missing SDK support)
        let signals = DeviceSignals::derive(CHROME_MAC_UA, None, Some("1:65536"));
        assert!(
            !signals.looks_like_browser(),
            "missing JA4 should be rejected even with valid UA"
        );
    }

    #[test]
    fn looks_like_browser_rejects_missing_platform() {
        // Has JA4 but unrecognizable UA
        let signals = DeviceSignals::derive(BOT_UA, Some("t13d1516h2_abc_def"), None);
        assert!(
            !signals.looks_like_browser(),
            "unrecognizable UA should be rejected even with JA4"
        );
    }
}
