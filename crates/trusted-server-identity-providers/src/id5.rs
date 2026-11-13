//! ID5 identity provider: resolves the ID5 ID server side.
//!
//! Posts the request's consent and network signals to ID5's fetch endpoint.
//! It needs no login and sends no hashed email. ID5 returns complete EIDs
//! under `ids`, or the legacy `universal_uid`, which is given to the first
//! configured source with the reply's `ext.linkType` as UID `ext`, as
//! Prebid's ID5 module does.
//!
//! Parsing is lenient per entry: an entry of `ids`, `universal_uid` or
//! `cache_control` with an unexpected shape is skipped rather than failing
//! the whole response. The opt-out UID `"0"` is never stored or forwarded.
//! `cache_control.max_age_sec` is bounded by [`bounded_expiry`]: a missing
//! value gets the default TTL and a zero or tiny one the minimum TTL.
//!
//! Before any lookup, the provider reads ID5's first-party cookies listed
//! in `cookie_names` (default `id5id`, written by Prebid's ID5 module), in
//! order, then, when the Lockr capability is enabled, the cookies Lockr AIM
//! writes (`[settings.lockr]`, default key `id5id`). A cookie holding the ID5 fetch response JSON
//! (plain or percent-encoded) is parsed exactly like a lookup response, and
//! a bare ID string becomes one UID on the first source with agent type 1
//! and the default TTL. A reply's non-empty `signature` is kept as the
//! UIDs' private refresh state, due a tenth of their TTL before they
//! expire, and sent back as `s` in the next fetch so ID5 can return the
//! same ID, as Prebid's ID5 module does. A
//! percent-encoded JSON cookie is decoded and its entries, `ext` objects
//! included, are copied out of the decoded text. A cookie that yields no
//! usable UID is ignored and the lookup runs as usual. A cookie holding
//! ID5's opt-out (a bare `0`, or a reply whose `universal_uid` is `0`)
//! reports an opt-out: the lookup is skipped and stored ID5 UIDs are
//! removed. A fetch reply whose `universal_uid` is `0` reports the same
//! opt-out.
//!
//! The request follows ID5's server-side client (its Prebid Server module):
//! `gpp_sid` is sent comma-joined, the client IP goes in `ip` (IPv4) or
//! `ipv6`, `provider` names who operates the integration (`provider_name`,
//! default the publisher domain), `domain` is the publisher domain,
//! `origin` is `trusted-server` and `v` is the crate version, without which
//! ID5 rejects the fetch. The field names for the client IP and user
//! agent should be confirmed with ID5 for each partner account.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::num::NonZeroUsize;
use std::time::Duration;

use bytes::Bytes;
use chrono::{DateTime, SecondsFormat, Utc};
use error_stack::{Report, ResultExt as _};
use http::header::{CONTENT_TYPE, ORIGIN};
use http::{HeaderMap, HeaderValue, Method};
use serde::{Deserialize, Serialize};
use trusted_server_core::ec::eid_types::{SourceDomain, UnixSeconds};
use trusted_server_core::ec::provider::{
    CookieRead, EidLevelFields, EidShape, IdentityProvider, LookupInput, LookupOutcome,
    ProviderCapabilities, ProviderCommon, ProviderConfigError, ProviderEid, ProviderError,
    ProviderExpiry, ProviderKind, ProviderRefresh, ProviderRequest, ProviderResponse, ProviderUid,
    RequestCookies, SourceCapability, SourceInput,
};
use url::Url;

use serde_json::value::RawValue;

use crate::common::{
    CookieReader, LockrSettings, ReadsLockr, VendorEid, bounded_expiry, is_routable_ip,
    is_usable_id, join_section_ids, percent_decoded,
};

/// Formats the request time as the RFC 3339 `ts` the `/gs/v2` fetch requires.
fn request_timestamp(now: UnixSeconds) -> Result<String, Report<ProviderError>> {
    let now = i64::try_from(now.get()).change_context(ProviderError::Request)?;
    DateTime::<Utc>::from_timestamp(now, 0)
        .map(|time| time.to_rfc3339_opts(SecondsFormat::Secs, true))
        .ok_or_else(|| Report::new(ProviderError::Request).attach("current time is out of range"))
}

/// Names this integration to ID5 in `origin`, as ID5's Prebid Server module
/// sends `pbs-java`.
const INTEGRATION_ORIGIN: &str = "trusted-server";

/// The integration version sent to ID5 as `v`; the fetch endpoint rejects a
/// request without one.
const INTEGRATION_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The UID ID5 returns for a visitor who opted out.
const OPTED_OUT_UID: &str = "0";

/// Most EIDs accepted from one ID5 response.
const MAX_EIDS: NonZeroUsize = NonZeroUsize::new(4).expect("should be non-zero");

/// Most UIDs accepted per ID5 EID.
const MAX_UIDS_PER_EID: NonZeroUsize = NonZeroUsize::new(2).expect("should be non-zero");

/// The ID5 provider kind.
pub struct Id5;

/// Settings for an `id5` provider.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Id5Settings {
    /// The ID5 partner number assigned to the publisher.
    pub partner_id: u32,
    /// First-party cookies read in order before any lookup; the first
    /// yielding a usable UID wins.
    #[serde(default = "Id5Settings::default_cookie_names")]
    pub cookie_names: Vec<String>,
    /// Who operates this integration, sent to ID5 as `provider`. Defaults
    /// to the publisher domain.
    #[serde(default)]
    pub provider_name: Option<String>,
    /// The Lockr capability; see [`LockrSettings`].
    #[serde(default)]
    pub lockr: LockrSettings,
}

impl Id5Settings {
    fn default_cookie_names() -> Vec<String> {
        vec!["id5id".to_owned()]
    }
}

/// A built ID5 provider.
#[derive(Debug)]
pub struct Id5Provider {
    capabilities: ProviderCapabilities,
    url: Url,
    partner_id: u32,
    provider_name: Option<String>,
    cookies: CookieReader,
}

impl ReadsLockr for Id5 {
    const LOCKR_COOKIE_NAMES: &'static [&'static str] = &["id5id"];
}

impl ProviderKind for Id5 {
    const KIND: &'static str = "id5";
    type Settings = Id5Settings;
    type Provider = Id5Provider;

    fn build(
        common: &ProviderCommon<'_>,
        settings: Id5Settings,
    ) -> Result<Id5Provider, Report<ProviderConfigError>> {
        if common.sources.is_empty() {
            return Err(Report::new(ProviderConfigError::Invalid)
                .attach("an id5 provider needs at least one source"));
        }
        let mut url = common
            .endpoint
            .ok_or_else(|| {
                Report::new(ProviderConfigError::Invalid).attach("a id5 provider needs an endpoint")
            })?
            .clone();
        url.path_segments_mut()
            .map_err(|()| {
                Report::new(ProviderConfigError::Invalid).attach("endpoint cannot hold a path")
            })?
            .pop_if_empty()
            .push(&format!("{}.json", settings.partner_id));
        Ok(Id5Provider {
            capabilities: ProviderCapabilities {
                sources: common
                    .sources
                    .iter()
                    .map(|source| SourceCapability {
                        source: source.clone(),
                        input: SourceInput::Anonymous,
                    })
                    .collect(),
                shape: EidShape {
                    max_eids: MAX_EIDS,
                    max_uids_per_eid: MAX_UIDS_PER_EID,
                    eid_level_fields: EidLevelFields::Preserved,
                },
            },
            url,
            partner_id: settings.partner_id,
            provider_name: settings.provider_name,
            cookies: CookieReader::with_lockr::<Self>(
                common,
                settings.cookie_names,
                settings.lockr,
            )?,
        })
    }
}

#[derive(Serialize)]
struct FetchBody<'a> {
    partner: u32,
    provider: &'a str,
    domain: &'a str,
    origin: &'static str,
    v: &'static str,
    gdpr: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    gdpr_consent: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    us_privacy: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gpp_string: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gpp_sid: Option<String>,
    ts: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    ua: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ipv6: Option<String>,
    /// The previous reply's `signature`, which lets ID5 return the same ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    s: Option<&'a str>,
}

/// The top-level reply; each field is parsed separately so one bad shape
/// does not fail the rest.
#[derive(Deserialize)]
struct FetchReply<'body> {
    #[serde(default, borrow)]
    universal_uid: Option<&'body RawValue>,
    #[serde(default, borrow)]
    ids: Option<&'body RawValue>,
    #[serde(default, borrow)]
    cache_control: Option<&'body RawValue>,
    #[serde(default, borrow)]
    ext: Option<&'body RawValue>,
    #[serde(default, borrow)]
    signature: Option<&'body RawValue>,
}

/// Prebid's shared `id5id` cookie: the reply for each partner under
/// `pbjs`, keyed by partner ID. Its top level holds the last partner's reply.
#[derive(Deserialize)]
struct MultiplexedCookie<'body> {
    #[serde(default, borrow)]
    pbjs: Option<BTreeMap<String, &'body RawValue>>,
}

#[derive(Deserialize)]
struct ReplyExt<'body> {
    #[serde(rename = "linkType", default, borrow)]
    link_type: Option<&'body RawValue>,
}

#[derive(Deserialize)]
struct Id5Id<'body> {
    #[serde(default, borrow)]
    eid: Option<VendorEid<'body>>,
}

#[derive(Deserialize)]
struct CacheControl {
    max_age_sec: Option<u64>,
}

impl Id5Provider {
    fn primary_source(&self) -> &SourceDomain {
        &self.capabilities.sources[0].source
    }

    /// Returns the configured source matching `source` after normalization
    /// (case and trailing dot).
    fn known_source(&self, source: &str) -> Option<&SourceDomain> {
        let source = SourceDomain::try_from(source).ok()?;
        self.capabilities
            .sources
            .iter()
            .map(|known| &known.source)
            .find(|known| **known == source)
    }

    /// Parses an ID5 fetch reply into its kept EIDs.
    fn parse_reply<'body>(
        &self,
        body: &'body [u8],
    ) -> Result<Vec<ProviderEid<'body>>, serde_json::Error> {
        let reply: FetchReply<'body> = serde_json::from_slice(body)?;
        let max_age = reply
            .cache_control
            .and_then(|raw| serde_json::from_str::<CacheControl>(raw.get()).ok())
            .and_then(|cache| cache.max_age_sec)
            .map(Duration::from_secs);
        let expiry = bounded_expiry(max_age);
        let signature = reply
            .signature
            .and_then(|raw| serde_json::from_str::<Cow<'body, str>>(raw.get()).ok())
            .filter(|signature| !signature.is_empty());
        let refresh = || {
            signature.as_ref().map(|signature| ProviderRefresh {
                state: signature.clone(),
                from: refresh_from(expiry),
            })
        };

        let entries: BTreeMap<String, &'body RawValue> = reply
            .ids
            .and_then(|raw| serde_json::from_str(raw.get()).ok())
            .unwrap_or_default();
        let mut eids: Vec<ProviderEid<'body>> = entries
            .into_values()
            .filter_map(|raw| serde_json::from_str::<Id5Id<'body>>(raw.get()).ok())
            .filter_map(|id| id.eid)
            .filter_map(|mut eid| {
                let source = self.known_source(&eid.source)?;
                eid.source = Cow::Owned(source.as_str().to_owned());
                eid.uids.retain(|uid| is_usable_uid(&uid.id));
                (!eid.uids.is_empty()).then(|| eid.into_provider_eid(expiry))
            })
            .collect();
        for uid in eids.iter_mut().flat_map(|eid| eid.uids.iter_mut()) {
            uid.refresh = refresh();
        }
        let primary = self.primary_source().as_str();
        let has_primary = eids.iter().any(|eid| eid.source == primary);
        let universal_uid = reply
            .universal_uid
            .and_then(|raw| serde_json::from_str::<Cow<'body, str>>(raw.get()).ok())
            .filter(|uid| is_usable_uid(uid));
        if !has_primary && let Some(uid) = universal_uid {
            let link_type = reply
                .ext
                .and_then(|raw| serde_json::from_str::<ReplyExt<'_>>(raw.get()).ok())
                .and_then(|ext| ext.link_type)
                .and_then(|link_type| {
                    RawValue::from_string(format!(r#"{{"linkType":{}}}"#, link_type.get())).ok()
                });
            eids.push(ProviderEid {
                source: Cow::Owned(primary.to_owned()),
                inserter: None,
                matcher: None,
                mm: None,
                ext: None,
                uids: vec![ProviderUid {
                    id: uid,
                    atype: Some(1),
                    ext: link_type.map(Cow::Owned),
                    expiry,
                    refresh: refresh(),
                }],
            });
        }
        Ok(eids)
    }

    /// Reads one ID5 cookie value: a fetch reply JSON or a bare ID.
    ///
    /// A shared cookie that holds replies per partner yields only this
    /// partner's reply, and nothing when it holds none for this partner.
    fn cookie_eids<'cookie>(&self, value: &'cookie str) -> Option<Vec<ProviderEid<'cookie>>> {
        let decoded = percent_decoded(value);
        let eids = if decoded.trim_start().starts_with('{') {
            match partner_reply(decoded, self.partner_id)? {
                Cow::Borrowed(reply) => self.parse_reply(reply.as_bytes()).ok()?,
                Cow::Owned(reply) => self
                    .parse_reply(reply.as_bytes())
                    .ok()?
                    .into_iter()
                    .map(owned)
                    .collect(),
            }
        } else {
            let id = decoded.trim();
            if !is_usable_uid(id) || !is_usable_id(id) {
                return None;
            }
            vec![ProviderEid {
                source: Cow::Owned(self.primary_source().as_str().to_owned()),
                inserter: None,
                matcher: None,
                mm: None,
                ext: None,
                uids: vec![ProviderUid {
                    id: Cow::Owned(id.to_owned()),
                    atype: Some(1),
                    ext: None,
                    expiry: bounded_expiry(None),
                    refresh: None,
                }],
            }]
        };
        (!eids.is_empty()).then_some(eids)
    }
}

impl IdentityProvider for Id5Provider {
    fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    fn build_request(
        &self,
        lookup: &LookupInput<'_>,
    ) -> Result<ProviderRequest, Report<ProviderError>> {
        let consent = lookup.consent;
        let origin = format!("https://{}", lookup.publisher_domain);
        let routable_ip = lookup.client.ip.filter(|ip| is_routable_ip(*ip));
        let body = FetchBody {
            partner: self.partner_id,
            provider: self
                .provider_name
                .as_deref()
                .unwrap_or(lookup.publisher_domain),
            domain: lookup.publisher_domain,
            origin: INTEGRATION_ORIGIN,
            v: INTEGRATION_VERSION,
            gdpr: u8::from(consent.gdpr_applies),
            gdpr_consent: consent.raw_tc_string.as_deref(),
            us_privacy: consent.raw_us_privacy.as_deref(),
            gpp_string: consent.raw_gpp_string.as_deref(),
            gpp_sid: consent.gpp_section_ids.as_deref().map(join_section_ids),
            ts: request_timestamp(lookup.now)?,
            ua: lookup.client.user_agent,
            ip: routable_ip.filter(IpAddr::is_ipv4).map(|ip| ip.to_string()),
            ipv6: routable_ip.filter(IpAddr::is_ipv6).map(|ip| ip.to_string()),
            s: lookup
                .current
                .iter()
                .flat_map(|(_, eid)| eid.uids())
                .find_map(|uid| uid.refresh())
                .map(|refresh| refresh.state.as_str()),
        };
        let body = serde_json::to_vec(&body).change_context(ProviderError::Request)?;
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            ORIGIN,
            HeaderValue::from_str(&origin).change_context(ProviderError::Request)?,
        );
        Ok(ProviderRequest {
            method: Method::POST,
            url: self.url.clone(),
            headers,
            body: Bytes::from(body),
            context: None,
        })
    }

    fn parse_response<'body>(
        &self,
        response: ProviderResponse<'body>,
    ) -> Result<LookupOutcome<'body>, Report<ProviderError>> {
        if response.status == http::StatusCode::NO_CONTENT {
            return Ok(LookupOutcome::NoMatch);
        }
        if !response.status.is_success() {
            return Err(Report::new(ProviderError::InvalidResponse)
                .attach(format!("ID5 returned status {}", response.status)));
        }
        if is_opt_out_reply(response.body) {
            return Ok(LookupOutcome::OptedOut);
        }
        let eids = self
            .parse_reply(response.body)
            .change_context(ProviderError::InvalidResponse)?;
        Ok(if eids.is_empty() {
            LookupOutcome::NoMatch
        } else {
            LookupOutcome::Found(eids)
        })
    }

    fn read_request_cookies<'cookie>(
        &self,
        cookies: RequestCookies<'cookie>,
    ) -> CookieRead<'cookie> {
        if self
            .cookies
            .any(cookies, |value| is_opt_out_cookie(value, self.partner_id))
        {
            return CookieRead::OptedOut;
        }
        CookieRead::from(self.cookies.read(cookies, |value| self.cookie_eids(value)))
    }
}

/// Whether one ID5 cookie value records an opt-out for `partner_id`: a bare
/// `0`, or a (possibly percent-encoded) reply whose `universal_uid` is `0`.
fn is_opt_out_cookie(value: &str, partner_id: u32) -> bool {
    let decoded = percent_decoded(value);
    let decoded = decoded.trim();
    if !decoded.starts_with('{') {
        return decoded == OPTED_OUT_UID;
    }
    partner_reply(Cow::Borrowed(decoded), partner_id)
        .is_some_and(|reply| is_opt_out_reply(reply.as_bytes()))
}

/// The fetch reply for `partner_id` in a JSON cookie value.
///
/// Prebid's shared cookie keeps each partner's reply under `pbjs`; it yields
/// this partner's entry, or `None` when it has none. A cookie without `pbjs`
/// is a single reply and is returned as is.
fn partner_reply(decoded: Cow<'_, str>, partner_id: u32) -> Option<Cow<'_, str>> {
    let selected = serde_json::from_str::<MultiplexedCookie<'_>>(&decoded)
        .ok()
        .and_then(|cookie| cookie.pbjs)
        .map(|partners| {
            partners
                .get(&partner_id.to_string())
                .map(|reply| reply.get().to_owned())
        });
    match selected {
        None => Some(decoded),
        Some(reply) => reply.map(Cow::Owned),
    }
}

/// Whether a fetch reply body, taken as is, has a `universal_uid` of `0`.
fn is_opt_out_reply(body: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|reply| {
            reply
                .get("universal_uid")
                .and_then(serde_json::Value::as_str)
                .map(|uid| uid == OPTED_OUT_UID)
        })
        .unwrap_or(false)
}

/// Copies an EID parsed from a temporary string so it outlives that string.
fn owned<'any>(eid: ProviderEid<'_>) -> ProviderEid<'any> {
    ProviderEid {
        source: Cow::Owned(eid.source.into_owned()),
        inserter: eid.inserter.map(|value| Cow::Owned(value.into_owned())),
        matcher: eid.matcher.map(|value| Cow::Owned(value.into_owned())),
        mm: eid.mm,
        ext: eid.ext.map(|ext| Cow::Owned(ext.into_owned())),
        uids: eid
            .uids
            .into_iter()
            .map(|uid| ProviderUid {
                id: Cow::Owned(uid.id.into_owned()),
                atype: uid.atype,
                ext: uid.ext.map(|ext| Cow::Owned(ext.into_owned())),
                expiry: uid.expiry,
                refresh: uid.refresh.map(|refresh| ProviderRefresh {
                    state: Cow::Owned(refresh.state.into_owned()),
                    from: refresh.from,
                }),
            })
            .collect(),
    }
}

/// When a UID that expires at `expiry` is refreshed: a tenth of its TTL
/// early, so the refresh runs while the UID and its signature are still
/// stored.
fn refresh_from(expiry: ProviderExpiry) -> ProviderExpiry {
    match expiry {
        ProviderExpiry::After(ttl) => ProviderExpiry::After(ttl - ttl / 10),
        other => other,
    }
}

/// Returns whether `id` is a real UID: not empty and not the opt-out UID.
fn is_usable_uid(id: &str) -> bool {
    !id.is_empty() && id != OPTED_OUT_UID
}

#[cfg(test)]
mod tests {
    use http::StatusCode;
    use serde_json::json;
    use trusted_server_core::consent::types::ConsentContext;
    use trusted_server_core::ec::eid_types::{AgentType, EidFields, Expiry, ProviderId, UidValue};
    use trusted_server_core::ec::kv_types::{KvEid, KvEntry, KvPartnerId, KvRefresh, KvUid};
    use trusted_server_core::ec::provider::{
        ClientSignals, OwnedEntries, ProviderExpiry, build_kind,
    };
    use trusted_server_core::settings::{IdentityProviderConfig, ProviderSecrets};

    use super::*;

    fn provider() -> Box<dyn IdentityProvider> {
        let config: IdentityProviderConfig = serde_json::from_value(json!({
            "id": "id5",
            "kind": "id5",
            "endpoint": "https://id5.example.com/gs/v2",
            "allowed_domains": ["id5.example.com"],
            "sources": ["id5.example.com", "id5-link.example.com"],
            "settings": { "partner_id": 1234 },
        }))
        .expect("should parse provider config");
        build_kind::<Id5>(&config, &ProviderSecrets::default()).expect("should build provider")
    }

    fn respond<'body>(
        provider: &dyn IdentityProvider,
        headers: &'body HeaderMap,
        body: &'body [u8],
    ) -> LookupOutcome<'body> {
        provider
            .parse_response(ProviderResponse {
                status: StatusCode::OK,
                headers,
                body,
                context: None,
            })
            .expect("should parse response")
    }

    #[test]
    fn keeps_the_reply_signature_as_refresh_state() {
        let provider = provider();
        let headers = HeaderMap::new();
        let body = json!({
            "universal_uid": "id5-example",
            "signature": "signature-example",
            "cache_control": { "max_age_sec": 36_000 },
        })
        .to_string();

        let outcome = respond(&*provider, &headers, body.as_bytes());

        let LookupOutcome::Found(eids) = outcome else {
            panic!("should find the ID5 ID");
        };
        let refresh = eids[0].uids[0]
            .refresh
            .as_ref()
            .expect("should keep the signature as refresh state");
        assert_eq!(
            refresh.state, "signature-example",
            "should store the signature"
        );
        assert!(
            matches!(refresh.from, ProviderExpiry::After(ttl) if ttl == Duration::from_secs(32_400)),
            "should refresh a tenth of the TTL before the UID expires"
        );
    }

    #[test]
    fn keeps_no_refresh_state_without_a_signature() {
        let provider = provider();
        let headers = HeaderMap::new();
        let body = json!({ "universal_uid": "id5-example", "signature": "" }).to_string();

        let outcome = respond(&*provider, &headers, body.as_bytes());

        let LookupOutcome::Found(eids) = outcome else {
            panic!("should find the ID5 ID");
        };
        assert!(
            eids[0].uids[0].refresh.is_none(),
            "should keep no refresh state for an empty signature"
        );
    }

    #[test]
    fn sends_the_stored_signature_as_s() {
        // Arrange
        let uid = KvUid::new(
            UidValue::try_from("id5-example".to_owned()).expect("should build uid"),
            AgentType::try_from(1).expect("should build atype"),
            None,
            Some(ProviderId::new("id5")),
            Expiry::At(UnixSeconds::new(1_800_000_000)),
        )
        .with_refresh(KvRefresh {
            state: "signature-example".to_owned(),
            from: UnixSeconds::new(1_700_000_000),
        });
        let record = KvPartnerId::from_eids(
            vec![KvEid::new(EidFields::default(), vec![uid])],
            UnixSeconds::new(1_700_000_000),
        )
        .expect("should keep the live uid");
        let mut entry = KvEntry::tombstone(1);
        entry.consent.ok = true;
        entry.ids.insert("id5.example.com".to_owned(), record);
        let sources = [SourceDomain::try_from("id5.example.com").expect("should build source")];
        let owner = ProviderId::new("id5");
        let consent = ConsentContext::default();
        let lookup = LookupInput {
            ec_id: "ec-example",
            consent: &consent,
            client: ClientSignals {
                ip: None,
                user_agent: None,
            },
            publisher_domain: "www.example.com",
            due_sources: &sources,
            current: OwnedEntries::new(Some(&entry), &sources, &owner),
            cookies: RequestCookies::default(),
            hashed_email: None,
            now: UnixSeconds::new(1_700_000_000),
        };

        // Act
        let request = provider()
            .build_request(&lookup)
            .expect("should build request");

        // Assert
        let body: serde_json::Value =
            serde_json::from_slice(&request.body).expect("should send JSON");
        assert_eq!(
            body.get("s"),
            Some(&json!("signature-example")),
            "should send the previous signature as s"
        );
    }

    #[test]
    fn posts_consent_and_network_signals_to_the_partner_url() {
        let provider = provider();
        let consent = ConsentContext {
            raw_us_privacy: Some("1YNN".to_owned()),
            gpp_section_ids: Some(vec![7, 8]),
            ..ConsentContext::default()
        };
        let lookup = LookupInput {
            ec_id: "ec-example",
            consent: &consent,
            client: ClientSignals {
                ip: Some("11.22.33.44".parse().expect("should parse ip")),
                user_agent: Some("ExampleBrowser/1.0"),
            },
            publisher_domain: "www.example.com",
            due_sources: &[],
            current: OwnedEntries::empty(),
            cookies: RequestCookies::default(),
            hashed_email: None,
            now: trusted_server_core::ec::eid_types::UnixSeconds::new(1_700_000_000),
        };

        let request = provider
            .build_request(&lookup)
            .expect("should build request");
        let body: serde_json::Value =
            serde_json::from_slice(&request.body).expect("should send JSON");

        assert_eq!(request.method, Method::POST, "should post");
        assert_eq!(
            request.url.as_str(),
            "https://id5.example.com/gs/v2/1234.json",
            "should call the partner's fetch URL"
        );
        assert_eq!(
            request.headers.get(ORIGIN).map(HeaderValue::as_bytes),
            Some(&b"https://www.example.com"[..]),
            "should send the publisher origin"
        );
        assert_eq!(body["partner"], 1234, "should send the partner");
        assert_eq!(
            body["provider"], "www.example.com",
            "should name the publisher as operator by default"
        );
        assert_eq!(
            body["domain"], "www.example.com",
            "should send the publisher domain"
        );
        assert_eq!(
            body["origin"], "trusted-server",
            "should name the integration"
        );
        assert_eq!(
            body["v"],
            env!("CARGO_PKG_VERSION"),
            "should send the integration version ID5 requires"
        );
        for legacy in ["o", "tml", "nbPage"] {
            assert!(body.get(legacy).is_none(), "should not send `{legacy}`");
        }
        let ts = body["ts"].as_str().expect("should send a ts string");
        DateTime::parse_from_rfc3339(ts).expect("should send ts as an RFC 3339 timestamp");
        assert_eq!(body["gdpr"], 0, "should send that GDPR does not apply");
        assert_eq!(body["us_privacy"], "1YNN", "should send US privacy");
        assert_eq!(
            body["gpp_sid"], "7,8",
            "should send GPP section IDs comma-joined"
        );
        assert_eq!(body["ip"], "11.22.33.44", "should send the client IP");
        assert_eq!(
            body["ua"], "ExampleBrowser/1.0",
            "should send the user agent"
        );
    }

    #[test]
    fn drops_a_private_client_ip() {
        let provider = provider();
        let consent = ConsentContext::default();
        let lookup = LookupInput {
            ec_id: "ec-example",
            consent: &consent,
            client: ClientSignals {
                ip: Some("10.1.2.3".parse().expect("should parse ip")),
                user_agent: None,
            },
            publisher_domain: "www.example.com",
            due_sources: &[],
            current: OwnedEntries::empty(),
            cookies: RequestCookies::default(),
            hashed_email: None,
            now: trusted_server_core::ec::eid_types::UnixSeconds::new(1_700_000_000),
        };

        let request = provider
            .build_request(&lookup)
            .expect("should build request");
        let body: serde_json::Value =
            serde_json::from_slice(&request.body).expect("should send JSON");

        assert!(body.get("ip").is_none(), "should not send a private IP");
    }

    #[test]
    fn sends_an_ipv6_address_as_ipv6() {
        let provider = provider();
        let consent = ConsentContext::default();
        let lookup = LookupInput {
            ec_id: "ec-example",
            consent: &consent,
            client: ClientSignals {
                ip: Some("2001:4860::1".parse().expect("should parse ip")),
                user_agent: None,
            },
            publisher_domain: "www.example.com",
            due_sources: &[],
            current: OwnedEntries::empty(),
            cookies: RequestCookies::default(),
            hashed_email: None,
            now: trusted_server_core::ec::eid_types::UnixSeconds::new(1_700_000_000),
        };

        let request = provider
            .build_request(&lookup)
            .expect("should build request");
        let body: serde_json::Value =
            serde_json::from_slice(&request.body).expect("should send JSON");

        assert!(body.get("ip").is_none(), "should not send IPv6 as ip");
        assert_eq!(body["ipv6"], "2001:4860::1", "should send ipv6");
    }

    #[test]
    fn parses_complete_eids_for_configured_sources() {
        let provider = provider();
        let headers = HeaderMap::new();
        let body = serde_json::to_vec(&json!({
            "ids": {
                "id5": { "eid": { "source": "id5.example.com", "uids": [{ "id": "id5-example", "atype": 1, "ext": { "linkType": 2 } }] } },
                "other": { "eid": { "source": "unknown.example.com", "uids": [{ "id": "x" }] } },
            },
            "cache_control": { "max_age_sec": 3600 },
        }))
        .expect("should encode body");

        let LookupOutcome::Found(eids) = respond(&*provider, &headers, &body) else {
            panic!("should find an EID");
        };

        assert_eq!(eids.len(), 1, "should drop unconfigured sources");
        assert_eq!(eids[0].source, "id5.example.com", "should keep the source");
        assert_eq!(eids[0].uids[0].id, "id5-example", "should keep the UID");
        assert!(eids[0].uids[0].ext.is_some(), "should keep the UID ext");
        assert!(
            matches!(eids[0].uids[0].expiry, ProviderExpiry::After(d) if d == Duration::from_secs(3600)),
            "should use the cache max age"
        );
    }

    #[test]
    fn falls_back_to_the_legacy_universal_uid() {
        let provider = provider();
        let headers = HeaderMap::new();
        let body = serde_json::to_vec(&json!({
            "universal_uid": "legacy-example",
            "ext": { "linkType": 2 },
            "ids": {
                "id5": { "eid": { "source": "id5.example.com", "uids": [{ "id": "0" }] } },
                "broken": { "eid": "not an object" },
            },
            "cache_control": "not an object",
        }))
        .expect("should encode body");

        let LookupOutcome::Found(eids) = respond(&*provider, &headers, &body) else {
            panic!("should find an EID");
        };

        assert_eq!(
            eids[0].source, "id5.example.com",
            "should use the first source"
        );
        assert_eq!(eids.len(), 1, "should drop the opted-out EID");
        assert_eq!(eids[0].uids[0].id, "legacy-example", "should keep the UID");
        assert_eq!(
            eids[0].uids[0].ext.as_deref().map(RawValue::get),
            Some(r#"{"linkType":2}"#),
            "should carry the reply linkType as UID ext"
        );
        assert!(
            matches!(eids[0].uids[0].expiry, ProviderExpiry::After(d) if d == crate::common::DEFAULT_TTL),
            "should use the default TTL without a cache max age"
        );
    }

    #[test]
    fn normalizes_sources_skips_bad_entries_and_bounds_expiry() {
        let provider = provider();
        let headers = HeaderMap::new();
        let body = serde_json::to_vec(&json!({
            "ids": {
                "a": { "eid": { "source": "ID5.Example.com.", "uids": [{ "id": "id5-example" }, { "id": "0" }] } },
                "b": { "eid": { "source": 7 } },
                "c": { "eid": { "source": "id5-link.example.com", "uids": [{ "id": "0" }] } },
            },
            "cache_control": { "max_age_sec": 0 },
        }))
        .expect("should encode body");

        let LookupOutcome::Found(eids) = respond(&*provider, &headers, &body) else {
            panic!("should find an EID");
        };

        assert_eq!(eids.len(), 1, "should skip bad and opted-out entries");
        assert_eq!(
            eids[0].source, "id5.example.com",
            "should store the configured source"
        );
        assert_eq!(eids[0].uids.len(), 1, "should drop the opt-out UID");
        assert_eq!(eids[0].uids[0].id, "id5-example", "should keep the UID");
        assert!(
            matches!(eids[0].uids[0].expiry, ProviderExpiry::After(d) if d == crate::common::MIN_TTL),
            "should keep a zero max age for the minimum TTL"
        );
    }

    #[test]
    fn reports_no_match_for_an_empty_reply() {
        let provider = provider();
        let headers = HeaderMap::new();
        let body = serde_json::to_vec(&json!({})).expect("should encode body");

        assert!(
            matches!(respond(&*provider, &headers, &body), LookupOutcome::NoMatch),
            "should report no match"
        );
    }

    #[test]
    fn reports_an_opt_out_reply() {
        let provider = provider();
        let headers = HeaderMap::new();
        let body = serde_json::to_vec(&json!({
            "universal_uid": "0",
            "cache_control": { "max_age_sec": 7200 },
        }))
        .expect("should encode body");

        let outcome = respond(&*provider, &headers, &body);

        assert!(
            matches!(outcome, LookupOutcome::OptedOut),
            "should report an opt-out for a universal_uid of 0"
        );
    }

    #[test]
    fn does_not_percent_decode_an_opt_out_reply() {
        let provider = provider();
        let headers = HeaderMap::new();
        let body = serde_json::to_vec(&json!({
            "universal_uid": "0",
            "ext": { "note": "a%22b" },
        }))
        .expect("should encode body");

        let outcome = respond(&*provider, &headers, &body);

        assert!(
            matches!(outcome, LookupOutcome::OptedOut),
            "should read the reply as sent"
        );
    }

    #[test]
    fn rejects_an_error_status_and_a_malformed_body() {
        let provider = provider();
        let headers = HeaderMap::new();

        for (status, body) in [
            (StatusCode::INTERNAL_SERVER_ERROR, &b"{}"[..]),
            (StatusCode::OK, b"not json"),
        ] {
            let error = provider
                .parse_response(ProviderResponse {
                    status,
                    headers: &headers,
                    body,
                    context: None,
                })
                .expect_err("should reject the response");
            assert!(
                matches!(error.current_context(), ProviderError::InvalidResponse),
                "should report an invalid response"
            );
        }
    }

    /// Source, UID, agent type, whether a UID `ext` is kept, and TTL.
    type ReadUid = (String, String, Option<i32>, bool, Duration);

    fn read(provider: &dyn IdentityProvider, header: &str) -> Option<Vec<ReadUid>> {
        provider
            .read_request_cookies(RequestCookies::new(header))
            .into_found()
            .map(|eids| {
                eids.into_iter()
                    .flat_map(|eid| {
                        let source = eid.source.into_owned();
                        eid.uids.into_iter().map(move |uid| {
                            let ProviderExpiry::After(ttl) = uid.expiry else {
                                panic!("should state a relative expiry");
                            };
                            (
                                source.clone(),
                                uid.id.into_owned(),
                                uid.atype,
                                uid.ext.is_some(),
                                ttl,
                            )
                        })
                    })
                    .collect()
            })
    }

    fn reply_cookie() -> serde_json::Value {
        json!({
            "universal_uid": "legacy-example",
            "signature": "signature-example",
            "ids": {
                "id5": { "eid": { "source": "id5.example.com", "uids": [{ "id": "id5-example", "atype": 1, "ext": { "linkType": 2 } }] } },
            },
            "cache_control": { "max_age_sec": 7200 },
        })
    }

    #[test]
    fn reads_a_plain_json_reply_cookie() {
        let provider = provider();
        let header = format!("other=1; id5id={}", reply_cookie());

        let uids = read(&*provider, &header).expect("should read the cookie");

        assert_eq!(
            uids,
            vec![(
                "id5.example.com".to_owned(),
                "id5-example".to_owned(),
                Some(1),
                true,
                Duration::from_secs(7200)
            )],
            "should parse the reply like a lookup response"
        );
    }

    #[test]
    fn reads_a_percent_encoded_json_reply_cookie() {
        let provider = provider();
        let header = format!("id5id={}", urlencoding::encode(&reply_cookie().to_string()));

        let uids = read(&*provider, &header).expect("should read the cookie");

        assert_eq!(
            uids,
            vec![(
                "id5.example.com".to_owned(),
                "id5-example".to_owned(),
                Some(1),
                true,
                Duration::from_secs(7200)
            )],
            "should decode the reply and keep ext objects"
        );
    }

    #[test]
    fn reads_a_bare_id_cookie_from_a_configured_name() {
        let config: IdentityProviderConfig = serde_json::from_value(json!({
            "id": "id5",
            "kind": "id5",
            "endpoint": "https://id5.example.com/gs/v2",
            "allowed_domains": ["id5.example.com"],
            "sources": ["id5.example.com"],
            "settings": { "partner_id": 1234, "cookie_names": ["example_lockr_id5", "id5id"] },
        }))
        .expect("should parse provider config");
        let provider =
            build_kind::<Id5>(&config, &ProviderSecrets::default()).expect("should build provider");

        let uids = read(&provider, "id5id=%7Bbroken; example_lockr_id5=bare-example")
            .expect("should read the bare ID");

        assert_eq!(
            uids,
            vec![(
                "id5.example.com".to_owned(),
                "bare-example".to_owned(),
                Some(1),
                false,
                crate::common::DEFAULT_TTL
            )],
            "should store the bare ID on the first source with the default TTL"
        );
    }

    #[test]
    fn reads_lockr_after_own_cookies_and_falls_back_to_the_lookup() {
        let config: IdentityProviderConfig = serde_json::from_value(json!({
            "id": "id5",
            "kind": "id5",
            "endpoint": "https://id5.example.com/gs/v2",
            "allowed_domains": ["id5.example.com"],
            "sources": ["id5.example.com"],
            "settings": {
                "partner_id": 1234,
                "lockr": { "enabled": true, "cookie_names": ["example_lockr_id5"] },
            },
        }))
        .expect("should parse provider config");
        let provider =
            build_kind::<Id5>(&config, &ProviderSecrets::default()).expect("should build provider");

        let own_first = read(
            &provider,
            "example_lockr_id5=lockr-example; id5id=own-example",
        );
        let lockr = read(
            &provider,
            "id5id=%7Bbroken; example_lockr_id5=lockr-example",
        );
        let neither = read(&provider, "example_lockr_id5=%7Bbroken");

        assert_eq!(
            own_first.map(|uids| uids[0].1.clone()).as_deref(),
            Some("own-example"),
            "should prefer the own cookie"
        );
        assert_eq!(
            lockr.map(|uids| uids[0].1.clone()).as_deref(),
            Some("lockr-example"),
            "should read the Lockr cookie when the own cookie yields nothing"
        );
        assert!(neither.is_none(), "should leave the ID to the lookup");
        assert!(
            provider
                .read_request_cookies(
                    RequestCookies::new("example_lockr_id5=lockr-example").where_gdpr_applies(true)
                )
                .into_found()
                .is_none(),
            "should not read Lockr cookies where GDPR applies, as Lockr AIM does not run there"
        );
    }

    #[test]
    fn reports_an_opt_out_cookie_so_the_lookup_is_skipped() {
        let provider = provider();
        let opted_out = json!({ "universal_uid": "0", "ids": {} }).to_string();

        for header in [
            "id5id=0".to_owned(),
            format!("id5id={opted_out}"),
            format!("id5id={}", urlencoding::encode(&opted_out)),
        ] {
            assert!(
                matches!(
                    provider.read_request_cookies(RequestCookies::new(&header)),
                    CookieRead::OptedOut
                ),
                "should report an opt-out from {header}"
            );
        }
    }

    /// A shared cookie holding partner 1234's reply and partner 5678's, with
    /// 5678's at the top level, as Prebid writes it.
    fn shared_cookie(partner_1234: &serde_json::Value) -> String {
        let other = json!({
            "universal_uid": "other-example",
            "ids": {
                "id5": { "eid": { "source": "id5.example.com", "uids": [{ "id": "other-partner-example", "atype": 1 }] } },
            },
        });
        let mut cookie = other.clone();
        cookie["pbjs"] = json!({ "1234": partner_1234, "5678": other });
        format!("id5id={}", urlencoding::encode(&cookie.to_string()))
    }

    #[test]
    fn reads_the_configured_partner_reply_from_a_shared_cookie() {
        let provider = provider();

        let uids = read(&*provider, &shared_cookie(&reply_cookie()))
            .expect("should read the partner's reply");

        assert_eq!(
            uids.iter().map(|uid| uid.1.as_str()).collect::<Vec<_>>(),
            vec!["id5-example"],
            "should read partner 1234's ID, not the top-level reply"
        );
    }

    #[test]
    fn ignores_a_shared_cookie_without_the_configured_partner() {
        let provider = provider();
        let cookie = json!({
            "universal_uid": "0",
            "ids": {},
            "pbjs": { "5678": { "universal_uid": "0", "ids": {} } },
        });
        let header = format!("id5id={cookie}");

        assert!(
            matches!(
                provider.read_request_cookies(RequestCookies::new(&header)),
                CookieRead::Absent
            ),
            "should neither read another partner's reply nor its opt-out"
        );
    }

    #[test]
    fn reports_the_configured_partner_opt_out_from_a_shared_cookie() {
        let provider = provider();
        let header = shared_cookie(&json!({ "universal_uid": "0", "ids": {} }));

        assert!(
            matches!(
                provider.read_request_cookies(RequestCookies::new(&header)),
                CookieRead::OptedOut
            ),
            "should report partner 1234's opt-out"
        );
    }

    #[test]
    fn ignores_malformed_cookies_so_the_lookup_runs() {
        let provider = provider();

        for header in ["id5id={not json", "other=bare-example"] {
            assert!(
                matches!(
                    provider.read_request_cookies(RequestCookies::new(header)),
                    CookieRead::Absent
                ),
                "should read nothing from {header}"
            );
        }
    }
}
