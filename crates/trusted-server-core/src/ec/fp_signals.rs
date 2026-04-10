//! First-party signal collection from browser cookies.
//!
//! Extracts structured partner UIDs from browser cookies set by identity
//! vendors (e.g. UID2, `RampID`). Partners configure which cookie names to check
//! and an optional JSON path to navigate into a structured cookie value.
//!
//! # Module structure
//!
//! - [`FpSignalPartnerConfig`] — per-partner extraction configuration
//! - [`FpSignal`] — a resolved (`partner_id`, uid) pair
//! - [`FpSignalError`] — KV write failures during signal persistence
//! - [`extract_fp_signals`] — main extraction entry point

use std::collections::HashMap;

use cookie::CookieJar;
use error_stack::{Report, ResultExt};
use serde::{Deserialize, Serialize};

use super::kv::{CasWriteOutcome, KvIdentityGraph};
use super::kv_types::{KvPartnerId, MAX_UID_LENGTH};
use super::log_id;

/// Per-partner configuration for first-party signal extraction.
///
/// Stored as part of the partner registry entry. Describes which cookies to
/// inspect and how to navigate their values to extract the partner UID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FpSignalPartnerConfig {
    /// Unique partner identifier matching the partner registry entry.
    pub partner_id: String,
    /// Ordered list of cookie names to check. The first cookie found in the
    /// request jar wins; remaining names are not checked.
    pub cookie_names: Vec<String>,
    /// Optional dot-separated JSON path to extract from a structured cookie
    /// value (e.g. `"advertising_token"` or `"user.id"`).
    ///
    /// When `None`, the raw cookie value is used as the UID.
    pub json_path: Option<String>,
    /// Time-to-live in seconds for the extracted signal in the KV store.
    pub ttl_sec: u64,
}

/// A resolved first-party signal for a single partner.
///
/// Produced by [`extract_fp_signals`] when a matching cookie is found and
/// a non-empty UID is extracted.
#[derive(Debug, Clone, PartialEq)]
pub struct FpSignal {
    /// Partner identifier, matching the source [`FpSignalPartnerConfig`].
    pub partner_id: String,
    /// Extracted user identifier value.
    pub uid: String,
}

/// Errors that can occur when persisting first-party signals to the KV store.
#[derive(Debug, derive_more::Display)]
pub enum FpSignalError {
    /// A KV write operation failed for the given reason.
    #[display("KV write failed: {message}")]
    KvWrite {
        /// Human-readable description of the write failure.
        message: String,
    },
}

impl core::error::Error for FpSignalError {}

/// Walks a dot-separated path through an already-parsed JSON value.
///
/// Follows each `.`-separated segment in `path` through the object tree.
/// Returns `Some(string)` only when the final node is a JSON string. Returns
/// `None` for an empty path, a missing key at any level, or a non-string leaf.
fn walk_json_path(value: &serde_json::Value, path: &str) -> Option<String> {
    if path.is_empty() {
        return None;
    }

    let mut current = value;

    for segment in path.split('.') {
        current = current.get(segment)?;
    }

    current.as_str().map(str::to_owned)
}

/// Extracts a string value from a JSON document by dot-separated path.
///
/// Parses `value` as JSON, then delegates to [`walk_json_path`] to navigate
/// the object tree. Returns `Some(string)` only when the final node is a JSON
/// string. Returns `None` for an empty path, invalid JSON, a missing key at
/// any level, or a non-string leaf.
fn extract_json_path(value: &str, path: &str) -> Option<String> {
    if path.is_empty() {
        return None;
    }

    let parsed: serde_json::Value = serde_json::from_str(value).ok()?;
    walk_json_path(&parsed, path)
}

/// Extracts first-party signals from a cookie jar using partner configurations.
///
/// For each [`FpSignalPartnerConfig`]:
/// 1. Iterates `cookie_names` in order — the first cookie present in `jar` wins.
/// 2. If the cookie value is JSON containing `identity_expires`, validates that
///    `identity_expires > now_ms + 300_000` (5-minute buffer). Expired tokens
///    are skipped.
/// 3. If `json_path` is `Some`, extracts the UID via [`walk_json_path`] when a
///    parsed value is already available, or [`extract_json_path`] otherwise.
///    On extraction failure, logs at `debug` level and skips.
/// 4. If `json_path` is `None`, uses the raw cookie value as the UID.
/// 5. Skips signals with an empty UID after trimming whitespace.
///
/// Returns a [`Vec<FpSignal>`] of successfully extracted signals, one per
/// partner at most.
#[must_use]
pub fn extract_fp_signals(
    jar: &CookieJar,
    configs: &[FpSignalPartnerConfig],
    now_ms: u64,
) -> Vec<FpSignal> {
    let mut signals = Vec::new();

    for config in configs {
        let cookie_value = config
            .cookie_names
            .iter()
            .find_map(|name| jar.get(name).map(|c| c.value().to_owned()));

        let Some(raw) = cookie_value else {
            continue;
        };

        // Parse the cookie value as JSON once. This serves both the UID2 expiry
        // check and the json_path extraction, avoiding a double parse.
        let parsed_json: Option<serde_json::Value> = serde_json::from_str(&raw).ok();

        // UID2 expiry check: if the cookie value is JSON with `identity_expires`,
        // require at least 5 minutes of validity remaining.
        if let Some(ref parsed) = parsed_json {
            if let Some(expires_ms) = parsed
                .get("identity_expires")
                .and_then(serde_json::Value::as_u64)
            {
                let min_valid_ms = now_ms.saturating_add(300_000);
                if expires_ms <= min_valid_ms {
                    log::debug!(
                        "Skipping fp_signal for partner '{}': UID2 token expired or expiring soon \
                         (identity_expires={expires_ms}, now_ms={now_ms})",
                        config.partner_id,
                    );
                    continue;
                }
            }
        }

        let uid = match &config.json_path {
            Some(path) => {
                // Use the already-parsed value when available to avoid a second parse.
                let extracted = match parsed_json.as_ref() {
                    Some(parsed) => walk_json_path(parsed, path),
                    None => extract_json_path(&raw, path),
                };
                match extracted {
                    Some(value) => value,
                    None => {
                        log::debug!(
                            "Skipping fp_signal for partner '{}': JSON path '{}' not found or \
                             non-string in cookie value",
                            config.partner_id,
                            path,
                        );
                        continue;
                    }
                }
            }
            None => raw,
        };

        let uid = uid.trim().to_owned();
        if uid.is_empty() {
            continue;
        }

        if uid.len() > MAX_UID_LENGTH {
            log::debug!(
                "Skipping fp_signal for partner '{}': UID exceeds {MAX_UID_LENGTH} bytes (got {})",
                config.partner_id,
                uid.len(),
            );
            continue;
        }

        signals.push(FpSignal {
            partner_id: config.partner_id.clone(),
            uid,
        });
    }

    signals
}

/// Maximum number of CAS retry attempts for [`write_fp_signals`].
const MAX_RETRIES: u32 = 3;

/// Filters signals to those that are missing or stale in the existing KV entry.
///
/// For each signal, looks up the partner TTL from `configs` and checks whether
/// the existing stored value is still fresh (`now - existing.synced < ttl`).
/// Signals that are fresh are skipped; signals that are missing or stale are
/// included in the returned slice.
fn filter_stale_signals<'a>(
    signals: &'a [FpSignal],
    existing_ids: &HashMap<String, KvPartnerId>,
    configs: &[FpSignalPartnerConfig],
    now: u64,
) -> Vec<&'a FpSignal> {
    let ttl_by_partner: HashMap<&str, u64> = configs
        .iter()
        .map(|c| (c.partner_id.as_str(), c.ttl_sec))
        .collect();

    signals
        .iter()
        .filter(|signal| {
            // A missing TTL means the partner config was removed between
            // extraction and write. Default to 0 (always refresh) since
            // the signal was legitimately extracted with a valid config.
            let ttl = ttl_by_partner
                .get(signal.partner_id.as_str())
                .copied()
                .unwrap_or(0);
            match existing_ids.get(&signal.partner_id) {
                Some(existing) => now.saturating_sub(existing.synced) >= ttl,
                None => true,
            }
        })
        .collect()
}

/// Writes first-party signals to the KV identity graph using optimistic CAS.
///
/// Reads the existing entry for `ec_id`, filters out signals that are still
/// within their configured TTL, then writes the remaining signals back using
/// a CAS write. Retries up to [`MAX_RETRIES`] times on CAS conflict.
///
/// Returns `Ok(())` immediately when:
/// - The entry does not exist (nothing to write to).
/// - The entry is a tombstone (`consent.ok == false`).
/// - All signals are fresh and no write is needed.
///
/// # Errors
///
/// Returns [`FpSignalError::KvWrite`] on non-CAS store errors or after
/// exhausting all retries.
pub fn write_fp_signals(
    kv: &KvIdentityGraph,
    ec_id: &str,
    signals: &[FpSignal],
    configs: &[FpSignalPartnerConfig],
) -> Result<(), Report<FpSignalError>> {
    let now = super::current_timestamp();

    for attempt in 0..MAX_RETRIES {
        let (mut entry, generation) =
            match kv.get(ec_id).change_context(FpSignalError::KvWrite {
                message: format!("Failed to read entry for key '{}…'", log_id(ec_id)),
            })? {
                Some(pair) => pair,
                None => {
                    log::debug!(
                        "write_fp_signals: no entry for '{}…', skipping",
                        log_id(ec_id)
                    );
                    return Ok(());
                }
            };

        if !entry.consent.ok {
            log::debug!(
                "write_fp_signals: entry for '{}…' is a tombstone, skipping",
                log_id(ec_id)
            );
            return Ok(());
        }

        let stale = filter_stale_signals(signals, &entry.ids, configs, now);
        if stale.is_empty() {
            return Ok(());
        }

        for signal in &stale {
            entry.ids.insert(
                signal.partner_id.clone(),
                KvPartnerId {
                    uid: signal.uid.clone(),
                    synced: now,
                },
            );
        }

        match kv
            .cas_write(ec_id, &entry, generation)
            .change_context(FpSignalError::KvWrite {
                message: format!("KV write failed for key '{}…'", log_id(ec_id)),
            })? {
            CasWriteOutcome::Ok => return Ok(()),
            CasWriteOutcome::Conflict => {
                log::debug!(
                    "write_fp_signals: CAS conflict on attempt {}/{MAX_RETRIES} for '{}…', retrying",
                    attempt + 1,
                    log_id(ec_id),
                );
            }
        }
    }

    Err(Report::new(FpSignalError::KvWrite {
        message: format!(
            "CAS conflict after {MAX_RETRIES} retries writing fp_signals for '{}…'",
            log_id(ec_id)
        ),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cookie::Cookie;

    /// Builds a [`CookieJar`] from a list of `(name, value)` pairs.
    fn jar_with(cookies: &[(&str, &str)]) -> CookieJar {
        let mut jar = CookieJar::new();
        for &(name, value) in cookies {
            jar.add(Cookie::new(name.to_owned(), value.to_owned()));
        }
        jar
    }

    /// Builds a minimal [`FpSignalPartnerConfig`] with no JSON path.
    fn raw_config(partner_id: &str, cookie_names: &[&str]) -> FpSignalPartnerConfig {
        FpSignalPartnerConfig {
            partner_id: partner_id.to_owned(),
            cookie_names: cookie_names.iter().map(ToString::to_string).collect(),
            json_path: None,
            ttl_sec: 3600,
        }
    }

    /// Builds a [`FpSignalPartnerConfig`] with a JSON path.
    fn json_config(
        partner_id: &str,
        cookie_names: &[&str],
        json_path: &str,
    ) -> FpSignalPartnerConfig {
        FpSignalPartnerConfig {
            partner_id: partner_id.to_owned(),
            cookie_names: cookie_names.iter().map(ToString::to_string).collect(),
            json_path: Some(json_path.to_owned()),
            ttl_sec: 3600,
        }
    }

    // -------------------------------------------------------------------------
    // filter_stale_signals tests
    // -------------------------------------------------------------------------

    /// Builds a minimal [`FpSignalPartnerConfig`] with a custom TTL.
    fn config_with_ttl(partner_id: &str, ttl_sec: u64) -> FpSignalPartnerConfig {
        FpSignalPartnerConfig {
            partner_id: partner_id.to_owned(),
            cookie_names: vec![],
            json_path: None,
            ttl_sec,
        }
    }

    /// Builds a [`KvPartnerId`] with the given `synced` timestamp.
    fn partner_id_synced(synced: u64) -> KvPartnerId {
        KvPartnerId {
            uid: "some_uid".to_owned(),
            synced,
        }
    }

    #[test]
    fn filter_signals_skips_fresh_ids() {
        // Arrange — signal synced 100s ago with 86400s TTL → still fresh
        let signals = [FpSignal {
            partner_id: "uid2".to_owned(),
            uid: "tok_abc".to_owned(),
        }];
        let mut existing = HashMap::new();
        existing.insert("uid2".to_owned(), partner_id_synced(1000));
        let configs = [config_with_ttl("uid2", 86400)];
        let now = 1100_u64;

        // Act
        let result = filter_stale_signals(&signals, &existing, &configs, now);

        // Assert
        assert!(
            result.is_empty(),
            "should skip signal with fresh existing id"
        );
    }

    #[test]
    fn filter_signals_includes_stale_ids() {
        // Arrange — synced 100000s ago with 86400s TTL → stale
        let signals = [FpSignal {
            partner_id: "uid2".to_owned(),
            uid: "tok_abc".to_owned(),
        }];
        let mut existing = HashMap::new();
        existing.insert("uid2".to_owned(), partner_id_synced(1000));
        let configs = [config_with_ttl("uid2", 86400)];
        let now = 101_000_u64;

        // Act
        let result = filter_stale_signals(&signals, &existing, &configs, now);

        // Assert
        assert_eq!(result.len(), 1, "should include stale signal");
        assert_eq!(
            result[0].partner_id, "uid2",
            "should include the stale uid2 signal"
        );
    }

    #[test]
    fn filter_signals_includes_new_ids() {
        // Arrange — no existing entry for partner
        let signals = [FpSignal {
            partner_id: "liveramp".to_owned(),
            uid: "ramp_123".to_owned(),
        }];
        let existing: HashMap<String, KvPartnerId> = HashMap::new();
        let configs = [config_with_ttl("liveramp", 86400)];
        let now = 50000_u64;

        // Act
        let result = filter_stale_signals(&signals, &existing, &configs, now);

        // Assert
        assert_eq!(
            result.len(),
            1,
            "should include signal with no existing entry"
        );
        assert_eq!(
            result[0].partner_id, "liveramp",
            "should include the new liveramp signal"
        );
    }

    #[test]
    fn filter_signals_mixed_fresh_and_stale() {
        // Arrange — uid2 fresh (synced 100s ago), liveramp stale (synced 100000s ago)
        let signals = [
            FpSignal {
                partner_id: "uid2".to_owned(),
                uid: "tok_uid2".to_owned(),
            },
            FpSignal {
                partner_id: "liveramp".to_owned(),
                uid: "ramp_stale".to_owned(),
            },
        ];
        let now = 200_000_u64;
        let mut existing = HashMap::new();
        // uid2: synced at 199_900 → age 100s, TTL 86400s → fresh
        existing.insert("uid2".to_owned(), partner_id_synced(199_900));
        // liveramp: synced at 100_000 → age 100_000s, TTL 86400s → stale
        existing.insert("liveramp".to_owned(), partner_id_synced(100_000));
        let configs = [
            config_with_ttl("uid2", 86400),
            config_with_ttl("liveramp", 86400),
        ];

        // Act
        let result = filter_stale_signals(&signals, &existing, &configs, now);

        // Assert
        assert_eq!(result.len(), 1, "should include only the stale signal");
        assert_eq!(
            result[0].partner_id, "liveramp",
            "should include liveramp as the stale signal"
        );
    }

    // -------------------------------------------------------------------------
    // extract_json_path tests
    // -------------------------------------------------------------------------

    #[test]
    fn extract_json_path_top_level_key() {
        // Arrange
        let json = r#"{"token": "abc123"}"#;

        // Act
        let result = extract_json_path(json, "token");

        // Assert
        assert_eq!(
            result,
            Some("abc123".to_owned()),
            "should extract top-level string key"
        );
    }

    #[test]
    fn extract_json_path_nested_key() {
        // Arrange
        let json = r#"{"user": {"id": "nested_val"}}"#;

        // Act
        let result = extract_json_path(json, "user.id");

        // Assert
        assert_eq!(
            result,
            Some("nested_val".to_owned()),
            "should extract nested string key"
        );
    }

    #[test]
    fn extract_json_path_missing_key() {
        // Arrange
        let json = r#"{"token": "abc123"}"#;

        // Act
        let result = extract_json_path(json, "missing");

        // Assert
        assert!(result.is_none(), "should return None for missing key");
    }

    #[test]
    fn extract_json_path_non_string_leaf() {
        // Arrange
        let json = r#"{"count": 42}"#;

        // Act
        let result = extract_json_path(json, "count");

        // Assert
        assert!(result.is_none(), "should return None for non-string leaf");
    }

    #[test]
    fn extract_json_path_invalid_json() {
        // Arrange
        let json = "not-json-at-all";

        // Act
        let result = extract_json_path(json, "token");

        // Assert
        assert!(result.is_none(), "should return None for invalid JSON");
    }

    #[test]
    fn extract_json_path_empty_path() {
        // Arrange
        let json = r#"{"token": "abc123"}"#;

        // Act
        let result = extract_json_path(json, "");

        // Assert
        assert!(result.is_none(), "should return None for empty path");
    }

    #[test]
    fn extract_json_path_partial_path() {
        // Arrange — path partially valid but leaf is an object, not a string
        let json = r#"{"user": {"id": "val"}}"#;

        // Act
        let result = extract_json_path(json, "user");

        // Assert
        assert!(
            result.is_none(),
            "should return None when leaf is an object, not a string"
        );
    }

    // -------------------------------------------------------------------------
    // extract_fp_signals tests
    // -------------------------------------------------------------------------

    #[test]
    fn extract_fp_signals_raw_cookie_value() {
        // Arrange
        let jar = jar_with(&[("uid2_token", "raw_uid_value")]);
        let configs = [raw_config("uid2", &["uid2_token"])];

        // Act
        let signals = extract_fp_signals(&jar, &configs, 0);

        // Assert
        assert_eq!(signals.len(), 1, "should extract one signal");
        assert_eq!(
            signals[0].partner_id, "uid2",
            "should have correct partner_id"
        );
        assert_eq!(
            signals[0].uid, "raw_uid_value",
            "should use raw cookie value"
        );
    }

    #[test]
    fn extract_fp_signals_json_cookie_value() {
        // Arrange
        let json = r#"{"advertising_token": "tok_abc"}"#;
        let jar = jar_with(&[("uid2", json)]);
        let configs = [json_config("uid2", &["uid2"], "advertising_token")];

        // Act
        let signals = extract_fp_signals(&jar, &configs, 0);

        // Assert
        assert_eq!(signals.len(), 1, "should extract one signal");
        assert_eq!(
            signals[0].uid, "tok_abc",
            "should extract value at JSON path"
        );
    }

    #[test]
    fn extract_fp_signals_first_match_wins() {
        // Arrange — second cookie name would also match, but first takes precedence
        let jar = jar_with(&[("cookie_a", "uid_from_a"), ("cookie_b", "uid_from_b")]);
        let configs = [raw_config("partner1", &["cookie_a", "cookie_b"])];

        // Act
        let signals = extract_fp_signals(&jar, &configs, 0);

        // Assert
        assert_eq!(signals.len(), 1, "should extract exactly one signal");
        assert_eq!(
            signals[0].uid, "uid_from_a",
            "should use first matching cookie"
        );
    }

    #[test]
    fn extract_fp_signals_first_match_wins_skips_missing_first() {
        // Arrange — first cookie name is absent; second should win
        let jar = jar_with(&[("cookie_b", "uid_from_b")]);
        let configs = [raw_config("partner1", &["cookie_a", "cookie_b"])];

        // Act
        let signals = extract_fp_signals(&jar, &configs, 0);

        // Assert
        assert_eq!(
            signals.len(),
            1,
            "should fall through to second cookie name"
        );
        assert_eq!(
            signals[0].uid, "uid_from_b",
            "should use second cookie when first is absent"
        );
    }

    #[test]
    fn extract_fp_signals_missing_cookie_skips() {
        // Arrange
        let jar = jar_with(&[]);
        let configs = [raw_config("uid2", &["uid2_token"])];

        // Act
        let signals = extract_fp_signals(&jar, &configs, 0);

        // Assert
        assert!(
            signals.is_empty(),
            "should skip partner when cookie is absent"
        );
    }

    #[test]
    fn extract_fp_signals_empty_uid_skips() {
        // Arrange — cookie present but value is whitespace only
        let jar = jar_with(&[("uid2_token", "   ")]);
        let configs = [raw_config("uid2", &["uid2_token"])];

        // Act
        let signals = extract_fp_signals(&jar, &configs, 0);

        // Assert
        assert!(
            signals.is_empty(),
            "should skip signal with empty UID after trim"
        );
    }

    #[test]
    fn extract_fp_signals_failed_json_path_skips() {
        // Arrange — cookie is valid JSON but path doesn't exist
        let jar = jar_with(&[("uid2", r#"{"other_key": "val"}"#)]);
        let configs = [json_config("uid2", &["uid2"], "advertising_token")];

        // Act
        let signals = extract_fp_signals(&jar, &configs, 0);

        // Assert
        assert!(
            signals.is_empty(),
            "should skip when JSON path extraction fails"
        );
    }

    #[test]
    fn extract_fp_signals_nested_json_path() {
        // Arrange
        let json = r#"{"user": {"advertising_token": "nested_tok"}}"#;
        let jar = jar_with(&[("uid2", json)]);
        let configs = [json_config("uid2", &["uid2"], "user.advertising_token")];

        // Act
        let signals = extract_fp_signals(&jar, &configs, 0);

        // Assert
        assert_eq!(signals.len(), 1, "should extract signal from nested path");
        assert_eq!(signals[0].uid, "nested_tok", "should use nested JSON value");
    }

    #[test]
    fn extract_fp_signals_multiple_partners() {
        // Arrange
        let json = r#"{"advertising_token": "uid2_tok"}"#;
        let jar = jar_with(&[("uid2_cookie", json), ("ramp_cookie", "ramp_raw_id")]);
        let configs = [
            json_config("uid2", &["uid2_cookie"], "advertising_token"),
            raw_config("liveramp", &["ramp_cookie"]),
        ];

        // Act
        let signals = extract_fp_signals(&jar, &configs, 0);

        // Assert
        assert_eq!(signals.len(), 2, "should extract one signal per partner");
        assert_eq!(
            signals[0].partner_id, "uid2",
            "should have uid2 as first partner"
        );
        assert_eq!(
            signals[0].uid, "uid2_tok",
            "should extract uid2 advertising_token"
        );
        assert_eq!(
            signals[1].partner_id, "liveramp",
            "should have liveramp as second partner"
        );
        assert_eq!(
            signals[1].uid, "ramp_raw_id",
            "should use raw ramp cookie value"
        );
    }

    // -------------------------------------------------------------------------
    // UID2 expiry tests
    // -------------------------------------------------------------------------

    #[test]
    fn extract_fp_signals_uid2_valid_token_extracted() {
        // Arrange — token expires more than 5 minutes from now
        let now_ms = 1_700_000_000_000_u64;
        let expires_ms = now_ms + 600_000;
        let json =
            format!(r#"{{"advertising_token": "tok_valid", "identity_expires": {expires_ms}}}"#);
        let jar = jar_with(&[("uid2_cookie", &json)]);
        let configs = [json_config("uid2", &["uid2_cookie"], "advertising_token")];

        // Act
        let signals = extract_fp_signals(&jar, &configs, now_ms);

        // Assert
        assert_eq!(signals.len(), 1, "should extract valid UID2 token");
        assert_eq!(
            signals[0].uid, "tok_valid",
            "should use advertising_token from valid entry"
        );
    }

    #[test]
    fn extract_fp_signals_uid2_expired_token_skipped() {
        // Arrange — token expires less than 5 minutes from now
        let now_ms = 1_700_000_000_000_u64;
        let expires_ms = now_ms + 100_000;
        let json =
            format!(r#"{{"advertising_token": "tok_expired", "identity_expires": {expires_ms}}}"#);
        let jar = jar_with(&[("uid2_cookie", &json)]);
        let configs = [json_config("uid2", &["uid2_cookie"], "advertising_token")];

        // Act
        let signals = extract_fp_signals(&jar, &configs, now_ms);

        // Assert
        assert!(signals.is_empty(), "should skip expired UID2 token");
    }

    #[test]
    fn extract_fp_signals_uid2_expiry_at_boundary_skipped() {
        // Arrange — token expires exactly at the 5-minute boundary (identity_expires == now_ms + 300_000)
        // The check is `<=`, so this should be skipped.
        let now_ms = 1_700_000_000_000_u64;
        let expires_ms = now_ms + 300_000;
        let json =
            format!(r#"{{"advertising_token": "tok_boundary", "identity_expires": {expires_ms}}}"#);
        let jar = jar_with(&[("uid2_cookie", &json)]);
        let configs = [json_config("uid2", &["uid2_cookie"], "advertising_token")];

        // Act
        let signals = extract_fp_signals(&jar, &configs, now_ms);

        // Assert
        assert!(
            signals.is_empty(),
            "should skip token at exact 5-minute boundary"
        );
    }

    #[test]
    fn extract_fp_signals_uid2_expiry_just_above_boundary_extracted() {
        // Arrange — token expires one millisecond above the 5-minute boundary
        // (identity_expires == now_ms + 300_001); should be extracted.
        let now_ms = 1_700_000_000_000_u64;
        let expires_ms = now_ms + 300_001;
        let json = format!(
            r#"{{"advertising_token": "tok_just_valid", "identity_expires": {expires_ms}}}"#
        );
        let jar = jar_with(&[("uid2_cookie", &json)]);
        let configs = [json_config("uid2", &["uid2_cookie"], "advertising_token")];

        // Act
        let signals = extract_fp_signals(&jar, &configs, now_ms);

        // Assert
        assert_eq!(
            signals.len(),
            1,
            "should extract token just above the 5-minute boundary"
        );
        assert_eq!(
            signals[0].uid, "tok_just_valid",
            "should use advertising_token from just-valid entry"
        );
    }

    #[test]
    fn extract_fp_signals_non_uid2_json_without_expiry_extracted() {
        // Arrange — JSON cookie without `identity_expires` is not a UID2 token;
        // expiry check is skipped and the value is extracted normally.
        let now_ms = 1_700_000_000_000_u64;
        let json = r#"{"id": "non_uid2_value"}"#;
        let jar = jar_with(&[("partner_cookie", json)]);
        let configs = [json_config("partner", &["partner_cookie"], "id")];

        // Act
        let signals = extract_fp_signals(&jar, &configs, now_ms);

        // Assert
        assert_eq!(
            signals.len(),
            1,
            "should extract non-UID2 JSON cookie normally"
        );
        assert_eq!(
            signals[0].uid, "non_uid2_value",
            "should use extracted JSON value"
        );
    }

    #[test]
    fn extract_fp_signals_skips_uid_exceeding_max_length() {
        // Arrange — cookie value exceeds MAX_UID_LENGTH (512 bytes)
        let long_uid = "x".repeat(MAX_UID_LENGTH + 1);
        let jar = jar_with(&[("partner_cookie", &long_uid)]);
        let configs = [raw_config("partner", &["partner_cookie"])];

        // Act
        let signals = extract_fp_signals(&jar, &configs, 0);

        // Assert
        assert!(
            signals.is_empty(),
            "should skip signal with UID exceeding MAX_UID_LENGTH"
        );
    }

    #[test]
    fn extract_fp_signals_accepts_uid_at_max_length() {
        // Arrange — cookie value is exactly MAX_UID_LENGTH (512 bytes)
        let uid = "x".repeat(MAX_UID_LENGTH);
        let jar = jar_with(&[("partner_cookie", &uid)]);
        let configs = [raw_config("partner", &["partner_cookie"])];

        // Act
        let signals = extract_fp_signals(&jar, &configs, 0);

        // Assert
        assert_eq!(
            signals.len(),
            1,
            "should accept signal at exactly MAX_UID_LENGTH"
        );
        assert_eq!(signals[0].uid, uid, "should use the full UID value");
    }

    #[test]
    fn extract_fp_signals_raw_config_with_expired_identity_expires_skipped() {
        // Arrange — a partner with json_path: None whose cookie value happens to be
        // valid JSON containing an expired `identity_expires`. The expiry check runs
        // on any parseable JSON with that field, regardless of json_path config.
        let now_ms = 1_700_000_000_000_u64;
        let expires_ms = now_ms + 100_000;
        let json = format!(r#"{{"identity_expires": {expires_ms}, "some_id": "raw_id"}}"#);
        let jar = jar_with(&[("partner_cookie", &json)]);
        let configs = [raw_config("partner", &["partner_cookie"])];

        // Act
        let signals = extract_fp_signals(&jar, &configs, now_ms);

        // Assert
        assert!(
            signals.is_empty(),
            "should skip raw-config partner when cookie JSON has expired identity_expires"
        );
    }
}
