//! The JS-readable mirror of provider-owned bidstream UIDs (spec §10).

use base64::Engine as _;
use serde_json::{Map, Value, json};

use crate::consent::ConsentContext;
use crate::consent::allows_eid_persistence;
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
/// Holds live, provider-written UIDs of `bidstream_enabled` partners, in
/// partner config order, as unpadded base64url JSON
/// `{"v":1,"eids":[...]}`. UIDs that would push the value past
/// [`MAX_MIRROR_BYTES`] are skipped. Returns [`MirrorCookie::Expire`] when
/// consent does not allow the EC or EID persistence, the row is missing or withdrawn,
/// or nothing qualifies.
///
/// # Performance
///
/// Re-encodes once per kept UID; the size cap bounds this to a few dozen
/// small encodes per navigation.
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
    let mut eids: Vec<Value> = Vec::new();
    let mut encoded: Option<String> = None;
    for partner in partners
        .in_config_order()
        .filter(|partner| partner.bidstream_enabled)
    {
        let Some(record) = row.ids.get(&partner.source_domain) else {
            continue;
        };
        for eid in record.eids() {
            let entry_index = eids.len();
            eids.push(eid_json(&partner.source_domain, eid));
            let mut kept = 0_usize;
            for uid in eid
                .uids()
                .iter()
                .filter(|uid| uid.owner().is_some() && uid.is_live(now))
            {
                uids_mut(&mut eids[entry_index]).push(uid_json(uid));
                if let Some(value) = encode(&eids) {
                    encoded = Some(value);
                    kept += 1;
                } else {
                    uids_mut(&mut eids[entry_index]).pop();
                }
            }
            if kept == 0 {
                eids.pop();
            }
        }
    }
    match encoded {
        Some(value) if !eids.is_empty() => MirrorCookie::Set(MirrorValue(value)),
        _ => MirrorCookie::Expire,
    }
}

/// Whether consent allows the mirror: EC consent and EID persistence.
#[must_use]
pub(crate) fn mirror_consent(consent: &ConsentContext) -> bool {
    ec_consent_granted(consent) && allows_eid_persistence(consent)
}

fn uids_mut(eid: &mut Value) -> &mut Vec<Value> {
    eid["uids"]
        .as_array_mut()
        .expect("should hold a uids array")
}

fn encode(eids: &[Value]) -> Option<String> {
    let json = serde_json::to_vec(&json!({ "v": 1, "eids": eids })).ok()?;
    let value = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json);
    (value.len() <= MAX_MIRROR_BYTES).then_some(value)
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

    fn partners(bidstream: bool) -> PartnerRegistry {
        let partner = serde_json::from_value(json!({
            "name": "ids", "source_domain": "ids.example.com", "bidstream_enabled": bidstream
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
                build_mirror(Some(&entry), &granting(), &partners(false), now),
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
}
