//! The JA4 and User-Agent family rule for Edge Cookie creation (ID-7).
//!
//! A browser's TLS `ClientHello` comes from its own TLS stack: Chromium
//! browsers use `BoringSSL`, Firefox uses NSS, and Safari and every iOS
//! browser use Apple's stack. `JA4_b`, the middle section of a JA4
//! fingerprint, is a truncated hash of the sorted cipher-suite list. It names
//! that stack, ignores GREASE and Chromium's extension-order randomization,
//! and survives browser releases that only change extensions (`JA4_c`).
//!
//! A request whose trusted JA4 names one family while its User-Agent claims a
//! browser of another family contradicts itself. A non-browser TLS stack
//! (Python, Go) behind a browser User-Agent is the common case. The rule is
//! deliberately narrow: an unrecognized `JA4_b`, an absent JA4, or a
//! User-Agent that claims no browser never contradicts.
//!
//! The rule reads the full JA4 in memory only; storage keeps JA4 section 1
//! ([`super::kv_types::KvDevice::ja4_class`]).

/// The TLS client family a `JA4_b` cipher hash identifies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TlsFamily {
    /// Chromium-based browsers (`BoringSSL`).
    Chromium,
    /// Firefox and other Gecko browsers (NSS).
    Firefox,
    /// Safari (Apple's TLS stack).
    Safari,
    /// A TLS library no current browser uses.
    NonBrowser,
}

/// `JA4_b` cipher hashes with a known TLS client family.
///
/// Source: `FoxIO`'s public JA4+ mapping, `ja4plus-mapping.csv` at commit
/// `288d3c3d149697ad977f868651bcd42a7a1a82da` of
/// <https://github.com/FoxIO-LLC/ja4>. Each entry quotes the row it comes
/// from. Rows whose cipher hash is shared with a Windows system TLS stack
/// (`WinINET`) are left out, so a Windows component never counts as a
/// non-browser.
///
/// To refresh: read the current `ja4plus-mapping.csv` (or the JA4 database it
/// links to), take the middle section of each `ja4` value whose Application or
/// Library names a browser or a non-browser HTTP library, and add or remove
/// rows here. Add only cipher hashes that a public source attributes; an
/// unlisted hash never blocks, so a missing row costs coverage, not traffic.
/// Then update the tests below and `docs/guide/edge-cookies.md`.
const CIPHER_HASH_FAMILIES: &[(&str, TlsFamily)] = &[
    // Chromium Browser: t13d1516h2_8daaf6152771_02713d6af862 and its
    // t13d1517h2 / t13i variants.
    ("8daaf6152771", TlsFamily::Chromium),
    // Chromium Browser over QUIC: q13d0312h3_55b375c5d22e_06cda9e17597.
    ("55b375c5d22e", TlsFamily::Chromium),
    // Mozilla Firefox: t13d1715h2_5b57614c22b0_7121afd63204.
    ("5b57614c22b0", TlsFamily::Firefox),
    // Safari: t13d2014h2_a09f3c656075_14788d8d241b.
    ("a09f3c656075", TlsFamily::Safari),
    // Python: t13d181000_85036bcba153_d41ae481755e.
    ("85036bcba153", TlsFamily::NonBrowser),
    // Python: t13d4312h1_c7886603b240_b26ce05bbdd6.
    ("c7886603b240", TlsFamily::NonBrowser),
    // GoLang: t13d190900_9dc949149365_97f8aa674fd9.
    ("9dc949149365", TlsFamily::NonBrowser),
    // GoLang webhooks: t13d1412h2_e33ad33b3d25_6b314db333b6.
    ("e33ad33b3d25", TlsFamily::NonBrowser),
    // GoLang: t12d160700_8cdfa2d4673b_18dd7303c4a5.
    ("8cdfa2d4673b", TlsFamily::NonBrowser),
    // GoLang: t13d141000_cbb2034c60b8_e7c285222651.
    ("cbb2034c60b8", TlsFamily::NonBrowser),
];

/// What a User-Agent claims about the browser that sent it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowserClaim {
    /// A browser whose TLS stack the User-Agent names.
    Engine(TlsFamily),
    /// A browser whose TLS stack the User-Agent does not settle, such as any
    /// iOS browser. Only a non-browser TLS stack contradicts it.
    AnyEngine,
}

/// Returns `true` when a trusted `ja4`'s TLS client family contradicts the
/// browser family `user_agent` claims.
///
/// With `allow_non_browser_tls` (`[ec].allow_non_browser_tls`), a
/// non-browser TLS stack no longer contradicts; a browser-engine mismatch
/// still does.
#[must_use]
pub(super) fn contradicts_user_agent(
    user_agent: &str,
    ja4: &str,
    allow_non_browser_tls: bool,
) -> bool {
    let (Some(claim), Some(family)) = (browser_claim(user_agent), tls_family(ja4)) else {
        return false;
    };
    match (claim, family) {
        (_, TlsFamily::NonBrowser) => !allow_non_browser_tls,
        (BrowserClaim::Engine(claimed), family) => claimed != family,
        (BrowserClaim::AnyEngine, _) => false,
    }
}

/// Looks up the family of `ja4`'s `JA4_b` cipher hash.
fn tls_family(ja4: &str) -> Option<TlsFamily> {
    let cipher_hash = ja4.split('_').nth(1)?;
    CIPHER_HASH_FAMILIES
        .iter()
        .find_map(|(hash, family)| (*hash == cipher_hash).then_some(*family))
}

/// Reads the browser family a User-Agent claims, or `None` when it claims no
/// browser.
///
/// Every current browser's User-Agent starts with `Mozilla/5.0`. Order
/// matters: iOS User-Agents also contain `Safari/`, and Chromium ones contain
/// `Safari/` too.
fn browser_claim(user_agent: &str) -> Option<BrowserClaim> {
    if !user_agent.starts_with("Mozilla/5.0 ") {
        return None;
    }
    // iOS browsers run on WebKit, but the rule does not assume which TLS
    // stack each one uses, so they claim a browser without naming a family.
    if ["iPhone", "iPad", "iPod", "CriOS/", "FxiOS/", "EdgiOS/"]
        .iter()
        .any(|token| user_agent.contains(token))
    {
        return Some(BrowserClaim::AnyEngine);
    }
    if user_agent.contains("Firefox/") {
        return Some(BrowserClaim::Engine(TlsFamily::Firefox));
    }
    if user_agent.contains("Chrome/") || user_agent.contains("Chromium/") {
        return Some(BrowserClaim::Engine(TlsFamily::Chromium));
    }
    if user_agent.contains("Macintosh")
        && user_agent.contains("Version/")
        && user_agent.contains("Safari/")
    {
        return Some(BrowserClaim::Engine(TlsFamily::Safari));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHROME_MAC_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
        AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36";
    const EDGE_WINDOWS_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
        AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36 Edg/146.0.0.0";
    const FIREFOX_MAC_UA: &str =
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:149.0) Gecko/20100101 Firefox/149.0";
    const SAFARI_MAC_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
        AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";
    const CHROME_IOS_UA: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 26_0 like Mac OS X) \
        AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/146.0.0.0 Mobile/15E148 Safari/604.1";

    // Public fingerprints from FoxIO's ja4plus-mapping.csv (see the table).
    const CHROMIUM_JA4: &str = "t13d1516h2_8daaf6152771_02713d6af862";
    const FIREFOX_JA4: &str = "t13d1715h2_5b57614c22b0_7121afd63204";
    const SAFARI_JA4: &str = "t13d2014h2_a09f3c656075_14788d8d241b";
    const PYTHON_JA4: &str = "t13d181000_85036bcba153_d41ae481755e";
    const GO_JA4: &str = "t13d190900_9dc949149365_97f8aa674fd9";
    // Test-only: a listed cipher hash with an extension hash no list names,
    // as a later browser release would send.
    const NEWER_CHROMIUM_JA4: &str = "t13d1517h2_8daaf6152771_0123456789ab";
    // Test-only: a fictional fingerprint whose cipher hash no list names.
    const UNLISTED_JA4: &str = "t13d1516h2_000000000000_000000000000";

    #[test]
    fn a_browser_user_agent_with_a_non_browser_ja4_contradicts() {
        for ua in [CHROME_MAC_UA, FIREFOX_MAC_UA, SAFARI_MAC_UA, CHROME_IOS_UA] {
            for ja4 in [PYTHON_JA4, GO_JA4] {
                assert!(
                    contradicts_user_agent(ua, ja4, false),
                    "should refuse a non-browser TLS stack behind {ua:?}"
                );
            }
        }
    }

    #[test]
    fn a_browser_engine_mismatch_contradicts() {
        for (ua, ja4) in [
            (CHROME_MAC_UA, FIREFOX_JA4),
            (CHROME_MAC_UA, SAFARI_JA4),
            (FIREFOX_MAC_UA, CHROMIUM_JA4),
            (SAFARI_MAC_UA, CHROMIUM_JA4),
            (EDGE_WINDOWS_UA, FIREFOX_JA4),
        ] {
            assert!(
                contradicts_user_agent(ua, ja4, false),
                "should refuse {ua:?} with another engine's TLS stack"
            );
        }
    }

    #[test]
    fn allowing_non_browser_tls_still_refuses_an_engine_mismatch() {
        for ua in [CHROME_MAC_UA, FIREFOX_MAC_UA, SAFARI_MAC_UA, CHROME_IOS_UA] {
            for ja4 in [PYTHON_JA4, GO_JA4] {
                assert!(
                    !contradicts_user_agent(ua, ja4, true),
                    "should allow a non-browser TLS stack behind {ua:?} when configured"
                );
            }
        }
        assert!(
            contradicts_user_agent(FIREFOX_MAC_UA, CHROMIUM_JA4, true),
            "should still refuse another browser engine's TLS stack"
        );
    }

    #[test]
    fn matching_browser_families_do_not_contradict() {
        for (ua, ja4) in [
            (CHROME_MAC_UA, CHROMIUM_JA4),
            (CHROME_MAC_UA, NEWER_CHROMIUM_JA4),
            (EDGE_WINDOWS_UA, CHROMIUM_JA4),
            (FIREFOX_MAC_UA, FIREFOX_JA4),
            (SAFARI_MAC_UA, SAFARI_JA4),
            (CHROME_IOS_UA, SAFARI_JA4),
            (CHROME_IOS_UA, CHROMIUM_JA4),
        ] {
            assert!(
                !contradicts_user_agent(ua, ja4, false),
                "should allow {ua:?} with its own family's TLS stack"
            );
        }
    }

    #[test]
    fn an_unlisted_ja4_or_a_non_browser_user_agent_never_contradicts() {
        for ua in [CHROME_MAC_UA, FIREFOX_MAC_UA, SAFARI_MAC_UA] {
            for ja4 in [UNLISTED_JA4, "t13d1516h2", "", "not-a-ja4"] {
                assert!(
                    !contradicts_user_agent(ua, ja4, false),
                    "should not refuse {ua:?} for unrecognized JA4 {ja4:?}"
                );
            }
        }
        for ua in ["curl/8.0.0", "python-requests/2.32.0", ""] {
            assert!(
                !contradicts_user_agent(ua, PYTHON_JA4, false),
                "should leave a User-Agent that claims no browser to other gates"
            );
        }
    }
}
