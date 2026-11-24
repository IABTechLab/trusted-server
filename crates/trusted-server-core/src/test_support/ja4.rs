//! JA4 and User-Agent fixtures for the ID-7 family rule (`ec::ja4_family`).
//!
//! The listed fingerprints are public entries from `FoxIO`'s
//! `ja4plus-mapping.csv`, the source `ec::ja4_family` cites. The others are
//! marked test-only and fictional.

use edgezero_core::http::{Extensions, HeaderValue, Request, header};
use edgezero_core::ingress::{CapturedTarget, HeaderFidelity, RequestIngress, TargetUnavailable};
use edgezero_core::ja4::{CapturedJa4, Ja4Fingerprint, Ja4Source};

/// A Chrome on macOS User-Agent.
pub const CHROME_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
    AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36";
/// A Microsoft Edge on Windows User-Agent, a Chromium browser.
pub const EDGE_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
    AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36 Edg/146.0.0.0";
/// A Firefox on macOS User-Agent.
pub const FIREFOX_USER_AGENT: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:149.0) Gecko/20100101 Firefox/149.0";
/// A Safari on macOS User-Agent.
pub const SAFARI_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
    AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";
/// A Chrome on iOS User-Agent, which runs on `WebKit`.
pub const CHROME_IOS_USER_AGENT: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 26_0 like Mac OS X) \
    AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/146.0.0.0 Mobile/15E148 Safari/604.1";

/// Chromium Browser.
pub const CHROMIUM_JA4: &str = "t13d1516h2_8daaf6152771_02713d6af862";
/// Mozilla Firefox.
pub const FIREFOX_JA4: &str = "t13d1715h2_5b57614c22b0_7121afd63204";
/// Safari.
pub const SAFARI_JA4: &str = "t13d2014h2_a09f3c656075_14788d8d241b";
/// Python, a non-browser TLS stack.
pub const PYTHON_JA4: &str = "t13d181000_85036bcba153_d41ae481755e";
/// Go's `crypto/tls` default list, which every Go program sends. The family
/// table leaves it out as a generic TLS library default.
pub const GO_DEFAULT_JA4: &str = "t13d190900_9dc949149365_97f8aa674fd9";
/// A 43-suite list `FoxIO` attributes to Python but which is not `CPython`'s
/// own default-context list. The family table leaves it out as a generic
/// TLS library default.
pub const GENERIC_PYTHON_JA4: &str = "t13d4312h1_c7886603b240_b26ce05bbdd6";
/// Test-only: Chromium's listed cipher hash with an extension hash no list
/// names, as a later browser release would send.
pub const NEWER_CHROMIUM_JA4: &str = "t13d1517h2_8daaf6152771_0123456789ab";
/// Test-only: a fictional fingerprint whose cipher hash no list names.
pub const UNLISTED_JA4: &str = "t13d1516h2_000000000000_000000000000";

/// Returns `ja4` as a JA4 `EdgeZero` captured from `source`.
///
/// # Panics
///
/// Panics when `ja4` is not a well-formed JA4 fingerprint.
#[must_use]
pub fn captured_ja4(ja4: &str, source: Ja4Source) -> CapturedJa4 {
    CapturedJa4::Captured {
        fingerprint: Ja4Fingerprint::parse(ja4).expect("should parse fixture JA4"),
        source,
    }
}

/// Records `ja4` as the JA4 claim of the request's [`RequestIngress`], where
/// `ctx.ja4()` reads it.
///
/// Keeps the rest of an ingress `EdgeZero` already captured; a request
/// without one gets an ingress that exposes nothing else.
pub fn insert_ja4(extensions: &mut Extensions, ja4: CapturedJa4) {
    let ingress = extensions.remove::<RequestIngress>().unwrap_or_else(|| {
        RequestIngress::new(
            CapturedTarget::Unavailable(TargetUnavailable::NotExposed),
            None,
            HeaderFidelity::default(),
        )
    });
    extensions.insert(ingress.with_ja4(ja4));
}

/// Gives `request` a Chrome User-Agent and Chromium's runtime TLS JA4, the
/// signals of a current browser.
pub fn insert_browser_signals(request: &mut Request) {
    request.headers_mut().insert(
        header::USER_AGENT,
        HeaderValue::from_static(CHROME_USER_AGENT),
    );
    insert_ja4(
        request.extensions_mut(),
        captured_ja4(CHROMIUM_JA4, Ja4Source::RuntimeTls),
    );
}
