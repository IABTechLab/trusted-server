//! `SharedID` handled the way Prebid's `SharedID` module defines it.
//!
//! The publisher configures `SharedID` as a managed Prebid User ID module
//! (`sharedId`, or its older name `pubCommonId`). Its `storage.name` names the
//! browser cookie the server reads. Prebid sends the ID under source
//! `pubcid.org` with agent type 1, so the server stores and sends it that way
//! too, and re-files a record still holding it under the legacy `sharedid.org`
//! source on the record's next write. A `_pubcid_optout` cookie stops the
//! server reading, storing and sending it.

use std::borrow::Cow;

use http::Request;

use crate::cookies::extract_cookie_value;
use crate::integrations::prebid::{PrebidManagedUserIdConfig, PrebidUserIdStorageType};
use crate::openrtb::Eid;

use super::kv::PartnerIdUpdate;
use super::kv_types::KvEntry;
use super::registry::PartnerRegistry;

/// Source Prebid sends `SharedID` under, and the one the server stores and
/// sends it under.
pub const SHAREDID_SOURCE: &str = "pubcid.org";

/// Source the server used to store `SharedID` under before it followed
/// Prebid. Records holding it are re-filed under [`SHAREDID_SOURCE`].
pub const LEGACY_SHAREDID_SOURCE: &str = "sharedid.org";

/// `OpenRTB` agent type of `SharedID`: a device-based ID.
pub const SHAREDID_ATYPE: i32 = 1;

/// Cookie Prebid's `SharedID` module reads as the reader's opt-out.
pub const SHAREDID_OPTOUT_COOKIE: &str = "_pubcid_optout";

/// Cookie the server read before it followed Prebid's `storage.name`, still
/// read when Prebid configures no `SharedID` storage.
pub const LEGACY_SHAREDID_COOKIE: &str = "sharedId";

/// Prebid `userSync.userIds` names of the `SharedID` module.
const SHAREDID_MODULE_NAMES: [&str; 2] = ["sharedId", "pubCommonId"];

/// Where Prebid's configuration says the `SharedID` lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SharedIdStorage {
    /// Prebid stores it in this cookie, possibly with local storage too.
    Cookie(String),
    /// Prebid stores it only in local storage, which the server cannot read.
    LocalStorageOnly,
    /// Prebid manages no `SharedID` module with storage settings.
    Unconfigured,
}

impl SharedIdStorage {
    /// Resolves the `SharedID` storage from managed Prebid User ID entries.
    ///
    /// Prebid matches entry names case-insensitively, so any case of
    /// `sharedId` or `pubCommonId` selects the module.
    #[must_use]
    pub fn from_managed_user_ids(entries: &[PrebidManagedUserIdConfig]) -> Self {
        let storage = entries
            .iter()
            .filter(|entry| {
                SHAREDID_MODULE_NAMES
                    .iter()
                    .any(|name| entry.name.eq_ignore_ascii_case(name))
            })
            .find_map(|entry| entry.storage.as_ref());
        match storage {
            None => Self::Unconfigured,
            Some(storage) => match storage.storage_type {
                PrebidUserIdStorageType::Cookie | PrebidUserIdStorageType::CookieAndHtml5 => {
                    Self::Cookie(storage.name.clone())
                }
                PrebidUserIdStorageType::Html5 => Self::LocalStorageOnly,
            },
        }
    }

    /// Returns the cookie the server reads the `SharedID` from.
    ///
    /// That is Prebid's configured cookie, or the legacy `sharedId` cookie
    /// when Prebid configures no `SharedID` storage, and `None` when Prebid
    /// keeps it in local storage only.
    #[must_use]
    pub fn cookie_name(&self) -> Option<&str> {
        match self {
            Self::Cookie(name) => Some(name),
            Self::LocalStorageOnly => None,
            Self::Unconfigured => Some(LEGACY_SHAREDID_COOKIE),
        }
    }
}

/// The `SharedID` cookies one request carried.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SharedIdRequest {
    value: Option<String>,
    opted_out: bool,
}

impl SharedIdRequest {
    /// Reads the `SharedID` cookie named `cookie_name` and the opt-out from
    /// the request.
    ///
    /// Prebid URL-encodes cookie values, so the value is decoded.
    #[must_use]
    pub fn from_request<B>(cookie_name: Option<&str>, req: &Request<B>) -> Self {
        let value = cookie_name
            .and_then(|name| extract_cookie_value(req, name))
            .map(|value| decode_cookie_value(&value).into_owned())
            .filter(|value| !value.trim().is_empty());
        let opted_out = extract_cookie_value(req, SHAREDID_OPTOUT_COOKIE).is_some();
        Self { value, opted_out }
    }

    /// Returns the `SharedID` cookie value the server may read, or `None`
    /// when the reader opted out.
    #[must_use]
    pub fn readable_value(&self) -> Option<&str> {
        if self.opted_out {
            return None;
        }
        self.value.as_deref()
    }

    /// Returns whether the request carried the `SharedID` cookie, whether or
    /// not the reader opted out.
    #[must_use]
    pub fn has_value(&self) -> bool {
        self.value.is_some()
    }

    /// Returns whether the request carried `_pubcid_optout`.
    #[must_use]
    pub fn opted_out(&self) -> bool {
        self.opted_out
    }
}

#[cfg(test)]
impl SharedIdRequest {
    /// Builds a request carrying only a `SharedID` cookie.
    pub(crate) fn with_value_for_test(value: &str) -> Self {
        Self {
            value: Some(value.to_owned()),
            opted_out: false,
        }
    }

    /// Builds a request carrying a `SharedID` cookie and `_pubcid_optout`.
    pub(crate) fn opted_out_for_test(value: &str) -> Self {
        Self {
            value: Some(value.to_owned()),
            opted_out: true,
        }
    }
}

/// Returns whether `source` carries `SharedID`.
#[must_use]
pub fn is_sharedid_source(source: &str) -> bool {
    source.eq_ignore_ascii_case(SHAREDID_SOURCE)
        || source.eq_ignore_ascii_case(LEGACY_SHAREDID_SOURCE)
}

/// Returns the source the server sends a stored source under:
/// [`SHAREDID_SOURCE`] for either `SharedID` source, else `source` itself.
#[must_use]
pub fn canonical_source(source: &str) -> &str {
    if is_sharedid_source(source) {
        SHAREDID_SOURCE
    } else {
        source
    }
}

/// Rewrites `SharedID` EIDs to [`SHAREDID_SOURCE`] with agent type 1, so a
/// later merge keeps one EID for the same ID from any source.
pub(crate) fn canonicalize_sharedid_eid(eid: &mut Eid) {
    if !is_sharedid_source(&eid.source) {
        return;
    }
    if eid.source != SHAREDID_SOURCE {
        SHAREDID_SOURCE.clone_into(&mut eid.source);
    }
    for uid in &mut eid.uids {
        uid.atype = Some(SHAREDID_ATYPE);
    }
}

/// Drops `SharedID` EIDs from an outbound set when the reader opted out.
#[must_use]
pub(crate) fn strip_opted_out_sharedid_eids(
    eids: Option<Vec<Eid>>,
    opted_out: bool,
) -> Option<Vec<Eid>> {
    if !opted_out {
        return eids;
    }
    eids.map(|eids| {
        eids.into_iter()
            .filter(|eid| !is_sharedid_source(&eid.source))
            .collect()
    })
}

/// Drops `SharedID` record updates read from the request when the reader
/// opted out.
#[must_use]
pub(crate) fn drop_opted_out_sharedid_updates(
    updates: Vec<PartnerIdUpdate>,
    sharedid: &SharedIdRequest,
) -> Vec<PartnerIdUpdate> {
    if !sharedid.opted_out {
        return updates;
    }
    updates
        .into_iter()
        .filter(|update| !is_sharedid_source(&update.partner_id))
        .collect()
}

/// Moves a `SharedID` the record holds under [`LEGACY_SHAREDID_SOURCE`] to
/// [`SHAREDID_SOURCE`], when a partner is registered for that source.
///
/// A `SharedID` the record already holds under [`SHAREDID_SOURCE`] wins, and
/// the legacy copy is dropped.
pub(crate) fn refile_legacy_sharedid(entry: &mut KvEntry, registry: &PartnerRegistry) {
    if registry.get(SHAREDID_SOURCE).is_none() {
        return;
    }
    let Some(legacy) = entry.ids.remove(LEGACY_SHAREDID_SOURCE) else {
        return;
    };
    entry
        .ids
        .entry(SHAREDID_SOURCE.to_owned())
        .or_insert(legacy);
}

/// Returns the partner source a `SharedID` read from the cookie is filed
/// under: [`SHAREDID_SOURCE`] when a partner is registered for it, else
/// [`LEGACY_SHAREDID_SOURCE`] when only that one is.
pub(crate) fn sharedid_partner_source(registry: &PartnerRegistry) -> Option<&str> {
    [SHAREDID_SOURCE, LEGACY_SHAREDID_SOURCE]
        .into_iter()
        .find_map(|source| registry.get(source))
        .map(|partner| partner.source_domain.as_str())
}

fn decode_cookie_value(value: &str) -> Cow<'_, str> {
    urlencoding::decode(value).unwrap_or(Cow::Borrowed(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::prebid::PrebidManagedUserIdStorage;
    use crate::openrtb::Uid;
    use crate::settings::EcPartner;

    use super::super::kv_types::KvPartnerId;

    fn managed(
        name: &str,
        storage: Option<(PrebidUserIdStorageType, &str)>,
    ) -> PrebidManagedUserIdConfig {
        PrebidManagedUserIdConfig {
            name: name.to_owned(),
            params: serde_json::Map::new(),
            storage: storage.map(|(storage_type, cookie)| PrebidManagedUserIdStorage {
                storage_type,
                name: cookie.to_owned(),
                expires: None,
                refresh_in_seconds: None,
            }),
        }
    }

    fn partner(source: &str) -> EcPartner {
        serde_json::from_value(serde_json::json!({
            "name": source,
            "source_domain": source,
            "bidstream_enabled": true,
        }))
        .expect("should build test partner")
    }

    fn registry(sources: &[&str]) -> PartnerRegistry {
        let partners: Vec<_> = sources.iter().map(|source| partner(source)).collect();
        PartnerRegistry::from_config(&partners).expect("should build test registry")
    }

    fn live_entry() -> KvEntry {
        let mut entry = KvEntry::tombstone(1000);
        entry.consent.ok = true;
        entry
    }

    fn request(cookies: &str) -> Request<()> {
        Request::builder()
            .header(http::header::COOKIE, cookies)
            .body(())
            .expect("should build request")
    }

    #[test]
    fn storage_reads_the_configured_cookie() {
        for storage_type in [
            PrebidUserIdStorageType::Cookie,
            PrebidUserIdStorageType::CookieAndHtml5,
        ] {
            let storage = SharedIdStorage::from_managed_user_ids(&[managed(
                "SHAREDID",
                Some((storage_type, "_pubcid")),
            )]);

            assert_eq!(
                storage.cookie_name(),
                Some("_pubcid"),
                "should read the cookie Prebid writes for {storage_type:?}"
            );
        }
    }

    fn settings_with_sharedid_storage(storage: &serde_json::Value) -> crate::settings::Settings {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings
            .integrations
            .insert_config(
                "prebid",
                &serde_json::json!({
                    "enabled": true,
                    "external_bundle_url": "https://assets.example/prebid/trusted-prebid.js",
                    "bundle": { "modules": { "bidder": ["exampleBidderBidAdapter"] } },
                    "managed_user_ids": [{ "name": "sharedId", "storage": storage }],
                }),
            )
            .expect("should configure Prebid");
        settings
    }

    #[test]
    fn settings_resolve_the_prebid_sharedid_cookie() {
        let settings = settings_with_sharedid_storage(
            &serde_json::json!({ "type": "cookie", "name": "_pubcid" }),
        );

        assert_eq!(
            settings.sharedid_storage().cookie_name(),
            Some("_pubcid"),
            "should read the cookie named by Prebid's storage.name"
        );
    }

    #[test]
    fn settings_without_prebid_keep_the_legacy_cookie() {
        let settings = crate::test_support::tests::create_test_settings();

        assert_eq!(
            settings.sharedid_storage().cookie_name(),
            Some(LEGACY_SHAREDID_COOKIE),
            "should keep reading the legacy cookie without Prebid storage"
        );
    }

    #[test]
    fn prepare_runtime_rejects_the_sharedid_cookie_as_a_template_cache_key() {
        let mut settings = settings_with_sharedid_storage(
            &serde_json::json!({ "type": "cookie", "name": "_pubcid" }),
        );
        let co: crate::creative_opportunities::CreativeOpportunitiesConfig =
            serde_json::from_value(serde_json::json!({
                "gam_network_id": "99999",
                "template_cache_key_cookies": ["_pubcid"],
            }))
            .expect("should build creative opportunities config");
        settings.creative_opportunities = Some(co);

        let error = settings
            .prepare_runtime()
            .expect_err("should reject keying the template cache on the SharedID cookie");

        assert!(
            format!("{error:?}").contains("_pubcid"),
            "should name the SharedID cookie: {error:?}"
        );
    }

    #[test]
    fn storage_reads_the_legacy_module_name() {
        let storage = SharedIdStorage::from_managed_user_ids(&[managed(
            "pubCommonId",
            Some((PrebidUserIdStorageType::Cookie, "_sharedid")),
        )]);

        assert_eq!(
            storage.cookie_name(),
            Some("_sharedid"),
            "should accept Prebid's older module name"
        );
    }

    #[test]
    fn local_storage_only_reads_no_cookie() {
        let storage = SharedIdStorage::from_managed_user_ids(&[managed(
            "sharedId",
            Some((PrebidUserIdStorageType::Html5, "_pubcid")),
        )]);

        assert_eq!(
            storage.cookie_name(),
            None,
            "should read no cookie when Prebid keeps SharedID in local storage"
        );
    }

    #[test]
    fn unconfigured_storage_reads_the_legacy_cookie() {
        for entries in [
            vec![],
            vec![managed("sharedId", None)],
            vec![managed(
                "id5Id",
                Some((PrebidUserIdStorageType::Cookie, "id5")),
            )],
        ] {
            let storage = SharedIdStorage::from_managed_user_ids(&entries);

            assert_eq!(
                storage,
                SharedIdStorage::Unconfigured,
                "should be unconfigured without SharedID storage"
            );
            assert_eq!(
                storage.cookie_name(),
                Some(LEGACY_SHAREDID_COOKIE),
                "should keep reading the legacy cookie"
            );
        }
    }

    #[test]
    fn request_reads_only_the_named_cookie() {
        let req = request("sharedId=legacy; _pubcid=11111111-2222-4333-8444-555555555555");

        let read = SharedIdRequest::from_request(Some("_pubcid"), &req);

        assert_eq!(
            read.readable_value(),
            Some("11111111-2222-4333-8444-555555555555"),
            "should read the configured cookie and ignore `sharedId`"
        );
    }

    #[test]
    fn request_decodes_the_cookie_value() {
        let req = request("_pubcid=a%2Db");

        let read = SharedIdRequest::from_request(Some("_pubcid"), &req);

        assert_eq!(
            read.readable_value(),
            Some("a-b"),
            "should URL-decode the value"
        );
    }

    #[test]
    fn opted_out_request_reads_no_value() {
        let req = request("_pubcid=example-id; _pubcid_optout=1");

        let read = SharedIdRequest::from_request(Some("_pubcid"), &req);

        assert!(read.opted_out(), "should report the opt-out");
        assert_eq!(
            read.readable_value(),
            None,
            "should read no value after an opt-out"
        );
    }

    #[test]
    fn request_without_a_cookie_name_reads_only_the_opt_out() {
        let req = request("sharedId=example-id; _pubcid_optout=1");

        let read = SharedIdRequest::from_request(None, &req);

        assert_eq!(read.readable_value(), None, "should read no value");
        assert!(read.opted_out(), "should still report the opt-out");
    }

    #[test]
    fn canonicalize_moves_legacy_source_to_pubcid_with_atype_one() {
        for source in ["sharedid.org", "PUBCID.ORG"] {
            let mut eid = Eid::new(
                source,
                vec![Uid {
                    id: "example-id".to_owned(),
                    atype: Some(3),
                    ext: None,
                }],
            );

            canonicalize_sharedid_eid(&mut eid);

            assert_eq!(
                eid.source, SHAREDID_SOURCE,
                "should send `{source}` as pubcid.org"
            );
            assert_eq!(
                eid.uids[0].atype,
                Some(SHAREDID_ATYPE),
                "should send atype 1"
            );
        }
    }

    #[test]
    fn canonicalize_leaves_other_sources_alone() {
        let mut eid = Eid::new(
            "id5-sync.com",
            vec![Uid {
                id: "example-id".to_owned(),
                atype: Some(3),
                ext: None,
            }],
        );

        canonicalize_sharedid_eid(&mut eid);

        assert_eq!(eid.source, "id5-sync.com", "should keep the source");
        assert_eq!(eid.uids[0].atype, Some(3), "should keep the atype");
    }

    #[test]
    fn strip_drops_sharedid_sources_only_after_an_opt_out() {
        let eids = vec![
            Eid::new("pubcid.org", Vec::new()),
            Eid::new("sharedid.org", Vec::new()),
            Eid::new("id5-sync.com", Vec::new()),
        ];

        let kept =
            strip_opted_out_sharedid_eids(Some(eids.clone()), false).expect("should keep the EIDs");
        let stripped =
            strip_opted_out_sharedid_eids(Some(eids), true).expect("should keep the other EIDs");

        assert_eq!(kept.len(), 3, "should keep every EID without an opt-out");
        assert_eq!(
            stripped
                .iter()
                .map(|eid| eid.source.as_str())
                .collect::<Vec<_>>(),
            vec!["id5-sync.com"],
            "should drop both SharedID sources after an opt-out"
        );
    }

    #[test]
    fn drop_removes_sharedid_updates_after_an_opt_out() {
        let updates = vec![
            PartnerIdUpdate::new("pubcid.org", "example-id"),
            PartnerIdUpdate::new("id5-sync.com", "other-id"),
        ];

        let kept = drop_opted_out_sharedid_updates(
            updates.clone(),
            &SharedIdRequest::with_value_for_test("example-id"),
        );
        let dropped = drop_opted_out_sharedid_updates(
            updates,
            &SharedIdRequest::opted_out_for_test("example-id"),
        );

        assert_eq!(kept.len(), 2, "should keep every update without an opt-out");
        assert_eq!(
            dropped,
            vec![PartnerIdUpdate::new("id5-sync.com", "other-id")],
            "should drop the SharedID update after an opt-out"
        );
    }

    #[test]
    fn refile_moves_a_legacy_sharedid_to_pubcid() {
        let mut entry = live_entry();
        entry.ids.insert(
            LEGACY_SHAREDID_SOURCE.to_owned(),
            KvPartnerId::single("legacy-id"),
        );

        refile_legacy_sharedid(&mut entry, &registry(&[SHAREDID_SOURCE]));

        assert!(
            !entry.ids.contains_key(LEGACY_SHAREDID_SOURCE),
            "should remove the legacy source"
        );
        assert_eq!(
            entry.ids.get(SHAREDID_SOURCE).map(|id| id.uid.as_str()),
            Some("legacy-id"),
            "should file the ID under pubcid.org"
        );
    }

    #[test]
    fn refile_keeps_the_stored_pubcid_value() {
        let mut entry = live_entry();
        entry.ids.insert(
            LEGACY_SHAREDID_SOURCE.to_owned(),
            KvPartnerId::single("legacy-id"),
        );
        entry
            .ids
            .insert(SHAREDID_SOURCE.to_owned(), KvPartnerId::single("stored-id"));

        refile_legacy_sharedid(&mut entry, &registry(&[SHAREDID_SOURCE]));

        assert_eq!(entry.ids.len(), 1, "should keep one SharedID");
        assert_eq!(
            entry.ids.get(SHAREDID_SOURCE).map(|id| id.uid.as_str()),
            Some("stored-id"),
            "should keep the value already under pubcid.org"
        );
    }

    #[test]
    fn refile_waits_for_a_pubcid_partner() {
        let mut entry = live_entry();
        entry.ids.insert(
            LEGACY_SHAREDID_SOURCE.to_owned(),
            KvPartnerId::single("legacy-id"),
        );

        refile_legacy_sharedid(&mut entry, &registry(&[LEGACY_SHAREDID_SOURCE]));

        assert!(
            entry.ids.contains_key(LEGACY_SHAREDID_SOURCE),
            "should keep the legacy source while no pubcid.org partner is registered"
        );
    }

    #[test]
    fn partner_source_prefers_pubcid() {
        assert_eq!(
            sharedid_partner_source(&registry(&[LEGACY_SHAREDID_SOURCE, SHAREDID_SOURCE])),
            Some(SHAREDID_SOURCE),
            "should file under pubcid.org when both are registered"
        );
        assert_eq!(
            sharedid_partner_source(&registry(&[LEGACY_SHAREDID_SOURCE])),
            Some(LEGACY_SHAREDID_SOURCE),
            "should keep filing under sharedid.org when only it is registered"
        );
        assert_eq!(
            sharedid_partner_source(&registry(&["id5-sync.com"])),
            None,
            "should file nowhere without a SharedID partner"
        );
    }
}
