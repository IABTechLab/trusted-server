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
pub use types::{
    ConsentContext, ConsentSource, PrivacyFlag, RawConsentSignals, TcfConsent, UsPrivacy,
};

use std::time::{SystemTime, UNIX_EPOCH};

use cookie::CookieJar;
use fastly::Request;

use crate::consent_config::{ConflictMode, ConsentConfig, ConsentMode};
use crate::geo::GeoInfo;

/// Number of deciseconds in one day (86 400 seconds × 10).
const DECISECONDS_PER_DAY: u64 = 86_400 * 10;

/// GPP section ID for EU TCF v2.
const GPP_SECTION_ID_TCF_EU_V2: u16 = 2;

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
        let gpp_section_ids = signals
            .raw_gpp_sid
            .as_deref()
            .and_then(gpp::parse_gpp_sid_cookie);
        let gdpr_applies =
            has_eu_tcf_signal(signals.raw_tc_string.is_some(), gpp_section_ids.as_deref());
        log::debug!("Consent proxy mode: jurisdiction={jur}, skipping decode");
        return ConsentContext {
            raw_tc_string: signals.raw_tc_string,
            raw_gpp_string: signals.raw_gpp_string,
            gpp_section_ids,
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
    apply_tcf_conflict_resolution(&mut ctx, input.config);
    apply_expiration_check(&mut ctx, input.config);
    apply_gpc_us_privacy(&mut ctx, input.config);

    // KV Store write: persist cookie-sourced consent for future requests.
    try_kv_write(input, &ctx);

    log_consent_context(&ctx);
    ctx
}

/// Marks TCF consent as expired when it exceeds the configured maximum age.
///
/// Clears whichever decoded TCF source is active (`tcf` or `gpp.eu_tcf`) but
/// preserves raw strings for proxy-mode forwarding.
fn apply_expiration_check(ctx: &mut ConsentContext, config: &ConsentConfig) {
    if !config.check_expiration {
        return;
    }

    let (is_expired, age_days) = {
        let Some(tcf) = effective_tcf(ctx) else {
            return;
        };
        (
            is_consent_expired(tcf, config.max_consent_age_days),
            consent_age_days(tcf),
        )
    };

    if !is_expired {
        return;
    }

    log::warn!(
        "TCF consent expired (age: {age_days}d, max: {}d)",
        config.max_consent_age_days
    );
    ctx.expired = true;

    ctx.tcf = None;
    if let Some(gpp) = &mut ctx.gpp {
        gpp.eu_tcf = None;
    }

    ctx.gdpr_applies =
        has_eu_tcf_signal(ctx.raw_tc_string.is_some(), ctx.gpp_section_ids.as_deref());
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

    // GDPR applies when an EU TCF signal is present via standalone TC string
    // or via GPP section ID 2.
    let gdpr_applies =
        has_eu_tcf_signal(signals.raw_tc_string.is_some(), gpp_section_ids.as_deref());

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

/// Resolves whether an EU TCF signal is present from raw signal hints.
#[must_use]
fn has_eu_tcf_signal(raw_tc_present: bool, gpp_section_ids: Option<&[u16]>) -> bool {
    raw_tc_present || gpp_section_ids.is_some_and(|ids| ids.contains(&GPP_SECTION_ID_TCF_EU_V2))
}

/// Returns the effective decoded TCF consent for enforcement decisions.
#[must_use]
fn effective_tcf(ctx: &ConsentContext) -> Option<&types::TcfConsent> {
    ctx.tcf
        .as_ref()
        .or_else(|| ctx.gpp.as_ref().and_then(|g| g.eu_tcf.as_ref()))
}

/// Returns whether TCF consent allows EID transmission.
#[must_use]
fn allows_eid_transmission(tcf: &types::TcfConsent) -> bool {
    tcf.has_storage_consent() && tcf.has_personalized_ads_consent()
}

/// Resolves conflicts between standalone TC and GPP EU TCF consents.
fn apply_tcf_conflict_resolution(ctx: &mut ConsentContext, config: &ConsentConfig) {
    let Some(standalone_tcf) = ctx.tcf.as_ref() else {
        return;
    };
    let Some(gpp_tcf) = ctx.gpp.as_ref().and_then(|g| g.eu_tcf.as_ref()) else {
        return;
    };

    let standalone_allows = allows_eid_transmission(standalone_tcf);
    let gpp_allows = allows_eid_transmission(gpp_tcf);

    if standalone_allows == gpp_allows {
        return;
    }

    let select_gpp = match config.conflict_resolution.mode {
        ConflictMode::Restrictive => !gpp_allows,
        ConflictMode::Permissive => gpp_allows,
        ConflictMode::Newest => select_newest_signal(
            standalone_tcf,
            gpp_tcf,
            config.conflict_resolution.freshness_threshold_days,
        )
        .unwrap_or(!gpp_allows),
    };

    let source = if select_gpp { "gpp" } else { "standalone" };
    log::info!(
        "TCF conflict detected; mode={:?}, selected={source}",
        config.conflict_resolution.mode
    );

    // Clone only the winner after the decision is made.
    ctx.tcf = Some(if select_gpp {
        gpp_tcf.clone()
    } else {
        return; // Standalone is already in ctx.tcf — nothing to do.
    });
}

/// Returns whether GPP should win under the `newest` strategy.
#[must_use]
fn select_newest_signal(
    standalone_tcf: &types::TcfConsent,
    gpp_tcf: &types::TcfConsent,
    freshness_threshold_days: u32,
) -> Option<bool> {
    let threshold_ds = u64::from(freshness_threshold_days) * DECISECONDS_PER_DAY;

    let standalone_delta = standalone_tcf
        .last_updated_ds
        .saturating_sub(gpp_tcf.last_updated_ds);
    if standalone_delta > threshold_ds {
        return Some(false);
    }

    let gpp_delta = gpp_tcf
        .last_updated_ds
        .saturating_sub(standalone_tcf.last_updated_ds);
    if gpp_delta > threshold_ds {
        return Some(true);
    }

    None
}

/// Returns the current time in deciseconds since the Unix epoch.
pub(crate) fn now_deciseconds() -> u64 {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    dur.as_secs() * 10 + u64::from(dur.subsec_millis()) / 100
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

    let tcf = consent_ctx.and_then(effective_tcf);

    match tcf {
        Some(tcf) if allows_eid_transmission(tcf) => Some(eids),
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
// SSC consent gating
// ---------------------------------------------------------------------------

/// Determines whether SSC (Synthetic Session Cookie) creation is permitted
/// based on the user's consent and detected jurisdiction.
///
/// The decision follows the jurisdiction's consent model:
///
/// - **GDPR (EU/UK)**: opt-in required — TCF Purpose 1 (store/access
///   information on a device) must be explicitly consented. If no TCF data is
///   available under GDPR, consent is assumed absent and SSC is blocked.
/// - **US state privacy**: opt-out model — SSC is allowed unless the user has
///   explicitly opted out via the US Privacy string or Global Privacy Control.
/// - **Non-regulated / Unknown**: SSC is allowed (no consent requirement).
#[must_use]
pub fn allows_ssc_creation(ctx: &ConsentContext) -> bool {
    match &ctx.jurisdiction {
        jurisdiction::Jurisdiction::Gdpr => {
            // EU/UK: explicit opt-in required (TCF Purpose 1 = store/access device).
            match effective_tcf(ctx) {
                Some(tcf) => tcf.has_storage_consent(),
                None => false,
            }
        }
        jurisdiction::Jurisdiction::UsState(_) => {
            // US: opt-out model — allow unless user explicitly opted out.
            if let Some(usp) = &ctx.us_privacy {
                usp.opt_out_sale != PrivacyFlag::Yes
            } else {
                // No US Privacy string — fall back to GPC signal.
                !ctx.gpc
            }
        }
        jurisdiction::Jurisdiction::NonRegulated | jurisdiction::Jurisdiction::Unknown => true,
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
    apply_tcf_conflict_resolution(&mut ctx, input.config);
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
    let usp_status = signal_status(ctx.us_privacy.is_some(), ctx.raw_us_privacy.is_some());

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

    use super::{
        allows_ssc_creation, apply_expiration_check, apply_tcf_conflict_resolution,
        build_consent_context, build_context_from_signals, should_try_kv_fallback,
        ConsentPipelineInput,
    };
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::{
        ConsentContext, GppConsent, PrivacyFlag, RawConsentSignals, TcfConsent, UsPrivacy,
    };
    use crate::consent_config::{ConflictMode, ConsentConfig, ConsentMode};
    use crate::cookies::parse_cookies_to_jar;

    fn make_tcf(last_updated_ds: u64, allows_eids: bool) -> TcfConsent {
        TcfConsent {
            version: 2,
            cmp_id: 1,
            cmp_version: 1,
            consent_screen: 0,
            consent_language: "EN".to_owned(),
            vendor_list_version: 1,
            tcf_policy_version: 4,
            created_ds: last_updated_ds,
            last_updated_ds,
            purpose_consents: vec![
                true,
                false,
                false,
                allows_eids,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
            ],
            purpose_legitimate_interests: vec![false; 24],
            vendor_consents: Vec::new(),
            vendor_legitimate_interests: Vec::new(),
            special_feature_opt_ins: vec![false; 12],
        }
    }

    fn make_conflicting_context(
        standalone_last_updated_ds: u64,
        standalone_allows_eids: bool,
        gpp_last_updated_ds: u64,
        gpp_allows_eids: bool,
    ) -> ConsentContext {
        ConsentContext {
            raw_tc_string: Some("standalone".to_owned()),
            raw_gpp_string: Some("gpp".to_owned()),
            gpp_section_ids: Some(vec![2]),
            gdpr_applies: true,
            tcf: Some(make_tcf(standalone_last_updated_ds, standalone_allows_eids)),
            gpp: Some(GppConsent {
                version: 1,
                section_ids: vec![2],
                eu_tcf: Some(make_tcf(gpp_last_updated_ds, gpp_allows_eids)),
            }),
            ..ConsentContext::default()
        }
    }

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

    #[test]
    fn proxy_mode_marks_gdpr_when_gpp_sid_contains_tcf_section() {
        let jar = parse_cookies_to_jar("__gpp_sid=2,6");
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
            "should set gdpr_applies when __gpp_sid includes section 2"
        );
    }

    #[test]
    fn marks_gdpr_when_gpp_sid_contains_tcf_section_even_if_gpp_decode_fails() {
        let signals = RawConsentSignals {
            raw_gpp_string: Some("invalid-gpp".to_owned()),
            raw_gpp_sid: Some("2,6".to_owned()),
            ..RawConsentSignals::default()
        };

        let ctx = build_context_from_signals(&signals);

        assert!(
            ctx.gdpr_applies,
            "should set gdpr_applies when section 2 exists even if __gpp decode fails"
        );
    }

    #[test]
    fn conflict_resolution_restrictive_prefers_denial() {
        let mut ctx = make_conflicting_context(10, true, 20, false);
        let config = ConsentConfig::default();

        apply_tcf_conflict_resolution(&mut ctx, &config);

        let tcf = ctx
            .tcf
            .expect("should keep an effective TCF after resolution");
        assert!(
            !tcf.has_personalized_ads_consent(),
            "restrictive mode should choose the denying signal"
        );
    }

    #[test]
    fn conflict_resolution_permissive_prefers_grant() {
        let mut ctx = make_conflicting_context(10, true, 20, false);
        let mut config = ConsentConfig::default();
        config.conflict_resolution.mode = ConflictMode::Permissive;

        apply_tcf_conflict_resolution(&mut ctx, &config);

        let tcf = ctx
            .tcf
            .expect("should keep an effective TCF after resolution");
        assert!(
            tcf.has_personalized_ads_consent(),
            "permissive mode should choose the granting signal"
        );
    }

    #[test]
    fn conflict_resolution_newest_prefers_fresher_signal() {
        let mut ctx = make_conflicting_context(2 * super::DECISECONDS_PER_DAY, true, 0, false);
        let mut config = ConsentConfig::default();
        config.conflict_resolution.mode = ConflictMode::Newest;
        config.conflict_resolution.freshness_threshold_days = 1;

        apply_tcf_conflict_resolution(&mut ctx, &config);

        let tcf = ctx
            .tcf
            .expect("should keep an effective TCF after resolution");
        assert!(
            tcf.has_personalized_ads_consent(),
            "newest mode should pick the signal that is newer than threshold"
        );
    }

    #[test]
    fn expiration_clears_gpp_embedded_tcf() {
        let mut ctx = ConsentContext {
            raw_gpp_string: Some("DBACNY".to_owned()),
            gpp_section_ids: Some(vec![2]),
            gdpr_applies: true,
            gpp: Some(GppConsent {
                version: 1,
                section_ids: vec![2],
                eu_tcf: Some(make_tcf(0, true)),
            }),
            ..ConsentContext::default()
        };
        let config = ConsentConfig {
            max_consent_age_days: 0,
            ..ConsentConfig::default()
        };

        apply_expiration_check(&mut ctx, &config);

        assert!(ctx.expired, "should mark embedded TCF as expired");
        assert!(
            ctx.gpp.as_ref().is_some_and(|g| g.eu_tcf.is_none()),
            "should clear decoded GPP embedded TCF when expired"
        );
        assert!(
            ctx.gdpr_applies,
            "should keep gdpr_applies true when raw EU TCF signal is still present"
        );
    }

    // -----------------------------------------------------------------------
    // allows_ssc_creation tests
    // -----------------------------------------------------------------------

    /// Helper: builds a TCF consent with configurable Purpose 1 (storage).
    fn make_tcf_with_storage(has_storage: bool) -> TcfConsent {
        TcfConsent {
            version: 2,
            cmp_id: 1,
            cmp_version: 1,
            consent_screen: 0,
            consent_language: "EN".to_owned(),
            vendor_list_version: 1,
            tcf_policy_version: 4,
            created_ds: 0,
            last_updated_ds: 0,
            purpose_consents: vec![
                has_storage,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
            ],
            purpose_legitimate_interests: vec![false; 24],
            vendor_consents: Vec::new(),
            vendor_legitimate_interests: Vec::new(),
            special_feature_opt_ins: vec![false; 12],
        }
    }

    #[test]
    fn ssc_allowed_gdpr_with_storage_consent() {
        let ctx = ConsentContext {
            jurisdiction: Jurisdiction::Gdpr,
            tcf: Some(make_tcf_with_storage(true)),
            gdpr_applies: true,
            ..ConsentContext::default()
        };
        assert!(
            allows_ssc_creation(&ctx),
            "GDPR + TCF Purpose 1 consented should allow SSC"
        );
    }

    #[test]
    fn ssc_blocked_gdpr_without_storage_consent() {
        let ctx = ConsentContext {
            jurisdiction: Jurisdiction::Gdpr,
            tcf: Some(make_tcf_with_storage(false)),
            gdpr_applies: true,
            ..ConsentContext::default()
        };
        assert!(
            !allows_ssc_creation(&ctx),
            "GDPR + TCF Purpose 1 not consented should block SSC"
        );
    }

    #[test]
    fn ssc_blocked_gdpr_no_tcf_data() {
        let ctx = ConsentContext {
            jurisdiction: Jurisdiction::Gdpr,
            tcf: None,
            gpp: None,
            gdpr_applies: true,
            ..ConsentContext::default()
        };
        assert!(
            !allows_ssc_creation(&ctx),
            "GDPR with no TCF data should block SSC"
        );
    }

    #[test]
    fn ssc_allowed_gdpr_via_gpp_embedded_tcf() {
        let ctx = ConsentContext {
            jurisdiction: Jurisdiction::Gdpr,
            tcf: None,
            gpp: Some(GppConsent {
                version: 1,
                section_ids: vec![2],
                eu_tcf: Some(make_tcf_with_storage(true)),
            }),
            gdpr_applies: true,
            ..ConsentContext::default()
        };
        assert!(
            allows_ssc_creation(&ctx),
            "GDPR + GPP embedded TCF with P1 consent should allow SSC"
        );
    }

    #[test]
    fn ssc_allowed_us_state_no_optout() {
        let ctx = ConsentContext {
            jurisdiction: Jurisdiction::UsState("CA".to_owned()),
            us_privacy: Some(UsPrivacy {
                version: 1,
                notice_given: PrivacyFlag::Yes,
                opt_out_sale: PrivacyFlag::No,
                lspa_covered: PrivacyFlag::NotApplicable,
            }),
            ..ConsentContext::default()
        };
        assert!(
            allows_ssc_creation(&ctx),
            "US state + no opt-out should allow SSC"
        );
    }

    #[test]
    fn ssc_blocked_us_state_opted_out() {
        let ctx = ConsentContext {
            jurisdiction: Jurisdiction::UsState("CA".to_owned()),
            us_privacy: Some(UsPrivacy {
                version: 1,
                notice_given: PrivacyFlag::Yes,
                opt_out_sale: PrivacyFlag::Yes,
                lspa_covered: PrivacyFlag::NotApplicable,
            }),
            ..ConsentContext::default()
        };
        assert!(
            !allows_ssc_creation(&ctx),
            "US state + opt-out should block SSC"
        );
    }

    #[test]
    fn ssc_blocked_us_state_gpc_implies_optout() {
        let ctx = ConsentContext {
            jurisdiction: Jurisdiction::UsState("CA".to_owned()),
            us_privacy: None,
            gpc: true,
            ..ConsentContext::default()
        };
        assert!(
            !allows_ssc_creation(&ctx),
            "US state + GPC=true with no US Privacy string should block SSC"
        );
    }

    #[test]
    fn ssc_allowed_us_state_no_signals() {
        let ctx = ConsentContext {
            jurisdiction: Jurisdiction::UsState("CA".to_owned()),
            us_privacy: None,
            gpc: false,
            ..ConsentContext::default()
        };
        assert!(
            allows_ssc_creation(&ctx),
            "US state + no opt-out signals should allow SSC (opt-out model)"
        );
    }

    #[test]
    fn ssc_allowed_non_regulated() {
        let ctx = ConsentContext {
            jurisdiction: Jurisdiction::NonRegulated,
            ..ConsentContext::default()
        };
        assert!(
            allows_ssc_creation(&ctx),
            "non-regulated jurisdiction should always allow SSC"
        );
    }

    #[test]
    fn ssc_allowed_unknown_jurisdiction() {
        let ctx = ConsentContext {
            jurisdiction: Jurisdiction::Unknown,
            ..ConsentContext::default()
        };
        assert!(
            allows_ssc_creation(&ctx),
            "unknown jurisdiction should allow SSC (no geo data available)"
        );
    }

    #[test]
    fn ssc_us_privacy_not_applicable_allows_ssc() {
        let ctx = ConsentContext {
            jurisdiction: Jurisdiction::UsState("VA".to_owned()),
            us_privacy: Some(UsPrivacy {
                version: 1,
                notice_given: PrivacyFlag::NotApplicable,
                opt_out_sale: PrivacyFlag::NotApplicable,
                lspa_covered: PrivacyFlag::NotApplicable,
            }),
            ..ConsentContext::default()
        };
        assert!(
            allows_ssc_creation(&ctx),
            "US Privacy with opt_out=N/A should allow SSC"
        );
    }
}
