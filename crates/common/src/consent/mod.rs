//! Consent signal extraction, decoding, and normalization.
//!
//! This module implements the consent forwarding pipeline:
//!
//! 1. **Extract** raw consent strings from cookies and HTTP headers.
//! 2. **Decode** each signal into structured data (TCF v2, GPP, US Privacy).
//! 3. **Build** a normalized [`ConsentContext`] that flows through the auction
//!    pipeline and populates `OpenRTB` bid requests.
//!
//! # Supported signals
//!
//! - **TCF v2** — `euconsent-v2` cookie (IAB Transparency & Consent Framework)
//! - **GPP** — `__gpp` and `__gpp_sid` cookies (IAB Global Privacy Platform)
//! - **US Privacy** — `us_privacy` cookie (IAB US Privacy / CCPA)
//! - **GPC** — `Sec-GPC` header (Global Privacy Control)
//!
//! # Usage
//!
//! ```ignore
//! let consent = consent::build_consent_context(jar.as_ref(), &req);
//! ```

mod extraction;
pub mod gpp;
pub mod tcf;
pub mod types;
pub mod us_privacy;

pub use extraction::extract_consent_signals;
pub use types::{ConsentContext, ConsentSource, RawConsentSignals};

use cookie::CookieJar;
use fastly::Request;

/// Extracts, decodes, and normalizes consent signals from a request.
///
/// This is the primary entry point for the consent pipeline. It:
///
/// 1. Reads raw consent strings from cookies and headers.
/// 2. Decodes each signal (TCF v2, GPP, US Privacy).
/// 3. Builds a [`ConsentContext`] with both raw and decoded data.
/// 4. Logs a summary for observability.
///
/// Decoding failures are logged and the corresponding decoded field is set to
/// `None` — the raw string is still preserved for proxy-mode forwarding.
pub fn build_consent_context(jar: Option<&CookieJar>, req: &Request) -> ConsentContext {
    let signals = extract_consent_signals(jar, req);
    log_consent_signals(&signals);
    build_context_from_signals(&signals)
}

/// Extracts raw consent signals and logs them (without decoding).
///
/// Use this when you need the raw signals but don't need decoded data.
/// Prefer [`build_consent_context`] for the full pipeline.
pub fn extract_and_log_consent(jar: Option<&CookieJar>, req: &Request) -> RawConsentSignals {
    let signals = extract_consent_signals(jar, req);
    log_consent_signals(&signals);
    signals
}

/// Builds a [`ConsentContext`] from previously extracted raw signals.
///
/// This is the decode + normalize stage of the pipeline. Each signal is
/// decoded independently; failures are logged at `warn` level and the
/// corresponding decoded field is left as `None`.
#[must_use]
pub fn build_context_from_signals(signals: &RawConsentSignals) -> ConsentContext {
    // Decode US Privacy
    let decoded_us_privacy =
        signals
            .raw_us_privacy
            .as_deref()
            .and_then(|s| match us_privacy::decode_us_privacy(s) {
                Ok(usp) => Some(usp),
                Err(e) => {
                    log::warn!("Failed to decode US Privacy string: {e}");
                    None
                }
            });

    // Decode TCF v2
    let decoded_tcf =
        signals
            .raw_tc_string
            .as_deref()
            .and_then(|s| match tcf::decode_tc_string(s) {
                Ok(tcf) => Some(tcf),
                Err(e) => {
                    log::warn!("Failed to decode TC String: {e}");
                    None
                }
            });

    // Decode GPP
    let decoded_gpp =
        signals
            .raw_gpp_string
            .as_deref()
            .and_then(|s| match gpp::decode_gpp_string(s) {
                Ok(gpp_consent) => Some(gpp_consent),
                Err(e) => {
                    log::warn!("Failed to decode GPP string: {e}");
                    None
                }
            });

    // Resolve GPP section IDs:
    // - Prefer decoded GPP section IDs (authoritative).
    // - Fall back to __gpp_sid cookie (transport hint).
    let gpp_section_ids = decoded_gpp
        .as_ref()
        .map(|g| g.section_ids.clone())
        .or_else(|| {
            signals
                .raw_gpp_sid
                .as_deref()
                .and_then(gpp::parse_gpp_sid_cookie)
        });

    // GDPR applies if we have a TCF string (standalone or from GPP).
    let gdpr_applies =
        decoded_tcf.is_some() || decoded_gpp.as_ref().is_some_and(|g| g.eu_tcf.is_some());

    ConsentContext {
        raw_tc_string: signals.raw_tc_string.clone(),
        raw_gpp_string: signals.raw_gpp_string.clone(),
        gpp_section_ids,
        raw_us_privacy: signals.raw_us_privacy.clone(),

        gdpr_applies,
        tcf: decoded_tcf,
        gpp: decoded_gpp,
        us_privacy: decoded_us_privacy,

        gpc: signals.gpc,
        source: ConsentSource::Cookie,
    }
}

/// Logs a summary of the extracted consent signals.
///
/// Emits an `info`-level log line when at least one consent signal is present,
/// or a `debug`-level line when no signals were found.
pub fn log_consent_signals(signals: &RawConsentSignals) {
    if signals.is_empty() {
        log::debug!("No consent signals found on request");
    } else {
        log::info!("Consent signals: {}", signals);
    }
}
