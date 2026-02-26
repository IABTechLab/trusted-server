//! Consent forwarding configuration types.
//!
//! Defines the `[consent]` TOML section and its nested sub-sections for
//! controlling how Trusted Server interprets, validates, and forwards
//! privacy consent signals to advertising partners.

use serde::{Deserialize, Serialize};

/// TCF spec recommends 13 months (≈395 days).
const MAX_CONSENT_AGE_DAYS: u32 = 395;

/// How many days newer one string must be to win under the `newest` strategy.
const FRESHNESS_THRESHOLD_DAYS: u32 = 30;

/// EU member states (27) + EEA non-EU (3) + UK GDPR (1).
const GDPR_COUNTRIES: &[&str] = &[
    "AT", "BE", "BG", "HR", "CY", "CZ", "DK", "EE", "FI", "FR", "DE", "GR", "HU", "IE", "IT", "LV",
    "LT", "LU", "MT", "NL", "PL", "PT", "RO", "SK", "SI", "ES", "SE", "IS", "LI", "NO", "GB",
];

/// US states with active comprehensive privacy laws (as of 2026).
const US_PRIVACY_STATES: &[&str] = &[
    "CA", "VA", "CO", "CT", "UT", "MT", "OR", "TX", "FL", "DE", "IA", "NE", "NH", "NJ", "TN", "MN",
    "MD", "IN", "KY", "RI",
];

/// Converts a static `&[&str]` slice to an owned `Vec<String>`.
fn str_vec(codes: &[&str]) -> Vec<String> {
    codes.iter().copied().map(String::from).collect()
}

/// Top-level consent configuration (`[consent]` in TOML).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConsentConfig {
    /// Operating mode for consent handling.
    ///
    /// - `"interpreter"` — decode consent strings and forward structured data
    ///   (recommended; enables observability and enforcement).
    /// - `"proxy"` — forward raw strings without decoding.
    #[serde(default = "default_consent_mode")]
    pub mode: ConsentMode,

    /// Whether to check consent expiration based on TCF timestamps.
    #[serde(default = "default_true")]
    pub check_expiration: bool,

    /// Maximum consent age in days before it is considered expired.
    ///
    /// TCF spec recommends 13 months (≈395 days).
    #[serde(default = "default_max_consent_age_days")]
    pub max_consent_age_days: u32,

    /// GDPR jurisdiction configuration.
    #[serde(default)]
    pub gdpr: GdprConfig,

    /// US state privacy law configuration.
    #[serde(default)]
    pub us_states: UsStatesConfig,

    /// Defaults for constructing a US Privacy string when only `Sec-GPC`
    /// is present and no explicit `us_privacy` cookie exists.
    #[serde(default)]
    pub us_privacy_defaults: UsPrivacyDefaultsConfig,

    /// How to resolve conflicts when both TCF and GPP strings are present
    /// but disagree on consent status.
    #[serde(default)]
    pub conflict_resolution: ConflictResolutionConfig,

    /// Name of the KV Store used for consent persistence.
    ///
    /// When set, consent data is persisted per Synthetic ID so that
    /// returning users without consent cookies can still have their
    /// consent preferences applied. Set to `None` to disable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consent_store: Option<String>,
}

impl Default for ConsentConfig {
    fn default() -> Self {
        Self {
            mode: ConsentMode::Interpreter,
            check_expiration: true,
            max_consent_age_days: MAX_CONSENT_AGE_DAYS,
            gdpr: GdprConfig::default(),
            us_states: UsStatesConfig::default(),
            us_privacy_defaults: UsPrivacyDefaultsConfig::default(),
            conflict_resolution: ConflictResolutionConfig::default(),
            consent_store: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Consent mode
// ---------------------------------------------------------------------------

/// Operating mode for the consent pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConsentMode {
    /// Decode consent strings and forward structured data.
    Interpreter,
    /// Forward raw strings without decoding.
    Proxy,
}

// ---------------------------------------------------------------------------
// Consent forwarding mode (per-partner)
// ---------------------------------------------------------------------------

/// How consent signals are forwarded to a specific partner integration.
///
/// Controls whether consent travels through the `OpenRTB` body, raw `Cookie`
/// headers, or both. The default (`Both`) preserves backward compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConsentForwardingMode {
    /// Forward consent in the `OpenRTB` body only; strip consent cookies.
    OpenrtbOnly,
    /// Forward consent cookies only; omit consent fields from the body.
    CookiesOnly,
    /// Forward consent in both cookies and body (default).
    #[default]
    Both,
}

impl ConsentForwardingMode {
    /// Whether consent cookies should be stripped from forwarded requests.
    ///
    /// Returns `true` for [`OpenrtbOnly`](Self::OpenrtbOnly) since consent
    /// travels exclusively through the request body in that mode.
    #[must_use]
    pub const fn strips_consent_cookies(self) -> bool {
        matches!(self, Self::OpenrtbOnly)
    }

    /// Whether consent fields should be included in the request body.
    ///
    /// Returns `true` for [`OpenrtbOnly`](Self::OpenrtbOnly) and
    /// [`Both`](Self::Both); `false` for [`CookiesOnly`](Self::CookiesOnly).
    #[must_use]
    pub const fn includes_body_consent(self) -> bool {
        !matches!(self, Self::CookiesOnly)
    }
}

// ---------------------------------------------------------------------------
// GDPR
// ---------------------------------------------------------------------------

/// GDPR jurisdiction configuration (`[consent.gdpr]`).
///
/// The `applies_in` list is used for **observability and logging only** — it
/// does NOT cause consent to be synthesized. When a user's country appears in
/// this list, the system logs that GDPR applies, enabling publishers to
/// monitor compliance coverage.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GdprConfig {
    /// ISO 3166-1 alpha-2 country codes where GDPR applies.
    #[serde(default = "default_gdpr_countries")]
    pub applies_in: Vec<String>,
}

impl Default for GdprConfig {
    fn default() -> Self {
        Self {
            applies_in: str_vec(GDPR_COUNTRIES),
        }
    }
}

// ---------------------------------------------------------------------------
// US States
// ---------------------------------------------------------------------------

/// US state privacy law configuration (`[consent.us_states]`).
///
/// Config-driven to avoid recompilation when new state laws take effect.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UsStatesConfig {
    /// US state codes with active comprehensive privacy laws.
    #[serde(default = "default_us_privacy_states")]
    pub privacy_states: Vec<String>,
}

impl Default for UsStatesConfig {
    fn default() -> Self {
        Self {
            privacy_states: str_vec(US_PRIVACY_STATES),
        }
    }
}

// ---------------------------------------------------------------------------
// US Privacy defaults (GPC handling)
// ---------------------------------------------------------------------------

/// Publisher-configurable defaults for constructing a US Privacy string
/// when only the `Sec-GPC` header is present (`[consent.us_privacy_defaults]`).
///
/// These reflect the publisher's actual compliance posture — they are
/// **publisher policy**, not protocol requirements.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UsPrivacyDefaultsConfig {
    /// Whether the publisher has actually shown a CCPA notice to the user.
    #[serde(default = "default_true")]
    pub notice_given: bool,

    /// Whether the publisher is subject to the Limited Service Provider
    /// Agreement.
    #[serde(default)]
    pub lspa_covered: bool,

    /// Whether a `Sec-GPC: 1` header should be interpreted as an opt-out
    /// of sale.
    #[serde(default = "default_true")]
    pub gpc_implies_optout: bool,
}

impl Default for UsPrivacyDefaultsConfig {
    fn default() -> Self {
        Self {
            notice_given: true,
            lspa_covered: false,
            gpc_implies_optout: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Conflict resolution
// ---------------------------------------------------------------------------

/// How to resolve disagreements between GPP and TC String when both are
/// present (`[consent.conflict_resolution]`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConflictResolutionConfig {
    /// Resolution strategy.
    #[serde(default = "default_conflict_mode")]
    pub mode: ConflictMode,

    /// How many days newer one string must be to win under the `newest`
    /// strategy.
    #[serde(default = "default_freshness_threshold_days")]
    pub freshness_threshold_days: u32,
}

impl Default for ConflictResolutionConfig {
    fn default() -> Self {
        Self {
            mode: ConflictMode::Restrictive,
            freshness_threshold_days: FRESHNESS_THRESHOLD_DAYS,
        }
    }
}

/// Conflict resolution strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConflictMode {
    /// Deny consent when signals disagree (most privacy-safe).
    Restrictive,
    /// Use the newer signal based on timestamps.
    Newest,
    /// Grant consent when signals disagree (requires legal review).
    Permissive,
}

// ---------------------------------------------------------------------------
// Serde default value functions
// ---------------------------------------------------------------------------

const fn default_consent_mode() -> ConsentMode {
    ConsentMode::Interpreter
}

const fn default_true() -> bool {
    true
}

const fn default_max_consent_age_days() -> u32 {
    MAX_CONSENT_AGE_DAYS
}

const fn default_conflict_mode() -> ConflictMode {
    ConflictMode::Restrictive
}

const fn default_freshness_threshold_days() -> u32 {
    FRESHNESS_THRESHOLD_DAYS
}

fn default_gdpr_countries() -> Vec<String> {
    str_vec(GDPR_COUNTRIES)
}

fn default_us_privacy_states() -> Vec<String> {
    str_vec(US_PRIVACY_STATES)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{ConflictMode, ConsentConfig, ConsentMode};

    #[test]
    fn default_config_uses_interpreter_mode() {
        let config = ConsentConfig::default();
        assert_eq!(
            config.mode,
            ConsentMode::Interpreter,
            "default mode should be interpreter"
        );
    }

    #[test]
    fn default_config_enables_expiration_checking() {
        let config = ConsentConfig::default();
        assert!(
            config.check_expiration,
            "expiration checking should be enabled by default"
        );
        assert_eq!(
            config.max_consent_age_days, 395,
            "default max age should be 395 days"
        );
    }

    #[test]
    fn default_gdpr_countries_includes_eu_eea_uk() {
        let config = ConsentConfig::default();
        let countries = &config.gdpr.applies_in;
        assert!(
            countries.contains(&"DE".to_owned()),
            "should include Germany"
        );
        assert!(
            countries.contains(&"NO".to_owned()),
            "should include Norway (EEA)"
        );
        assert!(countries.contains(&"GB".to_owned()), "should include UK");
        assert_eq!(
            countries.len(),
            31,
            "should have 31 countries (27 EU + 3 EEA + 1 UK)"
        );
    }

    #[test]
    fn default_us_privacy_states_includes_california() {
        let config = ConsentConfig::default();
        assert!(
            config.us_states.privacy_states.contains(&"CA".to_owned()),
            "should include California"
        );
    }

    #[test]
    fn default_us_privacy_defaults_reflect_common_posture() {
        let config = ConsentConfig::default();
        let defaults = &config.us_privacy_defaults;
        assert!(defaults.notice_given, "notice_given should default to true");
        assert!(
            !defaults.lspa_covered,
            "lspa_covered should default to false"
        );
        assert!(
            defaults.gpc_implies_optout,
            "gpc_implies_optout should default to true"
        );
    }

    #[test]
    fn default_conflict_resolution_is_restrictive() {
        let config = ConsentConfig::default();
        assert_eq!(
            config.conflict_resolution.mode,
            ConflictMode::Restrictive,
            "default conflict mode should be restrictive"
        );
        assert_eq!(
            config.conflict_resolution.freshness_threshold_days, 30,
            "default freshness threshold should be 30 days"
        );
    }

    #[test]
    fn deserializes_from_empty_json() {
        let config: ConsentConfig =
            serde_json::from_str("{}").expect("should deserialize empty JSON with defaults");
        assert_eq!(config.mode, ConsentMode::Interpreter);
        assert!(config.check_expiration);
    }

    #[test]
    fn deserializes_proxy_mode() {
        let config: ConsentConfig =
            serde_json::from_str(r#"{"mode": "proxy"}"#).expect("should deserialize proxy mode");
        assert_eq!(config.mode, ConsentMode::Proxy, "should parse proxy mode");
    }

    #[test]
    fn consent_forwarding_mode_strips_cookies_only_for_openrtb() {
        use super::ConsentForwardingMode;

        assert!(
            ConsentForwardingMode::OpenrtbOnly.strips_consent_cookies(),
            "openrtb_only should strip consent cookies"
        );
        assert!(
            !ConsentForwardingMode::CookiesOnly.strips_consent_cookies(),
            "cookies_only should not strip consent cookies"
        );
        assert!(
            !ConsentForwardingMode::Both.strips_consent_cookies(),
            "both should not strip consent cookies"
        );
    }

    #[test]
    fn consent_forwarding_mode_includes_body_consent_except_cookies_only() {
        use super::ConsentForwardingMode;

        assert!(
            ConsentForwardingMode::OpenrtbOnly.includes_body_consent(),
            "openrtb_only should include body consent"
        );
        assert!(
            !ConsentForwardingMode::CookiesOnly.includes_body_consent(),
            "cookies_only should not include body consent"
        );
        assert!(
            ConsentForwardingMode::Both.includes_body_consent(),
            "both should include body consent"
        );
    }

    #[test]
    fn deserializes_full_config() {
        let json = serde_json::json!({
            "mode": "interpreter",
            "check_expiration": false,
            "max_consent_age_days": 180,
            "gdpr": { "applies_in": ["DE", "FR"] },
            "us_states": { "privacy_states": ["CA"] },
            "us_privacy_defaults": {
                "notice_given": false,
                "lspa_covered": true,
                "gpc_implies_optout": false
            },
            "conflict_resolution": {
                "mode": "newest",
                "freshness_threshold_days": 15
            }
        });
        let config: ConsentConfig =
            serde_json::from_value(json).expect("should deserialize full config");
        assert!(!config.check_expiration);
        assert_eq!(config.max_consent_age_days, 180);
        assert_eq!(config.gdpr.applies_in, vec!["DE", "FR"]);
        assert_eq!(config.us_states.privacy_states, vec!["CA"]);
        assert!(!config.us_privacy_defaults.notice_given);
        assert!(config.us_privacy_defaults.lspa_covered);
        assert_eq!(config.conflict_resolution.mode, ConflictMode::Newest);
        assert_eq!(config.conflict_resolution.freshness_threshold_days, 15);
    }
}
