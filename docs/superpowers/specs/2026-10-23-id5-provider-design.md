# Native ID5 identity provider

Status: Draft, revision 1.

Issue: part of [#353](https://github.com/IABTechLab/trusted-server/issues/353).
Related: [#90](https://github.com/IABTechLab/trusted-server/issues/90) (ID5
support), [#1246](https://github.com/IABTechLab/trusted-server/issues/1246)
(Lockr IDs to KV), [#1245](https://github.com/IABTechLab/trusted-server/issues/1245)
(server-to-server identity), [#777](https://github.com/IABTechLab/trusted-server/issues/777)
(modules).

This spec implements one vendor on the
[server-side identity providers](2026-10-20-server-side-identity-providers-design.md)
framework (PR #1428). It is written against the framework's revision 12
(commit `b170c4413`), and "framework section 4.7" means section 4.7 of that
revision. A plain "(section 3.2)" is a section of this spec.

Code references follow the framework's conventions. Every `file:line` is to
`c84a73da7`. Core paths are written without `crates/trusted-server-core/src/`.

Vendor sources, and how this spec cites them:

- **Prebid.js 10.26.0**, read from
  `crates/trusted-server-js/lib/node_modules/prebid.js/modules/`. Cited as
  `id5IdSystem.js:<line>` and `id5IdSystem.md:<line>`.
- **ID5's Prebid Server module**, `io.id5.prebid.server.hooks.modules:id5-user-id`
  version `3.43-rc.1`. Its sources jar is published on Maven Central, and is
  cited by file name, such as `HttpFetchClient.java:100`. Its public guide
  is "ID5 User ID Module for Prebid Server Java" on `wiki.id5.io`, cited as
  "ID5 PBS guide".
- **ID5's in-app and CTV server-to-server guides** on `wiki.id5.io`
  (`identitycloud/retrieve-id5-ids/mobile-in-app-integration` and
  `en/identitycloud/retrieve-id5-ids/CTV-Integration`), cited as "ID5 in-app
  guide" and "ID5 CTV guide".
- **The IAB Global Vendor List**, version 182, fetched from
  `vendor-list.consensu.org/v3/vendor-list.json` on 2026-10-23.

ID5's web server-side fetch page
(`.../custom-integrations/server-side-fetch-endpoint`) needs a login, so this
spec could not read it. Where a fact rests only on that page, it is marked
**assumed**. Section 12 lists every assumed fact.

Terms used throughout:

- **ID5 ID**: the `id5-sync.com` UID ID5 shares with bidders. Its values
  start with `ID5*` (ID5 PBS guide).
- **Signature**: an opaque string ID5 returns with each ID. The caller stores
  it and sends it back on its next call. ID5 uses it to re-identify the
  visitor and to honour opt-outs (ID5 CTV guide).
- **`gs/v2`**: the server-side fetch endpoint the ID5 Prebid Server module
  calls (section 3.1).

## 1. Problem statement

Today TS gets an ID5 ID only from the browser. ID5's Prebid module or Lockr's
SDK resolves it, and the EID fidelity work forwards it (framework section
3.1). A visitor whose browser blocks Prebid.js, the ID5 script or ID5's hosts
gets no ID5 ID at all.

ID5's browser flow needs the browser twice over. The Prebid module calls a
config endpoint, then a fetch endpoint the config names, both with
`credentials: 'include'`, so ID5's own third-party cookie travels
(`id5IdSystem.js:392-397`, `:399-415`, `:433-448`). TrueLink also needs ID5's
bootstrap script on the page (`id5IdSystem.js:456`).

ID5 also runs a server-side endpoint. Its Prebid Server module calls it once
per auction with the device's IP and user agent and the consent strings, and
gets EIDs back (`HttpFetchClient.java:105-108`, `:119-144`). Nothing in that
call needs the browser. So TS can call ID5 from the edge on the writer
navigation, under the framework's coordinator, and serve the ID to bidders
from KV and `ts-eids-s`.

## 2. Goals and non-goals

### 2.1 Goals

- **I1.** An `id5` provider on the framework trait (framework sections 4.2
  to 4.6) that resolves an ID5 ID from `ClientIp` and `UserAgent` alone.
- **I2.** The signature is kept as private state, so later calls let ID5
  recognise the visitor and honour an opt-out.
- **I3.** A clear operator choice between Lockr capture and native ID5 for
  `id5-sync.com`, with what bidders see in each case and a migration path
  between them.
- **I4.** No new write path. Every ID5 effect goes through the writer's one
  flush (framework section 5.2). G4 and G12 hold unchanged.
- **I5.** Off by default, and kept off until the activation gates of section
  13 have passed on a live service.

### 2.2 Non-goals

- **TrueLink.** It needs ID5's bootstrap script in the page
  (`id5IdSystem.js:456`, `:471`). A server call has no TrueLink data to send,
  so v1 declares no TrueLink source (section 4.3).
- **Hashed emails and partner data.** ID5 accepts a `pd` string and, on its
  in-app endpoint, a `hem` (`id5IdSystem.js:489-491`; ID5 in-app guide). The
  framework has no HEM in v1 (framework section 2.2). They are additive
  later.
- **Segments, A/B testing and GAM targeting.** The Prebid module's
  `segments`, `abTesting` and `gamTargetingPrefix` are browser features
  (`id5IdSystem.js:104-123`, `:573-599`).
- **Other adapters.** KV identity runs only on Fastly (framework section 11).

## 3. ID5's server-side API

### 3.1 Endpoints

| Endpoint                                 | Use                                                   | Source                                    | Status                 |
| ---------------------------------------- | ----------------------------------------------------- | ----------------------------------------- | ---------------------- |
| `https://api.id5-sync.com/gs/v2`         | Server-side fetch, called by the Prebid Server module | ID5 PBS guide; `HttpFetchClient.java:100` | Verified               |
| `https://api.id5-sync.com/ga/v1`         | In-app server-to-server fetch                         | ID5 in-app guide                          | Verified, not for web  |
| `https://api.id5-sync.com/gc/v1`         | CTV server-to-server fetch                            | ID5 CTV guide                             | Verified, not for web  |
| `https://id5-sync.com/api/config/prebid` | Browser config call that names the fetch URL          | `id5IdSystem.js:39`, `:399-415`           | Verified, browser only |

The provider calls `gs/v2`, because it is the endpoint ID5 runs for a server
acting for web traffic. The request is a `POST` of a JSON body to
`<endpoint>/<partner>.json` (`HttpFetchClient.java:100`, `:105-108`). Whether
ID5 accepts calls to `gs/v2` from a caller other than Prebid Server is
**assumed** (section 12, question 1).

The browser config call is not used. It returns the fetch URL at runtime
(`id5IdSystem.js:434`), and the coordinator only calls hosts the descriptor
declares (framework section 4.7).

### 3.2 Request

The fields the Prebid Server module sends, from `FetchRequest.java:15-64` and
`HttpFetchClient.java:119-144`. Null fields are left out
(`HttpFetchClient.java:62`).

| Field                                                 | Meaning                                                   | Sent by this provider                          |
| ----------------------------------------------------- | --------------------------------------------------------- | ---------------------------------------------- |
| `partner`                                             | ID5 Partner Number                                        | Yes, from config                               |
| `ts`                                                  | Request time, ISO 8601 (`clock.instant().toString()`)     | Yes, from `input.now()`                        |
| `ipv4`, `ipv6`                                        | Device IP. `ipv4` has the deserialization alias `ip` only | Yes, one of them, from `ClientIp`              |
| `ua`                                                  | Device user agent                                         | Yes, from `UserAgent`                          |
| `domain`                                              | `site.domain`                                             | Yes, from `input.publisher_domain()`           |
| `ref`                                                 | `site.ref`, the referrer                                  | No. TS has no referrer input                   |
| `provider`                                            | The integrator name ID5 issues                            | Yes, when configured                           |
| `origin`                                              | Caller kind. PBS sends `pbs-java`                         | Yes, `trusted-server` (**assumed** value)      |
| `version`                                             | Caller version                                            | Yes, the TS crate version                      |
| `gdpr`                                                | `"1"` or `"0"`, a string                                  | Yes, from `gdpr_applies()`                     |
| `gdpr_consent`                                        | TC string                                                 | When present                                   |
| `us_privacy`                                          | US Privacy string                                         | When present                                   |
| `gpp_string`                                          | GPP string                                                | When present                                   |
| `gpp_sid`                                             | GPP section IDs, comma-separated                          | When present                                   |
| `coppa`                                               | COPPA flag                                                | No. A known child blocks the call (section 7)  |
| `maid`, `bundle`, `att`, `providerMetadata`, `_trace` | App and PBS fields                                        | No                                             |
| `signature`                                           | The stored signature                                      | When state holds one (**assumed** for `gs/v2`) |

The Prebid Server module sends no signature. It is stateless and calls ID5
on every auction. ID5's in-app and CTV endpoints take `signature` and ask the
caller to send it back (ID5 in-app guide, ID5 CTV guide). The browser fetch
sends it as `s` (`id5IdSystem.js:486-488`). That `gs/v2` takes it as
`signature` is **assumed**.

The in-app guide names the IPv4 field `ip`, and the PBS module sends
`ipv4`. This provider follows the PBS module, because it calls the same
endpoint. The field name is confirmed with ID5 before activation (section 12,
question 2).

The request carries no credential. The Partner Number in the path identifies
the caller (`HttpFetchClient.java:99-100`).

### 3.3 Response

| Field                              | Meaning                                                                               | Source                                                                 | Status                          |
| ---------------------------------- | ------------------------------------------------------------------------------------- | ---------------------------------------------------------------------- | ------------------------------- |
| `ids.<key>.eid`                    | One OpenRTB EID per key, such as `id5id` and `euid`. The PBS module forwards them all | `FetchResponse.java:10-19`; ID5 in-app guide                           | Verified                        |
| `ids.id5id.eid.uids[0]`            | `{id, atype: 1, ext: {linkType, pba}}`, source `id5-sync.com`                         | ID5 PBS guide; `id5IdSystem.md:84-123`                                 | Verified                        |
| `ids.euid.eid`                     | An EUID, `atype` 3, its source named in the reply                                     | `id5IdSystem.js:143-156`, `:246-252`; `id5IdSystem.md:110-121`, `:129` | Verified for Prebid             |
| `ids.id5-<provider>`               | An integrator-specific ID                                                             | ID5 in-app guide                                                       | Verified for in-app             |
| UID `"0"`                          | No consent, or an opt-out                                                             | `id5IdSystem.md:125`; ID5 in-app guide                                 | Verified                        |
| `signature`                        | Opaque state to send back                                                             | ID5 in-app guide, ID5 CTV guide                                        | **Assumed** for `gs/v2`         |
| `privacy.id5_consent`              | Whether ID5 found consent                                                             | ID5 in-app guide, ID5 CTV guide                                        | **Assumed** for `gs/v2`, unused |
| Error body `{code, message, type}` | Validation and authentication errors                                                  | ID5 in-app guide                                                       | Verified for in-app             |

The PBS module treats any status other than 200 as no ID
(`HttpFetchClient.java:210-215`). ID5 asks callers to ignore unknown response
fields (ID5 in-app guide), so the reply types in section 4.5 accept them.

No response field carries a refresh time or a maximum age (ID5 CTV guide).

### 3.4 Agreement, rate limits and latency

- **Agreement.** The ID5 ID is free but needs a registration and a Partner
  Number (`id5IdSystem.md:7`). A party deploying for publishers needs each
  publisher's signed ID5 agreement and a `provider` name ID5 confirms (ID5
  PBS guide). Whether a TS operator counts as such a party is **assumed**
  (section 12, question 1).
- **Rate limits.** None published. **Assumed** unlimited within fair use.
  The framework's burst cap and circuit breaker apply (framework section
  4.7).
- **Latency.** None published. ID5's only timing figure is the example
  `auctionDelay: 50` (`id5IdSystem.md:46`), a cap on how long Prebid waits
  for every ID module, not a measurement. ID5's own server-side integration
  never blocks the auction on ID5 (ID5 PBS guide).
- **Test partner.** ID5 documents Partner Number `173` for testing its
  in-app and CTV endpoints (ID5 in-app guide, ID5 CTV guide).

### 3.5 Lifetime and refresh

- **Web refresh.** ID5 "strongly recommends 2 hours between refreshes"
  (`id5IdSystem.md:70`).
- **Server-side refresh.** Eight hours, on a user event (ID5 in-app guide,
  ID5 CTV guide).
- **Deletion.** Server-side callers delete a stored ID after 30 days without
  a user event, a clock reset by every response (ID5 in-app guide, ID5 CTV
  guide). Web storage lasts 90 days (`id5IdSystem.md:42`), and ID5's GVL
  entry declares a cookie lifetime of 7,776,000 seconds, also 90 days.

That the 30-day server-side deletion rule applies to `gs/v2` is **assumed**.

## 4. Mapping onto the trait

### 4.1 Inputs

- **Needed: `ClientIp`, `UserAgent`.** Both are required body fields on
  every ID5 server endpoint (ID5 in-app guide, ID5 CTV guide), and the PBS
  module sends both (`HttpFetchClient.java:130-133`). Without them the
  provider is skipped (framework section 4.4).
- **Not declared: `PageUrl`.** The framework's section 12.4 sketch wants it.
  The verified `gs/v2` field set has no page URL, only `domain` and the
  referrer `ref` (section 3.2). `domain` comes from `publisher_domain()`,
  which is always available. Declaring less discloses less (framework section
  9.2). If ID5 confirms a page URL field, `PageUrl` becomes a wanted input,
  which is additive.
- **Not declared: `Region`, `HashedEmail`.** `gs/v2` takes neither.

### 4.2 Private state

The signature is the provider's private state, as framework section 4.6
shows for `exampleid`:

```rust
#[derive(Serialize, Deserialize)]
struct Id5State {
    signature: String,
}
```

- It lives in `ps.st` only, never in a cookie, never sent to bidders, and
  redacted by the admin lookup (framework section 6.1).
- The provider declares `with_private_state()`, so it runs only on the
  writer request and gets no dry run (framework sections 4.7, 5.2).
- A reply without `signature` keeps the stored one. A reply with one
  replaces it.
- The framework caps state at 1,024 bytes of JSON (framework section 4.5).
  The signature's size is not documented. A larger one would make every
  result's positive part fail, so no ID would ever be stored. The size is an
  activation gate (section 13).
- The state outlives the ID. When a record expires and is dropped (framework
  section 5.4, step 4), `ps.st` stays, so the next `Initial` call still sends
  the signature and ID5 can recognise the visitor.

### 4.3 Sources

| Source                   | Declared                              | Default `atype`          | Why                                                                                 |
| ------------------------ | ------------------------------------- | ------------------------ | ----------------------------------------------------------------------------------- |
| `id5-sync.com`           | Always. The primary source            | 1 (`id5IdSystem.js:136`) | The ID5 ID                                                                          |
| The EUID source          | Only when `euid_source` is configured | 3 (`id5IdSystem.js:150`) | ID5 returns an EUID only when ID5 enables it for the partner (`id5IdSystem.md:129`) |
| `true-link-id5-sync.com` | No                                    | 1 (`id5IdSystem.js:164`) | Needs the browser bootstrap (section 2.2)                                           |
| `id5-<provider>` keys    | No                                    |                          | Integrator-specific, not for bidders in general                                     |

The descriptor is built from config, so an operator registers an
`[[ec.partners]]` entry only for the sources it enables (framework section
10.2).

The EUID source string is the one ID5's reply names: the Prebid module reads
it from the response (`id5IdSystem.js:147-149`, `:248-249`). ID5's own
example names `uidapi.com` (`id5IdSystem.md:111`). If that is the value, it
collides with the UID2 provider and with Lockr, which both claim
`uidapi.com` (framework sections 12.2, 12.3), and validation refuses the
config until one of them is narrowed with `sources` (framework section
10.2). EUID token lifetimes are not documented, so `euid_source` also needs
`euid_expires_sec`, and EUID stays off until ID5 states the lifetime
(section 12, question 6).

The provider matches a reply's source against its declared sources with
`ProviderDescriptor::source_named` (framework section 4.3). It drops every
other EID, and keeps at most one ID per source, so the harness's "declared
sources only, one per source" check holds (framework section 13).

### 4.4 Results

- **`ext.linkType`.** The UID-level `ext` is kept as ID5 sends it,
  `{linkType, pba}` (ID5 PBS guide). The framework drops an `ext` over 256
  serialized bytes and keeps the ID (framework section 4.5). The size of
  `pba` is not documented, so when the whole `ext` does not fit, the provider
  keeps `{linkType}` alone. Bidders then get the link type without `pba`.
  Whether they need `pba` is section 12, question 7.
- **`atype`.** The reply's `uids[0].atype`, else the defaults of section
  4.3.
- **Refresh hint.** Every ID carries `with_refresh_in(2 h)`, ID5's web
  guidance (section 3.5). The operator's `refresh_after_sec` then applies
  only after a skipped call (framework section 8).
- **Expiry.** The provider marks its sources expiring (`with_expiring_ids`).
  The ID5 ID gets `with_expires_in(30 days)` (section 3.5). An EUID gets
  `euid_expires_sec`. A visitor who returns after the ID expired gets an
  `Initial` call, with the signature still in state (section 4.2).
- **`also_invalidate`.** When a reply returns the ID5 ID but not a declared
  EUID source, and the stored EUID record was written by `id5`, the provider
  invalidates it. It never invalidates a record another writer wrote. An
  invalidation sets `ps.n = now` (framework section 8), so the provider adds
  `with_retry_in(2 h)` and is not called again at once.
- **UID `"0"`.** ID5 says no consent or an opt-out. The coordinator has
  already applied TS's consent policy (section 7), so the most likely cause
  is an opt-out ID5 knows of and TS does not. The provider returns a sticky
  opt-out that keeps the signature: `Resolution::opted_out_keeping_state()`,
  with the reply's new signature when it has one. That constructor is new
  (section 11, F1). Clearing the signature would lose ID5's own record of the
  opt-out, which ID5 reads from it (ID5 CTV guide).
- **No ID5 ID at all.** `Resolution::no_match`, with the reply's signature
  when it has one. The stored ID stays served until it expires, because no
  error deletes a valid ID (framework section 8).
- **Errors.** A non-2xx status is `Rejected`, through `success_json`
  (framework section 4.5). The framework's backoff and breaker apply
  (framework section 8). The error body is never logged (section 9).

### 4.5 The provider

```rust
/// The ID5 source, as the Prebid module names it (`id5IdSystem.js:40`).
const ID5_SOURCE: &str = "id5-sync.com";
/// ID5 Technology SAS in the IAB Global Vendor List (`id5IdSystem.js:36`).
const ID5_GVL_ID: u16 = 131;
/// ID5's web refresh guidance (`id5IdSystem.md:70`).
const ID5_REFRESH: Duration = Duration::from_secs(7_200);
/// ID5's server-side deletion window (section 3.5). Assumed for `gs/v2`.
const ID5_EXPIRY: Duration = Duration::from_secs(30 * 86_400);
/// ID5's UID for no consent or an opt-out (`id5IdSystem.md:125`).
const ID5_NO_ID: &str = "0";
/// Sent as `origin`, as the PBS module sends `pbs-java`. Assumed value.
const ID5_ORIGIN: &str = "trusted-server";
/// The framework's UID-level `ext` cap (framework section 4.5).
const MAX_EXT_BYTES: usize = 256;

/// Settings in `[identity.id5]` (section 8).
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct Id5Config {
    /// The ID5 Partner Number.
    #[validate(range(min = 1))]
    pub partner_id: u64,
    /// The server-side fetch endpoint ID5 issues, without the partner path.
    #[validate(url)]
    pub endpoint: String,
    /// The integrator name ID5 issues, sent as `provider`.
    pub provider_name: Option<String>,
    /// The EID-level `inserter` for values this provider writes.
    pub inserter: Option<String>,
    /// The EUID source ID5's replies name, when ID5 enables EUID.
    pub euid_source: Option<String>,
    /// How long an EUID stays servable. Required with `euid_source`.
    pub euid_expires_sec: Option<u64>,
}

impl IntegrationConfig for Id5Config {
    /// The table is read only when `[identity]` selects `id5` (#777).
    fn is_enabled(&self) -> bool {
        true
    }
}

impl IdentityProviderConfig for Id5Config {
    fn build_descriptor(&self) -> Result<ProviderDescriptor, Report<IdentityProviderError>> {
        let mut sources: Vec<SourceDomain> = vec![ID5_SOURCE.parse()?];
        if let Some(euid) = &self.euid_source {
            if self.euid_expires_sec.is_none() {
                return Err(IdentityProviderError::config("`euid_source` needs `euid_expires_sec`"));
            }
            sources.push(euid.parse()?);
        }

        let mut builder = ProviderDescriptor::builder(ID5_INTEGRATION_ID.parse()?);
        for source in &sources {
            builder = builder.with_source(source.clone());
            if let Some(inserter) = &self.inserter {
                builder = builder.with_provenance(source, EidProvenance::new().with_inserter(inserter));
            }
        }
        builder
            .with_expiring_ids()
            .with_resolves(
                Resolves::new()
                    .with_endpoint(&self.endpoint)
                    .with_needs([Input::ClientIp, Input::UserAgent])
                    .with_private_state(),
            )
            .with_consent_policy(ConsentPolicy::strict().with_tcf_vendor(ID5_GVL_ID))
            .build()
    }
}

pub struct Id5Provider {
    descriptor: ProviderDescriptor,
    partner_id: u64,
    /// `<endpoint>/<partner>.json`, on the declared endpoint's host.
    fetch_url: Url,
    provider_name: Option<String>,
    euid_expiry: Option<Duration>,
}

#[derive(Serialize, Deserialize)]
struct Id5State {
    signature: String,
}

/// The `gs/v2` body (section 3.2). `None` fields are left out, as the PBS
/// module leaves out nulls.
#[derive(Serialize)]
struct FetchBody<'a> {
    partner: u64,
    ts: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    ipv4: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ipv6: Option<String>,
    ua: &'a str,
    domain: &'a str,
    origin: &'static str,
    version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<&'a str>,
    gdpr: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    gdpr_consent: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    us_privacy: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gpp_string: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gpp_sid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    signature: Option<String>,
}

/// ID5's reply, parsed permissively. `ids` is verified
/// (`FetchResponse.java:10-12`); `signature` is assumed for `gs/v2`.
#[derive(Deserialize)]
struct FetchReply {
    #[serde(default)]
    ids: BTreeMap<String, Id5Entry>,
    signature: Option<String>,
}

#[derive(Deserialize)]
struct Id5Entry {
    eid: Id5Eid,
}

#[derive(Deserialize)]
struct Id5Eid {
    source: String,
    #[serde(default)]
    uids: Vec<Id5Uid>,
}

#[derive(Deserialize)]
struct Id5Uid {
    id: String,
    atype: Option<i32>,
    ext: Option<serde_json::Value>,
}

impl Id5Provider {
    /// Builds the provider from its validated config.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::Config`] when the config cannot
    /// describe a valid provider or the endpoint cannot take the partner path.
    pub fn new(config: &Id5Config) -> Result<Self, Report<IdentityProviderError>> {
        let base = config.endpoint.trim_end_matches('/');
        let fetch_url = Url::parse(&format!("{base}/{}.json", config.partner_id))
            .map_err(|_| IdentityProviderError::config("`endpoint` cannot take the partner path"))?;
        Ok(Self {
            descriptor: config.build_descriptor()?,
            partner_id: config.partner_id,
            fetch_url,
            provider_name: config.provider_name.clone(),
            euid_expiry: config.euid_expires_sec.map(Duration::from_secs),
        })
    }

    fn resolved_id(&self, source: &SourceDomain, uid: &Id5Uid) -> ResolvedId {
        let is_id5 = source.as_str() == ID5_SOURCE;
        let (default_atype, expiry) = if is_id5 {
            (1, ID5_EXPIRY)
        } else {
            (3, self.euid_expiry.unwrap_or(ID5_EXPIRY))
        };
        let id = ResolvedId::new(source, uid.id.clone())
            .with_atype(uid.atype.unwrap_or(default_atype))
            .with_expires_in(expiry)
            .with_refresh_in(ID5_REFRESH);
        match uid.ext.as_ref().and_then(fit_ext) {
            Some(ext) => id.with_ext(ext),
            None => id,
        }
    }
}

/// Keeps ID5's UID-level `ext` when it fits the framework's cap, else only
/// `linkType`, so a large `pba` does not cost the link type.
fn fit_ext(ext: &serde_json::Value) -> Option<serde_json::Value> {
    let fits = serde_json::to_vec(ext).is_ok_and(|bytes| bytes.len() <= MAX_EXT_BYTES);
    if fits {
        return Some(ext.clone());
    }
    ext.get("linkType").map(|link_type| json!({ "linkType": link_type }))
}

/// `ts` as RFC 3339 in UTC, as the PBS module sends it.
fn timestamp(now: UnixTime) -> Result<String, Report<IdentityProviderError>> {
    i64::try_from(now.as_secs())
        .ok()
        .and_then(|secs| chrono::DateTime::from_timestamp(secs, 0))
        .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .ok_or_else(|| IdentityProviderError::config("time is out of range"))
}

impl IdentityProvider for Id5Provider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn request(
        &self,
        input: &ResolveInput<'_>,
    ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
        let consent = input.consent();
        let (ipv4, ipv6) = match input.client_ip()? {
            IpAddr::V4(ip) => (Some(ip.to_string()), None),
            IpAddr::V6(ip) => (None, Some(ip.to_string())),
        };
        let gpp_sid = consent
            .gpp_section_ids()
            .filter(|ids| !ids.is_empty())
            .map(|ids| ids.iter().map(u16::to_string).collect::<Vec<_>>().join(","));

        let body = FetchBody {
            partner: self.partner_id,
            ts: timestamp(input.now())?,
            ipv4,
            ipv6,
            ua: input.user_agent()?,
            domain: input.publisher_domain(),
            origin: ID5_ORIGIN,
            version: env!("CARGO_PKG_VERSION"),
            provider: self.provider_name.as_deref(),
            gdpr: if consent.gdpr_applies() { "1" } else { "0" },
            gdpr_consent: consent.raw_tc_string(),
            us_privacy: consent.raw_us_privacy(),
            gpp_string: consent.raw_gpp_string(),
            gpp_sid,
            signature: input.state::<Id5State>()?.map(|state| state.signature),
        };
        ProviderRequest::post_json(&self.fetch_url, &body).map(Some)
    }

    fn response(
        &self,
        response: &ProviderResponse,
        input: &ResolveInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let reply: FetchReply = response.success_json()?;
        let state = reply.signature.map(|signature| Id5State { signature });

        let mut ids: Vec<ResolvedId> = Vec::new();
        let mut opted_out = false;
        for entry in reply.ids.values() {
            let Some(source) = self.descriptor.source_named(&entry.eid.source) else {
                continue;
            };
            let Some(uid) = entry.eid.uids.first() else {
                continue;
            };
            if uid.id == ID5_NO_ID {
                opted_out |= source == input.primary_source();
                continue;
            }
            if ids.iter().any(|id| id.source() == source) {
                continue;
            }
            ids.push(self.resolved_id(source, uid));
        }

        let resolution = if opted_out {
            Resolution::opted_out_keeping_state()
        } else {
            let found_id5 = ids.iter().any(|id| id.source() == input.primary_source());
            let mut resolution = Resolution::found(ids);
            if found_id5 {
                for source in self.descriptor.sources() {
                    let returned = resolution.ids().iter().any(|id| id.source() == source);
                    let ours = input
                        .stored(source)
                        .is_some_and(|stored| stored.writer() == Some(self.descriptor.id()));
                    if !returned && ours {
                        resolution = resolution.also_invalidate(source).with_retry_in(ID5_REFRESH);
                    }
                }
            }
            resolution
        };

        Ok(match &state {
            Some(state) => resolution.replacing_state(state),
            None => resolution,
        })
    }
}
```

Registration, as framework section 4.6:

```rust
Ok(Some(
    IntegrationRegistration::builder(ID5_INTEGRATION_ID)
        .try_with_identity_provider(Id5Provider::new(&config))?
        .build(),
))
```

What the listing relies on:

- **No new dependency.** `chrono` is already a core dependency
  (`crates/trusted-server-core/Cargo.toml:20`), with `wasmbind` for the
  Cloudflare target (`:61-63`). `timestamp` never reads the clock itself; it
  formats `input.now()`.
- **No secrets.** The Partner Number is an operator identifier, as
  LiveRamp's placement ID is (framework section 12.1). The config has no
  `Redacted` field, so `secret_fields()` gains nothing.
- **One host.** `fetch_url` is built from the declared endpoint, so its host
  is the descriptor's host and passes the coordinator's check (framework
  section 4.7).
- **One crate.** Under #777 the provider lives in `crates/identity/id5`, the
  `crates/<feature>/<vendor>` layout #777 uses for Lockr. Before #777 lands it
  lives in `integrations/id5.rs`, as Lockr does today.
- **Assumed fields.** `signature` in the body and the reply, the `origin`
  value and the `ipv4` field name are assumed (section 12). Each is one line
  to change once ID5 confirms it.

## 5. Hold mode versus after-send

**Recommendation: `after_send`.**

ID5 keeps private state, so in either mode it runs only on the writer
navigation (framework section 4.7, step 8). The choice only decides whether
the navigation's own server-side auction waits for it.

Reasons for `after_send`:

- **No published latency.** ID5 publishes no response time for any endpoint
  (section 3.4). The framework's hold budget is 100 ms, shared by every
  hold-mode provider (framework section 4.7). Without a measured edge p95
  under that budget, holding risks cut-off calls that cost the ID anyway.
- **ID5 does not block either.** ID5's own server-side integration never
  holds the auction for ID5 (ID5 PBS guide).
- **Holding helps rarely.** The ID lasts 30 days and refreshes every two
  hours (section 4.4). While a refresh runs, the stored ID stays valid and
  is served. Holding helps only the first `Initial` call, on a visitor's
  second navigation (no provider runs on the first, framework section 4.7,
  step 8). After send, that ID reaches bidders from the third navigation.
- **Holding a stateful provider has a cost.** A stateful hold answer stays
  in the mutation and never enters the cookie, so a failed flush loses it
  (framework section 5.2, phase 3). A cut-off call has already sent the
  signature, and ID5 may have rotated it in the reply TS dropped (section 12,
  question 3).
- **Most auctions run on `/auction`.** A stateful provider never runs on
  `/auction` (framework section 5.3). On a site whose auctions all go
  through `/auction`, hold mode is after-send anyway.
- **Origin delay.** On a navigation auction, a due hold-mode provider delays
  bid dispatch, and in some modes the page's first byte (framework section
  4.7).

Settings: `mode = "after_send"`, `timeout_ms = 1000` (the after-send
default).

An operator may switch to `hold` once the coordinator's response-time logs
(framework section 4.7) show an edge p95 at or below 80 ms for a week, the
framework's own example timeout. That is activation gate A6 (section 13).

The framework's section 10.1 example shows ID5 in hold mode with
`timeout_ms = 80`. Section 11 asks revision 13 to change it.

## 6. Ownership against Lockr

Lockr's API traffic passes through TS's proxy, and Lockr returns an ID5 ID
the framework can capture (framework section 12.2). Lockr's ID5 EID has
`source: "id5-sync.com"`, UID `atype` 1, and EID-level
`inserter: "Lockr-For-Publishers"`, `matcher: ""` and `mm: 2` (#1246). The
same browser also held an independently cached ID5 SDK identity with a
different UID, and direct Rubicon requests carried Lockr's (#1246). Native
ID5 calls ID5 with the edge's view of IP and user agent and its own
signature, so its UID can differ from both. Only one provider may claim an
owned source (framework section 10.2).

### 6.1 The three choices

- **A. Lockr capture.** Lockr claims `id5-sync.com`, with `owner = "lockr"`
  or no owner. `id5` is not selected.
- **B. Native ID5.** `id5` claims and owns `id5-sync.com`. Lockr is not
  selected.
- **C. Native ID5 owns, Lockr narrowed.** `id5` owns `id5-sync.com`. Lockr
  stays selected for its other sources, with
  `sources = ["liveramp.com", "uidapi.com"]` (framework section 10.2). Its
  captured ID5 IDs are discarded as narrowed away (framework section 4.5).

Under #777's configuration rules, `identity` runs one module (`module`, a
string), and its one listed implementation is `lockr`. Choice C needs both
`lockr` and `id5` selected, so it needs `identity` to take `modules`, a list.
Section 11 lists that as a change to #777.

Native ID5 unowned is not offered. An unowned source is filled only when
absent and never refreshed (framework section 7.2), and ID5 needs refresh to
keep the ID and the signature current.

### 6.2 What bidders see

The server value is the one TS adds from KV or `ts-eids-s` (framework
section 7.1). The browser value is the one in the `/auction` body or
`ts-eids`, forwarded with its own EID-level fields.

| Case                                       | A. Lockr capture                                                                                      | B. Native ID5                                                 | C. Native ID5, Lockr narrowed                                                                          |
| ------------------------------------------ | ----------------------------------------------------------------------------------------------------- | ------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------ |
| Server value, UID                          | Lockr's captured UID                                                                                  | ID5's UID for the edge call                                   | As B                                                                                                   |
| Server value, `atype`                      | Lockr's, 1 (#1246)                                                                                    | ID5's `uids[0].atype`, else 1                                 | As B                                                                                                   |
| Server value, UID-level `ext`              | Lockr's, if any                                                                                       | `{linkType, pba}`, or `{linkType}` when trimmed (section 4.4) | As B                                                                                                   |
| Server value, EID-level fields             | `inserter: "Lockr-For-Publishers"`, `matcher: ""`, `mm: 2` (framework section 12.2 `with_provenance`) | `inserter` from config, else none. No `matcher` or `mm`       | As B                                                                                                   |
| Owner has no valid value, browser sent one | The browser's (framework section 7.1)                                                                 | The browser's, from ID5's Prebid module if the page runs it   | The browser's: Lockr's UID with Lockr's EID-level fields, still forwarded from Prebid's `ortb2` config |
| Client-side Prebid bidders                 | Lockr's UID                                                                                           | ID5 module's UID, if the page runs it                         | Lockr's UID. It can differ from the server-side value on the same page view                            |
| Visitor whose browser blocks the scripts   | Nothing, unless Lockr's proxied call ran                                                              | ID5's UID from the second navigation on                       | As B                                                                                                   |

Choice C's split is the cost to weigh. Server-side bidders get ID5's own UID
and client-side bidders keep Lockr's. A bidder that sees both on one page
view sees two ID5 IDs for one visitor.

Native ID5 declares no `matcher` or `mm`. ID5 documents those fields for
enriched IDs only (ID5 CTV guide), and the provider does not invent them.
The `inserter` follows ID5's PBS guide: "canonical domain of the entity
adding the EID", such as the publisher's domain. Its value is decision D9.

### 6.3 Migration

Every switch is a config change and one deploy. Validation refuses any
config where two enabled providers claim `id5-sync.com` with an owner set
(framework section 10.2), so no intermediate state has two writers.

**Pull sync first.** If `id5-sync.com` has `pull_sync_enabled = true`, its
generic pull provider claims the source and blocks any owner (framework
sections 10.4, 14). Set `pull_sync_enabled = false` in the same or an
earlier deploy.

**A to B or C.**

1. Select `id5`, set `owner = "id5"` on the `id5-sync.com` partner entry,
   and either deselect Lockr (B) or narrow it with `sources` (C).
2. The identity config's fingerprint `cf` changes, so every `ts-eids-s`
   drops its positive entries and is rebuilt from KV. Pending markers and
   `ts-eids-r` removals are kept (framework section 5.6).
3. Existing records are treated by writer:

   | Existing record                       | Served after the switch                                   | What replaces it                                                             |
   | ------------------------------------- | --------------------------------------------------------- | ---------------------------------------------------------------------------- |
   | `w: lockr`, Lockr deselected (B)      | No: writer not configured (framework section 7.1)         | ID5's `Initial`, which treats it as absent                                   |
   | `w: lockr`, Lockr narrowed (C)        | No, under proposed change F2 (section 11)                 | ID5's `Refresh`: a record another provider wrote is due at once              |
   | `w: pull-id5-sync-com`, pull disabled | No: writer not configured (framework section 7.1)         | ID5's `Initial`                                                              |
   | No `w` (browser ingest or legacy)     | No: ID5 marks its source expiring (framework section 6.1) | ID5's `Refresh`; `OwnerWrite` is never blocked by it (framework section 5.4) |

   Until ID5's first call for a visitor, the server set lacks the source,
   and bidders get the browser's value when the browser sends one
   (framework section 7.1). On the first eligible writer navigation, ID5 is
   called after send, and its `OwnerWrite` lands in that navigation's one
   flush.

4. Batch sync to `id5-sync.com` then applies only when the source is absent
   (framework section 7.2).

**B or C to A.**

1. Deselect `id5`, set `owner = "lockr"` or remove the owner, and remove
   Lockr's narrowing.
2. Records with `w: id5` are no longer served, and count as absent, so
   Lockr's capture or the browser fills the source (framework section 7.1).
3. The next write of each row drops ID5's `ps` entry (framework section 5.4,
   step 4), with its signature, `ps.iv` and any sticky `ps.o`. A visitor who
   had opted out through ID5 is called again if `id5` is re-selected later.
   Proposed change F3 (section 11) keeps the opt-out.

**B to C, and C to B.** Selecting or deselecting a narrowed Lockr changes
nothing for ID5's records, because a narrowed Lockr never writes
`id5-sync.com`. Un-narrowing Lockr while `id5` owns the source is refused by
validation (framework section 10.2), so the two never write it together.

Disabling `id5` is also the kill switch. Its records stop reaching bidders on
the next request, with no write, because a disabled writer's records are
never served (framework section 7.1).

### 6.4 Recommendation

- **Sites with Lockr: choice A.** Lockr's traffic is first-party through
  TS's proxy, so Safari's blocking of vendor hosts does not stop it, and
  client-side and server-side bidders see one ID5 UID.
- **Sites without Lockr: choice B.**
- **Choice C** only after a measured comparison on the site: the share of
  auctions with an `id5-sync.com` EID under A and under C, and bidder
  response to each. It is worth it only when visitors who block the Lockr
  SDK are a large share. It also needs the #777 change in section 11.

## 7. Consent policy

`ConsentPolicy::strict().with_tcf_vendor(131)`, with no relaxation.

- **GVL ID 131.** The IAB Global Vendor List, version 182, lists vendor 131
  as "ID5 Technology SAS". ID5's Prebid module uses the same ID
  (`id5IdSystem.js:36`).
- **TCF purposes.** Where TCF applies (framework section 9.1), `strict()`
  requires Purposes 1 and 4. ID5's GVL entry declares Purposes 1, 3, 5 and
  10 under consent, none under legitimate interest, Special Purposes 1 and 3,
  and Features 1, 2 and 3. ID5's own Prebid check requires Purpose 1 and
  vendor consent before it stores anything (`id5IdSystem.js:606-611`).
  Purpose 4 is TS's floor for any ID sent to bidders. Whether TS should also
  require Purposes 3 and 5 is decision D8.
- **Vendor consent.** Vendor 131 must have consent. ID5 declares no
  legitimate-interest purpose, so there is no legitimate-interest path.
- **US signals.** `strict()` blocks every US opt-out TS decodes (sale,
  sharing, targeted advertising), GPC, DNT and a known child (framework
  sections 4.3, 9.1). ID5's own module forwards US Privacy and GPP and lets
  ID5 decide (`id5IdSystem.js:478-484`). No ID5 document gives a legal basis
  to relax any of them, and the framework requires one (framework section
  4.3). The raw strings are still forwarded, so ID5 applies its own reading
  as well.
- **What ID5 receives.** The IP and user agent, the publisher domain, the
  raw consent strings and the signature. ID5's PBS guide asks the publisher's
  privacy policy and consent setup to cover that sharing. That is on the
  rollout checklist (section 13).
- **Sticky opt-out.** A UID `"0"` sets `ps.o` (section 4.4). ID5 has a GVL
  ID, so a TC string whose `LastUpdated` is after the opt-out and that grants
  vendor 131 and the policy's purposes clears it, as does a GPP section
  change the fingerprint shows (framework section 8). An operator can clear
  it through the admin endpoints.
- **Disclosure.** Each served ID5 value passes this policy on the request
  that serves it, in the auction and in `/_ts/api/v1/identify` (framework
  section 7.1).

## 8. Configuration

Module settings follow #777's rule: `[<type>.<name>]` holds the settings and
the type's selector chooses what runs (PR #1348,
`docs/guide/configuration-rules.md`). Coordinator settings stay in
`[ec.identity.providers.id5]` (framework section 10.5). Hosts and IDs below
are examples.

```toml
[identity]
module = "id5"

[identity.id5]
partner_id = 1234
# Required. The fetch endpoint ID5 issues, without the partner path.
endpoint = "https://id5.example.com/gs/v2"
# Optional. The integrator name ID5 issues, sent as `provider`.
provider_name = "example-provider"
# Optional. EID-level inserter on the values this provider writes.
inserter = "example.com"
# Optional, and off until ID5 states EUID lifetimes (section 4.3).
# euid_source = "euid.example.com"
# euid_expires_sec = 86400

[ec.identity.providers.id5]
mode = "after_send"
timeout_ms = 1000
# Used only after a skipped call. Each ID carries a 2-hour hint.
refresh_after_sec = 7200
max_calls_per_minute = 1

[[ec.partners]]
name = "ID5"
source_domain = "id5-sync.com"
openrtb_atype = 1
bidstream_enabled = true
owner = "id5"
```

`endpoint` has no default. ID5's PBS guide names
`https://api.id5-sync.com/gs/v2`, and the operator sets the value ID5
confirms for TS (section 12, question 1).

Choice C adds Lockr with its narrowing, which needs the #777 change of
section 11:

```toml
[identity]
modules = ["lockr", "id5"]

[ec.identity.providers.lockr]
capture_enabled = true
sources = ["liveramp.com", "uidapi.com"]
```

On `main`, before #777 lands, the same settings sit in `[integrations.id5]`
with `enabled = true`, and `Id5Config` gains the `enabled` field that
framework section 4.6 shows.

Validation (framework section 10.2) adds nothing ID5-specific beyond
`build_descriptor`'s errors: an `euid_source` without `euid_expires_sec`, an
endpoint that is not `https`, and a source that does not normalize.

## 9. Logging and data handling

- Never logged: the UID, the signature, the request body, the reply body,
  the consent strings, the client IP and the user agent (framework section
  9.3). The error body is never logged either, because ID5's `message` can
  echo input.
- Logged: the provider ID, the outcome, the HTTP status, the response time
  and, for gate A3, the signature's length in bytes.
- `FetchBody`, `FetchReply` and `Id5State` get no `Debug` derive, so no
  `{:?}` can print them.
- ID5's PBS module logs the request body at debug (`HttpFetchClient.java:102-103`).
  This provider does not copy that.

## 10. Invariants

- **G4.** No new write. ID5 runs only on the writer navigation (framework
  section 4.7, step 8), and its result joins that navigation's one flush
  (framework section 5.2, phase 4). No exception is used or proposed.
- **G12.** ID5's removals are `also_invalidate`, by revision, and the sticky
  opt-out, which removes every record `id5` wrote (framework section 5.4).
  F1 changes only what happens to `ps.st` on an opt-out. Removals still apply
  first and are never skipped.
- **G5.** ID5's values are cached in `ts-eids-s` as an owned source once in
  KV (framework section 6.2). A stateful answer reaches the cookie only
  through a KV read (framework section 5.2, phase 3).
- **Calls.** At most one ID5 call per visitor per minute (the burst cap),
  and normally one per two hours of activity (section 4.4). Each refresh
  costs at most that navigation's one KV write.

## 11. Proposed framework changes

Each change is additive under framework section 4.2. Revision 13 folds them
in. This spec does not edit the framework.

**F1. `Resolution::opted_out_keeping_state`** (framework sections 4.5, 8).
Row of framework section 4.2: a new outcome, a new `Resolution` constructor.

```rust
impl Resolution {
    /// The provider reports the user opted out, and its private state is how
    /// the vendor recognises that opt-out on later calls. As
    /// [`opted_out`](Self::opted_out), except that `ps.st` is kept, or
    /// replaced when the resolution also says `replacing_state`. From a
    /// provider not declared `with_private_state()`, it is `opted_out`, with
    /// a warning, and the harness fails the test.
    pub fn opted_out_keeping_state() -> Self;
}
```

Section 8 table row: `Resolution::opted_out_keeping_state` | `WriterRemove`
for every record this provider wrote, owned or unowned | `o = {at: now, fp}`;
`st` kept, or replaced when the resolution says so | info. The opt-out
applies with the other removals. Its state replacement applies only when the
live `ps.st` still hashes to the call's `expected_state`; otherwise the live
state is kept and the opt-out still applies (framework section 5.4, step 3).
`is_opted_out()` returns true for it.

Test: "an `opted_out_keeping_state` result removes every record the provider
wrote, sets `ps.o`, and keeps `ps.st`, or stores the replacement state when
the result carries one; the next call after a re-grant sends the kept state".

**F2. A writer that no longer claims a source** (framework sections 4.7,
step 4, and 7.1). Section 7.1's rule for disabled writers extends to a writer
narrowed away:

> A record whose writer is disabled, no longer configured, or no longer
> claims the record's source after `sources` narrowing has no policy left to
> check for that source, so it is never served, and it counts as absent for
> `FillMissing` and for unowned `Initial`.

Section 4.7, step 4's "An expired record, or one whose writer is disabled or
no longer configured, counts as absent" gains the same clause. The flush
check of section 5.3 already drops staged entries of such a writer.

Test: "after Lockr is narrowed away from `id5-sync.com`, a record with
`w: lockr` for that source is not sent to bidders or returned by
`/identify`, the browser's value is sent instead, and the owner's next call
replaces the record".

**F3. Opt-outs outlive a provider's removal** (framework sections 5.4,
step 4, and 6.1). Today a write drops the `ps` entries of providers no longer
configured, opt-outs included. Re-enabling the provider then calls visitors
who opted out. New rule and key:

```toml
[ec.identity]
# Seconds a provider's sticky opt-out is kept after the provider stops being
# configured. Default 31536000.
retired_opt_out_sec = 31536000
```

> When a provider is no longer configured, a write keeps its `ps.o`, its
> `ps.oc`, and the `ps.st` of an `opted_out_keeping_state` result, and drops
> its other `ps` fields. The entry is dropped once `ps.o.at` is older than
> `retired_opt_out_sec`. Retired entries count toward the 32-entry `ps` cap;
> when the cap would be exceeded, the oldest retired entries go first.

Test: "a provider removed from config and re-added within
`retired_opt_out_sec` is not dispatched for a visitor whose opt-out predates
the removal, and its kept state is sent after a re-grant".

**F4. Document corrections.** No API change.

- Section 10.1's example: `[ec.identity.providers.id5]` with
  `mode = "after_send"`, `timeout_ms = 1000` and `refresh_after_sec = 7200`,
  and its vendor table as `[identity.id5]` under #777.
- Section 12.4: ID5 needs `ClientIp` and `UserAgent` and wants nothing,
  because `gs/v2` has no page URL field. It declares `id5-sync.com` always,
  the EUID source from config, and no TrueLink source. A UID `"0"` maps to
  `opted_out_keeping_state`. Cite this spec.
- Section 10.5: under #777, `identity` in `[ec.identity]` is one of `[ec]`'s
  own settings, like `ec_store` and the partner registry, so the rule that
  `[ec.<name>]` configures an `ec` module named `<name>` does not apply to it
  (PR #1348, `docs/guide/configuration-rules.md`).

**Change to #777, not the framework.** `identity` runs several modules, with
`modules`, a list, so Lockr and native providers can run side by side with
`sources` narrowing (section 6.1). Test: "`[identity] modules = ["lockr",
"id5"]` validates when Lockr is narrowed away from the source `id5` owns, and
is refused when it is not".

## 12. Open questions

Each assumed fact is a question for ID5.

1. **May TS call `gs/v2`?** Whether ID5 permits an edge server other than
   Prebid Server to call `gs/v2` for a publisher's Partner Number, under
   which agreement, and with which `provider` and `origin` values. If not,
   which endpoint TS should call.
2. **Field names.** Whether `gs/v2` reads the IPv4 address as `ipv4`, as the
   PBS module sends it, or as `ip`, as the in-app guide names it.
3. **Signature.** Whether `gs/v2` takes `signature` in the body and returns
   one, its maximum size (the framework stores at most 1,024 bytes of JSON),
   and whether an older signature is still accepted after ID5 has issued a
   newer one in a reply TS never received.
4. **UID `"0"`.** Whether a `"0"` from `gs/v2` can mean anything other than
   no consent or an opt-out, and whether `privacy.id5_consent` is returned.
   If ID5 returns `"0"` for a transient reason, the sticky opt-out of section
   4.4 is too strong, and `invalidate` with a retry is the fallback.
5. **Lifetime.** Whether the 30-day deletion and 8-hour refresh of the
   in-app and CTV guides apply to web traffic on `gs/v2`, or the 90-day and
   2-hour figures of the Prebid module.
6. **EUID.** The source string ID5 returns for EUID on `gs/v2`, and the EUID
   token's lifetime.
7. **`pba`.** What `pba` is, its typical size, and whether bidders need it,
   since the framework caps the UID-level `ext` at 256 bytes.
8. **Rate limits and latency.** Any per-partner rate limit, and ID5's
   expected latency from Fastly POPs.
9. **TCF purposes.** Which purposes `gs/v2` requires before it returns a
   real ID. If it needs Purposes 3 and 5, TS's policy should require them
   too, so TS does not call only to get `"0"` and a sticky opt-out.
10. **One identity per visitor.** Whether ID5 can link the edge call's ID to
    the ID its browser module or Lockr already holds for the visitor, for
    example by a field that carries the browser's ID5 UID. `gs/v2` has none
    in the verified field set.

## 13. Rollout

The provider ships unselected. It runs only once an operator selects it, and
an operator selects it only after these gates pass, in order:

- **A1. Framework.** Framework rollout steps 1 and 2 are live: the one-write
  flush, providers, private state, the breaker (framework section 14), and
  F1. F2 and F3 are needed before choice C or before an operator disables
  `id5` on a service where it has run.
- **A2. ID5 approval.** ID5 answers questions 1 to 4 of section 12 in
  writing, and the assumed constants and field names of section 4.5 are
  corrected to match.
- **A3. Staging.** On a staging service with ID5's test partner, recorded
  reply shapes replace the harness fixtures' assumed shapes, with every
  value replaced by example data. The logged signature length is under
  1,024 bytes with margin.
- **A4. Privacy.** The publisher's privacy policy and CMP cover sharing the
  IP, user agent and consent strings with ID5 (ID5 PBS guide), and vendor
  131 is in the CMP's vendor list.
- **A5. Live acceptance.** On one production service, choice B, or C on a
  test hostname, `mode = "after_send"`:
  - After two navigations from a consenting browser, the admin EC lookup
    (`/_ts/admin/ec`, `ec/admin.rs:276`) shows `id5-sync.com` with
    `w: id5`, an `exp`, and a redacted `ps.st`.
  - The next `/auction` carries the ID5 EID in `x-ts-eids` with the
    configured `inserter`.
  - A browser with vendor 131 denied gets no ID5 call (a debug skip line)
    and no server-side ID5 value.
  - A browser that blocks Prebid.js and ID5's hosts gets an ID5 value from
    its second navigation.
  - The logs hold no UID, signature or consent string.
  - The KV write count per page load stays at one or zero.
    If #777 has removed `/_ts/admin` by then, its replacement lookup is used.
- **A6. Hold mode**, optional: the coordinator's response-time logs show an
  edge p95 at or below 80 ms for a week (section 5).
- **A7. Choice C**, optional: the measured comparison of section 6.4, and
  the #777 change of section 11.

Rollback is deselecting `id5`. Its records stop being served at once
(section 6.3), and no write is needed.

## 14. Test plan

**Provider harness** (framework section 13). Fixtures use example values
only, such as `ID5*example` and a `192.0.2.0/24` address.

```rust
let provider = Id5Provider::new(&id5_config()).expect("should build the provider");
let mut harness = ProviderHarness::new(&provider)
    .with_client_ip("192.0.2.10")
    .with_user_agent("ExampleBrowser/1.0")
    .with_state(&json!({ "signature": "example-signature" }));

let request = harness
    .request()
    .expect("should build a request")
    .expect("should not skip the request");
assert_eq!(
    request.url().path(),
    "/gs/v2/1234.json",
    "should post to the partner path"
);
let body: serde_json::Value = harness.request_json();
assert_eq!(body["ipv4"], "192.0.2.10", "should send the IPv4 address as `ipv4`");
assert_eq!(body["signature"], "example-signature", "should send the stored signature");

let resolution = harness
    .respond(
        StatusCode::OK,
        &json!({
            "ids": { "id5id": { "eid": {
                "source": "id5-sync.com",
                "uids": [{ "id": "ID5*example", "atype": 1, "ext": { "linkType": 2 } }]
            } } },
            "signature": "example-signature-2"
        }),
    )
    .expect("should parse the response");
assert_eq!(resolution.ids()[0].uid(), "ID5*example", "should return the ID5 ID");
```

Request cases:

- POST to `<endpoint>/<partner>.json` on the declared host, a JSON body, no
  `Cookie` header.
- An IPv6 client sends `ipv6` and no `ipv4`.
- `gdpr` is `"1"` or `"0"`. `gdpr_consent`, `us_privacy`, `gpp_string` and
  `gpp_sid` appear only when present, and `gpp_sid` is comma-joined.
- No `signature` on the first call; the stored one after.
- `ts` is RFC 3339 UTC from the harness clock.
- No `PageUrl`, `Region` or `HashedEmail` read is recorded.
- A missing user agent skips the call (`InputUnavailable`).

Response cases:

- The ID5 ID with `atype` 1, `ext.linkType`, a 30-day expiry and a 2-hour
  refresh hint, and the new signature as state.
- A reply without `signature` keeps the old state (`StateChange::Keep`).
- An `ext` over 256 bytes is trimmed to `{linkType}`.
- An `id5-<provider>` key and any undeclared source are dropped.
- With `euid_source` set: the EUID with `atype` 3 and `euid_expires_sec`.
  A reply with the ID5 ID and no EUID invalidates a stored EUID record
  `id5` wrote, with a 2-hour retry, and leaves one another writer wrote.
- UID `"0"` for `id5-sync.com` gives `opted_out_keeping_state` with the new
  signature (F1).
- An empty `ids` gives `no_match` and keeps the stored ID.
- A 400 is `Rejected` and a 500 feeds the breaker (framework section 8).
- A body that is not JSON is `Malformed`.

Conformance: `harness.check_conformance_with_state::<Id5State>(&cases)` with
the request and response cases above. The provider is stateful, so plain
`check_conformance` would fail with a pointer to it (framework section 13).
The config has no secrets, so `check_descriptor_ignores_secrets` is not
needed.

**Descriptor and config.**

- Sources follow config: `id5-sync.com` alone, or with the EUID source.
- `euid_source` without `euid_expires_sec` is a `Config` error.
- With `owner = "id5"` on `id5-sync.com` and Lockr selected unnarrowed,
  validation fails. With Lockr narrowed, it passes.
- An EUID source of `uidapi.com` beside an enabled UID2 provider fails
  validation until one is narrowed.
- A `pull_sync_enabled` partner for `id5-sync.com` beside `owner = "id5"`
  fails validation.

**Coordinator and storage**, beside the framework's own tests:

- ID5 is never dispatched on `/auction`, `/_ts/page-bids` or a new visitor's
  first navigation, and gets no dry run.
- A returning visitor with ID5 due: one KV write after send, holding the ID,
  its `w`, `rv`, `exp`, `ra` and the signature in `ps.st`.
- A refresh that returns the same UID writes a new `rv` and `at`.
- A record past its 30-day `exp` is dropped, `ps.st` stays, and the next
  call is `Initial` with the signature.
- Migration (section 6.3): after A to C, a `w: lockr` record is not served
  (F2) and the browser's Lockr value is; after the first ID5 call, ID5's
  value is. After C to A, `w: id5` records are not served.
- F3: removing and re-adding `id5` keeps an opted-out visitor uncalled.

**Logging.** A test logger captures every line from a full call, success
and error, and asserts that no fixture UID, signature, consent string,
address or user agent appears.

## 15. Decisions to confirm

| ID  | Decision             | Proposed                                                                                                                |
| --- | -------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| D1  | Endpoint             | `gs/v2`, the endpoint ID5 runs for Prebid Server, pending ID5's approval (section 12, question 1)                       |
| D2  | Mode                 | `after_send`. `hold` only after gate A6                                                                                 |
| D3  | `PageUrl`            | Not declared. `domain` from the publisher domain                                                                        |
| D4  | Sources              | `id5-sync.com` always; EUID opt-in through config; no TrueLink in v1                                                    |
| D5  | UID `"0"`            | A sticky opt-out that keeps the signature (F1). Fallback: `invalidate` with a retry, if ID5 says `"0"` can be transient |
| D6  | Refresh              | A 2-hour hint on every ID, ID5's web guidance. Each refresh can cost that navigation's one KV write                     |
| D7  | Expiry               | 30 days, ID5's server-side deletion window, refreshed by every response                                                 |
| D8  | TCF purposes         | `strict()` (Purposes 1 and 4) plus vendor 131. Not Purposes 3 and 5 unless ID5 says `gs/v2` needs them                  |
| D9  | EID-level provenance | `inserter` from config, such as the publisher's domain. No `matcher` or `mm`                                            |
| D10 | Ownership            | Lockr sites: A. Other sites: B. C only after a measured comparison and the #777 change                                  |
| D11 | Large `ext`          | Trim to `{linkType}` rather than drop it                                                                                |
| D12 | IP field             | `ipv4` or `ipv6`, as the PBS module sends them, pending ID5's answer                                                    |
| D13 | Framework changes    | F1 (keep state on opt-out), F2 (narrowed writers not served), F3 (opt-outs outlive removal), F4 (docs)                  |
| D14 | #777                 | `identity` takes `modules`, a list                                                                                      |
