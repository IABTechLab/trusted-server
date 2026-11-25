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
//! behind a browser User-Agent, such as Python's, is the common case. The rule is
//! deliberately narrow: an unrecognized `JA4_b`, an absent JA4, or a
//! User-Agent that claims no browser never contradicts.
//!
//! The rule reads the full JA4 in memory only; storage keeps JA4 section 1
//! ([`super::kv_types::KvDevice::ja4_class`]).

use crate::settings::Ja4ContradictionExempt;

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
/// from.
///
/// A non-browser row is kept only when its cipher list is specific to one
/// client library. A TLS library's generic default list is left out, because
/// every program built on that library sends it, including scanning and
/// corporate proxies that forward real readers. Rows left out on that ground:
///
/// - Python `t13d4312h1_c7886603b240_b26ce05bbdd6`: 43 cipher suites, a broad
///   OpenSSL-style list, not the 17-suite list `CPython` sets for its default
///   context (kept below as `85036bcba153`). Nothing ties it to Python
///   rather than to other OpenSSL clients that send the same list.
/// - Every `GoLang` row (`9dc949149365`, `e33ad33b3d25`, `8cdfa2d4673b`,
///   `cbb2034c60b8`): Go programs use `crypto/tls`'s built-in default cipher
///   list, so the hash names the Go version, not an application.
///   `9dc949149365` appears for plain Go, Sliver Agent and Go's `net`
///   package alike, and the `GoLang` row `t12d190800_d83cc789557e` shares
///   `WinINET`'s hash, so a Go hash can match a system TLS stack.
/// - Chromium over QUIC `q13d0312h3_55b375c5d22e_06cda9e17597`:
///   `55b375c5d22e` hashes the three TLS 1.3 suites (`1301,1302,1303`) that
///   QUIC requires, so every HTTP/3 client sends it, Firefox and Safari
///   included.
///
/// Rows whose cipher hash is shared with a Windows system TLS stack
/// (`WinINET`) are also left out, so a Windows component never counts as a
/// non-browser.
///
/// To refresh: read the current `ja4plus-mapping.csv` (or the JA4 database it
/// links to), take the middle section of each `ja4` value whose Application or
/// Library names a browser or a non-browser HTTP client library, and add or
/// remove rows here under the rules above. Add only cipher hashes that a
/// public source attributes; an unlisted hash never blocks, so a missing row
/// costs coverage, not traffic. Then update the tests below and
/// `docs/guide/edge-cookies.md`.
const CIPHER_HASH_FAMILIES: &[(&str, TlsFamily)] = &[
    // Chromium Browser: t13d1516h2_8daaf6152771_02713d6af862 and its
    // t13d1517h2 / t13i variants.
    ("8daaf6152771", TlsFamily::Chromium),
    // Mozilla Firefox: t13d1715h2_5b57614c22b0_7121afd63204.
    ("5b57614c22b0", TlsFamily::Firefox),
    // Safari: t13d2014h2_a09f3c656075_14788d8d241b.
    ("a09f3c656075", TlsFamily::Safari),
    // Python: t13d181000_85036bcba153_d41ae481755e and its t13i variant. Its
    // 18 suites are the 17 of `CPython`'s own default-context cipher string
    // (`ssl.create_default_context()`) plus the renegotiation SCSV, so the
    // list is Python's choice, not OpenSSL's default.
    ("85036bcba153", TlsFamily::NonBrowser),
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
/// browser family `user_agent` claims and `exempt`
/// (`[ec].ja4_contradiction_exempt`) does not exempt that contradiction.
///
/// [`Ja4ContradictionExempt::NonBrowser`] exempts a non-browser TLS stack but
/// not a browser-engine mismatch; [`Ja4ContradictionExempt::All`] exempts
/// both.
#[must_use]
pub(super) fn contradicts_user_agent(
    user_agent: &str,
    ja4: &str,
    exempt: Ja4ContradictionExempt,
) -> bool {
    if exempt == Ja4ContradictionExempt::All {
        return false;
    }
    let (Some(claim), Some(family)) = (browser_claim(user_agent), tls_family(ja4)) else {
        return false;
    };
    match (claim, family) {
        (_, TlsFamily::NonBrowser) => exempt == Ja4ContradictionExempt::Off,
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
    use crate::test_support::ja4::{
        CHROME_IOS_USER_AGENT, CHROME_USER_AGENT, CHROMIUM_JA4, EDGE_USER_AGENT, FIREFOX_JA4,
        FIREFOX_USER_AGENT, GENERIC_PYTHON_JA4, GO_DEFAULT_JA4, NEWER_CHROMIUM_JA4, PYTHON_JA4,
        QUIC_JA4, SAFARI_JA4, SAFARI_USER_AGENT, UNLISTED_JA4,
    };

    const BROWSER_USER_AGENTS: [&str; 4] = [
        CHROME_USER_AGENT,
        FIREFOX_USER_AGENT,
        SAFARI_USER_AGENT,
        CHROME_IOS_USER_AGENT,
    ];
    const ENGINE_MISMATCHES: [(&str, &str); 5] = [
        (CHROME_USER_AGENT, FIREFOX_JA4),
        (CHROME_USER_AGENT, SAFARI_JA4),
        (FIREFOX_USER_AGENT, CHROMIUM_JA4),
        (SAFARI_USER_AGENT, CHROMIUM_JA4),
        (EDGE_USER_AGENT, FIREFOX_JA4),
    ];

    #[test]
    fn a_browser_user_agent_with_a_non_browser_ja4_contradicts() {
        for ua in BROWSER_USER_AGENTS {
            assert!(
                contradicts_user_agent(ua, PYTHON_JA4, Ja4ContradictionExempt::Off),
                "should refuse a non-browser TLS stack behind {ua:?}"
            );
        }
    }

    #[test]
    fn a_browser_engine_mismatch_contradicts() {
        for (ua, ja4) in ENGINE_MISMATCHES {
            assert!(
                contradicts_user_agent(ua, ja4, Ja4ContradictionExempt::Off),
                "should refuse {ua:?} with another engine's TLS stack"
            );
        }
    }

    #[test]
    fn exempting_non_browser_tls_still_refuses_an_engine_mismatch() {
        for ua in BROWSER_USER_AGENTS {
            assert!(
                !contradicts_user_agent(ua, PYTHON_JA4, Ja4ContradictionExempt::NonBrowser),
                "should allow a non-browser TLS stack behind {ua:?} when exempted"
            );
        }
        for (ua, ja4) in ENGINE_MISMATCHES {
            assert!(
                contradicts_user_agent(ua, ja4, Ja4ContradictionExempt::NonBrowser),
                "should still refuse {ua:?} with another engine's TLS stack"
            );
        }
    }

    #[test]
    fn exempting_all_contradictions_refuses_none() {
        for ua in BROWSER_USER_AGENTS {
            assert!(
                !contradicts_user_agent(ua, PYTHON_JA4, Ja4ContradictionExempt::All),
                "should allow a non-browser TLS stack behind {ua:?}"
            );
        }
        for (ua, ja4) in ENGINE_MISMATCHES {
            assert!(
                !contradicts_user_agent(ua, ja4, Ja4ContradictionExempt::All),
                "should allow {ua:?} with another engine's TLS stack"
            );
        }
    }

    #[test]
    fn matching_browser_families_do_not_contradict() {
        for (ua, ja4) in [
            (CHROME_USER_AGENT, CHROMIUM_JA4),
            (CHROME_USER_AGENT, NEWER_CHROMIUM_JA4),
            (EDGE_USER_AGENT, CHROMIUM_JA4),
            (FIREFOX_USER_AGENT, FIREFOX_JA4),
            (SAFARI_USER_AGENT, SAFARI_JA4),
            (CHROME_IOS_USER_AGENT, SAFARI_JA4),
            (CHROME_IOS_USER_AGENT, CHROMIUM_JA4),
        ] {
            assert!(
                !contradicts_user_agent(ua, ja4, Ja4ContradictionExempt::Off),
                "should allow {ua:?} with its own family's TLS stack"
            );
        }
    }

    #[test]
    fn a_generic_tls_library_default_never_contradicts() {
        for ua in BROWSER_USER_AGENTS {
            for ja4 in [GO_DEFAULT_JA4, GENERIC_PYTHON_JA4] {
                assert!(
                    !contradicts_user_agent(ua, ja4, Ja4ContradictionExempt::Off),
                    "should not refuse {ua:?} for a TLS library's default list {ja4:?}"
                );
            }
        }
    }

    #[test]
    fn the_quic_cipher_list_never_contradicts() {
        for ua in BROWSER_USER_AGENTS {
            assert!(
                !contradicts_user_agent(ua, QUIC_JA4, Ja4ContradictionExempt::Off),
                "should not refuse {ua:?} over HTTP/3, where every client sends one cipher list"
            );
        }
    }

    #[test]
    fn an_unlisted_ja4_or_a_non_browser_user_agent_never_contradicts() {
        for ua in [CHROME_USER_AGENT, FIREFOX_USER_AGENT, SAFARI_USER_AGENT] {
            for ja4 in [UNLISTED_JA4, "t13d1516h2", "", "not-a-ja4"] {
                assert!(
                    !contradicts_user_agent(ua, ja4, Ja4ContradictionExempt::Off),
                    "should not refuse {ua:?} for unrecognized JA4 {ja4:?}"
                );
            }
        }
        for ua in ["curl/8.0.0", "python-requests/2.32.0", ""] {
            assert!(
                !contradicts_user_agent(ua, PYTHON_JA4, Ja4ContradictionExempt::Off),
                "should leave a User-Agent that claims no browser to other gates"
            );
        }
    }
}
