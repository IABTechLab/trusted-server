# Provider opt-out enforcement for client EIDs

## Scope and requirements

Fix #1555 on top of #1566 (`feat/axum-identity-providers`), which builds on
#1488. The MVP covers Fastly and Axum. `/auction`, initial-page
auctions and `/_ts/page-bids` must withhold cookie/body EIDs for sources owned
by a provider with an active opt-out in the matching EC row. Legacy
browser-written records in provider-owned sources must stop serving indefinitely.
No providers configured must preserve cookie and body output.

## Design

Record enabled provider IDs alongside each partner's existing ownership metadata.
A source is opted out when any enabled owner has a row opt-out whose end time is
strictly greater than the request time. Reuse `KvEntry::opted_out`; do not extend
memory. Disabled providers lose ownership as today; their vendor consent gates
remain. Source lookup retains existing normalization.

Extend the shared client filter to take the request's snapshot and EC context.
Read only the row matching that context's EC ID. The auction already loads this
snapshot, and publisher/page-bids already carry it. Recognized-browser requests
reuse those reads; the policy-only path below adds a read where it was previously
skipped. No additional writes are introduced.
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

### Read-only policy access without browser classification

Fastly's browser classifier suppresses the normal identity graph when JA4 is
missing. Axum does the same when the User-Agent is not a recognized browser. These requests can still run auctions with client EIDs, so suppressing
the policy read would bypass a remembered opt-out. For nonbrowser requests with
enabled provider ownership, retain a dedicated auction-policy graph in adapter request state and set a
one-way policy-only flag on the EC context. Core auction entry points use it only at their existing
validated, live-auction snapshot read sites. Initial-page policy reads require
an existing, nongenerated, consent-allowed EC and an eligible auction.

Policy-only snapshots stay local to auction targeting: never cache them as the
ordinary EC snapshot. The policy-only flag suppresses stored-ID enrichment. Generation,
KV writes, provider collection, pull sync and completeness-marker finalization
keep their original graph/snapshot inputs. No providers configured keeps the old
read behavior. This intentionally adds one policy read to an accepted auction
whose browser classification would previously skip a readable row; it adds no
read to rejected, disabled, consent-denied or no-slot auctions, nor to existing
bot/prefetch skips on publisher and page-bids paths. `/auction` retains its
existing API admission policy.

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
on this stack. Axum stores EC rows and opt-out memory only for the life of its
process; restarting the development server loses both. Cloudflare and Spin reject enabled providers at startup; their
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
  rows, since production TLS trusts public roots and the process-local store
  has no raw-write interface. Keep these seeded/controlled proofs separate from
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

- Scope correction: restack on #1566 and cover both MVP adapters, including
  Axum unknown-browser policy reads. This branch does not update #1566 itself.

## Native runtime qualification

The normal Axum executable currently supplies no geolocation data, so consent
has an unknown jurisdiction and prevents EC identity use. A TC string alone
does not enable it. Normal TOML validation, local push and publisher startup
can be verified; the full normal-process identity acceptance obligation remains
blocked by that adapter capability. Controlled application services supplying a
known country and fixture vendor/bidder transports can exercise the complete
router and socket flows, but do not establish the default binary's full identity
flow. These are separate evidence scopes.
