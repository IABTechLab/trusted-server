//! `SharedID` handled the way Prebid's `SharedID` module defines it.
//!
//! The publisher configures `SharedID` as a managed Prebid User ID module
//! (`sharedId`, or its older name `pubCommonId`). Its `storage.name` names the
//! browser cookie the server reads. Prebid sends the ID under source
//! `pubcid.org` with agent type 1, so the server stores and sends it that way
//! too, and re-files a record still holding it under the legacy `sharedid.org`
//! source on the record's next write. A `_pubcid_optout` cookie stops the
//! server reading, storing and sending it.
//!
//! When Prebid keeps the `SharedID` in a cookie, a document navigation with EC
//! and EID consent writes the cookie back from the EC record if the request
//! has none or a different value, and keeps Prebid's `<name>_cst`
//! consent-hash companion consistent so Prebid does not rewrite the cookie
//! from script. When neither the record nor the request has a `SharedID`, the
//! server creates one, a UUID v4 as Prebid writes, in the navigation's one
//! page-load write: itself, or through the `sharedid` identity provider when
//! one owns the source. A `_pubcid_optout` cookie or a denied EC consent
//! expires both cookies.

use std::borrow::Cow;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use edgezero_core::body::Body as EdgeBody;
use http::{HeaderValue, Request, Response, header};
use uuid::Uuid;

use crate::consent::{ConsentContext, allows_eid_persistence};
use crate::cookies::extract_cookie_value;
use crate::integrations::prebid::{PrebidManagedUserIdConfig, PrebidUserIdStorageType};
use crate::openrtb::Eid;
use crate::settings::Settings;

use super::eid_types::UnixSeconds;
use super::eids::source_views;
use super::kv::{KvIdentityGraph, PartnerIdUpdate};
use super::kv_types::KvEntry;
use super::registry::PartnerRegistry;
use super::{EcContext, EcKvSnapshot, current_timestamp, log_id};

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

/// Suffix of the cookie Prebid stores its consent hash in.
const CONSENT_HASH_COOKIE_SUFFIX: &str = "_cst";

/// Lifetime of the written `SharedID` cookies: one year in seconds.
const SHAREDID_COOKIE_MAX_AGE: u32 = 365 * 24 * 60 * 60;

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

    /// Returns the cookie the server may write the `SharedID` back to: Prebid's
    /// configured cookie, and `None` unless Prebid stores it in a cookie, since
    /// Prebid would not read a server-written value from anywhere else.
    #[must_use]
    pub fn write_back_cookie(&self) -> Option<&str> {
        match self {
            Self::Cookie(name) => Some(name),
            Self::LocalStorageOnly | Self::Unconfigured => None,
        }
    }
}

/// The `SharedID` cookies one request carried.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SharedIdRequest {
    value: Option<String>,
    opted_out: bool,
    /// The cookie the response may write back to, with the request's value
    /// of its consent-hash companion.
    write_back: Option<WriteBackCookies>,
}

/// The `SharedID` cookie the server writes back, and the request's
/// `<name>_cst` value.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WriteBackCookies {
    name: String,
    consent_hash: Option<String>,
}

impl WriteBackCookies {
    fn consent_hash_cookie_name(&self) -> String {
        format!("{}{CONSENT_HASH_COOKIE_SUFFIX}", self.name)
    }
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
        Self {
            value,
            opted_out,
            write_back: None,
        }
    }

    /// Reads the `SharedID` cookies a navigation may write back: the cookie
    /// `storage` names, its `<name>_cst` companion and the opt-out.
    ///
    /// Without a cookie to write back to, this reads what
    /// [`Self::from_request`] reads.
    #[must_use]
    pub fn for_write_back<B>(storage: &SharedIdStorage, req: &Request<B>) -> Self {
        let mut request = Self::from_request(storage.cookie_name(), req);
        request.write_back = storage.write_back_cookie().map(|name| {
            let mut cookies = WriteBackCookies {
                name: name.to_owned(),
                consent_hash: None,
            };
            cookies.consent_hash = extract_cookie_value(req, &cookies.consent_hash_cookie_name())
                .map(|value| decode_cookie_value(&value).into_owned());
            cookies
        });
        request
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
            ..Self::default()
        }
    }

    /// Builds a request carrying a `SharedID` cookie and `_pubcid_optout`.
    pub(crate) fn opted_out_for_test(value: &str) -> Self {
        Self {
            value: Some(value.to_owned()),
            opted_out: true,
            write_back: None,
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

/// Returns a newly created `SharedID` for the record, when Prebid stores it in
/// a cookie the server writes back, the request carries none and no opt-out,
/// and a partner that no identity provider owns is registered for it.
///
/// The update only fills an empty slot in the navigation's page-load write,
/// so a `SharedID` the record already holds wins. A `sharedid` provider that
/// owns the source creates one itself instead.
pub(crate) fn created_sharedid_update(
    sharedid: &SharedIdRequest,
    registry: &PartnerRegistry,
) -> Option<PartnerIdUpdate> {
    if sharedid.write_back.is_none() || sharedid.value.is_some() || sharedid.opted_out {
        return None;
    }
    let source = sharedid_partner_source(registry)?;
    if registry.is_provider_owned(source) {
        return None;
    }
    Some(PartnerIdUpdate::new(source, Uuid::new_v4().to_string()))
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

/// What the response does with the `SharedID` cookies.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SharedIdWriteBack {
    /// Leave both cookies alone.
    Unchanged,
    /// Set the cookie to the stored value and the consent hash with it.
    SetBoth(String),
    /// Set only the consent-hash companion.
    SetConsentHash,
    /// Expire both cookies.
    ExpireBoth,
}

/// Inputs that decide the `SharedID` write-back for one response.
struct SharedIdDecision<'a> {
    request: &'a SharedIdRequest,
    stored: Option<&'a str>,
    ec_allowed: bool,
    eid_allowed: bool,
    consent_hash: &'a str,
}

/// Decides the `SharedID` write-back from the request, record and consent.
///
/// A `SharedID` is created in the page-load write, not here.
fn decide_write_back(input: &SharedIdDecision<'_>) -> SharedIdWriteBack {
    let request = input.request;
    let Some(cookies) = &request.write_back else {
        return SharedIdWriteBack::Unchanged;
    };
    if request.opted_out || !input.ec_allowed {
        if request.value.is_some() || cookies.consent_hash.is_some() {
            return SharedIdWriteBack::ExpireBoth;
        }
        return SharedIdWriteBack::Unchanged;
    }
    if !input.eid_allowed {
        return SharedIdWriteBack::Unchanged;
    }
    match (input.stored, request.value.as_deref()) {
        (Some(stored), browser) if browser != Some(stored) => {
            SharedIdWriteBack::SetBoth(stored.to_owned())
        }
        // Creation in the page-load write failed or was not allowed.
        (None, None) => SharedIdWriteBack::Unchanged,
        _ if cookies.consent_hash.as_deref() == Some(input.consent_hash) => {
            SharedIdWriteBack::Unchanged
        }
        _ => SharedIdWriteBack::SetConsentHash,
    }
}

/// Expires managed `SharedID` cookies on any finalized response after denial
/// or opt-out, when the request carried them (S2S design §10, WB-1).
pub(crate) fn expire_sharedid_on_denied_consent(
    settings: &Settings,
    sharedid: &SharedIdRequest,
    response: &mut Response<EdgeBody>,
) {
    let decision = decide_write_back(&SharedIdDecision {
        request: sharedid,
        stored: None,
        ec_allowed: false,
        eid_allowed: false,
        consent_hash: "",
    });
    if decision == SharedIdWriteBack::ExpireBoth {
        expire_sharedid_cookies(settings, sharedid, response);
    }
}

/// Writes the `SharedID` cookies back from the EC record on a consented
/// navigation, after the navigation's page-load write.
///
/// Reads the record once when the request has not read it yet, and never
/// writes it.
pub(crate) async fn apply_sharedid_write_back(
    settings: &Settings,
    ec_context: &mut EcContext,
    graph: &KvIdentityGraph,
    registry: &PartnerRegistry,
    sharedid: &SharedIdRequest,
    response: &mut Response<EdgeBody>,
) {
    if sharedid.write_back.is_none() {
        return;
    }
    let Some(ec_id) = ec_context.ec_value().map(str::to_owned) else {
        return;
    };
    let Some(source) = sharedid_partner_source(registry).map(str::to_owned) else {
        log::debug!("SharedID: skipping write-back, no SharedID partner is configured");
        return;
    };
    let consent = ec_context.consent();
    let eid_allowed = allows_eid_persistence(consent);
    let consent_hash = prebid_consent_hash(consent);

    if eid_allowed
        && !sharedid.opted_out
        && matches!(ec_context.kv_snapshot(), EcKvSnapshot::NotRead)
    {
        let snapshot = graph.load_snapshot(&ec_id).await;
        ec_context.set_kv_snapshot(snapshot);
    }
    let entry = ec_context
        .kv_snapshot()
        .entry_for(&ec_id)
        .filter(|entry| entry.consent.ok);
    let stored = entry.and_then(|entry| stored_sharedid(entry, &source));
    let decision = decide_write_back(&SharedIdDecision {
        request: sharedid,
        stored: stored.as_deref(),
        ec_allowed: true,
        eid_allowed,
        consent_hash: &consent_hash,
    });
    if decision != SharedIdWriteBack::ExpireBoth && entry.is_none() {
        log::debug!(
            "SharedID: skipping write-back for '{}' without a live record",
            log_id(&ec_id)
        );
        return;
    }

    match decision {
        SharedIdWriteBack::Unchanged => {}
        SharedIdWriteBack::ExpireBoth => expire_sharedid_cookies(settings, sharedid, response),
        SharedIdWriteBack::SetConsentHash => {
            set_consent_hash_cookie(settings, sharedid, &consent_hash, response);
        }
        SharedIdWriteBack::SetBoth(value) => {
            set_sharedid_cookies(settings, sharedid, &value, &consent_hash, response);
        }
    }
}

/// Returns the live `SharedID` the record holds under `source`, whether a
/// browser cookie or the `sharedid` provider wrote it.
fn stored_sharedid(entry: &KvEntry, source: &str) -> Option<String> {
    let record = entry.ids.get(source)?;
    let now = UnixSeconds::new(current_timestamp());
    source_views(source, record, SHAREDID_ATYPE, now)
        .flat_map(|view| Eid::from(view).uids)
        .map(|uid| uid.id)
        .next()
}

fn sharedid_set_cookie(settings: &Settings, name: &str, value: &str, max_age: u32) -> String {
    format!(
        "{name}={}; Domain={}; Path=/; Secure; SameSite=Lax; Max-Age={max_age}",
        urlencoding::encode(value),
        settings.publisher.ec_cookie_domain(),
    )
}

fn append_set_cookie(response: &mut Response<EdgeBody>, cookie: &str) {
    match HeaderValue::from_str(cookie) {
        Ok(value) => {
            response.headers_mut().append(header::SET_COOKIE, value);
        }
        Err(err) => log::warn!("SharedID: skipping Set-Cookie with invalid header value: {err}"),
    }
}

fn set_sharedid_cookies(
    settings: &Settings,
    sharedid: &SharedIdRequest,
    value: &str,
    consent_hash: &str,
    response: &mut Response<EdgeBody>,
) {
    let Some(cookies) = &sharedid.write_back else {
        return;
    };
    let cookie = sharedid_set_cookie(settings, &cookies.name, value, SHAREDID_COOKIE_MAX_AGE);
    append_set_cookie(response, &cookie);
    set_consent_hash_cookie(settings, sharedid, consent_hash, response);
}

fn set_consent_hash_cookie(
    settings: &Settings,
    sharedid: &SharedIdRequest,
    consent_hash: &str,
    response: &mut Response<EdgeBody>,
) {
    let Some(cookies) = &sharedid.write_back else {
        return;
    };
    let cookie = sharedid_set_cookie(
        settings,
        &cookies.consent_hash_cookie_name(),
        consent_hash,
        SHAREDID_COOKIE_MAX_AGE,
    );
    append_set_cookie(response, &cookie);
}

fn expire_sharedid_cookies(
    settings: &Settings,
    sharedid: &SharedIdRequest,
    response: &mut Response<EdgeBody>,
) {
    let Some(cookies) = &sharedid.write_back else {
        return;
    };
    append_set_cookie(
        response,
        &sharedid_set_cookie(settings, &cookies.name, "", 0),
    );
    append_set_cookie(
        response,
        &sharedid_set_cookie(settings, &cookies.consent_hash_cookie_name(), "", 0),
    );
}

/// Computes the consent hash Prebid stores in `<name>_cst`.
///
/// Mirrors Prebid's `getConsentHash()`: each consent handler (TCF, US
/// Privacy, GPP, COPPA, in that order) hashes its data with cyrb53, the
/// joined handler hashes are hashed again, and the low 32 bits are written
/// little-endian in base64. The handler data comes from the request's consent
/// cookies, assuming Prebid's TCF, GPP and US Privacy modules read the same
/// strings from the CMP.
fn prebid_consent_hash(consent: &ConsentContext) -> String {
    let gdpr = consent.raw_tc_string.as_ref().map_or_else(
        || serde_json::Value::Null,
        |tc| serde_json::json!([consent.gdpr_applies, tc]),
    );
    let usp = consent
        .raw_us_privacy
        .as_ref()
        .map_or_else(|| serde_json::Value::Null, |usp| serde_json::json!(usp));
    let gpp = consent.raw_gpp_string.as_ref().map_or_else(
        || serde_json::Value::Null,
        |gpp| {
            let sections = consent.gpp_section_ids.clone().unwrap_or_default();
            serde_json::json!([sections, gpp])
        },
    );
    let handlers = [
        cyrb53(&gdpr.to_string()).to_string(),
        cyrb53(&usp.to_string()).to_string(),
        cyrb53(&gpp.to_string()).to_string(),
        "0".to_owned(),
    ];
    encode_consent_hash(cyrb53(&handlers.join(":")))
}

fn encode_consent_hash(hash: u64) -> String {
    let mut bytes = Vec::with_capacity(4);
    let mut remaining = hash;
    while remaining > 0 {
        bytes.push((remaining & 0xff) as u8);
        remaining = (remaining & 0xffff_ffff) >> 8;
    }
    BASE64.encode(bytes)
}

/// Prebid's cyrb53 string hash over UTF-16 code units.
fn cyrb53(input: &str) -> u64 {
    let mut h1: u32 = 0xdead_beef;
    let mut h2: u32 = 0x41c6_ce57;
    for unit in input.encode_utf16() {
        let unit = u32::from(unit);
        h1 = (h1 ^ unit).wrapping_mul(2_654_435_761);
        h2 = (h2 ^ unit).wrapping_mul(1_597_334_677);
    }
    h1 = (h1 ^ (h1 >> 16)).wrapping_mul(2_246_822_507)
        ^ (h2 ^ (h2 >> 13)).wrapping_mul(3_266_489_909);
    h2 = (h2 ^ (h2 >> 16)).wrapping_mul(2_246_822_507)
        ^ (h1 ^ (h1 >> 13)).wrapping_mul(3_266_489_909);
    (u64::from(h2 & 0x001f_ffff) << 32) | u64::from(h1)
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

    const EXAMPLE_TC_STRING: &str = "CPXxRfAPXxRfAAfKABENB-CgAAAAAAAAAAYgAAAAAAAA";
    const STORED_ID: &str = "11111111-2222-4333-8444-555555555555";
    const OTHER_ID: &str = "66666666-7777-4888-9999-aaaaaaaaaaaa";
    const CONSENT_HASH: &str = "znv0HA==";

    fn write_back_request(cookies: &str) -> SharedIdRequest {
        SharedIdRequest::for_write_back(
            &SharedIdStorage::Cookie("_pubcid".to_owned()),
            &request(cookies),
        )
    }

    fn decide(
        request: &SharedIdRequest,
        stored: Option<&str>,
        ec_allowed: bool,
        eid_allowed: bool,
    ) -> SharedIdWriteBack {
        decide_write_back(&SharedIdDecision {
            request,
            stored,
            ec_allowed,
            eid_allowed,
            consent_hash: CONSENT_HASH,
        })
    }

    #[test]
    fn cyrb53_matches_prebid_vectors() {
        assert_eq!(
            cyrb53(""),
            3_338_908_027_751_811,
            "should hash the empty string"
        );
        assert_eq!(cyrb53("abc"), 5_059_922_895_146_125, "should hash `abc`");
    }

    #[test]
    fn consent_hash_matches_prebid_without_consent_data() {
        assert_eq!(
            prebid_consent_hash(&ConsentContext::default()),
            CONSENT_HASH,
            "should match Prebid's hash with no consent data"
        );
    }

    #[test]
    fn consent_hash_matches_prebid_for_each_consent_string() {
        let cases = [
            (
                ConsentContext {
                    raw_tc_string: Some(EXAMPLE_TC_STRING.to_owned()),
                    gdpr_applies: true,
                    ..ConsentContext::default()
                },
                "FpjVEA==",
                "TCF",
            ),
            (
                ConsentContext {
                    raw_us_privacy: Some("1YNN".to_owned()),
                    ..ConsentContext::default()
                },
                "32Bazg==",
                "US Privacy",
            ),
            (
                ConsentContext {
                    raw_gpp_string: Some(format!("DBABMA~{EXAMPLE_TC_STRING}")),
                    gpp_section_ids: Some(vec![7]),
                    ..ConsentContext::default()
                },
                "gX2afg==",
                "GPP",
            ),
        ];

        for (consent, expected, label) in cases {
            assert_eq!(
                prebid_consent_hash(&consent),
                expected,
                "should match Prebid's hash for {label}"
            );
        }
    }

    #[test]
    fn write_back_reads_the_cookie_and_its_decoded_companion() {
        let read = write_back_request(&format!("_pubcid={STORED_ID}; _pubcid_cst=znv0HA%3D%3D"));

        assert_eq!(
            read.readable_value(),
            Some(STORED_ID),
            "should read the cookie"
        );
        assert_eq!(
            read.write_back
                .as_ref()
                .and_then(|cookies| cookies.consent_hash.as_deref()),
            Some(CONSENT_HASH),
            "should URL-decode the companion"
        );
    }

    #[test]
    fn only_cookie_storage_writes_back() {
        for storage in [
            SharedIdStorage::LocalStorageOnly,
            SharedIdStorage::Unconfigured,
        ] {
            let read = SharedIdRequest::for_write_back(&storage, &request("other=1"));

            assert_eq!(
                decide(&read, Some(STORED_ID), true, true),
                SharedIdWriteBack::Unchanged,
                "should not write back with {storage:?}"
            );
        }
    }

    #[test]
    fn record_present_and_cookie_absent_sets_both_from_the_record() {
        assert_eq!(
            decide(&write_back_request("other=1"), Some(STORED_ID), true, true),
            SharedIdWriteBack::SetBoth(STORED_ID.to_owned()),
            "should set the stored value"
        );
    }

    #[test]
    fn nothing_stored_sets_nothing() {
        assert_eq!(
            decide(&write_back_request("other=1"), None, true, true),
            SharedIdWriteBack::Unchanged,
            "should leave creation to the sharedid provider"
        );
    }

    #[test]
    fn equal_cookie_with_consistent_companion_is_unchanged() {
        let request =
            write_back_request(&format!("_pubcid={STORED_ID}; _pubcid_cst={CONSENT_HASH}"));

        assert_eq!(
            decide(&request, Some(STORED_ID), true, true),
            SharedIdWriteBack::Unchanged,
            "should set nothing"
        );
    }

    #[test]
    fn equal_cookie_with_missing_or_different_companion_sets_only_the_companion() {
        for cookies in [
            format!("_pubcid={STORED_ID}"),
            format!("_pubcid={STORED_ID}; _pubcid_cst=AAAAAA%3D%3D"),
        ] {
            assert_eq!(
                decide(&write_back_request(&cookies), Some(STORED_ID), true, true),
                SharedIdWriteBack::SetConsentHash,
                "should re-set only the companion for `{cookies}`"
            );
        }
    }

    #[test]
    fn denied_ec_consent_or_opt_out_expires_both() {
        let present = format!("_pubcid={STORED_ID}; _pubcid_cst={CONSENT_HASH}");

        assert_eq!(
            decide(&write_back_request(&present), Some(STORED_ID), false, false),
            SharedIdWriteBack::ExpireBoth,
            "Purpose 1 denied"
        );
        assert_eq!(
            decide(
                &write_back_request(&format!("{present}; _pubcid_optout=1")),
                Some(STORED_ID),
                true,
                true
            ),
            SharedIdWriteBack::ExpireBoth,
            "opted out"
        );
    }

    #[test]
    fn denied_ec_consent_without_cookies_sets_nothing() {
        assert_eq!(
            decide(&write_back_request("other=1"), None, false, false),
            SharedIdWriteBack::Unchanged,
            "nothing to expire"
        );
    }

    #[test]
    fn denied_eid_consent_leaves_the_browser_cookie_alone() {
        for cookies in ["other=1".to_owned(), format!("_pubcid={OTHER_ID}")] {
            assert_eq!(
                decide(&write_back_request(&cookies), Some(STORED_ID), true, false),
                SharedIdWriteBack::Unchanged,
                "Purpose 4 denied should set nothing for `{cookies}`"
            );
        }
    }

    #[test]
    fn stored_value_wins_over_a_different_cookie() {
        let request =
            write_back_request(&format!("_pubcid={OTHER_ID}; _pubcid_cst={CONSENT_HASH}"));

        assert_eq!(
            decide(&request, Some(STORED_ID), true, true),
            SharedIdWriteBack::SetBoth(STORED_ID.to_owned()),
            "should restore the stored value"
        );
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
