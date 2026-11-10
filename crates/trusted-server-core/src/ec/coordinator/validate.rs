//! Turns a provider's parsed response into stored entries (spec §8.2).

use std::borrow::Cow;

use crate::ec::coordinator::results::DropCounts;
use crate::ec::eid_types::{
    AgentType, EidFields, EidValidationError, Expiry, MAX_EID_FIELD_BYTES, MatchMethod, ProviderId,
    RawJson, SourceDomain, UidValue, UnixSeconds,
};
use crate::ec::kv_types::{KvEid, KvRefresh, KvUid};
use crate::ec::provider::{EidLevelFields, EidShape, ProviderEid, ProviderExpiry, ProviderUid};
use crate::ec::registry::PartnerRegistry;

/// Largest private refresh state a provider may keep with one UID.
const MAX_REFRESH_STATE_BYTES: usize = 2048;

/// Policy one response is validated against.
pub(crate) struct ValidationPolicy<'a> {
    /// Owner recorded on every kept UID.
    pub provider: &'a ProviderId,
    /// Sources the provider claims in config.
    pub claimed: &'a [SourceDomain],
    /// The provider's declared shape limits.
    pub shape: EidShape,
    /// Partner config, for the default `atype`.
    pub partners: &'a PartnerRegistry,
    /// Request time.
    pub now: UnixSeconds,
}

/// Validated entries grouped by source, in response order.
pub(crate) type ValidatedEntries = Vec<(SourceDomain, Vec<KvEid>)>;

/// Validates parsed entries, keeping valid ones grouped by source in
/// response order.
pub(crate) fn validate(
    eids: Vec<ProviderEid<'_>>,
    policy: &ValidationPolicy<'_>,
) -> (ValidatedEntries, DropCounts) {
    let mut dropped = DropCounts::default();
    let mut grouped: ValidatedEntries = Vec::new();
    let mut kept_eids = 0_usize;

    for eid in eids {
        let Some(source) = claimed_source(&eid.source, policy.claimed) else {
            dropped.record(EidValidationError::UnclaimedSource);
            continue;
        };
        if kept_eids == policy.shape.max_eids.get() {
            dropped.over_limit = dropped.over_limit.saturating_add(1);
            continue;
        }
        let fields = match eid_fields(&eid, policy.shape.eid_level_fields) {
            Ok(fields) => fields,
            Err(reason) => {
                dropped.record(reason);
                continue;
            }
        };
        let default_atype = policy
            .partners
            .find_by_source_domain(source.as_str())
            .map_or(1, |partner| partner.openrtb_atype);
        let slot = match grouped.iter().position(|(existing, _)| existing == source) {
            Some(index) => index,
            None => {
                grouped.push((source.clone(), Vec::new()));
                grouped.len() - 1
            }
        };
        let max_uids = policy.shape.max_uids_per_eid.get();
        let mut uids = Vec::with_capacity(eid.uids.len().min(max_uids));
        for uid in eid.uids {
            if uids.len() == max_uids {
                dropped.over_limit = dropped.over_limit.saturating_add(1);
                continue;
            }
            match stored_uid(uid, default_atype, policy) {
                Ok(uid) if is_duplicate(&grouped[slot].1, &uids, &uid) => {
                    dropped.duplicate = dropped.duplicate.saturating_add(1);
                }
                Ok(uid) => uids.push(uid),
                Err(reason) => dropped.record(reason),
            }
        }
        if !uids.is_empty() {
            grouped[slot].1.push(KvEid::new(fields, uids));
            kept_eids += 1;
        }
    }

    grouped.retain(|(_, eids)| !eids.is_empty());
    (grouped, dropped)
}

fn claimed_source<'p>(raw: &str, claimed: &'p [SourceDomain]) -> Option<&'p SourceDomain> {
    let source = SourceDomain::try_from(raw).ok()?;
    claimed.iter().find(|candidate| **candidate == source)
}

fn bounded_field(value: Option<&Cow<'_, str>>) -> Result<Option<Box<str>>, EidValidationError> {
    match value {
        Some(text) if text.len() > MAX_EID_FIELD_BYTES => Err(EidValidationError::FieldTooLong),
        Some(text) => Ok(Some(Box::from(text.as_ref()))),
        None => Ok(None),
    }
}

fn eid_fields(
    eid: &ProviderEid<'_>,
    level: EidLevelFields,
) -> Result<EidFields, EidValidationError> {
    if level == EidLevelFields::Ignored {
        return Ok(EidFields::default());
    }
    Ok(EidFields {
        inserter: bounded_field(eid.inserter.as_ref())?,
        matcher: bounded_field(eid.matcher.as_ref())?,
        mm: eid.mm.map(MatchMethod::try_from).transpose()?,
        ext: eid.ext.map(RawJson::try_from).transpose()?,
    })
}

fn stored_uid(
    uid: ProviderUid<'_>,
    default_atype: i32,
    policy: &ValidationPolicy<'_>,
) -> Result<KvUid, EidValidationError> {
    let id = UidValue::try_from(uid.id.into_owned())?;
    let atype = AgentType::try_from(uid.atype.unwrap_or(default_atype))?;
    let ext = uid.ext.map(RawJson::try_from).transpose()?;
    let expiry = match resolve_expiry(uid.expiry, policy.now) {
        Some(at) => Expiry::At(at),
        None => Expiry::Never,
    };
    if !expiry.is_live(policy.now) {
        return Err(EidValidationError::Expired);
    }
    let stored = KvUid::new(id, atype, ext, Some(policy.provider.clone()), expiry);
    let Some(refresh) = uid.refresh else {
        return Ok(stored);
    };
    // Unusable refresh state is dropped, never the UID: a refresh with no
    // stated time, with empty or oversized state, or due already (which
    // would make the source due on every navigation) is simply not stored.
    let usable = !refresh.state.is_empty() && refresh.state.len() <= MAX_REFRESH_STATE_BYTES;
    match resolve_expiry(refresh.from, policy.now) {
        Some(from) if usable && from > policy.now => Ok(stored.with_refresh(KvRefresh {
            state: refresh.state.into_owned(),
            from,
        })),
        _ => {
            log::debug!(
                "Identity lookup: provider={} drops unusable refresh state",
                policy.provider
            );
            Ok(stored)
        }
    }
}

/// Resolves a provider-stated time against `now`. `None` when no time was
/// stated.
fn resolve_expiry(expiry: ProviderExpiry, now: UnixSeconds) -> Option<UnixSeconds> {
    match expiry {
        ProviderExpiry::Unspecified => None,
        ProviderExpiry::At(at) => Some(at),
        ProviderExpiry::After(after) => Some(now.saturating_add_secs(after.as_secs())),
    }
}

fn is_duplicate(earlier: &[KvEid], current: &[KvUid], uid: &KvUid) -> bool {
    earlier
        .iter()
        .flat_map(KvEid::uids)
        .chain(current)
        .any(|existing| existing.id() == uid.id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec::provider::ProviderRefresh;
    use crate::settings::EcPartner;
    use serde_json::value::RawValue;
    use std::num::NonZeroUsize;
    use std::time::Duration;

    fn registry() -> PartnerRegistry {
        let partner: EcPartner = serde_json::from_value(serde_json::json!({
            "name": "ids", "source_domain": "ids.example.com", "openrtb_atype": 3, "bidstream_enabled": true
        }))
        .expect("should build partner");
        PartnerRegistry::from_config(&[partner]).expect("should build registry")
    }

    fn uid(id: &str) -> ProviderUid<'_> {
        ProviderUid {
            id: Cow::Borrowed(id),
            atype: None,
            ext: None,
            expiry: ProviderExpiry::Unspecified,
            refresh: None,
        }
    }

    fn eid<'a>(source: &'a str, uids: Vec<ProviderUid<'a>>) -> ProviderEid<'a> {
        ProviderEid {
            source: Cow::Borrowed(source),
            inserter: None,
            matcher: None,
            mm: None,
            ext: None,
            uids,
        }
    }

    fn policy<'a>(
        provider: &'a ProviderId,
        claimed: &'a [SourceDomain],
        partners: &'a PartnerRegistry,
    ) -> ValidationPolicy<'a> {
        ValidationPolicy {
            provider,
            claimed,
            shape: EidShape {
                max_eids: NonZeroUsize::new(2).expect("should be non-zero"),
                max_uids_per_eid: NonZeroUsize::new(2).expect("should be non-zero"),
                eid_level_fields: EidLevelFields::Preserved,
            },
            partners,
            now: UnixSeconds::new(1000),
        }
    }

    fn claimed() -> [SourceDomain; 1] {
        [SourceDomain::try_from("ids.example.com").expect("should parse")]
    }

    #[test]
    fn keeps_valid_uids_and_fills_owner_atype_and_expiry() {
        let partners = registry();
        let provider = ProviderId::new("example-provider");
        let claimed = claimed();
        let mut timed = uid("u-2");
        timed.expiry = ProviderExpiry::After(Duration::from_secs(60));

        let (entries, dropped) = validate(
            vec![eid("IDS.example.com", vec![uid("u-1"), timed])],
            &policy(&provider, &claimed, &partners),
        );

        assert_eq!(dropped, DropCounts::default(), "should drop nothing");
        let uids = entries[0].1[0].uids();
        assert_eq!(uids[0].atype().get(), 3, "should fill atype from partner");
        assert_eq!(uids[0].owner(), Some(&provider), "should record owner");
        assert_eq!(
            uids[1].expiry(),
            Expiry::At(UnixSeconds::new(1060)),
            "should resolve relative expiry"
        );
    }

    #[test]
    fn keeps_the_uid_when_its_refresh_state_is_unusable() {
        let partners = registry();
        let provider = ProviderId::new("example-provider");
        let claimed = claimed();
        let in_a_minute = ProviderExpiry::After(Duration::from_secs(60));
        let cases = [
            (
                "state".to_owned(),
                in_a_minute,
                Some(UnixSeconds::new(1060)),
            ),
            (String::new(), in_a_minute, None),
            ("x".repeat(MAX_REFRESH_STATE_BYTES + 1), in_a_minute, None),
            ("state".to_owned(), ProviderExpiry::Unspecified, None),
            (
                "state".to_owned(),
                ProviderExpiry::After(Duration::ZERO),
                None,
            ),
            (
                "state".to_owned(),
                ProviderExpiry::At(UnixSeconds::new(1000)),
                None,
            ),
            (
                "state".to_owned(),
                ProviderExpiry::At(UnixSeconds::new(999)),
                None,
            ),
        ];

        for (state, from, expected) in cases {
            let mut refreshed = uid("u-1");
            refreshed.refresh = Some(ProviderRefresh {
                state: Cow::Owned(state),
                from,
            });

            let (entries, dropped) = validate(
                vec![eid("ids.example.com", vec![refreshed])],
                &policy(&provider, &claimed, &partners),
            );

            assert_eq!(dropped, DropCounts::default(), "should drop no UID");
            assert_eq!(
                entries[0].1[0].uids()[0]
                    .refresh()
                    .map(|refresh| refresh.from),
                expected,
                "should keep only usable refresh state"
            );
        }
    }

    #[test]
    fn drops_invalid_values_by_reason() {
        let partners = registry();
        let provider = ProviderId::new("example-provider");
        let claimed = claimed();
        let long = "x".repeat(crate::ec::kv_types::MAX_STORED_UID_LENGTH + 1);
        let mut expired = uid("u-old");
        expired.expiry = ProviderExpiry::At(UnixSeconds::new(1000));
        let array_ext = RawValue::from_string("[1]".to_owned()).expect("should build raw");
        let mut bad_ext = uid("u-ext");
        bad_ext.ext = Some(&array_ext);

        let (entries, dropped) = validate(
            vec![
                eid(
                    "ids.example.com",
                    vec![uid(" "), uid(&long), expired, bad_ext, uid("ok"), uid("ok")],
                ),
                eid("unclaimed.example.com", vec![uid("u-3")]),
            ],
            &policy(&provider, &claimed, &partners),
        );

        assert_eq!(dropped.invalid_uid, 2, "should drop blank and long UIDs");
        assert_eq!(dropped.expired, 1, "should drop expired UID");
        assert_eq!(
            dropped.invalid_field, 1,
            "should drop UID with non-object ext"
        );
        assert_eq!(dropped.duplicate, 1, "should drop duplicate UID");
        assert_eq!(dropped.unclaimed_source, 1, "should drop unclaimed source");
        assert_eq!(entries.len(), 1, "should keep the claimed source only");
        assert_eq!(entries[0].1[0].uids().len(), 1, "should keep one valid UID");
    }

    #[test]
    fn enforces_shape_limits_from_the_end() {
        let partners = registry();
        let provider = ProviderId::new("example-provider");
        let claimed = claimed();

        let (entries, dropped) = validate(
            vec![
                eid("ids.example.com", vec![uid("a"), uid("b"), uid("c")]),
                eid("ids.example.com", vec![uid("d")]),
                eid("ids.example.com", vec![uid("e")]),
            ],
            &policy(&provider, &claimed, &partners),
        );

        assert_eq!(
            dropped.over_limit, 2,
            "should drop third entry and third UID"
        );
        let ids: Vec<&str> = entries[0]
            .1
            .iter()
            .flat_map(KvEid::uids)
            .map(|uid| uid.id().as_str())
            .collect();
        assert_eq!(
            ids,
            ["a", "b", "d"],
            "should keep the first entries and UIDs"
        );
    }

    #[test]
    fn ignored_eid_level_fields_are_stripped_not_rejected() {
        let partners = registry();
        let provider = ProviderId::new("example-provider");
        let claimed = claimed();
        let mut with_fields = eid("ids.example.com", vec![uid("a")]);
        with_fields.inserter = Some(Cow::Borrowed("inserter.example.com"));
        with_fields.mm = Some(99);
        let mut policy = policy(&provider, &claimed, &partners);
        policy.shape.eid_level_fields = EidLevelFields::Ignored;

        let (entries, dropped) = validate(vec![with_fields], &policy);

        assert_eq!(
            dropped,
            DropCounts::default(),
            "should not count ignored fields"
        );
        assert_eq!(
            entries[0].1[0].fields(),
            &EidFields::default(),
            "should strip EID-level fields"
        );
    }

    #[test]
    fn preserved_eid_level_fields_are_validated() {
        let partners = registry();
        let provider = ProviderId::new("example-provider");
        let claimed = claimed();
        let mut bad_mm = eid("ids.example.com", vec![uid("a")]);
        bad_mm.mm = Some(99);
        let mut kept = eid("ids.example.com", vec![uid("b")]);
        kept.inserter = Some(Cow::Borrowed("inserter.example.com"));
        kept.mm = Some(3);

        let (entries, dropped) =
            validate(vec![bad_mm, kept], &policy(&provider, &claimed, &partners));

        assert_eq!(dropped.invalid_field, 1, "should drop unknown mm entry");
        assert_eq!(
            entries[0].1[0].fields().inserter.as_deref(),
            Some("inserter.example.com"),
            "should keep valid inserter"
        );
        assert_eq!(
            entries[0].1[0].fields().mm,
            Some(MatchMethod::Authenticated),
            "should keep valid mm"
        );
    }
}
