# Provider opt-out enforcement for client EIDs

## Scope and requirements

Fix #1555 on top of #1646 (`feat/1574-axum-shared-identity`), which includes
#1566, #1641, #1635, #1613 and #1637. Merge each new #1646 head into this
branch during review; do not rebase or change dependency branches. The MVP covers Fastly and Axum. `/auction`, initial-page
auctions and `/_ts/page-bids` must withhold cookie/body EIDs for sources owned
by a provider with an active opt-out in the matching EC row. Legacy
browser-written records in provider-owned sources must stop serving indefinitely.
No providers configured must preserve cookie and body output.

## Design

Record enabled provider IDs alongside each partner's existing ownership metadata.
A source is opted out when any enabled owner has a row opt-out whose end time is
strictly greater than the request time. Reuse `KvEntry::opted_out`; do not extend
memory. Disabled providers lose ownership as today; their vendor consent gates
remain. Source lookup retains existing normalization. For the shared emitted
SharedID namespace, remembered opt-outs and legacy-read expiry consult enabled
owners of both `pubcid.org` and `sharedid.org`, including normalized client
spellings. Existing per-source consent and write-ownership rules remain unchanged.

Extend the shared client filter to take the request's snapshot and EC context.
Read only the row matching that context's EC ID. The auction already loads this
snapshot, and publisher/page-bids already carry it. The shared identity
application keeps the configured record graph available even without browser
fingerprint metadata. Reuse those reads; no additional writes are introduced.
Keep vendor-consent and jurisdiction filtering, then merge server and client
EIDs and apply the existing overall consent gate. Missing, failed or mismatched snapshots cannot establish an
opt-out and preserve existing degraded behavior. A readable row must not be
deliberately skipped merely because TLS browser metadata is missing. Ordinary client EIDs never clear an opt-out.

`live_eids` also checks owner opt-outs so stale row IDs cannot bypass the client
filter. A legacy single-UID record has no timestamp or owner/expiry fields.
Treat it as expired immediately while its source is provider-owned; continue to
serve live provider-written UIDs using their existing per-UID expiry. This is a
read-time expiration: no schema migration, invented age, extra writes or global
TTL. Batch/pixel/old browser singles cannot be distinguished and receive the
same treatment once an enabled provider owns their source. Unowned and disabled
sources retain their current behavior. Apply these rules to identify's direct
record reader and explain the omissions in the existing admin auction preview.

### Shared lifecycle and request access

Both Fastly and Axum use the shared identity application introduced by #1646.
Its browser eligibility and record-access rules replace the earlier adapter-local
policy-only graph. Missing fingerprint metadata does not hide the record graph.
Apply remembered opt-out policy at bidder targeting using that graph's matching
snapshot. Preserve the shared application's generation, consent, provider,
body-ingestion, response-cookie and finalization rules. Do not restore deleted
adapter-local identity modules, stores or pull-sync code.

## Alternatives and trade-offs

Adding a timestamp/TTL to browser records would require migration and a policy
for records that have no timestamp; it would leave old IDs serving for an
arbitrary grace period. Suppressing all client IDs for owned sources would also
remove valid client IDs when no opt-out exists. The selected read-time gates
remove only known denied client sources and legacy stored fallback records.

## Compatibility, failures and runtimes

No configuration or persisted schema changes. Existing rows remain readable and
rollback can resume serving legacy singles; opt-out correctness therefore needs
this code on every serving instance. Fastly and Axum support enabled providers
on this stack. Axum uses the prepared EdgeZero record service selected by
`ec.storage`; the local provider persists rows and opt-out memory across restarts. Cloudflare and Spin reject enabled providers at startup; their
no-provider behavior must remain unchanged. This does not implement provider
support on those adapters or cookie/localStorage opt-outs absent from the row.

## Acceptance and preservation evidence

- Cookie and request-body EIDs for an active owner opt-out disappear at bidder
  dispatch; unrelated and unregistered sources survive. Include an opted-out
  source absent from `ids`, canonical source variants, and repeated requests.
- At the exact opt-out deadline, the client source becomes eligible again;
  unknown/disabled owner entries do not suppress it. Any active enabled owner
  suppresses a shared source; cover overlapping owners in registry-level tests. Missing/mismatched snapshots
  do not borrow another visitor's opt-out.
- A legacy stored single disappears only for enabled ownership; live provider
  UIDs and unowned singles remain, and provider expiry still applies.
- Identify and admin preview agree with the new stored-record policy.
- No-provider cookie/body output is unchanged; vendor and global consent gates
  retain coverage.
- Run targeted failing regressions before fixes, target-matched tests/lints and
  the repository CI gates. Exercise the actual Fastly app under Viceroy with
  disposable config/KV plus a capturing bidder. Compare clearly active/expired/no-provider
  controls; prove exact deadline equality deterministically in unit tests.
  Capture initial-page auctions as well as API and page-bids. Run the normal
  Axum binary through a disposable `trusted-server.toml` and local config push;
  exercise provider collection, the SharedID `_pubcid_optout` cookie behavior
  and allowed client forwarding.
  Use the real Axum router with a controlled transport for persisted opt-out
  rows, when explicit vendor-response control is required. Keep these seeded/controlled proofs separate from
  normal-binary and live-vendor testing.

## Review log

- Round 1: no blocking findings. Added overlapping-owner coverage, explicit
  initial-page runtime capture, and deterministic exact-deadline testing.

- Round 2: independent source review found no blocking or optional findings.

- Runtime follow-up: stock Viceroy exposed a real integration gap: missing JA4
  prevented policy reads on accepted auctions. Independent raw-issue review
  confirmed it as blocking. Added dedicated read-only policy access with no
  stored-ID enrichment. Independent amendment review required preserving core
  validation timing and completeness-marker behavior; policy snapshots remain
  local to satisfy both requirements.

- Implementation amendment: retain the graph in adapter request state because
  runtime graph handles do not satisfy HTTP-extension Send/Sync requirements.
  The EC context carries only a policy-only flag; snapshots remain local.

- Scope correction: the initial implementation covered Fastly and Axum on
  #1566. The #1646 refresh replaces its adapter-specific lifecycle with the
  shared application and retains remembered opt-out and legacy-record policy.

## Native runtime qualification

Start the production Axum executable through prepared config and secret stores,
using the same envelope and EdgeZero startup options as its acceptance tests.
Use an isolated local record directory. For deterministic jurisdiction, build
with `geo-cloudfront` and use the existing protected CloudFront ingress fixture:
admit the loopback peer, supply its synthetic credential and checked forwarding
metadata, and provide country `AU`. This establishes the normal binary's identity
path with controlled geo and publisher/bidder peers. It does not establish live
vendor, deployed ingress or real-browser behavior.

No runtime environment reads are added. Prepared store and geo selection use
EdgeZero's supported startup configuration. The existing narrowly guarded geo
profile compatibility exception remains owned by #1529/#1532.

The earlier missing-geolocation and process-local-store qualification statements
are historical. Refresh runtime evidence against the merged head before deciding
whether the draft is ready.
