//! Shared EID resolution and formatting helpers.
//!
//! Used by `/auction`, the navigation auction and the admin view to resolve
//! source-domain keyed IDs from KV entries, convert them to `OpenRTB` EID
//! structures, and build base64-encoded response headers. Every KV ID passes
//! the shared servability check in [`crate::ec::identity::servable`], so
//! expired IDs and IDs a writer or owner may not disclose are never sent.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use error_stack::{Report, ResultExt as _};

use crate::error::TrustedServerError;
use crate::openrtb::{Eid, Uid};

use super::identity::servable::{ServeContext, is_servable};
use super::identity::{EidProvenance, SourceDomain};
use super::kv_types::{KvEntry, KvPartnerId};
use super::registry::PartnerRegistry;

/// Maximum size (in bytes) for the base64-encoded `x-ts-eids` header value.
pub const MAX_EIDS_HEADER_BYTES: usize = 4096;

/// A source-domain keyed ID resolved from a KV entry against the partner registry.
///
/// Only includes partners with `bidstream_enabled = true` and a non-empty UID.
pub struct ResolvedPartnerId {
    /// The partner's identity source domain and EC KV `ids` key.
    pub source_domain: String,
    /// The synced user ID value.
    pub uid: String,
    /// `OpenRTB` agent type: the stored provider `atype`, else the partner's.
    pub openrtb_atype: i32,
    /// Stored UID-level `ext`, when a provider supplied one.
    pub ext: Option<serde_json::Value>,
    /// EID-level fields the writing provider declared for this source.
    pub provenance: Option<EidProvenance>,
}

/// Resolves source-domain keyed IDs from a KV entry against the partner registry.
///
/// Filters to partners with `bidstream_enabled = true` and non-empty UIDs,
/// sorted deterministically by source domain.
#[must_use]
pub(crate) fn resolve_partner_ids(
    registry: &PartnerRegistry,
    entry: &KvEntry,
    serve: &ServeContext<'_>,
) -> Vec<ResolvedPartnerId> {
    resolve_partner_records(
        registry,
        entry
            .ids
            .iter()
            .filter_map(|(source, record)| Some((source.parse::<SourceDomain>().ok()?, record))),
        serve,
    )
}

/// Resolves any set of records, such as the merged server set of KV and the
/// identity cookies, the way [`resolve_partner_ids`] resolves a KV entry.
pub(crate) fn resolve_partner_records<'r>(
    registry: &PartnerRegistry,
    records: impl IntoIterator<Item = (SourceDomain, &'r KvPartnerId)>,
    serve: &ServeContext<'_>,
) -> Vec<ResolvedPartnerId> {
    let mut resolved = Vec::new();

    for (source, record) in records {
        let Some(partner) = registry.get(source.as_str()) else {
            continue;
        };
        if !partner.bidstream_enabled || !is_servable(serve, &source, record) {
            continue;
        }
        let provenance = record
            .w
            .as_deref()
            .and_then(|writer| writer.parse().ok())
            .and_then(|writer| serve.catalog.get(&writer))
            .and_then(|provider| provider.descriptor().provenance(&source).cloned());

        resolved.push(ResolvedPartnerId {
            source_domain: partner.source_domain.clone(),
            uid: record.uid.clone(),
            openrtb_atype: record.atype.unwrap_or(partner.openrtb_atype),
            ext: record.ext.clone(),
            provenance,
        });
    }

    resolved.sort_by(|a, b| a.source_domain.cmp(&b.source_domain));
    resolved
}

/// Converts resolved partner IDs to `OpenRTB` `Eid` entries.
#[must_use]
pub fn to_eids(resolved: &[ResolvedPartnerId]) -> Vec<Eid> {
    resolved
        .iter()
        .map(|item| {
            let mut eid = Eid::new(
                item.source_domain.clone(),
                vec![Uid {
                    id: item.uid.clone(),
                    atype: Some(item.openrtb_atype),
                    ext: item.ext.clone(),
                }],
            );
            if let Some(provenance) = &item.provenance {
                eid.inserter = provenance.inserter().map(str::to_owned);
                eid.matcher = provenance.matcher().map(str::to_owned);
                eid.mm = provenance.mm();
            }
            eid
        })
        .collect()
}

/// Builds a base64-encoded EID header value, truncating if needed.
///
/// Returns `(encoded_value, was_truncated)`. If the full set of EIDs exceeds
/// [`MAX_EIDS_HEADER_BYTES`] after base64 encoding, partners are removed
/// from the end of the deterministic partner ordering until it fits.
///
/// # Errors
///
/// Returns an error if JSON serialization fails.
pub fn build_eids_header(
    resolved: &[ResolvedPartnerId],
) -> Result<(String, bool), Report<TrustedServerError>> {
    let eids = to_eids(resolved);
    encode_eids_header(&eids)
}

/// Encodes a pre-built EID slice into a base64 header value with truncation.
///
/// Like [`build_eids_header`] but operates on already-constructed `Eid` values
/// (e.g., from `UserInfo.eids` in the auction response path).
///
/// Returns `(encoded_value, was_truncated)`.
///
/// # Errors
///
/// Returns an error if JSON serialization fails.
pub fn encode_eids_header(eids: &[Eid]) -> Result<(String, bool), Report<TrustedServerError>> {
    let try_encode = |size: usize| -> Result<String, Report<TrustedServerError>> {
        let json = serde_json::to_vec(&eids[..size]).change_context(
            TrustedServerError::Configuration {
                message: "Failed to serialize eids header payload".to_owned(),
            },
        )?;
        Ok(BASE64.encode(json))
    };

    // Fast path: try the full slice first (common case — no truncation).
    let encoded = try_encode(eids.len())?;
    if encoded.len() <= MAX_EIDS_HEADER_BYTES {
        return Ok((encoded, false));
    }

    // Binary search for the largest count that fits within the limit.
    // Invariant: lo always fits, hi never fits.
    let mut lo: usize = 0;
    let mut hi: usize = eids.len();

    while lo + 1 < hi {
        let mid = lo + (hi - lo) / 2;
        let encoded = try_encode(mid)?;
        if encoded.len() <= MAX_EIDS_HEADER_BYTES {
            lo = mid;
        } else {
            hi = mid;
        }
    }

    // `lo` is the largest size that fits. Re-encode it for the final value.
    if lo == 0 && !eids.is_empty() {
        log::warn!(
            "encode_eids_header: no EIDs fit within {MAX_EIDS_HEADER_BYTES}B; emitting empty truncated header"
        );
    }
    let encoded = try_encode(lo)?;
    Ok((encoded, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redacted::Redacted;
    use crate::settings::EcPartner;

    fn make_test_partner(source_domain: &str) -> EcPartner {
        EcPartner {
            name: format!("Partner {source_domain}"),
            source_domain: source_domain.to_owned(),
            openrtb_atype: EcPartner::default_openrtb_atype(),
            bidstream_enabled: true,
            api_token: Some(Redacted::new(format!(
                "token-{source_domain}-32-bytes-minimum-value"
            ))),
            batch_rate_limit: EcPartner::default_batch_rate_limit(),
            pull_sync_enabled: false,
            pull_sync_url: None,
            pull_sync_allowed_domains: vec![],
            pull_sync_ttl_sec: EcPartner::default_pull_sync_ttl_sec(),
            pull_sync_rate_limit: EcPartner::default_pull_sync_rate_limit(),
            ts_pull_token: None,
        }
    }

    #[test]
    fn resolve_partner_ids_sorts_by_source_domain() {
        let partners = vec![
            make_test_partner("zeta.example.com"),
            make_test_partner("alpha.example.com"),
        ];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");

        let mut entry = KvEntry::tombstone(1000);
        entry.consent.ok = true;
        entry.ids.insert(
            "zeta.example.com".to_owned(),
            super::super::kv_types::KvPartnerId::new("uid-z".to_owned()),
        );
        entry.ids.insert(
            "alpha.example.com".to_owned(),
            super::super::kv_types::KvPartnerId::new("uid-a".to_owned()),
        );

        let resolved = resolve_partner_ids(&registry, &entry, &serve_context(&serve_parts()));
        let source_domains: Vec<&str> = resolved
            .iter()
            .map(|item| item.source_domain.as_str())
            .collect();

        assert_eq!(
            source_domains,
            vec!["alpha.example.com", "zeta.example.com"],
            "should sort deterministically by source domain"
        );
    }

    struct ServeParts {
        consent: crate::consent::ConsentContext,
        catalog: crate::ec::identity::catalog::ProviderCatalog,
        opted_out: std::collections::BTreeSet<crate::ec::identity::IdentityProviderId>,
    }

    fn serve_parts() -> ServeParts {
        ServeParts {
            consent: crate::ec::identity::test_support::allowing_consent(),
            catalog: crate::ec::identity::catalog::ProviderCatalog::empty(),
            opted_out: std::collections::BTreeSet::new(),
        }
    }

    fn serve_context(parts: &ServeParts) -> ServeContext<'_> {
        ServeContext {
            now: crate::ec::identity::UnixTime::from_secs(100),
            consent: &parts.consent,
            signals: crate::ec::identity::policy::PolicySignals::default(),
            catalog: &parts.catalog,
            opted_out: &parts.opted_out,
        }
    }

    #[test]
    fn resolve_partner_ids_skips_expired_records_and_uses_stored_atype_and_ext() {
        let partners = vec![
            make_test_partner("a.example.com"),
            make_test_partner("b.example.com"),
        ];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        let mut entry = KvEntry::minimal("a.example.com", "ua", 1);
        entry.ids.insert(
            "b.example.com".to_owned(),
            super::super::kv_types::KvPartnerId {
                exp: Some(50),
                ..super::super::kv_types::KvPartnerId::new("ub")
            },
        );
        let stored = entry.ids.get_mut("a.example.com").expect("should hold a");
        stored.atype = Some(3);
        stored.ext = Some(serde_json::json!({ "linkType": 2 }));

        let resolved = resolve_partner_ids(&registry, &entry, &serve_context(&serve_parts()));

        assert_eq!(resolved.len(), 1, "should drop the expired b");
        assert_eq!(resolved[0].source_domain, "a.example.com", "should keep a");
        assert_eq!(
            resolved[0].openrtb_atype, 3,
            "should prefer the stored atype"
        );
        let eids = to_eids(&resolved);
        assert_eq!(
            eids[0].uids[0].ext,
            Some(serde_json::json!({ "linkType": 2 })),
            "should emit the stored UID-level ext"
        );
    }

    #[test]
    fn to_eids_maps_resolved_ids_correctly() {
        let resolved = vec![
            ResolvedPartnerId {
                uid: "LR_xyz".to_owned(),
                source_domain: "liveramp.com".to_owned(),
                openrtb_atype: 3,
                ext: None,
                provenance: None,
            },
            ResolvedPartnerId {
                uid: "ID5_abc".to_owned(),
                source_domain: "id5-sync.com".to_owned(),
                openrtb_atype: 1,
                ext: None,
                provenance: None,
            },
            ResolvedPartnerId {
                uid: "pair-id".to_owned(),
                source_domain: "google.com".to_owned(),
                openrtb_atype: 571187,
                ext: None,
                provenance: None,
            },
        ];

        let eids = to_eids(&resolved);

        assert_eq!(eids.len(), 3, "should produce one EID per resolved partner");
        assert_eq!(eids[0].source, "liveramp.com");
        assert_eq!(eids[0].uids[0].id, "LR_xyz");
        assert_eq!(eids[0].uids[0].atype, Some(3));
        assert_eq!(eids[1].source, "id5-sync.com");
        assert_eq!(eids[1].uids[0].id, "ID5_abc");
        assert_eq!(eids[1].uids[0].atype, Some(1));
        assert_eq!(eids[2].source, "google.com", "should preserve PAIR source");
        assert_eq!(eids[2].uids[0].id, "pair-id", "should preserve PAIR ID");
        assert_eq!(
            eids[2].uids[0].atype,
            Some(571187),
            "should preserve PAIR vendor-specific atype"
        );
    }

    #[test]
    fn build_eids_header_truncates_when_too_large() {
        let mut resolved = Vec::new();
        for idx in 0..64 {
            resolved.push(ResolvedPartnerId {
                uid: format!("uid_{}", "x".repeat(100)),
                source_domain: format!("partner-{idx}.example.com"),
                openrtb_atype: 3,
                ext: None,
                provenance: None,
            });
        }

        let (encoded, truncated) =
            build_eids_header(&resolved).expect("should build truncated header");

        assert!(truncated, "should report truncation for large payload");
        assert!(
            encoded.len() <= MAX_EIDS_HEADER_BYTES,
            "should cap encoded header bytes"
        );
    }

    #[test]
    fn build_eids_header_fits_without_truncation() {
        let resolved = vec![ResolvedPartnerId {
            uid: "u1".to_owned(),
            source_domain: "ssp.com".to_owned(),
            openrtb_atype: 3,
            ext: None,
            provenance: None,
        }];

        let (encoded, truncated) =
            build_eids_header(&resolved).expect("should build header without truncation");

        assert!(!truncated, "should not truncate small payload");
        assert!(!encoded.is_empty(), "should produce non-empty value");
    }
}
