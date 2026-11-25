# Drain after-response tasks on SIGTERM

Issue: #1575 (A3 in #1558, wave 5). Stack: #1635
`feat/1573-axum-production-serving`, above #1531.

## Scope and acceptance

Connect Trusted Server's pull-sync lookups and guarded follow-up writes to
EdgeZero's bounded after-response scheduler on Axum. Preserve Fastly's
per-request delivery gate, consent checks and conditional writes.

Axum on this base has no EC record store, provider coordinators or page-write
finalization. Normal production requests therefore cannot obtain a follow-up
write permit. This PR provides the scheduling and shared execution plumbing;
production activation and the acceptance item "pending record write completes
on SIGTERM" remain A2 #1574 and CI test A8 #1579. It does not add a record store
or issue permits in the absence of a successful page phase. Missing-secret
startup remains covered by #1635.

## Dependency and rollout

All six EdgeZero crates use `rc/identity` revision
`9d02d7b3185b49e81d9ac972ca3feef932ed848a`, matching #1635 and the rest of the
stack. This is R2 (edgezero#490), which already includes #509's bounded
Fastly/Axum after-response implementation at `2620377a`. It supplies the task
scheduler and shared shutdown grace used here; no additional EdgeZero runtime
changes are required.

#1646 (`feat/1574-axum-shared-identity`) also changes Fastly's
`post_delivery.rs`, core's `ec/after_response.rs`, and Axum's `app.rs`/`lib.rs`.
Whichever change lands second must merge the other and verify the combined
lifecycle, rather than independently replacing its work.

## Design

Move the existing captured lookup inputs and execution algorithm from Fastly
into core's `ec::after_response`. Keep Fastly's thin wrapper, telemetry ordering,
fresh post-delivery HTTP client with proxy marker disabled, and committed-body
delivery decision unchanged. No Fastly scheduling migration is needed.

On Axum, retain the request's `AfterResponseHandle` before the publisher
consumes the request. Prepare optional lookup inputs only for a browser
navigation with a graph, coordinator and EC. After successful publisher body
assembly and ownership attachment, register a task only when finalized state
already carries a follow-up permit. Missing work/permit is an ordinary no-op;
missing scheduler for eligible work is logged and the response is preserved.
Move the permit-bearing `EcContext`; its `Clone` implementation drops permits.
The Axum task owns its inputs and client facade; outbound requests start inside
the task's fresh EdgeZero scope, never in the request's scope.

Use existing `ec.identity_after_response_budget_ms` (default 2000 ms) as the
whole Axum task budget, including reads and the guarded write. EdgeZero starts
that budget after response-body release, also on client disconnect. SIGTERM
closes admission and shares the existing configured shutdown grace between
in-flight responses and tasks. It must not grant tasks a second grace period.
A task can be cancelled by its own budget or the remaining shutdown grace;
partial work is never retried unconditionally. No new configuration is added.

The shared runner consumes the non-cloneable permit, verifies the captured EC
still matches, skips nothing-due and freshly minted ECs, reads the live row
before disclosure, and delegates results to the existing generation-guarded
follow-up commit. A withdrawal or conflicting generation cannot be overwritten.
Do not log request cookies, EC values or credentials.

Cloudflare and Spin keep their current behavior: this change neither wires
waitUntil nor changes Spin's before-response execution. Core remains free of
Tokio and platform dependencies.

## Alternatives

Keeping a second lookup runner in Axum duplicates privacy and write guards.
Building Axum storage here would combine A2 with A3 and make the stack harder
to review. Sharing the existing runner and keeping activation gated is the
smallest change compatible with the staged roadmap.

## Verification

Use tests first for shared work and Axum registration. Cover positive eligible
work, absent permit/store, changed EC, browser/navigation gating, multiple Cookie
headers, withdrawn rows and conditional-write conflicts. Existing Fastly
post-send tests remain the preservation checks for committed and fallback
responses. Run targeted native core and Axum tests, Fastly after-response tests,
format and target-matched clippy; do not run the full local CI list.

A subprocess fixture must run the real production host and the same Axum
scheduling function with explicitly supplied eligible work. Hold a request,
register its task, SIGTERM, release the response, observe full response and task
completion, and successful exit within grace. A long task demonstrates grace
exhaustion; request plus task share one deadline. The eligible fixture executes
the shared runner and real pull-sync coordinator
with a scripted provider response and fixture-only live row, asserting the
guarded update. A separate loopback SDK HTTP call from its post-response row
read verifies the fresh outbound scope; this is not real-partner HTTPS proof.
A control bypassing registration must lose completion. Also test client
disconnect release and task-budget exhaustion independently of shutdown grace.
This is fixture activation, not proof of a deployed EC record write. Use controlled loopback upstreams and observable task effects.

Also launch the actual application through the operator task/proxy path for a
publisher smoke and ordinary response drain. Back up and exactly restore any
necessary local launcher/`.env.*.local` edits. Never edit `trusted-server.toml`
or `.env`. Stop only owned processes and remove owned disposable resources.

## Review log

- Round 1 (independent agent): source contracts confirmed; clarified owned
  permit transfer, real lookup/write fixture reach, disconnect release and
  separate task-budget coverage. No blocking findings.

- Round 2 (fresh independent agent): no blocking findings. Added settings
  documentation for the Axum whole-task budget; all optional feedback resolved.

## Reconciliation with #1646 (2026-11-25)

#1646 landed its own shared runner first, so this PR keeps that one and drops
its own: core `ec/app/lookups.rs` `AfterResponseLookups` with
`IdentityApp::run_after_response_lookups`, scheduled by
`IdentityApp::schedule_after_response` from Axum `dispatch_ec_route` and the
shared EC route. The `ec::after_response` runner, `AfterResponseLookups::capture`
and Axum's `after_response::schedule_lookups` described above were removed.
`schedule_after_response` now checks `EcContext::has_after_response_permit()`
before registering and returns whether it registered; a missing scheduler or a
rejected registration is logged, not returned as an error. This PR's drain and
registration tests now exercise that runner from
`crates/trusted-server-adapter-axum/src/after_response_drain_tests.rs`.

The base is now #1646 (A2), which supplies the Axum record store and issues
production follow-up permits, so the "no EC record store" and "activation
remains A2 #1574" statements above describe the earlier #1635 base. This PR's
Cargo dev-dependency change was absorbed: #1646 already depends on
`trusted-server-identity-providers` directly. CI proof of a pending record write
is #1579, implemented in #1654.
