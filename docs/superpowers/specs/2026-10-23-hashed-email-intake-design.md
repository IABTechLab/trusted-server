# Hashed-email intake

Status: Draft, revision 1. Not yet reviewed.

Issue: part of [#1245](https://github.com/IABTechLab/trusted-server/issues/1245)
(LiveRamp server-to-server). Related: [#630](https://github.com/IABTechLab/trusted-server/issues/630)
and its parent [#619](https://github.com/IABTechLab/trusted-server/issues/619)
(both closed as not planned), [#777](https://github.com/IABTechLab/trusted-server/issues/777)
(pluggable modules).

Builds on the [server-side identity providers](2026-10-20-server-side-identity-providers-design.md)
spec, revision 12, called "the framework" below. "Framework section 4.4"
means section 4.4 of that spec; a bare "section 4.4" means this one. The
framework lists hashed-email intake as a non-goal that "needs its own spec"
(framework section 2.2). This is that spec. The LiveRamp ATS and UID2 provider
specs build on it.

Every `file:line` reference is to `c84a73da7`, the commit the framework is
pinned to. Core paths are written without the `crates/trusted-server-core/src/`
prefix, and Fastly adapter paths start with `adapter-fastly/src/`. Prebid.js
references are to version 10.26.0, read from
`crates/trusted-server-js/lib/node_modules/prebid.js/`.

Terms used throughout, in addition to the framework's:

- **Hash**: the SHA-256 digest of a normalized email address, 32 bytes. TS
  never sees the address itself.
- **Normalization profile**: the rules the publisher applies to an address
  before hashing it. Vendors use different rules (section 4.4), so one
  address can have more than one hash.
- **Intake header**: `x-ts-hem`, the private response header the publisher
  origin uses to give TS the hash (section 4.1).
- **Reset header**: `x-ts-hem-reset`, the private response header the
  publisher origin sends on sign-out or account change (section 4.8).
- **HEM provider**: an enabled identity provider whose descriptor declares
  `Input::HashedEmail` in any `needs` or `wants` (framework section 4.3).
- **Intake-eligible request**: a request on which TS may accept a hash
  (section 4.2).

## 1. Reconciling #630

### 1.1 What was decided

#630, "HEM Resolution (LiveRamp)", was part 2 of #619, "As publisher I want
to use Existing 1st Party Data". #619 held one design with four parts: a bot
gate, a harvest of partner cookies into KV, HEM resolution through LiveRamp,
and cross-browser propagation of IDs between rows that share an IP-derived
prefix. On 2026-05-26 the same maintainer closed #619 and both of its
sub-issues, #629 (first-party signal collection) and #630, as not planned,
within one minute. No comment, linked pull request or discussion records a
reason. #619's parent, #712 ("Implement Edge Cookie (EC) key system
(Phase 2)"), was closed as completed on 2026-07-09.

So the closure is recorded, but its grounds are not. This spec cannot show
that #630 was rejected on grounds it addresses. It can show what the closed
design did, which later documents in this repository object to, and what has
changed since.

### 1.2 What #630 proposed, and what later documents object to

| #630 design (#619 §2)                                                                                    | Objection in a later document                                                                                                                                                                           | This spec                                                                                                                |
| -------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| A request header, `X-ts-hem`, added "at their origin or CDN layer before proxying to TS"                 | "Merely naming a header such as `X-ts-hem` does not create that trust boundary" (#1245 design comment, §4 option A). "A bare header convention is not sufficient" (§4 option C)                         | A response header from the configured publisher origin only. Browser-supplied copies are stripped (section 4.2)          |
| Consent: "No additional consent check beyond the existing EC gate" (#619 §2.7)                           | The native spec "must define global EC permission plus LiveRamp-specific consent before disclosure" (#1245 §10)                                                                                         | A hash gate stricter than the EC gate, plus each provider's own policy (section 4.9)                                     |
| HEM fields on `PartnerRecord`, a pull-sync variant, written with `upsert_partner_id` (#619 §2.3, §2.6)   | "Do not put LiveRamp-specific placement/request fields into the shared EC partner type or add a private LiveRamp KV writer" (#1245 §6). Pull sync "is not a ready-made ATS resolver" (#1245 §6)         | No partner-record fields and no writer. The hash feeds the framework's coordinator, mutation and one write (section 4.7) |
| An endpoint and response shape "to be confirmed with LiveRamp": `api.rlcdn.com/api/identity/v1/envelope` | The documented identified call is `GET /api/identity/v2/envelope` (#1245 §9)                                                                                                                            | Out of scope. The LiveRamp provider spec owns the vendor call                                                            |
| No retention, logout or account-switch rule                                                              | "Consent withdrawal and DSR deletion must remove ... any retained resolution inputs/bindings. Define logout/account-switch behavior separately" (#1245 §10). Retention "remain[s] undecided" (#1245 §5) | Request-scoped, never stored (section 4.6). An explicit reset contract (section 4.8)                                     |
| Cross-browser propagation in the same story (#619 §3)                                                    | "Any cross-browser association needs an approved linking mechanism and its own privacy, consent and household/shared-IP rules" (#1245 §12)                                                              | Out of scope (section 2.2). The hash binds to one browser's EC only (section 4.3)                                        |
| Shipped beside a cookie harvest and a bot gate with their own KV writes (#619 §0, §1)                    | Each extra write is a cost and a CAS race (framework section 1, P3)                                                                                                                                     | No new write. Removals use the framework's existing removal path (section 4.8)                                           |

The browser LiveRamp design records the same position: native resolution "is
not implementation-ready" because TS "has no approved source for hashed
email", and #630 "must not be silently revived"
(`docs/superpowers/specs/2026-08-21-liveramp-integration-design.md`, §5.4).
Its §15.1 says to "create or reopen a dedicated issue only after product
approval" and to reconcile #630 there.

### 1.3 What is different now

- **Product asked again.** #1245 ("Write a spec on non-browser dependent
  LiveRamp sync (s2s API)") was opened on 2026-10-06 by the author of #619,
  after the browser LiveRamp work shipped. That is the product approval the
  browser design's §15.1 asked for, for a spec. It is not yet approval to
  build.
- **There is new evidence.** The 2026-10-06 publisher test found Safari's
  tracking protection blocking Prebid's `identityLink` request, so no
  envelope reached the auction (#1245 §1). Browser-only collection does not
  reach these visitors. The framework cannot fix that case without a hash
  (framework section 1, P1).
- **There is a shared path.** The framework gives a vendor-neutral provider
  trait, consent policies, pre-disclosure revalidation and one KV write per
  page load. #630 had none of these and had to invent its own.
- **The trust boundary is now specified.** #1245's design comment set out the
  requirements a hash input must meet (§4, option A). Section 4 meets each of
  them.

### 1.4 Conclusion

Every objection a later document raises against #630's design is addressed
in this spec. The closure's own grounds are not recorded, so this spec cannot
claim to address them. It is therefore conditional: **approval of this spec
requires the maintainer who closed #630 to confirm that the closure does not
rule out hashed-email intake designed this way** (decision D1, section 11).
Until then, nothing here is approved to build, and section 9 keeps intake off.

## 2. Goals and non-goals

### 2.1 Goals

- **H1.** A publisher origin can give TS a hash for the visitor it has
  authenticated, on the response to that visitor's navigation, and TS fills
  `ResolveInput::hashed_email()` (framework section 4.4) with it.
- **H2.** TS never sees a plaintext address. The publisher normalizes and
  hashes.
- **H3.** A hash is accepted only from the configured publisher origin and is
  bound to the current request's full EC ID. TS never mints an EC for it and
  never attaches it to another visitor's row.
- **H4.** A hash never reaches the browser, a log, a metric, a cookie, KV, a
  bidder, Prebid Server or any provider that did not declare it.
- **H5.** A hash is request-scoped. It is used only by that request's
  after-send providers and then dropped.
- **H6.** Sign-out and account change remove the IDs derived from the
  previous account.
- **H7.** No new KV write path. G4 and G12 of the framework hold unchanged.
- **H8.** Off by default, with a shadow mode that measures the publisher
  contract before any hash leaves TS.

### 2.2 Non-goals

- **A browser hash-submission endpoint.** Section 3.5 explains why.
- **Vendor calls.** The LiveRamp ATS and UID2 specs decide endpoints,
  payloads, consent parameters and ownership. This spec supplies only the
  input.
- **Storing hashes.** Any later proposal to persist a hash, or a value
  derived from one, needs its own retention decision (section 4.6).
- **Cross-browser linking**, as in framework section 2.2.
- **Other adapters.** Intake runs where KV identity runs, on Fastly
  (framework section 2.2). The header strips run on every adapter (section
  4.2).
- **Hashed phone numbers and custom IDs.** Section 5 sketches them as a
  separate decision.

## 3. How the hash reaches TS: options A to D

#1245's design comment (§4) lists four options and selects none.

### 3.1 Option A: a private publisher-origin response header

On a navigation where the publisher's backend recognizes a signed-in visitor,
the origin returns the page with the visitor's hash in a response header. TS
reads it, removes it, and uses it after send.

- **Trust.** TS already fetches the publisher origin for every page
  (`publisher.rs:4433`, `handle_publisher_request`). A response from the
  configured origin is the publisher speaking. No new credential is needed.
- **Binding.** The browser sends its session cookie and its `ts-ec` cookie on
  the same request, so the origin's answer is about the same browser whose EC
  TS holds.
- **Retention.** Request-scoped by construction: the origin sends the hash
  again on every signed-in navigation, so TS never needs to keep it.
- **Consent.** The request carries the visitor's own consent signals, which
  TS decodes before routing.
- **Costs.** The hash arrives after routing has started, so hold-mode
  providers on the navigation cannot use it (section 4.7). Signed-in
  navigations must bypass the shared caches (section 4.5). The origin has to
  add two headers.

### 3.2 Option B: an authenticated backend API

The publisher's backend calls a new TS endpoint at sign-in with the hash and
the visitor's EC.

- It needs endpoint authentication, replay protection and rate limits, and a
  way to prove the caller may bind this hash to this EC. Knowing an EC string
  must not be enough (#1245 §4).
- The call carries no visitor consent. The framework forbids authorizing on
  the row's stored consent snapshot, which "authorizes nothing" (framework
  section 7.1). TS would have to keep the hash until the visitor's next
  request, which breaks H5, or call the vendor with no consent, which is not
  allowed.
- A write outside page loads would be a new writer, beside batch sync.

**What it would need later:** a signed binding token that TS issues to the
page and the backend returns, a stored-hash retention decision or a consent
proof carried in the call, and its own entry in the framework's list of
writers outside page loads (framework G4).

### 3.3 Option C: a trusted upstream request header

A publisher-controlled CDN in front of TS adds the hash to the browser's
request.

- It arrives before routing, so phase 1 could capture it and hold-mode
  providers could use it. That is its one advantage over option A.
- Most TS deployments have no layer in front of TS. TS is the edge.
- TS must tell the trusted hop from the public client. A bare header is
  spoofable, so the hop needs mutual TLS or a signature with a shared secret,
  a timestamp and replay protection, bound to the request.

**What it would need later:** a `[trusted_upstream]` contract (secret, signed
header format, clock skew, replay window), strip-and-reject of unsigned
copies, and validation that the deployment really has such a hop.

### 3.4 Option D: browser envelope reuse

The browser's LiveRamp module keeps resolving envelopes, which reach TS in
the `/auction` body and `ts-eids` and are seeded into KV (framework section
7.2). This works today and the framework keeps it. It supplies no hash, so it
does not help a visitor whose browser module never obtained an envelope,
which is the #1245 Safari case. It needs nothing new.

### 3.5 Not an option: a browser hash-submission endpoint

An endpoint where page script posts a hash would let any visitor submit any
hash. TS would then call the vendor with a hash the visitor chose and serve
the result, which the visitor's page can read from `x-ts-eids` (framework
section 7.1). That makes TS an oracle that turns someone else's email address
into their envelope. #1245 says such an endpoint "should not be treated as
equivalent to a trusted publisher signal". This spec rejects it.

### 3.6 Comparison and recommendation

| Option             | Trust boundary        | Binding to the EC       | Consent on the call    | Timing                          | New surface                             |
| ------------------ | --------------------- | ----------------------- | ---------------------- | ------------------------------- | --------------------------------------- |
| A, origin response | the configured origin | same request as `ts-ec` | the visitor's, decoded | after routing; after-send only  | two response headers                    |
| B, backend API     | a new credential      | needs a binding token   | none on the call       | event-driven, out of page load  | an endpoint, a secret, a writer         |
| C, upstream header | a signed hop          | same request as `ts-ec` | the visitor's, decoded | before routing; hold mode works | a signing contract and a layer in front |
| D, browser reuse   | not a hash input      | existing browser ingest | existing               | existing                        | none                                    |

**Recommendation: option A for v1** (decision D2). It is the only option that
needs no new credential, keeps the hash request-scoped and has the visitor's
consent on the same request. Its timing cost matters little: an initial
resolution needs a hash only once per envelope, and after-send work is the
framework's default mode. Option D stays as it is. Option C is the upgrade
path if a deployment with a trusted layer in front of TS needs hold-mode
initial resolution. Option B is the upgrade path for sign-ins that never
produce a navigation through TS, and needs the retention decision first.

## 4. Design: the private origin response header

### 4.1 The contract

The publisher origin may set two response headers on the response to any
request TS proxies to it:

```http
x-ts-hem: basic=dbf1912b4cad4df9823f627874244e64b50a6ac550b9cb33b17df671a26679db, plus-stripped=86e0b9e56c17cc4d12387e1949b85053fbe73bc3ce5a1188713a9d300cc6133d
x-ts-hem-reset: 1
Cache-Control: private, no-store
```

The example hashes are of the fictional address `jane.doe+news@example.com`
under each profile (section 4.4).

- **`x-ts-hem`**: the hashes of the signed-in visitor's address, one per
  normalization profile the publisher supports. Sent on every response to a
  signed-in visitor's navigation, not only the first.
- **`x-ts-hem-reset: 1`**: the account this browser was signed in to has
  signed out or changed (section 4.8).
- **`Cache-Control`** must forbid shared storage on any response that carries
  either header (section 4.5).

TS removes both headers from every response before the browser sees it,
whether it accepted them or not, and whatever the consent (section 4.2).

The header names are fixed. A configurable name would need its own strip
lists and validation, for no gain (decision D3).

### 4.2 Trust boundary

**Accepted from one place only.** TS accepts `x-ts-hem` only from the
response of the configured publisher origin on the publisher fallback route
(`adapter-fastly/src/app.rs:873-881`, `handle_publisher_request`). A response
from an integration proxy, an asset route (`app.rs:840`), the template cache
or any other route never supplies a hash.

**Only on an intake-eligible request.** TS decides before the origin fetch,
once pre-route filters have allowed dispatch, and stores the decision on
`EcContext`. A request is intake-eligible when all of these hold:

1. `ec.identity.hashed_email.mode` is `shadow` or `on` (section 4.11).
2. It is the writer request (framework section 5.1). That already requires a
   real browser (`adapter-fastly/src/app.rs:410`), `Sec-Fetch-Dest: document`
   and no prefetch.
3. It is a returning EC: the EC came from the `ts-ec` cookie and was not
   generated on this request (section 4.3).
4. It carries at least one configured `session_cookies` cookie (section
   4.11). A visitor without a session cannot be signed in, so the origin has
   nothing to say, and TS keeps that request's caching unchanged (section
   4.5).
5. The hash gate passes on this request's consent (section 4.9).

On any other request a hash in the response is discarded and counted (section
4.10).

**Browser copies never reach the origin.** `rewrite_origin_request`
(`publisher.rs:3124-3147`) removes `x-ts-hem` and `x-ts-hem-reset` from the
request TS forwards to the origin. An origin that reflects request headers
into its response can then never hand a browser-chosen value back to TS.
Both names also join `INTERNAL_HEADERS` (`constants.rs:67`), so
`copy_custom_headers` (`http_util.rs:23-34`) keeps them out of integration
upstream requests too. That function already skips every `x-ts-` name; the
list entry documents the intent.

**Removed before the browser sees it.** Right after the origin response
arrives (`publisher.rs:5175`), before the header URL rewrite
(`publisher.rs:5227`) and the template-cache gate (`publisher.rs:5247`), TS
reads both headers and removes every field of each. This runs on every
adapter, every status and every consent outcome, including when intake is
off, when consent is denied and when the request is not intake-eligible. The
Fastly terminal hook, `apply_terminal_response_effects`
(`adapter-fastly/src/main.rs:727-747`), removes both names again from every
response on every route, so a header that an integration upstream sets is
removed too.

### 4.3 Binding to the current EC

- **One EC source.** `EcContext` takes the EC ID only from the `ts-ec` cookie
  (`ec/mod.rs:325`). A hash binds to that value and to nothing a header or
  body could name.
- **No minting.** A request whose EC was generated on it is not
  intake-eligible (section 4.2, rule 3). The framework runs no provider on a
  new visitor's first navigation anyway (framework section 4.7, step 8), so
  the hash on that navigation would have no consumer. The next signed-in
  navigation carries it again.
- **No rotation.** A request that rotates an orphaned EC skips phase 4
  (framework section 5.2), and intake drops the hash when the EC is replaced,
  as `replace_with_generated` already drops the pull-sync marker
  (`ec/mod.rs:612-617`).
- **No other visitor.** The hash is held on this request's `EcContext` and
  read only by this request's providers. Nothing can name another EC. Before
  any byte reaches a provider, phase 4 re-reads the live row and requires it
  live and consenting (framework section 4.7, "Pre-disclosure
  revalidation").
- **Residual risk.** A client that has copied another browser's `ts-ec`
  cookie and holds its own publisher session can attach its own account's
  hash to that row. That needs the cookie, which is the same bar as every
  other EC write. It fails the real-browser gate unless it is a real browser.

### 4.4 Format

**Grammar.** `x-ts-hem` is a list of members separated by `,`, with optional
spaces around each. A member is `<profile>=<digest>`:

- `<profile>` is `basic` or `plus-stripped`.
- `<digest>` is exactly 64 hexadecimal digits, upper or lower case. TS
  decodes it to 32 bytes.

**Normalization profiles.** The publisher normalizes, validates and hashes.
TS cannot check normalization, because it sees only the digest. The two
vendors this spec serves use different rules, so one digest cannot serve
both for every address:

| Profile         | Rules                                                                                                                                                  | Source                                                                                                                                                                                                         |
| --------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `basic`         | Trim leading and trailing spaces. Lowercase. For `gmail.com` only, remove `.` and any `+` suffix from the local part. Other domains keep `+` and `.`   | UID2 [Normalization and Encoding](https://unifiedid.com/docs/getting-started/gs-normalization-encoding); the pinned Prebid UID2 module matches (`libraries/uid2IdSystemShared/uid2IdSystem_shared.js:351-398`) |
| `plus-stripped` | Remove spaces, tabs and other empty characters. Lowercase. For all domains, remove `+` and what follows it. For Gmail, remove `.` before `@`. Validate | LiveRamp [Step 3: Prepare the Identifiers](https://developers.liveramp.com/authenticatedtraffic-api/docs/3-prepare-the-identifiers)                                                                            |

For `jane.doe+news@example.com`, `basic` hashes the address as it is and
`plus-stripped` hashes `jane.doe@example.com`, so the digests differ (section
4.1). For an address with no `+` and no Gmail domain, they are equal, and the
publisher still sends both members.

Each provider declares the profile it needs (framework change F2, section 6).
UID2 declares `basic`; LiveRamp declares `plus-stripped`. A provider whose
profile the header lacks sees `HashedEmail` as unavailable.

**Algorithm.** SHA-256 only. UID2 takes "a Base64-encoded SHA-256 hash of a
normalized email address", encoded from the hash bytes, not the hex string
(UID2 normalization page). LiveRamp accepts SHA-256, SHA-1 and MD5 and
recommends SHA-256 "if you are only able to hash one" (LiveRamp Step 3). TS
keeps the 32 bytes, and each provider encodes them as its vendor takes them,
through `HashedEmail::to_hex` or `to_base64` (framework section 4.4).

**Size.** The header field must be at most 256 bytes. Two members take 158.

**Malformed input.** TS accepts the header whole or not at all. It rejects
it, and uses no member, when:

- the response carries more than one `x-ts-hem` field
- the field is over 256 bytes or is not visible ASCII
- a member has an unknown profile, a repeated profile, or a digest that is
  not 64 hex digits
- a digest is the SHA-256 of the empty string
  (`e3b0c442…b855`), a sign the publisher hashed a missing address

`x-ts-hem-reset` is accepted only with the exact value `1`, in one field. Any
other value is ignored.

A rejection is counted by reason (section 4.10). It never fails the page.

### 4.5 Caching

A hash is visitor-specific. If a response that carries one were stored in a
shared cache, a later visitor's request could read it, and TS would bind one
visitor's hash to another's EC. Three rules prevent that:

1. **Intake-eligible requests skip every shared cache.** TS sends their
   origin fetch with `with_cache_bypass()` (`platform/http.rs:105-109`),
   which the Fastly adapter maps to `set_pass(true)`
   (`adapter-fastly/src/platform.rs:454`), and gives them a new template-cache
   bypass reason, `IdentityIntake`, beside `CookiePolicy`
   (`publisher.rs:4647-4661`, `:6145`). So their response comes from the
   origin for this request, and no stored template answers it. Today
   `apply_origin_cache_intent` (`publisher.rs:4357`) may let a non-ad
   navigation use the platform cache. For intake-eligible requests the bypass
   wins.
2. **A hash on a cacheable response is rejected.** TS accepts `x-ts-hem`
   only when the origin's `Cache-Control` forbids shared storage
   (`cache_control_headers_are_private_or_no_store`, `cache_policy.rs:338`)
   and no edge-cache header (`EDGE_CACHE_HEADER_NAMES`, `cache_policy.rs:24-29`)
   is present. Rule 1 already keeps TS's own caches out. This rule makes an
   origin that marks signed-in pages as shareable visible in the metrics
   before it causes harm elsewhere.
3. **A response that carried either header is never stored.** The
   template-cache gate (`template_cache_ttl`, `publisher.rs:6600`) refuses it
   with the same `IdentityIntake` reason, even on a request that was not
   intake-eligible. TS also marks it terminal-private
   (`enforce_terminal_private_cache_privacy`, `response_privacy.rs:87-90`),
   so the browser and any cache after TS see `private, no-store`.

Requests without a session cookie keep their current cache behaviour. The
cost is that signed-in navigations of returning, consenting real browsers no
longer hit the platform cache. Shadow mode measures that cost (section 9).

TS cannot undo a mistake a cache makes before TS sees the response. A
non-eligible request may still be answered from the platform cache with a
stored response that carries the header, if the origin let one be stored.
TS strips the header and uses no hash from it (rules 1 and 3). The stored
page itself is the origin's leak, not TS's.

### 4.6 Retention

- **Request-scoped.** The decoded hashes live on this request's `EcContext`,
  which reaches finalize and phase 4 through `EcFinalizeState` in response
  extensions (`adapter-fastly/src/app.rs:318-332`, inserted at `:535`). They
  are dropped when the request ends.
- **Used only by after-send providers of that request** (section 4.7).
- **Never stored.** Not in KV, `ts-eids-s`, `ts-eids-r`, provider state, logs,
  metrics, traces or error messages. `HashedEmail` implements neither
  `Serialize`, `Deserialize` nor `Display` (framework change F1), so no
  serializer can write one by accident. A provider that copies the hash into
  its state, call context, `uid` or `ext` fails conformance (framework change
  F9).
- **Refresh does not need it.** LiveRamp refresh takes the existing envelope
  (#1245 §5, citing LiveRamp Step 7), and UID2 refresh takes the refresh
  token in its private state (framework section 12.3).

What is lost when a later visit carries no hash, because the visitor is
signed out, the session cookie is missing or consent changed:

- **Nothing stored is lost.** IDs already resolved stay, and `Refresh` keeps
  them current without the hash.
- **No initial resolution.** A source whose `Initial` kind needs the hash is
  left out at eligibility step 4 (framework section 4.7). The skip changes no
  `ps.n`, so the next navigation that carries the hash is eligible at once.
- **No re-resolution after expiry or invalidation.** When a refresh fails for
  good, LiveRamp's refresh 204 or UID2's expired refresh token, the stored ID
  is removed or ages out, and the source waits for a navigation that carries
  the hash.
- **No retry of a missed call.** A missed initial resolution (timeout, the
  burst cap, a failed flush) has no durable input to retry from, as #1245 §5
  says. The visitor's next signed-in navigation is the retry.
- **The new visitor's first navigation and any rotated navigation** never
  use the hash (section 4.3).

Storing a hash, or a keyed digest of one, would remove these losses at the
cost of a new store of personal data with retention, access and deletion
rules. This spec does not do it (decision D5).

### 4.7 Delivery and timing

The framework's phase 1 captures the user agent, DNT and the page URL into
`EcContext` before routing (framework sections 4.4 and 5.2). The hash cannot
be captured there: it arrives with the origin response, after routing has
started, and on the navigation the auction is dispatched before the origin
fetch (framework section 3.4). Its path:

**Phase 1, at the routing decision.** TS computes intake eligibility (section
4.2) and stores it on `EcContext`. Consent is already decoded.

**Phase 2, routing.** `handle_publisher_request` holds `&mut EcContext`
(`adapter-fastly/src/app.rs:873-881`). It strips the browser copies from the
origin request, fetches with the bypass of section 4.5, and, when the
response arrives (`publisher.rs:5175`), parses and removes both headers. On
an intake-eligible request with a valid header on a 2xx or 304 response, it
stores the hashes on `EcContext`. **Phase 2 is the phase that captures the
hash.** This is the one input the framework does not capture in phase 1
(framework change F3).

**Phase 3, finalize.** The framework runs after-send dry runs in phase 2
(framework section 5.2). At that point the hash is not yet known, so a
provider whose `Initial` needs it would always be skipped. They move to the
start of phase 3, after the origin response has been read (framework change
F4). Finalize already holds the response and already sets the pending
markers from the dry runs, so nothing else moves. A dry run that returns
`Some` gets its marker as before. A reset (section 4.8) is staged here.

**Phase 4, after send.** The coordinator re-reads the live row, recomputes
eligibility, and rebuilds each marked provider's request from the same
in-memory hash (framework section 5.2, step 3). Results join the mutation and
its one write.

**Hold mode.** A hold-mode provider on the navigation is dispatched before the
origin responds, so it never has the hash there. `/auction` and
`/_ts/page-bids` are stagers and never have it. So a source whose kind needs
the hash is resolved only after send. A hold-mode HEM provider with such a
source due, on a writer navigation that carries the hash, runs after send if
it did not already run in hold mode on that request (framework change F5). A
provider is dispatched at most once per request, so a multi-source provider
that ran in hold mode leaves its hash-needing sources to the next signed-in
navigation.

**Status.** The writer flushes and dispatches after-send providers only when
the response status is 2xx or 304 (framework section 5.1). A hash on any
other status is discarded with the rest of that request's after-send work.

**Single-page apps.** A visitor who signs in through a script call and stays
in the app sends its hash only on its next full navigation through TS
(section 8, question 7).

### 4.8 Lifecycle

**Sign-out.** The origin sends `x-ts-hem-reset: 1` on the sign-out response
and stops sending `x-ts-hem`. TS then removes every record that any HEM
provider wrote and clears those providers' private state, because those IDs
may be derived from the account that signed out.

**Account change.** The origin sends `x-ts-hem-reset: 1` on the response
where the signed-in account changes, and the new account's `x-ts-hem` on the
same or a later response. The reset applies before that request's own
results (below), so the new account resolves from a clean slate.

**How a reset is applied.** A reset is a removal, so it takes the framework's
removal path and its rules (framework sections 5.6 and 6.2):

- It is accepted from the configured publisher origin on any publisher
  fallback response with a cookie EC, not only on writer navigations. A
  sign-out is often a form post answered with a redirect, which is not a 2xx
  writer navigation. Like a hash, it is accepted only on a response whose
  `Cache-Control` forbids shared storage (section 4.5, rule 2), so a stored
  copy can never reset every visitor who reads it.
- For each HEM provider it stages `rs: {<provider>: <at>}`, with
  `at = now`, in a new `ts-eids-r` cookie on that response (framework change
  F6). The read order already hides the records a removal names (framework
  section 6.3), so the previous account's IDs stop reaching bidders from that
  response on (framework G12).
- On a writer request it also joins the mutation. Mutation step 2 applies
  removals before positive changes (framework section 5.4), so a reset and
  the new account's result in one request leave only the new result.
- Applying it removes every record the provider wrote that was acquired at or
  before `at`, clears the provider's `ps.st`, and stores `ps.rs = at` in KV.
  The cookie entry is applied once KV's `ps.rs` is at or after `at`. Unlike an
  opt-out, it does not block the provider afterwards.
- A response to a client that fails `looks_like_browser()` keeps its reset in
  its own `ts-eids-r` and never makes the removal overflow write, as for a
  capture removal (framework section 4.7, "The real-browser gate").
- It needs no consent, like every removal (framework section 5.6), and is
  honoured in every `mode`, including `off`, whenever a HEM provider is
  enabled. Turning intake off never strands a previous account's IDs.

**No new write path.** A reset uses the removal cookies and, when a batch does
not fit, the existing removal overflow exception (framework section 6.2). The
framework's G4 exceptions stay at two.

**What the publisher must do.** Send the reset on every sign-out and account
change. Without it, IDs derived from the previous account stay in the row
and keep being refreshed, because refresh needs no hash, and the next person
to use that browser is bid on as the previous account. TS cannot detect an
account change by itself without storing something derived from the hash
(decision D5). The browser's own vendor modules keep their own storage, such
as LiveRamp's `_lr_env`, and can re-seed an ID through browser ingest. The
publisher's sign-out must clear that storage too. That is outside TS.

**A session that expires without sign-out** sends no reset. The previous IDs
stay until they expire or a later reset arrives. This matches what the
browser modules do with their own storage.

**Consent withdrawal.** The framework's tombstone removes the row and every ID
in it (framework section 5.1). The hash gate fails on that request, so the
hash is discarded. Nothing was stored, so nothing else needs deleting.

**Deletion.** The deferred deletion framework tombstones the row (framework
section 5.5). TS holds no hash to delete. A vendor-side deletion of the
address, for example a LiveRamp or UID2 opt-out, belongs to each vendor spec.

### 4.9 Consent

Using a hash at all is gated separately from each provider's policy. The
**hash gate** passes when all of these hold on the request's consent:

1. Global EC consent: `ec_allowed()` (`ec/mod.rs:621`).
2. EID persistence: `allows_eid_persistence` (`consent/mod.rs:495`).
3. `ConsentPolicy::strict()` with no vendor (framework section 4.3): where TCF
   applies, TCF Purposes 1 and 4; blocked when any consent string present
   does not decode; blocked by every US opt-out TS decodes (sale, sharing,
   targeted advertising), by the GPC and known-child fields of GPP US
   sections, by `Sec-GPC: 1` and by `DNT: 1`. "Where TCF applies" is as in
   framework section 9.1.

The gate is checked at intake, before the hash is stored on `EcContext`
(section 4.2, rule 5). A provider's relaxations (`allowing_dnt`,
`allowing_gpc`, `allowing_us_opt_out`) never relax the hash gate. Sending an
address hash to a vendor is the most sensitive disclosure TS makes, so the
strictest policy applies whatever the vendor's basis for its other inputs
(decision D6).

Then, per provider, as the framework already requires: the provider's own
`ConsentPolicy` (eligibility step 3, framework section 4.7), and the live-row
check before disclosure. LiveRamp's own requirement, a positive TCF consent
string in the EU and vendor 97 (LiveRamp Step 4;
`modules/identityLinkIdSystem.js:38`), is set in the LiveRamp spec.

The removal headers need no consent (section 4.8). Both headers are stripped
whatever the consent (section 4.2).

### 4.10 Security, logging and metrics

- **Never logged.** The hash, the header value and any part of it never
  appear in a log line, an error message or a metric dimension (framework
  section 9.3). A rejection is logged at debug with its reason only:
  `not_eligible`, `consent`, `cacheable_response`, `duplicate_field`,
  `oversize`, `bad_member`, `empty_address`, `status`.
- **Redacted `Debug`.** `EcContext` derives `Debug` (`ec/mod.rs:249`). The new
  field relies on `HashedEmail`'s redacting `Debug` (framework section 4.2,
  conventions).
- **Never sent onward.** The hash reaches only a provider whose descriptor
  declares `Input::HashedEmail`, through the coordinator's checks (framework
  section 4.7, "Dispatch"). It never enters the auction request, Prebid
  Server, `x-ts-eids`, `/identify` or the admin views. No code path copies
  `EcContext` fields into those.
- **The resulting IDs are visible to page scripts.** An envelope resolved from
  a hash is a server-side ID like any other, so it appears in `x-ts-eids`,
  which page scripts can read (framework section 7.1). That exposure is the
  framework's accepted one. It covers the visitor's own ID only.
- **Metrics.** Counters by outcome per request: intake-eligible, accepted per
  profile, rejected by reason, reset received, and intake-eligible requests
  that bypassed the cache. No identifier, EC or URL is a dimension.

### 4.11 Configuration

```toml
[ec.identity.hashed_email]
# "off", "shadow" or "on". Default "off".
# off:    never accept a hash. Both headers are still stripped on every
#         response, and resets are still honoured (section 4.8).
# shadow: decide eligibility, bypass caches, parse, gate and count, then
#         discard the hash. No provider ever sees it.
# on:     accepted hashes reach HEM providers.
mode = "off"
# Required unless mode is "off". Cookie names of the publisher's signed-in
# session. Only a request carrying one is intake-eligible.
session_cookies = ["example_session"]
```

Validation (framework section 10.2), each an error:

- `mode` is not `off`, `shadow` or `on`.
- `mode` is not `off` and `session_cookies` is empty.
- A `session_cookies` entry is not a valid cookie name, is repeated, or names
  a TS identity cookie (`ts-ec`, `ts-eids`, sharedId, `ts-ec-pull-complete`,
  `__Host-ts-eids-s` or the `__Host-ts-eids-r-` prefix).
- `mode = "on"` and no enabled provider declares `Input::HashedEmail`. Intake
  would bypass caches for nothing. `shadow` is allowed without one, to
  measure before a vendor is enabled.
- `mode = "on"` and `ec.ec_store` is unset. Without KV identity no provider
  runs (framework section 2.2).

The table sits under `[ec.identity]` and moves with it under #777 (framework
section 10.5). Under the configuration rules of #1348
(`docs/guide/configuration-rules.md` on `feature/7-integration-seam-impl`), a
`[ec.<name>]` table holds the settings of a module named `<name>`, so
`[ec.identity]` would have to become one of the `ec` type's own settings, as
`ec_store` is. Framework section 10.5 settles that for both tables (section
8, question 10).

## 5. Optional extension: hashed phone and custom IDs

This is a separate decision (D8). Nothing in sections 1 to 4 depends on it.

**Hashed phone.** LiveRamp takes `it=11` for phone hashes (LiveRamp Step 4).
Its Step 3 says "only SHA1 hashes should be passed" for phone numbers, of
"U.S. ten-digit phone numbers" with country code and formatting removed. UID2
takes E.164, `+[country code][subscriber number]`, hashed with SHA-256 and
base64-encoded (UID2 normalization page). The two hashes differ in algorithm,
length and input. A phone extension would add:

- `Input::HashedPhone`, a new variant of the `#[non_exhaustive]` `Input`
  (framework section 4.3).
- `HashedPhone`, with two profiles: `sha1-us10` (20 bytes) and `sha256-e164`
  (32 bytes), declared per provider as section 4.4 declares email profiles.
- `ResolveInput::hashed_phone()`.
- `x-ts-hph` beside `x-ts-hem`, with the same trust boundary, binding,
  caching, retention, reset and gate.

SHA-1 of a ten-digit number is reversible by enumeration in minutes on
ordinary hardware, so a SHA-1 phone hash is close to the plaintext number. It
needs its own privacy review before any design.

**Approved custom ID.** LiveRamp takes `it=15` with `iv` set to
`ANA_ID:CUSTOM_ID` (LiveRamp Step 4). It needs a custom namespace account ID
from LiveRamp (#1245 §9, an unconfirmed prerequisite). It is a publisher
identifier, not a hash, so it would be a different input,
`Input::PublisherCustomId`, with its own format rules. It is not specified
here.

## 6. Proposed framework changes

Each change is additive under framework section 4.2: new builder methods,
new `#[non_exhaustive]` variants, new private fields and crate-private types.
No existing provider changes, except the two section 12 examples that use
the hash (F10). Revision 13 folds these in.

**F1. `HashedEmail` construction and traits (framework section 4.4).**
Replace "No code in v1 constructs one." with:

```rust
impl HashedEmail {
    /// Decodes 64 hexadecimal digits, either case, into the 32 digest bytes.
    /// `None` for any other input and for the SHA-256 of the empty string.
    pub(crate) fn from_hex_digest(hex: &str) -> Option<Self>;
}
```

Add: "Only hashed-email intake constructs one (intake spec, section 4.4).
`HashedEmail` implements neither `Serialize`, `Deserialize` nor `Display`."

Test: `from_hex_digest` accepts upper and lower case, rejects 63 and 65
digits, non-hex and the empty-string digest; a `compile_fail` doc test shows
`serde_json::to_string(&hash)` does not compile.

**F2. Email normalization profiles (framework sections 4.3 and 4.4).**

```rust
/// How the publisher normalized an address before hashing it.
#[non_exhaustive]
pub enum EmailNormalization {
    /// Trim, lowercase; for `gmail.com`, remove `.` and the `+` suffix.
    Basic,
    /// Remove all whitespace, lowercase, remove the `+` suffix on every
    /// domain; for Gmail, remove `.` before `@`.
    PlusStripped,
}

impl Resolves {
    /// The profile `ResolveInput::hashed_email` returns. Default: `Basic`.
    pub fn with_email_normalization(self, normalization: EmailNormalization) -> Self;
}
```

`EmailNormalization` derives what `Input` derives. `build()` returns `Config`
when `with_email_normalization` is called without `Input::HashedEmail` in any
`needs` or `wants`. `ResolveInput::hashed_email()` returns the hash of the
declared profile, or `InputUnavailable` when the request has none for it.

Test: a provider declaring `PlusStripped` receives the `plus-stripped` member
and is unavailable when only `basic` was sent; `build()` rejects a profile
without the input.

**F3. Request-scoped hashes on `EcContext` (framework sections 4.4 and 5.2).**

```rust
impl EcContext {
    pub(crate) fn set_hashed_emails(&mut self, hashes: HashedEmails);
    pub(crate) fn hashed_email(&self, profile: EmailNormalization) -> Option<&HashedEmail>;
    pub(crate) fn is_intake_eligible(&self) -> bool;
}
```

`HashedEmails` is crate-private and holds at most one hash per profile.
Replacing the EC clears it. Add to framework section 4.4, "Where each input
comes from": "**`HashedEmail`**: from the publisher origin's `x-ts-hem`
response header, captured in phase 2 when the origin response arrives
(intake spec, section 4.7). It is the one input not captured in phase 1."

Test: an orphan rotation and a generated EC leave no hash; `Debug` of an
`EcContext` holding one prints no digest.

**F4. After-send dry runs move to phase 3 (framework section 5.2).** In phase
2, remove the bullets that run after-send dry runs. Add to phase 3, first:
"For a returning EC, the coordinator calls `request` for each eligible
after-send provider as a dry run, with `input.is_dry_run()` true, after the
origin response has been read, so a hash captured in phase 2 is available.
Providers declared `with_private_state()` get a marker without a dry run, as
before."

Test: on a signed-in navigation, a provider whose `Initial` needs
`HashedEmail` gets a pending marker; on the same navigation without the
header, it gets none and its `ps.n` is unchanged.

**F5. Hash-needing kinds run after send (framework section 4.7, step 8).**
Add: "A source whose kind needs `HashedEmail` is resolved only after send on
the writer navigation, because the hash arrives with the origin response. A
hold-mode provider with such a source due, on a writer navigation that
carries the hash, is dispatched after send if it was not dispatched in hold
mode on that request. A provider is dispatched at most once per request."

Test: a hold-mode HEM provider with no stored record is not called before the
auction, is called after send with the hash, and is not called twice when a
second source of it was refreshed in hold mode on the same request.

**F6. Account reset (framework sections 5.4, 6.1, 6.2 and 8).**

- `ts-eids-r` payload gains `rs: {<provider>: <at>}`.
- KV `ps` gains `rs`, the time of the last applied reset, with
  `#[serde(default, skip_serializing_if = ...)]`.
- `IdentityMutation`'s `ProviderStateChange` gains a reset. Mutation step 2
  applies it with the removals: when `at` is after the live `ps.rs`, remove
  every record the provider wrote and acquired at or before the reset's
  time, clear `ps.st`, and set `ps.rs` to that time.
- Applied rule (framework section 6.2): a reset entry is applied when KV's
  `ps.rs` is at or after its `at`.
- Section 8 gains a row: "Reset from the publisher origin | every record the
  provider wrote, acquired at or before `at` | `st` cleared, `rs = at`; `o`,
  `n` and `f` unchanged | info".
- A reset is honoured only for providers that declare `Input::HashedEmail`.

Test: a reset and a new account's `Initial` result in one writer request leave
only the new ID and new state; a reset on a 302 sign-out response lands in a
`ts-eids-r` cookie, hides the old ID from the next `/auction` and is applied
by the next writer; a replayed applied reset changes nothing; a reset from a
non-browser client never triggers the overflow write; the one-write table
gains "Sign-out reset on a writer navigation: 1" and "Sign-out reset on a
non-writer response: 0".

**F7. Header strips (framework sections 6.2 and 14).** Section 6.2,
"Outbound requests", gains: "The publisher proxy removes the `x-ts-hem` and
`x-ts-hem-reset` request headers before forwarding to the origin
(`publisher.rs:3124-3147`), and both names join `INTERNAL_HEADERS`
(`constants.rs:67`). Every response loses both headers before the browser
sees it: at the origin response on every adapter (`publisher.rs:5175`), and
in the Fastly terminal hook on every route (`adapter-fastly/src/main.rs:727-747`)."
Section 14, step 0, gains the same strips and the parsing of
`[ec.identity.hashed_email]`, because `Ec` denies unknown fields
(`settings.rs:541-542`).

Test: a browser request carrying `x-ts-hem` reaches the origin without it; an
origin response carrying both headers reaches the browser without them on
consent denial, on a non-eligible request, with `mode = "off"` and on a 500;
an integration upstream's `x-ts-hem` is removed on Fastly.

**F8. Caching (framework section 9.4).** Add: "An intake-eligible request
bypasses the platform cache and the template cache (intake spec, section
4.5). A publisher response that carried `x-ts-hem` or `x-ts-hem-reset` is
never stored as a template and is sent as terminal-private.
`TemplateCacheBypassReason` gains `IdentityIntake`."

Test: an intake-eligible request is sent with cache bypass and no template
lookup; a response carrying `x-ts-hem` on a non-eligible request is not
stored and leaves TS with `private, no-store`; a hash on a response without
`private` or `no-store`, or with `Surrogate-Control`, is rejected.

**F9. Conformance (framework section 13).** `check_conformance` gains one
check, with no API change: "For a case with a test `HashedEmail`, the hash in
lower hex, upper hex and base64 appears in no replacement state, call
context, `ResolvedId` UID or `ext`." The request body, query and headers are
where a HEM provider sends it, so they are not checked.

Test: a fixture provider that stores the hex digest in its state fails
conformance.

**F10. Text and examples.**

- Section 2.2: replace the HEM non-goal with "**HEM intake** is specified in
  the hashed-email intake spec. Providers that need a HEM run only where it
  supplies one."
- G9: replace "even though v1 has no way to obtain one" with "and the intake
  spec supplies it on signed-in navigations".
- Section 4.4 accessor list: unchanged signatures; the note on `HashedEmail`
  changes as in F1.
- Section 10.1 and 10.2: add the table and errors of section 4.11.
- Section 12.1 (LiveRamp): add
  `.with_email_normalization(EmailNormalization::PlusStripped)`, and change
  "in v1 an owned `liveramp.com` source is resolved only by `Refresh`" to say
  `Initial` runs after send on signed-in navigations. LiveRamp Step 4 names
  the consent parameters `ct` and `cv` (TCF v2) and `gpp` and `gpp_sid`
  (GPP), not `gdpr_consent`; the example marks them assumed, and the LiveRamp
  spec should use the documented names.
- Section 12.3 (UID2): unchanged; `Basic` is the default.
- Section 16: record decisions D2 to D7 once confirmed.

## 7. Vendor facts and their sources

| Fact                                                                                             | Source                                                                                    | Status                                           |
| ------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------- | ------------------------------------------------ |
| UID2 email normalization (trim, lowercase, Gmail-only dot and `+` removal)                       | UID2 Normalization and Encoding; `uid2IdSystem_shared.js:351-398`                         | verified                                         |
| UID2 `email_hash` is base64 of the SHA-256 bytes, 44 characters                                  | UID2 Normalization and Encoding; `POST /token/generate`; `uid2IdSystem_shared.js:335-345` | verified                                         |
| UID2 returns status `optout` with HTTP 200 for an opted-out user                                 | UID2 `POST /token/generate`                                                               | verified                                         |
| LiveRamp email normalization (all whitespace, lowercase, `+` removal on all domains, Gmail dots) | LiveRamp Step 3                                                                           | verified                                         |
| LiveRamp accepts SHA-256, SHA-1 and MD5 email hashes and recommends SHA-256 if only one          | LiveRamp Step 3                                                                           | verified                                         |
| LiveRamp `it=4` email, `it=11` phone, `it=15` custom ID as `ANA_ID:CUSTOM_ID`; `atype=3` s2s     | LiveRamp Step 4                                                                           | verified                                         |
| LiveRamp phone: SHA-1 of a U.S. ten-digit number without country code                            | LiveRamp Step 3                                                                           | verified                                         |
| LiveRamp `iv` is 64 hex digits; its example is upper case; no case rule is stated                | LiveRamp Step 4                                                                           | **assumed** case-insensitive                     |
| LiveRamp consent parameters are `ct`/`cv` and `gpp`/`gpp_sid`                                    | LiveRamp Step 4                                                                           | verified                                         |
| LiveRamp says to call it only with a positive consent string for users in the EU                 | LiveRamp Step 4                                                                           | verified                                         |
| LiveRamp GVL ID 97                                                                               | `modules/identityLinkIdSystem.js:38`                                                      | verified                                         |
| LiveRamp states no rate limit                                                                    | LiveRamp Step 4                                                                           | **assumed** none known; to confirm with LiveRamp |
| LiveRamp refresh takes the envelope, not the hash                                                | #1245 §5, citing LiveRamp Step 7                                                          | from #1245                                       |
| The pinned Prebid UID2 module declares no GVL ID                                                 | `modules/uid2IdSystem.js` (no `gvlid`)                                                    | verified; the UID2 spec owns UID2's consent      |

The vendor pages are LiveRamp's
[Step 3](https://developers.liveramp.com/authenticatedtraffic-api/docs/3-prepare-the-identifiers)
and
[Step 4](https://developers.liveramp.com/authenticatedtraffic-api/docs/4-call-the-ats-envelope-api),
and UID2's
[Normalization and Encoding](https://unifiedid.com/docs/getting-started/gs-normalization-encoding)
and
[POST /token/generate](https://unifiedid.com/docs/endpoints/post-token-generate),
read on 2026-10-23.

## 8. Open questions

1. **#630's closure.** Does the maintainer who closed #619, #629 and #630 on
   2026-05-26 confirm that the closure does not rule out this design
   (section 1.4)?
2. **Publisher feasibility.** Can the publisher's backend add `x-ts-hem` and
   `x-ts-hem-reset` on the responses TS proxies, mark them `private`, compute
   both profiles, and detect every sign-out and account change? #1245 §4
   lists this as unconfirmed.
3. **LiveRamp hex case.** Step 4's example is upper case and no rule is
   stated. The framework's `to_hex` is lower case. The LiveRamp spec should
   confirm either is accepted.
4. **LiveRamp extra hash types.** LiveRamp recommends sending SHA-1 and MD5
   as well for match rate. V1 takes SHA-256 only. Is the match-rate cost
   acceptable, or should a later revision add profiles with other
   algorithms?
5. **Detecting account change without the publisher.** Should TS store a
   keyed digest of the last hash per row, so it can reset by itself when a
   different hash arrives? That is a new store of personal data (decision
   D5).
6. **Memory hygiene.** With the Fastly reusable-sandbox feature, one instance
   serves several requests. Should `HashedEmail` overwrite its bytes on drop?
   That may need a new dependency such as `zeroize`.
7. **Single-page apps.** A sign-in through a script call reaches TS only at
   the next full navigation. Is that delay acceptable, or does it motivate
   option B later?
8. **Cache cost.** How much edge-cache hit rate do signed-in navigations lose
   to the bypass of section 4.5? Shadow mode measures it.
9. **`googlemail.com`.** Neither vendor page mentions it. The publisher follows
   the pages literally unless the vendors say otherwise.
10. **Config path under #777.** `[ec.identity]` and this table cannot be
    `[ec.<name>]` module tables under the configuration rules. Framework
    section 10.5 decides where both go.

## 9. Rollout

Off by default. Each step ships on its own.

0. **Strips first, with the framework's step 0** (framework section 14). The
   request and response strips, the `INTERNAL_HEADERS` entries, the
   template-cache refusal and terminal-private mark for responses that
   carry either header, and parsing of `[ec.identity.hashed_email]`. With
   these deployed, an origin that starts sending the headers early never
   leaks them, and a rollback to this release still loads a config with the
   table.
1. **Shadow, after the framework's step 2.** `mode = "shadow"`: eligibility,
   cache bypass, parsing, the hash gate, resets and every metric of section
   4.10. No provider receives a hash. This measures the publisher contract
   (accepted and rejected rates by reason, resets seen) and the cache cost,
   with no disclosure to any vendor.
2. **On, per provider.** `mode = "on"` once a HEM provider's own spec has
   shipped. Each HEM provider stays disabled until its vendor spec's
   checklist has passed.

**Activation gates** before `mode = "on"`:

- Decision D1 is confirmed (section 1.4).
- Shadow mode has run on the live service. Rejections other than
  `not_eligible` and `consent` are near zero, and resets arrive on sign-out.
- The publisher has confirmed, in writing, normalization per profile,
  `private` caching on every response with either header, and the reset on
  every sign-out and account change (section 8, question 2).
- A privacy review covers disclosing address hashes to each enabled vendor.
- The vendor spec's prerequisites hold, for LiveRamp the placement, origin
  and country approvals of #1245 §9.
- **Live acceptance**, with an approved test account whose address is under a
  publisher-controlled test domain: a signed-in navigation produces an
  accepted hash and a pending marker, `/_ts/admin/ec` then shows the
  provider's ID, a sign-out removes it, and the browser never receives
  either header. Evidence keeps only status, source and counts (#1245 §11).

**Rollback.** `mode = "off"` stops accepting hashes at once. Resets keep
working, IDs already stored stay and keep refreshing, and disabling a provider
stops its IDs being served (framework section 7.1). Do not roll back past
step 0: earlier code strips neither header, so an origin still sending
`x-ts-hem` would leak it to browsers.

## 10. Testing

**Intake unit tests** (core, native).

- Parsing: one and two members, either case, spaces around members; rejection
  of a duplicate field, an oversize field, non-ASCII, an unknown profile, a
  repeated profile, 63 and 65 digits and the empty-string digest; a rejected
  header yields no member. Fixtures use the digests of fictional
  `example.com` addresses only, such as those in section 4.1.
- Eligibility: each rule of section 4.2 on its own makes a request
  non-eligible.
- Strips: every case of F7.
- Caching: every case of F8.
- Binding: a generated EC, a rotation and a header-supplied EC carry no hash.
- Consent: the hash gate blocks each signal of section 4.9 on its own,
  including `DNT: 1` for a provider that declares `allowing_dnt()`.
- Logging: a capturing logger sees no digest, in either case or in base64,
  across every accept and reject path.

**Provider harness.** HEM providers are tested with `ProviderHarness` and
`check_conformance` (framework section 13). A fictional provider shows the
pattern the LiveRamp and UID2 specs follow:

```rust
let provider = ExampleHemProvider::new(&example_hem_config()).expect("should build the provider");
let hash = HashedEmail::for_test("86e0b9e56c17cc4d12387e1949b85053fbe73bc3ce5a1188713a9d300cc6133d");

let mut signed_in = ProviderHarness::new(&provider)
    .with_client_ip("192.0.2.10")
    .with_hashed_email(hash.clone());
let request = signed_in
    .request()
    .expect("should build a request")
    .expect("should call the provider for an initial lookup");
assert!(
    request.url().query().is_some_and(|query| query.contains(&hash.to_hex())),
    "should send the hash in the declared encoding"
);

let mut signed_out = ProviderHarness::new(&provider).with_client_ip("192.0.2.10");
assert!(
    signed_out.request().expect("should not fail").is_none(),
    "should skip an initial lookup without a hash"
);

signed_in.check_conformance(
    &ConformanceCases::default().with_resolve_case(
        HarnessSetup::default()
            .with_client_ip("192.0.2.10")
            .with_hashed_email(hash),
        StatusCode::OK,
        &json!({ "uid": "example-envelope" }),
    ),
);
```

With F9, the conformance run also fails a provider that copies the hash into
its state, call context or IDs.

**Coordinator and phases.**

- F4 and F5 cases, including a signed-in navigation answered with 302, which
  dispatches nothing and keeps no hash.
- A second navigation without the header after a successful `Initial` runs
  `Refresh` and never needs the hash.
- A `no_match` with a hash sets `ps.n` as framework section 8 says, so a
  signed-in visitor is not re-queried on every navigation.

**Reset.** Every case of F6, and: a reset on a sign-out with consent denied
still applies; with `mode = "off"` it still applies; it removes only records
of HEM providers.

**One-write invariant.** The framework's table (framework section 13) gains:
signed-in navigation with an accepted hash and an `Initial` result, 1 write;
the same with nothing due, 0; sign-out reset on a writer navigation, 1;
sign-out reset on a non-writer response, 0.

**Adapters.** On Axum, Cloudflare and Spin, an origin response carrying
either header reaches the client without it, and no hash is accepted. The
parity test crate passes.

## 11. Decisions to confirm

| ID  | Decision                    | Proposed choice                                                                                                                                |
| --- | --------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| D1  | #630 reconciliation         | The maintainer who closed #630 confirms the closure does not rule out this design. Until then nothing here is approved to build                |
| D2  | Intake option for v1        | Option A, a private publisher-origin response header. C and B are later upgrade paths; D stays as is                                           |
| D3  | Header names                | Fixed: `x-ts-hem` and `x-ts-hem-reset`                                                                                                         |
| D4  | Hash format                 | SHA-256 only, hex in the header, two named normalization profiles, whole-header rejection on any error                                         |
| D5  | Retention                   | Request-scoped. No stored hash and no stored digest of one. Account change relies on the publisher's reset                                     |
| D6  | Hash gate                   | `strict()` with no vendor on top of the EC and EID-persistence gates. Provider relaxations never relax it                                      |
| D7  | Caching                     | Intake-eligible requests, those carrying a session cookie, bypass the platform and template caches; a hash on a shareable response is rejected |
| D8  | Hashed phone and custom IDs | Not in v1. A separate decision after a privacy review of SHA-1 phone hashes                                                                    |
| D9  | Rollout                     | Strips with framework step 0, then shadow, then on per provider after live acceptance                                                          |

## 12. Review log

No review rounds yet.
