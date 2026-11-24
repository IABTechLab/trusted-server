//! The JS-readable mirror of provider-owned bidstream UIDs (spec §10).

use base64::Engine as _;
use serde_json::{Map, Value};

use crate::consent::ConsentContext;
use crate::consent::allows_eid_persistence;
use crate::ec::EcContext;
use crate::ec::consent::ec_consent_granted;
use crate::ec::eid_types::{Expiry, UnixSeconds};
use crate::ec::kv_types::{KvEid, KvEntry, KvUid};
use crate::ec::registry::PartnerRegistry;

/// Largest encoded mirror value.
pub const MAX_MIRROR_BYTES: usize = 3584;

/// Seconds in a day; the cap on a mirrored UID's `exp` counts from the
/// start of the current UTC day.
const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

/// Longest a mirrored UID stays valid on the page, counted from the start of
/// the current UTC day: 30 days (WB-4).
const MIRROR_MAX_EXPIRY_SECS: u64 = 30 * SECONDS_PER_DAY;

/// How close a carried mirror UID's `exp` may come before the mirror is
/// re-set with the current `exp`: 7 days. Until then an otherwise unchanged
/// mirror is kept, so a returning reader gets no `Set-Cookie` for it.
const MIRROR_REFRESH_WINDOW_SECS: u64 = 7 * SECONDS_PER_DAY;

/// What to do with the mirror cookie on this response.
#[derive(Debug, PartialEq, Eq)]
pub enum MirrorCookie {
    /// Set the cookie to this value.
    Set(MirrorValue),
    /// Expire the cookie.
    Expire,
}

/// An encoded mirror value within [`MAX_MIRROR_BYTES`], with the JSON it
/// encodes.
#[derive(Debug, PartialEq, Eq)]
pub struct MirrorValue {
    json: String,
    encoded: String,
}

impl MirrorValue {
    /// The encoded cookie value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.encoded
    }

    /// The JSON `{"v":1,"eids":[...]}` the cookie value encodes.
    #[must_use]
    pub fn json(&self) -> &str {
        &self.json
    }
}

/// Builds the mirror from the row as it stands after this navigation.
///
/// Holds live, provider-written UIDs of partners with both
/// `bidstream_enabled` and `mirror_enabled` while no TCF vendor gating the
/// source is denied consent, no provider excludes it where GDPR applies for
/// this visitor and no provider's cookies on this request record an opt-out
/// for it, in partner config order, as unpadded base64url JSON
/// `{"v":1,"eids":[...]}`. Every UID carries `exp`: its stated expiry,
/// capped at the start of `now`'s UTC day plus 30 days, or that cap when it
/// states none. Every mirrored UID therefore expires within 30 days (WB-4).
/// A value built from an unchanged row changes at most once per UTC day;
/// [`current_carried_mirror`] keeps the carried cookie across those changes
/// until a UID nears its `exp`. UIDs that would push the value past
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
    ec_context: &EcContext,
    partners: &PartnerRegistry,
    now: UnixSeconds,
) -> MirrorCookie {
    let consent = ec_context.consent();
    let Some(row) = row.filter(|row| row.consent.ok && mirror_consent(consent)) else {
        return MirrorCookie::Expire;
    };
    let max_exp = max_exp(now);
    let mut mirror = MirrorBuilder::default();
    // A partner opts in with `mirror_enabled`, and a UID never reaches the
    // page while a TCF vendor gating its source (that of any configured
    // provider claiming it, enabled or disabled) is denied consent, or while
    // a provider excludes its source where GDPR applies, as it never reaches
    // the bidstream (`KvEntry::live_eids`). Nor while the provider's cookies
    // on this request record an opt-out the row may not reflect yet.
    for partner in partners.in_config_order().filter(|partner| {
        partner.bidstream_enabled
            && partner.mirror_enabled
            && !partner.owner_vendor_consent_denied(consent)
            && !partner.excluded_for(consent)
            && !ec_context.source_opted_out_in_cookies(&partner.source_domain)
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
                .map(|uid| serde_json::to_string(&uid_json(uid, max_exp)));
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
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&json);
    debug_assert!(
        encoded.len() <= MAX_MIRROR_BYTES,
        "should respect the size cap"
    );
    MirrorCookie::Set(MirrorValue { json, encoded })
}

/// Returns the carried mirror when it still serves the reader as `rebuilt`
/// would, so the response need not re-set it.
///
/// The carried value must decode to the same EIDs and UIDs as `rebuilt`, in
/// the same order, apart from `exp`. Each carried UID's `exp` must equal the
/// rebuilt one, or be no later than it (so it stays within the cap and any
/// shortened stated expiry) and at least [`MIRROR_REFRESH_WINDOW_SECS`] after
/// `now`. Every kept UID therefore still expires within 30 days (WB-4), and
/// an unchanged returning reader gets a mirror `Set-Cookie` only when a UID
/// nears its `exp`. Returns `None` when the carried value does not qualify or
/// does not decode as a mirror.
#[must_use]
pub(crate) fn current_carried_mirror(
    carried: &str,
    rebuilt: &MirrorValue,
    now: UnixSeconds,
) -> Option<MirrorValue> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(carried)
        .ok()?;
    let json = String::from_utf8(bytes).ok()?;
    let mut carried_exps = Vec::new();
    let mut rebuilt_exps = Vec::new();
    let carried_uids = without_exps(serde_json::from_str(&json).ok()?, &mut carried_exps)?;
    let rebuilt_uids = without_exps(serde_json::from_str(&rebuilt.json).ok()?, &mut rebuilt_exps)?;
    let refresh_at = now.get().saturating_add(MIRROR_REFRESH_WINDOW_SECS);
    let current = carried_uids == rebuilt_uids
        && carried_exps.len() == rebuilt_exps.len()
        && carried_exps
            .iter()
            .zip(&rebuilt_exps)
            .all(|(carried, rebuilt)| {
                carried == rebuilt || (refresh_at <= *carried && carried <= rebuilt)
            });
    current.then(|| MirrorValue {
        json,
        encoded: carried.to_owned(),
    })
}

/// Removes every UID's `exp` from a mirror JSON value, appending each to
/// `exps` in order. Returns `None` when the value is not a mirror or a UID
/// has no integer `exp`.
fn without_exps(mut value: Value, exps: &mut Vec<u64>) -> Option<Value> {
    for eid in value.get_mut("eids")?.as_array_mut()? {
        for uid in eid.get_mut("uids")?.as_array_mut()? {
            exps.push(uid.as_object_mut()?.remove("exp")?.as_u64()?);
        }
    }
    Some(value)
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

/// The `source` of each EID in an encoded mirror value, or `None` when the
/// value does not decode as a mirror.
pub(crate) fn mirror_sources(value: &str) -> Option<Vec<String>> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .ok()?;
    let mirror: Value = serde_json::from_slice(&bytes).ok()?;
    mirror
        .get("eids")?
        .as_array()?
        .iter()
        .map(|eid| eid.get("source")?.as_str().map(str::to_owned))
        .collect()
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

/// The latest mirror `exp`: the start of `now`'s UTC day plus
/// [`MIRROR_MAX_EXPIRY_SECS`], so at most 30 days ahead and stable within
/// the day.
fn max_exp(now: UnixSeconds) -> UnixSeconds {
    let day_start = now.get() - now.get() % SECONDS_PER_DAY;
    UnixSeconds::new(day_start).saturating_add_secs(MIRROR_MAX_EXPIRY_SECS)
}

fn uid_json(uid: &KvUid, max_exp: UnixSeconds) -> Value {
    let mut object = Map::new();
    object.insert("id".to_owned(), Value::from(uid.id().as_str()));
    object.insert("atype".to_owned(), Value::from(uid.atype().get()));
    if let Some(ext) = uid.ext() {
        object.insert("ext".to_owned(), ext.to_value());
    }
    let exp = match uid.expiry() {
        Expiry::At(at) => at.min(max_exp),
        Expiry::Never => max_exp,
    };
    object.insert("exp".to_owned(), Value::from(exp.get()));
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

    fn context(consent: ConsentContext) -> EcContext {
        EcContext::new_for_test(None, consent)
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
            &context(granting()),
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
        assert_eq!(
            serde_json::from_str::<Value>(value.json()).expect("should parse the JSON"),
            decode(&value),
            "should carry the JSON the cookie value encodes"
        );
    }

    #[test]
    fn caps_a_uid_without_stated_expiry_at_thirty_days_from_the_utc_day_start() {
        // Arrange
        let day_start = 1_767_225_600;
        let now = UnixSeconds::new(day_start + 13 * 60 * 60);
        let entry = row(vec![uid("never", Expiry::Never)]);

        // Act
        let MirrorCookie::Set(value) =
            build_mirror(Some(&entry), &context(granting()), &partners(true), now)
        else {
            panic!("should set the mirror");
        };

        // Assert
        let exp = decode(&value)["eids"][0]["uids"][0]["exp"]
            .as_u64()
            .expect("should carry a numeric exp");
        assert_eq!(
            exp,
            day_start + MIRROR_MAX_EXPIRY_SECS,
            "should expire 30 days after the start of the current UTC day"
        );
        assert!(
            exp > now.get() && exp <= now.get() + MIRROR_MAX_EXPIRY_SECS,
            "should expire within 30 days of now"
        );
    }

    #[test]
    fn caps_a_stated_expiry_at_thirty_days_from_the_utc_day_start() {
        // Arrange: a provider-stated expiry 40 days out, past the 30-day cap,
        // and one 10 days out, within it.
        let day_start = 1_767_225_600;
        let now = UnixSeconds::new(day_start + 13 * 60 * 60);
        let far = now.saturating_add_secs(40 * SECONDS_PER_DAY);
        let near = now.saturating_add_secs(10 * SECONDS_PER_DAY);
        let entry = row(vec![
            uid("far", Expiry::At(far)),
            uid("near", Expiry::At(near)),
        ]);

        // Act
        let MirrorCookie::Set(value) =
            build_mirror(Some(&entry), &context(granting()), &partners(true), now)
        else {
            panic!("should set the mirror");
        };

        // Assert
        let mirror = decode(&value);
        let uids = &mirror["eids"][0]["uids"];
        assert_eq!(
            uids[0]["exp"].as_u64(),
            Some(day_start + MIRROR_MAX_EXPIRY_SECS),
            "should cap a stated expiry past 30 days at the start of the UTC day plus 30 days"
        );
        assert_eq!(
            uids[1]["exp"].as_u64(),
            Some(near.get()),
            "should keep a stated expiry within the cap"
        );
    }

    #[test]
    fn added_exp_drops_a_uid_that_fit_the_budget_without_it() {
        // Arrange: four equal UIDs whose mirror JSON fits the budget without
        // `exp`, but not with the 17 bytes `,"exp":<10 digits>` adds to each.
        let ids: Vec<String> = (0..4).map(|i| format!("{i}{}", "x".repeat(629))).collect();
        let without_exp = json!({ "v": 1, "eids": [{
            "source": "ids.example.com",
            "uids": ids.iter().map(|id| json!({ "id": id, "atype": 1 })).collect::<Vec<_>>()
        }] })
        .to_string();
        assert!(
            encoded_len(without_exp.len()) <= MAX_MIRROR_BYTES,
            "should fit all four UIDs without exp"
        );
        let entry = row(ids.iter().map(|id| uid(id, Expiry::Never)).collect());

        // Act
        let MirrorCookie::Set(value) = build_mirror(
            Some(&entry),
            &context(granting()),
            &partners(true),
            UnixSeconds::new(1_767_225_600),
        ) else {
            panic!("should set the mirror");
        };

        // Assert
        assert!(
            value.as_str().len() <= MAX_MIRROR_BYTES,
            "should fit the size limit"
        );
        let kept: Vec<_> = decode(&value)["eids"][0]["uids"]
            .as_array()
            .expect("should hold uids")
            .iter()
            .map(|uid| uid["id"].as_str().expect("should carry an id").to_owned())
            .collect();
        assert_eq!(
            kept,
            ids[..3],
            "should drop the UID that no longer fits once each UID carries exp"
        );
    }

    #[test]
    fn uid_without_stated_expiry_keeps_its_exp_within_a_utc_day_and_moves_on_the_next() {
        // Arrange
        let day_start = 1_767_225_600;
        let entry = row(vec![uid("never", Expiry::Never)]);
        let build = |now: u64| {
            build_mirror(
                Some(&entry),
                &context(granting()),
                &partners(true),
                UnixSeconds::new(now),
            )
        };

        // Act
        let morning = build(day_start + 60);
        let evening = build(day_start + SECONDS_PER_DAY - 1);
        let next_day = build(day_start + SECONDS_PER_DAY);

        // Assert
        assert_eq!(
            morning, evening,
            "should build the same mirror throughout one UTC day"
        );
        assert_ne!(
            evening, next_day,
            "should move the exp forward on the next UTC day"
        );
    }

    fn built_at(entry: &KvEntry, now: u64) -> MirrorValue {
        match build_mirror(
            Some(entry),
            &context(granting()),
            &partners(true),
            UnixSeconds::new(now),
        ) {
            MirrorCookie::Set(value) => value,
            MirrorCookie::Expire => panic!("should build a mirror"),
        }
    }

    #[test]
    fn keeps_a_carried_mirror_whose_earlier_exp_is_not_yet_near() {
        // Arrange
        let day_start = 1_767_225_600;
        let entry = row(vec![uid("never", Expiry::Never)]);
        let carried = built_at(&entry, day_start + 60);
        let now = day_start + 22 * SECONDS_PER_DAY + 60;
        let rebuilt = built_at(&entry, now);
        assert_ne!(carried, rebuilt, "should move the rebuilt exp forward");

        // Act
        let kept = current_carried_mirror(carried.as_str(), &rebuilt, UnixSeconds::new(now));

        // Assert
        assert_eq!(
            kept.as_ref(),
            Some(&carried),
            "should keep the carried value and its JSON"
        );
    }

    #[test]
    fn refreshes_a_carried_mirror_whose_exp_is_within_seven_days() {
        let day_start = 1_767_225_600;
        let entry = row(vec![uid("never", Expiry::Never)]);
        let carried = built_at(&entry, day_start + 60);
        let now = day_start + 23 * SECONDS_PER_DAY + 61;
        let rebuilt = built_at(&entry, now);

        assert_eq!(
            current_carried_mirror(carried.as_str(), &rebuilt, UnixSeconds::new(now)),
            None,
            "should re-set a mirror whose exp is within 7 days"
        );
    }

    #[test]
    fn refreshes_a_carried_mirror_whose_uids_or_stated_expiry_changed() {
        let day_start = 1_767_225_600;
        let now = day_start + SECONDS_PER_DAY;
        let carried = built_at(&row(vec![uid("never", Expiry::Never)]), day_start + 60);
        let cases = [
            (row(vec![uid("other", Expiry::Never)]), "a different UID"),
            (
                row(vec![
                    uid("never", Expiry::Never),
                    uid("added", Expiry::Never),
                ]),
                "an added UID",
            ),
            (
                row(vec![uid(
                    "never",
                    Expiry::At(UnixSeconds::new(day_start + 20 * SECONDS_PER_DAY)),
                )]),
                "a stated expiry earlier than the carried exp",
            ),
        ];

        for (entry, case) in cases {
            let rebuilt = built_at(&entry, now);
            assert_eq!(
                current_carried_mirror(carried.as_str(), &rebuilt, UnixSeconds::new(now)),
                None,
                "should re-set the mirror for {case}"
            );
        }
    }

    #[test]
    fn refreshes_a_carried_value_that_is_not_a_mirror() {
        let day_start = 1_767_225_600;
        let rebuilt = built_at(&row(vec![uid("never", Expiry::Never)]), day_start);
        let without_exp = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            r#"{"v":1,"eids":[{"source":"ids.example.com","uids":[{"id":"never","atype":1}]}]}"#,
        );

        for carried in ["not base64!", "e30", without_exp.as_str()] {
            assert_eq!(
                current_carried_mirror(carried, &rebuilt, UnixSeconds::new(day_start)),
                None,
                "should re-set a carried value that does not decode as a mirror: {carried}"
            );
        }
    }

    #[test]
    fn expires_without_consent_bidstream_or_uids() {
        let entry = row(vec![uid("live", Expiry::Never)]);
        let now = UnixSeconds::new(1000);

        assert!(
            matches!(
                build_mirror(
                    Some(&entry),
                    &context(ConsentContext::default()),
                    &partners(true),
                    now
                ),
                MirrorCookie::Expire
            ),
            "should expire without consent"
        );
        assert!(
            matches!(
                build_mirror(
                    Some(&entry),
                    &context(granting()),
                    &partners_with(false, false),
                    now
                ),
                MirrorCookie::Expire
            ),
            "should expire for non-bidstream sources"
        );
        assert!(
            matches!(
                build_mirror(None, &context(granting()), &partners(true), now),
                MirrorCookie::Expire
            ),
            "should expire without a row"
        );
    }

    #[test]
    fn withholds_uids_of_a_source_opted_out_in_provider_cookies() {
        // Arrange: the row still holds the UID; the provider's cookies on this
        // request record an opt-out that a later write clears it for.
        let entry = row(vec![uid("live", Expiry::Never)]);
        let mut opted_out = context(granting());
        opted_out.set_cookie_opted_out_sources(vec!["IDS.example.com".to_owned()]);
        let mut other_opted_out = context(granting());
        other_opted_out.set_cookie_opted_out_sources(vec!["other.example.com".to_owned()]);

        // Act
        let withheld = build_mirror(
            Some(&entry),
            &opted_out,
            &partners(true),
            UnixSeconds::new(1000),
        );
        let kept = build_mirror(
            Some(&entry),
            &other_opted_out,
            &partners(true),
            UnixSeconds::new(1000),
        );

        // Assert
        assert_eq!(
            withheld,
            MirrorCookie::Expire,
            "should not mirror a source its provider's cookies opt out of"
        );
        assert!(
            matches!(kept, MirrorCookie::Set(_)),
            "should still mirror when another source is opted out"
        );
    }

    #[test]
    fn mirror_is_off_unless_the_partner_opts_in() {
        let entry = row(vec![uid("live", Expiry::Never)]);

        let mirror = build_mirror(
            Some(&entry),
            &context(granting()),
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
            &context(consent_granting_vendors(vec![1])),
            &registry,
            now,
        );
        let granted = build_mirror(
            Some(&entry),
            &context(consent_granting_vendors(vec![9999])),
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
    fn withholds_uids_of_a_source_excluded_where_gdpr_applies() {
        let entry = row(vec![uid("live", Expiry::Never)]);
        let mut registry = partners(true);
        assert!(
            registry.exclude_where_gdpr_applies("ids.example.com"),
            "should exclude the source"
        );
        let now = UnixSeconds::new(1000);

        let gdpr = build_mirror(
            Some(&entry),
            &context(consent_granting_vendors(Vec::new())),
            &registry,
            now,
        );
        let elsewhere = build_mirror(Some(&entry), &context(granting()), &registry, now);

        assert_eq!(
            gdpr,
            MirrorCookie::Expire,
            "should not mirror a source excluded where GDPR applies on a GDPR visit"
        );
        assert!(
            matches!(elsewhere, MirrorCookie::Set(_)),
            "should mirror the source outside GDPR"
        );
    }

    #[test]
    fn withholds_uids_while_a_disabled_providers_vendor_lacks_consent() {
        // Arrange: enabled provider `new` (vendor 7) owns the source that
        // disabled provider `old` (vendor 42) claimed, so both vendors gate it.
        let entry = row(vec![uid("live", Expiry::Never)]);
        let mut ec = crate::test_support::tests::create_test_settings().ec;
        ec.partners = vec![
            serde_json::from_value(json!({
                "name": "ids", "source_domain": "ids.example.com",
                "bidstream_enabled": true, "mirror_enabled": true
            }))
            .expect("should build partner"),
        ];
        ec.identity_providers = vec![
            toml::from_str(
                "id = \"new\"\nkind = \"k\"\ntcf_vendor_id = 7\nendpoint = \"https://ids.example.com/\"\nsources = [\"ids.example.com\"]",
            )
            .expect("should parse enabled provider"),
            toml::from_str(
                "id = \"old\"\nkind = \"k\"\nenabled = false\ntcf_vendor_id = 42\nendpoint = \"https://ids.example.com/\"\nsources = [\"ids.example.com\"]",
            )
            .expect("should parse disabled provider"),
        ];
        let registry = PartnerRegistry::from_ec_with_kinds(&ec, |kind| kind == "k")
            .expect("should build registry");
        let now = UnixSeconds::new(1000);

        // Act
        let owner_only = build_mirror(
            Some(&entry),
            &context(consent_granting_vendors(vec![7])),
            &registry,
            now,
        );
        let both = build_mirror(
            Some(&entry),
            &context(consent_granting_vendors(vec![7, 42])),
            &registry,
            now,
        );

        // Assert
        assert_eq!(
            owner_only,
            MirrorCookie::Expire,
            "should withhold the source while the disabled provider's vendor lacks consent"
        );
        assert!(
            matches!(both, MirrorCookie::Set(_)),
            "should mirror the source once every gating vendor has consent"
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

        let mirror = build_mirror(
            Some(&entry),
            &context(granting()),
            &pull,
            UnixSeconds::new(1000),
        );

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
            &context(granting()),
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
            &context(granting()),
            &partners(true),
            UnixSeconds::new(1000),
        ) else {
            panic!("should set the mirror");
        };

        assert_eq!(
            decode(&value),
            json!({ "v": 1, "eids": [
                { "source": "ids.example.com", "uids": [{ "id": "a", "atype": 1, "exp": 2_592_000 }] },
                { "source": "ids.example.com", "uids": [{ "id": "b", "atype": 1, "exp": 2_592_000 }] }
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
            &context(granting()),
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
                &context(granting()),
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
