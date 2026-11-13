# EdgeZero PR 275 Hard Cutover

**Date:** 2026-10-17, rewritten 2026-11-05, updated 2026-11-06, 2026-11-07,
2026-11-08, 2026-11-11, 2026-11-12 and 2026-11-13

**Status:** Approved at `85f5ca3d9`, with the corrections the implementation
found folded in since. The [Review log](#review-log) has eleven agent review
rounds: six in fresh Claude Code sessions (Rounds 1 to 6) and five GitHub
reviews on PR #1506 (Rounds 7 to 11). Before Round 6 the spec was updated against PR
275 head `9b31a697` and its review. The maintainer answered Q1 to Q7 on
2026-11-08 ([Maintainer answers](#maintainer-answers)), and Round 11 approved
with nothing blocking. The log then records the implementation reviews and
their fixes.

**EdgeZero target:** `IABTechLab/edgezero` PR 275 (branch
`docs/outbound-http-spec`). This spec was checked against head
`9b31a69760255c279f163a3e56bc95a7eed228e3`, called **H** below. Earlier drafts
used `8f82547b`. The commits since then change Cloudflare response egress,
Cloudflare tests, documentation, and CI. They change no public API and no line
an earlier draft cited. The Fastly guide's same-name paragraph, which Fastly
differences item 12 cites, is new in `9b31a697`. The cut is stacked on PR 275
and follows its head, and `main` pins the head the cut was last rebased onto
(D5, [Merge gate and pin](#merge-gate-and-pin)). On 2026-11-08 the head was
`5ea8d93c`, and the 2026-11-08 revision rechecked at `5ea8d93c` only the facts
the answers rely on. The cut now pins `14a6ca8b`
(`14a6ca8bb5a5cbe055a1f725bdab8f42569a3bf0`), PR 275's head on 2026-11-11. It
is four commits after `a7d7fd0f`, the previous pin, which was five commits
after `dcbf622`, the cut's first pin, and changed only the absolute target of
the Axum native timer in production code. The four commits change production
code in two places: Spin frames an empty outbound body only for `POST`, `PUT`,
and `PATCH`, and EdgeZero adds a received-head preparer that no adapter calls
yet. Every other citation is still at H.

**Trusted Server base:** `main` at `51730bdaf`, which pins EdgeZero `v0.0.11`.

**Requirements:**
[EdgeZero requirements for the PR 275 hard cutover](2026-10-17-edgezero-pr275-requirements.md),
requirements EZ-1 to EZ-25 and the gate EZ-Q1. That file is the copy Trusted
Server handed over. PR 275 carries EdgeZero's copy, which has been amended since,
including its 2026-10-24 Scoped Amendment and later criteria changes. EdgeZero
is measured against its own copy, and this spec cites that copy's criterion
numbers.

**Tracking:** #1505, the Auctioneer integration epic linked with
IABTechLab/auctioneer#8. This cut is its first scope item. #840 tracks the
wider EdgeZero migration.

Citations: `crates/...:<line>` is Trusted Server at `51730bdaf`.
`edgezero:<path>:<line>` is EdgeZero at H. `edgezero@5ea8d93c:` is PR 275's
head on 2026-11-08. `edgezero@v0.0.11:` is the current pin.

## Problem

Trusted Server pins the six EdgeZero crates to `tag = "v0.0.11"`
(`Cargo.toml:59-64`). PR 275 replaces EdgeZero's request lifecycle and adds a
portable outbound HTTP client, with no compatibility layer. Between the earlier
head `f6c708cc` and H, EdgeZero built the nine blocking requirements Trusted
Server asked for (EZ-1 to EZ-9) and most of EZ-10 and EZ-16 to EZ-23. A
requirement-by-requirement review of H on 2026-11-06 found five blocking
defects, 30 should-fix findings, and 5 nits, mostly documentation
([Open EdgeZero defects at H](#open-edgezero-defects-at-h)).

Repinning `main` to H without code changes fails to compile on every target. A
compile probe at `8f82547b` found 167 error sites under nine API changes: 63 in
core, 38 in the Fastly adapter, 34 in Spin, 22 in Cloudflare, and 10 in Axum. That count is
a floor, because rustc stops at type checking.

Three behavior changes survive the mechanical fixes and break Trusted Server at
runtime.

- Inbound bodies arrive as deadline-bounded streams on every adapter, Fastly
  included (`edgezero:crates/edgezero-adapter-fastly/src/request.rs:857-876`).
  `into_bytes().unwrap_or_default()` then reads an empty body, so
  `POST /auction` and EC batch sync parse nothing
  (`crates/trusted-server-core/src/auction/endpoints.rs:153`,
  `crates/trusted-server-core/src/ec/batch_sync.rs:152`). `request_body_bytes`
  rejects the stream with 400, which breaks first-party sign and rebuild,
  request signing, and cache purge.
- The Fastly, Cloudflare, and Spin outbound clients reject streamed request
  bodies (`crates/trusted-server-adapter-fastly/src/platform.rs:366-368`,
  `crates/trusted-server-adapter-cloudflare/src/platform.rs:330-335`,
  `crates/trusted-server-adapter-spin/src/platform.rs:514-519`), so publisher
  forwards fail on three adapters. Axum's client buffers them.
- Every response passes EdgeZero's egress framing, Fastly included once it runs
  the canonical lifecycle. Any `Trailer` header becomes a 500
  (`edgezero:crates/edgezero-core/src/response_egress_framing.rs:127-128`).
  `make_response_bodiless` keeps an origin `Trailer` on some HEAD and 304
  publisher responses (`crates/trusted-server-core/src/publisher.rs:2970-3010`).
  After the cut, H strips `Trailer` from upstream heads
  (`edgezero:crates/edgezero-core/src/outbound.rs:2211-2228`), so the remaining
  sources are operator config and Trusted Server's own code.

The maintainer decided the cut adopts PR 275 completely. Trusted Server deletes
its own outbound layer, Fastly moves onto EdgeZero's lifecycle, and EdgeZero
follows redirects. This spec maps every site onto H. It names the places where
H cannot express today's behavior and says what Trusted Server gives up there
until EdgeZero's follow-ups land.

## Goals

- Move all six EdgeZero crates to PR 275 in one pull request, with no shim,
  dual path, or feature flag.
- Send every outbound request through EdgeZero's `HttpClient`. Delete
  `PlatformHttpClient`, `PlatformBackend`, backend naming, the four outbound
  clients, the Spin decoder, the Cloudflare encoding strip, the redirect loop,
  and Trusted Server's Fastly response delivery.
- Run Fastly on EdgeZero's canonical lifecycle inside Trusted Server's sandbox
  loop, with Trusted Server logic around and inside the `prepare` and
  `finalize` hooks.
- Keep Fastly responses, auction semantics, consent, and identity as they are
  today, apart from the changes in [Fastly differences](#fastly-differences).
- Read every inbound body through a bounded EdgeZero reader, with today's
  per-endpoint caps.
- Produce Trusted Server's share of EdgeZero's qualification evidence (EZ-Q1).
  The maintainer runs the hosted legs by hand. Neither the merge nor production
  deploys wait for EdgeZero's EZ-Q1 record (D5).

## Non-goals

- Building what H lacks: surrogate keys (EZ-11), a certificate-check mode
  (EZ-12), a Spin host override (EZ-13), an idle bound for streamed bodies
  (EZ-14), fire-and-forget sends (EZ-15), and a published test double (EZ-24).
  Trusted Server takes the interim each requirement names. It does not build
  its own version.
- Reusable sandboxes in production. The `reusable-sandbox` feature stays off by
  default (`crates/trusted-server-adapter-fastly/Cargo.toml:13-18`). EZ-2's
  reuse-mode parts are follow-ups.
- Streaming publisher responses on Axum, Cloudflare, and Spin. They keep
  collecting every publisher response variant before egress, raw ones included,
  with `buffer_publisher_response_async`
  (`crates/trusted-server-core/src/publisher.rs:1853`) (2.5).
- Declaring stores on Cloudflare and Spin. Their apps declare none today, and
  their config and KV fallbacks stay (section 7).
- Toolchain changes. Rust 1.98.1, Viceroy 0.21.0, Fastly CLI 16.0.0, Node
  24.20.0, and Spin 4.1.0 already match H.
- Moving Spin from `wasm32-wasip1` to `wasm32-wasip2`.
- Changing consent, identity, auction, or HTML rewriting behavior.
- Replacing `PlatformTimer` with EdgeZero's clock. The HTML end-of-file budget
  keeps using it
  (`crates/trusted-server-core/src/publisher/html_finalization.rs:315`).

## Decisions

| #   | Decision                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     | Owner                                              |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------- |
| D1  | Hard cut onto PR 275's APIs in one pull request. No compatibility layer, shim, dual path, or feature flag.                                                                                                                                                                                                                                                                                                                                                                                                   | Maintainer                                         |
| D2  | No Trusted Server transport plumbing. Every outbound request uses EdgeZero's `HttpClient`. Where H lacks a feature, Trusted Server takes the requirement's interim.                                                                                                                                                                                                                                                                                                                                          | Maintainer                                         |
| D3  | Fastly calls `send_request_with_registries_and_hooks` inside the `run_custom` and `serve_custom` loop. Trusted Server logic moves into the callback around it and into `prepare` and `finalize`.                                                                                                                                                                                                                                                                                                             | Maintainer                                         |
| D4  | Redirect following uses `HttpClient::follow` (EZ-9).                                                                                                                                                                                                                                                                                                                                                                                                                                                         | Maintainer                                         |
| D5  | The cut is stacked on PR 275. It pins PR 275's head and repins at each rebase. `main` merges it without waiting for EdgeZero's EZ-Q1 record and pins the head the cut was last rebased onto. Production deploys do not wait for qualification.                                                                                                                                                                                                                                                               | Maintainer (Q1, Q5)                                |
| D6  | Requests use the adapter's default client with the proxy marker off. Fastly post-delivery work uses a fresh EdgeZero Fastly client.                                                                                                                                                                                                                                                                                                                                                                          | Spec author                                        |
| D7  | Every request sets a constant 1 s connect timeout and a per-call `timeout` that never depends on a remaining budget. It is the site's configured timeout where the site map names one, 2 s for Tinybird, and otherwise 60 s streamed and 15 s buffered. Redirect chains get a 60 s deadline. The response write deadline is 60 s after egress starts. Each value is Trusted Server's to set, and EdgeZero exposes every knob for it.                                                                         | Spec author, confirmed by the maintainer (Q2)      |
| D8  | Sites that relay bytes or run Trusted Server's own rewrite codec use `ResponseDecoding::Original`. Sites that parse a body Trusted Server does not decode itself use `Decode`. One deviation: the Cloudflare publisher origin uses `Decode` (2.5).                                                                                                                                                                                                                                                           | Spec author; Cloudflare deviation: maintainer      |
| D9  | `ts config validate` and `push` reject `certificate_check = false` and `origin_readthrough_enabled = true`. At runtime both are ignored with a warning. Spin ignores `origin_host_header_override` with a warning, which deviates from the EZ-13 interim (Configuration explains why).                                                                                                                                                                                                                       | Spec author, from the interims, with one deviation |
| D10 | Trusted Server never sends `Trailer`, and never sends `Content-Length` or a hop-by-hop field from operator config. Validate and push reject such operator entries. Runtime skips them with a warning.                                                                                                                                                                                                                                                                                                        | Spec author                                        |
| D11 | Fastly auction telemetry is queued during the request and sent after delivery inside the serving callback (EZ-15 interim).                                                                                                                                                                                                                                                                                                                                                                                   | Spec author, from the EZ-15 interim                |
| D12 | Tests use a scripted `OutboundHttpClient` in Trusted Server test code until EZ-24 lands.                                                                                                                                                                                                                                                                                                                                                                                                                     | Spec author, from the EZ-24 interim                |
| D13 | EZ-2 measurements baseline on the `main` commit the cut branch is based on when the maintainer runs them, instead of `a9037770a` plus #1123. EdgeZero acknowledges the change before its hosted phase.                                                                                                                                                                                                                                                                                                       | Spec author                                        |
| D14 | A Spin component that fails to instantiate blocks the cut. CI's Spin job proves it.                                                                                                                                                                                                                                                                                                                                                                                                                          | Spec author                                        |
| D15 | Framework responses carry `x-ts-version` through EdgeZero's synthesized response headers (EZ-19), where EdgeZero applies them. On Fastly, `prepare` supplies a per-request set that adds the service version, environment, geo, and operator headers (EZ-19.2).                                                                                                                                                                                                                                              | Spec author                                        |
| D16 | The Axum dev server reads `EDGEZERO__ADAPTER__INGRESS__*` from the environment.                                                                                                                                                                                                                                                                                                                                                                                                                              | Spec author                                        |
| D17 | Upstream failures keep today's statuses. Only the statuses the cut introduces on purpose (413, 408, and the publisher-forward 400) change.                                                                                                                                                                                                                                                                                                                                                                   | Spec author                                        |
| D18 | Provider endpoint validation also accepts plain `http` for the loopback IP literals `127.0.0.1` and `[::1]`, so test fixtures reach local bidder stubs.                                                                                                                                                                                                                                                                                                                                                      | Maintainer (Q6)                                    |
| D19 | Trusted Server confirms as present the follow-ups the cut uses: EZ-10, EZ-16, EZ-17, EZ-18, EZ-19, EZ-20, EZ-22, and EZ-23. The criteria of their present parts join the gate (EZ-Q1.6), and the record maps conditional legs (EZ-Q1.8). EZ-18.4's bodiless part follows D20. EZ-22.6's Cloudflare and Spin retention recipe is not adopted, so EZ-22 criterion 10 is deferred. EZ-13 and EZ-21 are not confirmed, so their criteria stay out of the gate. EZ-21's interim applies, and D9 replaces EZ-13's. | Spec author                                        |
| D20 | Accept EdgeZero's strict suppression of bodiless Fastly responses (HEAD, 204, 205, 304) in place of SDK framing parity. Trusted Server withdraws the bodiless part of EZ-18.4. EdgeZero amended EZ-18.4, EZ-18.6, and EZ-18 criteria 3 and 6 to match in `dcbf622`.                                                                                                                                                                                                                                          | Maintainer (Q7)                                    |

D1 to D4 are settled and not open for review. The rationale for each other
decision sits in the section that implements it.

## Design

### 1. Pin, lockfile, manifests, and fixtures

**Pin.** The six EdgeZero crates move from `tag = "v0.0.11"` to a `rev` on the
same `IABTechLab/edgezero` git source. Features and `default-features` stay as
they are. The rev is PR 275's head, `14a6ca8b` at the 2026-11-11 repin, and
the cut repins it at each rebase. At merge, `main` pins the head the cut was
last rebased onto (D5).

**spin-sdk.** Trusted Server's `spin-sdk` must come from the same source as
EdgeZero's: `version = "=7.0.0"` from the `stackpop/spin-rust-sdk` git fork at
rev `6c59ed5b4fcb377072763aee0b27a3abdd8edb72` (`edgezero:Cargo.toml:106`).
With crates.io `~7.0` (`Cargo.toml:111`) the graph holds two `spin-sdk` 7.0.0
packages, and `check-spin` fails with E0308 at
`crates/trusted-server-adapter-spin/src/lib.rs:18` and E0277 at `:15`. The
probe confirmed that both errors clear after the switch.

**Lockfile.** H's 26 exact pins move these entries:

| Crate                 | Lock today | After  | Why                                   |
| --------------------- | ---------- | ------ | ------------------------------------- |
| `async-compression`   | 0.4.42     | 0.4.43 | `=0.4.43`                             |
| `brotli-decompressor` | 5.0.3      | 5.0.1  | `=5.0.1`, a downgrade                 |
| `bytes`               | 1.12.0     | 1.12.1 | `=1.12.1`                             |
| `hyper`               | 1.10.1     | 1.12.0 | `=1.12.0`                             |
| `want`                | 0.3.1      | 0.3.2  | required by hyper 1.12.0              |
| `syn`                 | 3.0.4      | 3.0.9  | required by the fork's `wit-bindgen`  |
| `spin-sdk` stack      | crates.io  | git    | the fork brings `wasip3` 0.10.0 along |

A scoped `cargo update -p edgezero-core` also rewrote the `colored` 3.1.1 and
`winapi-util` 0.1.11 edges from `windows-sys` 0.61.2 to 0.48.0, which no pin
requires. Restore those by hand and validate with `--locked`. The probe's final
lock removes 25 packages, adds 27, and changes 3 edges, all explained. The
`esi` patch stays valid, because it accepts `fastly ^0.13`.

While Trusted Server pins PR 275, H's exact pins are version ceilings. They
include `serde_json`, `bytes`, `http`, `url`, `hyper`, the `fastly` crates at
0.13.1, `worker` 0.8.5, `reqwest` 0.13.4, and the compression crates.
Dependabot updates of those fail resolution. Ignore rules are a follow-up.

**`edgezero.toml`.** Add:

```toml
[capabilities.outbound]
hosts = ["*"]
```

Without it, `ts build`, `ts serve`, and `ts deploy --adapter spin` fail with
"Spin outbound host drift". An absent list means HTTPS only
(`edgezero:crates/edgezero-core/src/manifest.rs:1152-1169`), while
`crates/trusted-server-adapter-spin/spin.toml:43` allows HTTP and HTTPS. `"*"`
expands to both (`manifest.rs:1106-1120`). Declare no required or optional
capabilities. The manifest now rejects unknown top-level keys
(`manifest.rs:343-376`), and Trusted Server's has none.

**Wrangler files.** Add `enable_weak_ref` and `enable_request_signal` to
`compatibility_flags` in `crates/trusted-server-adapter-cloudflare/wrangler.toml:12`
and `wrangler.ci.toml:6`, as H's template does
(`edgezero:crates/edgezero-adapter-cloudflare/src/templates/wrangler.toml.hbs:4`).
EdgeZero's guide says its bounded config and KV reads need the first
(`edgezero:docs/guide/adapters/cloudflare.md:38-42`). Disconnect observation
needs the second. `cache_option_enabled` stays, because H's Cloudflare client
sets the fetch `cache` field for `Bypass`.

**Certificate check in fixtures and CI.** The three integration fixture configs
(`crates/trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml:96-97`,
`trusted-server.trace.toml:38`, `trusted-server.trace-auth.toml:37`), the two
integration scripts (`scripts/integration-tests.sh:76`,
`scripts/integration-tests-browser.sh:39`), and the CI build steps
(`.github/workflows/test.yml:282`,
`.github/actions/setup-integration-test-env/action.yml:94`, `:104`, `:135`)
drop `certificate_check = false`. Their local origins use plain HTTP, so the
setting has no effect there today, and nothing reads it at build time. D9
rejects it. The doc comment at `crates/trusted-server-core/src/settings.rs:1916`
changes to "Must be true. Origin TLS is always verified."

**Test bidders (D18).** Today CI reaches local bidder stubs through a Viceroy
alias. `generate-viceroy-config` predicts Trusted Server's Fastly backend names
and registers static backends under them that point at the loopback origin
(`crates/trusted-server-integration-tests/src/bin/generate-viceroy-config.rs:118-196`).
Trusted Server's dynamic registration then gets `NameInUse` and treats it as
reuse (`crates/trusted-server-adapter-fastly/src/backend.rs:262-265`). On H the
alias cannot work. Backend names hash private identity fields
(`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:1009-1023`), an
unknown `NameInUse` fails the request (`outbound.rs:1132-1134`), the naming code
the generator compiles against is deleted, and HTTPS is always verified.
Provider endpoints must be HTTPS today
(`crates/trusted-server-core/src/auction/plan.rs:565-584`).

- Endpoint validation also accepts `http://127.0.0.1:<port>` and
  `http://[::1]:<port>`. Hosted runtimes cannot reach loopback, so production
  behavior does not change.
- `--bidder-origin-url` becomes `--provider-endpoint-origin <url>`. When it is
  given, the generator replaces the scheme and authority of every
  `auction.providers.*.endpoint` and keeps the path and query. It stops
  injecting backend aliases, so `inject_controlled_bidder_backends`, its
  `BackendNamingPolicy` import, and its tests
  (`generate-viceroy-config.rs:370-468`) are deleted.
  `INTEGRATION_BIDDER_ORIGIN_URL` stays and sets the flag. Only the two trace
  runs set it (`.github/workflows/integration-tests.yml:39-40`,
  `scripts/generate-integration-viceroy-configs.sh:45-46`,
  `scripts/integration-tests-browser.sh:45-50`), so the integration config's
  providers keep their endpoints.
- The Next.js trace browser test runs on Viceroy only. It adds
  `/api/trace-bidder` to its forbidden evidence list
  (`crates/trusted-server-integration-tests/browser/tests/nextjs/mobile-trace-live.spec.ts:381-389`),
  because the rewritten endpoint no longer contains `bidder.example.com`.
- `scripts/template-cache-local-test.sh`, which the required `cargo test` job
  runs (`.github/workflows/test.yml:69-76`), serves its stub bidder over plain
  HTTP on `127.0.0.1` and sets the `pbs-main` endpoint to
  `http://127.0.0.1:{bid_port}/bid`. Its CA generation, `openssl` check, TLS
  socket wrap, and backend pre-registration under Trusted Server's predicted
  name (`:82-120`, `:239`, `:350-391`) are deleted. The `hb_pb` assertions stay.
- Tests 5 and 6, EZ-3 criterion 6, and EZ-6's Trusted Server leg configure
  loopback bidders the same way on every runtime.

### 2. Outbound HTTP on EdgeZero's client

#### 2.1 One client per request

`RuntimeServices` drops its `http_client` and `backend` fields and their
builders (`crates/trusted-server-core/src/platform/types.rs:182-184`,
`:258-268`, `:418-430`). It gains two required fields,
`outbound_client: HttpClient` and `clock: MonotonicClock`. A new field name
keeps the old and new clients apart while sites are ported (commit sequence).
Both types are `Send + Sync`
(`edgezero:crates/edgezero-core/src/outbound.rs:419-422`,
`edgezero:crates/edgezero-core/src/time.rs:28-31`), which matters because
`RuntimeServices` travels in response extensions.

Each adapter's per-request service builder fills them from
`ctx.http_client()` (`edgezero:crates/edgezero-core/src/context.rs:570`) with
`.with_proxy_marker(false)` (`outbound.rs:806`), and from
`ctx.monotonic_clock()` (`context.rs:691`) (D6). Every adapter inserts a default
client (`edgezero:crates/edgezero-adapter-fastly/src/request.rs:851-853`,
`edgezero:crates/edgezero-adapter-axum/src/request.rs:93`,
`edgezero:crates/edgezero-adapter-cloudflare/src/request.rs:380`,
`edgezero:crates/edgezero-adapter-spin/src/request.rs:164`). A missing client is
an internal error, not a panic.

Tests that drive a router with `oneshot` have no adapter to insert a client.
`ctx.http_client()` reads only request extensions (`context.rs:568-572`), and
`oneshot` adds none (`edgezero:crates/edgezero-core/src/router.rs:662-668`).
H's Cloudflare and Spin clients are wasm-only, so native tests have no default
either. `test_support.rs` gains `insert_outbound_client`, which inserts any
`HttpClient` the test passes into a request's extensions. The route helpers
pass a scripted client, which fails every request the test did not script.
Every `oneshot` route helper uses it: the Fastly `route` helper
(`crates/trusted-server-adapter-fastly/src/app.rs:1497-1510`) and the Cloudflare
and Spin `route` helpers in `tests/routes.rs`. The parity `cf_*` and `spin_*`
helpers (`crates/trusted-server-integration-tests/tests/parity.rs:132-315`) and
the inline Cloudflare request in `discovery_route_body_is_json_parity`
(`parity.rs:371-378`) leave `oneshot` for `AxumTestServer` instead (section 8).
Tests that inject `RuntimeServices` through `routes_with_settings_and_services`
already carry their client.

Fastly post-delivery work (2.8) runs after the request is gone. It builds a
fresh `HttpClient::with_client(FastlyOutboundClient::new()).with_proxy_marker(false)`
and uses `MonotonicClock::default()`. The adapter default is the same client
type over the app's clock
(`edgezero:crates/edgezero-adapter-fastly/src/request.rs:851-853`), the app
clock is the default because Trusted Server never sets one, and
`FastlyOutboundClient::new()` uses `MonotonicClock::default()`
(`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:601-611`). Request and
post-delivery deadlines are therefore in one clock domain. EdgeZero's own
post-delivery fixture builds its client the same way
(`edgezero:crates/edgezero-test-harness/fixtures/reusable-app/crates/fixture-fastly/src/bin/outbound_progress/post_delivery.rs:86-103`).

The marker is off for every call. It is a response header that a relay would
pass to browsers, and Trusted Server never reads it on JSON calls, so one
setting covers both cases (EZ-7). Production code installs no client in router
state, which would override the adapter default
(`edgezero:crates/edgezero-core/src/router.rs:410-416`). Tests on the Axum test
server do, so no real transport starts (EZ-23.7). The exception is test 19's
bodiless GET, which supplies EdgeZero's Axum client.

#### 2.2 Settings every request carries

| Today                                                                                            | On H                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| ------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Backend first-byte and between-bytes timers                                                      | `.timeout(d)`, a total budget (D7). Fastly rounds `d` up to a power of two of milliseconds for the provider timer, and the guest enforces `d` itself (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:555-563`, `:1025-1031`). A request with only a deadline spreads over up to 31 backend configurations per origin, so every request sets `timeout`, and the value is a constant per site, never a remaining budget.                                       |
| Fixed 1 s Fastly connect timeout (`crates/trusted-server-adapter-fastly/src/backend.rs:240`)     | `.connect_timeout(Duration::from_secs(1))` on every request (EZ-4, `outbound.rs:1082`). Fastly backend identity includes the exact connect value in milliseconds (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:988-992`, `:1018-1021`), so it never depends on a budget. A 1 s connect bound over a shorter total is harmless, because the guest budget takes the earliest bound (`edgezero:crates/edgezero-core/src/time.rs:186-197`).                    |
| Transport-timeout canonicalization (`crates/trusted-server-core/src/platform/traits.rs:117-136`) | Deleted. Constant per-site timeouts and a constant connect timeout keep backend identity stable, which is the reuse canonicalization protected.                                                                                                                                                                                                                                                                                                                        |
| `with_cache_bypass()`                                                                            | `.cache_policy(OutboundCachePolicy::Bypass)`. Fastly maps it to pass (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:1089-1091`).                                                                                                                                                                                                                                                                                                                            |
| `with_stream_response()`                                                                         | `.stream_response()`. Sites that only set `with_streaming()` (passthrough) stay buffered, as today.                                                                                                                                                                                                                                                                                                                                                                    |
| Platform response cap of 10 MiB on Fastly and Cloudflare                                         | `.max_response_bytes(n)` on every buffered site. H defaults to 1 MiB (`outbound.rs:77`). Sites with no Trusted Server cap today set 10 MiB, so they keep today's limit.                                                                                                                                                                                                                                                                                                |
| Encoded bytes on Fastly and Axum                                                                 | `.response_decoding(..)` per D8. `Original` leaves `Accept-Encoding` as Trusted Server set it (`outbound.rs:1459`).                                                                                                                                                                                                                                                                                                                                                    |
| Explicit `Host` header                                                                           | H strips it before dispatch (`edgezero:crates/edgezero-core/src/outbound/dispatch.rs:101`, `outbound.rs:2229-2232`). A `Host` that differs from the URI authority, compared case-insensitively with default ports removed, becomes `.host_authority_override(..)` (`outbound.rs:1239`). On Fastly an override is part of backend identity (`outbound.rs:999`, `:1011`). Spin rejects any override (`edgezero:crates/edgezero-adapter-spin/src/outbound.rs:1526-1530`). |
| Backend `certificate_check`                                                                      | No equivalent. Verification is always strict (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:650-655`). D9 rejects `false`.                                                                                                                                                                                                                                                                                                                                  |
| Backend names, `ensure`, `predict_name`                                                          | Deleted. EdgeZero names Fastly backends `ez_b31v1_<sha256>` (`outbound.rs:1009-1023`), and batch results are positional.                                                                                                                                                                                                                                                                                                                                               |

H applies these rules to every request and response at dispatch. They are
listed in [Fastly differences](#fastly-differences).

- `Host` and `Content-Length` are dropped from requests, and H frames the body.
- The adapter clients add `Accept-Encoding: identity` when the request has none
  (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:688-692`; Axum,
  Cloudflare, and Spin do the same). Core dispatch adds nothing.
- Header values that are not UTF-8 are dropped from requests and responses,
  with a warning that names the header (`outbound.rs:2192-2209`, called at
  `:2075` and `:1758`). The whole field goes, not one value inside it.
- Hop-by-hop fields, and fields that `Connection` names, are stripped from
  requests and responses, `Trailer` included. A malformed `Connection` field is
  a 400 on a request and a 502 on a response (`outbound.rs:2080-2100`,
  `:2211-2234`).
- The URI is canonicalized as a WHATWG URL. Dot segments are removed, `%2e%2e`
  included, and default ports are dropped. Userinfo, fragments, and backslashes
  are rejected with 400 (`outbound.rs:1953-1986`, `:2248-2279`). Raw-string
  constructors such as `OutboundRequest::get` reject `#` anywhere
  (`outbound.rs:2248-2253`), so proxies build their requests with
  `OutboundRequest::new(method, uri)` from a parsed `http::Uri`, which drops a
  fragment while parsing. Creative click URLs with fragments keep working.

Sites that copy client headers write into `OutboundRequest::headers_mut()`
(`outbound.rs:1220`), never `header()`, because `header()` rejects a value that
is not UTF-8 and would fail the whole call (`outbound.rs:1191-1211`).

#### 2.3 Site map

| Site                               | Trusted Server code                                                                                                                                                                                                                                                                 | Call on H                                    | Response                                                                       | Timeout                                                                                                |
| ---------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------- | ------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------ |
| O1 Auction bidders                 | `GenericOpenRtbProvider::request_bids_routed` (`crates/trusted-server-core/src/auction/provider.rs:273-400`)                                                                                                                                                                        | One `start_batch_until` per auction (2.4)    | Buffered per profile: PBS and APS 2 MiB, Standard 1 MiB, Decode                | Configured provider timeout                                                                            |
| O2 Auction mediator                | `AdServerMockProvider::request_bids` (`crates/trusted-server-core/src/integrations/adserver_mock.rs:435-523`)                                                                                                                                                                       | `send` after the batch (2.4)                 | Buffered 2 MiB (`UPSTREAM_RTB_MAX_RESPONSE_BYTES`, unchanged), Decode          | Configured mediator timeout, plus the auction deadline                                                 |
| O3 Auction entry points            | `/auction` `handle_auction` (`crates/trusted-server-core/src/auction/endpoints.rs:407`), page bids `handle_page_bids` (`crates/trusted-server-core/src/publisher.rs:7634`), and the publisher page path                                                                             | O1 and O2                                    | As O1 and O2                                                                   | As O1 and O2                                                                                           |
| O4 Publisher, overlapped           | `publisher.rs:5318-5352`, `:5676`                                                                                                                                                                                                                                                   | `begin` (2.5)                                | Streamed, Original                                                             | 60 s                                                                                                   |
| O5 Publisher, plain                | `publisher.rs:5683-5697`                                                                                                                                                                                                                                                            | `send` with `from_request` (2.5)             | Streamed, Original                                                             | 60 s                                                                                                   |
| O6 Redirecting proxies             | `proxy_request` (`crates/trusted-server-core/src/proxy.rs:795-843`) from the first-party proxy (`proxy.rs:1577-1610`), GTM (`integrations/google_tag_manager.rs:1081`), the Prebid bundle (`integrations/prebid.rs:1281-1288`), and testlight (`integrations/testlight.rs:199-216`) | `follow` (2.6), or `send` when not following | Buffered 10 MiB (testlight 2 MiB), Original                                    | 15 s per hop, 60 s chain deadline                                                                      |
| O7 GPT                             | `integrations/gpt.rs:282`                                                                                                                                                                                                                                                           | `send`                                       | Buffered 10 MiB, Original                                                      | 15 s                                                                                                   |
| O8 JS asset proxy                  | `integrations/js_asset_proxy.rs:509`                                                                                                                                                                                                                                                | `send`                                       | Streamed, Original                                                             | 60 s                                                                                                   |
| O9 Asset proxy                     | `handle_asset_proxy_request` (`proxy.rs:1096-1220`), S3 preflight (`proxy.rs:1025-1094`)                                                                                                                                                                                            | `send` (2.7)                                 | Final GET streamed, Original. Final HEAD and preflight HEAD buffered, Original | 60 s GET, 15 s HEAD                                                                                    |
| O10 Sourcepoint                    | `integrations/sourcepoint.rs:817-960`                                                                                                                                                                                                                                               | `send`                                       | Streamed, Original                                                             | 60 s                                                                                                   |
| O11 Lockr                          | SDK GET (`integrations/lockr.rs:108-170`), API proxy (`lockr.rs:179-260`)                                                                                                                                                                                                           | `send`                                       | SDK buffered 16 MiB, Decode. API buffered 10 MiB, Original                     | 15 s                                                                                                   |
| O12 Permutive                      | SDK GET (`integrations/permutive.rs:108-170`), proxy (`permutive.rs:183-260`)                                                                                                                                                                                                       | `send`                                       | As Lockr                                                                       | 15 s                                                                                                   |
| O13 DataDome                       | `tags.js` (`integrations/datadome.rs:633-700`), `js_api` (`datadome.rs:716-800`)                                                                                                                                                                                                    | `send`                                       | `tags.js` buffered 16 MiB, Decode. `js_api` 10 MiB, Original                   | 15 s                                                                                                   |
| O14 DataDome Protection            | `filter_protection_request_inner` (`integrations/datadome/protection.rs:78-135`)                                                                                                                                                                                                    | `send`                                       | Buffered 10 MiB, Decode                                                        | `config.timeout_ms`                                                                                    |
| O15 Didomi                         | `integrations/didomi.rs:407-510`                                                                                                                                                                                                                                                    | `send`                                       | Buffered 10 MiB, Original. Bypass or `PlatformDefault` as today                | 15 s                                                                                                   |
| O16 EC pull sync                   | `dispatch_pull_sync` (`crates/trusted-server-core/src/ec/pull_sync.rs:116-300`)                                                                                                                                                                                                     | `send_all_until` per group (2.8)             | Buffered 64 KiB (`pull_sync.rs:410`), Decode                                   | 15 s                                                                                                   |
| O17 Tinybird                       | `crates/trusted-server-adapter-fastly/src/tinybird.rs:133-182`                                                                                                                                                                                                                      | `send` after delivery (2.8)                  | Ignored                                                                        | 2 s, today's timers (`tinybird.rs:19-20`)                                                              |
| O18 Fastly management API          | `FastlyManagementApiClient::make_request` (`crates/trusted-server-adapter-fastly/src/management_api.rs:158-188`)                                                                                                                                                                    | `send` from the admin handler (2.9)          | Buffered 1 MiB, Decode                                                         | 15 s                                                                                                   |
| O19 EOF fixture, `test-utils` only | `crates/trusted-server-core/src/test_support/async_html_finalization.rs:126-181`                                                                                                                                                                                                    | `send` from the end-of-file finish hook      | Buffered 64 bytes, Decode, Bypass                                              | `.timeout(transport_timeout_ms)`. On Fastly also `.deadline` at the remaining chain budget (see below) |

GTM asks for `identity`, so `Original` and `Decode` behave the same for it
when the origin honors that. The requirements doc lists it under decoding.

The EOF fixture keeps reporting `min(remaining, transport_timeout_ms)` on
Fastly and `transport_timeout_ms` elsewhere, because
`crates/trusted-server-integration-tests/tests/html_eof_runtime.rs:753-766`
asserts those values. Its phase timers come from the constant timeout, so its
backend identity stays stable.

Every row keeps today's method and request headers, with the dispatch rules in
2.2 applied. Bidder requests keep their consent and attested client-IP
headers. The DataDome Protection filter runs before routing, and it gets the
client from `RuntimeServices` like every handler. ESI assembly and the template
cache make no HTTP calls.

#### 2.4 Auction fan-out

- `ProviderRequestOutcome::Pending { request: PlatformPendingRequest, parse_state }`
  (`provider.rs:47-57`) becomes `Ready { request: OutboundRequest, parse_state }`.
  Providers build the request and return it unsent. `request_bids_routed`
  (`provider.rs:384-393`) and the mediator's `AuctionProvider::request_bids`
  (`adserver_mock.rs:513-523`) return `Ready`.
  `parse_response_with_context_and_state` takes an `OutboundResponse`
  (`crates/trusted-server-core/src/auction/orchestrator.rs:2627-2634`).
  `AuctionContext::transport_timeout_ms` is deleted, with its declaration
  (`crates/trusted-server-core/src/auction/types.rs:156`) and the test
  constructor that sets it (`auction/test_support.rs:30`). APS's planned
  parser, `parse_planned_aps_response`
  (`crates/trusted-server-core/src/integrations/aps.rs:879-884`), takes an
  `OutboundResponse`, like the Prebid parser.
- `dispatch_planned_auction` (`orchestrator.rs:1686`) puts the bidder requests
  in `client.start_batch_until(requests, auction_deadline)` (`outbound.rs:734`).
  On Fastly the call is synchronous and must hand every slot to the SDK before
  it returns (EZ-6.7). At H it stops at the first slot that completes during
  setup, so later slots wait for the next EdgeZero poll
  ([open defect 1](#open-edgezero-defects-at-h)). Fixed at the pin: EdgeZero's
  fix is in `598f0a2a`, an ancestor of `14a6ca8b`, and Trusted Server keeps no
  workaround for it. Axum, Cloudflare, and Spin
  spawn a local task
  (`edgezero:crates/edgezero-adapter-axum/src/outbound.rs:268`,
  `edgezero:crates/edgezero-adapter-cloudflare/src/outbound.rs:537`,
  `edgezero:crates/edgezero-adapter-spin/src/outbound.rs:622-625`), so their
  bidder requests leave at Trusted Server's next await.
  `DispatchedAuction` holds the `OutboundBatch` and a slot-to-provider map.
  Bidder requests now leave together after all are built and signed. Today each
  leaves as soon as it is built (`orchestrator.rs:1839-1850`).
- Bidders set `.timeout` to the configured provider timeout, not to today's
  `remaining_budget_ms(..).min(provider.timeout_ms())`
  (`orchestrator.rs:1817-1818`). The batch cutoff bounds them by the remaining
  budget, so the effective bound is the same and backend identity stays stable
  (D7).
- `auction_deadline` is `services.clock.deadline_after(timeout)`, taken at
  auction start. It replaces `collection_deadline(auction_start, timeout_ms)`
  (`orchestrator.rs:258`).
- `collect_dispatched_auction` replaces its `select_until` loop
  (`orchestrator.rs:2286-2289`) with `batch.next()` until the batch reports
  `Finished`. Where today's code polls without waiting before the deadline, it
  calls `batch.check()` (`edgezero:crates/edgezero-core/src/outbound/batch.rs:430`),
  which reports only outcomes an earlier EdgeZero poll recorded. Collection
  after the cutoff calls `batch.next()`, which returns `Finished(Cutoff)`
  without a host wait. At the pin under Viceroy 0.21.0, a `check` after the
  cutoff returns `Finished(Cutoff)` too (section 8).
- A `Cutoff` termination leaves slots `Unresolved`. Those become provider
  timeouts in the auction result, as expiry does today since #1433. Collection
  maps an `OutboundBatchFailure` to a provider error for every slot it leaves
  unresolved. On Spin at H, a batch whose earlier-deadline slot is still
  unobserved when the cutoff passes fails that way with an internal error
  ([open defect 3](#open-edgezero-defects-at-h)). Trusted Server adds no
  workaround. Fixed at the pin: EdgeZero's fix is in `598f0a2a`, an ancestor
  of `14a6ca8b`.
- The mediator goes out after the batch finishes, with `client.send(..)`,
  `.timeout(mediator timeout)`, and `.deadline(auction_deadline)`. The guest
  budget takes the earlier of the two
  (`edgezero:crates/edgezero-core/src/time.rs:160-200`).
- Dropping the batch or a pending request cancels it
  (`batch.rs:414-423`, `edgezero:crates/edgezero-core/src/outbound/progress.rs:94-104`).
  `DispatchedAuction::abandon` (`orchestrator.rs:95-131`), an origin error
  (`publisher.rs:5700-5714`), a non-HTML response, or a client that disconnects
  mid-stream therefore cancels in-flight bidder and mediator requests. Today
  Fastly lets them run to completion. Only the guest side is guaranteed. Fastly
  host cancellation is best effort and unqualified
  (`edgezero:docs/guide/capabilities.md:285`, `:352`), so a bidder may still receive
  and answer a cancelled request.
- `AuctionPlan::validate_for_target` (`crates/trusted-server-core/src/auction/plan.rs:417-466`),
  `AuctionTargetId`, and the target descriptors are deleted with
  `backend_naming.rs`, with their calls in the four adapters' state builders
  (`crates/trusted-server-adapter-fastly/src/app.rs:225`,
  `crates/trusted-server-adapter-cloudflare/src/app.rs:208`,
  `crates/trusted-server-adapter-axum/src/app.rs:95`,
  `crates/trusted-server-adapter-spin/src/app.rs:137`) and the test at
  `crates/trusted-server-core/src/auction/mod.rs:198-202`. Settings load no
  longer rejects a multi-provider plan on Cloudflare or Spin. The planned
  auction also drops its refusal when `!supports_concurrent_fanout()`
  (`orchestrator.rs:1700-1709`). All four adapters run batches (EZ-6), so
  Cloudflare and Spin now fan out, which closes issue #1484.
- The Fastly state builder keeps today's startup bound of 160 provider
  backends. It replaces the call at
  `crates/trusted-server-adapter-fastly/src/app.rs:225` with core's
  `validate_fastly_backend_bound` (`plan.rs:455`). The limits 200 and 40 live
  beside it in core (`crates/trusted-server-core/src/auction/plan.rs:21-23`).
  It counts distinct tuples of provider scheme, host, port, and configured
  timeout, because each tuple is at most one EdgeZero backend configuration.
  The count can only overstate. EdgeZero keys identity on the timer rung, so
  two timeouts that round to one rung share a configuration but count twice.
  Bidders set no host override, and the connect timeout is constant.
  The predicted-name collision check (`plan.rs:451-464`) goes, because EdgeZero
  owns names. The Cloudflare and Spin tests that rejected the plan became
  `cloudflare_and_spin_load_multi_provider_plan`
  (`crates/trusted-server-adapter-cloudflare/src/app.rs:941`,
  `crates/trusted-server-adapter-spin/src/app.rs:1105`), which assert that the
  plan loads. The `validate_for_target` tests (`plan.rs:695-830`) and the four
  target-descriptor tests (`platform.rs` in each adapter) are deleted.
- `canonicalize_transport_timeout_ms`, the backend-name prediction and
  uniqueness checks (`provider.rs:310-344`), and `reserved_backend_names` are
  deleted.
- Batch slots must be buffered both ways (`outbound.rs:1924-1934`). Bidder
  requests are built fresh with buffered JSON bodies, so they qualify.

`OutboundSlotResult::elapsed` runs from the batch's shared start to the slot's
recorded terminal observation, or its logical expiry for a classified timeout
(`edgezero:crates/edgezero-core/src/outbound/batch.rs:653-674`). Today each
provider's `response_time_ms` runs from its own launch, including request
construction and signing (`orchestrator.rs:1839`), until publisher-side
collection (`orchestrator.rs:2375`, `publisher.rs:4717-4723`). The new metric
includes launch skew from the shared start but excludes the wait between a
successful response's arrival and collection. Delaying collection after a
timely response must not increase its reported slot elapsed time.

On Fastly, EdgeZero records arrivals in any EdgeZero poll that services the
provider. That covers outbound waits, body reads, EdgeZero's timers, and every
poll of the canonical handler and egress drivers
(`edgezero:crates/edgezero-adapter-fastly/src/request.rs:427`,
`edgezero:crates/edgezero-adapter-fastly/src/response.rs:573`, through
`edgezero:crates/edgezero-adapter-fastly/src/outbound/progress.rs:352` and
`edgezero:crates/edgezero-adapter-fastly/src/outbound/scope.rs:207-224`).
`batch.check()` and `is_ready()` record nothing. They report only outcomes an
earlier poll recorded, and never wait
(`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:5456-5494`).
Cross-cutting requirement 5 and EZ-6.11 require a check to record, so this is
[open defect 2](#open-edgezero-defects-at-h). Fixed at the pin: EdgeZero's fix
is in `6f0cc74c`, an ancestor of `14a6ca8b`. At the pin under Viceroy 0.21.0,
a `check` after the cutoff returns `Finished(Cutoff)` (section 8). Collection
after the cutoff calls `batch.next()`, which also returns `Finished(Cutoff)`
without a host wait. The
publisher path reads the origin body through EdgeZero while it streams, and
those reads keep arrivals current. EdgeZero's `body-read-arrivals` harness case
covers that
(`edgezero:crates/edgezero-test-harness/src/net/wire/lifetimes.rs:2246`). A long
CPU stretch with no EdgeZero call can still push a bid's observation past the
cutoff, and the ledger then drops it (`batch.rs:645-650`). Test 6 checks the
publisher path end to end.

The cfg(test) orchestration paths (`run_routed`, `run_parallel_mediation`,
`run_parallel_only`, and `run_providers_parallel`, at `orchestrator.rs:356-840`,
`:1015`, `:1193`, `:1216`) and the cfg(test) `AuctionProvider` impls for APS
and Prebid (`integrations/aps.rs:1675`, `integrations/prebid.rs:3495`) move to
the scripted client (D12) or are deleted with their tests.

#### 2.5 Publisher origin

The overlapped path runs today when
`should_preload_ec && template_cache_key.is_none() && supports_concurrent_fanout && supports_pending_streaming_responses`
(`publisher.rs:5318-5325`). The two capability terms are deleted, and
`should_preload_ec` stays. The overlap exists to hide the EC KV snapshot read
behind the origin fetch, and `should_preload_ec` requires a KV graph
(`publisher.rs:5298-5310`, `:3459-3466`). Axum, Cloudflare, and Spin pass no
KV graph to `handle_publisher_request`
(`crates/trusted-server-adapter-axum/src/app.rs:256-264`,
`crates/trusted-server-adapter-cloudflare/src/app.rs:564-572`,
`crates/trusted-server-adapter-spin/src/app.rs:846-854`), and the cut keeps
their unavailable-KV fallbacks (section 7). So the overlap still runs only on
Fastly navigation GETs that preload an EC snapshot. Everywhere else the plain
path dispatches the auction first and then sends the origin request while the
bids are in flight (`publisher.rs:5467-5470`, `:5673-5697`). EZ-6 describes
the overlapped path the same way, so its Trusted Server check holds on both
paths.

The origin request:

- It is built with `OutboundRequest::from_request(req, origin_uri)?` (EZ-1,
  `outbound.rs:1154`), which keeps the inbound body and the admitted read
  deadline. `restrict_accept_encoding` (`publisher.rs:302`) and the
  `fastly-ssl` strip stay.
- `rewrite_origin_request` (`publisher.rs:3474-3495`) stops writing `Host`.
  Today it writes `settings.publisher.origin_host_header()`, which is
  `origin_host_header_override` when set and the origin host otherwise
  (`crates/trusted-server-core/src/settings.rs:257-261`), and the backend
  repeats the override (`publisher.rs:4978-4994`). On H, an override that
  differs from the origin URI authority becomes `.host_authority_override(..)`
  on Fastly, Axum, and Cloudflare. Spin sets none (D9).
- Options are `.stream_response()`, `Original`, the D7 timeouts, and the cache
  policy. `Bypass` maps to `Bypass`, and `Default` to `PlatformDefault`.
  `Shared` with a surrogate key is unreachable once D9 disables origin
  readthrough. `with_shared_cache` has one production caller
  (`publisher.rs:4849`).

**Cloudflare deviation from D8.** Workers rewrites the inbound
`Accept-Encoding` to `br, gzip` whether the client sent none or `identity`, so
`restrict_accept_encoding` forwards a coding the client may not accept, and the
origin answers in it. The Cloudflare publisher origin therefore sets
`ResponseDecoding::Decode`, raw passthrough included, and serves identity bytes
as Trusted Server's Cloudflare client did. A direct workerd probe (Wrangler
4.83.0 and 4.131.0) showed that EdgeZero's outbound `Accept-Encoding: identity`
is not replaced, so EdgeZero meets EZ-5 and the cause is Cloudflare's inbound
normalization. EZ-5's rules still apply under `Decode`: a 206 keeps the origin
coding, and HEAD and 304 responses keep their `Content-Encoding`.

Fastly and Axum keep `Original`, which relays encoded bytes as they do today
(the settings table in 2.2). Spin moves to `Original`. Trusted Server's Spin client decoded gzip and
br and dropped `Content-Encoding` and `Content-Length`
(`crates/trusted-server-adapter-spin/src/platform.rs:344-378`), so Spin clients
always got identity bytes. After the cut a raw gzip or br response reaches the
Spin client in the origin coding, processed HTML is re-encoded in it, and the
8 MiB input cap counts a raw body's encoded bytes. That matches Fastly and
Axum, and it is a client-visible Spin change.

Both paths dispatch the auction before the origin response is read, and the
batch must outlive the handler.

1. `handle_publisher_request` creates `owner = OutboundScopeOwner::response_body()`
   (`edgezero:crates/edgezero-core/src/outbound/scope.rs:223`) before the
   auction is dispatched, and returns `(PublisherResponse, OutboundScopeOwner)`.
   Every variant of `PublisherResponse` (`publisher.rs:1598-1660`) travels with
   it, `AssembleTemplate` on a template-cache hit included.
2. It awaits `owner.run(orchestrator.dispatch_auction(..))` (`scope.rs:253-264`)
   in place of today's plain await (`publisher.rs:5467-5470`). The batch that
   `dispatch_planned_auction` starts captures the ambient scope at call time
   (`outbound.rs:739-741`), so the response body owns it.
3. Both origin calls are also constructed inside `owner.run`:
   `owner.run(async { client.send(origin).await })` on the plain path, and
   `owner.run(async { client.begin(origin).await })` on the overlapped path. H
   captures the ambient scope when `send` or `begin` is called, not when the
   future is first polled (`outbound.rs:578-583`, `:700-705`), so
   `owner.run(client.send(origin))` still captures the callback scope.
   Outside the response-body scope, head finishing gives a streamed body its
   own response-body owner (`edgezero:crates/edgezero-core/src/outbound/head.rs:296-302`,
   `:384-400`). The raw `Buffered` and `PassThrough` conversions keep that body
   (`publisher.rs:1868-1877`, `:2093-2095`, `:2546-2556`), so the attach below
   would fail with "response body already owns an outbound scope". Inside the
   scope, head finishing binds the body to the shared scope and adds no owner
   (`head.rs:388-393`). Test 20 covers both calls and every raw variant.
4. The adapter call site attaches the owner exactly once, after
   `publisher_response_into_streaming_response` (`publisher.rs:2500`, Fastly) or
   `buffer_publisher_response_async` (the other three) returns:
   `owner.attach(response)?` (`scope.rs:164`). Neither function attaches,
   because the Fastly function calls the buffering one itself on a
   template-cache miss (`publisher.rs:2530-2546`), and `attach` fails on a body
   that already has an owner (`scope.rs:164-175`).
5. EdgeZero's egress releases the work when egress ends. Attaching to a
   buffered body is harmless. A handler error drops `owner`, which cancels the
   batch. Code that rebuilds a response body after the attach converts with
   `into_stream()`, which carries the owner. It never splits with
   `into_parts()` and rebuilds with `from_stream`, because dropping the
   returned owner closes the batch.

On Axum, Cloudflare, and Spin, `buffer_publisher_response_async` collects every
variant before it returns, so nothing streams to egress there (Non-goals). H
collects only `ResponseMode::Buffered` bodies (`head.rs:288-301`), and a
streamed response has no byte cap by default (`outbound.rs:1514-1515`,
`:1554-1556`). Without collection, the raw `Buffered` and `PassThrough`
variants, which the function returns unchanged today (`publisher.rs:1868-1877`,
`:2093-2095`), would stream, lose today's client caps, and fail during egress
instead of before it.

Today's clients bound the origin input before the publisher classifies the
response (`publisher.rs:5944-5964`, `:6046-6075`), so the bound covers HTML as
well as raw responses. Cloudflare's client caps the declared length and the
received bytes at 10 MiB
(`crates/trusted-server-adapter-cloudflare/src/platform.rs:187`, `:385-395`,
`:422-440`). Spin's client caps the received bytes at 8 MiB and separately
bounds gzip and br expansion at 8 MiB
(`crates/trusted-server-adapter-spin/src/platform.rs:29-33`, `:344-372`,
`:541-543`). Axum's client has no cap
(`crates/trusted-server-adapter-axum/src/platform.rs:364-366`). The cut keeps
those input bounds for every publisher classification, raw and processed
alike:

- Each adapter has an origin input cap `cap`: the smaller of
  `publisher.max_buffered_body_bytes` and its current client cap, so 10 MiB on
  Cloudflare and 8 MiB on Spin by default. Axum has no client cap, so only the
  setting applies there. Each adapter passes `cap` to
  `handle_publisher_request` and `buffer_publisher_response_async`, because
  the client modules that hold those constants are deleted.
- The origin request sets `.max_encoded_response_bytes(cap)`, so H fails any
  origin body over `cap` whatever its classification. These adapters collect
  before egress, so that failure arrives before the response is committed.
- Raw `Buffered` bodies, pass-through responses included, are collected with
  `Body::into_bytes_bounded(cap)`.
- Processed HTML is decoded by Trusted Server's rewrite codec, because the
  origin request uses `Original`. Its cumulative decoded bytes are bounded by
  `cap`, as the old clients bound the bytes they returned. The streaming path
  already applies a cumulative decoded bound, so this uses the same mechanism
  with `cap` (`crates/trusted-server-core/src/settings.rs:70-75`).
- The processed output keeps its separate `publisher.max_buffered_body_bytes`
  cap (`publisher.rs:1921`). An input bound and an output bound are different
  limits, and both apply.
- Overflow and source errors fail before egress with the site's 502. That is
  `TrustedServerError::Proxy`, or H's `ResponseTooLarge` through 2.11, as the
  client caps fail today. None of these go through the `PayloadTooLarge` rule
  in 2.11.

Test 21 covers this.

The overlapped path (O4) adds the origin `begin`:

1. `owner.run(async { client.begin(origin).await }).await?` (`outbound.rs:578`)
   before the auction is dispatched. The request is on its way once this future
   resolves. A `begin` future that is never awaited sends nothing.
2. Dispatch the auction under `owner.run`, as above.
3. `pending.collect().await` returns the streamed origin response
   (`edgezero:crates/edgezero-core/src/outbound/progress.rs:106`). A collected
   streamed response owns its own transport
   (`edgezero:docs/guide/proxying.md:83-88`).

`begin` accepts a bodiless GET or HEAD, or a buffered body
(`outbound.rs:1916-1923`). The overlapped path is GET navigation only. On
Fastly, EZ-1 settles the inbound body by reading it to the end, which is
immediate for a bodiless GET. A forward that starts after the inbound read
deadline fails with 408 even when bodiless
(`edgezero:crates/edgezero-core/src/outbound/dispatch.rs:160-175`). The origin
fetch starts milliseconds after the request arrives, so only pathological
requests reach that.

The plain path (O5) also covers POST, PUT, PATCH, DELETE, and OPTIONS. It calls
`client.send(..)` inside `owner.run`, as above, with the same request and
options, and hands the inbound body stream upward with
`.max_request_body_bytes(16 MiB)`. Overflow returns 413
(EZ-10). Axum and Cloudflare prebuffer the upload under the cap before they
contact the origin (EZ-1.11, EZ-10.6), so their origin sees no request on
overflow. Fastly and Spin stream it. On Fastly the overflow is found after
`send_async_streaming` has started
(`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:1555`, `:1462-1470`),
so the origin sees the head and up to 16 MiB of an aborted upload. Only a
buffered body is checked before sending
(`edgezero:crates/edgezero-core/src/outbound.rs:1903-1908`). Today Fastly
reads the whole inbound body into memory with no cap.

A streamed Spin upload also loses its declared length. The Spin inbound body
is a stream (`edgezero@14a6ca8b:crates/edgezero-adapter-spin/src/request.rs:97`),
`from_request` drops `Content-Length` with the other request framing fields
(`edgezero@14a6ca8b:crates/edgezero-core/src/outbound.rs:2253-2256`), and the
Spin client sets `Content-Length` only for a buffered body
(`edgezero@14a6ca8b:crates/edgezero-adapter-spin/src/outbound.rs:900-915`). So
a Spin publisher POST, PUT, or PATCH reaches the origin chunked, with no
`Content-Length`, where Trusted Server's Spin client buffered the upload and
sent its length (`crates/trusted-server-adapter-spin/src/platform.rs:512-523`).
An origin or WAF that requires a length answers 411 or rejects the request.
Trusted Server does not buffer the upload to restore the length. EdgeZero is
asked to keep the declared `Content-Length` for a streamed Spin upload
([PR 275 comment](https://github.com/IABTechLab/edgezero/pull/275#issuecomment-6863865880)).
`spin_runtime_publisher_post_body_reaches_origin` asserts the chunked framing,
so a repin that carries the fix fails it.

#### 2.6 Redirect following

`proxy_with_redirects` (`proxy.rs:1276-1560`) and `ProxyRedirectPolicy`
(`proxy.rs:770`) are replaced by
`client.follow(request, OutboundRedirectLimit::new(4)?, policy)`
(`outbound.rs:611-636`,
`edgezero:crates/edgezero-core/src/outbound/redirect.rs:320-340`). Today's
`MAX_REDIRECTS` of 4 (`proxy.rs:1284`) stays.

- `proxy_request` checks the initial target before `follow` or `send`, as today:
  scheme, `require_https`, host, and allowlist (`proxy.rs:1293-1327`). H never
  shows the first request to the policy (`redirect.rs:64`).
- The policy is a `'static` closure (`outbound.rs:623-635`) that owns a copy of
  `allowed_domains` and `require_https`, since `ProxyRedirectPolicy` borrows
  them today (`proxy.rs:770-776`). For each redirect it reads
  `resolved_target()` and returns, in today's order (`proxy.rs:1492-1524`):
  - `Err(Forbidden)` when `require_https` is set and the scheme is not `https`
  - `Stop` for any other scheme that is not `http` or `https`, which returns
    the 3xx as today
  - `Err(Proxy)` for a missing host, as today
  - `Err(AllowlistViolation)` when `is_host_permitted` rejects the host
  - `FollowPrepared` otherwise

  The policy also owns whether the request carries a body. When it follows
  the first 301, 302, or 303 of such a request, it warns that the body is
  dropped, naming only the target's host, as today's per-hop warning did. The
  HTTPS-required warnings name the scheme and host only, never the path or a
  query such as `ts-ec`.

- Followed requests set `.timeout(15 s)` per hop and
  `.deadline(clock.deadline_after(60 s))` for the whole chain. Per-call timeouts
  restart on every hop, and only a deadline holds across the chain
  (`outbound.rs:613-617`). Today each hop has 15 s timers and the chain has no
  bound.
- When `follow_redirects` is false, the caller uses `send`. The request keeps
  `PROXY_FORWARD_HEADERS` (`proxy.rs:67-73`), the fixed `Accept-Encoding`
  (`proxy.rs:454`), caller headers, and buffered body bytes.
- `finalize_response` (`proxy.rs:747`) decodes and re-encodes on its own
  (`proxy.rs:482-512`), so follow requests set `Original`. It receives the
  final URL from `OutboundFollowResult::final_uri()` (`redirect.rs:226-229`),
  which it uses to rewrite relative creative URLs (`proxy.rs:1426-1431`).
- `OutboundFollowFailure::Policy` maps back to the policy's Trusted Server
  error. `OutboundFollowFailure::Hop` goes through the error mapping in 2.11.

H matches Trusted Server on the rest (`redirect.rs:388-533`). 301, 302, and 303
rewrite to GET, except HEAD, and drop the body and its headers. 307 and 308
replay buffered bodies. A method that cannot be rewritten, the limit, or a
missing `Location` returns the 3xx.

Accepted differences:

- Cross-origin hops strip `Authorization`, `Cookie`, and the host authority
  override (`redirect.rs:392-407`). Trusted Server's proxies set none of them
  by default, so only caller-set values are affected.
- A `Location` with userinfo fails as a protocol error, which becomes the site's
  502 (`redirect.rs:479-492`). So do multiple or non-text `Location` values on
  Fastly, Axum, and Spin. Cloudflare never sees them: workerd merges duplicate
  fields and maps raw bytes to text, and EdgeZero follows the merged value
  (`edgezero:crates/edgezero-adapter-cloudflare/src/outbound.rs:648-652`,
  `:857-875`). Trusted Server took the first value and followed userinfo.
- A UTF-8 `Location` with non-ASCII characters is percent-encoded and followed.
  Trusted Server returned the 3xx, because `to_str` rejects non-ASCII bytes
  (`proxy.rs:1434-1438`). Locations with spaces were already percent-encoded
  and followed.
- A 3xx that is returned instead of followed now gets the call's ordinary body
  handling, so it can fail on the response cap or on decoding (EZ-9.11,
  EZ-9.12, `edgezero:crates/edgezero-core/src/outbound/head.rs:227-317`).
- `Expect` is removed on every body-dropping rewrite.
- Hops log at debug level without the query (`redirect.rs:212-222`).

#### 2.7 Asset proxy and Image Optimizer

- `send_asset_origin_request` (`proxy.rs:974-1001`) and
  `handle_asset_proxy_request` build `OutboundRequest::new(method, uri)?` with
  `ASSET_PROXY_FORWARD_HEADERS` (`proxy.rs:90`), `.stream_response()` on the
  GET, `Original`, the D7 timeouts, and the cache policy the caller passes. The
  signed S3 HEAD preflight, its raw error-body GET, and a signed raw HEAD
  without Image Optimizer options pass `Bypass` (#1516, ported in merge
  `a53f4d04c`), so Fastly's readthrough cache cannot turn a signed HEAD into a
  GET after signing. Every other asset request passes `PlatformDefault`. The
  S3 HEAD preflight stays buffered, as today (`proxy.rs:1037-1044`), and also
  sets `Original`. At H, `Decode` leaves a HEAD's encoded `Content-Encoding` and
  `Content-Length` alone only because EZ-5.9 is not adopted. `Original` keeps
  that metadata stable if EdgeZero adopts it.
- `asset_origin_host_header` (`proxy.rs:893-916`) stays. It is the signer's
  `host` input, and `s3_sigv4::sign_headers` fails without it
  (`crates/trusted-server-core/src/s3_sigv4.rs:128-147`). H strips `Host`
  before dispatch and sends the URI authority, so the header never reaches the
  wire. Test 12 checks that the authority, path, and query H sends equal what
  Trusted Server signed. H re-parses the URI as a WHATWG URL
  (`outbound.rs:1953-1989`), so they must agree byte for byte.
- Image Optimizer options go on with `.image_optimizer(opts)`
  (`outbound.rs:1263`), on the final client GET or HEAD, never on the diagnostic
  S3 HEAD preflight. H requires GET or HEAD, `PlatformDefault` caching, and a
  region, and returns 400 otherwise
  (`edgezero:crates/edgezero-core/src/outbound/image_optimizer.rs:197-217`).
  The asset route admits HEAD, a successful preflight proceeds to the final
  request, and the final request keeps the client's method and attaches the
  options today (`proxy.rs:1052-1057`, `:1171-1174`), so a final HEAD keeps its
  options. Test 3 and the EZ-8 hosted leg cover the final HEAD.
  `begin` and batches reject the options. Axum, Cloudflare, and Spin reject
  them before sending (`edgezero:crates/edgezero-adapter-axum/src/outbound.rs:153-160`,
  `edgezero:crates/edgezero-adapter-spin/src/outbound.rs:1520-1525`), as
  Trusted Server's own clients do today.
- Settings load already accepts only values H's types can express: known
  regions (`settings.rs:1131-1142`), H's format and filter sets plus aliases
  (`:1530-1560`), quality 0 to 100 (`:1497-1505`), positive dimensions, and crop
  offsets 0 to 100 that never combine with smart crop (`:1630-1640`,
  `crates/trusted-server-core/src/asset_image_optimizer.rs:330-343`). No new
  validation is needed.
- `PlatformImageOptimizerOptions` and its Fastly mapping
  (`crates/trusted-server-core/src/platform/image_optimizer.rs`,
  `crates/trusted-server-adapter-fastly/src/platform.rs:238-322`) are deleted.
  Per request, the asset proxy maps the validated profile plus the request's
  aspect ratio and offsets (`asset_image_optimizer.rs:88-89`, `:125-188`) to
  `OutboundImageOptimizerOptions`, with Trusted Server's region aliases mapped
  to H's seven regions. `preserve_query_string_on_origin_request` maps to
  `preserve_origin_query`. A builder error, which validation makes unreachable,
  serves the image unoptimized with a warning.
- Viceroy answers Image Optimizer requests with 502
  `ImageOptimizerUnavailable`
  (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:88-91`), unverified
  by a run. Proof needs an IO-entitled staging service (EZ-8). The synchronous
  optimizer call has no approved blocking bound yet, because EdgeZero's
  G4-OPTIMIZER-HORIZON approval is pending.

#### 2.8 Post-delivery work on Fastly

Both items run inside the serving callback after
`send_request_with_registries_and_hooks` returns `Routed`, and only when the
delivery committed with the application's body (section 3). Today pull sync
runs after every send (`crates/trusted-server-adapter-fastly/src/main.rs:547-556`).
They use the fresh client and clock from 2.1. EdgeZero releases
callback-owned batch drivers when the callback returns
(`edgezero:crates/edgezero-adapter-fastly/src/lifecycle.rs:145-146`), so both
finish before it does. Neither delays the client, which already has its
response. Each extends the sandbox's lifetime, as pull sync does today.

One outer `futures::executor::block_on` in `run_post_delivery`
(`crates/trusted-server-adapter-fastly/src/post_delivery.rs:45-61`) drives
both items, which await their sends inside it. That call is safe only here.
Inside the helper, dispatch already runs under `block_on`
(`edgezero:crates/edgezero-adapter-fastly/src/request.rs:427-431`), and nesting
panics. EdgeZero's fixture uses `block_on` in the same position
(`post_delivery.rs:93-178`).

- **Telemetry (D11).** `send_fire_and_forget` drops a pending request, and
  Fastly keeps it running (`tinybird.rs:133-148`). On H, dropping an
  `OutboundPending` cancels it (`progress.rs:94-110`). The sink is shared app
  state today (`crates/trusted-server-adapter-fastly/src/app.rs:229`, `:289`).
  After the cut, the callback creates a queue, an `Arc<Mutex<Vec<String>>>`,
  and `prepare` inserts a clone into the request extensions.
  `build_per_request_services` wraps the clone from `ctx.extensions()` in a
  per-request sink that queues NDJSON instead of sending. After delivery the
  callback drains the queue and awaits `client.send(..)` for each row, with
  today's headers, `.timeout(2 s)`, the 1 s connect timeout, and `Bypass`. Each
  row is one short request. Axum, Cloudflare, and Spin keep their no-op sink.
- **EC pull sync.** `dispatch_pull_sync` takes the post-delivery client and
  clock instead of the ones in `ec_state.services`
  (`crates/trusted-server-adapter-fastly/src/main.rs:679-686`). Each group of
  `ec.pull_sync_concurrency` requests (`pull_sync.rs:184`) goes out with
  `client.send_all_until(group, cutoff).await` (`outbound.rs:722`).
  Requests carry `.timeout(15 s)` and `.max_response_bytes(64 KiB)`, and the
  cutoff is `clock.deadline_after(15 s)` per group. Today each request has 15 s
  idle timers and no total bound, so the per-group cutoff is new.

Telemetry goes first, because its rows are short and pull sync can take a full
group budget.

#### 2.9 Fastly management API

`FastlyManagementApiClient::make_request` is a synchronous raw
`fastly::Request::send`. Key rotation reaches it through synchronous store-write
methods (`crates/trusted-server-core/src/request_signing/rotation.rs:96-285`,
`crates/trusted-server-adapter-fastly/src/platform.rs:78`, `:83`, `:144`,
`:149`). A synchronous wrapper around `HttpClient::send` would need `block_on`
inside a handler, which panics (2.8).

`PlatformConfigStore::put` and `delete`, and `PlatformSecretStore::create` and
`delete`, become async with `#[async_trait(?Send)]`, like the other platform
traits (`crates/trusted-server-core/src/platform/http.rs:301`), because
`RuntimeServices` holds both stores as `Arc<dyn ..>` (`platform/types.rs:162-164`).
The traits have no default bodies (`platform/traits.rs:12-36`, `:42-93`), so all
37 implementers in `crates/` change in step 8, and so does the guide snippet. That includes the test stores in
`config_payload.rs`, `request_signing/jwks.rs`, `secret_resolution.rs`, and
`settings_data.rs`. The Fastly
implementations hold the request's `HttpClient` and send
`OutboundRequest::new(method, uri)?` with `Fastly-Key`,
`Accept: application/json`, `Content-Type` (`management_api.rs:170-178`), and
the body, with a 1 MiB response cap and the D7 15 s timeout. Error bodies keep
today's 200-character truncation (`management_api.rs:33`, `:79-102`). The other
adapters keep returning their current unsupported error. Rotation and its admin
handlers await the calls.

#### 2.10 Capability methods

`supports_concurrent_fanout`, `supports_streaming_responses`, and
`supports_pending_streaming_responses` are deleted, with their 16, 16, and 7
call lines. On H every adapter streams and runs batches.

#### 2.11 Outbound errors

Today every site wraps transport errors in its own context, and `Proxy`,
`Integration`, `Auction`, and `Prebid` all map to 502
(`crates/trusted-server-core/src/error.rs:116-135`). Trusted Server has no 504
variant. One helper,
`outbound_error(error: EdgeError, context: TrustedServerError, source: OutboundRequestSource)`,
maps every `HttpClient` failure and every request-construction error
(`from_request`, `host_authority_override`, and the builders) (D17). `source`
says who shaped the request: `ClientForward` for a forward of the client's own
request, and `Internal` for a request Trusted Server built. Only the publisher
forward passes `ClientForward`.

- `PayloadTooLarge` becomes `RequestTooLarge` (413).
- `RequestTimeout` becomes `RequestTimeout` (408, section 4). It comes from the
  inbound read deadline while a forwarded body is settled or uploaded.
- `BadRequest` becomes `BadRequest` (400) only for
  `OutboundRequestSource::ClientForward`. The client's request usually causes
  it: a GET with a body, a malformed `Connection`, or a URI with a fragment or
  backslash. A `publisher.origin_url` with userinfo also causes it, so
  validation rejects such a URL (Configuration).
- Everything else keeps the site's context, which is 502 as today. That covers
  timeouts, connect failures, protocol errors, a Fastly `NameInUse`, and a
  `BadRequest` with `OutboundRequestSource::Internal`, from a request Trusted
  Server built itself.

The helper attaches the `EdgeError`, its budget source, and its
`provider_failure_detail()` to the report. It does not log; section 9 says
who does.

### 3. Fastly on the canonical lifecycle

`main` keeps choosing `run_custom` for single-request sandboxes and
`serve_custom` for reuse (`crates/trusted-server-adapter-fastly/src/main.rs:96-152`).
The callback now returns `Result<(), Infallible>`
(`edgezero:crates/edgezero-adapter-fastly/src/lifecycle.rs:153-181`) and
always returns `Ok(())`. An `Err` would end the reuse loop with no response
(`lifecycle.rs:139-149`).

The callback runs these steps in order.

1. **Native short-circuits, unchanged.** The trace-path check, health probe,
   `setup_once(init_logger)`, request id, sandbox metrics probe (reuse only),
   and JA4 debug probe (`main.rs:172-208`, `:342`, `:362`) answer with native
   responses before EdgeZero runs. The metrics probe must stay ahead of app
   assembly. The callback takes `clock = MonotonicClock::default()` and
   `request_start = clock.now()` at entry. Only `send_assembly_error` uses them.
2. **Assembly.** `sandbox.initialize` builds the router and calls
   `App::from_router::<TrustedServerApp>(router, FASTLY_PLATFORM)`
   (`edgezero:crates/edgezero-core/src/app.rs:347`). `from_router` runs
   `Hooks::configure` once, so the hand-built `App` and its
   `Self::configure(&mut app);` call, which would drop a `Result` at H
   (`crates/trusted-server-adapter-fastly/src/app.rs:1280-1287`), go away. On
   error, the callback calls `send_assembly_error(req, error, request_start, clock)`
   (`edgezero:crates/edgezero-adapter-fastly/src/request.rs:627-645`) and
   handles its result like any other delivery. A failed assembly is not
   retained, as today. `startup_error_router` (`app.rs:1047`) is deleted,
   because `send_assembly_error` replaces it.
3. **Native request work.** The callback runs today's native mutations on the
   owned request before the send: `compat::capture_fastly_forwarder`
   (`main.rs:433-435`), client-IP resolution and sanitization, and re-inserting
   `fastly-ssl` and `x-ts-tls-*` with their trace-path conditions
   (`main.rs:439-472`). It computes `client_info_from_request` and
   `derive_device_signals` (`main.rs:476-478`) and keeps the resolved client IP.
   It runs the geo lookup on that IP once, for both `prepare` and `finalize`.
   It also captures `SandboxCounters` (`main.rs:425-426`) and creates the
   telemetry queue (2.8). `RequestIngress` capture reads only the URL and
   `Host` (`edgezero:crates/edgezero-adapter-fastly/src/request.rs:772-782`),
   and none of these mutations touch either
   (`crates/trusted-server-adapter-fastly/src/compat.rs:66-130`).
4. **Send.**
   `send_request_with_registries_and_hooks(app, TrustedServerApp::stores(), req, prepare, finalize)`
   (`edgezero:crates/edgezero-adapter-fastly/src/request.rs:594-631`).
5. **Outcome** handling, post-delivery work (2.8), and the observation log.

`prepare(&mut FastlyRequest, &mut Extensions)` moves the computed values into
the extensions that `into_core_request` carried (`main.rs:484`): the forwarder,
device signals, client info, and the telemetry queue. The helper captures
`RequestIngress` before `prepare` and removes any copy `prepare` inserts
(`request.rs:466-468`), so `prepare` must not insert one. `prepare` also inserts
the request's synthesized header set, which the helper takes out before head
limits are checked (EZ-19.2, `request.rs:469`). Section 5 lists its fields.
`open_trusted_server_config_store` and its `ConfigStoreHandle` extension
(`main.rs:65-70`, `:384-394`, `:489`) are deleted, because nothing reads it. A
missing config store now fails settings load and surfaces as an assembly error.

The `finalize` hook calls
`finalize_response(&mut Response, &FinalizeInputs) -> Option<FinalizeState>`
(`crates/trusted-server-adapter-fastly/src/main.rs:414-479`).
`FinalizeInputs { settings, geo, counters }` carries the settings, the
request's one geo lookup, and the sandbox counters. It takes over today's
post-routing steps, all synchronous:

1. If `TraceTerminalResponse` is present, return without touching the response.
   Today it is sent raw (`main.rs:508-516`). Now it passes EdgeZero's egress,
   which strips hop-by-hop fields. Whether any trace header changes is
   unverified. The response diff (Runtime) covers a trace request.
2. Remove `EcFinalizeState`, `AssetProxyCachePolicy`, and `RequestFilterEffects`
   (`main.rs:518-520`).
3. Apply finalize headers unless `x-ts-finalized` is set, with the geo result
   the callback computed in its step 3 (`main.rs:522-541`, `:618-637`). Then the asset cache
   policy (`main.rs:541-543`), then EC finalize (`main.rs:545-547`).
4. Apply the terminal privacy effects that `send_edgezero_response` applies
   today (`main.rs:776-808`), and the sandbox counter headers.
5. Return the pull-sync inputs: the partner registry and the EC state. It
   returns `None` for a trace response, for a response that carries no EC
   state to finalize, and when the partner registry cannot be built, which is
   logged at error.

`finalize` runs for every routed response, including handler errors and
canonical 404 and 405, before egress policy and framing
(`edgezero:crates/edgezero-adapter-fastly/src/delivery.rs:313-334`). Detached
responses skip it: head-limit 414 and 431, framing rejections at admission, and
admitted source errors. They carry the request's synthesized set (section 5),
which holds the version, environment, geo, and operator headers. They carry no
EC cookies, cookie cache privacy, or sandbox counters. A framing fallback
replaces a routed response after `finalize` ran, so its finalize side effects,
such as EC KV writes, have happened, but its cookies never reach the client. It
carries the synthesized set in place of the finalize headers.

Outcomes of `FastlyRequestDelivery` (`delivery.rs:183-312`). The callback
handles every delivery the same way, including the one `send_assembly_error`
returns. Every failure log carries the sandbox counter context
(`[instance= ordinal= request=]`, as at `main.rs:726-768`).

| Outcome                                                                       | Callback action                                                                |
| ----------------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| `NoAttempt` with `RequestHeadConversion`                                      | Native 500 with fixed text, as today (`main.rs:499-505`) and EZ-17 criterion 7 |
| `NoAttempt` with `RequiredStore`                                              | Unreachable, because Trusted Server declares no required store. Native 500     |
| `Routed`, committed, application body                                         | Post-delivery work with `state`, even when `delivery.error()` is set           |
| `Routed` or `Detached` with `may_send_native_response()` true (not committed) | Log at error, send a native 500                                                |
| Any other `Routed` (unknown commit state, or committed with a fallback body)  | Log at error. No post-delivery work and no second response                     |
| Any other `Detached`                                                          | Log the report at debug, or at warn for a source error                         |
| `Aborted`                                                                     | Unreachable, because Trusted Server installs no abort policy. Log at error     |
| Any other variant (`FastlyRequestDelivery` is `#[non_exhaustive]`)            | Log at error                                                                   |

Outcome handling is one function generic over `FastlyRequestDelivery<S>`. Only
the routed post-delivery arm needs `FinalizeState`. The assembly-error result
has `S = ()` (`request.rs:634-644`), and its `Routed` arm cannot occur. Tests
drive every row with EdgeZero's `delivery::testing` helpers (test 9). They build
envelopes with the public `App::assembly_error_egress`,
`App::admitted_error_egress`, and `App::dispatch_admitted`, and detached
outcomes with `App::detached_ingress_error_egress`
(`edgezero:crates/edgezero-core/src/app.rs:94`, `:120-125`, `:221-226`,
`:276`). Viceroy cannot produce `NoAttempt`, `Unknown`, or
`NotCommitted` on demand.

After post-delivery work, the callback logs a `FastlyOutboundObservation::current()`
snapshot at info on a dedicated target: configuration count, registration
count, and total and maximum registration time
(`edgezero:crates/edgezero-adapter-fastly/src/context.rs:202-265`). Hosted
Fastly logs at info (`crates/trusted-server-adapter-fastly/src/logging.rs:19-62`),
and EZ-2's criteria 9 and 15 read it. Taking it after post-delivery work
includes pull-sync and telemetry registrations.

Deleted from the Fastly adapter: the `oneshot` dispatch in `handle_request`
(`main.rs:494`), the `into_core_request` call (`main.rs:484`),
`edge_error_response` (`main.rs:597`), `send_edgezero_response` and
`send_response_to_client` (`main.rs:723-808`), and `compat::to_fastly_response*`
and `stream_to_client` (`compat.rs:16-64`). Core's `stream_asset_body`
(`crates/trusted-server-core/src/proxy.rs:288`) goes with its tests. The sandbox
counters and retirement log stay
(`crates/trusted-server-adapter-fastly/src/sandbox.rs`).

### 4. Inbound request bodies

`crates/trusted-server-core/src/http_util.rs` gains one helper,
`read_request_body(headers: &HeaderMap, body: Body, max: usize) -> Result<Bytes, Report<TrustedServerError>>`.

- It returns `RequestTooLarge` without polling the body when a single valid
  `Content-Length` exceeds `max`.
- Otherwise it awaits `Body::into_bytes_bounded(max)`
  (`edgezero:crates/edgezero-core/src/body.rs:163`), which checks the cap before
  each append and passes source errors through unchanged.
- It maps errors by status. 413 becomes `RequestTooLarge`. 408 becomes a new
  `TrustedServerError::RequestTimeout`, mapped to 408. Another 4xx becomes
  `BadRequest` (400). A 5xx becomes `Proxy` (502). A Fastly
  host read error is a 5xx (`edgezero:crates/edgezero-adapter-fastly/src/request.rs:917-921`).
  Today `collect_body_bounded` maps chunk errors to a 502 too
  (`crates/trusted-server-core/src/integrations/mod.rs:198-204`).
- It does not log, and body bytes never reach its report. Each adapter renders
  the report through core's `error::http_error`, which logs it once: at warn
  for a 4xx, which the client caused, and at error for a 5xx. The auction and
  batch-sync routes answer their own 413 and log the report at warn.

It replaces `request_body_bytes` (`proxy.rs:47`,
`crates/trusted-server-core/src/request_signing/endpoints.rs:27`) and
`collect_body_bounded` (`integrations/mod.rs:181-215`). Core handlers take a
`Request`, not a `RequestContext`, so the helper wraps the `Body` reader instead
of `RequestContext::body_bytes`.

| Reader                                                                                                   | Cap, unchanged                      | Change                                                                                                                                                                  |
| -------------------------------------------------------------------------------------------------------- | ----------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `/auction` (`auction/endpoints.rs:140-160`)                                                              | 256 KiB (`endpoints.rs:47`)         | Helper replaces `into_bytes().unwrap_or_default()`. The `text/plain` 413 body stays                                                                                     |
| EC batch sync (`ec/batch_sync.rs:140-160`)                                                               | 2 MB                                | Helper. The `body_too_large` error body stays                                                                                                                           |
| Cache purge (`cache_purge.rs:135-142`)                                                                   | 4096 (`cache_purge.rs:30`)          | Helper replaces the stream rejection                                                                                                                                    |
| First-party sign and rebuild (`proxy.rs:1724-1725`, `:1909-1910`)                                        | 64 KiB (`proxy.rs:40-41`)           | Helper                                                                                                                                                                  |
| Request signing verify, rotate, deactivate (`request_signing/endpoints.rs:116`, `:306`, `:425`)          | 4096                                | Helper                                                                                                                                                                  |
| Integration proxies: Sourcepoint, Lockr, Permutive, DataDome, Didomi, testlight                          | 256 KiB (`integrations/mod.rs:166`) | Helper                                                                                                                                                                  |
| GTM beacon (`google_tag_manager.rs:802-830`)                                                             | `max_beacon_body_size`              | Helper                                                                                                                                                                  |
| Trace actions in the pre-dispatch hook (`crates/trusted-server-core/src/trace/actions.rs:40-55`, `:186`) | empty only                          | The helper's `Content-Length` check, then `PreDispatchRequest::body_bytes(0)` (`edgezero:crates/edgezero-core/src/pre_dispatch.rs:21`), mapped to the trace JSON errors |
| Publisher forward (O5)                                                                                   | none today                          | `from_request` streams it, with a 16 MiB `max_request_body_bytes`                                                                                                       |

At H, EdgeZero's own readers drain up to the cap before they return 413, even
for a declared length over the cap. That is EZ-10.4, an open should-fix
(`edgezero:crates/edgezero-core/src/context.rs:338-358`). Trusted Server's
helper checks the declared length itself, and the trace action makes the same
check before `body_bytes(0)`, so neither polls the body for a declared
overflow. Trusted Server uses none of the `Json` or `Form` extractors, so their
new 8 MiB and 1 MiB default caps do not apply.

EdgeZero's default admission sets the inbound read deadline to request start
plus 30 s (`edgezero:crates/edgezero-core/src/ingress.rs:34`, `:730-736`), and
reads after it fail with 408. Trusted Server keeps the default (Q2), which a
16 MiB upload meets at about 4.5 Mbit/s. If uploads need more later,
`Hooks::configure` installs an admission policy (`App::set_ingress_admission_policy`)
that gives the publisher fallback routes a longer read deadline
(`ingress.rs:743-763`). On Fastly the deadline is checked between
blocking reads (`edgezero:crates/edgezero-adapter-fastly/src/request.rs:864-925`),
so a read that blocks is bounded by the host, not by 30 s.

**Guard.** `clippy.toml` gains a `disallowed-methods` entry for
`edgezero_core::body::Body::into_bytes`. It returns `None` for a stream
(`body.rs:141`), which is how the empty-body bug compiles. The reason names the
bounded readers: `read_request_body` for inbound bodies and
`OutboundResponse::into_bytes_bounded` (`outbound.rs:864`) for outbound
responses. It does not name `Body::as_bytes`, which also returns `None` for a
stream (`body.rs:41-44`).

- The core and adapter crate roots add
  `#![cfg_attr(test, allow(clippy::disallowed_methods))]`, so lib targets check
  production code and unit tests stay free to buffer.
- A `tests/*.rs` crate root adds `#![allow(clippy::disallowed_methods)]` only
  when its tests buffer a `Body`: the Cloudflare and Spin `tests/routes.rs` and
  the parity suite. Those are separate crates. A test crate that later buffers
  one adds the allow when clippy asks for it.
- The one `test-utils` module that buffers bodies,
  `test_support/scripted_client.rs`, carries a module-level allow. The
  crate-root `cfg_attr` must not widen to `test-utils`, because `clippy-fastly`
  builds with `--all-features` (`.cargo/config.toml:31`) and would then skip
  production code.
- Production code carries no allow. The proxy and publisher readers share one
  `pub(crate)` helper, `http_util::body_as_reader`, which matches on
  `into_body_payload` and returns an error for a stream, so the first-party
  proxy keeps buffering its responses. DataDome Protection no longer calls
  `into_bytes`. Ported outbound reads such as `into_body().into_bytes()`
  (`ec/pull_sync.rs:468-472`) move to `OutboundResponse::into_bytes_bounded`.

### 5. Response egress

These rules now apply on all four adapters.

**`Trailer` and `Content-Length` (D10).** Framing rejects any `Trailer`
(`response_egress_framing.rs:127-128`), and a buffered body whose
`Content-Length` disagrees with its length, as a 500 before commit.

- H strips `Trailer` from upstream response heads (`outbound.rs:2211-2228`,
  applied at `:929` and `edgezero:crates/edgezero-core/src/outbound/head.rs:166`),
  so relays never see one.
- `make_response_bodiless` (`publisher.rs:2970-3010`) removes `Trailer` in every
  branch. Today it keeps `Trailer` wherever chunked framing survives, and the
  test at `publisher.rs:26560-26575` asserts that. The test changes with it.
- `validate_settings_for_deploy` (`crates/trusted-server-core/src/config.rs:258`)
  rejects an operator `response_headers` entry named `trailer`,
  `content-length`, or any hop-by-hop field: `connection`, `keep-alive`, `te`,
  `transfer-encoding`, `upgrade`, `proxy-connection`, `proxy-authenticate`,
  `proxy-authorization`, or `http2-settings`. `ts config validate` and
  `ts config push` then fail with the header name. At runtime,
  `apply_response_headers_with_cache_privacy`
  (`crates/trusted-server-core/src/response_privacy.rs:161-188`) skips such an
  entry with a warning, so a live config that has one keeps loading.
- Integration response headers already drop both names
  (`crates/trusted-server-core/src/integrations/registry.rs:450-462`).

**Stripped fields.** Egress removes `Connection` and the fields it names,
`Transfer-Encoding`, `Keep-Alive`, `TE`, `Upgrade`, `Proxy-Connection`,
`Proxy-Authenticate`, `Proxy-Authorization`, and `HTTP2-Settings`
(`response_egress_framing.rs:320-358`). A malformed `Connection` value, such as
a token that is not a field name (for example `x example`), is not stripped.
It is a framing error and a 500 before commit
(`response_egress_framing.rs:320-335`). Since EdgeZero `e65394a1`, egress
ignores up to 32 empty `Connection` members, so an empty token or a trailing
comma is not malformed. Operator
`response_headers` are Trusted Server's only source of such a value. Settings
accept any valid `HeaderName` and `HeaderValue` (`settings.rs:3437-3449`), and
the response-header helper applies the value unchanged
(`response_privacy.rs:167-188`). So D10 rejects and skips every operator
hop-by-hop entry.

Skipping is not wire-neutral for `Connection`. A valid `Connection` value names
fields, and egress removes every field it names from the response, whatever set
that field (`response_egress_framing.rs:320-333`). Skipping an operator
`Connection` entry also drops that removal. A field it names, such as an
operator `x-example`, then reaches the client, where H would have removed it.
The ban accepts that loss of nomination semantics instead of re-implementing
it. `ts config validate` and `push` reject every such config, so only a live
config that predates the cut can carry one, and the pre-deploy check (Before
deploy) finds it. Test 16 asserts the retained field. Skipping the other
hop-by-hop names changes nothing on the wire, because egress strips them
anyway.

**Write deadline (D7).** `Hooks::configure` installs
`App::set_response_egress_policy` (`app.rs:553-560`), whose policy receives the
egress start time, with a write deadline of 60 s after egress starts. The
default is 30 s from egress start
(`edgezero:crates/edgezero-core/src/response_egress.rs:22`, `:1231-1241`).
Without the policy, a streamed page ends at 30 s whatever its origin timeout.

**Head limits.** The defaults are 8192 target bytes, 100 header fields, and
64 KiB of header bytes (`ingress.rs:35-37`). An absolute URI counts its scheme
and host (`ingress.rs:845`). Adapters check them before admission, and the
router checks again after the pre-dispatch hook. Violations return 414 or 431.
Admission also rejects ambiguous request framing with 400: a duplicate or
malformed `Content-Length`, `Content-Length` with `Transfer-Encoding`, or a
`Transfer-Encoding` that is not chunked (`ingress.rs:887-942`). Fastly enforces
all of this now too. Trusted Server keeps the defaults, and test 11 checks that
realistic traffic fits. If it does not, `Hooks::configure` raises them with
`App::set_ingress_head_limits` (`app.rs:523`).

**Synthesized headers (D15).** `Hooks::configure` calls
`App::set_synthesized_response_headers` (`app.rs:570`) with `x-ts-version`
(`crates/trusted-server-core/src/constants.rs:36`), and on Fastly also the
service-version header that the finalize step sets. EdgeZero adds them only
where absent (`edgezero:crates/edgezero-core/src/response_headers.rs:51-62`),
to head-limit errors, framing rejections, admitted source errors, fallbacks,
and canonical 404 and 405 (`app.rs:221-252`, `:276-307`).

On Fastly, `prepare` replaces that set for each request (EZ-19.2). The
per-request set holds the finalize headers that do not depend on the response:
`x-ts-version`, `x-ts-fastly-version`, `x-ts-env` on staging, the geo headers or
`x-geo-info-available: false`, and the operator's `response_headers`
(`crates/trusted-server-adapter-fastly/src/middleware.rs:187-237`). Operator
entries whose names EZ-19.5 forbids are left out. This meets EZ-19 criterion 6.
`SynthesizedResponseHeaders::new` (`response_headers.rs:70`) rejects a set over
32 field values or 4096 bytes (`:6-9`), or one with a value outside visible
ASCII and interior tabs (`:13-14`, `:97-108`), such as a geo city with accents.
`prepare` leaves out each such geo or operator value before it builds the
set, as the geo headers already skip invalid values one by one
(`crates/trusted-server-core/src/geo.rs:51-59`). It logs that at debug, because
the routed response still carries the value and the set is built for every
request. An unportable operator value warns once instead, when the sandbox
loads its settings. Only a bounds failure then
falls back to the version headers alone, with the error category logged at
warn. Axum, Cloudflare, and Spin have no per-request hook and keep the
app-level set.

Admission refusals are not decorated (EZ-19.3), and assembly errors carry no set
(EZ-19.7, `app.rs:563-565`). Trusted Server installs no admission policy, so
refusals do not occur, but a startup failure now carries no `x-ts-version`.

**Error text.** EdgeZero renders `EdgeError::Internal` as
`internal server error` instead of `internal error: {source}`. 405 responses
carry a sorted `Allow` header, and overflow responses carry no `Retry-After`
(`edgezero:crates/edgezero-core/src/error.rs:452-461`, `:557-559`, `:822-845`).
No Trusted Server test matches the old text.

### 6. Pre-dispatch hook and request ingress

`TracePreDispatchHook::handle` (`crates/trusted-server-core/src/trace/dispatch.rs:117-127`)
takes `&mut PreDispatchRequest` (`router.rs:43`). Helpers that took
`&mut Request` or `&Request` take `&mut RequestParts`, `&RequestParts`, or the
hook request: `prepare_trusted_forwarder`
(`crates/trusted-server-core/src/forwarder.rs:63`), `trace/routes.rs:242`,
`action_response` (`trace/actions.rs:27`), `matches_trusted_origin`
(`trace/actions.rs:78`), and `MetadataSupplier` (`trace/dispatch.rs:90`) with
its four adapter suppliers.

- The hook can no longer replace the body. `mem::replace(request.body_mut())`
  (`trace/actions.rs:48-49`) becomes `request.body_bytes(0)` (section 4). It
  returns 413 for any byte and caches the body instead of consuming it.
- On canonical paths the hook runs after admission (`router.rs:579-599`).
  Refusals and aborts never reach it, a changed method or path is a 500, and
  the head is validated again after a hook that continues. The trace hook
  answers or continues and is not expected to change method or path. Test 9
  checks the trace routes through canonical dispatch.
- The `PreDispatchRequest` constructor is crate-private (`pre_dispatch.rs:93`).
  The 17 hook test calls drive the hook through
  `RouterService::builder().pre_dispatch_hook(..).build().oneshot(request)`.
  Tests that check a continue outcome add a route whose handler records the
  request, since `oneshot` hides the mutated request. `oneshot` runs the hook
  before route lookup (`router.rs:429-452`), unlike canonical paths, so the
  integration suite covers the canonical order on Viceroy, workerd, and Axum.
- `RequestIngress` and the fidelity types are unchanged. The H file is
  byte-identical to the `v0.0.11` one, and `edgezero_core::ingress` still
  exports them.

### 7. Mechanical API changes

| Change at H                                                                                      | Sites                                                  | Fix                                                                                                                                                                                                                                                                                                                                                        |
| ------------------------------------------------------------------------------------------------ | ------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Body` is an opaque struct (`body.rs:24`)                                                        | 46                                                     | Use `into_parts()`, `into_stream()`, `as_bytes()`, `is_stream()`. `BodyChunkSource::next_chunk` (`publisher.rs:218`, `:228`) converts once to a `BodyStream` or `Bytes` and holds that. Dropping a body that owns an outbound scope ends the scope (`body.rs:359-369`), so streamed outbound bodies stay alive until egress.                               |
| `Body::from_stream` takes `EdgeError` items (`body.rs:126`)                                      | 17                                                     | Publisher body streams pass the origin's `EdgeError` through, so origin failures keep their status class. Foreign `io::Error` streams, mostly in tests, use `from_external_stream` (`body.rs:103`), which maps to 500.                                                                                                                                     |
| `RequestContext::into_request` returns `Result` (`context.rs:579`)                               | 46, from 15 roots                                      | Propagate with `?`.                                                                                                                                                                                                                                                                                                                                        |
| `RequestContext::request` and `request_mut` removed                                              | 29                                                     | `parts()`, `headers()`, `extensions()` and their `_mut` forms. `enforce_basic_auth` (`crates/trusted-server-core/src/auth.rs:83`), `sanitize_trusted_client_ip_headers` (`http_util.rs:60`), and `normalize_spin_request` (`crates/trusted-server-adapter-spin/src/app.rs:296`) take `&mut RequestParts`. Adapter context lookups read `ctx.extensions()`. |
| `Hooks::routes` and `Hooks::configure` return `Result` (`app.rs:672`, `:733`)                    | 4, plus each `configure`                               | Return `Err(EdgeError)` on a settings failure, as below. Delete the Axum, Cloudflare, and Spin `startup_error_router`s (`crates/trusted-server-adapter-axum/src/app.rs:583`, `crates/trusted-server-adapter-cloudflare/src/app.rs:411`, `crates/trusted-server-adapter-spin/src/app.rs:509`). Fastly's goes with section 3.                                |
| `ConfigStore::get` returns `ConfigValue`, `get_bounded` required (`config_store.rs:409`, `:416`) | 4                                                      | Convert to `String` at the boundary (`crates/trusted-server-adapter-cloudflare/src/platform.rs:99`, `crates/trusted-server-adapter-spin/src/platform.rs:141`). The Spin test store implements `get_bounded`.                                                                                                                                               |
| `SpinConfigStore::open` replaced by `for_label`                                                  | 1 (`crates/trusted-server-adapter-spin/src/app.rs:91`) | Use `for_label`. Open failures surface at the first read, through the existing error branch.                                                                                                                                                                                                                                                               |
| `run_custom` and `serve_custom` callbacks return `Result`                                        | 2 (`main.rs:113`, `:138`)                              | Section 3.                                                                                                                                                                                                                                                                                                                                                 |
| `PreDispatchHook::handle` takes `PreDispatchRequest`                                             | 1 impl and 17 test calls                               | Section 6.                                                                                                                                                                                                                                                                                                                                                 |

**Startup failures.** Today the startup routers answer with the root error's
`status_code()` (`crates/trusted-server-adapter-axum/src/app.rs:584-585`,
`crates/trusted-server-adapter-cloudflare/src/app.rs:412-413`,
`crates/trusted-server-adapter-fastly/src/app.rs:1048-1049`), which is 500 for
settings errors, 503 for `KvStore`, 502 for upstream errors, and 400 for
`BadRequest` (`crates/trusted-server-core/src/error.rs:116-135`). Spin answers
503 for every failure (`crates/trusted-server-adapter-spin/src/app.rs:509-540`).
The cut keeps those statuses. A startup failure maps to the `EdgeError`
constructor for the root error's status: `internal` (500),
`service_unavailable` (503), `bad_gateway` (502), or `bad_request` (400)
(`edgezero:crates/edgezero-core/src/error.rs:225-596`). Spin always uses
`service_unavailable`. Trusted Server logs the report at error before
returning it.

- The body becomes EdgeZero's redacted JSON with no `x-ts-version`. Today it is
  Trusted Server's text, and the finalize step adds `x-ts-version`
  (`main.rs:523-538`).
- Spin's `/health` no longer answers 200 while startup fails
  (`crates/trusted-server-adapter-spin/src/app.rs:530-534`). The test at
  `:1531` goes with the router.
- The Axum dev server now takes an assembled `App`, so a settings failure stops
  it before it listens. Axum `main` returns `()` today
  (`crates/trusted-server-adapter-axum/src/main.rs:13`). It logs the error and
  exits non-zero.
- The Fastly, Cloudflare, and Spin smoke scripts keep their status and log
  checks, since they already read the diagnostic from the log
  (`scripts/smoke-common.sh:127-134`). Only `scripts/smoke-axum.sh:101-111`
  changes. It asserts a non-zero exit and the log line instead of an HTTP 500.

**Config and KV fallbacks on Cloudflare and Spin.** Trusted Server's Cloudflare
and Spin apps declare no `stores()`
(`crates/trusted-server-adapter-cloudflare/src/app.rs:445-461`,
`crates/trusted-server-adapter-spin/src/app.rs:549-565`). Only Fastly does
(`crates/trusted-server-adapter-fastly/src/app.rs:1376-1393`). At H those
adapters build a config registry only from declared metadata
(`edgezero:crates/edgezero-adapter-cloudflare/src/request.rs:770-775`,
`edgezero:crates/edgezero-adapter-spin/src/request.rs:568-572`), so
`ctx.config_store_default()` returns `None`. The `NoopConfigStore` and
`UnavailableKvStore` fallbacks
(`crates/trusted-server-adapter-cloudflare/src/platform.rs:636-644`,
`crates/trusted-server-adapter-spin/src/platform.rs:738-746`) are the live path
and stay.

### 8. Axum, Cloudflare, and Spin entry points

**Axum.**

- `AxumDevServer::with_config(app, config)` now takes an assembled `App`
  (`edgezero:crates/edgezero-adapter-axum/src/dev_server.rs:166-173`).
  `crates/trusted-server-adapter-axum/src/main.rs:41-42` passes
  `App::build::<TrustedServerApp>(AXUM_PLATFORM)` and exits non-zero on error
  (section 7). Every policy that `configure` installs now applies over real TCP
  (EZ-23).
- `AxumDevServerConfig` gains `ingress` (`dev_server.rs:58-64`), set from
  `AxumIngressConfig::from_env(&EnvConfig::from_env())` (D16). An invalid value
  fails startup. The variables are in [Configuration](#configuration).
- A request head over the raw-head byte limit or the field limit gets 431
  (`edgezero:crates/edgezero-adapter-axum/src/dev_server.rs:2069-2088`). Only a
  connection over `MAX_CONNECTIONS`, which is dropped on accept
  (`edgezero:crates/edgezero-adapter-axum/src/dev_server.rs:434-437`), or a
  head that misses its read timeout is closed. Response headers share the
  raw-head budget
  (`edgezero:crates/edgezero-adapter-axum/src/ingress_config.rs:89-102`).
  Accepted for the dev server.
- `EdgeZeroAxumService` is private (`edgezero:crates/edgezero-adapter-axum/src/lib.rs:29-30`).
  The Axum route tests (`crates/trusted-server-adapter-axum/tests/routes.rs`),
  the Axum parity leg (`crates/trusted-server-integration-tests/tests/parity.rs:21`,
  `:81`, `:100`), and the CLI proxy test
  (`crates/trusted-server-cli/tests/proxy_trusted_server.rs:84-92`) move to
  `AxumTestServer` (`edgezero:crates/edgezero-adapter-axum/src/test_utils.rs:57-72`).
  It needs the adapter's `test-utils` feature in those dev-dependencies. The
  tests build the app with
  `App::from_router::<TrustedServerApp>(routes_with_settings(..), AXUM_PLATFORM)`
  so they can inject test settings, listen on loopback port 0, and attach
  stores themselves, because the helper does not create them from metadata.
- The parity suite's Cloudflare and Spin helpers move to `AxumTestServer` too,
  with each app assembled under its own platform metadata (EZ-23.8) and the
  scripted client supplied through router state (EZ-23.7). Their production
  policies then apply, which EZ-23 criterion 13 needs (test 19). For that
  criterion's bodiless GET, the suite supplies EdgeZero's Axum client
  (`edgezero:crates/edgezero-adapter-axum/src/outbound.rs:55`) instead and
  records what a loopback origin receives. That client honors `HTTP_PROXY`, so
  the test runs with the proxy variables unset. The Cloudflare and Spin route tests
  in `tests/routes.rs` stay on `oneshot` (2.1).

**Cloudflare.**

- `run_app` keeps its signature
  (`edgezero:crates/edgezero-adapter-cloudflare/src/lib.rs:157-181`). It builds
  the app per request and returns assembly failures as an `Ok` response.
- An admission abort returns `Err` (`request.rs:299-310`). Trusted Server
  installs no abort policy, so the existing 500 fallback
  (`crates/trusted-server-adapter-cloudflare/src/lib.rs:36-39`) stays as a
  guard.
- Trusted Server's client is deleted with its encoding strip
  (`crates/trusted-server-adapter-cloudflare/src/platform.rs:400-415`), manual
  redirect (`:267`), and 10 MiB cap (`:187`). H's Cloudflare client honors the
  host override (`edgezero:crates/edgezero-adapter-cloudflare/src/outbound.rs:336-340`).
- Settings load from the `TRUSTED_SERVER_CONFIG` variable
  (`crates/trusted-server-adapter-cloudflare/src/lib.rs:29-31`), not the config
  store. Cloudflare declares no stores (section 7), so `services.config_store()`
  is `NoopConfigStore` at `v0.0.11` and at H, and the `block_on(get)` bridge at
  `platform.rs:99` is unused. Request signing on Cloudflare reads the no-op
  store and fails, as today. The cut leaves both unchanged.

**Spin.**

- `run_app` (`edgezero:crates/edgezero-adapter-spin/src/lib.rs:173`) returns
  `anyhow::Result<SpinResponse>`. `SpinResponse` is a streamed response that an
  adapter task writes (`lib.rs:83-85`). The entry point declares
  `anyhow::Result<edgezero_adapter_spin::SpinResponse>`, as H's template does.
  The `anyhow` exception in `AGENTS.md` still applies.
- `SpinPlatformHttpClient` and `decompress_body`
  (`crates/trusted-server-adapter-spin/src/platform.rs:372-690`) are deleted.
  That also removes the deprecated `spin_sdk::wasip3::spawn` call (`:579`).
- The fork keeps the `spin-sdk` APIs Trusted Server calls (`variables::get`,
  `time::sleep`, and `http_service`, at `platform.rs:701`, `:816`, and
  `lib.rs`). The probe's `check-spin` reported no error at those uses.
- Spin ignores `origin_host_header_override` today and sends the URI authority
  (`platform.rs:449-456`). Whether WASI hosts forbid an explicit `Host` is
  unverified (EZ-13). H's Spin client rejects any override, so Spin's settings
  load ignores the setting with a warning (D9). Production Spin builds drop
  that warning until EdgeZero installs a log backend (section 9).
- At H, Spin's client could send every buffered request body, bidder JSON
  included, chunked with no `Content-Length`. Another reviewer raised this as
  an open question on PR 275. At `14a6ca8b` the client sets `Content-Length`
  on a buffered body for every method but GET and HEAD, and frames an empty
  body only for POST, PUT, and PATCH
  (`edgezero:crates/edgezero-adapter-spin/src/outbound.rs:900-915`). Test 5's Spin
  leg records the framing a loopback bidder receives. Task 6 observed
  `Content-Length: 428` with no `Transfer-Encoding`.
- Spin config reads go through `block_on(handle.get(..))` with
  `SpinConfigStore::for_label` (`crates/trusted-server-adapter-spin/src/app.rs:86-108`).
  Whether the fork's key-value future completes there is unverified. The Spin
  cases in CI prove it (D14).
- Trusted Server builds `wasm32-wasip1`, and EdgeZero validates `wasm32-wasip2`
  (`edgezero:crates/edgezero-adapter-spin/src/cli.rs:129`). Only a running
  component proves it instantiates (D14).

**Worker deadline job.** `.github/workflows/worker-auction-deadlines.yml` runs
on every pull request and is `AGENTS.md` gate 15. It is not a required status
check. It builds the Cloudflare and Spin `deadline-runtime` examples
(`crates/trusted-server-adapter-cloudflare/Cargo.toml:52-56`,
`crates/trusted-server-adapter-spin/Cargo.toml:49-53`), which include each
adapter's production `platform.rs` and call `PlatformHttpClient`
(`crates/trusted-server-adapter-cloudflare/tests/runtime/deadline.rs:1-11`).
`scripts/test-worker-auction-deadlines.mjs` drives them against delayed headers,
trickling bodies, and slow publisher work. The cut ports both examples to the
adapter's EdgeZero `HttpClient`, through `start_batch_until` and `collect`,
against the same delayed origins, asserting the same outcomes. Every case models
a bid, so the examples call no single `send`. The runtime integration tests in
`crates/trusted-server-integration-tests/tests/integration.rs`, which CI runs
with `--include-ignored` and the Spin filter, cover the real client's `send` on
Cloudflare and Spin. The script keeps its cases. Gate 15's text stays, because
its build and run commands do not change.

**Fastly deadline harness.** The required `cargo test` job runs
`crates/trusted-server-adapter-fastly/tests/deadline-origin.mjs`
(`.github/workflows/test.yml:58-59`, `AGENTS.md` gate 14). It runs the ignored
tests `platform::deadline::tests::network_*`
(`crates/trusted-server-adapter-fastly/src/platform/deadline.rs:795-946`) under
Viceroy against a slow local origin, and asserts an exact pass count
(`deadline-origin.mjs:148-151`). Those tests reach the origin through a static
`deadline_origin` backend, which is the only place the harness writes the
origin's random port (`deadline-origin.mjs:103-112`), and the file is deleted.
H derives backends from the request URI and cannot target a named static
backend (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:977-1023`).

- The cut moves the cases to a new test module in the Fastly adapter binary.
  Each case builds `HttpClient::with_client(FastlyOutboundClient::new())` and
  calls `start_batch_until` with `http://127.0.0.1:<port>` URIs. The module path
  must not start with `platform::deadline`, because the dependents inventory
  searches that string. A module at the crate root, such as
  `deadline_network`, gives the filter `deadline_network::tests::network_`.
- The harness writes the port into an inline Viceroy config store named
  `deadline_fixture`, in place of the static backend, and the tests read it
  with `fastly::ConfigStore::open`. Viceroy 0.21.0's `run` and `serve` both
  build their context through `create_execution_context`, which installs the
  config's dictionaries, and `config_stores` is a serde alias for
  `dictionaries` (Viceroy `cli/src/execute_ctx.rs:96-113`, `src/config.rs:270`).
  The current harness already relies on the same path for
  `local_server.backends`.
- Three cases keep their outcomes: stalled headers and a trickling body end at
  the cutoff, and a HEAD skips its declared length.
- The fast-sibling case drops its missing-cache-bypass slot, because H accepts
  any cache policy in a batch (`outbound.rs:1082-1091`).
- The expired-snapshot case changes. After the sleep it calls `batch.next()`.
  That call expires both slots at the cutoff and returns `Finished(Cutoff)`
  without a host wait, so H drops the fast reply that landed during the sleep
  (`outbound.rs:921-926`,
  `edgezero:crates/edgezero-core/src/outbound/batch.rs:645-650`). The case
  asserts that drop. At the pin under Viceroy 0.21.0, a `batch.check()` at the
  same point also returns `Finished(Cutoff)`, so the case asserts only
  `batch.next()`. A new case asserts that a reply observed
  during an EdgeZero wait before the cutoff is kept.
- An EdgeZero wait drives a batch with no request lifecycle. With no canonical
  owner, the batch gets a caller-owned scope
  (`edgezero:crates/edgezero-core/src/outbound/scope.rs:314-338`), and
  `batch.next()` reaches `fastly_async_io::select` through the native runtime
  (`edgezero:crates/edgezero-adapter-fastly/src/outbound/progress.rs:315-383`).
  Every ported case depends on that path.
- The harness filter changes to the new module path, and the count assertion
  matches the number of ported cases. Gate 14's text stays, because it names
  only the harness path.

### 9. Logging

Trusted Server keeps `owns_logging` false. Its Fastly and Axum loggers already
admit target `edgezero::boot` at warn
(`edgezero:crates/edgezero-core/src/logging.rs:10-12`), where assembly failures
log once (`app.rs:113-146`).

On Cloudflare and Spin, EdgeZero's `init_logger` installs no backend
(`edgezero:crates/edgezero-adapter-cloudflare/src/lib.rs:86`, `:94`,
`edgezero:crates/edgezero-adapter-spin/src/lib.rs:139`), and Trusted Server
installs one only in its `test-utils` fixture builds. Production Cloudflare and
Spin builds therefore drop every log record, as they do today. The startup
report and every warning this spec names on those adapters, such as the Spin
`origin_host_header_override` warning, are not observable until EdgeZero's
`init_logger` installs a backend. Installing one is EdgeZero's job, so Trusted
Server adds no logger of its own and asks EdgeZero for one on PR 275 (Open
EdgeZero defects at H). `scripts/smoke-spin.sh`, which reads the startup
diagnostic from the component's stderr, fails until then.

Each outbound failure logs one line: the site
that consumes the helper's report (2.11) logs it with `{:?}`, naming the site
or partner, and that rendering prints the budget source and
`provider_failure_detail()` (`error.rs:490`). The helper itself does not log,
so a failure never logs its detail twice. The detail holds closed enums and
numeric codes, never provider text or URLs (EZ-16).

### 10. Documentation

- Guide pages that name the EdgeZero version, or describe outbound behavior,
  redirects, timeouts, header rules, or inbound body limits.
- The rejected settings (D9, D10, D18) and the doc comment on
  `certificate_check`.
- The Axum ingress variables (D16) and the Axum startup-failure exit.
- CLI manifest discovery. `build`, `serve`, and `deploy` resolve
  `EDGEZERO_MANIFEST`, then the nearest ancestor `edgezero.toml`, then a
  workspace scan (`edgezero:crates/edgezero-cli/src/manifest_source.rs:117-126`).
- `docs/guide/architecture.md`, which names the deleted platform types.
- `docs/guide/auction-orchestration.md`, which describes
  `ProviderRequestOutcome`, `PlatformResponse`, and fan-out through Fastly
  `select()` (`:11`, `:188`, `:334`).
- The `runtime-services` snippet in `docs/guide/integration-guide.md`
  (`:31-186`). `crates/trusted-server-integration-tests/tests/documentation_snippets.rs:5-10`
  compiles it, and the integration-tests job runs that test. The snippet
  implements `PlatformBackend`, the synchronous store writes, and
  `.http_client(..)`, so it changes in the same commits as those APIs: step 8
  makes its store writes async, and step 9 removes its backend and HTTP client
  and builds `RuntimeServices` with `outbound_client` and `clock`. The guide's
  prose after the snippet (`:195-203`) changes in step 11. The test writes the
  snippet into a fresh crate whose manifest lists only `error-stack`,
  `futures`, and core (`documentation_snippets.rs:60-63`). So the test adds
  `async-trait` to that manifest at the workspace requirement, read the way it
  reads `error-stack` (`:50-59`), and `trusted_server_core::platform`
  re-exports `HttpClient` and `MonotonicClock`, as it re-exports the KV types
  (`platform/mod.rs:54`). The snippet's `runtime_services` takes the client and
  clock as arguments, as adapters pass `ctx.http_client()` and
  `ctx.monotonic_clock()`.
- `crates/trusted-server-core/src/auction/README.md` and `scripts/README.md`,
  which describe the deleted layer and the changed harnesses.
- A `CHANGELOG.md` entry.

## Adapter impact

| Adapter    | Compile changes                                                                                         | Runtime changes                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| ---------- | ------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Fastly     | 38 sites, the lifecycle rewrite, deletion of the client, backends, and delivery code                    | [Fastly differences](#fastly-differences)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| Axum       | 10 sites, the dev server `App` and `ingress` field, route and parity tests on `AxumTestServer`          | EdgeZero client in place of Trusted Server's. Ingress caps. Framing, head limits, and `configure` policies apply. Inbound 30 s read deadline. Publisher origin input is capped at `publisher.max_buffered_body_bytes` for every classification, its first input cap, and raw responses are collected before egress (2.5). Startup failure exits the process                                                                                                                                                                                                                                                       |
| Cloudflare | 22 sites, deletion of the client, two compatibility flags, the deadline example                         | Multi-provider auctions load and fan out. EdgeZero client in place of the encoding strip (`Decode`, 2.5) and manual redirect. Publisher origin input still capped at 10 MiB for every classification, and raw responses collected before egress (2.5). Strict framing. Head limits. Inbound 30 s read deadline. Startup failure answers with the assembly error                                                                                                                                                                                                                                                   |
| Spin       | 34 sites, the `spin-sdk` fork, `SpinResponse`, deletion of the client and decoder, the deadline example | Multi-provider auctions load and fan out. Publisher origin input and HTML expansion still capped at 8 MiB for every classification, and raw responses collected before egress (2.5). Publisher responses keep the origin `Content-Encoding` (`Original`), where the old client decoded gzip and br (2.5). A publisher upload reaches the origin chunked, with no `Content-Length`, until EdgeZero keeps the declared length (2.5). Streamed egress. Head limits. Inbound 30 s read deadline. Startup failure answers 503 with the assembly error. Config open failure at first read. Outbound-host manifest check |

Core carries 63 sites plus the outbound migration. The CLI library compiles
against H unchanged, and its tests change only for the Axum harness.

### Fastly differences

Intended changes to Fastly behavior. The response diff's expected set lists
each one a request exercises.

Inbound:

1. A forwarded GET or HEAD that carries body bytes returns 400 (EZ-1).
2. A body read or forwarded more than 30 s after the request arrived returns 408.
3. A publisher upload over 16 MiB returns 413. The origin sees the head and up
   to 16 MiB of an aborted upload (2.5).
4. A request target over 8192 bytes returns 414. More than 100 header fields or
   64 KiB of headers returns 431. Ambiguous or malformed request framing
   returns 400.

Responses:

5. Framework errors render as EdgeZero JSON. Internal text is redacted. 405
   carries `Allow`. Detached responses other than startup failures carry the
   version, environment, geo, and operator headers through the synthesized set,
   or only the version headers when that set fails its bounds. They carry no EC
   cookies, cookie cache privacy, or sandbox counters (section 5).
6. Hop-by-hop fields are stripped, and `Trailer` is never sent.
7. Bodiless responses (HEAD, 204, 205, 304) skip the SDK's bulk framing
   (`edgezero:crates/edgezero-adapter-fastly/src/response.rs:399-405`), so their
   `Content-Length` and `Transfer-Encoding` can differ from today's (D20).
8. A startup failure answers with EdgeZero's redacted JSON and carries no
   `x-ts-version`, `x-ts-fastly-version`, or finalize headers (EZ-19.7). Today
   it carries `x-ts-version`. Its status is unchanged.
9. A response body must finish within 60 s after egress starts.

Outbound:

10. A streamed origin that stalls fails at the 60 s total instead of the 15 s
    first-byte or between-bytes timer, and a healthy streamed transfer longer
    than 60 s is cut. There is no idle bound until EZ-14.
11. Buffered calls have a 15 s total instead of 15 s idle timers. Redirect
    chains have a 60 s deadline.
12. Backend names are `ez_b31v1_*`, and provider timers round up to a power of
    two of milliseconds. A provider `NameInUse` fails the request with the
    site's 502. EdgeZero reports it as an internal error
    (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:1132-1134`,
    reached through `finish_backend_creation` at `:1261-1291`). Today Trusted
    Server treats `NameInUse` as reuse
    (`crates/trusted-server-adapter-fastly/src/backend.rs:262-265`). Fastly
    documents that re-registering a name with identical properties succeeds,
    and that conflicting properties fail. EdgeZero hashes the whole backend
    configuration into the name
    (`edgezero:docs/guide/adapters/fastly.md:733-737`), so a `NameInUse` means
    a real collision. The reviewer who asked about this accepted that on the
    PR 275 thread. The hosted same-name controls stay in EdgeZero's Fastly
    qualification.
13. TLS is always verified. `certificate_check = false` is ignored (D9).
14. A request or response header field whose value is not UTF-8 is dropped,
    with a warning that names the header. That includes `Cookie` on the
    publisher forward and origin `Set-Cookie`. Today they pass as raw bytes.
15. Upstream requests with no `Accept-Encoding` carry `identity`.
16. Hop-by-hop request fields, and fields that `Connection` names, no longer
    reach upstreams. A malformed request `Connection` returns 400 on the
    publisher forward. A malformed response `Connection` returns the site's 502.
17. The origin path is WHATWG-normalized. A request target with a fragment, a
    backslash, or userinfo returns 400 on the publisher forward.
18. Redirects follow the differences in section 2.6, including a 502 for a
    `Location` with userinfo.
19. An auction that is abandoned, or whose page response ends early, cancels its
    in-flight bidder and mediator requests on the guest side. Host cancellation
    is best effort, so a bidder may still see the whole request. Today they run
    to completion.
20. Bidder requests leave together after all are built, not one at a time.
21. Telemetry goes out after delivery. Pull sync and telemetry run only after a
    committed application body.
22. A bid that completes while no EdgeZero poll services the provider, and is
    first observed after the cutoff, is dropped. At H a `batch.check()` does not
    observe it (2.4). Today a snapshot after the deadline keeps it (#1433).
23. Pull sync has a 15 s cutoff per group.
24. Origin readthrough is unavailable (D9).

Responses, continued:

25. A streamed relay whose origin declares `Content-Length` keeps it instead of
    chunked framing (test 7).

Outbound, continued:

26. Bidder responses have no aggregate buffer cap. Today Trusted Server's
    Fastly collector caps the partial bid bodies it retains at 10 MiB together
    (`MAX_TOTAL_BUFFERED_BODY_BYTES`). After the cut each bidder response keeps
    today's cap for its profile, 2 MiB for Prebid Server and APS and 1 MiB for
    Standard, across up to 160 provider backends. The maintainer accepted the
    per-bidder caps as enough, so the cut adds no aggregate cap and asks
    EdgeZero for none. Each cap now applies in EdgeZero's buffered collection,
    so a body over it is classed `transport`. Today the platform collects up
    to 10 MiB, and a body over the profile's cap is classed `parse_response`,
    or `http_status` for an error status.

Unchanged, and checked by the response diff: routes, other statuses, other
headers, auction semantics, consent handling, EC cookies, upstream failures as
502 (D17), and `x-compress-hint` on tsjs bundles (`publisher.rs:597`).

## Configuration

**No new configuration keys.** Validation changes:

| Setting                                                                                                                                                                                                                            | Today                                                                                                                                                                                      | After the cut                                                                                                                                                                                                                                |
| ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `proxy.certificate_check = false`                                                                                                                                                                                                  | Disables origin TLS checks for the publisher, asset, redirect, and pull-sync backends (`crates/trusted-server-adapter-fastly/src/backend.rs:244-250`). Default `true` (`settings.rs:1937`) | `ts config validate` and `push` reject it. Runtime ignores it with a warning, which replaces `warn_if_certificate_check_disabled` (`crates/trusted-server-adapter-fastly/src/app.rs:241`). Only origins with invalid certificates then fail. |
| `creative_opportunities.origin_readthrough_enabled = true`                                                                                                                                                                         | Shared-cache origin fetch with a surrogate key. Off by default (`crates/trusted-server-core/src/creative_opportunities.rs:424-425`)                                                        | Validate and push reject it. Runtime treats it as `false` with a warning.                                                                                                                                                                    |
| `publisher.origin_host_header_override` on Spin                                                                                                                                                                                    | Ignored. Spin sends the URI authority (`crates/trusted-server-adapter-spin/src/platform.rs:449-456`)                                                                                       | Still ignored, now with a warning at settings load. Fastly, Axum, and Cloudflare send it through `host_authority_override`.                                                                                                                  |
| `publisher.origin_url` with userinfo                                                                                                                                                                                               | Accepted                                                                                                                                                                                   | Validate and push reject it. At runtime H rejects the request (2.11), so publisher requests answer 400.                                                                                                                                      |
| `response_headers` naming `trailer`, `content-length`, or a hop-by-hop field (`connection`, `keep-alive`, `te`, `transfer-encoding`, `upgrade`, `proxy-connection`, `proxy-authenticate`, `proxy-authorization`, `http2-settings`) | Applied                                                                                                                                                                                    | Validate and push reject the entry. Runtime skips it with a warning. A malformed `Connection` value would otherwise fail H's egress framing (section 5).                                                                                     |
| Multi-provider `auction.providers` on Cloudflare or Spin                                                                                                                                                                           | Startup fails (`crates/trusted-server-core/src/auction/plan.rs:417-466`)                                                                                                                   | Loads and fans out (2.4).                                                                                                                                                                                                                    |
| Provider endpoint `http://127.0.0.1:<port>` or `http://[::1]:<port>`                                                                                                                                                               | Rejected. Endpoints must be HTTPS (`plan.rs:565-584`)                                                                                                                                      | Accepted (D18). Other `http` endpoints are still rejected.                                                                                                                                                                                   |

Validation rejects at deploy time and degrades at runtime. A live config that
predates the cut keeps serving, and the pre-deploy check finds every entry to
fix.

The Spin row deviates from EZ-13's interim, which asked for rejection at
validation on Spin builds. `ts config validate` takes no adapter
(`edgezero:crates/edgezero-cli/src/args.rs:609-626`), so it cannot reject a
Spin-only setting. Spin's own settings load could reject it, but that would fail
startup for configs that serve today, which contradicts the rule above. The
requirements doc keeps its text. This spec records the deviation. EZ-13 is not
confirmed present (D19), so its criteria stay out of the gate, and its
Trusted Server criterion 3, which expects validation to fail, is not run.

Image Optimizer profiles need no change. Settings load already rejects
everything H's types cannot express (section 2.7).

**Constants, not configuration (D7).** The connect timeout is 1 s. Per-call
timeouts are the site's configured timeout where one exists, 2 s for Tinybird,
and otherwise 60 s streamed and 15 s buffered. Redirect chains get a 60 s
deadline. The response write deadline is 60 s after egress starts, and the
publisher upload cap is 16 MiB. Existing endpoint caps are unchanged.

**Axum ingress variables (D16).** Values must be positive integers in range,
and an invalid value fails startup
(`edgezero:crates/edgezero-adapter-axum/src/ingress_config.rs:28-58`,
`:185-199`).

| Variable                                           | Default | Range          |
| -------------------------------------------------- | ------- | -------------- |
| `EDGEZERO__ADAPTER__INGRESS__MAX_CONNECTIONS`      | 256     | 1 to 4096      |
| `EDGEZERO__ADAPTER__INGRESS__MAX_RAW_HEAD_BYTES`   | 65536   | 8 KiB to 1 MiB |
| `EDGEZERO__ADAPTER__INGRESS__MAX_HEADER_COUNT`     | 100     | 1 to 1024      |
| `EDGEZERO__ADAPTER__INGRESS__HEAD_READ_TIMEOUT_MS` | 10000   | 1 ms to 120 s  |

**Cloudflare publisher origin decoding.** No setting changes. The Cloudflare
publisher origin decodes origin bodies (D8 deviation, 2.5), as Trusted Server's
Cloudflare client did, so a Cloudflare response carries the origin's
`Content-Encoding` only on 206, HEAD, and 304.

**Unchanged for operators.** The app-config envelope, the canonical hash, the
Fastly chunk pointer format (`fastly_config_chunks` version 1,
`edgezero:crates/edgezero-adapter-fastly/src/chunked_config.rs:11-12`), store
links, secrets, and deploy flags are unchanged. No re-push is needed.

## Consent and privacy

- Bidder requests carry the same consent and attested client-IP headers as
  today. Test 3 asserts the headers on every bidder request.
- A header field whose value is not UTF-8 is dropped whole on the publisher
  forward and from origin responses. One raw-byte cookie in a combined `Cookie`
  header therefore drops every cookie to the origin, consent cookies included,
  and a raw-byte `Set-Cookie` no longer reaches the browser. The warning names
  the header, never its value.
- Detached responses fail closed. Head-limit and framing rejections happen
  before any handler, so they set no EC cookie and do no consent work.
- EC finalize and cookie writes run in `finalize` for every routed response, as
  they run after routing today. A framing fallback after `finalize` keeps its
  EC KV writes but drops its cookies, so the browser and the store can
  disagree for that one response. Framing fallbacks are unreachable from
  Trusted Server's sources after D10.
- Pull sync runs only after a committed application response. Its consent
  gating is unchanged.
- An abandoned auction cancels its bidder requests, so bidders may see a
  request cut off mid-response. Nothing extra is sent.
- Cross-origin redirects strip `Authorization` and `Cookie`, which is stricter
  than today.
- The proxy marker is off, so browsers see no new header.
- TLS is always verified, which is stricter than a config with
  `certificate_check = false`.
- EdgeZero logs redirect hops at debug without the query. Provider failure
  details hold no provider text, URLs, or header values. Trusted Server never
  logs body bytes.
- Telemetry rows have the same content, are sent later, and measure bidder time
  from the batch start to the recorded arrival, not to collection (2.4).

## Failure handling

| Failure                                                                              | Behavior                                                                                                                               |
| ------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------- |
| Inbound body over the endpoint cap, declared or streamed                             | 413 `RequestTooLarge`, logged at warn                                                                                                  |
| Inbound read after the 30 s read deadline                                            | 408 `RequestTimeout`, logged at warn                                                                                                   |
| Ambiguous or malformed request framing                                               | 400 from EdgeZero's ingress, before any handler runs                                                                                   |
| Any other inbound read failure mid-body, client-caused or host-side                  | 502 `Proxy`, logged at error. Every adapter maps the source error to `EdgeError::internal`                                             |
| Forwarded GET or HEAD with body bytes, or an invalid forwarded URI                   | 400 `BadRequest` (2.11)                                                                                                                |
| Publisher upload over 16 MiB                                                         | 413 `RequestTooLarge` (2.11)                                                                                                           |
| Origin, integration, or connect timeout                                              | The site's 502, as today. The budget source is attached to the report                                                                  |
| Bidder still pending at the cutoff                                                   | Provider timeout in the auction result                                                                                                 |
| Auction abandoned or page response ends early                                        | In-flight bidder and mediator requests are cancelled                                                                                   |
| Redirect policy rejection                                                            | Today's `Forbidden`, `AllowlistViolation`, or `Proxy` error                                                                            |
| Redirect hop failure, a bad `Location`, or a redirect chain past 60 s                | The site's 502                                                                                                                         |
| Image Optimizer without entitlement, or on Viceroy                                   | The asset proxy's 502, from `ImageOptimizerUnavailable`                                                                                |
| Fastly `NameInUse`, a backend name collision                                         | The site's 502                                                                                                                         |
| App assembly fails                                                                   | EdgeZero's assembly error response with today's status (503 on Spin). Not retained, so the next request retries. Axum exits at startup |
| Fastly delivery not committed                                                        | Native 500                                                                                                                             |
| Fastly delivery unknown or committed with a fallback body                            | Logged. No post-delivery work                                                                                                          |
| Fastly request head cannot be converted                                              | Native 500, as today                                                                                                                   |
| Framing violation                                                                    | EdgeZero 500 fallback. Unreachable from Trusted Server sources after D10                                                               |
| Telemetry or pull-sync send fails                                                    | Logged at warn. The client already has its response                                                                                    |
| `certificate_check = false`, readthrough, or forbidden `response_headers` at runtime | Warning, and the setting is ignored or skipped                                                                                         |
| `origin_host_header_override` on Spin                                                | Warning, and the setting is ignored, as today                                                                                          |
| Invalid `EDGEZERO__ADAPTER__INGRESS__*` on Axum                                      | Startup fails with the parse error                                                                                                     |
| Spin manifest outbound hosts disagree with `spin.toml`                               | `ts build`, `ts serve`, and `ts deploy --adapter spin` fail with "Spin outbound host drift"                                            |
| Spin component fails to instantiate                                                  | CI's Spin job fails. Blocks the cut (D14)                                                                                              |

## Compatibility and rollout

### Merge gate and pin

`main` merges the cut without waiting for EdgeZero's EZ-Q1 record, and
production deploys do not wait for qualification (D5, Q1). The maintainer
qualifies Trusted Server's hosted legs by hand
([EdgeZero qualification, Trusted Server's part](#edgezero-qualification-trusted-servers-part)).

The cut is stacked on PR 275, because its Axum changes need it (Q5). The cut
pins PR 275's head, repins whenever that head moves, and rebases onto `main`
regularly. At each repin it reruns the compile probe's tools to re-diff the
public API, and reruns the dependents inventory (Static). `main` pins the head
the cut was last rebased onto.

This accepts two risks:

- `main` and production can run a PR 275 head that EdgeZero has not
  qualified. PR 275 is open with change requests from two reviewers.
  EdgeZero's qualification rule says a defect forces a new candidate and a full
  rerun
  (`edgezero:docs/superpowers/plans/2026-10-18-trusted-server-outbound-05-qualification.md:416-417`).
  Without the gate, Trusted Server gets such fixes at its next repin.
- A force-push to the PR branch can orphan the pinned commit, and an orphaned
  commit breaks `--locked` builds once it is collected. The pin then moves to
  the new head.

Once PR 275 merges, Trusted Server moves from the PR 275 commit to an EdgeZero
release tag (Follow-ups).

Auctioneer pins EdgeZero `775366fd`, an earlier PR 275 commit. #1505 needs both
projects on one revision, so Auctioneer moves to Trusted Server's PR 275 commit
after the cut.

### Open EdgeZero defects at H

Review 5798502238 on PR 275 (2026-11-06, at H) checked every requirement
against EdgeZero's own copy of the contract. It found five blocking defects.
Trusted Server adds no workaround (D2). Re-reviews verified EdgeZero's fixes
for all five: review 5810546608 at `598f0a2a` for defects 1, 3, 4, and 5, and
review 5815197468 at `6f0cc74c` for defect 2. Because the cut follows PR 275's
head (D5), it picks those fixes up.

1. **Fastly batch setup (EZ-6.7, EZ-6.8).** Setup stops at the first slot that
   completes synchronously, such as a slot whose deadline passed during another
   slot's cold registration. Later bidders go out at the next EdgeZero poll, and
   a batch that is never polled never sends them (2.4).
2. **Fastly checks (EZ-6.11, cross-cutting requirement 5).** `check()` and
   `is_ready()` record no arrivals (2.4).
3. **Spin cutoff (EZ-6.14, EZ-6.16).** Spin can report the cutoff before it
   observes an expired earlier-deadline slot, and the whole batch then fails
   with an internal error. On Spin every bidder in that batch then fails,
   instead of one timing out (2.4).
4. **Qualification build (EZ-Q1.4).** EdgeZero has no single-request
   `run_custom` Fastly build for the hosted criteria, and no frozen-record
   export. Step 4 of the run below cannot start without them.
5. **Inventory (EZ-Q1.6, EZ-Q1.8).** The inventory cannot bring EZ-16 to EZ-22
   into the gate. Their 57 EdgeZero-owned local legs are planned and name no
   test, and 30 of them use a command the harness rejects.

Three of its should-fix findings change what Trusted Server sees. Fastly batch
slots and single sends take different SDK cache paths (EZ-2.9). EdgeZero's own
readers drain the body before a declared-length 413 (EZ-10.4, section 4). The
Cloudflare inbound read deadline can fire up to 1 ms early.

Other reviewers' open threads also reach Trusted Server:

- At H, Spin could send buffered request bodies chunked with no
  `Content-Length`. The pin sets the length (section 8).
- Every non-batch Fastly send waits for its response head in 1 ms polls, which
  costs guest CPU while an origin is slow.
- An upstream response with both `Transfer-Encoding` and `Content-Length` keeps
  the stale length.
- On Spin, request headers that wasmtime forbids, such as `HTTP2-Settings`, fail
  a forwarded request with 400.
- On Axum and Spin, a client-caused inbound body error, such as malformed
  chunked framing or an aborted upload, is an internal error
  (`edgezero:crates/edgezero-adapter-axum/src/request.rs:185`,
  `edgezero:crates/edgezero-adapter-spin/src/request.rs:227`). Trusted
  Server's body helper then answers 502 and logs at error. At the pin, Fastly
  and Cloudflare map such an error the same way, so Failure handling lists 502
  for every adapter.

Trusted Server's own asks on PR 275 are in
[one comment](https://github.com/IABTechLab/edgezero/pull/275#issuecomment-6863865880),
checked against head `1467e28a`. Trusted Server adds no workaround for them:

- A streamed Spin upload goes out chunked and loses its declared
  `Content-Length` (2.5).
- Cloudflare and Spin install no `log` backend, so their warnings and startup
  report are dropped (section 9).

The earlier question on Fastly `NameInUse` is answered (Fastly differences item
12).

### EdgeZero qualification, Trusted Server's part

At H no qualification record exists. EdgeZero's inventory lists 261 required
results, all missing (217 local, 44 external). No release leg is accepted, and
the approvals G4-FASTLY-POLICY, G4-OPTIMIZER-HORIZON, G4-APPLICABILITY, and
G4-MEASUREMENTS are pending
(`edgezero:docs/audits/2026-10-18-outbound-api-approval.md:30-41`). Since
`6f0cc74c`, its Strict Suppression And SDK Deviation Record (lines 795 to 809
at `5ea8d93c`) records the requester's choice to keep strict suppression,
without waiving EZ-18.4. D20 withdraws EZ-18.4's bodiless part, and EdgeZero
amended the contract to match in `dcbf622`
([PR 275 thread r4504573926](https://github.com/IABTechLab/edgezero/pull/275#discussion_r4504573926)):
EZ-18.4's SDK parity now covers only responses that can carry content, and
HEAD, 204, 205, and 304 follow EZ-18.6 strict suppression.
Trusted Server owns 24 of the 44 external legs: 15 local consumer legs, 4 hosted
legs on its own build, and the 5 consumer reviews. It runs the other 20 on its
staging service. The inventory assigns all 20 to EdgeZero's qualification
build, EZ-2 criteria 7 to 10 and 13a included, while the requirements doc and
the table below give those five to Trusted Server's build. EdgeZero reconciles
the two before the hosted phase.

These counts predate D19. Its confirmed follow-ups add 11 Trusted Server legs,
the EZ-10 to EZ-23 rows below. They also bring in five hosted legs that the
inventory gives EdgeZero: EZ-16 criterion 3, and EZ-18 criteria 3 and 3b on
HTTP/1.1 and HTTP/2, all on Trusted Server staging. EZ-Q1.4 does not list them
for EdgeZero's qualification build, so they fall to Trusted Server's build, and
EdgeZero reconciles them with EZ-2's five.

Trusted Server's inputs to the hosted legs:

- The maintainer is the operator and runs Trusted Server's hosted legs by hand
  for now (Q3). For each run, the maintainer chooses the staging service, its
  Image Optimizer entitlement, and the thresholds and sample counts for EZ-2's
  M1 to M7, and the run's record states them.
- HTTP/3 is qualified per adapter, not through Trusted Server's staging
  service (Q4). EdgeZero amended EZ-1 criterion 2 to match in `dcbf622`
  ([PR 275 thread r4504554519](https://github.com/IABTechLab/edgezero/pull/275#discussion_r4504554519)).
  Each adapter now has its own HTTP/3 leg that needs a disposition, and none
  is conditional on `hosted HTTP3 enabled`.
- The baseline commit for EZ-2 (D13). EZ-2's text and the runbook field
  (`edgezero:crates/edgezero-test-harness/acceptance/ts-outbound-qualification-runbook.md:69`)
  change to match.

The run:

1. At each repin, the cut branch runs Trusted Server's local legs.
2. The maintainer deploys Trusted Server's build of the pinned head to staging
   and runs its hosted legs by hand.
3. When EdgeZero's qualification build exists for the pinned head, the
   maintainer deploys it to the same staging service for EdgeZero's hosted
   legs. At H that build does not exist (open defect 4). Review 5810546608
   verified it at `598f0a2a`.
4. Trusted Server records its consumer review. EZ-5.9 and EZ-5.10 are not
   needed, because relays and the S3 preflight use `Original`. EZ-11 and EZ-14
   are not needed for the cut, because D9 and D7 cover them. Trusted Server
   confirms the follow-ups in D19 as present, so their criteria join the gate
   (EZ-Q1.6), with the Trusted Server legs in the table below. Reuse mode is
   not adopted in production. EZ-22 criterion 9 still joins the gate, because
   test 9 proves it without a feature gate: its reuse leg runs one `Sandbox`
   through a failed and then a successful assembly on every `cargo test-fastly`.
5. None of these steps gates the merge or production deploys (D5). A failed
   leg goes to EdgeZero on PR 275, and Trusted Server takes the fix at its next
   repin.

Trusted Server's working list of legs follows. EdgeZero's inventory
(`edgezero:crates/edgezero-test-harness/acceptance/outbound-cutover-cases.json`)
is authoritative. At H it lists the confirmed follow-ups' legs only as
`follow-up`, with no `present` decision that brings them into the gate (open
defect 5).

Stock Viceroy 0.21.0 cannot supply timing evidence (Runtime), so the Viceroy legs
Trusted Server adds to the record run on the repaired Viceroy that EdgeZero's
CI builds (`edgezero:scripts/ci_viceroy.sh`, `repair` mode,
`edgezero:.github/workflows/test.yml:387`, `:504`). The record names that build
as repaired (EZ-Q1.3), as EdgeZero's patch README requires
(`edgezero:patches/viceroy/README.md:3-5`). Trusted Server's review under EZ-Q1
criterion 3 accepts that build as EZ-Q1.3's Viceroy 0.21.0 only once EdgeZero
records the repaired-versus-stock disposition under G4-FASTLY-POLICY
(`edgezero:docs/audits/2026-10-18-outbound-api-approval.md:50-51`).

| Req   | Local, Trusted Server build                                                                                                                                         | Hosted, Trusted Server build                                                                                                                             | Hosted, EdgeZero build on Trusted Server staging |
| ----- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------ |
| EZ-1  | Criterion 13 on Viceroy, workerd, and Axum (test 1)                                                                                                                 | Criterion 5: an HTTP/2 GET with a DATA frame and no `Content-Length` gets 400, and the origin records nothing                                            | Criteria 2 and 6                                 |
| EZ-2  | Criterion 15 on Viceroy                                                                                                                                             | Criteria 7 to 10, 10b (EZ-16 is present), and 13a                                                                                                        | Criteria 11, 12, 13b, 14a, and 14b               |
| EZ-3  | Criterion 6, a 12-bidder auction on Viceroy (test 5)                                                                                                                | Criterion 7 (M7)                                                                                                                                         |                                                  |
| EZ-4  | Criterion 10, a connect-timeout readback unit test (test 3)                                                                                                         | Criterion 9                                                                                                                                              | Criterion 8 (M8)                                 |
| EZ-5  | Criterion 8 on Viceroy (test 7)                                                                                                                                     |                                                                                                                                                          |                                                  |
| EZ-6  | End to end on Viceroy, Axum, workerd, and Spin (test 6)                                                                                                             |                                                                                                                                                          | Scenarios 1, 2, 6, 7, 8, and 11                  |
| EZ-7  | Criterion 5 on Viceroy (test 3)                                                                                                                                     |                                                                                                                                                          |                                                  |
| EZ-8  | Criterion 9, a profile readback unit test (test 3)                                                                                                                  | The Trusted Server criterion on the IO-entitled service, with a final HEAD's metadata compared with the GET's                                            | Criterion 8 on the IO-entitled service           |
| EZ-9  | Criterion 20, unit and Axum (test 4)                                                                                                                                |                                                                                                                                                          |                                                  |
| EZ-10 | Criterion 9: today's read-cap 413 tests unchanged, an oversized chunked `POST /auction` (test 1), and a batch-sync stream past its declared length and cap (test 8) | Criterion 4: an HTTP/2 POST with no `content-length` over the cap gets 413                                                                               |                                                  |
| EZ-16 |                                                                                                                                                                     | Criterion 3, and criterion 8: a provider on a closed port logs one line with the provider and the refused cause, and the auction error body stays static |                                                  |
| EZ-17 | Criterion 7 on Viceroy (tests 9 and 13)                                                                                                                             |                                                                                                                                                          |                                                  |
| EZ-18 | Criterion 6 on Viceroy, through the response diff. Publisher HEAD and 304 follow D20                                                                                | Criteria 3 and 6 on hosted Fastly. HEAD, 204, and 304 follow D20                                                                                         |                                                  |
| EZ-19 | Criterion 6 on Viceroy (test 11)                                                                                                                                    |                                                                                                                                                          |                                                  |
| EZ-20 | Criterion 5 on Viceroy, through the response diff                                                                                                                   |                                                                                                                                                          |                                                  |
| EZ-22 | Criterion 9 on Viceroy (test 9), with today's 500, not 503, for the missing blob. Criterion 10 deferred (D19)                                                       |                                                                                                                                                          |                                                  |
| EZ-23 | Criterion 13 on the Axum test server (test 19)                                                                                                                      |                                                                                                                                                          |                                                  |
| EZ-Q1 | The five consumer reviews (step 5)                                                                                                                                  |                                                                                                                                                          |                                                  |

### Before deploy

- Run the new `ts config validate` against each environment's operator-owned
  `trusted-server.toml`. A failure names a setting from the Configuration
  table.
- Run the Viceroy response diff. On Fastly staging, run its read-only subset
  against production, with no admin writes or key rotation, and also normalize
  geo headers, `x-ts-fastly-version`, and `x-ts-env`. No read-only requests
  file exists. Build the subset from `scripts/fastly-response-diff.requests.json`
  by dropping `trace-enable-action`, the `POST /_ts/trace/enable` request, and
  `admin-eids`, the `GET /_ts/admin/eids` request with fixture credentials.
- Then deploy to production.

### Rollback

Revert the pull request and redeploy the previous build. Every config change the
new validation asks for removes or tightens a setting that the old build also
accepts, so rollback needs no config change. Two exceptions run the other way.
A multi-provider Cloudflare or Spin config accepted after the cut fails startup
again on the old build. A loopback `http` provider endpoint fails validation
again, which affects only test fixtures. `[capabilities.outbound]` and the
wrangler flags revert with the pull request.

### Operator-visible changes

- Fastly dynamic backend names become `ez_b31v1_*`. Dashboards keyed on backend
  names change.
- The template-cache build digest hashes `Cargo.lock` and `edgezero.toml`
  (`crates/trusted-server-core/build.rs:26`), so the first deploy misses shared
  template-cache entries once.
- One info log line per request carries the outbound observation counts
  (section 3).
- The Axum dev server has ingress caps and its own variables, and exits on a
  startup failure.

### In-flight work

A scan of the 118 open pull requests on 2026-11-05 found 50 with heavy overlap,
14 with moderate overlap, and 10 with light overlap. Heavy means the PR edits
files the cut deletes or rewrites, or changes the EdgeZero pin. Moderate means
it adds outbound sites or platform-type uses that need porting.

| PR                                                           | Conflict                                                                                                                         |
| ------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------- |
| #1490 November RC, #1424 AWS RC                              | Pin other EdgeZero revs and touch the platform HTTP layer. Rebuild on the cut                                                    |
| #1453 credential bindings                                    | Pins another rev. Touches Axum and Spin `platform.rs`, test support, `proxy.rs`, and `PlatformBackend`                           |
| #1261 request timing                                         | Pins another rev. Its hooks sit in the Fastly delivery code the cut replaces                                                     |
| #1487, #1488, #1489 server-side EIDs                         | New fan-out on `send_async` and `select`. Must use `start_batch_until`                                                           |
| #884, and #894 stacked on it                                 | #884 pins another rev and edits Fastly `main.rs`. #894 uses `PlatformHttpClient` and `proxy.rs`                                  |
| #1053 Prebid latency spec                                    | Pins a `stackpop/edgezero` branch. Cargo treats another git URL as another source, so it must move to `IABTechLab/edgezero`      |
| #1193 Fastly TTFB                                            | Edits Fastly post-send work and `platform/types.rs`                                                                              |
| #917 absolute creative URLs                                  | Textual conflict in the `proxy.rs` rebuild handler and Fastly `app.rs` tests                                                     |
| #839 EdgeZero Phase 1                                        | Superseded by the cut                                                                                                            |
| #1343 to #1448 stack (55 PRs)                                | 36 heavy, 9 moderate. The worst (#1448, #1390, #1348, #1399) touch the HTTP layer, backend naming, and the Image Optimizer types |
| #1310 ID5, #1325 and #1437 instream video, #1460 CWV loading | New outbound calls or uses of `StubHttpClient`, `PlatformBackendSpec`, and `BackendNamingPolicy`                                 |
| #1499 MaxMind geo (base `rc/edgezero-431-demo`)              | Imports `edgezero_adapter_axum::geo`, which H does not have                                                                      |
| #1516 S3 preflight cache bypass                              | Merged, ported in merge `a53f4d04c`. `Bypass` on the S3 HEAD preflight, its raw error GET, and a signed raw HEAD                 |

While the cut was open, #917, #1435, #1504, #1493, #1455, and #1516 merged to
`main`. The cut's Task 12 merges ported the first five, and merge `a53f4d04c`
ported #1516.

PR #1516 ran on the old API. Merge `a53f4d04c` ports it onto
`OutboundCachePolicy::Bypass` for the signed S3 HEAD preflight, its raw
error-body GET, and a signed raw HEAD without Image Optimizer options. The Image
Optimizer request keeps `PlatformDefault` and its options (section 2.7).
EdgeZero `c8fd1939` documents that split
(`edgezero@14a6ca8b:docs/guide/proxying.md:320-323`).

Issue #1484 asks for multi-provider fan-out on Cloudflare and Spin. The cut
implements it (2.4), and PR #1506 closes it with `Closes #1484`.

The cut lands first, and later pull requests rebase onto it. New uses of
`PlatformHttpClient`, `PlatformBackend`, and `StubHttpClient` are not frozen
while the cut is open (Q5). Pull requests that merge before the cut are ported
by the cut at its next rebase onto `main`, and the dependents inventory
(Static) finds their new uses.

## Verification

### Static

Every CI gate in `AGENTS.md`, and `--locked` builds after the lockfile review
in section 1 and the compile-probe re-diff at each repin. The Fastly and Axum adapters'
dev-dependencies enable EdgeZero's `test-utils` features
(`edgezero:crates/edgezero-adapter-fastly/Cargo.toml:23`,
`edgezero:crates/edgezero-adapter-axum/src/lib.rs:35-36`).

**Dependents inventory.** Each commit that deletes a symbol ports or deletes
every user of it in the same commit: code, tests, scripts, CI, and compiled doc
snippets alike. This command lists the remaining users. At `51730bdaf` it
prints 62 files, which this spec's sections and Expected files cover. After
step 9 it prints only `docs/guide/architecture.md`,
`docs/guide/auction-orchestration.md`, `docs/guide/integration-guide.md`, and
`crates/trusted-server-core/src/auction/README.md`, which step 11 rewrites.
After step 11 it prints nothing. Historical plan and archive documents are
excluded, because they stay as written records. The pattern avoids `\b`,
which POSIX extended regular expressions do not define. On macOS Git it matches
nothing. `request_body_bytes` must follow a non-identifier character, because
EdgeZero's `max_request_body_bytes` builder stays in use.

```sh
git grep -lE 'PlatformHttpClient|PlatformHttpRequest|PlatformResponse([^A-Za-z0-9_]|$)|PlatformPendingRequest|PlatformPendingResponse|PlatformSelectResult|PlatformBackend|BackendNamingPolicy|PlatformCacheIntent|PlatformImageOptimizer|StubHttpClient|NoopHttpClient|StubBackend|NoopBackend|UnavailableHttpClient|canonicalize_transport_timeout_ms|supports_concurrent_fanout|supports_streaming_responses|supports_pending_streaming_responses|proxy_with_redirects|ProxyRedirectPolicy|startup_error_router|validate_for_target|AuctionTargetId|platform::deadline|build_services_with_.*http_client|collect_body_bounded|(^|[^A-Za-z0-9_])request_body_bytes|send_edgezero_response|stream_asset_body|EdgeZeroAxumService|predict_name' \
  -- ':!docs/superpowers/specs' ':!docs/superpowers/plans' ':!docs/superpowers/archive' ':!CHANGELOG.md'
```

Two deleted uses share a name with code that stays, so scoped checks cover
them. The `AuctionContext` field shares its name with the EOF fixture's config
key. The Fastly adapter's `into_core_request` call shares its name with
EdgeZero's public Axum function, which the parity suite's transport-binding
test keeps using. Each prints nothing after step 9:

```sh
git grep -n transport_timeout_ms -- crates/trusted-server-core/src/auction \
  crates/trusted-server-core/src/integrations ':!crates/trusted-server-core/src/integrations/registry.rs'
git grep -n into_core_request -- crates/trusted-server-adapter-fastly
```

### New tests

1. **Real ingress.** In
   `crates/trusted-server-integration-tests/tests/integration.rs`, which CI runs
   with `--include-ignored` against Viceroy, workerd, and the Axum binary:
   `POST /auction` with a JSON body on Fastly and Axum, since workerd already
   has one. A publisher `POST` whose body reaches the origin stub byte for byte
   on Fastly, Cloudflare, and Axum. `HEAD /`, `OPTIONS /`, and a `GET /` with a
   body, which returns 400 (EZ-1 criterion 13). A chunked `POST /auction` over
   256 KiB with no `Content-Length` returns 413 (EZ-10 criterion 9).
2. **Spin.** Only the `trace-runtime-tests` job installs Spin 4.1.0 and builds
   the production component (`.github/workflows/integration-tests.yml:203-231`).
   Test 1's `POST /auction`, publisher `POST`, and `HEAD /` cases join as Spin
   cases named `spin_runtime_*`, and that job gains a third filter for them
   (`:230-231`). The `--include-ignored` job skips them (`:105-107`). Test 6's
   Spin leg, `navigation_bids_include_180_and_320_ms_and_exclude_600_ms_spin`,
   does not use the prefix, so both jobs name it on its own: the
   `--include-ignored` job skips it and the Spin filter runs it. A component
   that fails to instantiate fails CI (D14). `scripts/smoke-spin.sh` stays a
   manual check.
3. **Request readback.** The scripted client (D12) lives in
   `crates/trusted-server-core/src/test_support.rs` under
   `#[cfg(any(test, feature = "test-utils"))]`, as the EZ-24 interim says, so
   the Fastly adapter tests can use it. It implements EdgeZero's six-method
   head-first `OutboundHttpClient` trait
   (`edgezero:crates/edgezero-core/src/outbound.rs:426-446`) and supplies an
   `OutboundTime` whose timer factory wakes pending timers when the test clock
   advances (`edgezero:docs/guide/proxying.md:46-51`). It records every request
   and answers from a script with the arrival-time rules EdgeZero's batch uses. For each
   site in 2.3 it asserts the method, URI, and headers, `host_authority()` and
   `has_host_authority_override()`, a set `timeout`, a connect timeout of
   exactly 1 s (EZ-4 criterion 10), `Accept-Encoding` exactly as Trusted Server
   set it and absent when it set none, the decoding mode, the marker off
   through `effective_proxy_marker_enabled` (EZ-7), the response cap, the cache
   policy, and the Image Optimizer options read back from the request (EZ-8).
   On the asset proxy the options read back on a final GET and on a final
   HEAD, and never on the S3 preflight. For bidders it asserts the consent and
   attested client-IP headers. A header value that is not UTF-8 is dropped.
4. **Redirect policy (EZ-9 criterion 20).** The existing redirect, allowlist,
   and initial-target tests are ported to the scripted client and run again on
   the Axum binary against local origins. A redirect chain past its deadline
   fails, and a `Location` with userinfo fails.
5. **Auction.** A 12-bidder auction on Viceroy against 12 loopback `http`
   bidders on 12 distinct ports (D18), so each bidder needs its own cold
   backend registration (EZ-3 criterion 6). Backend identity leaves out the
   path, so bidders on one port would share a backend. A bid after the
   cutoff is dropped. The mediator runs after the batch. Two auctions on one
   configuration each configure and register exactly one backend per bidder in
   their own callback's observation. The observation is per callback, so it
   cannot show that the second auction reuses the first one's backend names;
   that needs reuse mode (EZ-2 criterion 21, deferred). The scripted client
   shows that two auctions with different remaining budgets send identical
   request options.
   Cloudflare and Spin load a
   multi-provider config and run the auction. On Spin, one bidder's provider
   timeout ends before the cutoff while a sibling answers after the cutoff,
   and the auction reports one provider timeout and the cutoff. The leg checks
   that outcome on a real runtime. It cannot force the event order behind open
   defect 3, so EdgeZero's fix carries the deterministic regression. The
   loopback bidders record the request framing they receive (section 8). An
   abandoned auction cancels its pending bidder requests. A bid that answers in
   time reports the same slot `elapsed` and telemetry `response_time_ms` when
   collection is delayed after its arrival (2.4).
6. **Publisher origin and bids (EZ-6).** On Viceroy, Axum, workerd, and Spin,
   with loopback bidders and an origin whose head arrives at 700 ms, a
   navigation with a 500 ms auction contains the bids that answered at 180 and
   320 ms and not the one that answered at 600 ms, as EZ-6's Trusted Server
   check states. A batch cutoff built from `ctx.monotonic_clock()` expires on
   time. The two paths differ (2.5):
   - On Fastly, a navigation GET that preloads an EC snapshot takes the
     overlapped path. The origin request is dispatched before bid
     construction, bids are collected at the `</body>` hold, and a bid that
     arrives while the origin body streams is observed before the cutoff.
     Core's `overlapped_navigation_starts_origin_before_ec_lookup` checks that
     order with the scripted client. The preload needs the KV graph, which the
     bot gate passes only for a client with a JA4 fingerprint. Stock Viceroy
     0.21.0 supplies none (its `downstream_tls_ja4` returns no value), so every
     navigation on Viceroy takes the plain path.
   - On Axum, workerd, and Spin, which have no KV graph, and on Viceroy, the
     plain path dispatches the auction first and sends the origin request
     while the bids are in flight.
   - On Viceroy,
     `navigation_bid_answered_while_origin_body_streams_is_injected_fastly`
     runs against an origin that sends its head at once and streams its body
     for about 1 s. The loopback servers record when each request head arrives
     and when each answer's last write starts. The origin request arrives
     before the 200 ms bidder answers, and every bid request arrives before the
     origin body ends. The 200 ms bid, which comes while the body streams, is
     injected. The 600 ms bid, which comes after the cutoff and before the body
     ends, is dropped.
7. **Encoded passthrough (EZ-5 criterion 8).** On Viceroy, against origin
   fixtures:
   - A gzip HTML page reaches the client with `Content-Encoding: gzip` and
     decodes to the rewritten document with the TSJS tag.
   - A br SVG relay is byte-identical, with the origin `Content-Length` and
     `ETag`.
   - `Range: bytes=0-99` against a gzip-stored asset returns 206 with the origin
     `Content-Range` and `Content-Length: 100`.
   - A JS asset revalidation with `If-None-Match` returns 304.
   - A `/first-party/proxy` HTML creative keeps `Content-Encoding: br`.
   - A gzip JSON bidder response is decoded.
   - An origin stub records `Accept-Encoding: identity` from a site that sets
     none.
8. **Inbound readers.** For each reader in section 4, a declared
   `Content-Length` over the cap returns 413 without polling the body, a stream
   that crosses the cap returns 413, a read past the deadline returns 408, a
   stream error with a 4xx status returns 400, and a host-side read error
   returns 502. The 400 case covers EdgeZero's ingress framing error. A
   client-caused error in the middle of a body reaches the helper as an
   internal error on every adapter, so it answers 502 (Failure handling).
   The trace action's check covers its declared case. A batch-sync stream that
   delivers more bytes than both its declared length and its cap returns 413
   (EZ-10 criterion 9).
9. **Fastly lifecycle.** Under Viceroy: `finalize` sets EC cookies, finalize
   headers with the geo lookup, the asset cache policy, and sandbox counters. A
   trace response passes untouched, and the trace routes answer through
   canonical dispatch. The callback returns `Ok` after a delivery failure, and
   a reused sandbox keeps serving. With `delivery::testing::{deliver_with_hooks, response_delivery}`,
   every row of the outcome table, including the result of
   `send_assembly_error`, takes its listed action. A request head that cannot
   be converted gets the native 500, if Viceroy can deliver one (EZ-17
   criterion 7). The reuse leg is
   `missing_config_blob_gets_500_then_recovers_in_same_sandbox`
   (`crates/trusted-server-adapter-fastly/src/app.rs:2110`), which runs without
   a feature gate. Assembly from a linked store with no app config blob fails,
   and EdgeZero's assembly error answers 500, today's startup status (EZ-22
   criterion 9's text says 503), with an EdgeZero JSON body and no
   `x-ts-version`. With the blob restored, the next initialization of the same
   `Sandbox` succeeds, and its `SandboxCounters` show one more build and the
   same instance (EZ-22 criterion 9).
10. **Validation.** `validate_settings_for_deploy` rejects each setting the
    Configuration table rejects, and each degrades as listed at runtime. Spin
    ignores `origin_host_header_override` with a warning. Settings load accepts
    a loopback `http` provider endpoint and still rejects other `http`
    endpoints. The Fastly state builder rejects an enabled plan with more than
    160 distinct provider tuples of scheme, host, port, and configured timeout,
    and accepts 160. Two providers that share a tuple count once. A disabled
    auction skips the check, as today (`plan.rs:421-423`).
11. **Head limits.** Through Viceroy, workerd, and the Axum binary, a signed
    `/first-party/proxy` URL that wraps a 4 KiB creative URL, sent with 40
    headers of 1 KiB each, is admitted. On Viceroy, a target of 8193 bytes
    returns 414, and about 72 KiB of headers in 60 lines returns 431. Viceroy's
    own HTTP parser answers 101 header lines with its own 431 before the guest
    runs, so the case avoids that (EZ-19 criterion 1). On Viceroy the 414, the
    431, and a framing 500 carry `x-ts-version`, `x-ts-fastly-version`, geo, and
    operator headers (EZ-19 criterion 6). D10 leaves Trusted Server no source of
    a framing 500, so that case uses a test-only route that answers with a
    `Trailer` field. The route exists only in the `test-utils` artifact the
    HTML end-of-file checks already build
    (`scripts/async-html-finalization-local-test.sh:2`). Canonical 404 and 405
    carry `x-ts-version` on every runtime.
12. **SigV4 host.** Under Viceroy, an S3-signed asset GET on a route without
    Image Optimizer reaches a local origin that records `Host`, path, and
    query, and each equals the signer's value. On a route with Image Optimizer,
    the HEAD preflight reaches it with the same checks.
13. **Post-delivery work.** Through `delivery::testing`, Fastly sends Tinybird
    rows and runs pull sync after a committed application delivery, and does
    neither when delivery did not commit or committed with a fallback body. A
    pull-sync group whose requests outlive `clock.deadline_after(15 s)` ends at
    the cutoff. The other adapters send no telemetry. Under Viceroy, pull sync
    reaches its partner stub after the origin aborts mid-body, and makes no call
    when a framing failure delivers the fallback (EZ-17 criterion 7).
14. **Management API.** Key rotation through the scripted client sends the
    expected method, headers, and body. A non-2xx answer yields an error with
    the body truncated to 200 characters.
15. **Axum ingress variables.** A variable raises its limit, and an invalid
    value fails startup.
16. **Response egress.** A unit test feeds `make_response_bodiless` a HEAD and a
    304 response that carry `Trailer`, and the output has none. On all four
    adapters, a publisher `HEAD` and a conditional `GET` that returns 304 return
    their real status. Validation rejects an operator `trailer` entry and an
    operator `connection` entry. A live config whose operator `connection`
    entry predates the cut loads, whether the value is `keep-alive,`, empty, or
    the malformed `x example`. The entry is skipped with a warning, and the
    response is served with its real status. Without the skip, `x example`
    fails egress with the framing 500 (section 5). A live config with
    `connection: x-example` and `x-example: example-value` serves `x-example`
    after the skip, which records that the ban drops nomination semantics.
17. **Write deadline.** A unit test calls the installed egress policy and gets a
    deadline 60 s after egress starts. Under Viceroy, a publisher page whose
    origin streams for 35 s reaches the client complete.
18. **Outbound error mapping.** `outbound_error` maps `PayloadTooLarge` to 413,
    `RequestTimeout` to 408, `BadRequest` to 400 on the publisher forward and to
    502 elsewhere, and `GatewayTimeout`, `BadGateway`, `ResponseTooLarge`, and
    `Internal` to the site's 502.
19. **Parity on the Axum test server (EZ-23 criterion 13).** The Axum,
    Cloudflare, and Spin apps run on `AxumTestServer` with production policies
    (section 8). A 9000-byte request target gets 414 from all three. A bodiless
    GET whose inbound body is an unread stream reaches a loopback origin with no
    request body.
20. **Publisher ownership (2.5).** These run with the scripted client behind
    H's real facade and head finishing, for `send` and for `begin` followed by
    `collect`. Each raw `Buffered` response (a pass-through 200, an
    unprocessable 404, and a 206 range response) attaches its owner once and
    keeps its bytes when the origin call is constructed inside `owner.run`. A control that
    constructs the call outside the scope gets "response body already owns an
    outbound scope" from `attach`. This pins the construction rule, not just
    the outcome.
21. **Publisher input caps (2.5).** On Axum, Cloudflare, and Spin,
    `buffer_publisher_response_async` collects every classification before
    egress. One matrix, `test_support::publisher_input_cap`, runs through each
    adapter's handler and finalizer: Axum's over loopback HTTP, and
    Cloudflare's and Spin's through the router. With
    `max_buffered_body_bytes` at its 16 MiB default, `cap` is 16 MiB on Axum,
    10 MiB on Cloudflare, and 8 MiB on Spin:
    - A raw body at `cap` is served byte for byte, and one byte over it fails
      with 502 before egress. Both raw routes are covered: a 200 that cannot be
      processed takes `ResponseRoute::PassThrough`, and a 404 that cannot be
      processed takes `ResponseRoute::BufferedUnmodified`.
    - A source error after part of the body fails with 502 before egress on
      both raw routes and on processed HTML, and none of the partial body is
      served.
    - Identity HTML and gzip HTML that decode to `cap` bytes are processed with
      every origin byte kept, and one decoded byte more fails with 502 before
      egress. The over-cap gzip page is far under `cap` encoded, so only its
      decoded size fails it. A gzip raw body is capped once decoded on
      Cloudflare (`Decode`), and is relayed in its coding on Axum and Spin
      (`Original`). A gzip raw body whose encoded bytes exceed `cap` fails with
      502 though it decodes under `cap`, because the origin request caps the
      encoded bytes too.
    - The pages link to the origin, and the rewrite shortens each link by more
      than the injected markup adds. The at-cap page therefore processes to
      fewer than `cap` bytes, and only the input cap can fail the longer one.
    - The processed output keeps its own bound. With `max_buffered_body_bytes`
      at 64 KiB, a page one byte under it fails with 502 once the injected
      markup grows it past the bound, and a raw body of the same length is
      relayed.

    Fastly streams the response with no input cap, so test 21 does not apply
    there.

Fastly differences items 16 and 17 are EdgeZero ingress and forwarding
behavior. No Trusted Server test covers them.

Ported tests: every user of `StubHttpClient`, `NoopHttpClient`, and
`StubBackend` moved to the scripted client or was deleted with the code it
tested, and the three doubles are deleted (`StubHttpClient` with its
self-tests in step 8, the other two in step 9). The bespoke doubles moved too:
`parity.rs:1137`, `:1402`, `:1445`, `:2183`, and `:2207`;
`crates/trusted-server-core/src/test_support.rs:405` and `:567`;
Fastly `app.rs:2934`, `:2954`, and `:3111`; `tinybird.rs:281` and `:318`;
`orchestrator.rs:3674` to `:4439`; `prebid.rs:3871`; `sourcepoint.rs:1175`;
`proxy.rs:2391` and `:2452`; `publisher.rs:12393`, `:19720`, and `:26239`; and
`auction/openrtb/test_executor.rs`. `ForbiddenLifecycle` (`parity.rs:1366-1400`)
also implements the store writes that 2.9 makes async. The scripted client
therefore supports streamed response bodies sent in chunks, a response held
until the test releases it, an error for one slot, and a forbid mode that fails
the test on any request. The hook tests move to `oneshot` (section 6). The Axum
route tests, the whole parity suite, and the CLI proxy test move to
`AxumTestServer` (section 8). The generator's bidder-alias tests
(`generate-viceroy-config.rs:370-468`) are deleted with the alias.

### Runtime

- `cargo test-fastly` and `cargo test-fastly-reuse` on stock Viceroy 0.21.0.
  Stock Viceroy reports some failed body reads as a clean end of stream. It
  also reports empty frames as ready while the next read may still wait
  (`edgezero:patches/viceroy/README.md:14-19`). So Trusted Server tests do not
  assert error preservation on body reads under stock Viceroy, and CI runs
  there are not timing evidence. CI keeps stock Viceroy. The qualification legs
  run on the repaired build
  ([EdgeZero qualification](#edgezero-qualification-trusted-servers-part)).
- `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml -- --include-ignored`
  as CI runs it, which covers `integration` and `parity`, and the
  `trace-runtime-tests` job's filters, which cover Spin.
- The worker deadline job, on workerd and Spin.
- The HTML end-of-file checks, because the publisher path and HTML finalization
  change: the `html_eof_runtime` test and its clippy run,
  `./scripts/async-html-finalization-local-test.sh axum --auction`, and
  `./scripts/async-html-finalization-local-test.sh fastly --release --auction`.
- `./scripts/integration-tests-browser.sh`, with loopback bidders.
- `scripts/smoke-fastly.sh`, `scripts/smoke-axum.sh`,
  `scripts/smoke-cloudflare.sh`, and `scripts/smoke-spin.sh`, with the
  `smoke-axum.sh` change in section 7.
- **Fastly response diff.** A script under `scripts/` sends a fixed request list
  to a `main` build and to the branch build, both under Viceroy. It diffs
  status, headers, and a body hash. The list covers a publisher page, `HEAD` and
  a conditional `GET` of a page, `POST /auction`, a first-party proxy URL, an
  asset, a tsjs bundle, an admin endpoint, `TRACE`, and a trace action. It
  normalizes `x-ts-version`, sandbox counter headers, dates, and generated IDs.
  The expected differences are exactly the [Fastly differences](#fastly-differences)
  the list exercises. Reviewers can rerun it.
- The hosted legs in the qualification table, on Fastly staging.

## Commit sequence

1. On `main` at `v0.0.11`, add test 1's cases except the `GET` with a body,
   and the response-diff script. They pass on `v0.0.11` and record the
   baseline.
2. Repin, update the lockfile and `spin-sdk`, add `[capabilities.outbound]` and
   the wrangler flags. Make the mechanical fixes (section 7), retype the hook
   (section 6), add `read_request_body` and move every reader to it (section 4),
   and port the Axum tests to `AxumTestServer`. Delete the Axum, Cloudflare,
   and Spin `startup_error_router`s, and return `Err` from `routes`. Fastly
   keeps its router until step 8. `Hooks::configure` installs the write policy
   and the synthesized headers on every adapter (section 5), because Axum,
   Cloudflare, and Spin run EdgeZero egress from this step. Trusted Server's
   outbound layer is still present and compiles, because H keeps
   `into_core_request` and `oneshot` public. Every target compiles. Fastly
   publisher forwards fail until step 7, GET included, because the inbound body
   is a stream and Trusted Server's Fastly client rejects streams. Spin
   publisher forwards also fail, GET included, because H streams every Spin
   request body that lacks an explicit `Content-Length: 0`
   (`edgezero:crates/edgezero-adapter-spin/src/request.rs:96-112`). Cloudflare
   publisher forwards fail only with a body. Red until step 7: the
   `cargo test` job's template-cache harnesses, `test-fastly-eof`, the Viceroy
   browser and trace jobs, the Fastly EC lifecycle job, and the
   publisher-forward integration cases. Whether fastly 0.13.1 always hands a
   GET a body handle is unverified. Running
   `./scripts/async-html-finalization-local-test.sh fastly --release` on this
   commit shows it.
3. Add the scripted client (D12), the `outbound_client` and `clock` fields of
   `RuntimeServices`, the outbound error helper (2.11), and the test helper that
   inserts a scripted client for `oneshot` route tests (2.1), with its uses in
   the Fastly, Cloudflare, and Spin route helpers. Move the parity suite's
   Cloudflare and Spin helpers to `AxumTestServer`, with the scripted client in
   router state (section 8). Every
   `RuntimeServices::builder()` call (41 in 16 files) sets both new fields in
   this step, because `build()` panics on a missing required service
   (`crates/trusted-server-core/src/platform/types.rs:454-470`). The adapters
   set them from the request context. The test helpers and fixtures use the
   scripted client, and `ForbiddenLifecycle` uses a forbid-mode script. The
   guide snippet waits for step 9, because `cargo check` never runs `build`.
4. Port the integrations (O10 to O15), with the `certificate_check` rule and the
   fixture and CI changes in section 1.
5. Port the proxies (O6 to O9), with redirect following and the asset proxy.
6. Port the auction (O1 to O3) and the provider contract. Delete the fan-out
   gate and `validate_for_target`, keep the Fastly backend bound, and add the
   loopback endpoint rule with the generator, fixture, browser-test, and
   `template-cache-local-test.sh` changes (D18).
7. Port the publisher origin (O4, O5), with the scope owner and both origin
   calls constructed inside it, raw collection on Axum, Cloudflare, and Spin,
   the readthrough rule, the host override, the `origin_url` rule, and the Spin
   warning (tests 20 and 21). Until
   step 8, Trusted Server's Fastly delivery converts the response body with
   `into_stream()`, so the attached owner lives until the end of the body. Port
   the EOF fixture (O19), and run the HTML end-of-file checks in Runtime. The
   integration cases turn green.
8. Move Fastly onto the canonical lifecycle (section 3), with post-delivery
   telemetry and pull sync (2.8), the management API (2.9), and the
   integration-guide snippet's async store writes with its test manifest.
   Fastly's error router goes here.
9. Delete Trusted Server's outbound layer and its test doubles (2.10, section
   8). Port the worker deadline examples and the Fastly deadline harness
   (section 8), update the integration-guide snippet and its test manifest, and
   add the `clippy.toml` guard. Rewrite the `IntegrationProxy` doc comment at
   `integrations/registry.rs:264`, which links to `PlatformPendingRequest`. The
   dependents inventory (Static) then prints only the four documents step 11
   rewrites.
10. Add the remaining egress rules (D10 validation, `make_response_bodiless`),
    the Axum ingress variables, the `smoke-axum.sh` change, and the observation
    log.
11. Update the rest of the documentation and `CHANGELOG.md`. The dependents
    inventory then prints nothing.

In steps 4 to 7, ported sites use `outbound_client` while the rest still use the
old layer. That transient dual path exists only inside the pull request.

## Follow-ups

- EZ-11 surrogate keys, to restore origin readthrough.
- EZ-12 certificate-check mode, if an operator still needs to skip
  verification.
- EZ-13 Spin host override, to honor `origin_host_header_override` on Spin.
- EZ-14 idle bound, to restore 15 s stall detection on streamed sites.
- EZ-15 fire-and-forget, to send telemetry during the request and on every
  adapter.
- EZ-24 published test double, to delete Trusted Server's scripted client.
- An EdgeZero ask for a version header on assembly-error responses (EZ-19.7).
- EZ-2 reuse-mode parts, before reusable sandboxes reach production.
- Streaming publisher responses on Axum, Cloudflare, and Spin.
- Declaring stores on Cloudflare and Spin, including whether the Cloudflare
  `block_on(get)` config bridge completes on workerd once it is live.
- Retained app instances on Cloudflare and Spin. H already has EZ-22's
  retention recipe, which retains only a successful assembly, so per-request
  assembly in the cut is a choice, not a forced interim. Adopting it brings
  EZ-22 criterion 10 into the gate (D19).
- `PlatformTimer` and EdgeZero's clock as one time source.
- An error renderer that brands framework errors.
- Dependabot ignore rules for EdgeZero-pinned crates.
- Moving the pin from the PR 275 commit to an EdgeZero release tag once PR 275
  merges (D5).
- Spin on `wasm32-wasip2`.
- The rest of #1505: Auctioneer as an in-process auction provider.

## Maintainer answers

The maintainer answered the open questions on 2026-11-08. The decisions they
change are D5, D7, D13, D18, and D20.

- **Q1. Merge gate (D5).** Do not wait for EdgeZero's EZ-Q1 record. `main`
  merges the cut pinned to PR 275, and production deploys do not wait for
  qualification ([Merge gate and pin](#merge-gate-and-pin)).
- **Q2. Timeouts (D7).** Each app that uses EdgeZero sets its own limits.
  EdgeZero already lets it set every one, so EdgeZero needs no change:
  - the per-request `timeout`, `connect_timeout`, and `deadline`
    (`edgezero@5ea8d93c:crates/edgezero-core/src/outbound.rs:1087`, `:1100`,
    `:1495`). The deadline also bounds a redirect chain under `follow`
    (`outbound.rs:615-623`).
  - the redirect count (`OutboundRedirectLimit::new`,
    `edgezero@5ea8d93c:crates/edgezero-core/src/outbound/redirect.rs:332`).
  - the response write deadline (`App::set_response_egress_policy`,
    `edgezero@5ea8d93c:crates/edgezero-core/src/app.rs:553`), 30 s by default
    (`response_egress.rs:22`).
  - the inbound read deadline, per request (`App::set_ingress_admission_policy`,
    `app.rs:514`), 30 s by default (`ingress.rs:34`).
  - the head limits (`App::set_ingress_head_limits`, `app.rs:523`).

  D7's values stand: a 1 s connect timeout, 60 s for streamed calls, 15 s for
  buffered calls, 60 s for redirect chains, a 60 s write deadline, and
  EdgeZero's default 30 s inbound read deadline for publisher uploads.

- **Q3. Hosted qualification.** The maintainer qualifies by hand for now, as
  the operator. For each run the maintainer chooses the staging service, its
  Image Optimizer entitlement, and EZ-2's M1 to M7 thresholds
  ([EdgeZero qualification, Trusted Server's part](#edgezero-qualification-trusted-servers-part)).
- **Q4. HTTP/3.** Qualified per adapter. EdgeZero amended EZ-1 criterion 2 in
  `dcbf622`, so its HTTP/3 legs no longer depend on Trusted Server's staging
  service
  ([PR 275 thread r4504554519](https://github.com/IABTechLab/edgezero/pull/275#discussion_r4504554519)).
- **Q5. In-flight pull requests.** The cut is stacked on PR 275, because its
  Axum changes need it. New uses of `PlatformHttpClient`, `PlatformBackend`,
  and `StubHttpClient` are not frozen. The cut ports them when it rebases
  ([In-flight work](#in-flight-work)).
- **Q6. Loopback bidder endpoints (D18).** Accepted. Provider endpoint
  validation accepts `http` on `127.0.0.1` and `[::1]`.
- **Q7. Bodiless Fastly framing (D20).** Strict suppression.
  - HEAD, 204, and 304 responses never include content (RFC 9110 §6.4.1). On
    HTTP/1.1 such a response ends at the blank line after its headers
    (RFC 9112 §6.3). On HTTP/2 a body there makes it malformed, and clients
    must not accept it (RFC 9113 §8.1.1). A server must not send content in a
    205 (RFC 9110 §15.3.6).
  - EdgeZero's local SDK controls show `send_to_client` breaking HTTP/2 clients
    on HEAD, 204, and 304, and sending a 205 body on both HTTP/1.1 and HTTP/2
    (`edgezero:docs/audits/evidence/2026-10-23-buffered-response-framing.json`,
    the nine `sdk_parity.failed_cases`).
  - Trusted Server withdraws EZ-18.4's bodiless part, and EdgeZero amended
    EZ-18.4, EZ-18.6, and EZ-18 criteria 3 and 6 to match in `dcbf622`
    ([PR 275 thread r4504573926](https://github.com/IABTechLab/edgezero/pull/275#discussion_r4504573926)).
    The response diff's expected set
    (`scripts/fastly-response-diff.expected.json`) cites Fastly differences
    items 5, 6, and 25. Bodiless framing, item 7, shows no difference on the
    diff's requests: the HEAD and 304 cases, `head-root` and
    `conditional-get-root`, expect only item 6's hop-by-hop fields.
  - The amendment keeps the nine cases as recorded reference differences, not
    parity failures. Hosted qualification of EZ-18 is still open, and it does
    not block the cut (D5).

## Expected files

- `Cargo.toml`, `Cargo.lock`, `edgezero.toml`, `clippy.toml`
- `crates/trusted-server-core/src/platform/` (delete `http.rs`,
  `backend_naming.rs`, `pending_response.rs`, `image_optimizer.rs`, and the HTTP
  doubles in `test_support.rs`; change `traits.rs`, `types.rs`, `mod.rs`)
- `crates/trusted-server-core/src/test_support.rs`,
  `test_support/scripted_client.rs` (scripted client), `test_support/body_read.rs`,
  `test_support/logging.rs`, and `test_support/async_html_finalization.rs`
- `crates/trusted-server-core/src/http_util.rs`, `error.rs`, `config.rs`,
  `settings.rs`, `response_privacy.rs`, `auth.rs`, `forwarder.rs`,
  `publisher.rs`, `proxy.rs`, `cache_purge.rs`, `creative_opportunities.rs`,
  `asset_image_optimizer.rs`, `html_processor/finish.rs`, `config_payload.rs`,
  `secret_resolution.rs`, `settings_data.rs`, `outbound.rs`,
  `edge_app.rs`
- `crates/trusted-server-core/src/auction/` (`mod.rs`, `orchestrator.rs`,
  `provider.rs`, `plan.rs`, `endpoints.rs`, `telemetry.rs`, `types.rs`,
  `test_support.rs`, `openrtb/test_executor.rs`, `openrtb/tests.rs`,
  `README.md`)
- `crates/trusted-server-core/src/ec/` (`pull_sync.rs`, `batch_sync.rs`)
- `crates/trusted-server-core/src/request_signing/` (`endpoints.rs`,
  `rotation.rs`)
- `crates/trusted-server-core/src/trace/` (`dispatch.rs`, `actions.rs`,
  `routes.rs`)
- `crates/trusted-server-core/src/integrations/` (`mod.rs`, `registry.rs`,
  `aps.rs`, and each integration in section 2.3), and
  `request_signing/jwks.rs`
- `crates/trusted-server-adapter-fastly/` (`Cargo.toml`, `src/main.rs`,
  `app.rs`, `middleware.rs`, `platform.rs`, `tinybird.rs`, `management_api.rs`,
  `compat.rs`, `delivery.rs`, `post_delivery.rs`, `sandbox.rs`, a new deadline
  test module, `tests/deadline-origin.mjs`; delete `backend.rs` and
  `platform/deadline.rs`)
- `crates/trusted-server-adapter-axum/` (`Cargo.toml`, `src/main.rs`, `app.rs`,
  `middleware.rs`, `platform.rs`, `tests/routes.rs`)
- `crates/trusted-server-adapter-cloudflare/` (`src/app.rs`, `middleware.rs`,
  `platform.rs`, `tests/routes.rs`, `tests/runtime/deadline.rs`,
  `wrangler.toml`, `wrangler.ci.toml`)
- `crates/trusted-server-adapter-spin/` (`src/lib.rs`, `app.rs`,
  `middleware.rs`, `platform.rs`, `tests/routes.rs`,
  `tests/runtime/deadline.rs`)
- `crates/trusted-server-integration-tests/` (`src/bin/generate-viceroy-config.rs`,
  `tests/parity.rs`, `tests/integration.rs`, `tests/documentation_snippets.rs`,
  `tests/html_eof_runtime.rs`, `tests/common/ec.rs`, `tests/common/runtime.rs`,
  environment helpers, the three
  fixture configs, and `browser/tests/nextjs/mobile-trace-live.spec.ts` if its
  bidder URL changes)
- `crates/trusted-server-cli/tests/proxy_trusted_server.rs`
- `.github/workflows/integration-tests.yml`, `.github/workflows/test.yml`,
  `.github/workflows/format.yml`,
  `.github/actions/setup-integration-test-env/action.yml`, and
  `.github/workflows/worker-auction-deadlines.yml` if its build steps change
- `scripts/integration-tests.sh`, `scripts/integration-tests-browser.sh`,
  `scripts/generate-integration-viceroy-configs.sh`,
  `scripts/template-cache-local-test.sh`, `scripts/smoke-axum.sh`,
  `scripts/README.md`, `scripts/test-worker-auction-deadlines.mjs` if its cases
  change, and the new response-diff script
- `docs/guide/integration-guide.md`, `docs/guide/architecture.md`, the other
  guide pages from section 10, and `CHANGELOG.md`
- `AGENTS.md`

## Review log

- **2026-11-05 rewrite.** This replaces the 2026-10-17 draft, which targeted
  `f6c708cc`, kept Trusted Server's outbound layer, and kept Fastly's custom
  lifecycle. The maintainer then chose D1 to D4, and EdgeZero implemented the
  requirements at H. Inputs were four read-only investigations at H: the
  blocking requirements and EZ-Q1 state, the follow-ups and the `v0.0.11` to H
  API diff, a compile probe of `51730bdaf` repinned to H, and a site-by-site
  migration map with an open pull request scan.
- **Round 1 (Claude Code, Opus, two fresh sessions with split scopes, verified
  against both trees).** Four blocking findings, all fixed. A connect timeout
  of `min(1 s, timeout)` and budget-derived timeouts would have split one
  origin across many Fastly backends, so D7 now uses constants and the batch
  cutoff carries the remaining budget. Deleting `asset_origin_host_header`
  would have broken S3 signing, so it stays. No rule mapped EdgeZero errors to
  Trusted Server errors and the 504 claims were false, so 2.11 and D17 keep
  today's 502s. The Cloudflare and Spin config and KV fallbacks are the live
  path, so they stay. Should-fix items adopted: H's request and response header
  and URI rules, the initial redirect check and policy order, `owner.run` and
  the owner in `PublisherResponse`, the provider contract, corrected response
  modes, a per-request telemetry sink, the Spin override as a warning, no new
  Image Optimizer validation, fixture configs, the EOF fixture site,
  `wrangler.ci.toml`, D8's wording, test 7 from EZ-5 criterion 8, the native
  request work before the send, the outcome table for detached and unknown
  deliveries, assembly errors without `x-ts-version`, startup-failure statuses
  and smoke scripts, the clippy guard for `tests/` crates, `body_as_reader`,
  the `Content-Length` precheck in the helper, the upload deadline question,
  the Cloudflare bridge reasoning, the qualification leg table and ownership
  counts, Spin in CI, and the observation log at info. Nits adopted throughout.
- **Round 2 (Claude Code, Opus, two fresh sessions: a cold whole-spec read and
  a check of every round-1 fix against both trees).** The fix check found 60
  of 64 round-1 findings fixed and 4 partly fixed. Three blocking findings, all
  fixed. Local bidder stubs were reachable only through a Viceroy backend-name
  alias that H cannot support, so D18 accepts loopback `http` endpoints and the
  generator rewrites them. The worker deadline CI job compiled against the
  deleted clients, so section 8 ports it. `validate_for_target` still rejected
  multi-provider plans on Cloudflare and Spin, so 2.4 deletes it and keeps the
  Fastly backend bound. Should-fix items adopted: the owner returned beside
  `PublisherResponse` and attached once at the call site, today's
  startup-failure statuses kept per error class, the Spin warning recorded as a
  deviation from the EZ-13 interim, five more Fastly differences (cancelled
  bidder requests, request hop-by-hop fields, `Location` userinfo,
  post-delivery gating, batched dispatch), tests for the write deadline, error
  mapping, post-delivery work, and synthesized headers, the unreachable
  Cloudflare config bridge and its test removed, Spin cases in the job that
  installs Spin, the EOF fixture's budget-derived values kept, and the scripted
  client's `Accept-Encoding` assertion. Nits adopted throughout, including
  citation corrections and the `outbound_client` field name for the transition.
- **Round 3 (Claude Code, Opus, one fresh session, blocking and should-fix
  only).** Four blocking findings, all fixed, and all of one kind: harnesses
  that depend on deleted code and that no section named. The Fastly deadline
  harness in the required `cargo test` job runs tests in `platform/deadline.rs`,
  so section 8 ports them. `template-cache-local-test.sh` reached its bidder
  through a predicted backend name and a private CA, so it moves to loopback
  `http`. `oneshot` route tests on Fastly, Cloudflare, Spin, and parity get no
  client, so 2.1 adds a helper that inserts a scripted one. The
  integration-guide snippet compiles the deleted APIs, so it changes in steps 8
  and 9. To stop finding these one round at a time, Verification now carries a
  dependents inventory command that must print nothing after step 9. Should-fix
  items adopted: gate 15 is already listed and not required, the generator's
  `--provider-endpoint-origin` flag and trace-only scope, the EOF fixture port
  in step 7, the bespoke test doubles and the scripted client's features, 12
  distinct bidder ports, the full red set in step 2, and the new home of the
  Fastly backend bound with the tests that flip.
- **Round 4 (Claude Code, Opus, one fresh session: round-3 fixes, the
  dependents inventory file by file, and the commit sequence).** All round-3
  findings fixed or partly fixed. One blocking finding, fixed with the
  reviewer's text. The Fastly deadline harness could not keep all five
  outcomes on H. It loses its only route to the origin port, a reply first
  polled after the cutoff is now dropped, and H has no missing-cache-bypass
  error. Section 8 now ports it through a config-store port and states the
  changed cases, and Fastly differences gains item 22. Should-fix items
  adopted: a portable inventory pattern (`\b` matched nothing on macOS Git)
  with the true count of 62 and the four documents step 11 rewrites,
  `auction-orchestration.md`, the `registry.rs` doc link,
  `AuctionContext::transport_timeout_ms` with a scoped check and APS's planned
  parser, the async store writes under `async_trait` with their 38
  implementers, the guide snippet's test manifest and re-exports, every
  `RuntimeServices::builder()` call setting the new fields in step 3, the Spin
  GET reason in step 2, and a test for the moved Fastly backend bound.
- **Round 5 (Claude Code, Opus, one fresh session: round-4 fixes, both
  inventory commands, and cross-section consistency).** No blocking findings.
  The config-store route to the origin port works, because Viceroy 0.21.0
  loads `local_server.config_stores` under `viceroy run`, and an EdgeZero wait
  drives a batch with no request lifecycle. Both "unverified" sentences now
  carry that evidence. Two should-fix items adopted. `batch.check()` never
  polls the SDK on Fastly, so 2.4, Fastly differences item 22, and the
  expired-snapshot harness case now use `batch.next()` after the cutoff. The
  new deadline test module must not sit under `platform::deadline`, which the
  inventory searches. Nits adopted: 37 store implementers in `crates/` plus the
  guide snippet, and a citation fix.
- **2026-11-06 update (Claude Code, Opus).** H moves from `8f82547b` to
  `9b31a697`, which changes no public API and no line an earlier draft cited.
  Inputs were a
  source check of every EZ requirement at `b56fccd2` by seven fresh sessions,
  review 5798502238 on PR 275 at `9b31a697` (5 blocking, 30 should-fix, 5 nits),
  and the other reviewers' open threads. Contradicted claims fixed:
  - Fastly batch setup and arrival recording (2.4).
  - Location handling on Cloudflare, percent-encoded locations, and the
    returned 3xx's body handling (2.6).
  - Streamed upload overflow (2.5, Fastly differences item 3).
  - `NameInUse`, now a documented collision (item 12).
  - The public envelope constructors (section 3).
  - EZ-19.3 versus EZ-19.7 (section 5).
  - The trace reader's declared-length case (section 4).
  - The unverified WASI `Host` claim and the Spin `run_app` citation (section 8).
  - The stock Viceroy limits (Runtime).
  - The required leg count, now 261.

  Gaps against Trusted Server's own criteria fixed:
  - `prepare` installs a per-request synthesized set (EZ-19 criterion 6).
  - The head-conversion outcome goes back to today's 500 (EZ-17 criterion 7).
    Fastly differences item 5 is removed, items 6 and 7 move up, and
    bodiless framing becomes item 7, so items 8 to 24 keep their numbers.
  - The confirmed follow-ups and their legs (D19).
  - The parity suite on the Axum test server (EZ-23 criterion 13).
  - The S3 preflight sets `Original`.
  - Bodiless Fastly framing is D20 and Q7.
  - A new section lists the open EdgeZero defects at H.

- **Round 6 (Claude Code, Opus, one fresh session: every changed claim against
  both trees, the new legs against EdgeZero's criteria text, numbering and
  anchors, and the hard-cut policy).** No blocking findings. 13 should-fix
  items adopted:
  - The handoff status of the requirements copy.
  - D19's scope under EZ-Q1.6 and EZ-Q1.8, and EZ-13's interim.
  - How collection handles a batch failure.
  - A misattributed core citation.
  - Spaces in `Location`, which already worked.
  - A self-referencing step.
  - Client body errors on Axum and Spin.
  - The five hosted follow-up legs and the post-D19 leg counts.
  - The inventory's `follow-up` legs.
  - The repaired Viceroy against EZ-Q1.3.
  - What test 5's Spin leg can prove.
  - Q7 reworded so neither option asks EdgeZero to rebuild old behavior.
  - The renumbering note.

  Nits adopted throughout, including per-value filtering of the synthesized
  set.

- **Round 7 (GitHub review 5810751514 on PR #1506, 2026-11-07, at
  `854fe2e36`).** One blocking finding and five should-fix, all adopted.
  - Blocking: the origin `send` and `begin` are now constructed inside
    `owner.run`. Constructed outside it, head finishing gives a raw origin body
    its own owner and the attach fails (2.5, test 20).
  - Axum, Cloudflare, and Spin now collect every raw publisher variant before
    egress, under today's caps (2.5, test 21).
  - The overlap claim and test 6 are narrowed to where the overlap runs:
    Fastly navigation GETs with an EC preload (2.5).
  - D10 now covers operator hop-by-hop fields, because a malformed
    `Connection` fails H's egress framing (section 5).
  - The final client HEAD keeps its Image Optimizer options (2.7).
  - The telemetry latency's changed stop point is stated, with a
    delayed-collection test (2.4, test 5).

- **Round 8 (GitHub review 5815168335 on PR #1506, 2026-11-07, at
  `5bd113408`).** Four round-7 fixes were accepted and resolved, and two were
  partly fixed. Both remainders are adopted:
  - The adapter input cap now covers every publisher classification, processed
    HTML included, through H's encoded-byte cap and a cumulative decoded-byte
    bound. The processed-output cap stays separate (2.5, test 21).
  - The claim that skipping operator hop-by-hop entries is wire-neutral is
    removed. Skipping `Connection` drops its nomination semantics, so a field
    it names reaches the client. Validation finds such configs before deploy
    (section 5, test 16).

- **Round 9 (GitHub review 5815676577 on PR #1506, 2026-11-08, at
  `a83dc4d89`).** The reviewer accepted and resolved the two round-8 fixes and
  found no new defect. Its verdict was BLOCKED on Q1 to Q7. The maintainer's
  answers are recorded in [Maintainer answers](#maintainer-answers):
  - The cut is stacked on PR 275 and follows its head. `main` merges without
    waiting for EdgeZero's EZ-Q1 record, and production deploys do not wait
    for qualification (D5, Merge gate and pin).
  - Limits are app-owned. EdgeZero already exposes every one, and D7's values
    stand.
  - Hosted qualification is manual, and the maintainer is the operator.
  - HTTP/3 is qualified per adapter, and EZ-18.4's bodiless part is withdrawn.
    Both are asked of EdgeZero in PR 275 threads.
  - New uses of the outbound layer are not frozen, and loopback `http`
    endpoints are accepted.
  - The five blocking defects found at H were verified fixed at later PR 275
    heads (Open EdgeZero defects at H).

- **Round 10 (GitHub review 5816590624 on PR #1506, 2026-11-08, at
  `85f5ca3d9`).** No new defect. The reviewer accepted the recorded answers and
  re-verified the six earlier corrections. Its verdict stayed BLOCKED only
  because the requester had not yet confirmed directly that the Q1 to Q7
  decisions, merge and production before EZ-Q1 among them, were theirs.

- **Round 11 (GitHub review 5816701136 on PR #1506, 2026-11-08, at
  `85f5ca3d9`).** The requester confirmed all seven decisions, and the reviewer
  changed its verdict to APPROVE at the same head, with nothing blocking.
  GitHub refused a formal approval because the reviewer and the author share
  one account, so the verdict is a review comment.

- **Implementation phase (2026-11-08 to 2026-11-13).** The cut was built from
  the approved spec, and each correction the code forced is folded in above.
  - An internal whole-branch review at `6c39a56a1` (Claude Code, Opus) found
    nothing critical, four important findings, and eight minor ones (M1 to
    M8). The important ones were client errors logged at error (I1), a CodeQL
    cleartext-logging alert in the scripted client (I2), no automated test for
    test 11's Fastly leg (I3), and twelve spec passages that contradicted the
    code (I4). The commits up to `9ec4711d3` fixed them and brought those
    passages in line.
  - GitHub review 5887835958 (2026-11-13, at `9ec4711d3`) asked for the full
    publisher cap and error matrix of test 21 and a Viceroy oracle for test 6's
    overlap. Both were built (tests 6 and 21).
  - GitHub review 5888499941 (2026-11-13, at `9ec4711d3`) left 72 findings: 2
    blocking, 29 should-fix, and 41 nits. The blocking ones were #1516's
    missing port and the 1 MiB Prebid Server and APS cap. The fix round that
    followed merged `main` with #1516 (In-flight work), restored the
    per-profile bidder caps (Fastly differences item 26), deleted the code the
    cut left over, made each outbound failure log once (section 9), added the
    missing tests, and corrected this spec. Two gaps it found are EdgeZero's,
    and Trusted Server asked EdgeZero for both: the Spin upload framing (2.5)
    and the Cloudflare and Spin log backend (section 9).
