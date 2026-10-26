//! Shared EID resolution and formatting helpers.
//!
//! Used by both `/_ts/api/v1/identify` and `/auction` to resolve source-domain
//! keyed IDs from KV entries, convert them to `OpenRTB` EID structures, and
//! build base64-encoded response headers. KV IDs are filtered by the shared
//! servability check in [`super::identity::servable`], and a stored record
//! goes out as one EID per stored entry.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use error_stack::{Report, ResultExt as _};

use crate::error::TrustedServerError;
use crate::openrtb::{Eid, Uid};

use super::identity::catalog::ProviderCatalog;
use super::identity::servable::{ServeContext, is_servable};
use super::identity::{IdentityProviderId, SourceDomain};
use super::kv_types::{KvEid, KvEntry};
use super::registry::PartnerRegistry;

/// Maximum size (in bytes) for the base64-encoded `x-ts-eids` header value.
pub const MAX_EIDS_HEADER_BYTES: usize = 4096;

/// A source-domain keyed record resolved from a KV entry against the partner
/// registry and the servability check.
///
/// Only includes partners with `bidstream_enabled = true`, a non-empty UID and
/// a servable record.
pub struct ResolvedPartnerId {
    /// The partner's identity source domain and EC KV `ids` key.
    pub source_domain: String,
    /// The first UID of the first stored entry.
    pub uid: String,
    /// `OpenRTB` agent type, used for a UID without a stored one.
    pub openrtb_atype: i32,
    /// The source's stored entry set, in order. One entry for a one-UID record.
    pub entries: Vec<KvEid>,
    /// The provider that wrote the record. `None` for browser ingest and
    /// version 1 rows.
    pub writer: Option<IdentityProviderId>,
    /// Unix seconds at which the record stops being servable, when it has an
    /// `exp`.
    pub expires_at: Option<u64>,
}

/// Resolves source-domain keyed IDs from a KV entry against the partner registry.
///
/// Filters to partners with `bidstream_enabled = true`, non-empty UIDs and
/// servable records, sorted deterministically by source domain.
#[must_use]
pub(crate) fn resolve_partner_ids(
    registry: &PartnerRegistry,
    entry: &KvEntry,
    serve: &ServeContext<'_>,
) -> Vec<ResolvedPartnerId> {
    let mut resolved = Vec::new();

    for (source_domain, partner_uid) in &entry.ids {
        let Some(uid) = partner_uid.first_uid().filter(|uid| !uid.is_empty()) else {
            continue;
        };

        let Some(partner) = registry.get(source_domain) else {
            continue;
        };
        if !partner.bidstream_enabled {
            continue;
        }

        let Ok(source) = source_domain.parse::<SourceDomain>() else {
            continue;
        };
        if !is_servable(serve, &source, partner_uid) {
            continue;
        }

        resolved.push(ResolvedPartnerId {
            source_domain: partner.source_domain.clone(),
            uid: uid.to_owned(),
            openrtb_atype: partner.openrtb_atype,
            entries: partner_uid.eids.clone(),
            writer: partner_uid
                .w
                .as_deref()
                .and_then(|writer| writer.parse().ok()),
            expires_at: partner_uid.exp,
        });
    }

    resolved.sort_by(|a, b| a.source_domain.cmp(&b.source_domain));
    resolved
}

/// Converts resolved partner IDs to `OpenRTB` `Eid` entries: one per stored
/// entry, in stored order.
#[must_use]
pub fn to_eids(resolved: &[ResolvedPartnerId]) -> Vec<Eid> {
    resolved
        .iter()
        .flat_map(|item| {
            item.entries
                .iter()
                .map(move |entry| stored_entry_to_eid(item, entry))
        })
        .collect()
}

fn stored_entry_to_eid(item: &ResolvedPartnerId, entry: &KvEid) -> Eid {
    let uids = entry
        .uids
        .iter()
        .map(|uid| Uid {
            id: uid.id.clone(),
            atype: Some(uid.atype.unwrap_or(item.openrtb_atype)),
            ext: uid.ext.clone(),
        })
        .collect();
    Eid {
        source: item.source_domain.clone(),
        uids,
        inserter: entry.inserter.clone(),
        matcher: entry.matcher.clone(),
        mm: entry.mm,
        ext: entry.ext.clone(),
    }
}

/// One source of a [`ServerSet`]: a servable record and its outbound EIDs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ServerSource {
    /// The partner's source domain, as the registry holds it.
    pub(crate) source_domain: String,
    /// The provider that wrote the record, if any.
    pub(crate) writer: Option<IdentityProviderId>,
    /// Unix seconds at which the record stops being servable.
    pub(crate) expires_at: Option<u64>,
    /// One EID per stored entry, in stored order, provenance defaults filled.
    pub(crate) eids: Vec<Eid>,
}

/// The servable server set of the KV row a request read (spec 7.1).
///
/// Built once per request; the bidstream merge and the `ts-eids-s` mirror
/// both read it. It never holds anything from `ts-eids-s`.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ServerSet {
    sources: Vec<ServerSource>,
}

impl ServerSet {
    /// The set's sources, ordered by source domain.
    #[must_use]
    pub(crate) fn sources(&self) -> &[ServerSource] {
        &self.sources
    }

    /// Every source's EIDs, in order.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn eids(&self) -> Vec<Eid> {
        self.sources
            .iter()
            .flat_map(|source| source.eids.iter().cloned())
            .collect()
    }

    /// A set of one source per EID, for tests that start from EIDs.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn from_eids(eids: Vec<Eid>) -> Self {
        Self {
            sources: eids
                .into_iter()
                .map(|eid| ServerSource {
                    source_domain: eid.source.clone(),
                    writer: None,
                    expires_at: None,
                    eids: vec![eid],
                })
                .collect(),
        }
    }
}

/// The servable server set of `entry` (spec 7.1).
///
/// Without a read row, or for a row that is not live and consenting, the set
/// is empty. Each record goes through [`resolve_partner_ids`] and
/// [`to_eids`], then [`fill_provenance`].
#[must_use]
pub(crate) fn server_set(
    entry: Option<&KvEntry>,
    registry: &PartnerRegistry,
    serve: &ServeContext<'_>,
) -> ServerSet {
    let Some(entry) = entry.filter(|entry| entry.consent.ok) else {
        return ServerSet::default();
    };
    let sources = resolve_partner_ids(registry, entry, serve)
        .into_iter()
        .map(|record| {
            let mut eids = to_eids(core::slice::from_ref(&record));
            if let Ok(source) = record.source_domain.parse::<SourceDomain>() {
                fill_provenance(&mut eids, record.writer.as_ref(), &source, serve.catalog);
            }
            ServerSource {
                source_domain: record.source_domain,
                writer: record.writer,
                expires_at: record.expires_at,
                eids,
            }
        })
        .collect();
    ServerSet { sources }
}

/// Applies the writer's `with_provenance` default for `source` to the EIDs
/// of a record it wrote (spec 4.3, 7.1).
///
/// Only an entry that sets none of `inserter`, `matcher` and `mm` gets the
/// default; an entry that sets any of them is the vendor's complete
/// statement and is left as stored. A record without a configured writer
/// gets nothing.
pub(crate) fn fill_provenance(
    eids: &mut [Eid],
    writer: Option<&IdentityProviderId>,
    source: &SourceDomain,
    catalog: &ProviderCatalog,
) {
    let Some(default) = writer
        .and_then(|writer| catalog.get(writer))
        .and_then(|provider| provider.descriptor().provenance(source))
    else {
        return;
    };
    for eid in eids
        .iter_mut()
        .filter(|eid| eid.inserter.is_none() && eid.matcher.is_none() && eid.mm.is_none())
    {
        eid.inserter = default.inserter().map(str::to_owned);
        eid.matcher = default.matcher().map(str::to_owned);
        eid.mm = default.mm();
    }
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
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::consent::ConsentContext;
    use crate::ec::identity::catalog::ProviderCatalog;
    use crate::ec::identity::policy::PolicySignals;
    use crate::ec::identity::test_support::{
        example_entry_set, full_record, provenance_provider, test_consent_allowing_everything,
    };
    use crate::ec::identity::{EidProvenance, IdentityProviderId, UnixTime};
    use crate::ec::kv_types::{KvEid, KvPartnerId, KvUid};
    use crate::redacted::Redacted;
    use crate::settings::EcPartner;
    use serde_json::json;

    /// Owns what a [`ServeContext`] borrows: permissive consent and no providers.
    struct ServeFixture {
        consent: ConsentContext,
        catalog: ProviderCatalog,
        opted_out: BTreeSet<IdentityProviderId>,
    }

    impl ServeFixture {
        fn new() -> Self {
            Self {
                consent: test_consent_allowing_everything(),
                catalog: ProviderCatalog::empty(),
                opted_out: BTreeSet::new(),
            }
        }

        fn at(&self, now: u64) -> ServeContext<'_> {
            ServeContext {
                now: UnixTime::from_secs(now),
                consent: &self.consent,
                signals: PolicySignals::default(),
                catalog: &self.catalog,
                opted_out: &self.opted_out,
            }
        }
    }

    fn registry_with(sources: &[&str]) -> PartnerRegistry {
        let partners: Vec<EcPartner> = sources
            .iter()
            .map(|source| EcPartner {
                openrtb_atype: 99,
                ..make_test_partner(source)
            })
            .collect();
        PartnerRegistry::from_config(&partners).expect("should build registry")
    }

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
            removed_pull_sync_ttl_sec: None,
            removed_pull_sync_rate_limit: None,
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
        entry
            .ids
            .insert("zeta.example.com".to_owned(), KvPartnerId::new("uid-z"));
        entry
            .ids
            .insert("alpha.example.com".to_owned(), KvPartnerId::new("uid-a"));

        let fixture = ServeFixture::new();
        let resolved = resolve_partner_ids(&registry, &entry, &fixture.at(100));
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

    #[test]
    fn resolve_partner_ids_skips_expired_records_and_uses_stored_atype() {
        let registry = registry_with(&["a.example.com", "b.example.com"]);
        let mut entry = KvEntry::minimal("a.example.com", "ua", 1);
        entry.ids.insert(
            "b.example.com".to_owned(),
            KvPartnerId {
                exp: Some(50),
                ..KvPartnerId::new("ub")
            },
        );
        entry
            .ids
            .get_mut("a.example.com")
            .expect("should hold source a")
            .eids[0]
            .uids[0]
            .atype = Some(3);
        let fixture = ServeFixture::new();

        let resolved = resolve_partner_ids(&registry, &entry, &fixture.at(100));

        assert_eq!(resolved.len(), 1, "should drop the expired b");
        assert_eq!(resolved[0].source_domain, "a.example.com", "should keep a");
        assert_eq!(
            to_eids(&resolved)[0].uids[0].atype,
            Some(3),
            "should prefer the stored atype"
        );
    }

    #[test]
    fn a_one_uid_record_goes_out_as_on_main() {
        let registry = registry_with(&["a.example.com"]);
        let entry = KvEntry::minimal("a.example.com", "ua", 1);
        let fixture = ServeFixture::new();

        let eids = to_eids(&resolve_partner_ids(&registry, &entry, &fixture.at(100)));

        assert_eq!(
            eids,
            vec![Eid::new(
                "a.example.com".to_owned(),
                vec![Uid {
                    id: "ua".to_owned(),
                    atype: Some(99),
                    ext: None
                }],
            )],
            "should be the EID main sends"
        );
    }

    #[test]
    fn a_full_record_goes_out_as_one_eid_per_stored_entry() {
        let registry = registry_with(&["a.example.com"]);
        let mut set = example_entry_set();
        set[1].uids[0].atype = None;
        let mut entry = KvEntry::minimal("b.example.com", "unregistered", 1);
        entry
            .ids
            .insert("a.example.com".to_owned(), full_record(None, set));
        let fixture = ServeFixture::new();

        let eids = to_eids(&resolve_partner_ids(&registry, &entry, &fixture.at(100)));

        assert_eq!(
            eids,
            vec![
                Eid {
                    source: "a.example.com".to_owned(),
                    uids: vec![
                        Uid {
                            id: "example-a".to_owned(),
                            atype: Some(1),
                            ext: None
                        },
                        Uid {
                            id: "example-b".to_owned(),
                            atype: Some(3),
                            ext: Some(json!({ "stype": "ppuid" }))
                        },
                    ],
                    inserter: Some("example.com".to_owned()),
                    matcher: None,
                    mm: Some(3),
                    ext: None,
                },
                Eid {
                    source: "a.example.com".to_owned(),
                    uids: vec![Uid {
                        id: "example-c".to_owned(),
                        atype: Some(99),
                        ext: None
                    }],
                    inserter: None,
                    matcher: Some("partner.example.com".to_owned()),
                    mm: None,
                    ext: None,
                },
            ],
            "should send every entry in stored order, a UID without atype taking the partner's"
        );
    }

    #[test]
    fn to_eids_maps_resolved_ids_correctly() {
        let resolved = vec![
            ResolvedPartnerId {
                uid: "LR_xyz".to_owned(),
                source_domain: "liveramp.com".to_owned(),
                openrtb_atype: 3,
                entries: KvPartnerId::new("LR_xyz".to_owned()).eids,
                writer: None,
                expires_at: None,
            },
            ResolvedPartnerId {
                uid: "ID5_abc".to_owned(),
                source_domain: "id5-sync.com".to_owned(),
                openrtb_atype: 1,
                entries: KvPartnerId::new("ID5_abc".to_owned()).eids,
                writer: None,
                expires_at: None,
            },
            ResolvedPartnerId {
                uid: "pair-id".to_owned(),
                source_domain: "google.com".to_owned(),
                openrtb_atype: 571187,
                entries: KvPartnerId::new("pair-id".to_owned()).eids,
                writer: None,
                expires_at: None,
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
                entries: KvPartnerId::new(format!("uid_{}", "x".repeat(100))).eids,
                writer: None,
                expires_at: None,
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
            entries: KvPartnerId::new("u1".to_owned()).eids,
            writer: None,
            expires_at: None,
        }];

        let (encoded, truncated) =
            build_eids_header(&resolved).expect("should build header without truncation");

        assert!(!truncated, "should not truncate small payload");
        assert!(!encoded.is_empty(), "should produce non-empty value");
    }

    fn provenance_catalog() -> ProviderCatalog {
        ProviderCatalog::new(
            vec![provenance_provider(
                "pa",
                &["a.example.com", "b.example.com"],
                vec![
                    (
                        "a.example.com",
                        EidProvenance::new()
                            .with_inserter("default-inserter.example.com")
                            .with_matcher("default-matcher.example.com")
                            .with_mm(2),
                    ),
                    (
                        "b.example.com",
                        EidProvenance::new().with_inserter("default-inserter.example.com"),
                    ),
                ],
            )],
            BTreeMap::new(),
        )
    }

    #[test]
    fn merge_provenance_applies_only_to_an_entry_with_no_eid_level_fields() {
        let registry = registry_with(&["a.example.com", "b.example.com"]);
        let kv_uid = |id: &str| KvUid {
            id: id.to_owned(),
            atype: None,
            ext: None,
        };
        let mut entry = KvEntry::minimal("unused.example.com", "unused", 1);
        entry.ids.clear();
        entry.ids.insert(
            "a.example.com".to_owned(),
            full_record(
                Some("pa"),
                vec![
                    KvEid {
                        uids: vec![kv_uid("bare")],
                        ..KvEid::default()
                    },
                    KvEid {
                        uids: vec![kv_uid("vendor")],
                        inserter: Some("vendor.example.com".to_owned()),
                        ..KvEid::default()
                    },
                ],
            ),
        );
        entry
            .ids
            .insert("b.example.com".to_owned(), KvPartnerId::new("seeded"));
        let fixture = ServeFixture {
            catalog: provenance_catalog(),
            ..ServeFixture::new()
        };

        let set = server_set(Some(&entry), &registry, &fixture.at(100));

        let uid = |id: &str| Uid {
            id: id.to_owned(),
            atype: Some(99),
            ext: None,
        };
        assert_eq!(
            set.eids(),
            vec![
                Eid {
                    source: "a.example.com".to_owned(),
                    uids: vec![uid("bare")],
                    inserter: Some("default-inserter.example.com".to_owned()),
                    matcher: Some("default-matcher.example.com".to_owned()),
                    mm: Some(2),
                    ext: None,
                },
                Eid {
                    source: "a.example.com".to_owned(),
                    uids: vec![uid("vendor")],
                    inserter: Some("vendor.example.com".to_owned()),
                    matcher: None,
                    mm: None,
                    ext: None,
                },
                Eid {
                    source: "b.example.com".to_owned(),
                    uids: vec![uid("seeded")],
                    inserter: None,
                    matcher: None,
                    mm: None,
                    ext: None,
                },
            ],
            "should fill the default only on the provider-written entry with no EID-level field"
        );
    }

    #[test]
    fn merge_server_set_is_empty_without_a_live_consenting_row() {
        let registry = registry_with(&["a.example.com"]);
        let fixture = ServeFixture::new();
        let mut entry = KvEntry::minimal("a.example.com", "ua", 1);
        entry.consent.ok = false;

        assert_eq!(
            server_set(None, &registry, &fixture.at(100)),
            ServerSet::default(),
            "should be empty without a KV read"
        );
        assert_eq!(
            server_set(Some(&entry), &registry, &fixture.at(100)),
            ServerSet::default(),
            "should be empty for a withdrawn row"
        );
    }
}
