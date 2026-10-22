//! `[ec.identity]`: coordinator policy and per-provider operator settings.
//!
//! Providers never read these. The coordinator applies them on top of each
//! provider's descriptor.

use std::collections::BTreeMap;

use error_stack::Report;
use serde::{Deserialize, Serialize};

use super::ids::IdentityProviderId;
use crate::error::TrustedServerError;

/// Longest hold-mode budget an operator may configure, in milliseconds.
const MAX_HOLD_BUDGET_MS: u64 = 1000;
/// `kv_revalidate_sec` must stay below the 24-hour tombstone TTL.
const TOMBSTONE_TTL_SECS: u64 = 86_400;
/// The legacy `ec.pull_sync_concurrency` default.
const LEGACY_PULL_SYNC_CONCURRENCY: usize = 3;
/// The `max_concurrency` default.
const DEFAULT_MAX_CONCURRENCY: usize = 4;

/// Coordinator policy: one place for every timing choice (`[ec.identity]`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IdentitySettings {
    /// Providers dispatched at once. Replaces `ec.pull_sync_concurrency`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<usize>,
    /// Shared wall-clock budget for all hold-mode providers, milliseconds.
    #[serde(default = "IdentitySettings::default_hold_budget_ms")]
    pub hold_budget_ms: u64,
    /// Wait after a call is cut off by the hold budget, seconds.
    #[serde(default = "IdentitySettings::default_hold_retry_sec")]
    pub hold_retry_sec: u64,
    /// Per provider response, before and after decoding.
    #[serde(default = "IdentitySettings::default_max_response_bytes")]
    pub max_response_bytes: usize,
    /// Seconds a pending marker outlives its provider's timeout.
    #[serde(default = "IdentitySettings::default_pending_grace_sec")]
    pub pending_grace_sec: u64,
    /// Staged cookie entries older than this are not written.
    #[serde(default = "IdentitySettings::default_staged_max_age_sec")]
    pub staged_max_age_sec: u64,
    /// A cookie not confirmed against KV for this long forces a KV read.
    #[serde(default = "IdentitySettings::default_kv_revalidate_sec")]
    pub kv_revalidate_sec: u64,
    /// Per-provider operator settings, keyed by provider ID.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub providers: BTreeMap<String, ProviderSettings>,
}

impl Default for IdentitySettings {
    fn default() -> Self {
        Self {
            max_concurrency: None,
            hold_budget_ms: Self::default_hold_budget_ms(),
            hold_retry_sec: Self::default_hold_retry_sec(),
            max_response_bytes: Self::default_max_response_bytes(),
            pending_grace_sec: Self::default_pending_grace_sec(),
            staged_max_age_sec: Self::default_staged_max_age_sec(),
            kv_revalidate_sec: Self::default_kv_revalidate_sec(),
            providers: BTreeMap::new(),
        }
    }
}

impl IdentitySettings {
    const fn default_hold_budget_ms() -> u64 {
        100
    }

    const fn default_hold_retry_sec() -> u64 {
        300
    }

    const fn default_max_response_bytes() -> usize {
        65_536
    }

    const fn default_pending_grace_sec() -> u64 {
        300
    }

    const fn default_staged_max_age_sec() -> u64 {
        604_800
    }

    const fn default_kv_revalidate_sec() -> u64 {
        3600
    }

    /// `max_concurrency`, else an explicitly changed legacy
    /// `ec.pull_sync_concurrency`, else 4.
    #[must_use]
    pub fn effective_max_concurrency(&self, legacy_pull_concurrency: usize) -> usize {
        self.max_concurrency
            .unwrap_or(if legacy_pull_concurrency == LEGACY_PULL_SYNC_CONCURRENCY {
                DEFAULT_MAX_CONCURRENCY
            } else {
                legacy_pull_concurrency
            })
    }

    /// The settings for `id`: its table if there is one, else `base` (a
    /// provider's own defaults, such as a pull partner's legacy rate limit),
    /// else the defaults.
    #[must_use]
    pub fn provider(
        &self,
        id: &IdentityProviderId,
        base: Option<ProviderSettings>,
    ) -> ProviderSettings {
        self.providers
            .get(id.as_str())
            .cloned()
            .or(base)
            .unwrap_or_default()
    }
}

/// When a provider is called.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderMode {
    /// Before the auction, which waits within the hold budget.
    Hold,
    /// After the response has been sent.
    #[default]
    AfterSend,
}

/// Operator settings for one provider (`[ec.identity.providers.<id>]`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderSettings {
    /// When the provider is called.
    #[serde(default)]
    pub mode: ProviderMode,
    /// Per-call timeout. Default: `hold_budget_ms` in hold mode, 1000 after
    /// send.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Narrows the sources the provider may write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sources: Option<Vec<String>>,
    /// Refresh interval without a provider hint, and the wait after a skip.
    #[serde(default = "ProviderSettings::default_refresh_after_sec")]
    pub refresh_after_sec: u64,
    /// Burst cap per visitor, in 60-second windows.
    #[serde(default = "ProviderSettings::default_max_calls_per_minute")]
    pub max_calls_per_minute: u32,
    /// Transport failures per 60-second window that open the breaker.
    #[serde(default = "ProviderSettings::default_breaker_failures_per_minute")]
    pub breaker_failures_per_minute: u32,
    /// Lower is dropped first when `ts-eids-s` exceeds its cap.
    #[serde(default)]
    pub cookie_priority: i32,
}

impl Default for ProviderSettings {
    fn default() -> Self {
        Self {
            mode: ProviderMode::default(),
            timeout_ms: None,
            sources: None,
            refresh_after_sec: Self::default_refresh_after_sec(),
            max_calls_per_minute: Self::default_max_calls_per_minute(),
            breaker_failures_per_minute: Self::default_breaker_failures_per_minute(),
            cookie_priority: 0,
        }
    }
}

impl ProviderSettings {
    const fn default_refresh_after_sec() -> u64 {
        86_400
    }

    const fn default_max_calls_per_minute() -> u32 {
        1
    }

    const fn default_breaker_failures_per_minute() -> u32 {
        50
    }

    /// The per-call timeout in milliseconds.
    #[must_use]
    pub fn effective_timeout_ms(&self, identity: &IdentitySettings) -> u64 {
        self.timeout_ms.unwrap_or(match self.mode {
            ProviderMode::Hold => identity.hold_budget_ms,
            ProviderMode::AfterSend => 1000,
        })
    }
}

fn config_error(message: String) -> Report<TrustedServerError> {
    Report::new(TrustedServerError::Configuration { message })
}

/// Validates the numeric and per-provider rules of `[ec.identity]`.
///
/// Rules that need the enabled providers (unknown IDs, ownership) run where
/// the provider catalog is built.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] naming the first violation.
pub fn validate_identity_settings(
    settings: &IdentitySettings,
) -> Result<(), Report<TrustedServerError>> {
    if settings.hold_budget_ms > MAX_HOLD_BUDGET_MS {
        return Err(config_error(format!(
            "ec.identity.hold_budget_ms must be at most {MAX_HOLD_BUDGET_MS}"
        )));
    }
    if settings.max_concurrency == Some(0) {
        return Err(config_error(
            "ec.identity.max_concurrency must be at least 1".to_owned(),
        ));
    }
    if settings.kv_revalidate_sec >= TOMBSTONE_TTL_SECS {
        return Err(config_error(format!(
            "ec.identity.kv_revalidate_sec must be below {TOMBSTONE_TTL_SECS}"
        )));
    }
    for (id, provider) in &settings.providers {
        if id.parse::<IdentityProviderId>().is_err() {
            return Err(config_error(format!(
                "ec.identity.providers.{id}: invalid provider id"
            )));
        }
        if provider.refresh_after_sec == 0 {
            return Err(config_error(format!(
                "ec.identity.providers.{id}.refresh_after_sec must be at least 1"
            )));
        }
        if provider.mode == ProviderMode::Hold
            && provider.effective_timeout_ms(settings) > settings.hold_budget_ms
        {
            return Err(config_error(format!(
                "ec.identity.providers.{id}.timeout_ms must not exceed hold_budget_ms in hold mode"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> IdentitySettings {
        toml::from_str(text).expect("should parse identity settings")
    }

    #[test]
    fn defaults_without_any_table() {
        let settings = parse("");
        assert_eq!(settings.hold_budget_ms, 100, "hold budget");
        assert_eq!(settings.hold_retry_sec, 300, "hold retry");
        assert_eq!(settings.max_response_bytes, 65_536, "max bytes");
        assert_eq!(settings.pending_grace_sec, 300, "grace");
        assert_eq!(settings.staged_max_age_sec, 604_800, "staged age");
        assert_eq!(settings.kv_revalidate_sec, 3600, "revalidate");
        assert_eq!(
            settings.effective_max_concurrency(3),
            4,
            "the default legacy value gives the new default"
        );
        assert_eq!(
            settings.effective_max_concurrency(6),
            6,
            "an explicit legacy value is still honoured"
        );
        assert_eq!(
            parse("max_concurrency = 2").effective_max_concurrency(6),
            2,
            "the new key wins"
        );
        let provider = settings.provider(&"anything".parse().expect("should parse id"), None);
        assert_eq!(provider.mode, ProviderMode::AfterSend, "mode");
        assert_eq!(
            provider.effective_timeout_ms(&settings),
            1000,
            "after-send timeout"
        );
        assert_eq!(
            (
                provider.refresh_after_sec,
                provider.max_calls_per_minute,
                provider.breaker_failures_per_minute,
                provider.cookie_priority
            ),
            (86_400, 1, 50, 0),
            "provider defaults"
        );
        validate_identity_settings(&settings).expect("defaults are valid");
    }

    #[test]
    fn provider_table_overrides_and_base_defaults() {
        let settings = parse(
            "hold_budget_ms = 120\n[providers.id5]\nmode = \"hold\"\nsources = [\"id5-sync.com\"]\ncookie_priority = 10\n",
        );
        let provider = settings.provider(&"id5".parse().expect("should parse id"), None);
        assert_eq!(provider.mode, ProviderMode::Hold, "hold mode");
        assert_eq!(
            provider.effective_timeout_ms(&settings),
            120,
            "hold timeout defaults to the budget"
        );
        assert_eq!(
            provider.sources,
            Some(vec!["id5-sync.com".to_owned()]),
            "narrowing"
        );
        assert_eq!(provider.cookie_priority, 10, "priority");

        let base = ProviderSettings {
            max_calls_per_minute: 3,
            ..ProviderSettings::default()
        };
        assert_eq!(
            settings
                .provider(&"pull-x".parse().expect("should parse id"), Some(base))
                .max_calls_per_minute,
            3,
            "a provider without a table uses its base"
        );
    }

    #[test]
    fn bounds_are_enforced() {
        for (name, text) in [
            ("budget over 1000", "hold_budget_ms = 1001"),
            ("zero concurrency", "max_concurrency = 0"),
            ("revalidate a day", "kv_revalidate_sec = 86400"),
            ("zero refresh", "[providers.p]\nrefresh_after_sec = 0"),
            (
                "timeout over budget",
                "[providers.p]\nmode = \"hold\"\ntimeout_ms = 101",
            ),
            ("bad provider id", "[providers.Bad]\nmode = \"hold\""),
        ] {
            assert!(
                validate_identity_settings(&parse(text)).is_err(),
                "should reject: {name}"
            );
        }
        assert!(
            toml::from_str::<IdentitySettings>("[providers.p]\nmode = \"sometimes\"").is_err(),
            "should reject an unknown mode"
        );
        assert!(
            toml::from_str::<IdentitySettings>("unknown_key = 1").is_err(),
            "should reject unknown keys"
        );
    }
}
