# Axum identity through the shared application

Issue: #1574. This implements A2 from the server-side identity design, on the
record backend in #1641 with production startup #1635, conformance #1613 and
environment guard #1637 merged into the branch. The PR targets #1641.

## Required behavior

A consented HTML navigation or explicit EC route creates an EC record before
issuing `ts-ec`. A failed store never issues an unbacked cookie. Returning ECs
survive unknown geography. Identify, batch sync and admin lookup use the one
shared graph; identify and batch sync answer locally with 501 without it.
Auction body EIDs are saved for an existing live record under EID consent,
including deployments with bidding disabled.
Identity cookies are emitted only on eligible HTML navigation or EC route responses, including cached-document `304` responses. Explicit withdrawal is the privacy exception: its EC deletion must accompany tombstoning on the same response regardless of method, status or content type.
Adapters supply platform services and call the shared app, without implementing
`EcKvStore` or invoking lifecycle/identity handlers directly.

## Existing code and gaps

`ec/app.rs` owns identity state and admin routes, but Axum and Fastly still call
EC generation, finalization and partner handlers themselves. Axum's separate
graph does not populate the shared app, and its batch route always returns 501.
The portable T5 runner supports navigation on Fastly only. No minting EC route
is mounted. Auction parsing captures body EIDs but a regression deliberately
asserts they never reach storage. Finalization can write identity cookies on arbitrary responses. Ordinary identity cookie output must be restricted to eligible document/EC responses while preserving explicit-withdrawal EC expiry everywhere.

## Design

Expand core `IdentityApp` with request lifecycle and partner route operations.
A request-local state holds consent, EC context and browser cookie inputs;
platform-derived geo/client signals remain inputs through `RuntimeServices`.
Core classifies navigation and EC response eligibility: document navigation is method-neutral and keeps lifecycle writes on HTML responses of any nonredirect status. Redirect statuses 301, 302, 303, 307 and 308 remain excluded. A navigation returning `304 Not Modified` retains its permit even without Content-Type because it validates the cached document. Otherwise final HTML Content-Type is required; a request Accept header alone cannot authorize identity issuance on JSON/asset responses.
Core generates identity,
executes partner handlers and finalizes persistence/cookies. Fastly retains its
existing delivery bridge, limiter, geo and provider transport; lifecycle and
handler decisions live in core. This includes before/after-response provider
and pull-sync orchestration: eligibility, live-row consent checks, dispatch,
collection and conditional follow-up commit. The delivery bridge only invokes
core after successful delivery and supplies a fresh outbound client. Axum uses
its prepared provider handle to build
the shared app once. Cloudflare and Spin retain their unsupported-store startup
checks and call the core facade for identity operations they expose.

Mount `/_ts/api/v1/ec` in core: same-origin POST accepts a bounded consent
snapshot (optional `tcString`, `gppString`, `applicableSections`; 16 KiB body,
4 KiB effective TCF decoder limit, 8 KiB GPP limit, at most 32 integer section
IDs in 1..=65535). Unknown fields, including `tcfString`, are rejected. Posted
values replace only their matching request cookies; omitted fields retain
those cookies. Present fields must have the declared string or array type;
null is rejected. Complete posted strings must match the client's cookie-safe
alphabets (`[A-Za-z0-9._-]+` for TCF, `[A-Za-z0-9._~=-]+` for GPP), so a
TCF supplementary segment cannot inject another cookie. Posted empty sections clear `__gpp_sid`; absent
sections retain it, defaulting to empty without a cookie. The client must match
the 32-entry cap. The `window.__tsjs_prebid.ecRoute` injection remains owed by
#1622. The shared browser/server JSON fixture lives at
`crates/trusted-server-js/lib/test/fixtures/ec-route-contract.json` and is POSTed
through both adapters in conformance tests. An empty browser snapshot makes no
call; the fixture also tests a server-only `{}` body. The route returns private no-store JSON containing only
the allowed mirror view. GET returns private 204, never mints or runs providers,
and can restore an existing SharedID from a readable record only when current
EC and EID permission both allow. Denied GETs emit no identity cookies and do
not change or tombstone the record; reconciliation remains a POST operation. Other methods are
answered locally. Malformed snapshots return 400, oversized bodies 413,
and missing/cross-site Origin 403 before record or provider work. Browser CMP capture/ordering remains T19/T20; A2 tests the HTTP
contract, not new injected client behavior. POST uses the same consent and
record-first creation as navigation. A failed authoritative withdrawal returns
503 while still clearing the browser identity; it never acknowledges a live
row as successfully withdrawn. Provider construction uses the existing provider crate on Axum as on Fastly;
provider/pull-sync decisions use core. Validate the runner's available completion
hooks before selecting after-response scheduling; any missing drained-task API
is an EdgeZero obligation, never a detached adapter task. Broader provider
feature qualification remains #1581.
No new cookie is issued for missing/denied consent,
missing trusted client IP or storage failure. Read-only partner APIs do not mint.
No User-Agent-only requirement is added for native creation; use the shared
request policy and available trusted signals.

Introduce an explicit existing-record body-EID commit path, separate from the
navigation write permit. It preserves Purpose 1/4 and partner/vendor consent,
source ownership, opt-outs, conditional writes and withdrawal tombstones. It
never creates or recovers an EC from an auction and emits no identity cookie.
Keep authoritative withdrawal handling on routes participating in the identity lifecycle. The same response expires the browser EC cookie independently of method, status and content type, including when the tombstone write fails. This EC deletion is the explicit privacy exception to the HTML document/EC route identity-cookie boundary; other identity cookie output remains suppressed on ineligible responses. Configured asset-only proxy routes retain
Fastly's existing streaming path without identity ingestion or finalization;
explicit reconciliation uses the EC/consent route. This preserves the asset
contract while WB-1 keeps every such response free of identity cookies.

## Store declaration and configuration

`edgezero.toml` already declares `trusted_server_kv`. Axum declares its record
layout in `Hooks::configure` and its logical ID in `Hooks::stores` unconditionally.
EdgeZero prepares the provider using local default or native bindings. Application
`ec.storage = { provider = "records", store = "trusted_server_kv" }` activates the
graph; omitting it is the no-store application control. Provider settings belong
to EdgeZero, including AWS endpoint/credential selection for DynamoDB Local.
Delete obsolete `TS_EC_*` reads and document migration to the binding file.
An ordinary-only or `KvError::Unsupported` binding rejects startup before listen.
AWS SDK dependencies remain test-only table administration helpers.

Without a remote binding, the unconditional declaration prepares a local record
provider even when identity is disabled; tests and operators need a writable
EdgeZero data directory. Remote-only bindings do not require a local directory.
Store preparation precedes application config loading, so deciding declarations
from `ec.storage` would require an EdgeZero config-first preparation API. Keep
the declaration unconditional. Config and secrets continue to use A1's prepared
immutable startup snapshots.
The temporary `TS_NATIVE_GEO_PROFILE_FILE` exception is narrowly justified by
#1529 / #1532; geo selection remains a separate AX-2 obligation.

## Alternatives and boundaries

Renaming the old switches would preserve adapter-local selection and violate
the declared-store requirement. Porting the old Axum stores would duplicate
EdgeZero providers. The selected approach uses the existing record API and
shared app, preserving Fastly's lazy record opening. No EdgeZero implementation
or infrastructure provisioning belongs in this PR.

A2 supplies identity behavior and evidence. The merged stack already has the
DynamoDB image/SIGTERM CI job; #1579 still needs current-revision qualification
and reconciliation of its shutdown acceptance. #1581 owns broader Axum identity
feature qualification. Local runtime evidence does not substitute for those gates.

## Verification

Extend T5 so identical navigation scenarios execute on Axum and Fastly, then add
partner locality with capturing origin, EC route cookies, body persistence,
non-document cookie suppression and unsupported-store startup cases. Exercise
positive persistence and rejected-write controls before response release.
Include a positive native creation control with trusted IP, consent and no JA4;
never fabricate TLS metadata to pass a Fastly-specific pre-T12 baseline. State
explicitly which shared signal policy each adapter scenario exercises.
Use existing Axum test geo injection for conformance; production probes use the
existing protected CloudFront profile with loopback admission and known country.
Run targeted adapter/core tests and clippy, formatting, and #1637's unweakened
guard. Do not run the full CI list locally.

Build the actual Axum production binary with `aws-dynamodb-ec` and
`geo-cloudfront`. Run with disposable prepared config/secrets and DynamoDB Local
in Docker. Through Chrome and the publisher proxy, observe the issued cookie and
persisted row; repeat with a failed provider and no-store control. Exercise
identify/batch locally and SIGTERM during an in-flight record write, observing
both response completion and the durable record. Record exact source/artifact
identity and scenario limits. Preserve operator files and stop all owned
processes/containers; remove disposable worktrees after publication.

Tests also require POST withdrawal/tombstone, GET restoring existing SharedID
without providers, missing Origin, and JSON excluding non-mirrored, expired and
vendor-denied IDs.

## Review log

- Round 1: independent source review identified unspecified EC route methods,
  consent payload and response view, and missing final HTML response gating.
  Defined POST/GET behavior, bounded consent snapshots, mirror-only JSON,
  T19/T20 client boundary and the final HTML condition. Also explicitly moved
  provider/pull-sync eligibility and follow-up orchestration into core, leaving
  only delivery scheduling and platform transport in the bridge.

- Round 2: fresh independent review found no architectural blocker. Clarified
  successful HTML status, concrete payload limits, negative consent/origin and
  mirror-view controls, and provider construction/scheduling ownership. All
  blocking and optional document findings are incorporated.

Implementation review also required a shared GET-only navigation policy and
API withdrawal persistence without cookie output. Dedicated regressions cover
both. The configured streaming asset path keeps its pre-existing no-identity
lifecycle contract; this is an explicit preservation boundary.

- Implementation self-review: added regressions and corrections for body EIDs
  when bidding is disabled and failed EC-route withdrawal acknowledgment.
  Updated native quickstarts to publish the explicit records provider and
  supply the local record data directory; reconciled current support guidance.

- Follow-up consumer review found that environment overlays cannot add the
  starter's missing storage provider key. Native quickstarts now require the
  explicit TOML entry. CLI publication accepts Axum's shared provider
  coordinators while preserving Cloudflare/Spin rejection, with a real
  `ts config push` regression for an enabled provider.

### Approved Fastly compatibility correction

The subsequent comparison with #1641 found that the GET-only and successful-2xx-only restrictions recorded above were implementation choices without an independent requirement basis. The user approved superseding both choices: preserve method-neutral publisher navigation, permit HTML error documents, and retain `304 Not Modified` navigation permission even without Content-Type. The five redirect statuses remain excluded. Explicit withdrawal must expire the EC cookie on the same response as tombstoning regardless of method, status or content type; this privacy exception supersedes the earlier API-cookie suppression choice. Keep the shared split-Cookie-field `ts-eids` capture as an intentional compatibility improvement, with regression coverage.
