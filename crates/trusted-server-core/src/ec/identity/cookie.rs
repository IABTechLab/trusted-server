//! The server-owned identity cookies: sealing, reading and formatting.
//!
//! `__Host-ts-eids-s` and every `__Host-ts-eids-r-*` cookie is sealed with
//! AES-256-GCM as `v1.<nonce>.<ciphertext and tag>`, base64url without
//! padding, with the full EC ID as associated data. A cookie copied from
//! another visitor, or left over after an EC rotation, fails to open and is
//! ignored. Values are never logged.

use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

use aes_gcm::aead::{Aead as _, KeyInit as _, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use http::{HeaderMap, header};
use serde::{Deserialize, Serialize};

use super::catalog::ProviderCatalog;
use super::ids::{IdentityProviderId, SourceDomain};
use super::input::RandomSource;
use super::mutation::{PartnerIdChange, ProviderStateChange};
use super::revision::{IdentityKeys, Revision, record_revision};
use crate::constants::{COOKIE_TS_EIDS_R_PREFIX, COOKIE_TS_EIDS_S as COOKIE_S};
use crate::ec::EcKvSnapshot;
use crate::ec::kv_types::{KvEntry, KvOptOut, KvPartnerId};

/// Lifetime of both identity cookie kinds, in seconds.
pub(crate) const COOKIE_MAX_AGE_SECS: u64 = 31_536_000;

const SEAL_VERSION: &str = "v1";
const NONCE_BYTES: usize = 12;

/// Seals `payload` with AES-256-GCM, the EC ID as associated data.
#[must_use]
pub(crate) fn seal(
    key: &[u8; 32],
    ec_id: &str,
    payload: &[u8],
    random: &dyn RandomSource,
) -> String {
    let cipher = Aes256Gcm::new_from_slice(key).expect("should accept a 32-byte key");
    let mut nonce = [0u8; NONCE_BYTES];
    random.fill(&mut nonce);
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: payload,
                aad: ec_id.as_bytes(),
            },
        )
        .expect("should encrypt a bounded in-memory payload");
    format!(
        "{SEAL_VERSION}.{}.{}",
        URL_SAFE_NO_PAD.encode(nonce),
        URL_SAFE_NO_PAD.encode(ciphertext)
    )
}

/// Opens a sealed value. `None` for any failure.
#[must_use]
pub(crate) fn open(key: &[u8; 32], ec_id: &str, value: &str) -> Option<Vec<u8>> {
    let mut parts = value.split('.');
    if parts.next()? != SEAL_VERSION {
        return None;
    }
    let nonce = URL_SAFE_NO_PAD.decode(parts.next()?).ok()?;
    let ciphertext = URL_SAFE_NO_PAD.decode(parts.next()?).ok()?;
    if parts.next().is_some() || nonce.len() != NONCE_BYTES {
        return None;
    }
    Aes256Gcm::new_from_slice(key)
        .ok()?
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad: ec_id.as_bytes(),
            },
        )
        .ok()
}

/// One cookie read by exact name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CookieRead {
    /// Not present.
    Absent,
    /// Two or more cookies share the name; treated as none.
    Duplicate,
    /// Present once.
    Value(String),
}

fn cookie_pairs(headers: &HeaderMap) -> impl Iterator<Item = (&str, &str)> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .map(|(name, value)| (name.trim(), value.trim()))
}

/// Reads a cookie that must occur at most once.
#[must_use]
pub(crate) fn read_single_cookie(headers: &HeaderMap, name: &str) -> CookieRead {
    let mut found = cookie_pairs(headers)
        .filter(|(cookie, _)| *cookie == name)
        .map(|(_, value)| value);
    match (found.next(), found.next()) {
        (None, _) => CookieRead::Absent,
        (Some(_), Some(_)) => CookieRead::Duplicate,
        (Some(value), None) => CookieRead::Value(value.to_owned()),
    }
}

/// Every cookie whose name starts with `prefix`, as `(name, value)`.
#[must_use]
pub(crate) fn read_prefixed_cookies(headers: &HeaderMap, prefix: &str) -> Vec<(String, String)> {
    cookie_pairs(headers)
        .filter(|(name, _)| name.starts_with(prefix))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

/// A `Set-Cookie` value with the identity cookie attributes. Host-only: no
/// `Domain`.
#[must_use]
pub(crate) fn set_cookie_header(name: &str, value: &str) -> String {
    format!("{name}={value}; Path=/; Secure; SameSite=Lax; HttpOnly; Max-Age={COOKIE_MAX_AGE_SECS}")
}

/// A `Set-Cookie` value that deletes an identity cookie.
#[must_use]
pub(crate) fn delete_cookie_header(name: &str) -> String {
    format!("{name}=; Path=/; Secure; SameSite=Lax; HttpOnly; Max-Age=0")
}

/// Maximum sealed bytes of `__Host-ts-eids-s`.
pub(crate) const MAX_S_COOKIE_BYTES: usize = 2048;

/// Serializes a flag as `1`, and reads `0` or `1`.
mod int_bool {
    use serde::{Deserialize as _, Deserializer, Serializer};

    #[allow(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde's `with` passes the field by reference"
    )]
    pub(super) fn serialize<S: Serializer>(value: &bool, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(u8::from(*value))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<bool, D::Error> {
        Ok(u8::deserialize(deserializer)? != 0)
    }
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes the field by reference"
)]
fn is_false(value: &bool) -> bool {
    !*value
}

/// The `__Host-ts-eids-s` payload.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct SCookie {
    /// Payload version, always 1.
    pub(crate) v: u8,
    /// When a request last confirmed this cookie against the KV row.
    pub(crate) kc: u64,
    /// Fingerprint of the identity config (providers, sources, owners).
    pub(crate) cf: String,
    /// A request saw this EC's row missing; the next writer reads KV.
    #[serde(default, skip_serializing_if = "is_false", with = "int_bool")]
    pub(crate) m: bool,
    /// Cached and staged records.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) ids: Vec<SEntry>,
    /// Mirrored and staged provider state, and pending markers.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) ps: BTreeMap<String, SProviderState>,
}

impl fmt::Debug for SCookie {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SCookie")
            .field("kc", &self.kc)
            .field("cf", &self.cf)
            .field("m", &self.m)
            .field("ids", &self.ids)
            .field("ps", &self.ps.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl SCookie {
    /// An empty cookie for the current config, confirmed at `now`.
    #[must_use]
    pub(crate) fn new(catalog: &ProviderCatalog, now: u64) -> Self {
        Self {
            v: 1,
            kc: now,
            cf: catalog.fingerprint().to_owned(),
            ..Self::default()
        }
    }

    /// Only the pending markers, for a config change or the size cap's last
    /// resort. `kc = 0` forces a KV read at the next navigation.
    #[must_use]
    fn markers_only(&self, cf: &str) -> Self {
        Self {
            v: 1,
            kc: 0,
            cf: cf.to_owned(),
            m: self.m,
            ids: Vec::new(),
            ps: self
                .ps
                .iter()
                .filter_map(|(provider, state)| {
                    state.p.map(|marker| {
                        (
                            provider.clone(),
                            SProviderState {
                                p: Some(marker),
                                ..SProviderState::default()
                            },
                        )
                    })
                })
                .collect(),
        }
    }
}

/// One record in `__Host-ts-eids-s`.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct SEntry {
    /// The source, as an index into the catalog's configured sources.
    pub(crate) s: usize,
    /// The UID. Absent on a stub.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) u: Option<String>,
    /// Writer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) w: Option<String>,
    /// KV `at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) t: Option<u64>,
    /// KV `exp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) e: Option<u64>,
    /// KV `ra`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) r: Option<u64>,
    /// KV `atype`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) a: Option<i32>,
    /// KV `ext`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) x: Option<serde_json::Value>,
    /// KV `rv` of a confirmed entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) rv: Option<String>,
    /// The revision of the record a staged entry replaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pv: Option<String>,
    /// When the entry was staged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) sa: Option<u64>,
    /// A browser KV-gap entry.
    #[serde(default, skip_serializing_if = "is_false", with = "int_bool")]
    pub(crate) b: bool,
    /// Known to be in KV.
    #[serde(default, skip_serializing_if = "is_false", with = "int_bool")]
    pub(crate) k: bool,
    /// A stub: dropped for size and still in KV.
    #[serde(default, skip_serializing_if = "is_false", with = "int_bool")]
    pub(crate) d: bool,
}

impl fmt::Debug for SEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SEntry")
            .field("s", &self.s)
            .field("w", &self.w)
            .field("b", &self.b)
            .field("k", &self.k)
            .field("d", &self.d)
            .finish_non_exhaustive()
    }
}

impl SEntry {
    /// A browser KV-gap entry staged at `sa`.
    #[must_use]
    pub(crate) fn staged_browser(s: usize, uid: &str, sa: u64) -> Self {
        Self {
            s,
            u: Some(uid.to_owned()),
            t: Some(sa),
            sa: Some(sa),
            b: true,
            ..Self::default()
        }
    }

    /// A provider entry staged at `sa`, replacing revision `pv`.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn staged_provider(
        s: usize,
        record: &KvPartnerId,
        pv: Option<String>,
        sa: u64,
    ) -> Self {
        Self {
            rv: None,
            pv,
            sa: Some(sa),
            k: false,
            ..Self::confirmed(s, record)
        }
    }

    /// A record known to be in KV.
    #[must_use]
    pub(crate) fn confirmed(s: usize, record: &KvPartnerId) -> Self {
        Self {
            s,
            u: Some(record.uid.clone()),
            w: record.w.clone(),
            t: record.at,
            e: record.exp,
            r: record.ra,
            a: record.atype,
            x: record.ext.clone(),
            rv: record.rv.clone(),
            k: true,
            ..Self::default()
        }
    }

    /// A stub for a source that is still in KV.
    #[must_use]
    pub(crate) fn stub(s: usize) -> Self {
        Self {
            s,
            d: true,
            ..Self::default()
        }
    }

    /// The UID, absent on a stub.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn uid(&self) -> Option<&str> {
        self.u.as_deref()
    }

    /// Whether this is a browser KV-gap entry.
    #[must_use]
    pub(crate) fn is_browser_gap(&self) -> bool {
        self.b && !self.d
    }

    /// Whether this is a staged provider entry.
    #[must_use]
    pub(crate) fn is_staged_provider(&self) -> bool {
        !self.k && !self.b && !self.d && self.w.is_some()
    }

    /// The record this entry carries. `None` for a stub.
    #[must_use]
    pub(crate) fn to_record(&self) -> Option<KvPartnerId> {
        Some(KvPartnerId {
            uid: self.u.clone()?,
            w: self.w.clone(),
            rv: self.rv.clone(),
            at: self.t,
            exp: self.e,
            ra: self.r,
            ru: None,
            atype: self.a,
            ext: self.x.clone(),
        })
    }
}

/// Mirrored and staged state of one provider in `__Host-ts-eids-s`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SProviderState {
    /// Next eligible time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) n: Option<u64>,
    /// Consecutive failures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) f: Option<u32>,
    /// Mirrored opt-out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) o: Option<KvOptOut>,
    /// Mirrored invalidated-UID hashes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) iv: Vec<String>,
    /// A staged opt-out clear.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) oc: Option<u64>,
    /// Pending marker: when an after-send dispatch happened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) p: Option<u64>,
    /// `n` and `f` are staged and not yet written.
    #[serde(default, skip_serializing_if = "is_false", with = "int_bool")]
    pub(crate) sg: bool,
    /// The KV `n` a cookie-only `n` was derived from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) kn: Option<u64>,
}

impl SProviderState {
    /// Still in KV, with no marker and no staged change: droppable for size.
    fn is_droppable(&self) -> bool {
        self.p.is_none() && !self.sg && self.oc.is_none()
    }
}

/// The result of opening `__Host-ts-eids-s`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SOpen {
    /// No cookie.
    Missing,
    /// Duplicated, failed to open or decode. Rebuilt by the next KV read.
    Failed,
    /// The identity config changed. Only the markers were kept.
    ConfigChanged(SCookie),
    /// Opened for this EC and config.
    Valid(SCookie),
}

impl SOpen {
    /// The usable cookie, if any.
    #[must_use]
    pub(crate) fn cookie(&self) -> Option<&SCookie> {
        match self {
            Self::Valid(cookie) | Self::ConfigChanged(cookie) => Some(cookie),
            Self::Missing | Self::Failed => None,
        }
    }
}

/// Opens `__Host-ts-eids-s` for this EC and config.
#[must_use]
pub(crate) fn open_s_cookie(
    keys: &IdentityKeys,
    ec_id: &str,
    catalog: &ProviderCatalog,
    headers: &HeaderMap,
) -> SOpen {
    let value = match read_single_cookie(headers, COOKIE_S) {
        CookieRead::Absent => return SOpen::Missing,
        CookieRead::Duplicate => {
            log::debug!("Ignoring duplicate {COOKIE_S} cookies");
            return SOpen::Failed;
        }
        CookieRead::Value(value) => value,
    };
    let Some(cookie) = open(&keys.cookie_s, ec_id, &value)
        .and_then(|payload| serde_json::from_slice::<SCookie>(&payload).ok())
        .filter(|cookie| cookie.v == 1)
    else {
        log::debug!("Ignoring {COOKIE_S} cookie that failed to open");
        return SOpen::Failed;
    };
    if cookie.cf != catalog.fingerprint() {
        return SOpen::ConfigChanged(cookie.markers_only(catalog.fingerprint()));
    }
    let source_count = catalog.configured_sources().len();
    let mut cookie = cookie;
    cookie.ids.retain(|entry| entry.s < source_count);
    SOpen::Valid(cookie)
}

fn seal_s_cookie(
    keys: &IdentityKeys,
    ec_id: &str,
    cookie: &SCookie,
    random: &dyn RandomSource,
) -> String {
    let payload = serde_json::to_vec(cookie).expect("should serialize the identity cookie");
    seal(&keys.cookie_s, ec_id, &payload, random)
}

/// Seals `cookie`, dropping entries until it fits [`MAX_S_COOKIE_BYTES`].
///
/// The drop order: confirmed entries become stubs; `ps` entries with no
/// marker or staged change go; staged provider entries go, lowest
/// `cookie_priority` first; browser KV-gap entries go last. If it still does
/// not fit, only the markers are kept, with `kc = 0`. Markers are never
/// dropped. `cookie` is updated to what was sealed.
#[must_use]
pub(crate) fn encode_s_cookie(
    keys: &IdentityKeys,
    ec_id: &str,
    cookie: &mut SCookie,
    random: &dyn RandomSource,
    priority: &dyn Fn(&str) -> i32,
) -> String {
    let mut sealed = seal_s_cookie(keys, ec_id, cookie, random);
    if sealed.len() <= MAX_S_COOKIE_BYTES {
        return sealed;
    }

    for entry in cookie.ids.iter_mut().filter(|entry| entry.k && !entry.d) {
        log::debug!(
            "Identity cookie over its cap: stubbing source index {}",
            entry.s
        );
        *entry = SEntry::stub(entry.s);
    }
    sealed = seal_s_cookie(keys, ec_id, cookie, random);
    if sealed.len() <= MAX_S_COOKIE_BYTES {
        return sealed;
    }

    cookie.ps.retain(|_, state| !state.is_droppable());
    sealed = seal_s_cookie(keys, ec_id, cookie, random);
    if sealed.len() <= MAX_S_COOKIE_BYTES {
        return sealed;
    }

    loop {
        let lowest = cookie
            .ids
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.is_staged_provider())
            .min_by_key(|(_, entry)| priority(entry.w.as_deref().unwrap_or_default()))
            .map(|(index, _)| index);
        let Some(index) = lowest else {
            break;
        };
        let dropped = cookie.ids.remove(index);
        log::debug!(
            "Identity cookie over its cap: dropping staged entry of {} for source index {}",
            dropped.w.as_deref().unwrap_or_default(),
            dropped.s
        );
        sealed = seal_s_cookie(keys, ec_id, cookie, random);
        if sealed.len() <= MAX_S_COOKIE_BYTES {
            return sealed;
        }
    }

    while let Some(index) = cookie.ids.iter().rposition(SEntry::is_browser_gap) {
        let dropped = cookie.ids.remove(index);
        log::debug!(
            "Identity cookie over its cap: dropping browser entry for source index {}",
            dropped.s
        );
        sealed = seal_s_cookie(keys, ec_id, cookie, random);
        if sealed.len() <= MAX_S_COOKIE_BYTES {
            return sealed;
        }
    }

    log::warn!("Identity cookie over its cap after every drop, keeping markers only");
    *cookie = cookie.markers_only(&cookie.cf.clone());
    seal_s_cookie(keys, ec_id, cookie, random)
}

/// A minimal cookie for a row seen missing: `m` only, which forces the next
/// writer to read KV and recover.
#[must_use]
pub(crate) fn minimal_missing_cookie(catalog: &ProviderCatalog) -> SCookie {
    SCookie {
        v: 1,
        kc: 0,
        cf: catalog.fingerprint().to_owned(),
        m: true,
        ids: Vec::new(),
        ps: BTreeMap::new(),
    }
}

/// What reconciling the cookie with a KV read needs.
pub(crate) struct CookieMergeContext<'a> {
    pub(crate) catalog: &'a ProviderCatalog,
    pub(crate) keys: IdentityKeys,
    /// Unix seconds.
    pub(crate) now: u64,
    pub(crate) kv_revalidate_sec: u64,
    /// Whether the live row makes a provider ineligible, which answers its
    /// marker (spec 5.6).
    pub(crate) answered_by_eligibility: &'a dyn Fn(&str, &KvEntry) -> bool,
}

#[cfg(test)]
fn never_answered(_: &str, _: &KvEntry) -> bool {
    false
}

#[cfg(test)]
impl<'a> CookieMergeContext<'a> {
    /// A context with test keys, a one-hour revalidation and no eligibility
    /// answers.
    pub(crate) fn for_test(catalog: &'a ProviderCatalog, now: u64) -> Self {
        Self {
            catalog,
            keys: super::test_support::test_keys(),
            now,
            kv_revalidate_sec: 3600,
            answered_by_eligibility: &never_answered,
        }
    }
}

/// The cookie after a KV read.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MergeOutcome {
    /// The reconciled cookie.
    pub(crate) cookie: SCookie,
    /// Whether it must be re-emitted.
    pub(crate) changed: bool,
    /// Whether the row is a tombstone: expire the cookie.
    pub(crate) expire: bool,
}

/// Reconciles the cookie with what a KV read found (spec 6.3, step 3).
#[must_use]
pub(crate) fn merge_with_kv(
    cookie: &SCookie,
    snapshot: &EcKvSnapshot,
    context: &CookieMergeContext<'_>,
) -> MergeOutcome {
    match snapshot {
        EcKvSnapshot::NotRead | EcKvSnapshot::Failed { .. } => MergeOutcome {
            cookie: cookie.clone(),
            changed: false,
            expire: false,
        },
        EcKvSnapshot::Missing { .. } => MergeOutcome {
            cookie: minimal_missing_cookie(context.catalog),
            changed: true,
            expire: false,
        },
        EcKvSnapshot::Present { entry, .. } if !entry.consent.ok => MergeOutcome {
            cookie: SCookie::new(context.catalog, context.now),
            changed: true,
            expire: true,
        },
        EcKvSnapshot::Present { entry, .. } => {
            let merged = SCookie {
                v: 1,
                kc: context.now,
                cf: context.catalog.fingerprint().to_owned(),
                m: false,
                ids: merge_ids(cookie, entry, context),
                ps: merge_provider_state(cookie, entry, context),
            };
            let content_changed = merged.ids != cookie.ids
                || merged.ps != cookie.ps
                || merged.m != cookie.m
                || merged.cf != cookie.cf;
            let stale = context.now.saturating_sub(cookie.kc) > context.kv_revalidate_sec / 2;
            MergeOutcome {
                cookie: merged,
                changed: content_changed || stale,
                expire: false,
            }
        }
    }
}

/// Whether a staged provider entry would still apply as an `OwnerWrite`.
fn staged_would_apply(entry: &SEntry, kv: Option<&KvPartnerId>, keys: &IdentityKeys) -> bool {
    match (&entry.pv, kv) {
        (Some(pv), Some(record)) => record_revision(keys, record).as_str() == pv,
        (Some(_), None) => false,
        (None, None) => true,
        (None, Some(record)) => record.w != entry.w,
    }
}

fn merge_ids(cookie: &SCookie, entry: &KvEntry, context: &CookieMergeContext<'_>) -> Vec<SEntry> {
    let sources = context.catalog.configured_sources();
    let owned = |source: &SourceDomain| context.catalog.owner_of(source).is_some();
    let mut ids = Vec::new();
    let mut covered = BTreeSet::new();

    for cookie_entry in &cookie.ids {
        let Some(source) = sources.get(cookie_entry.s) else {
            continue;
        };
        let kv = entry.ids.get(source.as_str());
        if cookie_entry.d || cookie_entry.k {
            // A stub or confirmed copy: the KV record wins, and the cookie
            // caches only owned sources.
            if let Some(record) = kv
                && owned(source)
            {
                ids.push(SEntry::confirmed(cookie_entry.s, record));
                covered.insert(cookie_entry.s);
            }
            continue;
        }
        if cookie_entry.b {
            if kv.is_none() {
                ids.push(cookie_entry.clone());
            }
            continue;
        }
        let flushed = kv.is_some_and(|record| {
            Some(&record.uid) == cookie_entry.u.as_ref() && record.w == cookie_entry.w
        });
        if flushed {
            if let Some(record) = kv
                && owned(source)
            {
                ids.push(SEntry::confirmed(cookie_entry.s, record));
                covered.insert(cookie_entry.s);
            }
        } else if staged_would_apply(cookie_entry, kv, &context.keys) {
            ids.push(cookie_entry.clone());
            covered.insert(cookie_entry.s);
        }
    }

    for (index, source) in sources.iter().enumerate() {
        if covered.contains(&index) || !owned(source) {
            continue;
        }
        if let Some(record) = entry.ids.get(source.as_str()) {
            ids.push(SEntry::confirmed(index, record));
        }
    }
    ids
}

/// Whether a KV read answers a provider's pending marker set at `marker`.
fn marker_answered(
    provider: &str,
    marker: u64,
    cookie: &SCookie,
    entry: &KvEntry,
    context: &CookieMergeContext<'_>,
) -> bool {
    let kv_state = entry.ps.get(provider);
    if kv_state.is_some_and(|state| state.n.is_some_and(|n| n >= marker) || state.o.is_some()) {
        return true;
    }
    let claimed: Vec<&SourceDomain> = provider
        .parse()
        .ok()
        .and_then(|id| context.catalog.get(&id))
        .map(|provider| provider.descriptor().sources().iter().collect())
        .unwrap_or_default();
    let sources = context.catalog.configured_sources();
    let acquired_since = claimed.iter().any(|source| {
        entry
            .ids
            .get(source.as_str())
            .is_some_and(|record| record.at.is_some_and(|at| at >= marker))
    });
    let lost_cookie_value = cookie.ids.iter().any(|cookie_entry| {
        cookie_entry.u.is_some()
            && sources.get(cookie_entry.s).is_some_and(|source| {
                claimed.contains(&source) && !entry.ids.contains_key(source.as_str())
            })
    });
    acquired_since || lost_cookie_value || (context.answered_by_eligibility)(provider, entry)
}

fn merge_provider_state(
    cookie: &SCookie,
    entry: &KvEntry,
    context: &CookieMergeContext<'_>,
) -> BTreeMap<String, SProviderState> {
    let mut ps: BTreeMap<String, SProviderState> = BTreeMap::new();
    for (provider, kv_state) in &entry.ps {
        let configured = provider
            .parse()
            .is_ok_and(|id| context.catalog.is_configured(&id));
        if !configured {
            continue;
        }
        ps.insert(
            provider.clone(),
            SProviderState {
                n: kv_state.n,
                f: (kv_state.f > 0).then_some(kv_state.f),
                o: kv_state.o.clone(),
                iv: kv_state.iv.clone(),
                ..SProviderState::default()
            },
        );
    }

    for (provider, state) in &cookie.ps {
        let kv_state = entry.ps.get(provider);
        let kv_n = kv_state.and_then(|kv_state| kv_state.n);
        let target = ps.entry(provider.clone()).or_default();
        if let Some(marker) = state.p
            && !marker_answered(provider, marker, cookie, entry, context)
        {
            target.p = Some(marker);
        }
        if state.sg
            && state
                .n
                .is_some_and(|staged| kv_n.is_none_or(|kv| kv < staged))
        {
            target.n = state.n;
            target.f = state.f;
            target.sg = true;
        }
        if let Some(clear) = state.oc
            && kv_state
                .and_then(|kv_state| kv_state.oc)
                .is_none_or(|kv_clear| kv_clear < clear)
        {
            target.oc = Some(clear);
        }
        if !state.sg
            && let (Some(n), Some(seen)) = (state.n, state.kn)
            && kv_n == Some(seen)
        {
            target.n = target.n.max(Some(n));
            target.kn = Some(seen);
        }
    }
    ps.retain(|_, state| *state != SProviderState::default());
    ps
}

/// A record the cookie view holds for one source.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Staging {
    /// Known to be in KV.
    Confirmed,
    /// Staged by a stager and not yet written.
    Staged {
        /// The revision it replaces.
        pv: Option<String>,
        /// When it was staged.
        sa: Option<u64>,
        /// A browser KV-gap entry.
        browser: bool,
    },
}

/// The cookie's records resolved to sources, stubs excluded.
#[must_use]
pub(crate) fn cookie_view(
    cookie: &SCookie,
    catalog: &ProviderCatalog,
) -> Vec<(SourceDomain, KvPartnerId, Staging)> {
    let sources = catalog.configured_sources();
    cookie
        .ids
        .iter()
        .filter_map(|entry| {
            let source = sources.get(entry.s)?.clone();
            let record = entry.to_record()?;
            let staging = if entry.k {
                Staging::Confirmed
            } else {
                Staging::Staged {
                    pv: entry.pv.clone(),
                    sa: entry.sa,
                    browser: entry.b,
                }
            };
            Some((source, record, staging))
        })
        .collect()
}

/// Maximum sealed bytes of one `__Host-ts-eids-r-*` cookie.
pub(crate) const MAX_R_COOKIE_BYTES: usize = 512;
/// Openable removal cookies at which TS writes overflow batches to KV
/// instead of adding another cookie.
pub(crate) const R_COOKIE_SOFT_LIMIT: usize = 4;

/// One staged removal. `s` is the source domain itself, never an index,
/// because removal cookies carry no `cf`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RRemoval {
    /// The source domain.
    pub(crate) s: String,
    /// The writer whose record is removed.
    pub(crate) w: String,
    /// The revision removed. Absent: whatever `w` acquired at or before `sa`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pv: Option<String>,
    /// When the removal was staged.
    pub(crate) sa: u64,
}

/// One response's batch of removals, opt-outs and invalidated-UID hashes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RBatch {
    /// Removals.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) rm: Vec<RRemoval>,
    /// Opt-outs: provider to the time it was reported.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) o: BTreeMap<String, u64>,
    /// Invalidated-UID hashes per provider.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) iv: BTreeMap<String, Vec<String>>,
}

impl RBatch {
    /// Whether the batch holds nothing.
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.rm.is_empty() && self.o.is_empty() && self.iv.is_empty()
    }

    fn item_count(&self) -> usize {
        self.rm.len() + self.o.len() + self.iv.values().map(Vec::len).sum::<usize>()
    }
}

/// The removal cookies a request carried.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct OpenedRemovals {
    /// Cookies that opened, by name.
    pub(crate) cookies: Vec<(String, RBatch)>,
    /// Names of cookies that failed to open. The writer deletes them.
    pub(crate) unopenable: Vec<String>,
}

/// Opens every `__Host-ts-eids-r-*` cookie for this EC.
#[cfg(test)]
#[must_use]
pub(crate) fn open_r_cookies(
    keys: &IdentityKeys,
    ec_id: &str,
    headers: &HeaderMap,
) -> OpenedRemovals {
    let mut opened = OpenedRemovals::default();
    for (name, value) in read_prefixed_cookies(headers, COOKIE_TS_EIDS_R_PREFIX) {
        match open(&keys.cookie_r, ec_id, &value)
            .and_then(|payload| serde_json::from_slice::<RBatch>(&payload).ok())
        {
            Some(batch) => opened.cookies.push((name, batch)),
            None => {
                log::debug!("Ignoring removal cookie that failed to open");
                opened.unopenable.push(name);
            }
        }
    }
    opened
}

/// Turns a batch into mutation changes. Entries whose source or provider
/// does not parse are dropped.
#[must_use]
pub(crate) fn r_batch_to_changes(
    batch: &RBatch,
) -> (
    Vec<PartnerIdChange>,
    BTreeMap<IdentityProviderId, ProviderStateChange>,
) {
    let mut changes = Vec::new();
    for removal in &batch.rm {
        let (Ok(source), Ok(writer)) = (
            removal.s.parse::<SourceDomain>(),
            removal.w.parse::<IdentityProviderId>(),
        ) else {
            continue;
        };
        changes.push(match &removal.pv {
            Some(pv) => PartnerIdChange::OwnerRemove {
                source,
                writer,
                expected: Revision::from_stored(pv),
            },
            None => PartnerIdChange::WriterRemove {
                source,
                writer,
                staged_at: Some(removal.sa),
            },
        });
    }

    let mut states: BTreeMap<IdentityProviderId, ProviderStateChange> = BTreeMap::new();
    for (provider, at) in &batch.o {
        let Ok(provider) = provider.parse::<IdentityProviderId>() else {
            continue;
        };
        // The batch carries no fingerprint: a re-grant by GPP section values
        // cannot be compared against it, so only the other clears apply.
        states.entry(provider).or_default().opt_out = Some(KvOptOut {
            at: *at,
            fp: String::new(),
        });
    }
    for (provider, hashes) in &batch.iv {
        let Ok(provider) = provider.parse::<IdentityProviderId>() else {
            continue;
        };
        states
            .entry(provider)
            .or_default()
            .iv_add
            .extend(hashes.iter().cloned());
    }
    (changes, states)
}

/// Whether the live row shows every removal and opt-out of the batch
/// applied. `iv` hashes never keep a cookie alive.
#[must_use]
pub(crate) fn r_batch_applied(batch: &RBatch, entry: &KvEntry, keys: &IdentityKeys) -> bool {
    let removals_applied = batch.rm.iter().all(|removal| {
        let Some(record) = entry.ids.get(&removal.s) else {
            return true;
        };
        match &removal.pv {
            Some(pv) => record_revision(keys, record).as_str() != pv,
            None => {
                record.w.as_deref() != Some(removal.w.as_str())
                    || record.at.is_some_and(|at| at > removal.sa)
            }
        }
    });
    let opt_outs_applied = batch.o.iter().all(|(provider, at)| {
        entry.ps.get(provider).is_some_and(|state| {
            state.o.as_ref().is_some_and(|opt_out| opt_out.at >= *at)
                || state.oc.is_some_and(|cleared| cleared > *at)
        })
    });
    removals_applied && opt_outs_applied
}

/// A new removal cookie name: the prefix and 8 random hex digits.
#[must_use]
pub(crate) fn new_r_cookie_name(random: &dyn RandomSource) -> String {
    let mut bytes = [0u8; 4];
    random.fill(&mut bytes);
    format!("{COOKIE_TS_EIDS_R_PREFIX}{}", hex::encode(bytes))
}

fn seal_r_batch(
    keys: &IdentityKeys,
    ec_id: &str,
    batch: &RBatch,
    random: &dyn RandomSource,
) -> String {
    let payload = serde_json::to_vec(batch).expect("should serialize a removal batch");
    seal(&keys.cookie_r, ec_id, &payload, random)
}

/// Where a response's removal batch goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RPlan {
    /// A new cookie of its own.
    Cookie {
        /// The cookie name.
        name: String,
        /// The sealed value.
        value: String,
    },
    /// Written to KV before send (spec 6.2, overflow).
    Overflow,
}

/// Plans a batch: one new cookie when it fits and fewer than four openable
/// removal cookies are present, else an overflow write.
#[must_use]
pub(crate) fn plan_r_batch(
    keys: &IdentityKeys,
    ec_id: &str,
    batch: &RBatch,
    inbound_openable: usize,
    random: &dyn RandomSource,
) -> RPlan {
    if inbound_openable >= R_COOKIE_SOFT_LIMIT {
        return RPlan::Overflow;
    }
    let value = seal_r_batch(keys, ec_id, batch, random);
    if value.len() > MAX_R_COOKIE_BYTES {
        return RPlan::Overflow;
    }
    RPlan::Cookie {
        name: new_r_cookie_name(random),
        value,
    }
}

/// Splits a batch across as many removal cookies as it needs, without
/// coarsening it: every removal, opt-out and hash is kept exactly.
#[must_use]
pub(crate) fn split_r_batch(
    keys: &IdentityKeys,
    ec_id: &str,
    batch: &RBatch,
    random: &dyn RandomSource,
) -> Vec<(String, String)> {
    let mut items: Vec<RBatch> = Vec::with_capacity(batch.item_count());
    items.extend(batch.rm.iter().map(|removal| RBatch {
        rm: vec![removal.clone()],
        ..RBatch::default()
    }));
    items.extend(batch.o.iter().map(|(provider, at)| RBatch {
        o: [(provider.clone(), *at)].into_iter().collect(),
        ..RBatch::default()
    }));
    for (provider, hashes) in &batch.iv {
        items.extend(hashes.iter().map(|hash| {
            RBatch {
                iv: [(provider.clone(), vec![hash.clone()])]
                    .into_iter()
                    .collect(),
                ..RBatch::default()
            }
        }));
    }

    let mut cookies = Vec::new();
    let mut current = RBatch::default();
    for item in items {
        let mut candidate = current.clone();
        candidate.rm.extend(item.rm.clone());
        candidate.o.extend(item.o.clone());
        for (provider, hashes) in &item.iv {
            candidate
                .iv
                .entry(provider.clone())
                .or_default()
                .extend(hashes.iter().cloned());
        }
        if current.is_empty()
            || seal_r_batch(keys, ec_id, &candidate, random).len() <= MAX_R_COOKIE_BYTES
        {
            current = candidate;
            continue;
        }
        cookies.push(current);
        current = item;
    }
    if !current.is_empty() {
        cookies.push(current);
    }

    cookies
        .into_iter()
        .map(|batch| {
            let value = seal_r_batch(keys, ec_id, &batch, random);
            if value.len() > MAX_R_COOKIE_BYTES {
                log::error!("A single removal does not fit a removal cookie; setting it anyway");
            }
            (new_r_cookie_name(random), value)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use http::{HeaderMap, HeaderValue, header};

    use super::*;
    use crate::ec::identity::input::SeededRandom;

    const EC: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.ABC123";
    const OTHER_EC: &str =
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.ABC123";

    #[test]
    fn seal_and_open_round_trip_bound_to_the_ec_id() {
        let key = [7u8; 32];
        let random = SeededRandom::new(3);
        let sealed = seal(&key, EC, br#"{"v":1}"#, &random);

        assert!(sealed.starts_with("v1."), "should carry the version prefix");
        assert_eq!(sealed.split('.').count(), 3, "should have three parts");
        assert!(!sealed.contains('='), "should be unpadded base64url");
        assert_eq!(
            open(&key, EC, &sealed).as_deref(),
            Some(&br#"{"v":1}"#[..]),
            "should open for its EC"
        );
        assert!(
            open(&key, OTHER_EC, &sealed).is_none(),
            "should not open for another EC"
        );
        assert!(
            open(&[8u8; 32], EC, &sealed).is_none(),
            "should not open with another key"
        );
        let mut tampered = sealed.clone();
        let last = tampered.pop().expect("should have a last character");
        tampered.push(if last == 'A' { 'B' } else { 'A' });
        assert!(
            open(&key, EC, &tampered).is_none(),
            "should reject tampering"
        );
        assert!(
            open(&key, EC, "v2.x.y").is_none(),
            "should reject another version"
        );
        assert!(
            open(&key, EC, "v1.x").is_none(),
            "should reject a short value"
        );
        assert_ne!(
            seal(&key, EC, b"same", &random),
            seal(&key, EC, b"same", &random),
            "every seal draws a fresh nonce"
        );
    }

    #[test]
    fn duplicate_cookie_names_count_as_none() {
        let mut headers = HeaderMap::new();
        headers.append(
            header::COOKIE,
            HeaderValue::from_static("__Host-ts-eids-s=one"),
        );
        headers.append(
            header::COOKIE,
            HeaderValue::from_static("other=1; __Host-ts-eids-s=two"),
        );
        assert_eq!(
            read_single_cookie(&headers, "__Host-ts-eids-s"),
            CookieRead::Duplicate,
            "a duplicate counts as none"
        );

        let mut single = HeaderMap::new();
        single.insert(
            header::COOKIE,
            HeaderValue::from_static("__Host-ts-eids-s=one; x=2"),
        );
        assert_eq!(
            read_single_cookie(&single, "__Host-ts-eids-s"),
            CookieRead::Value("one".to_owned()),
            "a single cookie is read"
        );
        assert_eq!(
            read_single_cookie(&HeaderMap::new(), "__Host-ts-eids-s"),
            CookieRead::Absent,
            "absent"
        );
    }

    #[test]
    fn prefixed_cookies_and_attributes() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static(
                "__Host-ts-eids-r-0a1b2c3d=a; __Host-ts-eids-r-11111111=b; x=1",
            ),
        );
        let found = read_prefixed_cookies(&headers, "__Host-ts-eids-r-");
        assert_eq!(
            found,
            vec![
                ("__Host-ts-eids-r-0a1b2c3d".to_owned(), "a".to_owned()),
                ("__Host-ts-eids-r-11111111".to_owned(), "b".to_owned()),
            ],
            "should find both removal cookies"
        );
        assert_eq!(
            set_cookie_header("__Host-ts-eids-s", "v1.a.b"),
            "__Host-ts-eids-s=v1.a.b; Path=/; Secure; SameSite=Lax; HttpOnly; Max-Age=31536000",
            "should use the identity cookie attributes and no Domain"
        );
        assert_eq!(
            delete_cookie_header("__Host-ts-eids-s"),
            "__Host-ts-eids-s=; Path=/; Secure; SameSite=Lax; HttpOnly; Max-Age=0",
            "should expire with matching attributes"
        );
    }
}

#[cfg(test)]
mod s_cookie_tests {
    use super::*;
    use crate::ec::EcKvSnapshot;
    use crate::ec::identity::input::SeededRandom;
    use crate::ec::identity::test_support::{catalog_with_owner, test_keys};
    use crate::ec::kv_types::{KvEntry, KvPartnerId, KvProviderState};

    const EC: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.ABC123";

    fn present(entry: KvEntry) -> EcKvSnapshot {
        EcKvSnapshot::Present {
            ec_id: EC.to_owned(),
            entry: Box::new(entry),
            generation: Some(1),
        }
    }

    fn headers_with(value: &str) -> http::HeaderMap {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::COOKIE,
            http::HeaderValue::from_str(&format!("{COOKIE_S}={value}"))
                .expect("should build cookie header"),
        );
        headers
    }

    fn owner_record(uid: &str, rv: &str) -> KvPartnerId {
        KvPartnerId {
            w: Some("owner".to_owned()),
            rv: Some(rv.to_owned()),
            at: Some(90),
            ..KvPartnerId::new(uid)
        }
    }

    #[test]
    fn round_trips_and_detects_config_change() {
        let keys = test_keys();
        let catalog = catalog_with_owner("owner", "a.example.com");
        let random = SeededRandom::new(1);
        let mut cookie = SCookie::new(&catalog, 100);
        cookie.ps.insert(
            "owner".to_owned(),
            SProviderState {
                p: Some(90),
                ..SProviderState::default()
            },
        );
        cookie.ids.push(SEntry::staged_browser(0, "u", 100));
        let sealed = encode_s_cookie(&keys, EC, &mut cookie, &random, &|_| 0);

        match open_s_cookie(&keys, EC, &catalog, &headers_with(&sealed)) {
            SOpen::Valid(opened) => assert_eq!(opened, cookie, "should round-trip"),
            other => panic!("should open, got {other:?}"),
        }
        let other_catalog = catalog_with_owner("owner", "b.example.com");
        match open_s_cookie(&keys, EC, &other_catalog, &headers_with(&sealed)) {
            SOpen::ConfigChanged(kept) => {
                assert!(kept.ids.is_empty(), "should drop positive entries");
                assert_eq!(
                    kept.ps.get("owner").and_then(|state| state.p),
                    Some(90),
                    "should keep markers"
                );
            }
            other => panic!("should flag the config change, got {other:?}"),
        }
        assert!(
            matches!(
                open_s_cookie(&keys, EC, &catalog, &http::HeaderMap::new()),
                SOpen::Missing
            ),
            "no cookie is Missing"
        );
        assert!(
            matches!(
                open_s_cookie(&keys, EC, &catalog, &headers_with("v1.bad.value")),
                SOpen::Failed
            ),
            "garbage fails to open"
        );
    }

    #[test]
    fn tombstone_expires_and_missing_row_minimizes() {
        let catalog = catalog_with_owner("owner", "a.example.com");
        let mut cookie = SCookie::new(&catalog, 100);
        cookie.ps.insert(
            "owner".to_owned(),
            SProviderState {
                p: Some(90),
                ..SProviderState::default()
            },
        );
        cookie.ids.push(SEntry::staged_browser(0, "u", 100));
        let context = CookieMergeContext::for_test(&catalog, 200);
        assert!(
            merge_with_kv(&cookie, &present(KvEntry::tombstone(1)), &context).expire,
            "tombstone expires the cookie"
        );
        let missing = merge_with_kv(
            &cookie,
            &EcKvSnapshot::Missing {
                ec_id: EC.to_owned(),
            },
            &context,
        );
        assert!(missing.cookie.m, "should set m");
        assert!(
            missing.cookie.ids.is_empty() && missing.cookie.ps.is_empty(),
            "no entries, no markers"
        );
        assert!(missing.changed, "should be rewritten");
        let unread = merge_with_kv(&cookie, &EcKvSnapshot::NotRead, &context);
        assert!(!unread.changed, "no read, no change");
        assert_eq!(unread.cookie, cookie, "no read keeps the cookie");
    }

    #[test]
    fn browser_gap_entry_is_dropped_once_kv_holds_the_source() {
        let catalog = catalog_with_owner("owner", "a.example.com");
        let mut cookie = SCookie::new(&catalog, 100);
        cookie.ids.push(SEntry::staged_browser(0, "u", 100));

        let held = merge_with_kv(
            &cookie,
            &present(KvEntry::minimal("a.example.com", "other", 1)),
            &CookieMergeContext::for_test(&catalog, 200),
        );
        assert!(held.changed, "should change");
        assert!(
            held.cookie.ids.iter().all(|entry| !entry.is_browser_gap()),
            "the b:1 entry is dropped once KV holds the source"
        );
        assert_eq!(held.cookie.kc, 200, "should stamp kc");

        let absent = merge_with_kv(
            &cookie,
            &present(KvEntry::minimal("z.example.com", "x", 1)),
            &CookieMergeContext::for_test(&catalog, 200),
        );
        assert!(
            absent.cookie.ids.iter().any(SEntry::is_browser_gap),
            "control: kept while KV lacks the source"
        );
    }

    #[test]
    fn owned_kv_record_is_cached_and_staged_owner_entry_follows_revision() {
        let catalog = catalog_with_owner("owner", "a.example.com");
        let mut entry = KvEntry::minimal("z.example.com", "x", 1);
        entry.ids.insert(
            "a.example.com".to_owned(),
            owner_record("in-kv", "1111111111111111"),
        );
        let context = CookieMergeContext::for_test(&catalog, 200);

        let cached = merge_with_kv(
            &SCookie::new(&catalog, 100),
            &present(entry.clone()),
            &context,
        );
        let confirmed = cached
            .cookie
            .ids
            .iter()
            .find(|cookie_entry| cookie_entry.s == 0)
            .expect("should cache the owned record");
        assert!(confirmed.k, "cached as known in KV");
        assert_eq!(confirmed.uid(), Some("in-kv"), "with the KV UID");

        let staged = |pv: &str| {
            let mut cookie = SCookie::new(&catalog, 100);
            cookie.ids.push(SEntry::staged_provider(
                0,
                &owner_record("staged", "ffffffffffffffff"),
                Some(pv.to_owned()),
                150,
            ));
            merge_with_kv(&cookie, &present(entry.clone()), &context).cookie
        };
        let kept = staged("1111111111111111");
        assert!(
            kept.ids
                .iter()
                .any(|cookie_entry| cookie_entry.uid() == Some("staged") && !cookie_entry.k),
            "a staged refresh OwnerWrite would apply stays staged"
        );
        let lost = staged("2222222222222222");
        assert!(
            lost.ids
                .iter()
                .all(|cookie_entry| cookie_entry.uid() != Some("staged")),
            "a staged refresh against another revision is dropped"
        );
        assert!(
            lost.ids
                .iter()
                .any(|cookie_entry| cookie_entry.uid() == Some("in-kv") && cookie_entry.k),
            "KV wins"
        );
    }

    #[test]
    fn marker_is_answered_by_a_newer_n_or_an_opt_out() {
        let catalog = catalog_with_owner("owner", "a.example.com");
        let mut cookie = SCookie::new(&catalog, 100);
        cookie.ps.insert(
            "owner".to_owned(),
            SProviderState {
                p: Some(150),
                ..SProviderState::default()
            },
        );
        let answered = |state: KvProviderState| {
            let mut entry = KvEntry::minimal("b.example.com", "x", 1);
            entry.ps.insert("owner".to_owned(), state);
            merge_with_kv(
                &cookie,
                &present(entry),
                &CookieMergeContext::for_test(&catalog, 200),
            )
            .cookie
            .ps
            .get("owner")
            .and_then(|state| state.p)
        };
        assert_eq!(
            answered(KvProviderState {
                n: Some(160),
                ..KvProviderState::default()
            }),
            None,
            "a newer n answers the marker"
        );
        assert_eq!(
            answered(KvProviderState {
                n: Some(140),
                ..KvProviderState::default()
            }),
            Some(150),
            "an older n does not"
        );
    }

    #[test]
    fn unchanged_recent_cookie_is_not_rewritten() {
        let catalog = catalog_with_owner("owner", "a.example.com");
        let cookie = SCookie::new(&catalog, 1_000);
        let entry = KvEntry::minimal("z.example.com", "x", 1);
        let recent = merge_with_kv(
            &cookie,
            &present(entry.clone()),
            &CookieMergeContext::for_test(&catalog, 1_100),
        );
        assert!(!recent.changed, "nothing changed and kc is recent");
        let stale = merge_with_kv(
            &cookie,
            &present(entry),
            &CookieMergeContext::for_test(&catalog, 1_000 + 1_801),
        );
        assert!(stale.changed, "kc older than half of kv_revalidate_sec");
    }

    #[test]
    fn size_cap_drops_in_order_and_keeps_markers() {
        let keys = test_keys();
        let catalog = catalog_with_owner("owner", "a.example.com");
        let random = SeededRandom::new(1);
        let mut cookie = SCookie::new(&catalog, 100);
        cookie.ps.insert(
            "owner".to_owned(),
            SProviderState {
                p: Some(90),
                ..SProviderState::default()
            },
        );
        for _ in 0..40 {
            cookie
                .ids
                .push(SEntry::staged_browser(0, &"u".repeat(60), 100));
        }
        let sealed = encode_s_cookie(&keys, EC, &mut cookie, &random, &|_| 0);
        assert!(sealed.len() <= MAX_S_COOKIE_BYTES, "should fit the cap");
        assert_eq!(
            cookie.ps.get("owner").and_then(|state| state.p),
            Some(90),
            "should keep the marker"
        );
        assert!(
            !cookie.ids.is_empty(),
            "should keep as many entries as fit, not drop them all"
        );
    }

    #[test]
    fn size_cap_stubs_confirmed_entries_before_dropping_staged_ones() {
        let keys = test_keys();
        let catalog = catalog_with_owner("owner", "a.example.com");
        let random = SeededRandom::new(1);
        let mut cookie = SCookie::new(&catalog, 100);
        cookie.ids.push(SEntry::confirmed(
            0,
            &owner_record(&"c".repeat(500), "1111111111111111"),
        ));
        cookie
            .ids
            .push(SEntry::staged_browser(0, &"b".repeat(400), 100));
        for _ in 0..3 {
            cookie.ids.push(SEntry::staged_provider(
                0,
                &owner_record(&"p".repeat(300), "ffffffffffffffff"),
                None,
                100,
            ));
        }
        let sealed = encode_s_cookie(&keys, EC, &mut cookie, &random, &|_| 0);
        assert!(sealed.len() <= MAX_S_COOKIE_BYTES, "should fit the cap");
        assert!(
            cookie.ids.iter().any(|entry| entry.d),
            "the confirmed entry becomes a stub first"
        );
        assert!(
            cookie.ids.iter().any(SEntry::is_browser_gap),
            "the browser gap entry is dropped last"
        );
    }
}

#[cfg(test)]
mod r_cookie_tests {
    use super::*;
    use crate::ec::identity::input::SeededRandom;
    use crate::ec::identity::mutation::PartnerIdChange;
    use crate::ec::identity::test_support::test_keys;
    use crate::ec::kv_types::{KvEntry, KvOptOut, KvPartnerId, KvProviderState};

    const EC: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.ABC123";

    fn removal(source: &str, writer: &str, sa: u64) -> RRemoval {
        RRemoval {
            s: source.to_owned(),
            w: writer.to_owned(),
            pv: None,
            sa,
        }
    }

    fn batch_of(count: usize) -> RBatch {
        RBatch {
            rm: (0..count)
                .map(|index| removal(&format!("s{index}.example.com"), "provider", 1_760_000_000))
                .collect(),
            ..RBatch::default()
        }
    }

    fn headers_for(cookies: &[(String, String)]) -> http::HeaderMap {
        let joined = cookies
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ");
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::COOKIE,
            http::HeaderValue::from_str(&joined).expect("should build header"),
        );
        headers
    }

    #[test]
    fn a_typical_batch_fits_one_cookie_and_a_long_domain_fits_alone() {
        let keys = test_keys();
        let random = SeededRandom::new(1);
        let RPlan::Cookie { name, value } = plan_r_batch(&keys, EC, &batch_of(4), 0, &random)
        else {
            panic!("four typical removals should fit one cookie");
        };
        assert!(
            name.starts_with("__Host-ts-eids-r-"),
            "should use the prefix"
        );
        assert_eq!(
            name.len(),
            "__Host-ts-eids-r-".len() + 8,
            "should add 8 hex digits"
        );
        assert!(value.len() <= MAX_R_COOKIE_BYTES, "should fit 512 bytes");

        let label = "a".repeat(63);
        let long_domain = format!("{label}.{label}.{label}.{}.com", "b".repeat(57));
        assert_eq!(long_domain.len(), 253, "should be a 253-byte domain");
        let one = RBatch {
            rm: vec![removal(&long_domain, "a-long-provider-id", 1_760_000_000)],
            ..RBatch::default()
        };
        assert!(
            matches!(
                plan_r_batch(&keys, EC, &one, 0, &random),
                RPlan::Cookie { .. }
            ),
            "one long removal fits"
        );
    }

    #[test]
    fn overflow_when_too_big_or_four_cookies_present() {
        let keys = test_keys();
        let random = SeededRandom::new(1);
        assert!(
            matches!(
                plan_r_batch(&keys, EC, &batch_of(12), 0, &random),
                RPlan::Overflow
            ),
            "too big"
        );
        assert!(
            matches!(
                plan_r_batch(&keys, EC, &batch_of(1), 4, &random),
                RPlan::Overflow
            ),
            "four present"
        );
        assert!(
            matches!(
                plan_r_batch(&keys, EC, &batch_of(1), 3, &random),
                RPlan::Cookie { .. }
            ),
            "control: three present still allows a cookie"
        );
    }

    #[test]
    fn split_keeps_every_entry_exactly() {
        let keys = test_keys();
        let random = SeededRandom::new(1);
        let big = RBatch {
            o: [("provider".to_owned(), 5)].into_iter().collect(),
            iv: [("provider".to_owned(), vec!["0123456789abcdef".to_owned()])]
                .into_iter()
                .collect(),
            ..batch_of(12)
        };
        let cookies = split_r_batch(&keys, EC, &big, &random);
        assert!(cookies.len() > 1, "should need several cookies");
        assert!(
            cookies
                .iter()
                .all(|(_, value)| value.len() <= MAX_R_COOKIE_BYTES),
            "each fits"
        );
        let opened = open_r_cookies(&keys, EC, &headers_for(&cookies));
        assert!(opened.unopenable.is_empty(), "every cookie opens");
        let mut rebuilt = RBatch::default();
        for (_, batch) in &opened.cookies {
            rebuilt.rm.extend(batch.rm.clone());
            rebuilt.o.extend(batch.o.clone());
            for (provider, hashes) in &batch.iv {
                rebuilt
                    .iv
                    .entry(provider.clone())
                    .or_default()
                    .extend(hashes.clone());
            }
        }
        rebuilt.rm.sort_by(|a, b| a.s.cmp(&b.s));
        let mut expected = big.rm.clone();
        expected.sort_by(|a, b| a.s.cmp(&b.s));
        assert_eq!(rebuilt.rm, expected, "every removal survives unchanged");
        assert_eq!(rebuilt.o, big.o, "the opt-out survives unchanged");
        assert_eq!(rebuilt.iv, big.iv, "the iv hash survives unchanged");
    }

    #[test]
    fn applied_rules() {
        let keys = test_keys();
        let mut entry = KvEntry::minimal("b.example.com", "x", 1);
        entry.ids.insert(
            "a.example.com".to_owned(),
            KvPartnerId {
                w: Some("p".to_owned()),
                at: Some(10),
                rv: Some("1111111111111111".to_owned()),
                ..KvPartnerId::new("u")
            },
        );
        let without_pv = |writer: &str, sa| RBatch {
            rm: vec![removal("a.example.com", writer, sa)],
            ..RBatch::default()
        };
        assert!(
            !r_batch_applied(&without_pv("p", 10), &entry, &keys),
            "pending"
        );
        assert!(
            r_batch_applied(&without_pv("p", 9), &entry, &keys),
            "re-acquired after sa"
        );
        assert!(
            r_batch_applied(&without_pv("q", 10), &entry, &keys),
            "other writer"
        );
        let with_pv = |pv: &str| RBatch {
            rm: vec![RRemoval {
                pv: Some(pv.to_owned()),
                ..removal("a.example.com", "p", 10)
            }],
            ..RBatch::default()
        };
        assert!(
            !r_batch_applied(&with_pv("1111111111111111"), &entry, &keys),
            "same revision"
        );
        assert!(
            r_batch_applied(&with_pv("2222222222222222"), &entry, &keys),
            "other revision"
        );

        entry.ps.insert(
            "p".to_owned(),
            KvProviderState {
                o: Some(KvOptOut {
                    at: 5,
                    fp: "0".repeat(16),
                }),
                ..KvProviderState::default()
            },
        );
        let opt_out = |at| RBatch {
            o: [("p".to_owned(), at)].into_iter().collect(),
            ..RBatch::default()
        };
        assert!(
            r_batch_applied(&opt_out(5), &entry, &keys),
            "KV o.at at or after"
        );
        assert!(
            !r_batch_applied(&opt_out(6), &entry, &keys),
            "newer opt-out pending"
        );
        entry.ps.get_mut("p").expect("should hold p").oc = Some(6);
        assert!(
            !r_batch_applied(&opt_out(6), &entry, &keys),
            "a same-second clear does not count"
        );
        entry.ps.get_mut("p").expect("should hold p").oc = Some(7);
        assert!(
            r_batch_applied(&opt_out(6), &entry, &keys),
            "a later clear makes it stale"
        );
        let iv_only = RBatch {
            iv: [("p".to_owned(), vec!["0".repeat(16)])]
                .into_iter()
                .collect(),
            ..RBatch::default()
        };
        assert!(
            r_batch_applied(&iv_only, &entry, &keys),
            "iv never keeps a cookie alive"
        );
    }

    #[test]
    fn batches_become_changes_by_source_domain() {
        let batch = RBatch {
            rm: vec![
                removal("a.example.com", "p", 10),
                RRemoval {
                    pv: Some("1111111111111111".to_owned()),
                    ..removal("b.example.com", "p", 10)
                },
                removal("not a domain", "p", 10),
            ],
            o: [("p".to_owned(), 7)].into_iter().collect(),
            iv: [("p".to_owned(), vec!["0".repeat(16)])]
                .into_iter()
                .collect(),
        };
        let (changes, states) = r_batch_to_changes(&batch);
        assert_eq!(
            changes.len(),
            2,
            "should drop the entry that does not parse"
        );
        assert!(
            matches!(
                &changes[0],
                PartnerIdChange::WriterRemove {
                    staged_at: Some(10),
                    ..
                }
            ),
            "a removal without pv is a WriterRemove"
        );
        assert!(
            matches!(&changes[1], PartnerIdChange::OwnerRemove { .. }),
            "a removal with pv is an OwnerRemove"
        );
        let state = &states[&"p".parse().expect("should parse id")];
        assert_eq!(state.opt_out.as_ref().map(|o| o.at), Some(7), "opt-out at");
        assert_eq!(state.iv_add.len(), 1, "iv hash");
    }

    #[test]
    fn unopenable_cookies_are_listed_for_deletion() {
        let keys = test_keys();
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::COOKIE,
            http::HeaderValue::from_static("__Host-ts-eids-r-00000000=v1.bad.value"),
        );
        let opened = open_r_cookies(&keys, EC, &headers);
        assert!(opened.cookies.is_empty(), "nothing opens");
        assert_eq!(
            opened.unopenable,
            vec!["__Host-ts-eids-r-00000000".to_owned()],
            "listed for deletion"
        );
    }
}
