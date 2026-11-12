//! Shared EID resolution and formatting helpers.
//!
//! Used by both `/_ts/api/v1/identify` and `/auction` to view source-domain
//! keyed IDs from KV entries as `OpenRTB` EID structures, and to build
//! base64-encoded response headers.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use error_stack::{Report, ResultExt as _};

use crate::consent::ConsentContext;
use crate::error::TrustedServerError;
use crate::openrtb::{Eid, Uid};

use super::eid_types::{RawJson, UnixSeconds};
use super::kv_types::{KvEid, KvEntry, KvPartnerId};
use super::registry::PartnerRegistry;

/// Maximum size (in bytes) for the base64-encoded `x-ts-eids` header value.
pub const MAX_EIDS_HEADER_BYTES: usize = 4096;

/// A borrowed EID entry: a single legacy UID or a full-fidelity provider entry.
#[derive(Debug, Clone, Copy)]
pub enum EidView<'row> {
    /// A record holding one UID and no EID-level fields.
    Single {
        /// Source domain.
        source: &'row str,
        /// The UID.
        uid: &'row str,
        /// The partner's configured agent type.
        atype: i32,
    },
    /// A provider-written entry; only live UIDs are emitted.
    Full {
        /// Source domain.
        source: &'row str,
        /// The stored entry.
        eid: &'row KvEid,
        /// Request time used to filter UIDs.
        now: UnixSeconds,
    },
}

impl From<EidView<'_>> for Eid {
    fn from(view: EidView<'_>) -> Self {
        match view {
            EidView::Single { source, uid, atype } => Eid::new(
                source,
                vec![Uid {
                    id: uid.to_owned(),
                    atype: Some(atype),
                    ext: None,
                }],
            ),
            EidView::Full { source, eid, now } => {
                let fields = eid.fields();
                let uids = eid
                    .uids()
                    .iter()
                    .filter(|uid| uid.is_live(now))
                    .map(|uid| Uid {
                        id: uid.id().as_str().to_owned(),
                        atype: Some(uid.atype().get()),
                        ext: uid.ext().map(RawJson::to_value),
                    })
                    .collect();
                Eid {
                    inserter: fields.inserter.as_deref().map(str::to_owned),
                    matcher: fields.matcher.as_deref().map(str::to_owned),
                    mm: fields.mm.map(i32::from),
                    ext: fields.ext.as_ref().map(RawJson::to_value),
                    ..Eid::new(source, uids)
                }
            }
        }
    }
}

/// Yields the live views of one stored record.
///
/// Legacy records yield one [`EidView::Single`]; provider records yield one
/// [`EidView::Full`] per entry that still has a live UID.
pub fn source_views<'row>(
    source: &'row str,
    record: &'row KvPartnerId,
    default_atype: i32,
    now: UnixSeconds,
) -> impl Iterator<Item = EidView<'row>> + 'row {
    let single =
        (!record.is_provider_written() && !record.uid.is_empty()).then_some(EidView::Single {
            source,
            uid: record.uid.as_str(),
            atype: default_atype,
        });
    let full = record
        .eids()
        .iter()
        .filter(move |eid| eid.uids().iter().any(|uid| uid.is_live(now)))
        .map(move |eid| EidView::Full { source, eid, now });
    single.into_iter().chain(full)
}

impl KvEntry {
    /// Iterates live entries for registered, `bidstream_enabled` sources in
    /// source order, borrowing from the row.
    ///
    /// A source whose owning provider has a TCF vendor ID is skipped while
    /// `consent` withholds that vendor's consent.
    pub fn live_eids<'row>(
        &'row self,
        registry: &'row PartnerRegistry,
        consent: &'row ConsentContext,
        now: UnixSeconds,
    ) -> impl Iterator<Item = EidView<'row>> + 'row {
        self.ids.iter().flat_map(move |(source, record)| {
            let partner = registry.get(source).filter(|partner| {
                partner.bidstream_enabled && !partner.owner_vendor_consent_denied(consent)
            });
            partner
                .into_iter()
                .flat_map(move |partner| source_views(source, record, partner.openrtb_atype, now))
        })
    }
}

/// Encodes a pre-built EID slice into a base64 header value with truncation.
///
/// Returns `(encoded_value, was_truncated)`. If the full set of EIDs exceeds
/// [`MAX_EIDS_HEADER_BYTES`] after base64 encoding, EIDs are removed from the
/// end until it fits.
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
    use crate::ec::eid_types::{AgentType, EidFields, Expiry, ProviderId, UidValue, UnixSeconds};
    use crate::ec::kv_types::{KvEid, KvPartnerId, KvUid};
    use crate::redacted::Redacted;
    use crate::settings::EcPartner;
    use crate::test_support::tests::consent_granting_vendors;
    use std::num::NonZeroU16;

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

    fn header_eid(source_domain: String, uid: String) -> Eid {
        Eid::new(
            source_domain,
            vec![Uid {
                id: uid,
                atype: Some(3),
                ext: None,
            }],
        )
    }

    #[test]
    fn encode_eids_header_truncates_when_too_large() {
        let eids: Vec<Eid> = (0..64)
            .map(|idx| {
                header_eid(
                    format!("partner-{idx}.example.com"),
                    format!("uid_{}", "x".repeat(100)),
                )
            })
            .collect();

        let (encoded, truncated) =
            encode_eids_header(&eids).expect("should build truncated header");

        assert!(truncated, "should report truncation for large payload");
        assert!(
            encoded.len() <= MAX_EIDS_HEADER_BYTES,
            "should cap encoded header bytes"
        );
    }

    #[test]
    fn encode_eids_header_fits_without_truncation() {
        let eids = vec![header_eid("ssp.example.com".to_owned(), "u1".to_owned())];

        let (encoded, truncated) =
            encode_eids_header(&eids).expect("should build header without truncation");

        assert!(!truncated, "should not truncate small payload");
        assert!(!encoded.is_empty(), "should produce non-empty value");
    }

    fn registry_with(source: &str, bidstream: bool) -> PartnerRegistry {
        let mut partner = make_test_partner(source);
        partner.bidstream_enabled = bidstream;
        PartnerRegistry::from_config(&[partner]).expect("should build registry")
    }

    fn provider_record(ids: &[(&str, Expiry)]) -> KvPartnerId {
        let uids = ids
            .iter()
            .map(|(id, expiry)| {
                KvUid::new(
                    UidValue::try_from(*id).expect("should build uid"),
                    AgentType::try_from(1).expect("should build atype"),
                    None,
                    Some(ProviderId::new("example-provider")),
                    *expiry,
                )
            })
            .collect();
        let fields = EidFields {
            inserter: Some("inserter.example.com".into()),
            ..EidFields::default()
        };
        KvPartnerId::from_eids(vec![KvEid::new(fields, uids)], UnixSeconds::new(0))
            .expect("should build record")
    }

    #[test]
    fn live_eids_skip_expired_uids_and_keep_eid_fields() {
        let registry = registry_with("ids.example.com", true);
        let mut entry = KvEntry::minimal("ids.example.com", "placeholder", 1);
        entry.ids.insert(
            "ids.example.com".to_owned(),
            provider_record(&[
                ("old", Expiry::At(UnixSeconds::new(10))),
                ("new", Expiry::Never),
            ]),
        );

        let eids: Vec<Eid> = entry
            .live_eids(&registry, &ConsentContext::default(), UnixSeconds::new(20))
            .map(Eid::from)
            .collect();

        assert_eq!(eids.len(), 1, "should emit one entry");
        assert_eq!(
            eids[0].inserter.as_deref(),
            Some("inserter.example.com"),
            "should keep inserter"
        );
        let ids: Vec<&str> = eids[0].uids.iter().map(|uid| uid.id.as_str()).collect();
        assert_eq!(ids, vec!["new"], "should drop expired uid");
    }

    #[test]
    fn live_eids_withhold_a_source_whose_owner_lacks_vendor_consent() {
        let mut registry = registry_with("ids.example.com", true);
        assert!(
            registry.mark_provider_owned("ids.example.com", NonZeroU16::new(9999)),
            "should mark the source"
        );
        let mut entry = KvEntry::minimal("ids.example.com", "placeholder", 1);
        entry.ids.insert(
            "ids.example.com".to_owned(),
            provider_record(&[("p-1", Expiry::Never)]),
        );
        let denied = consent_granting_vendors(vec![1]);
        let granted = consent_granting_vendors(vec![9999]);

        assert_eq!(
            entry
                .live_eids(&registry, &denied, UnixSeconds::new(0))
                .count(),
            0,
            "should withhold the provider's UIDs without its vendor consent"
        );
        assert_eq!(
            entry
                .live_eids(&registry, &granted, UnixSeconds::new(0))
                .count(),
            1,
            "should serve the provider's UIDs with its vendor consent"
        );
    }

    #[test]
    fn live_eids_respect_bidstream_flag() {
        let registry = registry_with("ids.example.com", false);
        let mut entry = KvEntry::minimal("ids.example.com", "v", 1);
        entry
            .ids
            .insert("ids.example.com".to_owned(), KvPartnerId::single("v"));
        assert_eq!(
            entry
                .live_eids(&registry, &ConsentContext::default(), UnixSeconds::new(0))
                .count(),
            0,
            "should hide non-bidstream source"
        );
    }

    #[test]
    fn live_eids_emit_legacy_single_uid() {
        let registry = registry_with("ids.example.com", true);
        let mut entry = KvEntry::minimal("ids.example.com", "v", 1);
        entry
            .ids
            .insert("ids.example.com".to_owned(), KvPartnerId::single("v"));
        let eids: Vec<Eid> = entry
            .live_eids(&registry, &ConsentContext::default(), UnixSeconds::new(0))
            .map(Eid::from)
            .collect();
        assert_eq!(eids[0].uids[0].id, "v", "should emit legacy uid");
        assert_eq!(eids[0].uids[0].atype, Some(3), "should use partner atype");
    }
}
