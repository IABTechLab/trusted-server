//! The JS-readable mirror of provider-owned bidstream UIDs (spec §10).

use base64::Engine as _;
use serde_json::{Map, Value};

use crate::consent::ConsentContext;
use crate::consent::{allows_eid_persistence, tcf_vendor_consent_denied};
use crate::ec::consent::ec_consent_granted;
use crate::ec::eid_types::{Expiry, UnixSeconds};
use crate::ec::kv_types::{KvEid, KvEntry, KvUid};
use crate::ec::registry::PartnerRegistry;

/// Largest encoded mirror value.
pub const MAX_MIRROR_BYTES: usize = 3584;

/// What to do with the mirror cookie on this response.
#[derive(Debug, PartialEq, Eq)]
pub enum MirrorCookie {
    /// Set the cookie to this value.
    Set(MirrorValue),
    /// Expire the cookie.
    Expire,
}

/// An encoded mirror value within [`MAX_MIRROR_BYTES`].
#[derive(Debug, PartialEq, Eq)]
pub struct MirrorValue(String);

impl MirrorValue {
    /// The encoded cookie value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Builds the mirror from the row as it stands after this navigation.
///
/// Holds live, provider-written UIDs of partners with both
/// `bidstream_enabled` and `mirror_enabled` whose owning provider's TCF vendor
/// consent is not denied, in
/// partner config order, as unpadded base64url JSON
/// `{"v":1,"eids":[...]}`. UIDs that would push the value past
/// [`MAX_MIRROR_BYTES`] are skipped, as are UIDs that fail to serialize.
/// Returns [`MirrorCookie::Expire`] when consent does not allow the EC or EID
/// persistence, the row is missing or withdrawn, or nothing qualifies.
///
/// # Performance
///
/// Linear in the row size: each EID header and UID is serialized once, the
/// encoded length is tracked arithmetically, and the value is base64-encoded
/// once at the end.
#[must_use]
pub fn build_mirror(
    row: Option<&KvEntry>,
    consent: &ConsentContext,
    partners: &PartnerRegistry,
    now: UnixSeconds,
) -> MirrorCookie {
    let Some(row) = row.filter(|row| row.consent.ok && mirror_consent(consent)) else {
        return MirrorCookie::Expire;
    };
    let mut mirror = MirrorBuilder::default();
    // A partner opts in with `mirror_enabled`, and a UID never reaches the
    // page while its owning provider's TCF vendor consent is denied, as it
    // never reaches the bidstream (`KvEntry::live_eids`).
    for partner in partners.in_config_order().filter(|partner| {
        partner.bidstream_enabled
            && partner.mirror_enabled
            && !partner
                .owner_tcf_vendor_id
                .is_some_and(|vendor| tcf_vendor_consent_denied(consent, vendor))
    }) {
        let Some(record) = row.ids.get(&partner.source_domain) else {
            continue;
        };
        let source = partner.source_domain.as_str();
        // Only provider-written UIDs carry an owner. Ownerless records come
        // from browser ingest, batch sync or legacy pull sync, so they are
        // never mirrored.
        for eid in record.eids() {
            let uids = eid
                .uids()
                .iter()
                .filter(|uid| uid.owner().is_some() && uid.is_live(now))
                .map(|uid| serde_json::to_string(&uid_json(uid)));
            mirror.push_eid(source, eid_header(source, eid), uids);
        }
    }
    let MirrorBuilder {
        mut json,
        eid_count,
    } = mirror;
    if eid_count == 0 {
        return MirrorCookie::Expire;
    }
    json.push_str(JSON_SUFFIX);
    let value = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json);
    debug_assert!(
        value.len() <= MAX_MIRROR_BYTES,
        "should respect the size cap"
    );
    MirrorCookie::Set(MirrorValue(value))
}

/// Accumulates the mirror JSON, tracking its encoded size so each part is
/// serialized once.
struct MirrorBuilder {
    json: String,
    eid_count: usize,
}

impl Default for MirrorBuilder {
    fn default() -> Self {
        Self {
            json: String::from(JSON_PREFIX),
            eid_count: 0,
        }
    }
}

impl MirrorBuilder {
    /// Appends one EID with every UID that still fits; drops the EID when
    /// none does. Parts that fail to serialize are skipped and logged.
    fn push_eid(
        &mut self,
        source: &str,
        header: Option<String>,
        uids: impl IntoIterator<Item = Result<String, serde_json::Error>>,
    ) {
        let Some(header) = header else {
            log::warn!("Skipping mirror EID for '{source}': header does not serialize");
            return;
        };
        let eid_start = self.json.len();
        let mut kept = 0_usize;
        for uid in uids {
            let Ok(uid) = uid else {
                log::warn!("Skipping mirror UID for '{source}': does not serialize");
                continue;
            };
            let added = if kept == 0 {
                usize::from(self.eid_count > 0) + header.len() + uid.len() + EID_SUFFIX.len()
            } else {
                1 + uid.len() + EID_SUFFIX.len()
            };
            if encoded_len(self.json.len() + added + JSON_SUFFIX.len()) > MAX_MIRROR_BYTES {
                continue;
            }
            if kept == 0 {
                if self.eid_count > 0 {
                    self.json.push(',');
                }
                self.json.push_str(&header);
            } else {
                self.json.push(',');
            }
            self.json.push_str(&uid);
            kept += 1;
        }
        if kept > 0 {
            self.json.push_str(EID_SUFFIX);
            self.eid_count += 1;
        } else {
            self.json.truncate(eid_start);
        }
    }
}

const JSON_PREFIX: &str = r#"{"v":1,"eids":["#;
const JSON_SUFFIX: &str = "]}";
const EID_SUFFIX: &str = "]}";

/// Unpadded base64 length of `bytes` input bytes.
fn encoded_len(bytes: usize) -> usize {
    (bytes * 4).div_ceil(3)
}

/// Serializes an EID without its UIDs, ending in an open `"uids":[`.
///
/// `uids` sorts last among the EID's keys, so the empty array closes the
/// serialized object.
fn eid_header(source: &str, eid: &KvEid) -> Option<String> {
    open_uids_array(serde_json::to_string(&eid_json(source, eid)).ok()?)
}

/// Turns a serialized EID ending in `"uids":[]}` into one ending in an open
/// `"uids":[`.
fn open_uids_array(mut header: String) -> Option<String> {
    let open = header.strip_suffix(EID_SUFFIX)?.len();
    header.ends_with(r#""uids":[]}"#).then(|| {
        header.truncate(open);
        header
    })
}

/// Whether consent allows the mirror: EC consent and EID persistence.
#[must_use]
pub(crate) fn mirror_consent(consent: &ConsentContext) -> bool {
    ec_consent_granted(consent) && allows_eid_persistence(consent)
}

fn eid_json(source: &str, eid: &KvEid) -> Value {
    let fields = eid.fields();
    let mut object = Map::new();
    object.insert("source".to_owned(), Value::from(source));
    if let Some(inserter) = &fields.inserter {
        object.insert("inserter".to_owned(), Value::from(inserter.as_ref()));
    }
    if let Some(matcher) = &fields.matcher {
        object.insert("matcher".to_owned(), Value::from(matcher.as_ref()));
    }
    if let Some(mm) = fields.mm {
        object.insert("mm".to_owned(), Value::from(i32::from(mm)));
    }
    if let Some(ext) = &fields.ext {
        object.insert("ext".to_owned(), ext.to_value());
    }
    object.insert("uids".to_owned(), Value::Array(Vec::new()));
    Value::Object(object)
}

fn uid_json(uid: &KvUid) -> Value {
    let mut object = Map::new();
    object.insert("id".to_owned(), Value::from(uid.id().as_str()));
    object.insert("atype".to_owned(), Value::from(uid.atype().get()));
    if let Some(ext) = uid.ext() {
        object.insert("ext".to_owned(), ext.to_value());
    }
    if let Expiry::At(at) = uid.expiry() {
        object.insert("exp".to_owned(), Value::from(at.get()));
    }
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec::eid_types::{AgentType, EidFields, ProviderId, UidValue};
    use crate::ec::kv_types::KvPartnerId;
    use crate::test_support::tests::consent_granting_vendors;
    use serde_json::json;
    use std::num::NonZeroU16;

    fn partners(bidstream: bool) -> PartnerRegistry {
        partners_with(bidstream, true)
    }

    fn partners_with(bidstream: bool, mirror: bool) -> PartnerRegistry {
        let partner = serde_json::from_value(json!({
            "name": "ids", "source_domain": "ids.example.com",
            "bidstream_enabled": bidstream, "mirror_enabled": mirror
        }))
        .expect("should build partner");
        PartnerRegistry::from_config(&[partner]).expect("should build registry")
    }

    fn uid(id: &str, expiry: Expiry) -> KvUid {
        KvUid::new(
            UidValue::try_from(id).expect("should build"),
            AgentType::try_from(1).expect("should build"),
            None,
            Some(ProviderId::new("p")),
            expiry,
        )
    }

    fn row(uids: Vec<KvUid>) -> KvEntry {
        let mut entry = KvEntry::tombstone(1000);
        entry.consent.ok = true;
        let record = KvPartnerId::from_eids(
            vec![KvEid::new(EidFields::default(), uids)],
            UnixSeconds::new(0),
        )
        .expect("should build record");
        entry.ids.insert("ids.example.com".to_owned(), record);
        entry
    }

    fn granting() -> ConsentContext {
        ConsentContext {
            jurisdiction: crate::consent::jurisdiction::Jurisdiction::NonRegulated,
            ..Default::default()
        }
    }

    fn decode(value: &MirrorValue) -> Value {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(value.as_str())
            .expect("should decode");
        serde_json::from_slice(&bytes).expect("should parse")
    }

    #[test]
    fn mirrors_live_provider_uids_with_exp() {
        let entry = row(vec![
            uid("live", Expiry::At(UnixSeconds::new(2000))),
            uid("old", Expiry::At(UnixSeconds::new(10))),
        ]);

        let MirrorCookie::Set(value) = build_mirror(
            Some(&entry),
            &granting(),
            &partners(true),
            UnixSeconds::new(1000),
        ) else {
            panic!("should set the mirror");
        };

        assert_eq!(
            decode(&value),
            json!({ "v": 1, "eids": [{ "source": "ids.example.com", "uids": [{ "id": "live", "atype": 1, "exp": 2000 }] }] }),
            "should hold only the live UID with its exp"
        );
    }

    #[test]
    fn expires_without_consent_bidstream_or_uids() {
        let entry = row(vec![uid("live", Expiry::Never)]);
        let now = UnixSeconds::new(1000);

        assert!(
            matches!(
                build_mirror(
                    Some(&entry),
                    &ConsentContext::default(),
                    &partners(true),
                    now
                ),
                MirrorCookie::Expire
            ),
            "should expire without consent"
        );
        assert!(
            matches!(
                build_mirror(Some(&entry), &granting(), &partners_with(false, false), now),
                MirrorCookie::Expire
            ),
            "should expire for non-bidstream sources"
        );
        assert!(
            matches!(
                build_mirror(None, &granting(), &partners(true), now),
                MirrorCookie::Expire
            ),
            "should expire without a row"
        );
    }

    #[test]
    fn mirror_is_off_unless_the_partner_opts_in() {
        let entry = row(vec![uid("live", Expiry::Never)]);

        let mirror = build_mirror(
            Some(&entry),
            &granting(),
            &partners_with(true, false),
            UnixSeconds::new(1000),
        );

        assert_eq!(
            mirror,
            MirrorCookie::Expire,
            "should not mirror a bidstream partner without mirror_enabled"
        );
        assert!(
            !serde_json::from_value::<crate::settings::EcPartner>(json!({
                "name": "ids", "source_domain": "ids.example.com"
            }))
            .expect("should build partner")
            .mirror_enabled,
            "should default mirror_enabled to off"
        );
    }

    #[test]
    fn withholds_uids_whose_owner_lacks_vendor_consent() {
        let entry = row(vec![uid("live", Expiry::Never)]);
        let mut registry = partners(true);
        assert!(
            registry.mark_provider_owned("ids.example.com", NonZeroU16::new(9999)),
            "should mark the source"
        );
        let now = UnixSeconds::new(1000);

        let denied = build_mirror(
            Some(&entry),
            &consent_granting_vendors(vec![1]),
            &registry,
            now,
        );
        let granted = build_mirror(
            Some(&entry),
            &consent_granting_vendors(vec![9999]),
            &registry,
            now,
        );

        assert_eq!(
            denied,
            MirrorCookie::Expire,
            "should not mirror a UID whose owner's vendor consent is denied"
        );
        assert!(
            matches!(granted, MirrorCookie::Set(_)),
            "should mirror the UID once the vendor has consent"
        );
    }

    #[test]
    fn does_not_mirror_browser_ingested_uid_of_pull_sync_source() {
        let mut entry = KvEntry::tombstone(1000);
        entry.consent.ok = true;
        entry
            .ids
            .insert("ids.example.com".to_owned(), KvPartnerId::single("browser"));
        let pull_partner = serde_json::from_value(json!({
            "name": "ids", "source_domain": "ids.example.com", "bidstream_enabled": true,
            "mirror_enabled": true, "pull_sync_enabled": true, "pull_sync_url": "https://sync.example.com/pull",
            "pull_sync_allowed_domains": ["sync.example.com"],
            "ts_pull_token": "example-pull-token-32-bytes-minimum"
        }))
        .expect("should build partner");
        let pull = PartnerRegistry::from_config(&[pull_partner]).expect("should build registry");

        let mirror = build_mirror(Some(&entry), &granting(), &pull, UnixSeconds::new(1000));

        assert_eq!(
            mirror,
            MirrorCookie::Expire,
            "should not mirror an ownerless browser-ingested UID"
        );
    }

    #[test]
    fn tombstoned_row_expires_the_mirror_even_with_consent_on_the_request() {
        let mut entry = row(vec![uid("live", Expiry::Never)]);
        entry.consent.ok = false;

        let decision = build_mirror(
            Some(&entry),
            &granting(),
            &partners(true),
            UnixSeconds::new(1000),
        );

        assert!(
            matches!(decision, MirrorCookie::Expire),
            "should expire the mirror for a withdrawn row"
        );
    }

    #[test]
    fn mirrors_several_eids_as_valid_json() {
        let mut entry = row(vec![uid("one", Expiry::Never), uid("two", Expiry::Never)]);
        let record = KvPartnerId::from_eids(
            vec![
                KvEid::new(EidFields::default(), vec![uid("a", Expiry::Never)]),
                KvEid::new(EidFields::default(), vec![uid("b", Expiry::Never)]),
            ],
            UnixSeconds::new(0),
        )
        .expect("should build record");
        entry.ids.insert("ids.example.com".to_owned(), record);

        let MirrorCookie::Set(value) = build_mirror(
            Some(&entry),
            &granting(),
            &partners(true),
            UnixSeconds::new(1000),
        ) else {
            panic!("should set the mirror");
        };

        assert_eq!(
            decode(&value),
            json!({ "v": 1, "eids": [
                { "source": "ids.example.com", "uids": [{ "id": "a", "atype": 1 }] },
                { "source": "ids.example.com", "uids": [{ "id": "b", "atype": 1 }] }
            ] }),
            "should join EIDs into valid JSON"
        );
    }

    #[test]
    fn skips_uids_that_do_not_fit() {
        let big: Vec<KvUid> = (0..20)
            .map(|i| uid(&format!("{i:02}{}", "x".repeat(300)), Expiry::Never))
            .collect();
        let entry = row(big);

        let MirrorCookie::Set(value) = build_mirror(
            Some(&entry),
            &granting(),
            &partners(true),
            UnixSeconds::new(1000),
        ) else {
            panic!("should set the mirror");
        };

        assert!(
            value.as_str().len() <= MAX_MIRROR_BYTES,
            "should fit the size limit"
        );
        let kept = decode(&value)["eids"][0]["uids"]
            .as_array()
            .expect("should hold uids")
            .len();
        assert!(kept > 0 && kept < 20, "should keep a prefix that fits");
    }

    #[test]
    fn several_uids_of_one_eid_respect_the_cap_at_every_boundary() {
        // Three fixed UIDs fill most of the budget; growing the last UID one
        // byte at a time crosses the cap at every possible offset.
        for tail in 1..400 {
            let mut uids: Vec<KvUid> = (0..3)
                .map(|i| uid(&format!("{i}{}", "x".repeat(800)), Expiry::Never))
                .collect();
            uids.push(uid(&"y".repeat(tail), Expiry::Never));
            let entry = row(uids);

            let MirrorCookie::Set(value) = build_mirror(
                Some(&entry),
                &granting(),
                &partners(true),
                UnixSeconds::new(1000),
            ) else {
                panic!("should set the mirror");
            };

            assert!(
                value.as_str().len() <= MAX_MIRROR_BYTES,
                "should fit the size limit with tail {tail}"
            );
            let kept = decode(&value)["eids"][0]["uids"]
                .as_array()
                .expect("should hold uids")
                .len();
            assert!(kept >= 3, "should keep the fixed UIDs with tail {tail}");
        }
    }
}
