# Design Spec: Integration Response-Header Hook

**Status:** Design only, and none of it is built at
`split/5-response-hook-docs` (§1a). The pull request that carried the
documentation set
([#1047](https://github.com/IABTechLab/trusted-server/pull/1047)) was closed
without merging on 2026-10-15, and
[#1347](https://github.com/IABTechLab/trusted-server/pull/1347), one of the
stack's pull requests, carries the same set. This spec is held with every
other design document of the stack in
[#1342](https://github.com/IABTechLab/trusted-server/pull/1342). The hook was
removed from the earlier draft of the documentation set because it has no
consumer, which is this spec's own §-rule for speculative surface. The spec is
retained as the design bar for the hook when its first consumer arrives (an
integration that must set response headers such as Accept-CH or detection
results). Revised 2026-08-25, on 2026-10-06 for the module wording, on
2026-10-15 and 2026-10-16 after its round 1 and round 2 reviews (Review log),
and on 2026-10-27 for the permission policy, which is selected when a service
is deployed.
**Author:** Engineering
**Issue references:** #782
**Related specs:** `2026-07-30-pluggable-providers-design.md`,
`2026-07-30-permission-model-design.md`,
`2026-07-30-provider-migration-rollout-design.md` and the baseline DataDome
design `2026-06-11-datadome-server-side-protection-design.md`
**Last updated:** 2026-10-27

> **Context.** Issue #782 already specifies this feature well; its done-when
> is the contract. PR #838 shipped the trait and registry wiring with **no
> adapter call site**. `apply_response_headers` had zero production
> callers, so the feature existed only in its own unit test. This spec
> restates the contract plus the two details the issue left open (ordering
> and collision policy), and adds the rule that prevents a repeat, which is
> that the hook lands with a consumer or not at all. The series (PRs
> #1043-#1047, closed on 2026-10-15 and carried now by the stack's pull
> requests #1343-#1347) applied that rule to this spec itself, see the
> Status above and §7.

---

## 1. Overview

The pre-existing DataDome design remains unchanged. For implementations
governed by this spec, §4a is the normative PR-specific delta and supersedes
that baseline wherever its generic request/effect API, header ordering,
session-by-header behavior, endpoint/request surface, cookie lifecycle,
challenge transport, or response-pointer behavior conflicts. The baseline
continues to provide historical context. It is not a second normative source
for those surfaces.

Integrations can today rewrite request-path behavior (proxies, attribute
rewriters, head injectors), and a request filter can name response headers
to set through `RequestFilterEffects.response_headers`, but no integration
can inspect a response and decide its headers from it. The hook will add
that capability. An integration registers a response-header mutator
via its `IntegrationRegistration` builder, and every adapter applies all
registered mutators to the outbound response for HTML document responses it
processed.

## 1a. What the code does today

This section states the code at `split/5-response-hook-docs` (commit
`61b1399c6`). A path that does not start with `crates/` is under
`crates/trusted-server-core/src/`. Sections 2 to 4a are a design, and none of
that design is built. A search of the code and the guides at that commit finds
none of the names below, and the same search at
`split/7-integration-seam-impl`, `split/31-caching-release` and
`split/50-cookies-by-path` (row 50) finds none either. The names are
`IntegrationResponseMutator`, `with_response_mutator`, `behavior_revision`,
`HOOK_INVARIANT_REVISION`, the seven fields of the cache revision tuple in §3,
`SecurityUse`, `DataDomeSecurityRequestFilter`, `RedactedRequestView`,
`PartnerEgress` and `vary_digest_key_secret_name`.

**Not built, and what each part waits for.**

- **The hook and its apply point (§2).** A registration has no response
  mutator and no revision number, and no adapter has an apply point.
- **The cache revision tuple (§2, §3 and §3a).** Four of its seven fields
  (`active_policy_digest`, `active_policy_ordinal`, `model_epoch` and
  `activation_generation`) come from the permission model's policy activation.
  Section 5.5 of `2026-07-30-permission-model-design.md` is headed "Policy
  revision activation (deferred)" and begins "Not implemented", the record in
  its section 11 says "None of it exists", and row 19 of §8 of
  `2026-07-30-provider-migration-rollout-design.md` says "Not implemented". At
  this commit each build embeds the repository's sample,
  `config/permissions/sample.yaml` (`DEFAULT_PERMISSION_RULES`,
  `permissions.rs` line 494), so the deployed build is the policy. Row 4 of
  the stack (#1345) builds section 3.1 of the permission model design, and at
  row 4 no build carries a policy and a domain's blob, the settings as
  `ts config push` stores them, names its policy file by name and SHA-256.
  Once row 4 is merged forward into this row, the pushed blob is the policy
  identity and `active_policy_digest` takes that `sha256`, while
  `active_policy_ordinal`, `model_epoch` and `activation_generation` still
  have nothing to come from. The remaining three fields
  (`integration_registry_revision`, `effective_config_revision` and
  `hook_invariant_revision`) have no code either. The `pre_epic_v1` test in §4
  item 8, the serve fence in §4a and the effective-config digest in §4a.3
  depend on the same activation.
- **The keyed digest of `Vary` request values (§2).** The setting
  `[cache] vary_digest_key_secret_name` does not exist. `CacheSettings` in
  `settings.rs` holds `asset_rules` only and refuses an unknown key, and no
  setting turns on the "response caching" that makes the key mandatory.
- **The redacted request view and the egress type (§3 and §4a).** Section 11
  of `2026-07-30-pluggable-providers-design.md` names `AuthorizedIdentity` and
  `RedactedRequestView` as "a typed boundary for identity that leaves the
  edge", in the list that opens "No row of the stack builds the following".
  That spec writes `AuthorizedIdentity` with nothing in angle brackets, so the
  `<PartnerEgress>` in `AuthorizedIdentity<PartnerEgress>` (§3) belongs to this
  spec alone. No spec defines `PartnerEgress` or lists what the view strips.
- **The security channel (§4a).** At this commit DataDome is core code, in
  `integrations/datadome.rs` and the `integrations/datadome/` directory, and
  its table is `[integrations.datadome]`. From `split/7-integration-seam-impl`
  on, DataDome is the crate `crates/bot-protection/datadome` and its table is
  `[bot-protection.datadome]`. None of the settings that §4a.3 adds exists in
  the tree at either branch or at `split/50-cookies-by-path`.

**What exists.**

- **Three response-header steps.** Only the operator's `[response_headers]`
  step is shared by every adapter.
  - The Edge Cookie module's headers. `ec_finalize_response` (`ec/finalize.rs`)
    appends them first, through `apply_module_response_headers` in
    `ec/module.rs`. `reserved_response_effect` refuses the `x-ts-` prefix,
    a `Set-Cookie` whose name starts with `ts-`, and the headers in
    `FRAMING_OR_HOP_BY_HOP_HEADERS`, which are `cache-control`, `connection`,
    `content-length`, `keep-alive`, `proxy-authenticate`,
    `proxy-authorization`, `te`, `trailer`, `transfer-encoding` and `upgrade`.
    A refused header makes the module's generation fail, and the response is
    served without an Edge Cookie.
  - The operator's `[response_headers]`.
    `apply_response_headers_with_cache_privacy` (`response_privacy.rs`)
    applies them, and each adapter's own `apply_finalize_headers`, in
    `crates/trusted-server-adapter-<name>/src/middleware.rs`, calls that
    function. The function receives the settings and the response only. It
    runs the cookie rule, applies the operator's headers (skipping
    `Cache-Control` and the edge cache fields once the response forbids shared
    storage), and runs the cookie rule again.
  - The request filters' response effects. `RequestFilterEffects`
    (`integrations/registry.rs`) carries `response_headers`, a flat list of set
    and append operations, and DataDome returns its response headers this way.
    Only the Fastly adapter runs request filters (`run_pre_route_filters` in
    `crates/trusted-server-adapter-fastly/src/app.rs`), and it applies the
    effects in `apply_terminal_response_effects` (`main.rs` of that crate).
    `apply_header_mutation_to_response` skips a single header named in
    `is_forbidden_filter_header` (hop-by-hop and framing headers, `host` and
    the `x-ts-` prefix) and applies everything else, `Set-Cookie` and
    `Cache-Control` included.
- **The order on each adapter.** On Fastly the order is `apply_finalize_headers`
  (geo and version headers, then the shared function with the operator's
  headers), the asset cache policy where one applies, `ec_finalize_response`
  and then `apply_terminal_response_effects`. That last function applies the
  request filter effects, calls `enforce_private_no_store` when the response
  carries the `TerminalPrivateResponse` marker, and runs the cookie rule and
  the uncacheable rule once more. Axum, Cloudflare and Spin call neither
  `filter_request` nor `ec_finalize_response`, so on them
  `apply_finalize_headers` is the only response-header step. On every adapter
  `apply_finalize_headers` calls core's `apply_git_version_header`
  (`version_header.rs`) ahead of `apply_response_headers_with_cache_privacy`.
  `apply_git_version_header` sets `x-ts-version` to the compiled-in git version
  and removes an inherited `x-ts-version` when no valid version is known.
- **The marker.** `enforce_terminal_private_cache_privacy` sets
  `TerminalPrivateResponse` (`response_privacy.rs`) on a response whose
  `no-store, private` policy belongs to Trusted Server, and only the Fastly
  adapter reads the marker. A response without the marker is not restored, so
  a late effect that replaces its `Cache-Control` stands unless the response
  also carries `Set-Cookie`.
- **The cookie rule.** `enforce_set_cookie_cache_privacy` does nothing on a
  response without `Set-Cookie`. On a response with one it removes the edge
  cache fields and writes `private, max-age=0`. It leaves a `Cache-Control`
  that already forbids shared storage as it is, except for the
  `private, max-age=60` policy that the code generates for inactive ad
  templates, which it replaces.
- **Processed HTML.** `publisher.rs` calls
  `enforce_synthesized_html_cache_privacy`, which sets `no-store, private` and
  drops the validators, `Expires`, `Age` and the edge cache fields, on an HTML
  response or a `304` when the ad stack runs or the response was authorized for
  the template cache, whatever cookies the response carries. It makes the same
  call for HTML where the DataDome client tag was suppressed. That rule does
  not apply to other processed HTML, which keeps the origin's policy unless
  another rule in `publisher.rs` changes it, such as the browser cap on
  structurally inactive ad templates.
- **Edge cache fields.** `EDGE_CACHE_HEADER_NAMES` (`cache_policy.rs`) holds
  `surrogate-control`, `fastly-surrogate-control`, `cdn-cache-control` and
  `cloudflare-cdn-cache-control`. `remove_edge_cache_headers` and
  `is_edge_cache_header_name` read it. The code has no `edge-control`.
- **Integration ids.** The registry can hold fourteen ids, being `aps`,
  `prebid`, `js_asset_proxy`, `testlight`, `nextjs`, `permutive`, `lockr`,
  `didomi`, `sourcepoint`, `osano`, `google_tag_manager`, `datadome`, `gpt` and
  `gpt_diagnostics`. Three of them hold an underscore, so
  `[a-z0-9][a-z0-9-]{0,63}` refuses `google_tag_manager`, `js_asset_proxy` and
  `gpt_diagnostics`. `IntegrationRegistry::with_plan` checks no id for a
  duplicate. That function has a `debug_assert_eq!` that a builder's id equals
  its registration's id, which runs in debug builds only, and
  `registered_integrations` merges entries that share an id.
- **Caches.** The module comment of `platform/template_cache.rs` names three
  caches, being the origin readthrough cache, the template cache and an
  assembled response, which "must never exist". The template cache keeps the
  page after the HTML rewrite and before the per-reader assembly, is used only
  when a deployment opts into a shared-template assembly mode, and is
  configured in `[creative_opportunities]` (`assembly_mode`,
  `template_cache_vary` and the keys beside them).
  `TemplateCacheKey::to_cache_key` hashes the schema version, assembly mode,
  scheme, host, path, origin identity, URL, `template_fingerprint`, the request
  values of the headers in the configured `Vary` list (each value
  length-prefixed, with absent kept apart from present) and the configured
  cookie values. Storage is refused when the origin's `Vary` names a header
  that list does not cover. A hit builds its headers again in
  `build_cached_template_response`, replays only the origin headers in
  `REPLAYABLE_POLICY_HEADERS`, and stamps `no-store, private`.
- **Routes and conditional requests.** `classify_response_route`
  (`publisher.rs`) processes a body whose type is `text/*`, JavaScript or JSON
  and whose encoding is supported, whatever the status except `204` and `205`,
  provided the request has a host. `strip_conditional_and_range_headers`
  removes `If-None-Match`, `If-Modified-Since`, `Range` and `If-Range` from the
  origin request, and only when the ad stack runs or the DataDome client tag is
  suppressed on an HTML page. An origin `304` on the ad stack path is answered
  with a `502`.

## 2. Contract

Nothing in this section is built. The names `with_response_mutator`,
`apply_response_headers` and `behavior_revision` are the design's own (§1a).

- `IntegrationRegistration::builder(ID).with_response_mutator(...)` registers
  a mutator; `IntegrationRegistry::apply_response_headers(...)` applies all
  registered mutators in registration order.
- Every registration carries a nonzero `behavior_revision: u32`, bumped for
  any change to its response decision, operation semantics, declared read set,
  or security field list. Integration IDs match
  `[a-z0-9][a-z0-9-]{0,63}` and are unique. Three of the ids the code ships,
  `google_tag_manager`, `js_asset_proxy` and `gpt_diagnostics`, hold an
  underscore and fail this rule (§1a, §8). The registry revision hashes the
  ordered registration list. Order is behavior, so the array is never sorted.
- **The mutator API is structured operations, not header-map access.** A
  mutator returns (or is handed a recorder for) typed operations,
  `append(name, value)` and `replace(name, value)` (v1 is headers-only;
  the cookie operation arrives with the deferred cookie surface, §3),
  which **core validates and applies**,
  attributing each to its integration id. PR #838's shape handed the
  integration an unrestricted `&mut HeaderMap`, which makes §3's collision
  policy unenforceable by construction, because core cannot validate or attribute
  writes it never sees. An API that cannot express a violation beats one
  that promises to catch it.
- **Every adapter calls the apply point** on its outbound-response path for
  processed documents. The response-finalization code the adapters share today
  is two core functions, `apply_git_version_header` and
  `apply_response_headers_with_cache_privacy`, and each adapter's own
  `apply_finalize_headers` calls both (§1a). Only the Fastly adapter runs
  request filters and applies their late response effects, in
  `apply_terminal_response_effects`, after `ec_finalize_response`. Which
  function is the apply point on each adapter is an open question (§8). Each
  adapter gains the call and a test proving it.
- **Ordering is three stages, and the last one is inviolable:** core
  response-header handling (the Edge Cookie module's response headers, EC
  `Set-Cookie` emission and header clearing, the operator's
  `[response_headers]`, and privacy headers) → integration operations →
  **final cache/privacy invariant enforcement**, which no integration
  operation can override. Running the hook dead-last would be wrong, because
  the code deliberately runs the cookie cache rule _after_ arbitrary header
  changes (`apply_response_headers_with_cache_privacy` runs
  `enforce_set_cookie_cache_privacy` a second time after the operator's
  headers). On a response that sets a cookie that rule removes the edge cache
  fields and writes `private, max-age=0`, unless `Cache-Control` already
  forbids shared storage (§1a gives the one exception). A hook applied after
  that recheck could combine an appended `Set-Cookie` with a replaced public
  `Cache-Control` into a **shared-cacheable cookie response**. The invariant
  pass therefore runs
  after all mutations, unconditionally, and it enforces more than the
  cookie rule. Core **snapshots the complete pre-hook cache restriction
  state**, whether the restriction came from core's own classification
  (HTML that ran the ad stack or was authorized for the template cache is
  made `no-store, private` whatever cookies it carries, and the cookie rule
  returns early without `Set-Cookie`) **or
  from the origin** (an origin-supplied `private, no-store` that core
  merely passed through), and the post-hook response may only be
  **equal or stronger** on the privacy axis: integrations can tighten
  caching, never loosen it, regardless of which header they replaced.
  "Equal or stronger" is a defined merge over **independent sticky
  directives, not a totally ordered lattice**, because `no-cache` and `private`
  are orthogonal constraints (RFC 9111: `no-cache` permits shared
  storage subject to revalidation; `private` forbids shared storage), so
  "replace `private` with the stronger `no-cache`" would make a
  personalized response shared-storable. Under the merge, each of the **six sticky directives**, `no-store`, `no-cache`,
  `private`, `must-revalidate`, `proxy-revalidate`, `no-transform`, is
  present-in-snapshot-or-mutation ⇒ present-in-final (this
  "snapshot or mutation ⇒ final" rule scopes to exactly these six), with two refinements. First, `must-understand` is the deliberate
  exception: **mutation-introduced `must-understand` is rejected**
  (snapshot-present survives untouched), because under RFC 9111
  §5.2.2.3 a cache that understands the status may then ignore an
  accompanying `no-store`, "adding" it weakens a stored `no-store`, so
  it is not additive. Second, stickiness is **form-preserving, not
  name-presence-only**: an **unqualified directive never becomes
  field-qualified, and a qualified field set never shrinks**, where
  `private` → `private="Set-Cookie"` keeps the directive name while
  authorizing shared storage of everything but one field (RFC 9111:
  qualified `private`/`no-cache` have materially weaker semantics), so
  a mutation supplying a qualified form where the snapshot is
  unqualified keeps the snapshot's bare form, and qualified snapshot
  sets may only grow; `public` is
  dropped whenever any restriction is present; **request-side authority
  is part of the invariant**. If the request carried `Authorization`
  and the origin did not itself authorize shared reuse (no `public`,
  `must-revalidate`, or `s-maxage` from the origin), an integration may
  not introduce `public`, `must-revalidate`, or `s-maxage` (RFC 9111
  §3.5 makes those the very directives that unlock shared caching of
  authenticated responses); the invariant pass forces `private, no-store` in
  that case regardless of the mutation, and where this rule meets the sticky
  rule for `must-revalidate` this rule decides, so a `must-revalidate` that a
  mutation introduces does not survive on such a request; `max-age`/`s-maxage`
  may
  only shrink relative to the snapshot; `stale-while-revalidate`/
  `stale-if-error` may appear only if the snapshot had them **and their
  durations may only shrink** (present-at-1s must not become
  present-at-1y); CDN-specific cache fields are **reserved outright, by enumerated
  name in the field registry**, being the names in `EDGE_CACHE_HEADER_NAMES`
  (`cache_policy.rs`). At `split/5-response-hook-docs` they are
  `Surrogate-Control`, `Fastly-Surrogate-Control`, `CDN-Cache-Control` and
  `Cloudflare-CDN-Cache-Control`, each individually tested ("host equivalents"
  was not a matching rule four adapters would implement identically). The code
  has no `Edge-Control`, and whether the list gains it is an open question
  (§8).
  Merging them per-directive on unrestricted responses was a hole (an
  unrestricted `CDN-Cache-Control: max-age=60` could become a year), and
  they are additionally stripped from any restricted response, and the
  final `Vary` is the **union of the complete snapshot `Vary` set**,
  origin-supplied members included, not only core-required ones, and
  the mutation. Ordering is normative so the final `Vary` reaches TS's
  own cache key, not just the wire: **mutation/invariant → final `Vary`
  computation → cache-key construction → body/metadata commit**, with
  three identity rules. Cache matching uses the **exact final publisher
  request** (post-overlay view, because keying from the redacted view would
  collapse personalized variants).

  The final `Vary` name list has one cross-adapter grammar. Core collects every
  `Vary` response field line from the snapshot plus mutation, parses each as an
  HTTP comma-list, trims optional whitespace around every member, and requires
  each non-empty member to be an RFC field-name token. It lowercases ASCII,
  removes duplicates, and sorts unique names by unsigned ASCII byte order. An
  empty member or invalid token introduced by a mutation invalidates that
  batch. If the reverted snapshot itself is malformed, the invariant replaces
  the final value with `Vary: *`, forces `no-store`, and writes no cache
  artifact. `*` in either source likewise dominates every other member, so the
  normalized result is the single `*` and is uncacheable. For an ordinary
  list, the wire response emits one lowercase `, `-joined value, while the
  variant descriptor stores `vary_names` as the exact sorted JSON array of
  lowercase strings. Thus field-line grouping, case, and input ordering cannot
  produce different cache identities.

  A normalized name nominates one request header. Digest construction obtains
  **all** values for that header from the exact final publisher request after
  overlay, preserving received field-line order and value octets. It does not
  comma-fold, trim, or split those request values. The `<name>` bytes in the
  HMAC input below are always the normalized lowercase ASCII name, and a name
  is digested once even if it appeared repeatedly in `Vary`. In the known-answer
  normalization fixture, snapshot/mutation lines `Vary: X-Tenant ,
Accept-Encoding` and `Vary: accept-encoding` produce wire value
  `accept-encoding, x-tenant` and descriptor value
  `["accept-encoding", "x-tenant"]`, then digest each nominated request
  field's original instances in their received order. Fixtures also cover
  differently grouped lines, case variants, duplicate names, empty members,
  invalid tokens, `*`, absent versus present-empty request fields, and one value
  containing a comma.

  **Every** `Vary`-nominated request value is stored **only as a keyed
  digest**, every value, not a sensitivity classification an unknown
  credential field could slip past: HMAC-SHA-256 with domain tag
  `tsvry1|`, over an input that encodes **presence, instance count, and
  length-prefixed octets**, since the earlier comma-join grammar had
  deterministic collisions (absent hashed identically to
  present-but-empty, violating RFC 9111 §4.1's absence-matches-only-
  absence, and two members `a`,`b` collided with one member `a,b`),
  which could select a representation built under a different
  credential or tenant. Absent field → `tsvry1|<name>|a`; present →
  `tsvry1|<name>|p|<count>` with `count` as ASCII decimal, then, per member in received order,
  `|<len>:<octets>` with `len` the ASCII-decimal byte count. Output
  lowercase hex (64 chars). **The key is a deployable contract, not an
  implementation detail**: the setting
  `[cache] vary_digest_key_secret_name` names a platform-secret-store
  entry containing one versioned keyring JSON object:
  `{ "schema_version": 1, "current_key_id": "<id>", "keys":
[{ "id": "<id>", "key_base64url": "<unpadded>" }] }`. `keys` is
  sorted by `id`, contains 1..4 unique entries, rejects unknown fields,
  and every value decodes to exactly 32 CSPRNG bytes. An id is derived,
  not operator-invented, and is the first 16 lowercase hex characters of
  SHA-256 over the raw key bytes. A supplied mismatch or one id bound to
  different bytes is fatal. The current id must exist in the array.
  Every cache entry stores its id. Raw keys never enter config, cache
  artifacts, logs, or metrics. Startup **fails** when response caching is
  enabled and the keyring/current key does not resolve (digests are never
  computed unkeyed).

  Lookup uses a stable per-representation **variant index** so the key ID is
  discoverable before the variant artifact is addressed. The base index key is
  the cache tuple excluding `Vary` values. Each bounded index descriptor stores
  only the normalized final `Vary` name list, key ID, corresponding keyed
  digests, artifact key, artifact expiry, and artifact revision tuple, never
  raw request values. A reader loads the index, groups descriptors by key ID,
  resolves each referenced key (one atomic keyring refresh if an ID is
  unknown), recomputes the digests from the exact final publisher request, and
  fetches only a descriptor whose complete name/digest tuple matches. A
  still-unknown ID, malformed descriptor, missing artifact, expiry, or revision
  mismatch is a miss for that descriptor, never a comparison under another
  key. Multiple matching descriptors are corruption and make the entire base
  lookup a miss with a metric. Index order never chooses a winner.

  Publication writes and verifies the immutable artifact first, then CAS-adds
  or replaces its complete descriptor in the index. Rekeying or a changed
  `Vary` set inserts the new artifact/descriptor before removing the old
  descriptor. A crash may leave a safely unreachable artifact or two
  nonmatching descriptors but cannot point to a partial artifact. Index
  capacity eviction removes expired descriptors first and otherwise the
  least-recently-used complete descriptor. It never rewrites a digest under a
  new key ID. This is the one meaning of “insert-new-then-index-update rekey” in
  the capability matrix and 304 rules.

  Rotation atomically replaces the secret entry with a new valid keyring
  containing the new current key **and all still-live previous keys**. Fleet
  propagation may be mixed only in the safe direction, where a process with the old
  keyring can write/read the old id. The variant index exposes that id before
  lookup, so a process seeing an unknown descriptor id refreshes the keyring
  once, then treats a still-unknown descriptor as a cache miss. It never
  guesses, probes with the current key, or computes unkeyed. A previous key may
  be retired only after no unexpired index descriptor references it **and** the
  maximum processed-artifact lifetime plus the adapter's qualified keyring
  refresh bound has elapsed since it stopped being current. Key IDs are never
  reused. Secret replacement atomicity, maximum propagation/refresh time, and
  unknown-id refresh are explicit adapter capability cells and startup gates;
  an unqualified adapter disables response caching rather than weakening the
  grammar. Cross-adapter fixtures cover old→mixed→new propagation, unknown-id
  refresh/miss, premature retirement rejection, and id/material mismatch.
  Known-answer vectors under the all-zero 32-byte test key:
  `tsvry1|authorization|p|1|10:Bearer abc` →
  `c880c5e8c36febc0b1581c92f1d598fded34391626e67372ed63b2857d8a7b6b`;
  absent-field form `tsvry1|x-tenant|a` →
  `a2ae26cf529a5843a25f1448acc4e90016d4c1dce0ffda5662e3ac459433e1ab`. And
  a response derived from a request carrying an **identity-bearing TS
  overlay** (the DataDome ClientID overlay) is forced `private, no-store`
  unless an explicit per-overlay contract says otherwise, because
  the `Authorization` rule protects origin credentials and this rule
  protects the identity TS itself injected. Parsing itself is a **shared core
  parser with fail-closed normalization**, not four adapter interpretations:
  a `Cache-Control` value that fails the shared grammar has an
  **enumerated result, not a "most restrictive reading"** (restrictions
  are independent axes, so no single most-restrictive point exists):
  the response is treated as **uncacheable for the storage decision**
  (`no-store`-equivalent in the invariant) and any mutation batch
  merging against the malformed value is rejected whole. Among
  well-formed values, duplicate directives keep the strongest, quoted and unquoted
  forms are equivalent, conflicting `max-age` values keep the smallest, and
  unknown extension directives are dropped **from mutations only, while
  unknown directives already in the snapshot are preserved verbatim** (a
  downstream cache may honor a restrictive extension TS does not
  recognize, so dropping it would weaken origin policy, RFC 9111 §5.2.3);
  `Expires` participates in the freshness bound via **RFC 9111 §4.2.1's
  freshness-lifetime algorithm, referenced directly**: the
  `Expires`-derived lifetime is `Expires − Date` (absent `Date` →
  response receipt time), invalid or duplicate date values are treated
  as already expired (the RFC's conservative option, chosen
  normatively), and `Age` is handled per the RFC, effective freshness
  is the minimum across `max-age`, `s-maxage`, and that derived
  lifetime and a mutation may **not introduce `max-age`/`s-maxage`
  where the snapshot supplied no upper bound**, HTTP prefers `max-age`
  over `Expires` (RFC 9111 §5.3), so introducing one would override an
  origin's shorter or already-expired `Expires`; and `Vary: *` is
  treated as uncacheable-by-shared-caches (no-store-equivalent for the
  invariant). Conformance fixtures cover each rule. Middle-stage placement also keeps
  the earlier property. An integration mutation is not silently stripped
  by ordinary core handling, only by the invariant pass, which logs the
  downgrade it applies.

## 3. Collision policy

> **Not built, and not reconciled with the code.** The cache revision tuple in
> this section has no code, and four of its fields come from the permission
> model's policy activation, which is not built (§1a). The reserved lists in
> this section are not the two lists the code has for response headers, which
> are `FRAMING_OR_HOP_BY_HOP_HEADERS` in `ec/module.rs` and
> `is_forbidden_filter_header` in `integrations/registry.rs` (§1a, §8).

- Mutators may not touch **reserved surface**, which is defined at two
  granularities because `Set-Cookie` is multi-valued: (a) reserved header
  _names_, HTTP framing and hop-by-hop headers (`Content-Length`,
  `Transfer-Encoding`, `Connection`, `Trailer`, `Upgrade`, `TE`,
  `Keep-Alive`), **freshness metadata** (`Age`, `Date`, `Expires`, since replacing `Age: 59`
  with `Age: 0` on a cached `max-age=60` response, or pushing `Expires`
  into the future, extends downstream freshness in exactly the way the
  monotonic merge forbids for `max-age`, so these are reserved outright
  rather than merged), **representation headers coupled to body bytes
  the hook cannot see** (`Content-Encoding`, `Content-Range`,
  `Content-Type`, `ETag`, `Last-Modified`, `Accept-Ranges`, and digest
  headers, since
  relabeling uncompressed bytes as Brotli, or advertising a validator or
  digest for bytes the hook never saw, corrupts responses or poisons
  caches), the `x-ts-*` namespace, and the
  consent/privacy headers core emits; (b) reserved cookie _names_ within `Set-Cookie`, `ts-ec`,
  `ts-eids`, and the other `ts-*` cookies core owns. **Cookie operations are deferred out of the v1 hook, headers only.**
  The write-side gate alone ("persistent cookies require P1") was shown
  insufficient, because it never modeled reading, using, forwarding, or
  withdrawing the cookie, so a P1-granted-then-withdrawn integration
  cookie would keep arriving on every request with nothing required to
  expire, hide, or stop egressing it, and an advertising-identifier
  cookie needs P4 the contract never expressed. Rather than ship
  "inside the permission model" as a claim the model does not back,
  `append_set_cookie` and the typed cookie builder are **deferred** to a
  follow-up spec whose entry bar comprises declared per-cookie required
  permissions, a typed authorized request-side view, stripping from
  unauthorized integration/proxy inputs, mandatory expiry on destructive
  P1 withdrawal, and startup-unique (name, domain, path) ownership.
  Integration IDs are **startup-unique, enforced**: registry
  construction rejects a duplicate ID (current code silently coalesces,
  which corrupts attribution and budgets), with a duplicate-ID test in
  the done-when. The registration's `behavior_revision` follows §2's bump
  contract. Configuration-dependent behavior is captured separately by the
  effective-config digest. Model-only activation is separate from both, so the
  **one cache revision tuple** contains exactly
  `integration_registry_revision`, `effective_config_revision`,
  `active_policy_digest`, `active_policy_ordinal`, `model_epoch`,
  `activation_generation`, and `hook_invariant_revision` from the strong active
  tuple at publication. Every processed artifact, mutation IR/read-set bundle,
  variant descriptor, and variant-index update stores that complete tuple. A
  lookup, local conditional, HEAD update, or 304 replay requires byte-for-byte
  equality with current active. In particular, the `permissions_v2` model CAS
  misses every `pre_epic_v1` artifact even though config/policy bytes did not
  change. Core also carries a nonzero
  `HOOK_INVARIANT_REVISION: u32`, bumped for every parser, merge, budget,
  cache-artifact, or invariant semantic change. The cache tuple stores all
  fields above, so a deploy cannot silently reuse old finals while a new
  restriction waits for cache expiry. Until then the
  operation set is headers-only, and `Set-Cookie` is fully reserved.
  Violations are rejected at the operation layer (§2) and
  logged at `warn` with the integration id. The reserved lists are single
  constants next to the definitions they protect, not duplicated in the
  hook.
- For non-reserved headers, the mutator API distinguishes **append** from
  **replace** explicitly. Append/replace legality comes from a **core-owned field registry**,
  not adapter judgment, and the v1 registry admits **inert fields
  only**: "headers-only" is not automatically permission-neutral, since
  `Link` (preload/prefetch), `Reporting-Endpoints`/NEL, CSP report
  directives, and `Refresh` cause browser-initiated vendor contact on
  requests that granted nothing. Fields with active egress side effects
  are **rejected in v1**; a follow-up may admit them behind declared
  required permissions gated at mutation time. Within the inert set,
  each field is classified append-legal (genuinely list-valued),
  replace-only (true singletons, e.g. `Content-Location`, `Retry-After`;
  an earlier draft miscited `Content-Language`, which is list-valued),
  or rejected; **unknown extension fields are rejected entirely in v1**
  (neither append nor replace, their side-effect class is unknowable).
  The v1 registry is enumerated here, not delegated: **admitted**,
  `Cache-Control` (monotonic merge per this section), `Vary` (union
  merge), `Content-Language` (append), `X-Robots-Tag` (append),
  `Retry-After` (replace-only), `Content-Location` (replace-only);
  everything else known is classified reserved or rejected by the rules
  above, and growing the admitted set is a spec change to this list (for cookies, see the §3 deferral above). Replacing a
  header the origin set is a deliberate act, visible in the mutator's code.
- Later registrations see earlier mutations (order = registration order,
  which is deterministic).
- **Operation-layer hygiene:** generic `append`/`replace` reject the
  `Set-Cookie` header name outright, cookies go only through
  the deferred cookie builder (when it exists), so cookie validation
  cannot be bypassed by spelling
  the header name in a generic op. Per-integration limits bound total
  operations (≤ 32), added headers (≤ 16), and added bytes (≤ 8 KiB), and
  a **cumulative final-response budget** (≤ 128 headers / ≤ 32 KiB total,
  counting `name: value` plus separators, within any lower adapter
  ceiling, and those ceilings are the enumerated capability cells **below**,
  with the counting rule fixed exactly, where counted bytes = Σ over emitted
  fields of `len(name) + 2 + len(value) + 2` (the `": "` and CRLF
  separators), validated against core's budget at startup so a batch
  that passes core can never fail only on one adapter. A snapshot
  already **over** the core budget before any mutation rejects every
  ordinary batch, the budget bounds additions and never bricks an
  over-budget origin response, which passes through and is counted;
  security follows the replacement/reserve rule below)
  bounds the sum across integrations. The budget has normative priority
  partitions, where ordinary mutators may consume at most **112 headers / 24 KiB**,
  reserving 16 headers / 8 KiB for the core-owned security channel. Ordinary
  batches remain registration-ordered within their partition. A security
  `Continue` batch uses the reserve and, if necessary, evicts whole accepted
  ordinary batches in reverse registration order until it fits. It never
  removes half a batch and never drops origin fields. Ordinary output can
  therefore never crowd out security effects. If the immutable origin head
  plus the security batch alone exceeds the full budget, the security batch
  is rejected atomically and the request follows the documented security
  fail-open path with a dedicated metric, origin fields are not silently
  sacrificed. A security `Respond` owns a replacement
  response. All ordinary mutation batches are discarded and the challenge
  is validated against the full 128-header / 32-KiB budget. A base publisher
  response already over the full budget still passes unchanged, but no
  ordinary mutation applies. Security `Continue` applies only if the final
  response fits after all ordinary batches are removed. These are separate
  outcomes and metrics.

  | Adapter    | Header-count / total-bytes ceiling (capability cell)                                                                                                       |
  | ---------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------- |
  | Axum       | no platform ceiling below the core budget (native HTTP stack), cell fixed at ≥ 128 headers / ≥ 32 KiB                                                      |
  | Fastly     | **qualification-pending**: the measured platform ceiling is recorded in this cell by the adapter-qualification commit, and unrecorded ⇒ hook startup fails |
  | Cloudflare | **qualification-pending**: same rule                                                                                                                       |
  | Spin       | **qualification-pending**: same rule                                                                                                                       |

  A recorded cell below core's 128-header / 32 KiB budget is a startup
  error (shrink the core budget or raise the ceiling, never a silent
  per-adapter divergence).

  Hook/cache eligibility has the following concrete adapter cells, and any
  `qualification-pending` cell fails startup when the depending feature is
  selected:

  | Capability                                                                                         | Fastly                                                                                                                | Axum (dev)                                          | Cloudflare                     | Spin                             |
  | -------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------- | ------------------------------ | -------------------------------- |
  | Runtime secret lookup for Vary-HMAC/DataDome                                                       | wired secret store, and qualify key-rotation behavior                                                                 | dev secret binding required                         | qualify Workers secret binding | qualify component secret binding |
  | Persisted processed artifact + mutation IR/read sets                                               | qualification-pending                                                                                                 | in-process dev implementation required, non-durable | qualification-pending          | qualification-pending            |
  | Atomic artifact/metadata entry commit                                                              | qualification-pending                                                                                                 | implementation required                             | qualification-pending          | qualification-pending            |
  | `Vary` variant index + insert-new-then-index-update rekey                                          | qualification-pending                                                                                                 | implementation required                             | qualification-pending          | qualification-pending            |
  | DataDome field-line order, trusted IP/port, fixed HTTPS backend/no-redirect, and exact form limits | qualification-pending                                                                                                 | qualification-pending                               | qualification-pending          | qualification-pending            |
  | SecurityUse JA4 request evidence                                                                   | platform value available, with exact-field/payload qualification and sign-offs 23/28 of the rollout spec's §8 pending | unavailable                                         | unavailable                    | unavailable                      |

  The qualification commit records storage lifetime, maximum object size,
  concurrency semantics, torn-write behavior, and fault-injection evidence;
  “platform has KV” is not a qualifying cell.
  `expose_host_fingerprints_to_vendor = true` also requires a qualified
  SecurityUse JA4 cell. Unsupported or pending is a startup error, while the
  default `false` remains portable.

  Each mutator receives an **immutable, redacted snapshot of the
  response head** (status and headers as of its turn, prior integrations'
  accepted operations applied) as its read context. It never holds a
  mutable reference (§2). Redaction is a security boundary, not
  tidiness, because the hook runs after core queues the EC `Set-Cookie`, so an
  unredacted view would hand a mutator the raw EC to copy into
  `X-Vendor-Identity` or its own cookie, walking around
  `AuthorizedIdentity<PartnerEgress>` entirely (§1a). The snapshot therefore
  **excludes every `Set-Cookie` value and every reserved identity,
  consent, and privacy header value** (names may be listed as present;
  values are withheld).
  Each registration also declares the complete set of response fields its
  decision may read, including status as a distinguished input. Core records
  the union with the accepted operation batch. Undeclared reads are a hard
  conformance failure in tests. An integration unable to declare a complete
  read set marks itself `revalidation = "refetch"`, which forbids IR replay
  after any origin metadata change.
  Operations arrive as **attributed batches bound to a registration
  ID**, one batch per integration per response, ordered by
  registration, with the security channel's batch (§4a) ordered **after**
  ordinary response mutators, one global order, core finalization →
  ordinary mutators → security effects → invariant pass, so the
  security layer's precedence over publisher-facing mutations holds through
  both position and its reserved/response-owning budget rule. The current flat effects vector satisfies neither
  attribution nor budgets and will be restructured accordingly. Validation
  and budgeting are **atomic per batch**: a batch that exceeds its
  budget is rejected whole (logged, attributed), never partially
  applied, since item-by-item rejection could apply a security 302's
  `Set-Cookie` while dropping its `Location`. The response itself is
  never rejected. A mutator that returns an error is skipped in full, its
  operations are all-or-nothing, and the response proceeds without it.
  **Panics are forbidden and fatal, not recoverable**: the primary target
  (`wasm32-wasip1`) has no unwinding support, so there is no unwind
  boundary to catch at, a spec that promised panic recovery would be
  unimplementable there. Mutators are infallible-by-construction or
  return `Result`; a panic is a bug that takes the instance down, same as
  anywhere else in the request path.

## 3a. Response eligibility, normative

The rows below for cache hits, `304` and `HEAD` describe a processed-document
cache that is not built. The code ships the template cache instead, and this
spec does not say which of the two the hook extends (§1a, §8). The recovery
fetch below removes six request fields. The code removes four of them,
`If-None-Match`, `If-Modified-Since`, `Range` and `If-Range`, and only when the
ad stack runs or the DataDome client tag is suppressed on an HTML page. The
code answers an origin `304` on the ad stack path with a `502`.

Which responses the hook runs on, enumerated so two implementations cannot
diverge silently:

In this table, **persisted post-hook finals** means the cache-safe ordinary
artifact only, comprising origin metadata plus accepted ordinary mutation IR and the
cache/privacy invariant result, with `Set-Cookie`, core request-specific
identity fields, security-channel effects, and origin validators excluded.
The security request filter evaluates every request before cache selection. A
fresh `Respond` bypasses the artifact, while a fresh `Continue` batch is
applied to the persisted ordinary artifact and the invariant pass reruns before
emission. This applies equally to ordinary hits, local conditionals,
origin-revalidation 304s, and HEAD. Therefore "`Set-Cookie` is never replayed"
means never replayed from storage. A freshly validated per-request typed cookie
operation may still emit on that response. Security `Respond` outputs are
always `private, no-store` and never become artifacts.

An **unconditional recovery fetch** first saves the client's preconditions for
later local evaluation, then removes every upstream conditional/range field
(`If-Match`, `If-None-Match`, `If-Modified-Since`, `If-Unmodified-Since`,
`If-Range`, and `Range`) so origin must return full bytes. TS processes that
200 under current revisions, constructs the processed validator, and only then
evaluates the saved client preconditions as the authoritative server for the
transformed representation. Another bodyless origin 304 can never satisfy
recovery.

The 304 **safe-update set** is exactly `Cache-Control`, `Expires`, `Date`,
`Age`, `Vary`, and the reserved CDN cache fields (the names in
`EDGE_CACHE_HEADER_NAMES`, §2). The phrase
"registry-admitted mutable fields" in the matrix denotes an empty set in v1;
adding any name requires a reviewed spec/registry revision and conformance
fixture, never a runtime wildcard.

| Response                                                             | Hook runs?                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| -------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Processed HTML document (rewritten by TS)                            | Yes                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| Streamed processed document                                          | Yes, operations apply to the header block before first byte                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| Pass-through proxy response (not processed)                          | No, TS is a transparent proxy for it                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| Served-from-cache processed document                                 | Serves the **persisted post-hook finals** captured at cache fill (revision-versioned, mismatch = miss), mutators run at fill/processing time only, so a normal hit and a conditional hit return identical policy metadata **by construction**, with no purity or determinism assumption about mutators                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| Redirect (3xx)                                                       | No                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| Error responses TS itself generates (4xx/5xx)                        | No                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| Origin response with a 3xx, 4xx or 5xx status and a processable body | Not decided. The code processes these bodies (§1a), and the matrix gains a result for them when the question in §8 is settled                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| `304 Not Modified` for a processed representation                    | **Persisted-metadata pass**: a cached processed 200 stores its final post-hook headers, accepted mutation-operation batches, and the union of every mutator's declared response-field read set (the persisted mutation IR), versioned by §3's complete cache revision tuple, including model epoch and logical activation generation. A local conditional hit re-emits persisted finals only when the complete tuple matches current active. An origin-revalidation 304 is staged and diffed against separately stored origin-side metadata. (a) Any byte-coupled field changed (`Content-Encoding`, `Content-Type`, validators, digests) → invalidate and fetch/process a full 200. (b) A changed metadata field that intersects any persisted mutator read set, or an artifact/mutator lacking a complete read-set declaration, is also unsafe → full 200 refetch and ordinary hook execution, because deterministic replay of old operations cannot stand in for re-evaluating a decision made from changed inputs. (c) If every changed field is outside every declared read set and belongs to the enumerated safe-update set (`Cache-Control`, reserved CDN cache fields, `Expires`, `Date`, `Age`, `Vary`, registry-admitted mutable fields), replay the persisted deterministic operations over updated origin metadata and rerun invariants. Updated origin metadata, finals, IR, read sets, and complete revision tuple publish in one atomic entry commit, and changed `Vary` uses insert-new-entry-then-index-update ordering. (d) No change → re-emit persisted finals. Artifact absence or any tuple mismatch triggers an unconditional recovery fetch so TS obtains bytes. For processed-document GET/HEAD routes, TS is explicitly the authoritative server for the transformed representation, and after processing the full 200 it evaluates RFC 9110 §13 preconditions against **processed** validators. This is not evaluation of origin validators by an intermediary cache. Other methods are never eligible for this recovery path. `Set-Cookie` and origin validators are never replayed. Absent metadata → cache miss |
| `HEAD` of a processed document                                       | **Serves the persisted GET artifact when one exists**, parity with GET/304 by construction, since RFC 9111 §4.3.5 lets HEAD metadata update a stored GET response and mutators are not required to be deterministic. With no persisted artifact, HEAD is processed like a GET (headers only) and its finals stored as a **distinct head-only artifact type that never satisfies a later GET**, when a stored GET artifact exists a HEAD may **update** it only when the comparison, made against the separately stored **origin-side** metadata, never the processed artifact's (origin and rewritten lengths differ by construction, so the processed length is never the comparand and HEAD metadata never touches processed-side headers), finds validators and origin `Content-Length` matching and no byte-coupled representation field changed (RFC 9111 §4.3.5); qualifying updates land origin-side under the same atomic-commit discipline as the 304 rules, and any mismatch **invalidates** the stored GET artifact rather than updating it                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| Informational `1xx`, `204`, `205`, `206`                             | No, enumerated so adapters do not infer independently                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |

This deliberately narrows #782's general "outbound response" phrasing to
processed documents (§6).

## 4. Done-when (from #782, sharpened)

1. Trait + builder + registry application, each public item documented.
2. **The old generic `RequestFilterEffects.response_headers` channel is
   removed.** DataDome uses the separate sealed, core-registered
   `DataDomeSecurityRequestFilter` and typed `DataDomeSecurityEffects` defined
   by §4a's PR-specific delta. Generic request filters
   receive only `RedactedRequestView` and ordinary attributed effects and
   cannot express the security view, owner overlay, cookie operation, or
   reserved security header. The dedicated channel is necessary because
   DataDome sets headers **and cookies** on 200, 301/302, 401, 403, and 429
   responses, response classes (§3a) the ordinary response hook never runs
   on, with cookie emission v1 reserves.
3. **At least one real consumer ships in the same PR**, an existing
   integration registering a mutator for a real need (or, failing a real
   need, the feature waits, and scaffolding with only self-referential tests is
   dead code and will be removed).
4. Every adapter applies mutations on its outbound path, with a per-adapter
   route test asserting an integration-set header appears in the response.
5. A parity-suite case asserts identical mutation behavior across adapters.
6. Reserved-surface, append/replace, operation-limit, duplicate-ID, and
   erroring-mutator semantics covered by unit tests.
7. **Every row of the §3a eligibility matrix has a test**, streaming,
   cache-hit, pass-through, redirect, error, and 304 each proven to run or
   not run the hook, not merely one positive header test per adapter. The row
   for an origin 3xx, 4xx or 5xx page gains its test when its result is
   decided (§8).
8. Cache/privacy invariant tests, one per restriction source and shape:
   a **core-owned** cookie already queued before the hook + an
   integration's public `Cache-Control` replacement → private/no-store,
   surrogate stripped; **core-private cookieless** processed HTML +
   public replacement → restriction preserved; **origin-private cookieless** processed HTML that retained the
   origin's cache restrictions + public replacement → restriction
   preserved (pass-through responses never run the hook, §3a); a cache-hit serve re-applying mutations without
   weakening the stored classification; a `Vary` mutation neither
   dropping core-required values nor bypassing the snapshot; the §3
   normalization fixture proves field-line grouping/case/order collapse to one
   sorted lowercase descriptor, repeated names digest once, request value
   instances retain octet/order boundaries, malformed/empty mutation members
   reject, a malformed snapshot becomes `Vary: *` plus `no-store`, and `*`
   writes no artifact; a `pre_epic_v1` artifact/index descriptor is a miss
   immediately after the model-only `permissions_v2` activation CAS even when
   config and policy digests are unchanged; each name in
   `EDGE_CACHE_HEADER_NAMES` individually stripped; and a rejected
   `Content-Encoding` mutation.

The `pre_epic_v1` test and the other tests of the cache revision tuple cannot
run until the permission model's policy activation exists (§1a). The work
extends tests that exist at `split/5-response-hook-docs`. For the cookie and
privacy rules these are the tests in `response_privacy.rs` (for example
`downgrades_operator_configured_set_cookie_with_public_cache_headers`,
`preserves_private_no_store_against_operator_cache_headers_without_cookie` and
`terminal_private_stamp_removes_every_cache_and_validator_header`), the
`apply_finalize_headers` tests in each adapter's `src/middleware.rs`, and the
Fastly tests of `apply_terminal_response_effects` in
`crates/trusted-server-adapter-fastly/src/main.rs`
(`late_filter_effects_cannot_make_an_assembled_response_public`,
`late_filter_effects_cannot_make_a_page_bids_response_public`,
`late_filter_effects_cannot_make_an_active_diagnostics_response_public` and
`terminal_response_preserves_unmarked_origin_private_policy`). For DataDome they
are the tests in `integrations/datadome/protection.rs`, from
`extract_header_mutations_appends_set_cookie_and_sets_other_headers` to
`classify_ok_response_preserves_request_header_effects`.

## 4a. The security channel, normative closed boundary

Nothing in this section is built (§1a). Sign-offs 23 and 28 are rows 23 and 28
of the table in §8 of `2026-07-30-provider-migration-rollout-design.md`. Row 23
permits a narrow `SecurityUse` authority for DataDome only, and row 28 requires
product and written vendor conformance approval of the reduced DataDome
surface. Both rows are open, and each says the hook is not shipped.

The security channel (today: DataDome) runs under a distinct, typed
`SecurityUse` authority rather than the advertising permissions P1/P4.
`SecurityUse` permits bot/fraud evaluation and challenge continuity only;
it never authorizes TS-controlled advertising identity, graph linkage, partner egress,
other integrations, or general raw-value observability. Request-scoped raw
security evidence may be disclosed only to the fixed DataDome Protection API
endpoint and only from that integration's explicit field allowlist. It is not
persisted in the identity graph, exposed to publisher origin or other
integrations, or emitted in logs. It carries its own configured retention and
deletion path. An advertising opt-out does not erase a
strictly security-scoped identifier, while an authenticated deletion request
or expiry under the security retention policy does. This is not a general
exception. The path-only `Request` remains publisher-originated data and is
explicitly covered by vendor retention/DSR sign-off. Query strings and full
referrers are never in the security view. Every degree of freedom is closed:

- **Host evidence is not a back door to the device module.**
  `[device] module = "fastly"` is an explicit opt-in selection (#1044) and
  the hook does not widen it, and no JA4-derived classification is stored by a
  mutator. If DataDome's Protection API is allowed to receive
  request-scoped `TlsProtocol`/`JA4` evidence, its registration enumerates each field,
  proves vendor necessity and payload bounds, and keeps it ephemeral under
  `SecurityUse`; those exact consumers and fields are part of product/vendor
  sign-offs 23/28. `TlsCipher` is omitted because the platform value has
  different semantics from the vendor field, `H2Fingerprint` is not in the
  vendor contract, and all other host evidence is omitted.
- **Deletion and retention name the system boundary honestly.** TS stores no
  server-side DataDome identifier mapping. On an authenticated TS deletion
  request it excludes the route from vendor validation, emits a typed
  `datadome` cookie deletion, and sends no ClientID to DataDome for that
  request, and re-presentation retries deletion. A lost browser response can leave
  the cookie until its configured `security_cookie_max_age`, which is the
  bounded residual sign-off 23 accepts. This operation does **not** claim to
  erase data already held by DataDome, because vendor-side retention and data-subject
  deletion require a named contractual/API procedure in the decision record.
  If no such vendor procedure exists, operator documentation says so and may
  not describe TS cookie deletion as vendor-data deletion.

- **Typed security-cookie operation with a concrete lifecycle, not
  header strings.** The channel emits cookies only through a typed
  operation, and the registration is not a placeholder, for DataDome
  it pins, **aligned to documented vendor behavior where hardening was
  not intended**: cookie name exactly `datadome`; one configured ownership
  tuple for every set and deletion, with path exactly `/` and
  `security_cookie_domain = "host-only"` (default) or one explicit normalized
  ASCII domain. Host-only mode requires the vendor `Domain` attribute to be
  absent. Explicit-domain mode requires it to equal the configured domain
  exactly, TS never accepts a different domain and never rewrites one scope
  into another. The explicit value cannot exceed the registrable domain,
  computed against the **vendored
  Mozilla PSL snapshot in §4a.1**,
  ICANN + private sections, IDNA-mapped; IP-literal or single-label
  hosts fall back to host-only), path `/`; `Secure` mandatory;
  `SameSite` configurable `Lax` (default) / `Strict` / `None`
  (`None` requires `Secure`), matching the vendor's endpoint options;
  the configured/returned `Domain` must additionally **domain-match the current
  request host** per RFC 10025, the current cookie specification obsoleting RFC 6265 (domain-match and the PSL boundary check
  are separate requirements) and a vendor cookie using `Expires` is
  normalized to its Max-Age equivalent (both present → `Max-Age` wins,
  per RFC 10025); a normalized lifetime exceeding the ceiling rejects
  the whole operation batch. The parser is total, where repeated `Cookie`
  request fields are joined with `"; "` (semicolon-space, the
  order-preserving join of RFC 10025) before parsing, and
  **duplicate `datadome` pairs after the join make the request-side
  identity ambiguous, so they are treated as cookie-absent for the vendor call and
  counted**, while cookies under other names pass through untouched;
  `Set-Cookie` fields are **never combined**, and a vendor response
  carrying more than one `datadome` `Set-Cookie` field, a `Set-Cookie`
  for any other name (the typed operation pins the name exactly), an
  **`HttpOnly` attribute** (the vendor requires script readability, so
  its presence is a protocol anomaly), duplicate attributes, duplicate
  cookies in one field, an unparseable `Expires`, or unknown attributes
  each reject the operation batch (the vendor cookie is well-formed;
  strictness is safe). `Expires` normalizes as
  `Max-Age = max(0, floor(expires − now))` whole seconds on the server
  wall clock at parse time (the shared skew-bounded basis, where a result of
  0 is a deletion), and the 512-byte limit measures the **normalized**
  serialized `name=value` plus attributes in bytes;
  `Max-Age` is capped by required operator configuration
  `security_cookie_max_age` in the vendor-supported range 7 days through
  **31,536,000 seconds** (one year); the returned cookie may be shorter but
  never longer. There is no silent one-year default. Size ≤ **512 bytes**
  (DataDome's current Fastly-module limit; 4 KiB was ours, not theirs).
  Where the contract **is** deliberately narrower than the vendor, the
  spec-pinned pointer allowlist starting at ClientID-only against
  DataDome's mandatory response-directed mapping set, that reduction
  needs explicit product **and vendor** acceptance under sign-off item 28. A
  violating operation is rejected whole (the batch rule). **`sessionByHeader`
  is startup-rejected in v1, one state, not three**:
  TS never sends `X-DataDome-X-Set-Cookie: true`; a vendor
  `X-Set-Cookie` **invalidates the batch → Continue** (its matrix cell, since
  a session mode the fleet never requested must not half-apply); and an incoming
  browser `X-DataDome-ClientID` is **not forwarded to the vendor**
  (cookie-only session identity, an earlier revision translated
  `X-Set-Cookie` into an ordinary cookie, which is not equivalent, because
  header-session clients expect JavaScript to receive `X-Set-Cookie`
  and `X-DD-B`, and the higher-priority header session would never see
  a cookie update. The older DataDome spec's "always send
  X-DataDome-X-Set-Cookie when the header ID is used" is superseded for
  v1 by this section). Supporting header mode later means the full vendor
  protocol, typed owner-scoped `X-Set-Cookie`/`X-DD-B` forwarding,
  CORS exposure, and a JavaScript/local-storage identifier observer,
  as an explicit opt-in under **sign-off 23**. Every `ts-*`
  name is rejected. **Read is owner-only across every _server-side_ surface, and the
  browser side is explicitly not ownable**: DataDome requires the
  cookie to be readable by its JavaScript and warns against `HttpOnly`,
  so **every same-origin page script can observe it**, and a Respond
  serves vendor-owned HTML under the publisher origin (vendor scripts
  with same-origin access to cookies, storage, and APIs, while publisher CSP
  can conversely break the challenge). Those browser-side observers,
  same-origin vendor code, CSP interaction, and challenge redirects
  enter **sign-offs 23/28** for ratification (both decision records
  still open), not implied. The server-side strip
  inventory is exhaustive, not integration-scoped, because the browser sends `datadome` in the ordinary
  `Cookie` header, so it is removed from **every non-DataDome surface**:
  other integrations' request views, publisher-origin proxy forwarding,
  proxy/click/Testlight upstreams, auction/page-bids request
  serialization, and logs (redaction list), each surface a tested row
  of the inventory. Only the security channel itself observes it. Vendor
  egress goes only to the fixed DataDome Protection API authority and path.
  Redirects are not followed. Deletion is always possible
  through the `SecurityUse` lifecycle, and advertising withdrawal never
  grants access to or reuses the identifier. No other request filter
  inherits the cookie capability.
- **Cookie ownership makes deletion total for the scope TS creates.** While
  DataDome is enabled, `datadome` is a security-owned name across the final
  response, and before the security batch applies, core removes and meters every
  origin, core, cached, or ordinary-mutator `Set-Cookie` for that name.
  Unrelated cookie names remain separate field lines. Only the typed security
  operation may emit it. Authenticated deletion emits the same configured
  `(name, domain mode/domain, path)` tuple with `Max-Age=0`; it does not guess a
  scope from the request cookie, whose wire form carries no Domain or Path.
  Candidate validation rejects a change of domain mode/domain while the
  previous active DataDome configuration can still have a live cookie. The
  supported migration is disable + wait at least the previous
  `security_cookie_max_age` + activate the new scope. A faster scope change
  requires a separate bounded deletion-fan-out design. The permission spec
  §5.5 whole-settings serve fence applies before cookie processing. A bounded
  old-generation admission validation may survive only during the pre-promotion
  drain. The register's promotion-not-before plus member quiescence proves it
  and every admitted effect ended before the activation CAS. After that CAS,
  no instance may emit, refresh, or delete a `datadome` cookie until it has
  loaded and leased the exact new active tuple. A stale instance stops at serve
  admission rather than extending the old scope. Fixtures cover origin
  collision, host-only vs explicit-domain set/delete, attempted domain change,
  and duplicate request cookies. The deletion claim therefore covers every
  cookie this contract can create, not arbitrary pre-contract scopes. The
  fence, the register and the activation CAS named in this bullet belong to the
  permission model's policy activation, which is not built (§1a).
- **The incoming `X-DataDome-ClientID` request header is owner-only,
  like the cookie.** DataDome prioritizes the header over the cookie,
  so leaving it in the shared request would hand other integrations and
  upstream routing the same identifier the cookie boundary strips, so core
  **removes it from the shared request** before integrations and
  upstream routing run, it joins `RedactedRequestView`'s enumerated
  strip set (providers spec, §1a), **and in v1 it is stripped for the
  vendor too, because the Protection API request's ClientID derives only from
  the `datadome` cookie, never from the incoming header** (DataDome's
  contract requires `X-DataDome-X-Set-Cookie: true` whenever a
  header-supplied ClientID is forwarded, so forwarding the header under
  cookie-only mode would misdeclare the session mode); observed header
  occurrences are counted, and a **fixture pins the path**: a request
  carrying both cookie and header produces a vendor payload whose
  ClientID equals the cookie value, with no header-derived identity
  sent. Only DataDome-returned overlay data reaches the publisher,
  never the raw browser-supplied header.
- **The pointer protocol has a total parser contract**, adapters
  cannot differ where malformed batches fail open, where the pointer list is
  tokenized by the vendor's documented space separation, repeated
  pointer header fields are concatenated with a single SP before
  tokenizing, tokens split on runs of SP/HTAB, empty tokens ignored,
  then names are ASCII-lowercased before duplicate detection. Duplicate
  names after normalization, invalid names, more than 16 pointers, or
  more than 4 KiB of pointer payload render the batch invalid
  (→ Continue, the vendor's fail-open). **Pointed-field multiplicity is
  closed**: for singleton fields (`Location`, `Content-Type`,
  `X-DataDome`, `X-DD-B`, `X-Set-Cookie`) more than one instance in the
  vendor response invalidates the batch atomically, never a
  first/last/join choice an adapter makes. List-valued fields
  (`Cache-Control`, `Pragma`) are joined per RFC 9110 §5.3 before their
  matrix outcome applies. `Set-Cookie` multiplicity follows the typed
  cookie rule (exactly one `datadome` field, above). No both-source
  priority rule exists in v1: the header session form (`X-Set-Cookie`)
  is matrix-governed as batch-invalid, so "header form wins" is
  unreachable and deleted.
- **Request-header pointers are a positive, enumerated allowlist with no
  default publisher-origin identifier exposure.**
  "Documented enrichment headers" is not enforceable. The registration
  enumerates the exact names from the **inline DataDome field contract in
  §4a.2**, spec-pinned
  today to exactly **`X-DataDome-ClientID`**, admitted only when the
  operator explicitly sets
  `[bot-protection.datadome] expose_client_id_to_origin = true` (default
  `false`); every other `X-DataDome-*`
  field is rejected until a reviewed commit adds it to §4a.2
  ("documented enrichment set, listed one by one" without an actual list
  was a wildcard whose contents could change outside the spec),
  resolving what was a contradiction. When the opt-in is false, the
  vendor-returned ClientID is discarded and the publisher origin is not
  an identifier observer. When true, it applies only to an owner-scoped
  publisher-upstream overlay, never the shared request. Startup logs the
  additional consumer, operator documentation must disclose its purpose
  and retention, and a fixture proves no other surface can read it.
  Everything else, authentication,
  `Cookie`, `Forwarded`/`X-Forwarded-*`, other identity, consent, and
  routing-authority fields, is rejected by name and by class, because a
  compromised endpoint must not replace origin credentials, inject
  `ts-ec`, or spoof client location.
- **Browser-response headers are decision-scoped through the single
  matrix, and only there.** This spec carries **no per-decision field
  list**: every pointer's outcome per decision and session mode,
  `Location`'s Respond-only 3xx admission, `Pragma`'s
  drop-individually middle path (response `Pragma: no-cache` has no
  standardized meaning, RFC 9111 §5.4), and the batch-invalidation
  default for unlisted names, is exactly one cell of the matrix in
  §4a.2.3. The fail-open consequence of batch
  invalidation stays within sign-off 28's scope, and both documented
  vendor responses are verbatim fixtures at the matrix.
- **Representation rules are decision-scoped and narrow.** A _Respond_
  decision (challenge/deny) owns its body but may describe it with
  **`Content-Type` only**, encoding and validator fields
  (`Content-Encoding`, `ETag`, `Last-Modified`, digests) stay reserved
  even for Respond, because challenge bodies are simple and uncacheable, the
  allowlist does not admit those fields, and ambiguity here decides
  whether a challenge enforces or silently fails open (batch rejection →
  Continue). If the vendor ever requires more, it arrives as a reviewed
  §4a.2 field-contract addition. A _Continue_ decision may not touch
  representation metadata of publisher bytes.
- **Respond transport is bounded, with exact measurement points.** The
  challenge body has a maximum size (64 KiB) and a **complete-response
  deadline of 3000 ms on the instance's monotonic clock, measured from
  immediately before vendor-backend acquisition/dispatch to the final
  body byte**, meaning connection setup and request send are inside the
  window, and the 1500 ms first-byte bound (the older spec's figure, now
  first-byte only) runs from the same origin on the same clock. At
  expiry the decision is final (batch fails → Continue) and the vendor
  request is canceled. Cancellation and resource cleanup complete
  asynchronously and never delay the response. TS sends
  `Accept-Encoding: identity`, and because that does not _guarantee_
  identity coding, a response arriving with any `Content-Encoding` is
  itself batch-invalid; `Content-Length` is recomputed from the actual
  bytes before Respond commits. **On a HEAD request, Respond validates
  the challenge body exactly as for GET (size, deadline, encoding) but
  emits no body**: the outward response **omits `Content-Length`** and
  carries no content, RFC 9110 §9.3.2 permits `Content-Length` on
  HEAD only when it equals the equivalent GET body's length, and the
  vendor does not guarantee method-invariant challenge bodies, so the
  validated HEAD bytes cannot establish the GET length (a
  vendor-guaranteed equivalence, if ever ratified, may restore the
  field as a reviewed change, and the older DataDome spec's HEAD handling
  remains superseded). Exceeding
  size, first-byte, or total deadline fails the batch → Continue.
- **One pointer contract, one place.** The single normative
  decision × session-mode × pointer matrix lives in **§4a.2.3**, this
  spec's earlier inline
  decision-scoped list and outcome list are deleted in its favor
  (duplicated lists disagreed about `X-Set-Cookie`, `X-DataDome`,
  `X-DD-*`, and `Pragma`, letting one conforming implementation accept
  the vendor's documented `Set-Cookie X-DD-B` allow-example while
  another invalidated the whole batch). No `X-DD-*` wildcard exists:
  every name is enumerated, `X-DD-B` included and forwarded exactly once
  as the vendor's documented cookie-mode browser-response signal. It is
  never copied into publisher-upstream or another integration. The
  documented vendor responses (both the challenge example and the
  `Set-Cookie X-DD-B` allow example) are **verbatim fixtures asserting
  the decision survives** and exactly the mapped fields emit.
- **Every security Respond ends uncacheable, unconditionally.** After
  the decision's fields are applied, the invariant pass forces
  `Cache-Control: private, no-store` and strips all CDN cache fields on
  **every** Respond regardless of status, vendor pointers, or cookie
  emission, a cookie-less `301` challenge with no effective vendor
  cache header could otherwise be heuristically stored and served to
  unrelated clients.
- **One global order:** core finalization → ordinary mutators →
  security effects → **final cache/privacy invariant pass,
  unconditionally last**. Security precedence over publisher-facing
  mutations comes from its position, not a "wins" rule. Nothing outranks
  the invariant pass, or a challenge could combine `Set-Cookie` with
  public caching. The older DataDome spec's "applies last, after
  finalization" wording is **superseded by this order**. That baseline remains
  unchanged. This PR-specific section prevents an implementation from placing
  DataDome after the invariant pass and reopening the
  public-cache-plus-cookie bug.
- The channel adopts the shared layers: structured attributed batches
  (§3, atomic per batch, a 302 must never lose `Location` to a budget
  while keeping its cookie), reserved header names, budgets, and the
  invariant pass, with one sequencing rule fail-open depends on, where the
  complete challenge batch is **validated and budgeted before the
  Respond decision commits**, so a rejection converts to Continue while
  the publisher route is still available, because discovering the rejection
  after Respond has short-circuited routing would leave nothing to fail
  open _to_.

### 4a.1 Public Suffix List snapshot (normative)

The vendored Mozilla PSL revision used for registrable-domain
computation in §4a, and the implementation PR vendors the list file
and records its upstream commit hash here. Rules: ICANN **and** private
sections apply, hostnames are IDNA-mapped before matching, and IP literals
and single-label hosts have no registrable domain (cookie falls back to
host-only). Updating the snapshot is a reviewed spec change.

| Field                  | Value                                                                |
| ---------------------- | -------------------------------------------------------------------- |
| Upstream repository    | `publicsuffix/list`                                                  |
| Upstream commit        | `e1b8015c3b2f0f4f8c18659c2480fc1a22c07b20`                           |
| Upstream source path   | `public_suffix_list.dat`                                             |
| Required vendored path | `crates/trusted-server-core/data/public_suffix_list.dat`             |
| Required hash path     | `crates/trusted-server-core/data/public_suffix_list.sha256`          |
| Required provenance    | `crates/trusted-server-core/data/public_suffix_list.provenance.json` |

The implementation copies the source bytes at that commit without editing
and writes the lowercase 64-hex SHA-256 plus one trailing LF (no filename or
other fields) to the required hash path. CI verifies the bytes, hash, and
commit reference together. Updating any one without the others fails. The
provenance file is canonical JSON with exactly
`{upstream_repository, upstream_commit_oid, upstream_commit_tree_oid,
source_path, source_blob_oid, source_sha256_hex}`. The vendoring PR description
quotes the same commit/tree/blob values and the independent command output
that verified the raw upstream SHA-256 and byte-for-byte vendored copy. A
commit OID without its tree and source-blob witness does not satisfy the
release gate.

### 4a.2 DataDome field contract (normative)

Adding or changing any name here is a reviewed spec change. This subsection
holds the **only** normative Protection API request-field and response-pointer
lists; §4a carries no duplicate or per-decision lists of its own.

#### 4a.2.1 Protection API request fields (browser request → DataDome only)

`SecurityUse` admits only the fields below to the configured DataDome
Protection API endpoint. They are request-scoped and are never persisted in
the identity graph, copied to publisher upstream or another integration, or
logged as raw values.

The endpoint is the fixed core-owned
`https://api-fastly.datadome.co/validate-request`; “configured endpoint” in
this subsection means that DataDome protection is enabled, not that an operator may
supply an authority. Redirect following is disabled. No TS-controlled
advertising identifier, consent-store key, graph value, request query, or full
referrer is admitted. The normalized publisher URL path remains disclosed and
may itself contain publisher-chosen data. Sign-offs 23/28 must classify that
surface, its retention, and DSR handling rather than calling the entire URL
identity-free.

Core-derived fields:

- `Key`, `IP`, `Method`, `Protocol`, `Host`, `ServerHostname`, `Request`
- `RequestModuleName`, `ModuleVersion`, `TimeRequest`, `Port`
- `ServerName`, `ServerRegion`
- `ClientID` from the single unambiguous `datadome` cookie only. The form key
  is always present because the Protection API declares it mandatory. Its
  value is the empty string when no unambiguous cookie exists
- `CookiesLen`, `AuthorizationLen`, and `PostParamLen` as lengths only
- `HeadersList`, containing only the source header names admitted by the
  next list plus `authorization`, `content-length`, and `cookie` (whose values
  remain length-only/ClientID-only above). Names are lowercased, comma-separated,
  and retain received field-line order, including repeated admitted names;
  arbitrary/custom header names are excluded. An adapter that cannot preserve
  received header order does not qualify this integration until DataDome
  approves a canonical replacement order in sign-off 28

Core derives those fields identically on every qualified adapter:

- `Key` is the resolved DataDome server secret and is never obtained from
  request/config text; `IP` and `Port` are the remote address and TCP source
  port from trusted connection metadata. Missing `Key`, `IP`, or `Port` skips
  the call through the metered fail-open path. No sentinel is synthesized
- `Method` is the validated HTTP method token; `Protocol` is exactly `http` or
  `https` from the adapter request URI; `Host` is the normalized ASCII request
  authority with a non-default port retained; `ServerHostname` is trusted TLS
  SNI/local-host metadata, omitted when unavailable
- `Request` is only the URL path. Empty path becomes `/`; dot segments are
  removed, percent escapes are preserved without percent-decoding and
  normalized to uppercase hex, and the complete query and fragment are
  discarded before the security view exists
- `RequestModuleName` is the literal `trusted-server`; `ModuleVersion` is the
  build's checked-in Trusted Server version; `TimeRequest` is the request-ingress
  Unix timestamp in decimal microseconds, captured once before integration
  processing and constrained to `0..=2^53-1`
- `ServerName` is the adapter-qualified deployment/service name and
  `ServerRegion` is its adapter-qualified region code. Either is omitted when
  the platform cannot supply it without request input
- `CookiesLen`, `AuthorizationLen`, and `PostParamLen` are decimal byte counts
  of the received field/body surfaces before redaction. Overflow beyond an
  unsigned 64-bit count skips the call. It never wraps or truncates

Exact request-header value mappings:

- `Accept` ← `accept`; `AcceptCharset` ← `accept-charset`;
  `AcceptEncoding` ← `accept-encoding`; `AcceptLanguage` ← `accept-language`
- `CacheControl` ← `cache-control`; `Connection` ← `connection`;
  `ContentType` ← `content-type`; `From` ← `from`; `Origin` ← a successfully
  parsed `origin` serialized as scheme + ASCII host + non-default port only;
  `Pragma` ← `pragma`; `Referer` ← a successfully parsed `referer` reduced to
  scheme + ASCII host + non-default port only; `UserAgent` ← `user-agent`;
  `Via` ← `via`
- `SecCHDeviceMemory` ← `sec-ch-device-memory`; `SecCHUA` ← `sec-ch-ua`;
  `SecCHUAArch` ← `sec-ch-ua-arch`; `SecCHUAFullVersionList` ←
  `sec-ch-ua-full-version-list`; `SecCHUAMobile` ← `sec-ch-ua-mobile`;
  `SecCHUAModel` ← `sec-ch-ua-model`; `SecCHUAPlatform` ←
  `sec-ch-ua-platform`
- `SecFetchDest` ← `sec-fetch-dest`; `SecFetchMode` ← `sec-fetch-mode`;
  `SecFetchSite` ← `sec-fetch-site`; `SecFetchStorageAccess` ←
  `sec-fetch-storage-access`; `SecFetchUser` ← `sec-fetch-user`
- `X-Requested-With` ← `x-requested-with`

Request-field multiplicity is normalized **before** parsing, truncation, and
form encoding, and adapters expose every received field line rather than a
preselected first/last value. Every admitted value line must contain valid HTTP
field-value octets **and** valid UTF-8 after OWS removal, otherwise the vendor
call is skipped through the metered fail-open path, because adapter-specific
byte-to-string replacement is forbidden:

- The list-valued source fields are exactly `accept`, `accept-charset`,
  `accept-encoding`, `accept-language`, `cache-control`, `connection`,
  `pragma`, `via`, `sec-ch-ua`, and `sec-ch-ua-full-version-list`. Core removes
  leading and trailing optional whitespace from each field value, rejects a
  value containing invalid field-value octets, and combines all field lines
  (including empty values) in received order with the two literal bytes `, `.
  This one normalized value is then parsed where the mapping above requires
  parsing and is then bounded. Commas inside an individual value are not split
  and reserialized.
- Every other admitted value-bearing source header in the exact mapping above
  is presented as received. Zero lines means omit the DataDome field. Each
  received line has its leading and trailing optional whitespace removed and
  is then checked for valid field-value octets and valid UTF-8, exactly as
  above. Two or more lines, identical or not, are preserved as separate lines
  in received order and reach the integration that way. Core does not drop a
  line, comma-join the lines, choose the first or the last, or skip the
  vendor call because a field arrived more than once. This covers `origin`,
  `referer`, `user-agent`, `content-type`, `from`, every remaining
  `sec-ch-*`/`sec-fetch-*` field, and `x-requested-with`.
- **Why core does not decide this.** Header multiplicity is attacker
  controlled, so a core rule that skipped the vendor call whenever an
  admitted field arrived twice would let any client turn bot detection off
  by sending one header twice. That is a bypass this document would have
  created rather than described, because nothing in the shipped code behaves
  that way. The protection path reads each mapped field through a single
  `headers().get()` in `header_value`
  (`crates/trusted-server-core/src/integrations/datadome/protection.rs`, and
  `crates/bot-protection/datadome/src/protection.rs` from
  `split/7-integration-seam-impl`) and
  calls the Protection API regardless of how many lines arrived. The
  principle behind the rewrite is that core must not invent vendor-specific
  header normalization and must not decide on a vendor's behalf to skip a
  vendor call. How a repeated or otherwise ambiguous field is interpreted,
  and how it is encoded into the vendor's payload, is the vendor's detection
  logic, so it belongs to the vendor integration, which sees every received
  line in received order through `HeadersList` and the presented field
  lines.
- **Where that logic belongs.** At `split/5-response-hook-docs` the DataDome
  integration is core code, in
  `crates/trusted-server-core/src/integrations/datadome.rs` and the
  `crates/trusted-server-core/src/integrations/datadome/` directory. From
  `split/7-integration-seam-impl` on it is the crate
  `crates/bot-protection/datadome`, which holds `protection.rs` and
  `protection_scope.rs`. Vendor detection logic of this kind belongs in the
  vendor's own module rather than in core, and the integration module seam
  (`2026-08-27-integration-provider-seam-design.md`) designs that move. This
  is a direction for that migration rather than a complaint about the code as
  it stands.
- `content-length` is not a vendor decision. It is HTTP framing, parsed by
  the host HTTP layer beneath Trusted Server before any integration runs,
  and no integration reads it as a value. `PostParamLen` is always the byte
  length of the body actually presented to core, not the numeric
  `content-length` value, so the header never reaches the vendor payload.
- `authorization` never reaches the vendor payload as a value.
  `AuthorizationLen` is the byte length of the OWS-normalized value. When
  more than one `authorization` line arrives, every line is presented in
  received order and how they are counted is the integration's decision,
  as it is for any other repeated field. Repetition no longer skips the
  vendor call, for the same reason as above, because that let the client
  decide whether the vendor is consulted at all.
- Multiple `cookie` field lines are permitted. Core OWS-normalizes them and
  joins them in received order with the literal bytes `; ` for the shared RFC
  cookie parser. `CookiesLen` is the byte length of that canonical joined
  value. `ClientID` is populated only when the parsed result contains exactly
  one syntactically valid `datadome` pair. Malformed cookie syntax or duplicate
  `datadome` pairs produces the required empty `ClientID` value without
  exposing another cookie. The original cookie values never enter the vendor
  payload.
- After successful normalization, `HeadersList` records the lowercased name of
  every admitted received field line in original line order, so repeated list
  fields and cookie lines remain repeated. A rejected request produces no
  `HeadersList` and no vendor call. Per-field caps apply to the single
  normalized value. The 24,576-byte cap applies after complete form encoding.

For an optional mapped value, zero received lines omits both source and mapped
field. One or more lines whose OWS-normalized values are all empty omits the
mapped form field but retains each received source name in `HeadersList`. If at
least one list-valued line is nonempty, empty siblings remain represented in
the exact received-order `, ` join. Mandatory `ClientID` and the three length
fields follow their explicit rules instead of this optional-field omission.

Adapter qualification fixtures feed the same ordered repeated-field corpus to
every host and assert byte-identical form fields, lengths, `HeadersList`, and
reject/omit outcomes. The corpus includes repeated list fields, identical and
different duplicates of the pass-through fields, multiple cookies, duplicate
`datadome` cookies, empty values, invalid octets, and headers whose individual
values contain commas. For a duplicated pass-through field the asserted
outcome is that every received line survives in received order and that the
vendor call still happens, never a skip. Invalid UTF-8 is a skip, never
replacement decoding.

`true-client-ip`, `x-forwarded-for`, and `x-real-ip` are not admitted in v1.
The trusted `IP` field already supplies connection provenance, and copying raw
forwarding headers would let a client or unqualified proxy manufacture vendor
evidence. A future adapter-normalized forwarding chain requires a separately
named typed field and vendor sign-off, never reuse of the raw header mapping.

Platform host evidence:

- `TlsProtocol`, capped by TS at 32 bytes
- `JA4`, capped by TS at 128 bytes, only when the operator explicitly sets
  `[bot-protection.datadome] expose_host_fingerprints_to_vendor = true`;
  the default is `false`, omission is represented by absence rather than an
  empty field, and startup logs the additional vendor disclosure
- `TlsCipher` is omitted in v1: DataDome defines it as the ordered list of
  cipher suites offered by the client, while `RuntimeServices::client_info()`
  exposes only the negotiated cipher. Substituting that value would silently
  change the field's meaning
- `H2Fingerprint` is omitted in v1 because the current Protection API contract
  does not define such a request field

`X-DataDome-ClientID` is never a Protection API source in cookie-mode v1.
No wildcard (`Sec-CH-*`, `Sec-Fetch-*`, `X-*`, or otherwise) expands this
list.

The following limits are bytes of the decoded field value before form
encoding. Truncation is UTF-8-boundary-safe. `XForwardedForIP` alone truncates
from the end, while every other bounded field retains its prefix:

- 8 bytes: `SecCHDeviceMemory`, `SecCHUAMobile`,
  `SecFetchStorageAccess`, `SecFetchUser`
- 16 bytes: `SecCHUAArch`
- 32 bytes: `SecCHUAPlatform`, `SecFetchDest`, `SecFetchMode`, and the TS cap
  on `TlsProtocol`
- 64 bytes: `ContentType`, `SecFetchSite`, and the TS cap on `ServerRegion`
- 128 bytes: `AcceptCharset`, `AcceptEncoding`, `CacheControl`, `Connection`,
  `From`, `Pragma`, `SecCHUA`, `SecCHUAModel`, `X-Requested-With`, and the TS
  cap on opt-in `JA4`
- 256 bytes: `AcceptLanguage`, `SecCHUAFullVersionList`, `Via`
- 512 bytes: `Accept`, `ClientID`, `HeadersList`, `Host`, `Origin`,
  origin-only `Referer`, `ServerHostname`, and `ServerName`
- 768 bytes: `UserAgent`
- 2,048 bytes: path-only `Request`

`Key`, `AuthorizationLen`, `CookiesLen`, `IP`, `Method`, `ModuleVersion`,
`Port`, `PostParamLen`, `Protocol`, `RequestModuleName`, and `TimeRequest` are
unbounded per-field by the vendor table but remain subject to the total bound.
The complete `application/x-www-form-urlencoded` body, including field names,
`=`/`&` separators, and percent-encoding expansion, must be at most **24,576
bytes**. Core constructs and measures the whole payload before issuing the
request. It does not silently drop optional fields to fit. Overflow skips the
vendor call and takes the same metered fail-open `Continue` path as a transport
failure.

This is deliberately narrower than DataDome's currently documented required
surface. Notably, it withholds `CookiesList` and omits empty source-header
fields. Product/vendor sign-off 28 therefore requires written confirmation
that this exact reduced profile is supported. Until that confirmation and
adapter conformance fixtures exist, the DataDome integration is not
release-qualified.

#### 4a.2.2 Request-direction pointer (vendor response → publisher-upstream overlay)

The complete set of vendor-response header pointers the security
channel (hook spec §4a) may copy into the owner-scoped
publisher-upstream overlay. Every `X-DataDome-*` name not listed here
is rejected.

| Header                | Direction                   | Scope                                                                                                                                                                  |
| --------------------- | --------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `X-DataDome-ClientID` | response → upstream overlay | Disabled by default. Admitted only with `expose_client_id_to_origin = true`. Owner-scoped publisher overlay only, never the shared request view or another integration |

#### 4a.2.3 The single pointer matrix (normative, decision × session mode × pointer)

This is the one authoritative browser-response contract. Session mode
is **cookie** in v1 (sessionByHeader is startup-rejected, and a header-mode
column is added by the sign-off-23 opt-in, never implicitly). No
wildcard rows exist, every accepted name is enumerated, and **every
cell terminates in exactly one outcome**.

| Pointer             | Respond (cookie mode)                                                                                                                                              | Continue (cookie mode)                        |
| ------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------- |
| `Set-Cookie`        | typed `datadome` cookie operation (hook spec §4a parser, exactly one `datadome` field)                                                                             | typed `datadome` cookie operation             |
| `X-Set-Cookie`      | **invalidate the batch → Continue** (mode mismatch: TS never requests header sessions in v1; a vendor response asserting one must not half-apply)                  | **invalidate the batch** → effects dropped    |
| `Location`          | forward on a 3xx Respond, replace; **on a non-3xx Respond → invalidate the batch → Continue** (a redirect field without a redirect status is a malformed decision) | **invalidate the batch** (effects dropped)    |
| `Content-Type`      | forward (owns its body), replace                                                                                                                                   | **invalidate the batch** (effects dropped)    |
| `Cache-Control`     | restricted merge, invariant pass last                                                                                                                              | **invalidate the batch** (effects dropped)    |
| `Pragma`            | drop-individually, logged (response `Pragma` has no standardized meaning, RFC 9111 §5.4)                                                                           | drop-individually, logged                     |
| `X-DataDome`        | forward, owner-scoped typed telemetry                                                                                                                              | forward, owner-scoped typed telemetry         |
| `X-DD-B`            | forward as a browser-response security signal, never copy to publisher-upstream or another integration                                                             | forward as a browser-response security signal |
| anything not listed | invalidate the batch → Continue                                                                                                                                    | invalidate → effects dropped                  |

Singleton-field multiplicity (`Location`, `Content-Type`, `X-DataDome`,
`X-DD-B`, `X-Set-Cookie` appearing more than once) invalidates the
batch atomically. List-valued fields (`Cache-Control`, `Pragma`) join
per RFC 9110 §5.3 before their cell applies (hook spec §4a).

`X-DD-B` is security-owned when DataDome is enabled. Before applying the fresh
security batch, core removes every pre-existing instance from the origin,
cached ordinary artifact, 304 metadata update, core response, or ordinary
mutator. A valid pointed vendor value then uses **replace-all** and the final
response cardinality must be exactly one. If the fresh vendor batch does not
point to it, final cardinality is zero. Append is never allowed. Fixtures cover
origin collision, cache-hit collision, 304 collision, repeated vendor fields,
and one valid fresh value, proving “exactly once” at final emission rather than
merely inside the vendor batch.

**Fixtures**: DataDome's documented challenge response (`Set-Cookie`,
`Pragma`, `X-DataDome`, `Cache-Control`) asserts the decision stays
**Respond** with exactly the mapped fields. The documented allow example
(`Set-Cookie X-DD-B`) asserts Continue proceeds with the cookie applied
and `X-DD-B` forwarded exactly once, neither fixture may fail open.

### 4a.3 DataDome configuration delta (normative)

The canonical v1 effective configuration is the pre-existing DataDome schema
plus this exact PR-specific delta. The configuration parser rejects unknown
security/session fields rather than ignoring them as compatibility toggles,
and the materialized values below participate in permission §5.5's complete
effective-config digest.

At `split/5-response-hook-docs` the pre-existing schema is the table
`[integrations.datadome]` (`DataDomeConfig` in `integrations/datadome.rs`, and
the "DataDome Integration" section of `docs/guide/configuration.md`), and the
code has no `[bot-protection]` section. The delta below is written under
`[bot-protection.datadome]`, the section name from
`split/7-integration-seam-impl` on, where DataDome is the crate
`crates/bot-protection/datadome`. None of the delta's settings exists in the
tree, and the effective-config digest of permission §5.5 is not built either
(§1a).

```toml
[bot-protection.datadome]

# Existing timeout_ms remains the 1500 ms first-byte bound.
complete_response_timeout_ms = 3000
challenge_body_max_bytes = 65536

# Required when enable_protection = true; there is no silent lifetime default.
security_cookie_max_age = 2592000 # example; allowed 604800..=31536000
security_cookie_domain = "host-only" # or one exact normalized ASCII domain
security_cookie_same_site = "Lax" # Lax | Strict | None

expose_client_id_to_origin = false
expose_host_fingerprints_to_vendor = false
```

The Protection API authority is not configurable. It is the core-owned
`https://api-fastly.datadome.co/validate-request` endpoint defined in §4a.2.1,
with redirects disabled. The baseline `protection_api_origin` field is a
startup error under this delta, as are `sessionByHeader`, `session_by_header`,
any equivalent session-mode spelling, and every other unknown
security/session field. Supporting another region or a publisher proxy is a
reviewed endpoint-registry and product/security decision, never a free-form
URL setting.

`complete_response_timeout_ms` defaults to and may not exceed 3000;
`challenge_body_max_bytes` defaults to and may not exceed 65,536.
`security_cookie_max_age` is mandatory when protection is enabled and must be
in `604800..=31536000` seconds. `security_cookie_domain` defaults to
`host-only`; an explicit value must pass §4a's exact-domain, domain-match, PSL,
and active-scope-change rules. `security_cookie_same_site` accepts exactly
`Lax`, `Strict`, or `None`; `None` is valid only with the unconditionally
emitted `Secure` attribute, and the cookie never carries `HttpOnly`.

Both exposure booleans default to `false`. ClientID-to-origin additionally
requires the owner-scoped overlay capability. Host signals additionally
require qualified JA4 availability and sign-offs 23/28. A selected adapter
that cannot preserve admitted request-header field-line order or enforce the
request/body limits fails startup for protection rather than synthesizing
different evidence.

### 4a.4 Shipped DataDome behavior that §4a changes

This spec has no rollout for a deployment that runs DataDome protection today,
and §8 of the rollout spec has no row for these changes. Where §4a requires
otherwise, the code at `split/5-response-hook-docs` behaves as follows. From
`split/7-integration-seam-impl` on, the same behavior is in
`crates/bot-protection/datadome`.

- **Session identity.** `build_protection_payload` takes `ClientID` from the
  `X-DataDome-ClientID` request header first and from the `datadome` cookie
  second, and sends `X-DataDome-X-Set-Cookie: true` when the header supplied
  the value. §4a takes `ClientID` from the cookie only, strips the header, and
  refuses a session by header at startup.
- **Fields sent.** The code sends `TrueClientIP`, `XForwardedForIP`,
  `X-Real-IP`, `TlsCipher`, `H2Fingerprint` and `JA4` whenever the request or
  the platform supplies a value. §4a.2.1 sends none of the first five, and
  sends `JA4` only when `expose_host_fingerprints_to_vendor` is `true`.
- **Request fields.** `Request` carries the path and the query, `Port` is the
  literal `0`, `RequestModuleName` is `Trusted-Server-Rust`, `Origin` and
  `Referer` carry the whole value, `PostParamLen` is the value of the
  `content-length` header, and `HeadersList` names each distinct header once.
  §4a.2.1 sends the path only, the TCP source port (and skips the call when it
  is missing), the name `trusted-server`, the scheme, host and non-default port
  of `Origin` and `Referer`, the byte length of the body core holds, and every
  admitted received line in order.
- **Response headers.** `extract_header_mutations` appends a vendor
  `Set-Cookie` as the vendor sent it and sets every other pointed header, and
  `is_forbidden_filter_header` refuses a few names when the effects are
  applied. §4a turns `Set-Cookie` into the typed `datadome` cookie operation,
  accepts only the names in the matrix of §4a.2.3, and treats any other name as
  an invalid batch.
- **Settings.** `protection_api_origin` is a setting with the default
  `https://api-fastly.datadome.co`, and the code has no setting for a cookie
  lifetime. §4a.3 makes `protection_api_origin` a startup error and makes
  `security_cookie_max_age` required once `enable_protection` is `true`.

## 5. Size and sequencing

The ordinary mutator API in §2 is a headers-only feature with no module
coupling. Its cache rules in §3 and §3a are coupled to the permission model,
because the revision tuple holds the policy activation fields, and that
activation is not built (§1a). The API lands only with the real consumer
required by §4 item 3, and without one scaffolding does not ship. The
separately typed security channel in §4a is already that consumer's
PR-specific contract, as it owns DataDome cookie operations and intentionally
couples configuration activation to permission §5.5. Those capabilities never
become part of the ordinary mutator API.

## 6. Divergences from issue #782

This spec supersedes #782 on the following points. The issue is updated to
reference this spec when the implementing PR (the hook's return with its
first consumer, §7) merges:

| #782 says                                          | This spec says                                                                                                       | Why                                                                                                                      |
| -------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| Mutations apply to the outbound response generally | Eligibility is the explicit §3a matrix, centered on processed documents                                              | Pass-through/error/304 mutation has different semantics per adapter, and enumerating beats implying                      |
| Ship the trait + registry + adapter application    | Additionally: structured operations API (§2), reserved surface (§3), and a real consumer in the same PR (§4, item 3) | PR #838 shipped the trait with zero call sites, and an unrestricted `&mut HeaderMap` cannot enforce any collision policy |

## 7. Disposition (2026-08-25)

- The earlier draft of PR #1047 carried an `IntegrationResponseMutator`
  response-header hook. The hook was removed from that PR before the series
  was presented, so PR #1047 carried the documentation set only, and no hook
  trait, registration builder, or adapter call site exists in the series'
  tree (§1a). PR #1047 was closed without merging on 2026-10-15, and one of
  the stack's pull requests, PR #1347, carries the same set at the same
  commit (see the Status). This is exactly the outcome §4 item 3 and §5
  require for a feature with no consumer, applied to the feature's own spec.
- The hook returns together with its first consumer, meaning an integration
  that must set response headers, for example `Accept-CH` client-hint
  requests or detection results such as §4a's security channel. The
  returning PR implements this spec, not PR #838's shape.
- Corrected on 2026-09-01, from review of PR #1084. §4a.2 previously
  required core to treat two or more lines of an admitted singleton
  request header as ambiguous and to skip the vendor call. Header
  multiplicity is attacker controlled, so that rule would have given any
  client a way to turn bot detection off by repeating a header, and it
  described no shipped behavior, because `header_value` in
  `crates/trusted-server-core/src/integrations/datadome/protection.rs`
  reads each mapped field with a single `headers().get()` and the
  Protection API is called regardless. Core now passes repeated lines
  through as received and leaves the interpretation to the vendor
  integration.
- Recorded for that future design, from review of the earlier draft. The
  `&mut HeaderMap` shape concern stands. Handing a mutator a mutable
  header map makes §3's collision policy unenforceable by construction,
  because core cannot validate or attribute writes it never sees, so the
  structured, attributed operations API of §2 is the required shape and the
  `&mut HeaderMap` API must not return with the hook.

## 8. Open questions

Each of these waits for a decision and, for most, a code change. This revision
changes no code and decides none of them.

1. Which cache the hook extends. §2 and §3a describe a processed-document
   cache with persisted finals, a variant index and a keyed digest. The code
   ships the template cache (§1a), which keeps the page after the HTML rewrite
   and builds its headers again on every hit. Is the processed-document cache
   the template cache, or a second cache beside it? If it is the template
   cache, the variant index, the keyed digest and the revision tuple have to
   be restated against `TemplateCacheKey::to_cache_key`, `template_fingerprint`
   and `VarySpec`. If it is a second cache, how does it fit the comment in
   `platform/template_cache.rs` that an assembled response must never exist?
2. What a stored entry is checked against until the permission model's policy
   activation exists. The tuple in §3 names four fields from that activation
   and none has code. The template cache checks `template_fingerprint`, which
   hashes the build digest, the browser bundles and the whole typed settings.
   At `61b1399c6` the build digest does not hash
   `config/permissions/sample.yaml`, because core's build script hashes only
   its `src` folder and the files it lists, so a change to the compiled sample
   leaves the fingerprint as it was. Row 4 of the stack (#1345) builds section
   3.1 of the permission model design, and at row 4 the policy's `sha256` sits
   in the typed settings, so a policy change changes the fingerprint, which
   leaves the policy's `text` out because `sha256` already names the file
   (`template_fingerprint`, `publisher.rs` lines 2116 to 2145 at row 4).
3. Whether `Edge-Control` joins `EDGE_CACHE_HEADER_NAMES`. The code strips four
   names and has no `edge-control`. If it joins, `remove_edge_cache_headers`,
   `is_edge_cache_header_name` and the tests of both change with it.
4. Whether the integration id rule admits `_`, or `google_tag_manager`,
   `js_asset_proxy` and `gpt_diagnostics` are renamed. A rename changes the
   table name `[integrations.google_tag_manager]` that deployments already use.
5. Which function is the apply point on each adapter, and how Done-when item 4
   (every adapter applies the mutations and has a route test) fits the
   capability cells in §3 that fail startup on Fastly, Cloudflare and Spin.
   Axum, Cloudflare and Spin run no request filters, so the security channel
   of §4a has no place to run on them today.
6. Whether the three response-header steps share one reserved list and one
   failure rule. The Edge Cookie module list refuses `cache-control` and fails
   the module's generation. The request filter list skips one header and lets
   `Cache-Control` and `Set-Cookie` through. §3 merges `Cache-Control` and
   rejects a whole batch.
7. How `TerminalPrivateResponse` relates to the snapshot in §2. The marker
   restores `no-store, private` on Fastly only, for a policy of Trusted
   Server's own, whereas the snapshot covers a restriction that came from core
   or from the origin. Does the snapshot read the marker or replace it?
8. Whether the hook runs on an origin response with a 3xx, 4xx or 5xx status,
   and on a processed body that is not HTML. `classify_response_route`
   processes a `text/*`, JavaScript or JSON body of a supported encoding
   whatever its status except `204` and `205`. §3a says no for a redirect and
   for an error that Trusted Server generates, and says nothing about these.
9. What turns on the response caching that makes the keyring mandatory, and
   where its secret is named. §2 puts `vary_digest_key_secret_name` in
   `[cache]`, which holds only the asset rules today.
10. Where `PartnerEgress` and the redacted request view are defined. The
    providers spec names the view without defining it, and no spec defines
    `PartnerEgress`.
11. How a deployment that runs DataDome protection today moves to the behavior
    in §4a.4. This spec has no rollout, rows 23 and 28 of the rollout spec's
    §8 do not cover one, and a deployment with `enable_protection = true`
    fails startup until it sets `security_cookie_max_age`.
12. Whether §4a and the cache, `304` and `HEAD` rules move into specs of their
    own, so that this spec covers the headers-only API alone.

## Revision record

| Date       | Change                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-08-25 | Disposition added (§7).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| 2026-09-01 | Repeated lines of an admitted request header pass through as received in §4a.2 (§7).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| 2026-10-06 | Module wording.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| 2026-10-15 | Fixes from review round 1. The status and the closed pull request, a new section 1a on what the code does today, the cookie rule, the shared finalization code and the DataDome section name stated as the code has them, the reserved CDN cache fields taken from `EDGE_CACHE_HEADER_NAMES`, the three ids the id rule refuses, the template cache and the cache revision tuple marked as unreconciled or not built, the changes to shipped DataDome behavior in section 4a.4, the order of sections 4 and 4a, the sign-off rows, `must-revalidate` precedence, the duplicate-ID test in the done-when, the existing tests, the open questions and the review log. |
| 2026-10-16 | Fixes from review round 2. What the providers design now says (section 1a), the two core functions that every adapter's `apply_finalize_headers` calls (sections 1a and 2), the closed pull request that carried the documentation set (#1047, section 7), the revision dates and the review log.                                                                                                                                                                                                                                                                                                                                                                   |
| 2026-10-27 | Section 1a keeps that each build embeds the repository's sample at its commit, and says that row 4 of the stack (#1345) builds section 3.1 of the permission model design, so that once row 4 is merged forward a domain's blob, which names its policy file by name and SHA-256, is the policy identity and gives `active_policy_digest` its value. Open question 2 says that row 4's policy `sha256` reaches the template fingerprint, which leaves the policy's text out and which the compiled sample never reached. The stack's pull requests are named without calling them drafts.                                                                           |

## Review log

1. **Round 1, 2026-10-15.** A review in a fresh session, against
   `split/5-response-hook-docs` at `61b1399c6`. Issues found, four of them
   blocking. The four were a cache revision that needs a policy activation
   nothing builds, a list of CDN cache fields that differs from the list in
   the code, an id rule that refuses three ids that ship, and a
   processed-document cache that never names the template cache already in
   the code. This
   revision states what the code does at that commit in a new section 1a and
   in place, marks the parts that are not built or are built by a later row of
   the stack, and puts the questions the code raises under Open questions, so
   the hook, the cache fields, the id rule and the CDN field list wait for
   those decisions and for code changes.
2. **Round 2, 2026-10-16.** A lighter review in a fresh session, checking the
   round 1 fixes against `split/5-response-hook-docs` at `61b1399c6`. Issues
   found, none blocking. This revision corrects three statements, being the
   wording of the providers design quoted in section 1a, the claim in section 2
   that one function is the only response-finalization code the adapters
   share, and the claim in section 7 that a pull request now closed (#1047)
   ships the documentation set.
3. **Round 3.** A review in a fresh session, against
   `split/5-response-hook-docs`. Issues found, one of them blocking. Section
   1a, which states what the code does today, said that the pushed blob is
   the policy identity, when today each build compiles the repository's
   sample in, and the pushed blob becomes the policy identity only once
   row 4 builds section 3.1 of the permission model design. This revision
   keeps today's code in section 1a and adds, as what row 4 builds, that a
   domain's blob then names its policy file by name and SHA-256 and gives
   `active_policy_digest` its value. Open question 2 says that the policy's
   `sha256` then sits in the settings the template fingerprint hashes, so a
   policy change changes the fingerprint, where a change to the compiled
   sample does not.
4. **Round 4.** A review in a fresh session, against
   `split/5-response-hook-docs`, of the round 3 fixes and the new text.
   Approved, none blocking. This revision names the file each build
   compiles in, `config/permissions/sample.yaml`, where section 1a said
   `permissions.yaml`, and says that the template fingerprint leaves the
   policy's `text` out, because `sha256` already names the file.
5. **Round 5.** A review in a fresh session, against
   `split/5-response-hook-docs`, with row 4 read on `split/3-permissions`.
   No issues found. The round found section 1a right at `61b1399c6`, the
   commit it states, where `DEFAULT_PERMISSION_RULES` still compiles the
   repository's sample in, and found that row 4's template fingerprint
   hashes the policy's `sha256` and leaves its text out, as open question 2
   says.
