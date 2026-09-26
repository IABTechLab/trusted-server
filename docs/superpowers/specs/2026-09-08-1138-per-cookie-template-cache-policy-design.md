# Per-cookie shared-template cache policy

Status: Implemented and independently reviewed; validation results are recorded in the implementation plan.

Issue: [#1138](https://github.com/IABTechLab/trusted-server/issues/1138).

## 1. Purpose

Allow publishers to share reader-neutral HTML templates across anonymous readers
while separating bounded cookie-selected variants and excluding requests whose
cookies can cause personal HTML. Extend the existing cookie-independence assertion
without weakening the default or the response eligibility guards.

This specification defines the issue's two configuration lists. The parsing,
validation, and compatibility rules below are proposed design decisions that make
the issue's behavior precise enough for implementation and testing.

## 2. Current behavior and failure modes

The relevant cache contains transformed origin HTML before per-reader assembly.
It is distinct from the platform's raw-origin cache and from the final response,
which must remain private and must never become a shared per-user response cache.

In `crates/trusted-server-core/src/publisher.rs`, the publisher handler computes
`cookie_disqualifies` before sending the origin request. Currently any `Cookie`
header disqualifies the request unless `origin_is_cookie_independent` is true.
The same decision protects both lookup and storage. TS mints an identity cookie,
so the conservative default excludes most repeat visitors.

The handler constructs `TemplateCacheKey` before the origin fetch. Configured
`template_cache_vary` values come from the request at TS's hop. On a response,
`template_cache_ttl` verifies that the configured header names cover the origin's
`Vary` declaration; it cannot prove what a downstream intermediary did to values.

### 2.1 Experiment cookie translated downstream

Request flow:

```text
Browser: Cookie: ab_bucket=A
  -> TS: selects a template key; X-Exp-Variant is absent
  -> Publisher CDN: translates ab_bucket into X-Exp-Variant: A
  -> Origin: renders arm A and responds with Vary: X-Exp-Variant
```

With cookie independence enabled and only `x-exp-variant` configured as a header
dimension, both arm A and arm B have the same absent header at TS. The drift guard
accepts the declared name, but the shared key does not distinguish the arms. A
template populated by one arm can therefore be served to the other.

### 2.2 Small logged-in population

An origin may render personal account state when `session` is present, while all
other visitors receive shareable HTML. The current boolean cannot exempt that
session-bearing population while admitting anonymous readers carrying TS cookies.

## 3. Scope and invariants

The change must:

- Add named cookie values as explicit per-variant template key dimensions.
- Exclude requests carrying named bypass cookies from both lookup and storage,
  falling back to the existing inline path.
- Apply `origin_is_cookie_independent` only to cookies outside those lists.
- Preserve existing behavior when both lists are absent or empty.
- Keep all existing request and response eligibility gates in force.
- Keep templates reader-neutral within each variant and assemble per-reader state
  after lookup as today.

The change does not introduce automatic cookie discovery, cookie-to-header mapping,
cookie mutation, session validation, a value allowlist, a cardinality limiter, a new
cache backend, or a new assembly mode. It does not alter the authorization carve-out,
raw-origin caching, or final-response privacy behavior.

No TS identity or consent cookie is implicitly exempted. Operators must still
assert independence for unlisted cookies when that assertion is valid for their
origin. A cookie that affects consent-dependent origin HTML needs an appropriate
key or bypass policy just like any other cookie.

## 4. Configuration contract

The fields live in the existing `[creative_opportunities]` table:

```toml
[creative_opportunities]
assembly_mode = "esi"

# Include all applicable existing header dimensions for the deployment.
template_cache_vary = ["x-exp-variant"]

# Bounded, reader-neutral variants only.
template_cache_key_cookies = ["ab_bucket"]

# Presence, including an empty value, requires inline processing.
template_cache_bypass_cookies = ["session"]

# Assertion applies only to cookies outside the two lists above.
origin_is_cookie_independent = true
```

Use `Option<Vec<String>>` for each new field, with the existing
`#[serde(default, skip_serializing_if = "Option::is_none")]` convention. Missing
and empty lists have identical runtime meaning. Missing fields remain omitted
when serializing configuration. The boolean retains its default of false.

### 4.1 Name validation

Validate configuration at the existing creative-opportunities validation boundary:

- Names are nonempty ASCII HTTP tokens: letters, digits, and
  ``! # $ % & ' * + - . ^ _ ` | ~``. Whitespace, separators such as `;` or `=`,
  control bytes, and non-ASCII bytes are invalid.
- Match names exactly and case-sensitively. Do not lowercase them as header names
  are lowercased; `session` and `Session` are distinct cookie names.
- Reject TS identity cookie names (`ts-ec`, `ts-eids`, `sharedId`) in the key list;
  allow them in the bypass list.
- Reject duplicates within either list and overlap between the two lists. Report
  the offending field and name as a configuration error, using existing error
  handling conventions.
- No wildcard, prefix, or regular-expression matching is supported.
- Preserve the existing prohibition on `Cookie` and `Authorization` in
  `template_cache_vary`.

Rejecting overlap makes contradictory operator intent visible at configuration
load. Runtime bypass still takes precedence whenever a request contains any
configured bypass cookie alongside configured key cookies.

### 4.2 Operator responsibility

Key cookies must represent bounded variants, such as experiment arms or region
buckets. Account IDs, session tokens, TS identity IDs, and other user identifiers
must not be keyed. Hashing such identifiers would still create a per-user cache
and would not make the template reader-neutral.

The implementation cannot infer cardinality or validate the publisher's semantic
assertion from a cookie name. Choosing stable, bounded values and identifying all
origin HTML dependencies remain operator responsibilities. Arbitrary values can
fragment the cache; this change does not add admission or cardinality controls.

## 5. Eligibility semantics

Evaluate the policy at the existing pre-fetch request gate, against all `Cookie`
fields in the request that is about to be forwarded to the publisher origin.

Preserve existing request preparation. In particular, GPT diagnostics preparation
removes its reserved cookie, drops fields that fail `to_str()`, removes empty
pairs, and combines retained pairs before generic cookie handling. The policy
classifies the resulting origin inputs; it does not recover or classify removed
browser bytes. Raw-header tests must also exercise the already-prepared request
boundary so earlier sanitization does not mask evaluator coverage.

When both configured lists are empty, retain the exact existing decision:

```text
cookie_disqualifies = Cookie header is present AND independence is false
cookie key dimensions = none
```

This legacy path deliberately preserves existing behavior even for empty or
malformed headers. The new parser does not silently change old deployments.

When either list is nonempty:

1. No `Cookie` header means no cookie disqualification. Each configured key cookie
   contributes an explicit absent dimension.
2. Parse and validate all fields using section 6. Unclassifiable or ambiguous
   input disqualifies the request.
3. Presence of any bypass cookie disqualifies the request, regardless of its value
   or the independence assertion.
4. With independence false, any unlisted cookie disqualifies the request.
5. Otherwise the cookie gate admits the request. Extract all configured key-cookie
   dimensions, including absence where applicable.

Admission by this gate is necessary but insufficient for shared caching: GET, ESI
mode, authorization, request cache semantics, response shareability, and all other
existing gates still apply.

For key list `["ab_bucket"]` and bypass list `["session"]`:

| Request cookies              | Independence false | Independence true | Key contribution if admitted     |
| ---------------------------- | ------------------ | ----------------- | -------------------------------- |
| No header                    | Admit              | Admit             | `ab_bucket` absent               |
| `ab_bucket=A`                | Admit              | Admit             | `ab_bucket` present, value `A`   |
| `ab_bucket=`                 | Admit              | Admit             | `ab_bucket` present, empty value |
| `ts-ec=reader1`              | Bypass             | Admit             | `ab_bucket` absent               |
| `ab_bucket=A; ts-ec=reader1` | Bypass             | Admit             | `ab_bucket` present, value `A`   |
| `session=`                   | Bypass             | Bypass            | None                             |
| `ab_bucket=A; session=token` | Bypass             | Bypass            | None                             |
| `Session=token`              | Bypass as unlisted | Admit             | `ab_bucket` absent               |
| Ambiguous or malformed input | Bypass             | Bypass            | None                             |

Compute the cookie decision once, before the origin request is consumed. Reuse it
for both lookup eligibility and the response-side store gate. A bypassed request
must not perform cache lookup, acquire a cache reservation, or store a template,
even if a matching anonymous template already exists or the origin response is
otherwise shareable. It must take the existing origin/inline fallback.

## 6. Cookie parsing contract

Use a cache-policy-specific parser or evaluator with a narrow interface. Do not
change the semantics of unrelated cookie helpers.

Existing `cookies.rs` helpers are insufficient as-is: `extract_cookie_value` reads
one selected header and the first matching pair, while the `CookieJar` helper
skips invalid pairs and collapses duplicate names. The origin can receive all
header fields, so those behaviors could discard a cache-relevant signal.

For the named-policy path:

- Inspect every `Cookie` field in wire order. Accept multiple fields when all
  contain valid pairs with unique names across the complete request.
- Split each field on semicolons. Trim only surrounding space and horizontal tab
  from each pair. Empty fields or empty pairs, including a trailing semicolon,
  cause bypass rather than being silently discarded.
- Split the trimmed pair on its first `=`. Require a valid nonempty token name
  immediately before it. A bare name or remaining whitespace in the name or value
  is malformed and bypasses. Pair trimming happens first: `ab_bucket= ` becomes
  an accepted empty value, while `ab_bucket =A` and `ab_bucket= A` bypass.
- For key cookies, accept an empty value. Otherwise accept unquoted cookie-octet bytes, or a value
  enclosed by exactly one matching pair of double quotes containing cookie-octet
  bytes. Cookie-octet bytes are hexadecimal `21`, `23–2B`, `2D–3A`, `3C–5B`, and
  `5D–7E`. This excludes whitespace, controls, comma, semicolon, double quote,
  backslash, and non-ASCII bytes from the value payload.
- For unlisted cookies only when independence is true, additionally accept commas
  and balanced double quotes. This admits compact JSON and comma/colon lists seen
  in browser cookies. Continue rejecting whitespace, backslashes, controls,
  non-ASCII bytes, unmatched quotes, and comma-delimited fragments whose prefix
  before `=` is a valid cookie name. Quotes cannot span semicolon-delimited pairs.
  Bypass-cookie presence remains unconditional; unlisted cookies with independence
  false still bypass regardless of value. This assumes origin cookie parsing
  treats unlisted values as opaque and parses each semicolon-separated pair
  independently. An origin parser that stops at nonstandard JSON could observe
  different key cookies depending on order; that deployment cannot assert this
  independence. The evaluator does not normalize or remove the ignored values.
- Preserve the original value bytes, including allowed `=` characters, case,
  percent escapes, and any surrounding quotes. Do not URL-decode, unquote, or
  otherwise normalize values. Quoted and unquoted representations may use
  separate keys; over-separation is safer than merging distinct inputs.
- Repeated key-cookie names cause bypass even when their values match. Different
  origins can interpret duplicates differently; this design does not choose first
  or last wins. Repeated unlisted names are allowed only with independence asserted
  and every value passing framing checks. Bypass names always bypass.
- For requests reaching this evaluator, malformed input or an unsupported byte
  sequence causes cache bypass. The evaluator introduces no new request error.
  Existing earlier validation errors remain unchanged for fields surviving
  preparation: the publisher calls
  `handle_request_cookies` before the cache gate, and failure of `to_str()` on its
  selected header still returns the existing `InvalidHeaderValue` error. An
  invalid later field that earlier parsing does not inspect must cause bypass
  when the policy evaluator inspects all fields.

Parsing may conservatively reduce cache hits for nonconforming clients. This is
intentional only when a named policy is active; the no-list compatibility path
continues to follow the previous boolean behavior.

## 7. Cache key representation

Extend `platform::TemplateCacheKey` with an explicit cookie-dimension collection,
using a small domain type with a cookie name and optional raw value bytes.
`None` means absent; a present zero-length value means `name=`. Do not reuse the
header namespace or inject synthetic headers.

For admitted requests:

- Include every configured key-cookie name, sorted by exact name bytes, with its
  presence and value. Multiple configured cookies form a combined variant.
- Exclude bypass and unlisted cookie values entirely.
- Encode cookie dimensions in a separate domain-tagged, length-prefixed section
  of the existing SHA-256 canonical key input. Include a count, each name, an
  explicit presence marker, and the length-prefixed value when present.
- Append that section only when key-cookie dimensions are nonempty. An empty
  collection must produce the same canonical bytes as the existing key format.
- Keep URL surrogate keys unchanged so a URL purge removes all cookie variants.
  Keep the global template purge key unchanged.

Thus cookie ordering and header splitting do not fragment valid equivalent
requests, but absent/empty values, different arms, and different cookie names do
not collide. Reordering configuration may still invalidate entries through the
existing complete-settings fingerprint; this harmless over-invalidation need
not be optimized away.

The template fingerprint already hashes the complete typed settings and TSJS
content. The new fields must participate through ordinary serialization, so
policy changes cannot reuse an entry admitted under a different policy.

No transform schema-version bump is required: template bytes and assembly markers
do not change. Preserve old keys when new fields are omitted and dimensions are
empty; nonempty dimensions and changed settings separate the new keys.

Raw cookie values must not be added to logs, diagnostic response headers, metric
labels, or error messages. Do not log the expanded key through its `Debug`
representation. The opaque hashed backend key retains the existing diagnostics
boundary.

## 8. Response guards and downstream variants

Leave `template_cache_ttl` response requirements intact. In particular:

- `Vary: Cookie` always refuses storage, including mixed-case names and repeated
  `Vary` fields, even when all request cookies are keyed or asserted irrelevant.
- `Vary: *`, uncovered header names, origin `Set-Cookie`, lack of authorized
  positive shared freshness, and other existing disqualifiers remain effective.
- The new cookie list does not automatically cover a header named in `Vary`.

For the motivating topology, operators must configure both `ab_bucket` as a key
cookie and `x-exp-variant` as a header dimension. The former distinguishes readers
at TS; the latter satisfies the existing header coverage contract. An absent
keyed cookie gets its own dimension because the CDN may choose a distinct default
arm for it.

This is still an operator assertion: TS cannot verify that the downstream header
is a deterministic function of the configured cookie. If the CDN also selects
HTML using another cookie or another unrepresented signal, that dependency needs
its own appropriate key or bypass policy. The old unsafe header-only
configuration is not detected or repaired automatically by this feature.

Response drift checks run when an origin response is fetched, not on a cache hit.
They cannot retroactively validate an already cached template against an origin
policy change. Existing freshness and purge mechanisms remain necessary.

## 9. Implementation boundaries

| Location                                                                      | Intended responsibility                                                                                          |
| ----------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| `crates/trusted-server-core/src/creative_opportunities.rs`                    | Optional configuration fields, validation, accessors as needed, and updated documentation of scoped independence |
| `crates/trusted-server-core/src/cookies.rs` or a focused adjacent core module | Pure named-policy parsing/evaluation with an explicit admitted/bypassed result and key-cookie dimensions         |
| `crates/trusted-server-core/src/platform/template_cache.rs`                   | Cookie dimension type and canonical hashed key extension                                                         |
| `crates/trusted-server-core/src/platform/mod.rs`                              | Export the new dimension type if required by existing platform API conventions                                   |
| `crates/trusted-server-core/src/publisher.rs`                                 | Evaluate once, feed both gates, construct key dimensions, retain existing inline fallback and response guards    |
| Existing core and Fastly adapter test fixtures                                | Update explicit configuration and key struct literals with empty/absent new fields                               |
| `docs/guide/configuration.md` and `trusted-server.example.toml`               | Policy semantics, deployment examples, limitations, and rollback instructions                                    |

Keep cookie policy independent of runtime SDKs. No new OS-specific dependency,
async runtime, or platform-specific implementation is needed. The Fastly adapter
continues consuming the shared opaque key; other adapters retain their current
cache capabilities.

Follow existing error-stack conventions at the configuration error boundary;
retain the existing `validate_runtime` string-error interface. Cookie policy
classification failure is a normal cache bypass, not a new page error. It does
not suppress errors raised by request handling before the evaluator runs.
Retain `X-TS-Template-Cache: bypass-request` and existing bounded diagnostics;
this change does not require a new public diagnostic value or metrics subsystem.

## 10. Acceptance and regression tests

### 10.1 Configuration

- Omitted new fields deserialize successfully, resolve to empty lists, and remain
  omitted on serialization. Existing fixture serialization/fingerprints remain
  unchanged when both fields are omitted.
- Explicit empty lists have legacy runtime semantics. Their serialized presence
  may safely change the settings fingerprint.
- Valid token names, including distinct case variants, are accepted.
- Empty/invalid names, duplicate entries, and cross-list overlap fail validation.
- Raw `Cookie` and `Authorization` remain prohibited header dimensions.
- Test key-only, bypass-only, and both-list configurations, with the unused list
  either omitted or explicitly empty. Both lists empty select the legacy path;
  either list nonempty activates the named policy.

### 10.2 Parsing and key isolation

- Cover missing header, missing keyed cookie, empty value, quoted values, values
  containing `=`, and exact case-sensitive name matching.
- Equivalent cookie ordering and valid splitting across multiple header fields
  yield equal dimensions and keys for the same settings.
- Changed key-cookie values, changed key-cookie names, and absent versus empty
  values produce different backend keys; multiple dimensions compose correctly.
- With independence true, changing only an unlisted TS identity cookie leaves the
  key unchanged.
- Empty bypass values disqualify. A bypass cookie in a later header also
  disqualifies; it must not be hidden by first-header extraction.
- Duplicate key names with equal or different values, across pairs or fields, bypass.
  Duplicate unlisted names remain eligible only with independence asserted and valid
  framing for every value.
- At the evaluator level, bare names, invalid names, empty pairs/fields, invalid
  quoting, forbidden value bytes, and non-ASCII input bypass under a named policy.
- At the evaluator level, preserve legacy boolean decisions for those same
  malformed headers when both lists are empty. This does not imply that every
  such request reaches the evaluator through the publisher handler.
- Pin the unchanged backend key for an existing fixture with empty cookie
  dimensions, and verify surrogate keys remain common to all URL variants.

### 10.3 Publisher behavior

Use the existing in-memory template cache and origin stubs to verify observable
lookup/reservation/store counts, origin requests, and rendered response content:

1. With `x-exp-variant` absent at TS and origin responses declaring
   `Vary: X-Exp-Variant`, arm A and arm B populate separate templates. Later
   requests hit the correct arm without another origin fetch. Readers in the
   same arm with different `ts-ec` values share when independence is true.
2. Warm an anonymous template, then send a session-bearing request. It reaches
   origin, makes no shared-cache call, and uses inline processing. A session
   request against an empty cache likewise never stores a template, even with an
   otherwise shareable origin response. Personal origin bytes never enter cache.
3. Independence false admits requests containing only configured key cookies,
   but adding any unlisted cookie bypasses. Independence true admits that same
   unlisted cookie while still honoring bypass cookies.
4. A missing experiment cookie and an explicitly empty one cannot reuse each
   other's template. A request carrying only ignored cookies uses the missing-arm
   template when independence is true.
5. Ambiguous/malformed cookie requests that reach the policy evaluator bypass an
   already warm cache and cannot store on a cold cache under a named policy.
   Separately preserve the existing error for a selected header that fails
   `to_str()`, and prove that invalid bytes in a later field bypass rather than
   being ignored by the evaluator. Neither path may access or store a template.
   Exercise these raw-field cases at the already-prepared request boundary, and
   separately verify that normal diagnostics preparation retains its existing
   sanitization and keys the actual forwarded cookies.
6. `Vary: Cookie` refuses storage under the new policy, including combined header
   lists. An uncovered downstream header still refuses storage even with a
   configured cookie dimension.
7. Existing `Set-Cookie`, authorization, request-method, freshness, privacy, and
   default inline-mode regressions continue passing. Per-reader assembly remains
   fresh on warm hits and is not captured in the shared template.
8. Changing a configured cookie policy changes the fingerprint/key and prevents
   reuse of entries created under the previous policy.
9. Exercise each list independently: a bypass-only configuration shares anonymous
   traffic and excludes session traffic with independence true; a key-only
   configuration separates experiment arms and admits only listed cookies with
   independence false. Repeat with the unused list explicitly empty to verify
   that it does not disable the named policy.

For the eventual implementation, run target-matched tests after runtime changes
and the full CI gates documented in `CLAUDE.md` before PR handoff, including
adapter tests/clippy, integration parity, JS checks, and docs formatting. This
spec-only change requires document review, whitespace validation, and docs
formatting; it does not claim runtime tests have been executed.

## 11. Deployment, compatibility, and rollback

Existing configurations retain their behavior. An operator who configures only
the boolean still gets its existing all-cookie meaning because no cookies have
been explicitly classified. Enabling ESI remains a separate prerequisite.

For an experiment rollout, identify every signal that selects origin HTML, add
bounded cookie dimensions and session bypass names, and retain required header
dimensions. Assert independence for remaining cookies only after confirming that
they do not change origin HTML. Check arm-correct content and cache diagnostics
with anonymous and session-bearing traffic during the canary.

Changing the typed policy automatically changes the template fingerprint. Old
entries become unreachable under the new policy and can expire normally; use
existing URL/global purge mechanisms when necessary during an incident. Purging
alone cannot fix an unsafe unchanged configuration.

Older binaries use `deny_unknown_fields` and reject either new field even when
its list is empty. Before rolling back a binary, remove both new fields from the
operator configuration and select a safe older policy. If the origin depends on
cookies, restore `origin_is_cookie_independent = false` or disable ESI; retaining
true after removing the lists loses both variant separation and session bypass.
Update the guide's existing rollback field-removal list accordingly.

## 12. Alternatives and decision

| Approach                                               | Trade-off                                                                                                                    | Decision                                        |
| ------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------- |
| Explicit key and bypass cookie lists                   | Direct operator intent; narrowly extends existing gate and key; requires precise parsing                                     | Recommended and specified here                  |
| Derive synthetic headers before key construction       | Equivalent variant expressiveness, but requires a mapping configuration and header namespace and still needs bypass handling | Not selected                                    |
| Keep the boolean and bypass all cookie-bearing traffic | Retains conservative safety but cannot support the two publisher deployment shapes                                           | Retained only as the default compatibility path |

Keying the complete `Cookie` header is excluded by the existing reader-neutral
template design: identity values would create a per-user response cache in
practice. The chosen policy admits only explicitly named variant dimensions and
uses inline processing for personal HTML.
