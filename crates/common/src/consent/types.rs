//! Consent signal types.
//!
//! This module defines the full consent type hierarchy:
//!
//! - [`RawConsentSignals`] — raw (undecoded) strings extracted from cookies/headers
//! - [`ConsentContext`] — the normalized output carrying both raw and decoded data
//! - [`UsPrivacy`] / [`PrivacyFlag`] — decoded US Privacy (CCPA) 4-char string
//! - [`TcfConsent`] — decoded TCF v2 core consent data
//! - [`GppConsent`] — decoded GPP consent data
//! - [`Jurisdiction`] — the privacy regime applicable to the request
//! - [`ConsentSource`] — how consent was sourced (cookie, KV store, etc.)

use core::fmt;

// ---------------------------------------------------------------------------
// Raw extraction layer
// ---------------------------------------------------------------------------

/// Raw consent signals extracted from cookies and HTTP headers.
///
/// All fields are optional because any combination of consent mechanisms may be
/// present (or absent) on a given request. No decoding or validation is
/// performed at this stage — the values are preserved exactly as received.
///
/// # Consent sources
///
/// | Field | Source | Standard |
/// |---|---|---|
/// | [`raw_tc_string`](Self::raw_tc_string) | `euconsent-v2` cookie | IAB TCF v2 |
/// | [`raw_gpp_string`](Self::raw_gpp_string) | `__gpp` cookie | IAB GPP |
/// | [`raw_gpp_sid`](Self::raw_gpp_sid) | `__gpp_sid` cookie | IAB GPP |
/// | [`raw_us_privacy`](Self::raw_us_privacy) | `us_privacy` cookie | IAB US Privacy (CCPA) |
/// | [`gpc`](Self::gpc) | `Sec-GPC` header | Global Privacy Control |
#[derive(Debug, Clone, Default)]
pub struct RawConsentSignals {
    /// TCF v2 consent string from the `euconsent-v2` cookie.
    pub raw_tc_string: Option<String>,
    /// GPP consent string from the `__gpp` cookie.
    pub raw_gpp_string: Option<String>,
    /// GPP section IDs from the `__gpp_sid` cookie (raw comma-separated string).
    pub raw_gpp_sid: Option<String>,
    /// US Privacy string from the `us_privacy` cookie (4-character format).
    pub raw_us_privacy: Option<String>,
    /// Global Privacy Control signal from the `Sec-GPC` header.
    ///
    /// When `true`, the browser has signaled the user's opt-out preference.
    pub gpc: bool,
}

impl RawConsentSignals {
    /// Returns `true` when at least one consent cookie signal is present.
    #[must_use]
    pub fn has_cookie_signals(&self) -> bool {
        self.raw_tc_string.is_some()
            || self.raw_gpp_string.is_some()
            || self.raw_gpp_sid.is_some()
            || self.raw_us_privacy.is_some()
    }

    /// Returns `true` when no consent signals were found on the request.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.has_cookie_signals() && !self.gpc
    }
}

impl fmt::Display for RawConsentSignals {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "euconsent-v2=")?;
        match &self.raw_tc_string {
            Some(s) => write!(f, "present ({} chars)", s.len())?,
            None => write!(f, "absent")?,
        }

        write!(f, ", __gpp=")?;
        match &self.raw_gpp_string {
            Some(s) => write!(f, "present ({} chars)", s.len())?,
            None => write!(f, "absent")?,
        }

        write!(f, ", __gpp_sid=")?;
        match &self.raw_gpp_sid {
            Some(s) => write!(f, "\"{}\"", s)?,
            None => write!(f, "absent")?,
        }

        write!(f, ", us_privacy=")?;
        match &self.raw_us_privacy {
            Some(s) => write!(f, "\"{}\"", s)?,
            None => write!(f, "absent")?,
        }

        write!(f, ", Sec-GPC={}", if self.gpc { "1" } else { "absent" })
    }
}

// ---------------------------------------------------------------------------
// Decoded consent types
// ---------------------------------------------------------------------------

/// Normalized consent context extracted from cookies and headers.
///
/// Carries both raw consent strings (for `OpenRTB` forwarding) and decoded
/// structured data (for TS-level enforcement and observability). This is the
/// central type that flows through the entire request lifecycle.
///
/// Built from [`RawConsentSignals`] by the decoding pipeline in
/// [`super::build_consent_context`].
#[derive(Debug, Clone, Default)]
pub struct ConsentContext {
    /// Raw TC String from `euconsent-v2` cookie, passed as-is in `user.consent`.
    pub raw_tc_string: Option<String>,
    /// Raw GPP string from `__gpp` cookie, passed as-is in `regs.gpp`.
    pub raw_gpp_string: Option<String>,
    /// GPP section IDs derived from decoded `__gpp` data.
    ///
    /// The `__gpp_sid` cookie is treated as a transport hint and validated
    /// against decoded section IDs when both are present.
    pub gpp_section_ids: Option<Vec<u16>>,
    /// Raw US Privacy string from `us_privacy` cookie.
    pub raw_us_privacy: Option<String>,
    /// Raw Google Additional Consent (AC) string.
    ///
    /// Covers ad tech providers not in the IAB Global Vendor List but
    /// participating in the Google ecosystem. Format: `{version}~{ids}~dv.`
    pub raw_ac_string: Option<String>,

    /// Whether GDPR applies to this request (derived from TCF presence).
    pub gdpr_applies: bool,
    /// Decoded TCF v2 consent data.
    pub tcf: Option<TcfConsent>,
    /// Decoded GPP consent data.
    pub gpp: Option<GppConsent>,
    /// Decoded US Privacy signal.
    pub us_privacy: Option<UsPrivacy>,

    /// Whether the TCF consent string has expired (age exceeds configured max).
    ///
    /// When `true` and `check_expiration` is enabled, the decoded `tcf` field
    /// is cleared (treated as no consent) but the raw string is preserved for
    /// proxy-mode forwarding.
    pub expired: bool,

    /// Global Privacy Control signal from `Sec-GPC` header.
    pub gpc: bool,
    /// Detected privacy jurisdiction for this request.
    pub jurisdiction: super::jurisdiction::Jurisdiction,
    /// Source of the consent data (for debugging).
    pub source: ConsentSource,
}

impl ConsentContext {
    /// Returns `true` when no consent signals are present.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.raw_tc_string.is_none()
            && self.raw_gpp_string.is_none()
            && self.gpp_section_ids.is_none()
            && self.raw_us_privacy.is_none()
            && self.raw_ac_string.is_none()
            && self.tcf.is_none()
            && self.gpp.is_none()
            && self.us_privacy.is_none()
            && !self.gpc
    }
}

// ---------------------------------------------------------------------------
// TCF v2
// ---------------------------------------------------------------------------

/// Decoded TCF v2.x consent data.
///
/// Extracted from either a standalone TC String (`euconsent-v2` cookie)
/// or from the EU TCF v2.2 section within a GPP string.
///
/// Only the core segment (segment type 0) is decoded. Publisher restrictions,
/// disclosed vendors, and allowed vendors segments are not yet supported.
#[derive(Debug, Clone)]
pub struct TcfConsent {
    /// TCF version (2).
    pub version: u8,
    /// CMP ID that collected this consent.
    pub cmp_id: u16,
    /// CMP version.
    pub cmp_version: u16,
    /// Consent screen number.
    pub consent_screen: u8,
    /// CMP language (ISO 639-1, two uppercase letters).
    pub consent_language: String,
    /// Vendor list version used.
    pub vendor_list_version: u16,
    /// TCF policy version.
    pub tcf_policy_version: u8,
    /// Timestamp when consent was created (deciseconds since epoch).
    pub created_ds: u64,
    /// Timestamp when consent was last updated (deciseconds since epoch).
    pub last_updated_ds: u64,

    /// Purpose consents (24 bits, 1-indexed).
    ///
    /// `true` at index 0 means purpose 1 is consented, etc.
    pub purpose_consents: Vec<bool>,
    /// Purpose legitimate interests (24 bits, 1-indexed).
    pub purpose_legitimate_interests: Vec<bool>,

    /// Vendor IDs with consent granted.
    pub vendor_consents: Vec<u16>,
    /// Vendor IDs with legitimate interest established.
    pub vendor_legitimate_interests: Vec<u16>,

    /// Special feature opt-ins (12 bits).
    pub special_feature_opt_ins: Vec<bool>,
}

impl TcfConsent {
    /// Looks up a 1-indexed purpose in a TCF bitfield.
    ///
    /// Returns `false` for purpose 0 (invalid) and out-of-range indices.
    fn purpose_bit(bits: &[bool], purpose: usize) -> bool {
        purpose
            .checked_sub(1)
            .and_then(|idx| bits.get(idx).copied())
            .unwrap_or(false)
    }

    /// Checks whether consent was granted for a specific TCF purpose.
    ///
    /// Purposes are 1-indexed per the TCF specification (Purpose 1 = index 0).
    /// Returns `false` if the purpose is out of range.
    #[must_use]
    pub fn has_purpose_consent(&self, purpose: usize) -> bool {
        Self::purpose_bit(&self.purpose_consents, purpose)
    }

    /// Checks whether legitimate interest was established for a specific TCF purpose.
    ///
    /// Purposes are 1-indexed per the TCF specification.
    /// Returns `false` if the purpose is out of range.
    #[must_use]
    pub fn has_purpose_li(&self, purpose: usize) -> bool {
        Self::purpose_bit(&self.purpose_legitimate_interests, purpose)
    }

    /// Checks whether a specific vendor has been granted consent.
    #[must_use]
    pub fn has_vendor_consent(&self, vendor_id: u16) -> bool {
        self.vendor_consents.contains(&vendor_id)
    }

    /// Checks whether a specific vendor has established legitimate interest.
    #[must_use]
    pub fn has_vendor_li(&self, vendor_id: u16) -> bool {
        self.vendor_legitimate_interests.contains(&vendor_id)
    }

    /// Whether Purpose 1 (Store/access information on a device) is consented.
    ///
    /// Required for any EID or cookie-based identifier to be set.
    #[must_use]
    pub fn has_storage_consent(&self) -> bool {
        self.has_purpose_consent(1)
    }

    /// Whether Purpose 2 (Basic ads) is consented.
    ///
    /// Required for bid adapters to participate in the auction.
    #[must_use]
    pub fn has_basic_ads_consent(&self) -> bool {
        self.has_purpose_consent(2)
    }

    /// Whether Purpose 4 (Personalized ads) is consented.
    ///
    /// Controls whether user first-party data and EIDs are transmitted.
    #[must_use]
    pub fn has_personalized_ads_consent(&self) -> bool {
        self.has_purpose_consent(4)
    }
}

// ---------------------------------------------------------------------------
// GPP
// ---------------------------------------------------------------------------

/// Decoded GPP (Global Privacy Platform) consent data.
///
/// Wraps the `iab_gpp` crate's decoded output with our domain types.
#[derive(Debug, Clone)]
pub struct GppConsent {
    /// GPP header version.
    pub version: u8,
    /// Active section IDs present in the GPP string.
    pub section_ids: Vec<u16>,
    /// Decoded EU TCF v2.2 section (if present in GPP, section ID 2).
    pub eu_tcf: Option<TcfConsent>,
}

// ---------------------------------------------------------------------------
// US Privacy (CCPA)
// ---------------------------------------------------------------------------

/// Decoded US Privacy string (legacy 4-character format).
///
/// Format: `1YNN` where:
/// - Char 1: Version (always `1`)
/// - Char 2: Notice given (`Y`/`N`/`-`)
/// - Char 3: Opt-out of sale (`Y`/`N`/`-`)
/// - Char 4: LSPA covered (`Y`/`N`/`-`)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsPrivacy {
    /// Specification version (currently always 1).
    pub version: u8,
    /// Whether explicit notice has been given to the consumer.
    pub notice_given: PrivacyFlag,
    /// Whether the consumer has opted out of the sale of personal information.
    pub opt_out_sale: PrivacyFlag,
    /// Whether the transaction is covered by the Limited Service Provider Agreement.
    pub lspa_covered: PrivacyFlag,
}

/// A tri-state flag used in the US Privacy string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivacyFlag {
    /// `Y` — yes / affirmative.
    Yes,
    /// `N` — no / negative.
    No,
    /// `-` — not applicable or unknown.
    NotApplicable,
}

impl fmt::Display for PrivacyFlag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Yes => write!(f, "Y"),
            Self::No => write!(f, "N"),
            Self::NotApplicable => write!(f, "-"),
        }
    }
}

impl From<bool> for PrivacyFlag {
    fn from(value: bool) -> Self {
        if value {
            Self::Yes
        } else {
            Self::No
        }
    }
}

impl fmt::Display for UsPrivacy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}{}{}{}",
            self.version, self.notice_given, self.opt_out_sale, self.lspa_covered,
        )
    }
}

// ---------------------------------------------------------------------------
// Metadata types
// ---------------------------------------------------------------------------

/// How consent was sourced for this request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConsentSource {
    /// Read from cookies on the incoming request.
    Cookie,
    /// Loaded from KV store via `SyntheticID` lookup.
    KvStore,
    /// Applied from explicit publisher policy defaults.
    PolicyDefault,
    /// No consent data available.
    #[default]
    None,
}

// ---------------------------------------------------------------------------
// Consent error
// ---------------------------------------------------------------------------

/// Errors that can occur during consent string decoding.
#[derive(Debug, derive_more::Display)]
pub enum ConsentDecodeError {
    /// The US Privacy string has an invalid format.
    #[display("invalid US Privacy string: {reason}")]
    InvalidUsPrivacy { reason: String },
    /// The TC String could not be decoded.
    #[display("invalid TC String: {reason}")]
    InvalidTcString { reason: String },
    /// The GPP string could not be decoded.
    #[display("invalid GPP string: {reason}")]
    InvalidGppString { reason: String },
}

impl core::error::Error for ConsentDecodeError {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_signals() {
        let signals = RawConsentSignals::default();
        assert!(signals.is_empty(), "default signals should be empty");
    }

    #[test]
    fn not_empty_with_tc_string() {
        let signals = RawConsentSignals {
            raw_tc_string: Some("CPXxGfAPXxGfA".to_owned()),
            ..Default::default()
        };
        assert!(!signals.is_empty(), "should not be empty with tc_string");
    }

    #[test]
    fn not_empty_with_gpc() {
        let signals = RawConsentSignals {
            gpc: true,
            ..Default::default()
        };
        assert!(!signals.is_empty(), "should not be empty with gpc=true");
    }

    #[test]
    fn has_no_cookie_signals_with_only_gpc() {
        let signals = RawConsentSignals {
            gpc: true,
            ..Default::default()
        };

        assert!(
            !signals.has_cookie_signals(),
            "should not report cookie signals when only gpc is present"
        );
    }

    #[test]
    fn has_cookie_signals_with_tc_string() {
        let signals = RawConsentSignals {
            raw_tc_string: Some("CPXxGfAPXxGfA".to_owned()),
            ..Default::default()
        };

        assert!(
            signals.has_cookie_signals(),
            "should report cookie signals when tc string is present"
        );
    }

    #[test]
    fn display_all_absent() {
        let signals = RawConsentSignals::default();
        let output = signals.to_string();
        assert!(
            output.contains("euconsent-v2=absent"),
            "should show euconsent-v2 absent"
        );
        assert!(output.contains("__gpp=absent"), "should show __gpp absent");
        assert!(
            output.contains("us_privacy=absent"),
            "should show us_privacy absent"
        );
        assert!(
            output.contains("Sec-GPC=absent"),
            "should show Sec-GPC absent"
        );
    }

    #[test]
    fn display_with_values() {
        let signals = RawConsentSignals {
            raw_tc_string: Some("CPXxGfAPXxGfA".to_owned()),
            raw_gpp_string: Some("DBACNYA~CPXxGfA".to_owned()),
            raw_gpp_sid: Some("2,6".to_owned()),
            raw_us_privacy: Some("1YNN".to_owned()),
            gpc: true,
        };
        let output = signals.to_string();
        assert!(
            output.contains("euconsent-v2=present (13 chars)"),
            "should show tc_string length"
        );
        assert!(
            output.contains("__gpp=present (15 chars)"),
            "should show gpp length"
        );
        assert!(
            output.contains("__gpp_sid=\"2,6\""),
            "should show gpp_sid value"
        );
        assert!(
            output.contains("us_privacy=\"1YNN\""),
            "should show us_privacy value"
        );
        assert!(output.contains("Sec-GPC=1"), "should show Sec-GPC as 1");
    }

    #[test]
    fn consent_context_empty_by_default() {
        let ctx = ConsentContext::default();
        assert!(ctx.is_empty(), "default ConsentContext should be empty");
    }

    #[test]
    fn consent_context_not_empty_with_tc_string() {
        let ctx = ConsentContext {
            raw_tc_string: Some("CPXx".to_owned()),
            ..Default::default()
        };
        assert!(
            !ctx.is_empty(),
            "should not be empty with raw_tc_string present"
        );
    }

    #[test]
    fn consent_context_not_empty_with_gpc() {
        let ctx = ConsentContext {
            gpc: true,
            ..Default::default()
        };
        assert!(!ctx.is_empty(), "should not be empty with gpc=true");
    }

    #[test]
    fn us_privacy_display() {
        let usp = UsPrivacy {
            version: 1,
            notice_given: PrivacyFlag::Yes,
            opt_out_sale: PrivacyFlag::No,
            lspa_covered: PrivacyFlag::NotApplicable,
        };
        assert_eq!(usp.to_string(), "1YN-", "should format as 1YN-");
    }

    #[test]
    fn privacy_flag_display() {
        assert_eq!(PrivacyFlag::Yes.to_string(), "Y");
        assert_eq!(PrivacyFlag::No.to_string(), "N");
        assert_eq!(PrivacyFlag::NotApplicable.to_string(), "-");
    }

    #[test]
    fn consent_source_default_is_none() {
        assert_eq!(
            ConsentSource::default(),
            ConsentSource::None,
            "default source should be None"
        );
    }

    fn make_tcf_consent() -> TcfConsent {
        TcfConsent {
            version: 2,
            cmp_id: 1,
            cmp_version: 1,
            consent_screen: 1,
            consent_language: "EN".to_owned(),
            vendor_list_version: 42,
            tcf_policy_version: 4,
            created_ds: 0,
            last_updated_ds: 0,
            // Purposes 1, 2, 4 consented (indices 0, 1, 3)
            purpose_consents: vec![
                true, true, false, true, false, false, false, false, false, false, false, false,
            ],
            // Purpose 7 LI (index 6)
            purpose_legitimate_interests: vec![
                false, false, false, false, false, false, true, false, false, false, false, false,
            ],
            vendor_consents: vec![10, 32, 755],
            vendor_legitimate_interests: vec![32],
            special_feature_opt_ins: vec![false; 12],
        }
    }

    #[test]
    fn tcf_has_purpose_consent() {
        let tcf = make_tcf_consent();
        assert!(tcf.has_purpose_consent(1), "should have Purpose 1 consent");
        assert!(tcf.has_purpose_consent(2), "should have Purpose 2 consent");
        assert!(
            !tcf.has_purpose_consent(3),
            "should not have Purpose 3 consent"
        );
        assert!(tcf.has_purpose_consent(4), "should have Purpose 4 consent");
    }

    #[test]
    fn tcf_purpose_consent_out_of_range() {
        let tcf = make_tcf_consent();
        assert!(
            !tcf.has_purpose_consent(0),
            "purpose 0 should return false (1-indexed)"
        );
        assert!(
            !tcf.has_purpose_consent(99),
            "out-of-range purpose should return false"
        );
    }

    #[test]
    fn tcf_has_purpose_li() {
        let tcf = make_tcf_consent();
        assert!(
            tcf.has_purpose_li(7),
            "should have Purpose 7 legitimate interest"
        );
        assert!(
            !tcf.has_purpose_li(1),
            "should not have Purpose 1 legitimate interest"
        );
    }

    #[test]
    fn tcf_has_vendor_consent() {
        let tcf = make_tcf_consent();
        assert!(
            tcf.has_vendor_consent(755),
            "should have consent for vendor 755"
        );
        assert!(
            !tcf.has_vendor_consent(999),
            "should not have consent for vendor 999"
        );
    }

    #[test]
    fn tcf_convenience_methods() {
        let tcf = make_tcf_consent();
        assert!(
            tcf.has_storage_consent(),
            "should have storage consent (P1)"
        );
        assert!(
            tcf.has_basic_ads_consent(),
            "should have basic ads consent (P2)"
        );
        assert!(
            tcf.has_personalized_ads_consent(),
            "should have personalized ads consent (P4)"
        );
    }
}
