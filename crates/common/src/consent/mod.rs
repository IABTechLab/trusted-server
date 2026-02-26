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
//! let consent = consent::build_consent_context(&consent::ConsentPipelineInput {
//!     jar: cookie_jar.as_ref(),
//!     req: &req,
//!     config: &settings.consent,
//!     geo: geo.as_ref(),
//!     synthetic_id: Some("sid_abc123"),
//! });
//! ```

mod extraction;
pub mod gpp;
pub mod jurisdiction;
pub mod kv;
pub mod tcf;
pub mod types;
pub mod us_privacy;

pub use extraction::extract_consent_signals;
pub use types::{ConsentContext, ConsentSource, PrivacyFlag, RawConsentSignals, TcfConsent};

use std::time::{SystemTime, UNIX_EPOCH};

use cookie::CookieJar;
use fastly::Request;

use crate::consent_config::{ConsentConfig, ConsentMode};
use crate::geo::GeoInfo;

/// Number of deciseconds in one day (86 400 seconds × 10).
const DECISECONDS_PER_DAY: u64 = 86_400 * 10;

/// Inputs to the consent processing pipeline.
///
/// Bundles all data needed to extract, decode, classify, and validate
/// consent signals from a single request.
pub struct ConsentPipelineInput<'a> {
    /// Parsed cookie jar from the incoming request.
    pub jar: Option<&'a CookieJar>,
    /// The incoming HTTP request (for header access).
    pub req: &'a Request,
    /// Publisher consent configuration.
    pub config: &'a ConsentConfig,
    /// Geolocation data from the request (for jurisdiction detection).
    pub geo: Option<&'a GeoInfo>,
    /// Synthetic ID for KV Store consent persistence.
    ///
    /// When set along with `config.consent_store`, enables:
    /// - **Read fallback**: loads consent from KV when cookies are absent.
    /// - **Write-on-change**: persists cookie-sourced consent to KV.
    pub synthetic_id: Option<&'a str>,
}

/// Extracts, decodes, and normalizes consent signals from a request.
///
/// This is the primary entry point for the consent pipeline. It:
///
/// 1. Reads raw consent strings from cookies and headers.
/// 2. Decodes each signal (TCF v2, GPP, US Privacy).
/// 3. Detects the privacy jurisdiction from geolocation.
/// 4. Checks consent expiration (if enabled).
/// 5. Constructs a US Privacy string from GPC when appropriate.
/// 6. Builds a [`ConsentContext`] with both raw and decoded data.
/// 7. Logs a summary for observability.
///
/// Decoding failures are logged and the corresponding decoded field is set to
/// `None` — the raw string is still preserved for proxy-mode forwarding.
pub fn build_consent_context(input: &ConsentPipelineInput<'_>) -> ConsentContext {
    let signals = extract_consent_signals(input.jar, input.req);
    log_consent_signals(&signals);

    // In proxy mode, skip decoding entirely.
    if input.config.mode == ConsentMode::Proxy {
        let jur = jurisdiction::detect_jurisdiction(input.geo, input.config);
        let gdpr_applies = signals.raw_tc_string.is_some();
        log::debug!("Consent proxy mode: jurisdiction={jur}, skipping decode");
        return ConsentContext {
            raw_tc_string: signals.raw_tc_string,
            raw_gpp_string: signals.raw_gpp_string,
            gpp_section_ids: signals
                .raw_gpp_sid
                .as_deref()
                .and_then(gpp::parse_gpp_sid_cookie),
            raw_us_privacy: signals.raw_us_privacy,
            raw_ac_string: None,
            gdpr_applies,
            tcf: None,
            gpp: None,
            us_privacy: None,
            expired: false,
            gpc: signals.gpc,
            jurisdiction: jur,
            source: ConsentSource::Cookie,
        };
    }

    // KV Store fallback: if no cookie-based signals exist, try loading
    // persisted consent from the KV Store.
    if should_try_kv_fallback(&signals) {
        if let Some(ctx) = try_kv_fallback(input) {
            log_consent_context(&ctx);
            return ctx;
        }
    }

    let mut ctx = build_context_from_signals(&signals);
    ctx.jurisdiction = jurisdiction::detect_jurisdiction(input.geo, input.config);
    apply_expiration_check(&mut ctx, input.config);
    apply_gpc_us_privacy(&mut ctx, input.config);

    // KV Store write: persist cookie-sourced consent for future requests.
    try_kv_write(input, &ctx);

    log_consent_context(&ctx);
    ctx
}

/// Marks TCF consent as expired when it exceeds the configured maximum age.
///
/// Clears the decoded `tcf` field (treated as no consent) but preserves the
/// raw string for proxy-mode forwarding. Re-evaluates `gdpr_applies` based
/// on whether a GPP EU TCF section is still available.
fn apply_expiration_check(ctx: &mut ConsentContext, config: &ConsentConfig) {
    if !config.check_expiration {
        return;
    }

    let tcf = match &ctx.tcf {
        Some(tcf) => tcf,
        None => return,
    };

    if !is_consent_expired(tcf, config.max_consent_age_days) {
        return;
    }

    let age_days = consent_age_days(tcf);
    log::warn!(
        "TCF consent expired (age: {age_days}d, max: {}d)",
        config.max_consent_age_days
    );
    ctx.expired = true;
    ctx.tcf = None;
    // Re-evaluate: GDPR may still apply if GPP has a TCF section.
    ctx.gdpr_applies = ctx.gpp.as_ref().is_some_and(|g| g.eu_tcf.is_some());
}

/// Constructs a US Privacy string from GPC when no explicit cookie exists
/// and the user is in a US state with a privacy law.
fn apply_gpc_us_privacy(ctx: &mut ConsentContext, config: &ConsentConfig) {
    if !ctx.gpc || ctx.us_privacy.is_some() {
        return;
    }
    if !matches!(&ctx.jurisdiction, jurisdiction::Jurisdiction::UsState(_)) {
        return;
    }

    if let Some(usp) = build_us_privacy_from_gpc(config) {
        log::info!("Constructed US Privacy string from GPC: {usp}");
        ctx.raw_us_privacy = Some(usp.to_string());
        ctx.us_privacy = Some(usp);
        ctx.source = ConsentSource::PolicyDefault;
    }
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

/// Decodes a raw consent string, logging a warning on failure.
///
/// Returns [`None`] and logs at `warn` level if decoding fails, preserving
/// the raw string for proxy-mode forwarding.
fn decode_or_warn<T, E: core::fmt::Display>(
    raw: Option<&str>,
    label: &str,
    decode: fn(&str) -> Result<T, E>,
) -> Option<T> {
    raw.and_then(|s| match decode(s) {
        Ok(value) => Some(value),
        Err(e) => {
            log::warn!("Failed to decode {label}: {e}");
            None
        }
    })
}

/// Builds a [`ConsentContext`] from previously extracted raw signals.
///
/// This is the decode + normalize stage of the pipeline. Each signal is
/// decoded independently; failures are logged at `warn` level and the
/// corresponding decoded field is left as `None`.
#[must_use]
pub fn build_context_from_signals(signals: &RawConsentSignals) -> ConsentContext {
    let decoded_us_privacy = decode_or_warn(
        signals.raw_us_privacy.as_deref(),
        "US Privacy string",
        us_privacy::decode_us_privacy,
    );
    let decoded_tcf = decode_or_warn(
        signals.raw_tc_string.as_deref(),
        "TC String",
        tcf::decode_tc_string,
    );
    let decoded_gpp = decode_or_warn(
        signals.raw_gpp_string.as_deref(),
        "GPP string",
        gpp::decode_gpp_string,
    );

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
        // AC string extraction not yet implemented — will be added when
        // the CMP-specific cookie source is determined (Phase 1a).
        raw_ac_string: None,

        gdpr_applies,
        tcf: decoded_tcf,
        gpp: decoded_gpp,
        us_privacy: decoded_us_privacy,

        expired: false,

        gpc: signals.gpc,
        jurisdiction: jurisdiction::Jurisdiction::default(),
        source: ConsentSource::Cookie,
    }
}

/// Returns the current time in deciseconds since the Unix epoch.
pub(crate) fn now_deciseconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
        / 100
}

/// Returns the age of a TCF consent string in days.
fn consent_age_days(tcf: &types::TcfConsent) -> u64 {
    now_deciseconds().saturating_sub(tcf.last_updated_ds) / DECISECONDS_PER_DAY
}

/// Checks whether a TCF consent string has expired.
///
/// Compares `last_updated_ds` (deciseconds since epoch) against the current
/// time and the configured maximum age. Returns `true` if the consent is
/// older than `max_age_days`.
#[must_use]
pub fn is_consent_expired(tcf: &types::TcfConsent, max_age_days: u32) -> bool {
    let max_age_ds = u64::from(max_age_days) * DECISECONDS_PER_DAY;
    now_deciseconds().saturating_sub(tcf.last_updated_ds) > max_age_ds
}

/// Constructs a US Privacy string from `Sec-GPC` and publisher config defaults.
///
/// Called when `gpc = true` but no explicit `us_privacy` cookie exists and the
/// user is in a US state with a privacy law. The resulting string reflects the
/// publisher's configured compliance posture, not a protocol assertion.
///
/// Returns [`None`] if the config says GPC should not imply opt-out.
#[must_use]
pub fn build_us_privacy_from_gpc(config: &ConsentConfig) -> Option<types::UsPrivacy> {
    let defaults = &config.us_privacy_defaults;
    if !defaults.gpc_implies_optout {
        return None;
    }

    Some(types::UsPrivacy {
        version: 1,
        notice_given: PrivacyFlag::from(defaults.notice_given),
        opt_out_sale: PrivacyFlag::Yes,
        lspa_covered: PrivacyFlag::from(defaults.lspa_covered),
    })
}

/// Filters Extended User IDs based on TCF consent.
///
/// Per Prebid's tcfControl enforcement:
/// - **Purpose 1** (Store/access information on a device) must be consented
///   for any EID to exist (identifiers require cookie/localStorage access).
/// - **Purpose 4** (Personalized ads) must be consented for EIDs to be
///   transmitted in the bid request.
///
/// Returns [`None`] if consent is missing or insufficient, stripping all EIDs
/// from the outgoing bid request.
#[must_use]
pub fn gate_eids_by_consent<T>(
    eids: Option<Vec<T>>,
    consent_ctx: Option<&ConsentContext>,
) -> Option<Vec<T>> {
    let eids = eids?;
    if eids.is_empty() {
        return None;
    }

    // Resolve the effective TCF consent — standalone or from GPP.
    let tcf = consent_ctx.and_then(|ctx| {
        ctx.tcf
            .as_ref()
            .or_else(|| ctx.gpp.as_ref().and_then(|g| g.eu_tcf.as_ref()))
    });

    match tcf {
        Some(tcf) if tcf.has_storage_consent() && tcf.has_personalized_ads_consent() => Some(eids),
        Some(_) => {
            log::info!("EIDs stripped: TCF Purpose 1 or 4 consent missing");
            None
        }
        None => {
            // No TCF data — if GDPR applies, block EIDs as a precaution.
            if consent_ctx.is_some_and(|c| c.gdpr_applies) {
                log::info!("EIDs stripped: GDPR applies but no TCF consent available");
                None
            } else {
                Some(eids)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// KV Store integration helpers
// ---------------------------------------------------------------------------

/// Returns whether KV fallback should be attempted for this request.
///
/// KV fallback is used only when cookie-based consent signals are absent.
/// A standalone `Sec-GPC` header should not suppress fallback reads.
#[must_use]
fn should_try_kv_fallback(signals: &RawConsentSignals) -> bool {
    !signals.has_cookie_signals()
}

/// Attempts to load consent from the KV Store when cookie signals are empty.
///
/// Returns `Some(ConsentContext)` if a valid entry was found and decoded,
/// `None` otherwise. Requires both `consent_store` and `synthetic_id` to
/// be configured.
fn try_kv_fallback(input: &ConsentPipelineInput<'_>) -> Option<ConsentContext> {
    let store_name = input.config.consent_store.as_deref()?;
    let synthetic_id = input.synthetic_id?;

    log::debug!("No cookie consent signals, trying KV fallback for '{synthetic_id}'");
    let mut ctx = kv::load_consent_from_kv(store_name, synthetic_id)?;

    // Re-detect jurisdiction from current geo (may differ from stored value).
    ctx.jurisdiction = jurisdiction::detect_jurisdiction(input.geo, input.config);
    apply_expiration_check(&mut ctx, input.config);
    apply_gpc_us_privacy(&mut ctx, input.config);

    Some(ctx)
}

/// Persists cookie-sourced consent to the KV Store when configured.
///
/// Only writes when consent signals are non-empty and have changed since
/// the last write (fingerprint comparison).
fn try_kv_write(input: &ConsentPipelineInput<'_>, ctx: &ConsentContext) {
    let Some(store_name) = input.config.consent_store.as_deref() else {
        return;
    };
    let Some(synthetic_id) = input.synthetic_id else {
        return;
    };

    kv::save_consent_to_kv(
        store_name,
        synthetic_id,
        ctx,
        input.config.max_consent_age_days,
    );
}

// ---------------------------------------------------------------------------
// Logging helpers
// ---------------------------------------------------------------------------

/// Logs a summary of the extracted consent signals.
///
/// Emits an `info`-level log line when at least one consent signal is present,
/// or a `debug`-level line when no signals were found.
fn log_consent_signals(signals: &RawConsentSignals) {
    if signals.is_empty() {
        log::debug!("No consent signals found on request");
    } else {
        log::info!("Consent signals: {}", signals);
    }
}

/// Derives a human-readable status label for a decoded signal.
///
/// Returns `"present"` when decoded data exists, `"decode-failed"` when only
/// the raw string exists, or `"absent"` when neither is available.
fn signal_status(decoded: bool, raw: bool) -> &'static str {
    if decoded {
        "present"
    } else if raw {
        "decode-failed"
    } else {
        "absent"
    }
}

/// Logs a structured summary of the fully-processed consent context.
fn log_consent_context(ctx: &ConsentContext) {
    if ctx.is_empty() {
        return;
    }

    let tcf_status = match (&ctx.tcf, ctx.expired) {
        (Some(_), _) => "present",
        (None, true) => "expired",
        (None, false) if ctx.raw_tc_string.is_some() => "decode-failed",
        _ => "absent",
    };

    let gpp_status = signal_status(ctx.gpp.is_some(), ctx.raw_gpp_string.is_some());
    let usp_status = signal_status(ctx.us_privacy.is_some(), false);

    log::info!(
        "Consent context: jurisdiction={}, tcf={tcf_status}, gpp={gpp_status}, \
         us_privacy={usp_status}, gpc={}, gdpr_applies={}, source={:?}",
        ctx.jurisdiction,
        ctx.gpc,
        ctx.gdpr_applies,
        ctx.source,
    );
}

#[cfg(test)]
mod tests {
    use fastly::Request;

    use super::{build_consent_context, should_try_kv_fallback, ConsentPipelineInput};
    use crate::consent::types::RawConsentSignals;
    use crate::consent_config::{ConsentConfig, ConsentMode};
    use crate::cookies::parse_cookies_to_jar;

    #[test]
    fn kv_fallback_allowed_when_only_gpc_present() {
        let signals = RawConsentSignals {
            gpc: true,
            ..RawConsentSignals::default()
        };

        assert!(
            should_try_kv_fallback(&signals),
            "should allow KV fallback when only Sec-GPC is present"
        );
    }

    #[test]
    fn kv_fallback_skipped_when_cookie_signal_present() {
        let signals = RawConsentSignals {
            raw_tc_string: Some("CPXxGfAPXxGfA".to_owned()),
            gpc: true,
            ..RawConsentSignals::default()
        };

        assert!(
            !should_try_kv_fallback(&signals),
            "should skip KV fallback when cookie signals are present"
        );
    }

    #[test]
    fn proxy_mode_marks_gdpr_when_raw_tc_exists() {
        let jar = parse_cookies_to_jar("euconsent-v2=CPXxGfAPXxGfA");
        let req = Request::get("https://example.com");
        let config = ConsentConfig {
            mode: ConsentMode::Proxy,
            ..ConsentConfig::default()
        };

        let ctx = build_consent_context(&ConsentPipelineInput {
            jar: Some(&jar),
            req: &req,
            config: &config,
            geo: None,
            synthetic_id: None,
        });

        assert!(
            ctx.gdpr_applies,
            "should set gdpr_applies when raw TC string is present in proxy mode"
        );
        assert_eq!(
            ctx.raw_tc_string.as_deref(),
            Some("CPXxGfAPXxGfA"),
            "should preserve raw TC string in proxy mode"
        );
        assert!(ctx.tcf.is_none(), "should skip TCF decoding in proxy mode");
    }
}
