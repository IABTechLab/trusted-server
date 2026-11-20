# Certificate checks on identity backends

Status: draft for review on 2026-11-16.

Issue: [#1556](https://github.com/IABTechLab/trusted-server/issues/1556).
Implementation plan:
[identity backend certificate checks](../plans/2026-11-16-identity-backend-certificate-checks.md).
Origin: PR #1487 review thread `discussion_r4509571994`.
Program placement: wave 2 of #1558, blocked by #1507 (cutover PR #1506). The
program plan records that "#1556 needs only a regression test: #1506 rejects
`proxy.certificate_check = false` and drops the setting from pull sync."

## Problem

`proxy.certificate_check` was meant for local development: setting it to
`false` relaxes TLS certificate verification for the publisher origin. Before
the EdgeZero cutover, Trusted Server registered its own Fastly backends from a
`PlatformBackendSpec`, and four call sites copied the setting into that spec:
asset routes, redirect hops, the publisher origin and EC pull sync
(`ec/pull_sync.rs:237` on main `1810c763a`).

Pull sync is not an origin request. It sends the reader's EC ID with the
partner's bearer token to a third-party partner. When an operator relaxed the
setting for a test origin, pull sync also stopped checking the partner's
certificate. An attacker in the network path could then impersonate the
partner and receive the EC ID and the token. At the reviewed commit, PR #1487
passed the setting into its provider lookup transport too. After review, the
transport was changed to force `certificate_check: true`
(`ec/coordinator/transport.rs:196`), and its constructor no longer takes the
setting. That left pull sync as the only identity backend still following it.

The issue limits "identity backends" to providers and pull sync. On main, the
first-party proxy paths also followed the setting while forwarding `ts-ec` to
third-party targets (`proxy_with_redirects` builds every hop, `proxy.rs:1286`
and `:1347`; `forward_ec_id` defaults to true, `proxy.rs:384`). The same
structural change on #1506 covers them, and the wording change below names
them, but this issue does not add proxy tests.

## What the cutover already changed

Inspected on 2026-11-16: main `1810c763a` and PR #1506 at
`cc811fc7aa0d234e447a868af5b8ab4ea359326a` (EdgeZero pin `10166628`). These
are source observations.

| Source at #1506 / EdgeZero `10166628`                                         | Fact                                                                                                                                                                                                                                                                                  | Consequence                                                                                                                                       |
| ----------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------- |
| `core/src/ec/pull_sync.rs` `pull_request`                                     | Builds an `edgezero_core::outbound::OutboundRequest`; reads no TLS setting                                                                                                                                                                                                            | Pull sync can no longer forward the setting                                                                                                       |
| `edgezero-core/src/outbound.rs`                                               | `OutboundRequest` has no certificate or TLS option                                                                                                                                                                                                                                    | No Trusted Server caller can relax verification per request                                                                                       |
| `edgezero-adapter-fastly/src/outbound.rs:659-663`                             | Every TLS backend calls `enable_ssl().check_certificate(host)`, with SNI for hostnames                                                                                                                                                                                                | Fastly always verifies                                                                                                                            |
| `edgezero-adapter-axum/src/outbound.rs:135-146`                               | `reqwest::Client::builder()` with no `danger_accept_invalid_certs`                                                                                                                                                                                                                    | Axum always verifies (reqwest default)                                                                                                            |
| Cloudflare `fetch`, Spin `wasi:http`                                          | No verification knob; Spin maps `TlsCertificateError` to a failure                                                                                                                                                                                                                    | Platform-enforced verification                                                                                                                    |
| `core/src/config.rs` `reject_disabled_certificate_check`                      | `ts config validate` and `push` reject `false`                                                                                                                                                                                                                                        | Operators cannot publish the setting                                                                                                              |
| `core/src/config.rs` `warn_ignored_certificate_check`                         | A live config that still has `false` keeps loading, logs a warning, and keeps `false` in `Settings`                                                                                                                                                                                   | The relaxed value can still reach `dispatch_pull_sync`'s `settings` at runtime                                                                    |
| `git grep certificate_check -- crates/` outside `settings.rs` and `config.rs` | No runtime reader remains                                                                                                                                                                                                                                                             | Production behavior already meets acceptance criterion 1 on #1506                                                                                 |
| Identity conformance PR #1613                                                 | Explicitly excludes post-delivery pull sync ("pull sync's partner writes keep their own tests")                                                                                                                                                                                       | The regression test belongs in pull sync's own tests, not the conformance suite                                                                   |
| PR #1489 (PR B) at `7bb9e741d`, stacked on #1488 then #1487, based on main    | Moves pull sync into `trusted-server-identity-providers/src/pull_sync.rs` (deletes core `ec/pull_sync.rs`). The provider takes an empty `PullSyncSettings {}` and sends through `PlatformLookupTransport::new(services)`, which forces `certificate_check: true` and takes no setting | Criterion 3 holds in #1489's source today. The core test is deleted by #1489, and the durable guard on that stack belongs at the lookup transport |

So on the cutover base no production code change is needed. What is missing is
a test showing that a relaxed value that does reach pull sync at runtime has no
effect on the request it sends.

## Acceptance criteria mapping

| Issue criterion                                                                               | Delivered by                                                                                                                                                                                                                                                                                                                 |
| --------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1. Pull sync and every provider backend always verify certificates, regardless of the setting | #1506 for pull sync (structural, table above). Providers do not exist on #1506; #1487 forces `true` today, and after it rebases onto the cutover the field disappears with `PlatformBackendSpec`                                                                                                                             |
| 2. A test shows a relaxed `proxy.certificate_check` never reaches identity backends           | This change: a differential pull-sync test (below)                                                                                                                                                                                                                                                                           |
| 3. PR B's pull-sync provider keeps the forced check                                           | Already true in #1489's source (table above), but no test pins it. Handoff obligation on #1487/#1489: when they rebase onto the cutover, keep `PlatformLookupTransport` settings-free and add a test at that layer. This PR cannot change those branches. The comment text is drafted in the plan; posting it needs approval |

## Design

### Regression test

Add `dispatch_pull_sync_ignores_relaxed_certificate_check` to the scripted-client
test module in `crates/trusted-server-core/src/ec/pull_sync.rs`.

- Arrange two runs that differ only in `settings.proxy.certificate_check`: the
  default `true`, and `false`, which is the value runtime loading keeps after
  its warning. Each run gets its own in-memory graph, seeded snapshot and
  `ScriptedHttpClient` with the same partner reply.
- Act: run pull sync in each.
- Assert: each run sends exactly one request. Method, URI, headers and body are
  equal across the runs. Both requests carry the same transport
  (`assert_transport` with the timeout, buffered cap, decoding and cache
  policy that `dispatch_pull_sync_sends_each_partner_its_bearer_token_and_transport`
  uses). Both runs persist the partner's UID, which shows the relaxed run
  actually dispatched and processed its response.

This test is a tripwire, not a proof of TLS. On the base it passes by
construction: `dispatch_pull_sync` reads only `settings.ec.pull_sync_concurrency`,
and `pull_request` takes no settings. It fails only if a future change inside
pull sync makes the setting alter a field the scripted client records (scheme,
host, headers, body or transport). A mutation check during implementation shows
that it does fail. It does not exercise the production `HttpClient` the Fastly
adapter builds (`adapter-fastly/src/post_delivery.rs`), which is where the
pre-cutover defect lived.

### Operator wording

The reject error, the runtime warning, the `Proxy::certificate_check` doc
comment, `docs/guide/configuration.md`, `docs/guide/error-reference.md:195`,
`docs/guide/edgezero.md:100` and `trusted-server.example.toml:213` all say
"origin TLS is always verified". That was the gap the review pointed at: an
operator reading it cannot tell that partner and identity traffic is covered
too. Change the wording to say that TLS is verified on every outbound HTTPS
request, including partner and identity requests. Plain-HTTP local origins
stay supported. `CHANGELOG.md:69` is history and stays as it is. The existing
tests match `proxy.certificate_check`, and the warning test also matches
`ignored`, so the new warning keeps that word.

### Alternatives considered

- **Test at the configuration layer only.** `validate_rejects_certificate_check_false`
  and `runtime_ignores_certificate_check_false_with_warning` already exist.
  They prove that the value is rejected or ignored, but not that the ignored
  value has no effect on identity traffic. They are kept, not replaced.
- **Source-scan test that `certificate_check` has no reader outside
  `config.rs`.** It would catch a new reader anywhere, but it breaks on any
  rename or move. The repository's source-scan checks live in CI scripts, not
  in unit tests. Rejected for this XS issue.
- **Narrow `dispatch_pull_sync`'s `settings: &Settings` to the concurrency
  value.** The compiler would then stop pull sync from reading `proxy.*` at
  all, which is stronger than the test. There is one production caller
  (`adapter-fastly/src/main.rs:679`). It was not chosen as the default because
  #1489 deletes this function and already gives the pull-sync provider an
  empty `PullSyncSettings {}`. Once a function's input is narrowed, the
  differential test has nothing to flip, so the two options are alternatives.
  It is an open choice for the user, with the same review cost.
- **Remove the field or make it private.** That would change the config
  surface #1506 deliberately kept, so live configs keep loading with a warning.
  Out of scope.
- **Runtime TLS probe (Viceroy or Axum against an origin with a bad
  certificate).** This is the only way to check the adapter's real
  verification. The guarantee lives in EdgeZero, which has no unit test for the
  Fastly `check_certificate` call. Recorded as residual risk and a possible
  EdgeZero follow-up. It is not a gate here.

## Preservation obligations

| Scenario                                   | Current behavior on #1506                             | Intended change      | Obligation                                    | Evidence                                                |
| ------------------------------------------ | ----------------------------------------------------- | -------------------- | --------------------------------------------- | ------------------------------------------------------- |
| Default config (`true`)                    | Pull sync sends one request with the strict transport | None                 | Unchanged request and UID persistence         | Existing pull-sync tests plus the new test's `true` run |
| Live config with `false`                   | Loads with a warning; pull sync ignores the value     | None                 | Same request as `true`                        | New differential test                                   |
| `ts config validate` / `push` with `false` | Rejected                                              | Message wording only | Still rejected, error still names the setting | `validate_rejects_certificate_check_false`              |
| Runtime warning                            | Logged once per load                                  | Message wording only | Still logged, still names the setting         | `runtime_ignores_certificate_check_false_with_warning`  |
| Origin, asset and redirect requests        | Always verified through EdgeZero                      | None                 | No change                                     | No code touched                                         |

## Delivery and stacking

- Base: `spec/edgezero-pr275-migration` (PR #1506) at `cc811fc7a`. #1556 is
  blocked by #1507, and on main pull sync still forwards the setting.
- Do not stack on #1613 (identity conformance): it excludes pull sync and is
  independent. Do not stack on #1487, #1488 or #1489: they are based on main
  and have not been rebased onto the cutover.
- If #1506 merges first, retarget the PR to main with no content change. If
  #1506's head moves, merge it into this branch; do not rebase.
- `rc/identity` (#1608) picks this up after it lands, like other wave-2 PRs.
- Closing #1556: criterion 3 depends on #1489. Recommendation: the PR body says
  "Closes #1556" and lists criterion 3 as the #1489 obligation, matching the
  program plan's "regression test, then close". The alternative is "Refs
  #1556", which keeps the issue open until #1489 ports the test. The user
  decides.

## Residual risks

- The test injects `ScriptedHttpClient`, so a relaxation in how the adapter
  builds its production `HttpClient` would not show up in it.
- The differential test sees only what `RecordedRequest` records. If EdgeZero
  later adds a per-request TLS option to `OutboundRequest`, the scripted client
  must record it before this test can catch a regression through it.
- Real verification by each adapter is EdgeZero's contract and is not exercised
  here. A Fastly unit test of `check_certificate` in EdgeZero would close that.
- Criterion 3 has no test until #1487/#1489 add one at the lookup transport.
