# EdgeZero Requirements for the Trusted Server Cutover

**Date:** 2026-10-17

**Status:** Handed to EdgeZero on 2026-10-18. PR 275 added its own edited copy in commit `bc300ace` and has amended it since, including its 2026-10-24 Scoped Amendment. EdgeZero's copy is the contract EdgeZero is measured against. This file is the text Trusted Server handed over.

**EdgeZero base:** PR 275 head `f6c708ccb5be19ba9fa92575db3c08c0f17e0d72` (branch `docs/outbound-http-spec`), called **F** below

**Consumer:** Trusted Server `main` at `a9037770a`, which pins EdgeZero `v0.0.9` today

**Companion:** [EdgeZero PR 275 hard cutover](2026-10-17-edgezero-pr275-hard-cutover-design.md), the Trusted Server side of the cut. The companion records the decisions; this copy is as handed over.

## Purpose

Trusted Server (TS) is moving onto EdgeZero PR 275 in one hard cut. TS will use EdgeZero's
canonical request lifecycle on all four adapters and EdgeZero's outbound HTTP API for every
outbound request, and it will delete its own transport layer. TS keeps no HTTP or transport
plumbing of its own. Where F cannot do what TS does in production today, the gap is a requirement
in this document, not a TS workaround.

This document states what EdgeZero must do, not how. Each requirement gives the Trusted Server need,
the gap at F with code citations, normative requirements on observable behavior, acceptance
criteria, what is out of scope, and open questions for the EdgeZero maintainers. API shape, internal
design, and test harnesses are the EdgeZero implementers' choice.

## Decisions behind these requirements

The TS maintainer made these decisions. The requirements assume them.

1. **Hard cut.** TS moves to the PR 275 APIs with no backward compatibility, no shims, and no dual
   paths.
2. **No TS plumbing.** TS deletes its own outbound HTTP layer, backend naming, raw-SDK clients,
   response decoder, encoding strip, redirect loop, and Fastly response delivery code. Every
   outbound call goes through EdgeZero's outbound API.
3. **Canonical Fastly lifecycle.** TS Fastly moves onto EdgeZero's canonical send path inside its
   custom entry point, with TS logic in the lifecycle's prepare and finalize hooks. Production runs
   single-request sandboxes. Reusable-sandbox mode stays off by default
   (`ts:crates/trusted-server-adapter-fastly/Cargo.toml:13-18`), so requirements specific to reuse
   mode are follow-ups needed before TS enables it.
4. **Redirects move into EdgeZero.** TS's per-hop redirect loop is deleted in favor of EZ-9.
5. **Blocking set.** The cut cannot ship to production without the blocking parts of EZ-1 through
   EZ-9 and a passing EZ-Q1 record. The record covers every acceptance criterion of those blocking
   parts, each in the environment it names, together with the criteria of any promoted requirement
   and of follow-ups present at the pinned commit (EZ-Q1.6). Criteria of reuse-mode follow-ups gate
   only before TS enables reuse mode. Everything else is follow-up unless the maintainers accept a
   change from [Proposed priority changes](#proposed-priority-changes).

## How Trusted Server will use EdgeZero after the cut

This is context for the requirements, not a requirement on EdgeZero's design.

- **Lifecycle.** Fastly runs EdgeZero's canonical send path with TS's client-IP sanitization and TLS
  signals in the prepare hook, and TS response finalization, EC finalization, privacy guards, and
  sandbox counters in the finalize hook. The state finalize returns drives post-send EC pull sync.
  Axum, Cloudflare, and Spin run EdgeZero's standard entry points. TS sets the response egress
  policy, head limits, and error renderer through `Hooks::configure`.
- **Auction.** Bidders become outbound requests that run as one batch per auction with the auction
  deadline as the cutoff. The mediator is one ordinary send after the bidders.
- **Publisher origin.** On the overlapped path (a navigation `GET` with EC preload,
  `ts:crates/trusted-server-core/src/publisher.rs:4821-4846`), the origin fetch begins before bid
  construction, runs alongside the bidder batch, and streams its response (EZ-6). Bids are collected
  at the `</body>` hold while the page streams. Every other origin forward, including `POST`,
  `PUT`, `PATCH`, `DELETE`, and `OPTIONS`, is an ordinary send with the inbound body streamed
  upstream. Both are built from the inbound request (EZ-1).
- **Proxies and integrations.** About twenty call sites send through EdgeZero. They include the
  first-party proxy, asset routes (with Fastly Image Optimizer and S3 SigV4 signing, EZ-8), GTM,
  GPT, the JS asset proxy, the Prebid bundle, Lockr, Permutive, DataDome, Sourcepoint, Didomi, and
  testlight. Redirects follow through EZ-9. Encoded bodies pass through undecoded where TS relays
  bytes (EZ-5).
- **Out-of-request work.** EC pull sync runs after delivery, inside the same lifecycle callback,
  with the request's outbound client (cross-cutting requirement 4).
  Tinybird telemetry is fire-and-forget (EZ-15). The Fastly management API runs inside admin
  handlers.
- **Inbound bodies.** Endpoints read bodies with EdgeZero's bounded readers at their own caps
  (EZ-10 restores 413).

## Requirement index

| ID    | Title                                                                             | Priority         | Adapters                                   |
| ----- | --------------------------------------------------------------------------------- | ---------------- | ------------------------------------------ |
| EZ-1  | Bodiless GET/HEAD forwarding                                                      | Blocking         | all four                                   |
| EZ-2  | Fastly connection reuse across requests                                           | Blocking in part | Fastly                                     |
| EZ-3  | Cold backend registration never fails a request on its own                        | Blocking         | Fastly                                     |
| EZ-4  | Connect timeout independent of the total budget                                   | Blocking         | all four                                   |
| EZ-5  | Encoded response passthrough                                                      | Blocking         | all four                                   |
| EZ-6  | Begin a request without waiting, and observe a batch without blocking             | Blocking         | all four                                   |
| EZ-7  | Keep the `x-edgezero-proxy` header off relayed responses                          | Blocking         | all four                                   |
| EZ-8  | Fastly Image Optimizer on outbound requests                                       | Blocking         | Fastly; others reject                      |
| EZ-9  | Redirect following with the application's target policy at every hop              | Blocking         | all four                                   |
| EZ-10 | 413 Payload Too Large for request-body overflow                                   | Follow-up        | all four                                   |
| EZ-11 | Cache tagging with surrogate keys                                                 | Follow-up        | Fastly, Axum, Spin; Cloudflare unsupported |
| EZ-12 | TLS certificate verification mode                                                 | Follow-up        | Fastly; others unsupported                 |
| EZ-13 | Spin host authority override                                                      | Follow-up        | Spin                                       |
| EZ-14 | Idle-time bound for streamed response bodies                                      | Follow-up        | all four                                   |
| EZ-15 | Explicit fire-and-forget send                                                     | Follow-up        | all four                                   |
| EZ-16 | Provider failure detail available to logs                                         | Follow-up        | all four                                   |
| EZ-17 | Fastly send outcome tells the application what was committed                      | Follow-up        | Fastly                                     |
| EZ-18 | `Content-Length` on buffered responses                                            | Follow-up        | all four                                   |
| EZ-19 | Application headers on detached and egress-fallback responses                     | Follow-up        | all four                                   |
| EZ-20 | A declared store that cannot open fails at use, not on every request              | Follow-up        | Fastly; Cloudflare and Spin should         |
| EZ-21 | Framing failure reason in logs                                                    | Follow-up        | all four                                   |
| EZ-22 | Fallible application state built and retained with the routes                     | Follow-up        | all four                                   |
| EZ-23 | Axum dev server applies application policies, and tests can run real Axum ingress | Follow-up        | Axum                                       |
| EZ-24 | Published outbound test double that behaves like production                       | Follow-up        | core                                       |
| EZ-25 | Documentation drift fixes                                                         | Follow-up        | docs                                       |
| EZ-Q1 | Recorded passing qualification run of the canonical Fastly composition            | Gate             | all four                                   |

## Cross-cutting requirements

These apply to every requirement in this document.

1. **Every send mode honors every option.** An outbound option (decoding mode, proxy marker, connect
   timeout, idle bound, cache intent, TLS verification, Image Optimizer) behaves the same on a
   single send, a begun request (EZ-6), a batch slot, and every redirect hop (EZ-9), and on a
   fire-and-forget send once EZ-15 lands. Where a requirement marks a send mode Unsupported for an
   option, that send mode rejects it (requirement 2). No send mode silently drops an option.
2. **One rejection for unsupported options.** An option whose requirement marks an adapter or send
   mode Unsupported is rejected there before anything is sent, with one documented error category
   that is the same on every adapter and consistent with F's other outbound preflight rejections.
   This rejection comes before every other outbound preflight check. An option marked BestEffort is
   accepted, and its weaker guarantee is documented in the capability matrix.
3. **Bounded memory.** Every response EdgeZero holds or drains on the application's behalf,
   including responses that arrive while the application is busy elsewhere, stays within that
   request's encoded, decoded, and final caps.
4. **Request lifetime.** For outbound purposes an inbound request's lifetime ends when the
   application's per-request lifecycle callback returns, not when its response is delivered. Work the
   application does inside that callback after delivery, such as TS's EC pull sync, can use the
   request's outbound client. Anything still in flight when the callback returns is released, except a fire-and-forget send, whose lifetime EZ-15 defines.
5. **Arrival decides lateness.** A response arrives when its outcome is final. For a buffered batch
   slot or buffered begun request that is when its last body byte was received, or when the error or
   cap failure that ended it occurred. For a streamed begun request it is when its response head was
   received. Lateness is judged at the earliest instant EdgeZero observed the arrival, never at when
   the application collected the result. Where an adapter can observe arrivals only during EdgeZero
   operations (an outbound wait, a body read, or a non-blocking check), it records each arrival no
   later than the next such operation, and the requirement states that adapter's capability level.
6. **Abandon releases; fire-and-forget is explicit.** Dropping or cancelling an in-flight request or
   batch releases it on every adapter as far as the host allows. Sending without waiting for the
   result is a separate, explicit capability (EZ-15). This document calls that a fire-and-forget
   send. "Detached" keeps EdgeZero's meaning of a response produced outside routing.
7. **Diagnostics stay off the wire.** Provider failure detail, framing failure reasons, and payload
   limit reasons are available to application logs and never reach a response. Error text that
   applications and clients see today does not change.
8. **No silent regressions.** Behavior TS relies on at F (deadline enforcement, admission and
   framing checks, reusable-sandbox serving, existing public APIs) keeps working unless a
   requirement changes it explicitly.
9. **Capability matrix.** Each requirement's per-adapter capability level (Native, BestEffort,
   Unsupported) appears in EdgeZero's capability matrix. Fixing guide text that is wrong at F is part
   of the requirement it concerns. Requirements do not ask EdgeZero to edit its immutable
   `docs/superpowers/specs/`.
10. **Checkable where it matters.** Each acceptance criterion names the environment where the
    behavior must hold, from among unit, Viceroy 0.21.0, hosted Fastly, workerd, Spin 4.1, and Axum.
    Viceroy 0.21.0 enforces first-byte and between-bytes timers on dynamic backends and reports their
    expiry as a generic host error (`viceroy:src/upstream.rs:351-372`, `viceroy:src/body.rs:235-244`).
    It stores the connect timeout without enforcing it, so connect-time behavior is checked on hosted
    Fastly. A reused Viceroy guest serves at most a handful of requests, Viceroy reports a refused
    connection without detail, and its HTTP layer rejects more than 100 header lines, so criteria
    that depend on those facts name hosted Fastly or are written around them. "Every adapter" in a
    criterion means Axum, workerd, Spin 4.1, and Viceroy 0.21.0. A criterion has a hosted Fastly leg
    only where it names hosted Fastly.

## Dependencies between requirements

- EZ-6's overlapped origin fetch is a GET built from the inbound request, so it needs EZ-1. On
  Fastly its timelines rely on EZ-2's exact deadline enforcement.
- EZ-9's hops rewritten to GET or HEAD are bodiless, so it needs EZ-1. Each new redirect origin is a
  new Fastly backend, so it interacts with EZ-2.
- EZ-3 depends on EZ-2. A cold registration can only stop failing requests on its own once exact
  deadlines are enforced independently of registration timing.
- EZ-4's connect timeout and EZ-14's idle bound change how Fastly backends are configured, so they
  interact with EZ-2's connection reuse.
- EZ-5 does not depend on EZ-18. A preserved upstream `Content-Length` is the application's to
  relay, and EZ-18 separately governs lengths on buffered responses EdgeZero frames.
- EZ-8 and EZ-11 conflict on Fastly. The platform disables Image Optimizer under cache bypass and
  ignores surrogate keys with Image Optimizer, so those combinations are rejected (cross-cutting
  requirement 2).
- EZ-15 and EZ-6 share abandonment semantics (cross-cutting requirement 6).
- EZ-16, EZ-17, and EZ-21 share the log-only diagnostics of cross-cutting requirement 7.
- EZ-20's "nothing was sent" outcome is the one EZ-17 reports, once EZ-17 lands.
- EZ-22 changes what canonical Fastly entry points do when assembly fails, EZ-17 reports it, and
  EZ-23's in-process tests build on EZ-22's state.
- EZ-24 follows EZ-6 and EZ-9 semantics, so it changes with them.
- EZ-Q1 runs at the rev that contains the blocking parts of EZ-1 through EZ-9 and any promoted
  requirement.

## Landing and pinning

Whether the blocking requirements land in PR 275 before it merges or in follow-up EdgeZero pull
requests is the EdgeZero maintainers' call. TS needs three things from the rev it pins:

- It contains the blocking parts of EZ-1 through EZ-9 and any requirement promoted to blocking.
- It has a passing EZ-Q1 record for that exact commit. EdgeZero produces the results for criteria
  in unit, Viceroy 0.21.0, workerd, Spin 4.1, and Axum that do not need TS, and assembles and keeps
  the record. TS produces its end-to-end checks and all hosted Fastly results, running an
  EdgeZero-provided qualification build for criteria TS configuration cannot express and its own
  build for the rest, in single-request mode unless a criterion says otherwise.
- It stays reachable. A tag at that commit is enough, so a later force-push or squash merge cannot
  leave TS pinned to an unreachable commit.

## Proposed priority changes

The EdgeZero and TS maintainers decide these.

| ID    | Change                                   | Reason                                                                                                                                                                                                                                                                                                                                                                            |
| ----- | ---------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| EZ-2  | Split into blocking and reuse-mode parts | Production runs single-request sandboxes. Connection reuse across those sandboxes (EZ-2.1 to 2.3, 2.6 to 2.10, and 2.13 to 2.17) blocks the cut. Behavior specific to reusable-sandbox mode (EZ-2.4, 2.5, 2.11, 2.12, and 2.18) is needed only before TS enables reuse mode, so it is a follow-up.                                                                                |
| EZ-15 | Stays Follow-up (recommended)            | TS emits Tinybird telemetry inside the `</body>` hold today. Until EZ-15 lands, TS sends the telemetry rows with an ordinary send after delivery, inside the same lifecycle callback (cross-cutting requirement 4), so the page tail is not held. Promote the Fastly part (EZ-15.1 to 15.8, 15.10, and the Fastly clause of 15.11) only if that later emission is not acceptable. |
| EZ-24 | Minimal core Blocking (recommended)      | The cut deletes TS's 22 outbound test doubles. Their replacement must match EdgeZero's begin, non-blocking batch checks, arrival-time completion, cutoff, and redirect behavior exactly, and a TS-written copy would drift and be thrown away later. The minimal core is EZ-24.1 to 24.10, with EZ-24.2 limited to the host and wasm32-wasip1 targets.                            |
| EZ-18 | Stays Follow-up                          | At F every buffered Fastly response goes out chunked with no `Content-Length`, where TS sends a fixed length today. Header parity is the only reason to promote it, since the tsjs bundles carry no `Content-Length` and host compression is unaffected. TS lists the change as an accepted Fastly difference until EZ-18 lands.                                                  |
| EZ-23 | Stays Follow-up                          | TS's Axum binary moves to `run_app`, which applies `Hooks::configure`. Only programmatic servers and in-process tests lose the policies, and TS keeps the ingress-level checks in its integration suite, which runs the real binary.                                                                                                                                              |
| EZ-13 | Close as documentation and evidence      | WASI HTTP 0.3 in spin-sdk 7.0.0 has one authority for routing and `Host`, so the override cannot be built. EdgeZero documents `Unsupported` as final with the evidence, and TS rejects `origin_host_header_override` on Spin at config validation.                                                                                                                                |

## Verification

Each requirement lists acceptance criteria and the TS check that proves it end to end. EdgeZero's
evidence comes from unit and adapter tests, Viceroy 0.21.0 with Fastly SDK 0.13.1, workerd, and
`spin up` on Spin 4.1, at Rust 1.98.1. TS reaches the same toolchain through TS PR #1123, which the
cut depends on. EZ-Q1 states what the qualification record must show before production. The hosted
measurements in EZ-2 to EZ-4 compare against a pre-cut TS build from `main` at `a9037770a`
with TS PR #1123. TS also runs its integration suite (Viceroy, workerd, and the Axum binary), its
browser suite, the Spin smoke, and a Fastly response diff against the same pre-cut build.

## Related EdgeZero work outside these requirements

These EdgeZero pull requests are not needed for the cut, but TS work that lands after it depends on
them, and each needs a rebase onto PR 275:

- **EdgeZero PR #403** (pre-dispatch hooks and request ingress fidelity). TS #1107 and #1251 use it
  heavily. F's ingress admission policy is synchronous and read-only, so it does not replace #403.
- **EdgeZero PR #389** (request timing). TS #1261 uses it.
- **EdgeZero PR #409** (trusted proxy metadata) touches the same request path.

## Citation roots

- `edgezero:` EdgeZero at F, `f6c708ccb5be19ba9fa92575db3c08c0f17e0d72`.
- `edgezero@v0.0.9:` EdgeZero at tag `v0.0.9`.
- `edgezero-ci:` GitHub Actions run 38934086784 on F.
- `ts:` Trusted Server `main` at `a9037770a`.
- `ts@pr1123:` Trusted Server at the head of PR #1123, commit `4c5ca8ae5`, by file and line.
- `fastly-sdk:` the `fastly` 0.13.1 crate. `fastly-sys:` and `fastly-shared:` the 0.13.1 crates.
- `viceroy:` `viceroy-lib` 0.21.0. `viceroy-0.17.0:` `viceroy-lib` 0.17.0, cited only to show
  history. `hyper-0.14.26:` the hyper version Viceroy 0.21.0 uses.
- `spin-sdk:` spin-sdk 7.0.0. `worker:` worker 0.8.5. `reqwest:` reqwest 0.13.4.
- `dep:<crate>-<version>/<path>` a registry crate at the version EdgeZero pins.

Claims marked **Unverified** say what would verify them.

## Blocking requirements

### EZ-1. Bodiless GET/HEAD forwarding

**Priority:** Blocking
**Adapters:** Fastly, Axum, Cloudflare, Spin. The outcome must be the same everywhere. Adapters may differ only in whether the inbound body has to be read to learn that it is empty (EZ-1.2).
**Related:** EZ-6, whose begin call must accept the same bodiless request. EZ-9, which owns resolving the body once per redirect chain (EZ-9.7). EZ-10, which leaves a GET carrying a body at 400 and moves only size overflow to 413. EZ-Q1, which requires a passing result for every criterion here in the environment it names. EZ-25, whose EZ-25.1 owns the forwarding-example change.

#### Need

- TS proxies every request that no TS route claims to the publisher origin. Fastly registers that fallback for seven methods (`ts:crates/trusted-server-adapter-fastly/src/app.rs:291-301`), GET, POST, HEAD, OPTIONS, PUT, PATCH and DELETE, on every route path and on `/` and `/{*rest}` (`:1319-1331`). Axum, Cloudflare and Spin call the same core handler (`ts:crates/trusted-server-adapter-axum/src/app.rs:253`, `ts:crates/trusted-server-adapter-cloudflare/src/app.rs:562`, `ts:crates/trusted-server-adapter-spin/src/app.rs:843`).
- The handler rewrites the inbound request in place (`ts:crates/trusted-server-core/src/publisher.rs:3123-3146`, called at `:4810`) and moves it, body included, into the origin request (`:4812-4813`).
- After the cut TS forwards with `OutboundRequest::from_request(req, origin_uri)` and `HttpClient::send`, streaming the inbound body upstream. Only the overlapped origin fetch uses EZ-6's begin call (`publisher.rs:4847-4872`). TS overlaps only when all of these hold. The request is a navigation GET with an EC id and a KV store, and the EC snapshot is needed (`:3109-3116`, `:4821-4833`). The request has no shared-template cache key. The client supports concurrent fan-out and pending streamed responses (`:4840-4846`).
- Without EZ-1 every forwarded GET and HEAD fails with 400. That is every page navigation, every HEAD, and every asset that falls through to the publisher, which is most TS traffic. The TS integration scenarios that load a page through the fallback (`ts:crates/trusted-server-integration-tests/tests/frameworks/scenarios.rs:68-69`) fail on every runtime.
- TS cannot work around it without the body handling that decision 2 forbids. Forcing an empty body on every GET silently drops a body a client really sent, and checking emptiness means TS reads the inbound body itself.
- Today TS on EdgeZero v0.0.9 buffers the client body on Fastly (`edgezero@v0.0.9:crates/edgezero-adapter-fastly/src/request.rs:520-525`) and forwards any non-empty body, GET included (`ts:crates/trusted-server-adapter-fastly/src/platform.rs:330-340`). After EZ-1 a GET that carries body bytes gets 400. Cloudflare's `fetch` forbids a GET body, and EdgeZero already normalizes to that rule (`edgezero:docs/superpowers/specs/2026-05-21-outbound-http-design.md:1588-1591`). TS accepts the change.

#### Gap at F

- Every adapter hands the application a streamed body for every admitted request, bodiless or not. See Fastly `edgezero:crates/edgezero-adapter-fastly/src/request.rs:498-507`, Axum `edgezero:crates/edgezero-adapter-axum/src/request.rs:43-48`, Cloudflare `edgezero:crates/edgezero-adapter-cloudflare/src/request.rs:370-385` and `:657-658`, and Spin `edgezero:crates/edgezero-adapter-spin/src/request.rs:574-579`. The stream constructor always yields a stream (`edgezero:crates/edgezero-core/src/body.rs:63-68`). Cloudflare used to keep an empty body when the Worker request reported none. Commit 86f6e834 replaced that with a lazy read, because the browser test runtime can report no body stream for a request that does carry bytes.
- `OutboundRequest::from_request` keeps that stream and drops `Content-Length` and `Transfer-Encoding` at once (`edgezero:crates/edgezero-core/src/outbound.rs:927-934`, `:1858`, `:1863-1866`).
- Preflight rejects every GET/HEAD with a streamed body with 400 (`outbound.rs:1561-1575`). EdgeZero's outbound design calls that a deliberate false positive (`outbound-http-design.md:1619-1639`), and it predates lazy ingress.
- EdgeZero's own forwarding example (`edgezero:docs/guide/proxying.md:21-54`) and the scaffold and demo `proxy_demo` routes, declared for GET (`edgezero:crates/edgezero-cli/src/templates/root/edgezero.toml.hbs:64-67`, `edgezero:examples/app-demo/edgezero.toml:70-76`), fail the same way. This was found by reading the code, not by running it. The guide example also does not compile, because it passes `ctx.into_request()` to `from_request` without unwrapping the `Result` it returns (`proxying.md:32`, `edgezero:crates/edgezero-core/src/context.rs:381`). No test catches the failure, because every `from_request` GET test supplies an empty buffered body (`outbound.rs:4391-4403`, `edgezero:crates/edgezero-cli/src/templates/core/src/handlers.rs.hbs:580-603`, `edgezero:examples/app-demo/crates/app-demo-core/src/handlers.rs:1411-1415`).
- Fastly, Cloudflare and Spin do not carry the client's HTTP version into the core request (Fastly `request.rs:711-719`, Cloudflare `request.rs:327-342`, and Spin, whose WASI request has no version, `dep:wasip3-0.7.1+wasi-0.3.0/wit/deps/http.wit:266-343`). Request headers alone therefore cannot tell an HTTP/1.1 bodiless GET from an HTTP/2 or HTTP/3 GET that carries DATA frames with no `content-length`.
- Hosts differ in whether they say a body is empty before it is read. hyper marks a zero-length HTTP/1 body as ended (`dep:hyper-1.12.0/src/proto/h1/dispatch.rs:313-314`). Fastly reports a known length only in some cases, and its docs list chunked HTTP/1.1 and HTTP/2-or-later without `content-length` as unknown (`fastly-sdk:src/http/body.rs:339-352`). Spin knows only an explicit `Content-Length` (`dep:wasip3-0.7.1+wasi-0.3.0/src/http_compat/mod.rs:148-153`, `:278-296`). F declares every adapter's framing host-managed (`edgezero:docs/guide/capabilities.md:98-102`).
- On the outbound side, Spin hands the host a body stream for every request, even an empty buffered GET (`edgezero:crates/edgezero-adapter-spin/src/outbound.rs:362-364`). Whether the Spin host then frames an empty GET as chunked is **Unverified**. Fastly sends an empty buffered body as an empty set body (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:445-453`) and a still-streamed body through its streaming send (`:1170-1187`). Whether the Fastly host frames a zero-byte streamed upload as chunked is **Unverified**.

#### Requirements

- **EZ-1.1** An inbound GET or HEAD is bodiless when the client sent no body bytes, whatever the protocol. For HTTP/1.1 that includes no `Content-Length` and no `Transfer-Encoding`, `Content-Length: 0`, and a chunked body that holds only the terminating zero-length chunk. For HTTP/2 and HTTP/3 it means the stream ended with the request headers, or the DATA frames held zero bytes, with or without a `content-length` field. A bodiless GET or HEAD built with `OutboundRequest::from_request` MUST forward successfully on all four adapters, through `HttpClient::send`, through EZ-6's begin call, and through EZ-9's redirect following. The upstream request MUST carry no body bytes and no `Transfer-Encoding`, and SHOULD NOT carry `Content-Length: 0`.
- **EZ-1.2** Where the host already knows the inbound body is empty, EdgeZero MUST NOT read it. A bodiless forward on that path can never fail with the inbound read deadline, and `RequestContext::take_body` and `into_request` yield an empty buffered body. These levels go in the capability matrix:
  - Axum. Native.
  - Cloudflare. Native when workerd reports no request body, and BestEffort if acceptance criterion 3 shows it does not. **Unverified** on workerd. Treating a request that reports no body as empty MUST NOT drop a body in a runtime that reports no body stream while still carrying bytes (the browser test runtime case behind commit 86f6e834).
  - Fastly. BestEffort. It depends on whether the host reports a zero length for HTTP/1.1, HTTP/2 and, where the service serves it, HTTP/3 bodiless requests. **Unverified** on hosted Fastly (acceptance criterion 2).
  - Spin. Unsupported, except for an explicit `Content-Length: 0`.

  On every other path EZ-1.4 and EZ-1.5 apply.

- **EZ-1.3** EdgeZero MUST NOT decide emptiness from request headers alone. A GET whose headers declare no body, but which carries data (for example HTTP/2 DATA frames with no `content-length`), MUST be treated as carrying a body. The framing EdgeZero reports to the application's admission policy (`IngressHead::framing()`) MUST stay `IngressFraming::HostManaged`. Learning emptiness is not raw framing validation.
- **EZ-1.4** When the adapter does not know whether a forwarded GET/HEAD body is empty, EdgeZero MUST resolve it before sending, reading no more than it takes to see the first body byte or the end of the body. Zero-length chunks do not count as body bytes. An empty body makes the forward bodiless (EZ-1.1). A body with bytes makes the forward fail as in EZ-1.8. An error while reading the inbound body MUST surface unchanged. An expired inbound read deadline stays 408, and a failed host read stays in the existing internal-error category.
- **EZ-1.5** The time spent resolving the body MUST count against the outbound request's own timeout or deadline. If the client neither sends a byte nor ends the body, the forward MUST fail no later than the first of two limits. The outbound budget fails with the gateway-timeout category (504). The inbound read deadline fails with 408. This holds for single send, the begin call (EZ-6), and redirect following (EZ-9). Axum, Cloudflare and Spin MUST enforce the limit while waiting. Fastly MAY observe it only after a blocked host read returns. That is BestEffort, the same limitation as F's `inbound-read-deadlines` cell (`capabilities.md:74`, `:78-83`).
- **EZ-1.6** Only GET and HEAD get this treatment. Other methods, and GET/HEAD requests whose body is already buffered, MUST behave as at F and MUST NOT cause an extra read. The single unsupported-option rejection (cross-cutting requirement 2) MUST come before any resolving, so a GET/HEAD carrying an option the adapter or send mode marks Unsupported is rejected without its body being read. EdgeZero MAY resolve emptiness before its other outbound preflight checks. If it does, a GET/HEAD with a streamed body and some other invalid setting may wait for the inbound body before that error comes back, and the guide MUST say so.
- **EZ-1.7** Batches keep EZ-6's rule against streamed request bodies. A forwarded bodiless GET/HEAD MUST be accepted into a batch after the application has read the inbound body with `RequestContext::body_bytes`. It SHOULD be accepted without that step wherever EZ-1.2 holds.
- **EZ-1.8** A forwarded GET/HEAD that carries body bytes MUST fail with the same 400 F returns today for a GET with a non-empty buffered body (`outbound.rs:1564-1567`). It MUST fail before any backend or connection is set up, and no body byte may reach the upstream.
- **EZ-1.9** The existing `RequestContext` body semantics MUST NOT change. `body_kind()` reports the body as not yet read until it is read or taken. `body_bytes` on a bodiless request returns empty bytes and caches them. A body that was already read forwards its cached bytes and is not read again. A body whose read failed forwards the same error (`context.rs:381-394`). A body that was taken forwards empty.
- **EZ-1.10** Adapters MUST NOT frame an outbound GET/HEAD with an empty body as chunked. That means no `Transfer-Encoding` and no zero-length chunk terminator. For other methods with an empty buffered body, adapters SHOULD send no chunked framing either.
- **EZ-1.11** A forwarded OPTIONS, DELETE, POST, PUT or PATCH built with `OutboundRequest::from_request` MUST succeed through `HttpClient::send` on all four adapters, with the inbound body streamed upstream byte for byte. That includes a bodiless one, such as a CORS preflight OPTIONS with no `Content-Length` on Spin. Where EZ-1.2 holds, a bodiless one SHOULD go out with no body framing. Elsewhere an adapter MAY send it as a zero-length chunked body. EZ-6's begin call rejects these streamed uploads before sending, so TS uses `send` for them.
- **EZ-1.12** Redirect following resolves an inbound GET/HEAD body once per chain (EZ-9.7).
- **EZ-1.13** These invariants MUST hold. This requirement owns the rule on unread bytes, and EZ-10.11 refers to it.
  - A GET/HEAD body is never forwarded.
  - Outbound framing comes only from the body EdgeZero actually sends, never from inbound framing headers.
  - On connections EdgeZero owns (Axum HTTP/1), request bytes left unread after any rejection, including EZ-1.8's 400 and EZ-10's 413, are never parsed as a new request. Closing the connection is acceptable.
  - Polling the body of a GET that sent `Expect: 100-continue` may cause a `100 Continue` before the 400 (`dep:hyper-1.12.0/src/proto/h1/conn.rs:411-416`). That is acceptable.
- **EZ-1.14** EZ-25.1 owns the change to the `docs/guide/proxying.md` forwarding example. The example MUST forward a bodiless GET, a HEAD and a POST with a body correctly and MUST compile as a doc test. The capability guide MUST carry the EZ-1.2 levels and note that a late forward on a read path can fail with 408.

#### Acceptance criteria

1. Environment is Axum, Viceroy 0.21.0, workerd and Spin 4.1. Given a route that forwards the inbound request with `from_request` and `send` to an origin that records what it receives, when a client sends a bodiless HTTP/1.1 GET, then the client gets the origin's status and the origin saw a GET with zero body bytes and no `Transfer-Encoding`. Record whether `Content-Length: 0` appeared. The same holds for HEAD, and for a GET whose chunked body holds only the terminator.
2. Environment is hosted Fastly, Trusted Server staging. Repeat criterion 1 with HTTP/1.1 and HTTP/2 clients that send no `content-length`. Repeat it with an HTTP/3 client where the staging service has HTTP/3 enabled. Whether a Fastly Compute service serves HTTP/3 to clients is **Unverified**. Where it does not, the record states that HTTP/3 was not available and marks the HTTP/3 leg not applicable. Record per protocol whether a bodiless forward still succeeds after the inbound read deadline has passed, which shows whether EZ-1.2 held.
3. Environment is workerd. The run first records whether workerd reports no request body for a bodiless GET. Where it does, a handler that takes the body of a bodiless GET observes an empty buffered body. Where it does not, the run records that, and Cloudflare's EZ-1.2 level is BestEffort.
4. Environment is Axum, Viceroy 0.21.0, workerd and Spin 4.1. When a client sends a GET with `Content-Length: 5` and five bytes, then the client gets the EZ-1.8 400 and the origin records nothing. On workerd, record whether the body reached the guest at all. On Axum, a request pipelined after it is either refused by a connection close or answered from the correct offset.
5. Environment is hosted Fastly, Trusted Server staging. An HTTP/2 GET with a DATA frame and no `content-length` gets the EZ-1.8 400, and the origin records nothing.
6. Environment is Axum. Given an outbound timeout of 200 ms, when a client sends a chunked GET and never sends a chunk, then the forward fails with 504 within 205 ms. With an inbound read deadline of 100 ms, it fails with 408 within 105 ms. On hosted Fastly (Trusted Server staging) the same scenario records the observed outcome and time as BestEffort.
7. Environment is unit. A GET whose streamed body yields only zero-length chunks and then ends is sent bodiless. A GET with a streamed body and an already expired deadline fails with 504, and the body is not read. A POST with a streamed body, and a GET with an empty buffered body, read nothing extra. A taken body forwards empty. A body whose read failed forwards the same error.
8. Environment is Axum and workerd. A bodiless GET forwarded after the inbound read deadline has passed still succeeds (EZ-1.2). On workerd this holds where criterion 3 showed that workerd reports no request body. Otherwise the run records the outcome, and Cloudflare is BestEffort. On Spin 4.1 it succeeds when the request had `Content-Length: 0`.
9. Environment is Axum, Viceroy 0.21.0, workerd and Spin 4.1. A forwarded POST with a 3-byte body arrives at the origin with exactly those 3 bytes. A forwarded bodiless OPTIONS and a bodiless DELETE with no `Content-Length` succeed, and the origin sees zero body bytes. Record whether chunked framing was used.
10. Environment is Axum, Viceroy 0.21.0, workerd and Spin 4.1, once EZ-6 is present. A bodiless navigation GET begun with EZ-6's begin call reaches the origin with no body, under EZ-6's rule for when a begun request is on its way. A begun POST with a streamed body is rejected before anything is sent.
11. Environment is Axum and Viceroy 0.21.0, once EZ-9 is present. A forwarded bodiless GET whose origin answers 307 to a second URL makes two requests, both bodiless, and the inbound body is read at most once.
12. Environment is Axum, Viceroy 0.21.0, workerd and Spin 4.1. After the handler calls `RequestContext::body_bytes` on a bodiless GET, the forwarded request is accepted into a batch.
13. Trusted Server end to end. `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test integration -- --include-ignored` passes `test_wordpress_fastly`, `test_nextjs_fastly`, `test_wordpress_cloudflare`, `test_nextjs_cloudflare`, `test_wordpress_axum` and `test_nextjs_axum` (`ts:crates/trusted-server-integration-tests/tests/integration.rs:137-167`, `:272-285`) with TS forwarding through `from_request` and no TS-side body handling. Those tests are ignored by default, so the flag is required, as TS CI passes it (`ts:.github/workflows/integration-tests.yml:106`, `:148`). New TS cases pass for `HEAD /` (origin status, no body), a forwarded `OPTIONS /` (origin status), and `GET /` with a body (400). TS's in-process parity suite does not cover adapter ingress, so it is not enough on its own.
14. Environment is Axum, Viceroy 0.21.0, workerd and Spin 4.1. A GET carrying an option the adapter marks Unsupported, with a streamed inbound body whose client never sends a byte, is rejected at once with the unsupported-option error, and the forward does not wait for the body.

#### Out of scope

- Forwarding a GET or HEAD body.
- Raw framing validation. F's `raw-ingress-framing-validation` capability stays as it is.
- Resolving emptiness for methods other than GET and HEAD, beyond what EZ-1.11 asks.
- Changing the batch rule on streamed bodies. EZ-6 owns that rule.

#### Open questions

1. Should a bodiless OPTIONS or DELETE that the adapter cannot recognize as empty without reading be sent without chunked framing, which means reading before sending? TS prefers yes only if hosted Fastly turns out not to report a zero length for HTTP/2 and HTTP/3 bodiless requests (acceptance criterion 2). TS forwards CORS preflight OPTIONS, and some origins handle a chunked OPTIONS badly. Otherwise TS prefers no.

### EZ-2. Fastly connection reuse across requests

**Priority:** Blocking for EZ-2.1 to EZ-2.3, EZ-2.6 to EZ-2.10, and EZ-2.13 to EZ-2.17. Follow-up for EZ-2.4, EZ-2.5, EZ-2.11, EZ-2.12, and EZ-2.18, which apply to reusable-sandbox mode and are required before Trusted Server enables it. Trusted Server production runs single-request sandboxes (`ts:crates/trusted-server-adapter-fastly/Cargo.toml:13-18`).
**Adapters:** Fastly (the behavior that changes). Axum, Cloudflare, Spin (client precedence only; Axum keeps its existing cross-request pooling).
**Related:** EZ-3 (removing the dispatch slack depends on EZ-2.6). EZ-4 (connect timeout is part of reuse eligibility). EZ-6 (begun requests and batch observation inherit EZ-2; EZ-6.16 owns a slot whose deadline is the cutoff). EZ-8 (Image Optimizer exception to EZ-2.6). EZ-9 and EZ-15 (count toward the limit). EZ-11 and EZ-12 (settings that affect eligibility). EZ-17 (owns the helpers' shape). EZ-Q1 (gates these criteria).

#### Need

Trusted Server registers Fastly dynamic backends itself today, and their names repeat across requests and sandboxes. The name covers scheme, host, port, certificate check, Host override, a per-provider discriminator, and the first-byte and between-bytes timers, and no counter or request value (`ts:crates/trusted-server-core/src/platform/backend_naming.rs:304-339`, `:352-421`). TS floors remaining budgets onto a ladder so timer values repeat (`:18-22`, `:482-502`), and canonicalizes the mediator's remaining time so "the backend name remains stable across equivalent budget values" (`ts:crates/trusted-server-core/src/auction/orchestrator.rs:958-966`). Registration treats `NameInUse` as reuse (`ts:crates/trusted-server-adapter-fastly/src/backend.rs:256-273`). Config validation budgets provider backends against Fastly's limit of 200, reserving 40 for other traffic (`ts:crates/trusted-server-core/src/platform/backend_naming.rs:23-24`, `:105-111`; `ts:crates/trusted-server-core/src/auction/plan.rs:434-447`). Because a fresh single-request sandbox presents the same backend settings as the last one, Fastly can pool the TLS connections to the publisher origin and to each bidder across sandboxes on a node.

After the cut TS deletes that layer. It sends origin, integration, and mediator calls with `HttpClient::send`, and bidder calls with `HttpClient::start_batch_until`, each request carrying `OutboundRequest::timeout` and the batch carrying a cutoff at auction start plus the auction timeout (2000 ms in the example config, `ts:trusted-server.example.toml:282`). Provider timeouts default to the auction timeout for the standard profile, 1000 ms for `prebid-server`, and 800 ms for APS (`ts:crates/trusted-server-core/src/auction/profile.rs:170-186`). Batch results carry their input index (`edgezero:crates/edgezero-core/src/outbound.rs:121-128`), so TS no longer needs per-provider backends, and two providers on one host may share connections.

What regresses at F:

- In a single-request sandbox, the TS production mode, reuse across sandboxes happens only when the effective budget repeats to the millisecond. The mediator, any provider launched with less than its timeout left (`ts:crates/trusted-server-core/src/auction/orchestrator.rs:453-462`), and any origin fetch bounded by a request deadline lose it. Each of those requests pays a new TCP and TLS handshake and a new backend registration.
- In a reused sandbox (TS feature `reusable-sandbox`, serving loop `ts:crates/trusted-server-adapter-fastly/src/main.rs:123-150`) no request reuses a connection an earlier request opened, and each request adds registrations the sandbox holds until it ends. A sandbox that serves 10 auctions with 15 bidder hosts holds more than 150, close to the platform limit of 200.
- The handshake cost on Fastly for TS's bidders has not been measured. Staging measurements M1 and M2 below measure it.

#### Gap at F

- Each `FastlyOutboundClient` takes a new value from a process-global counter and appends it to every backend name (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:35-36`, `:57-65`, `:307-317`). The canonical lifecycle builds a new client for every request (`edgezero:crates/edgezero-adapter-fastly/src/request.rs:446`, `:707-734`). The counter restarts with each wasm instance, so a reused sandbox gives each request a fresh suffix.
- Backend identity includes the effective budget rounded up to the millisecond (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:254-262`, `:664-685`, `:762-765`), and that budget is `deadline - now` at nanosecond precision (`edgezero:crates/edgezero-core/src/time.rs:138-187`). Any deadline- or cutoff-bound budget therefore gives a new backend per request.
- The backend cache lives in the client and is discarded with it (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:290-295`, `:319-377`).
- Platform timers are connect at a quarter of the budget, first byte at the rest, and between bytes at the whole budget (`:718-733`). The wait for response headers, batch observation, and body reads have no guest-side limit (`:443-453`, `:1279-1290`, `:784-825`, `:1203-1238`). Viceroy 0.21.0 enforces the first-byte timer, which there covers connect plus the response head, and the between-bytes timer, and does not enforce connect timeouts (`viceroy:src/wiggle_abi/req_impl.rs:617-644`; `viceroy:src/upstream.rs:351-375`, `:412-414`). It reports a timer expiry as a generic host error (`viceroy:src/error.rs:327-329`), which the SDK reports as `SendErrorCause::InternalError` (`fastly-sdk:src/http/request.rs:3353-3364`) and F maps to `EdgeError::Internal` when the deadline has not passed (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:73-75`, `:95-97`, `:959`). With F's three-quarter first-byte share, a slow response head is cut off before the budget, as a 500.
- The outbound design states that a newly constructed client does not intentionally reuse a previous client's connection and that the client must not outlive its request context (`edgezero:docs/superpowers/specs/2026-05-21-outbound-http-design.md:5139-5146`, `:5341-5352`). It assumes each request starts with an empty backend namespace (`:5413-5466`).
- The platform does not match that model. Viceroy creates its backend table once per sandbox and keeps it across the up to six requests a reused guest serves (`viceroy:src/sandbox.rs:54`, `:106-109`, `:170`), rejects any repeated name (`:698-707`), and builds a new HTTP client for every send, so it never pools connections (`viceroy:src/upstream.rs:356-368`). The SDK pools only for the same backend name with exactly the same settings, across sessions (`fastly-sdk:src/backend/builder.rs:281-294`). Fastly's documentation, fetched 2026-10-17 through a summarizing fetch tool and to be re-checked against the live pages, says "Compute registers dynamic backends at the node level." (Fastly's developer guide to backends, under Integrations and Non-Fastly services). It says a registration at the limit "blocks execution" until "another sandbox of the same service terminates and releases its backends" (Fastly's Compute errors reference). It lists "Maximum number of dynamic backends | 200 | per service" (Fastly's Compute resource limits page).
- Axum already pools across requests through one shared transport (`edgezero:crates/edgezero-adapter-axum/src/service.rs:73`; `edgezero:crates/edgezero-adapter-axum/src/request.rs:68-74`).

#### Requirements

Reuse (blocking):

- **EZ-2.1** On Fastly, two outbound requests MUST be eligible to share a pooled connection when they target the same scheme, host, and port, with the same Host authority, the same TLS settings, and the same connect timeout (EZ-4), subject only to EZ-2.2. This MUST hold for two requests made while serving one inbound request, and for requests made by different single-request sandboxes of one service wherever Fastly pools across sandboxes. It MUST hold regardless of path, query, method, headers, body, cache policy, response mode, and send mode.
- **EZ-2.2** Reuse eligibility MUST NOT depend on the exact remaining budget. Requests with equal per-call timeouts MUST always be eligible with each other, even when a batch cutoff or caller deadline makes their effective budgets differ. For requests bounded only by a deadline or cutoff, the number of distinct backend configurations per origin MUST stay within a small documented bound however many requests are served. Requests whose budgets differ only by scheduling jitter MUST be eligible with each other except at documented boundaries.
- **EZ-2.3** Across sandboxes, requests with equal origin settings MUST be poolable wherever Fastly pools across sandboxes. Nothing request-, sandbox-, client-, or deployment-specific may prevent that within one EdgeZero version. When two EdgeZero versions run side by side during a rolling deploy, neither MAY fail a request because the other registered the same origin with different settings.

Reuse in a reused sandbox (follow-up):

- **EZ-2.4** In a reused sandbox, reuse eligibility MUST NOT depend on which client instance sends. The adapter's per-request default client, a client the application keeps for the sandbox, and a `FastlyOutboundClient::new()` client used after delivery inside the lifecycle callback (cross-cutting requirement 4) MUST all reuse the same connections.
- **EZ-2.5** In a reused sandbox the number of backend registrations MUST grow only with the number of distinct origin configurations used, not with the number of requests served.

Deadlines (blocking):

- **EZ-2.6** The per-call deadline MUST still be exact. On Fastly, on a backend used before and on a backend registered for this request, waiting for response headers and each response-body read MUST end no later than the request's deadline plus 5 ms, failing with `EdgeError::GatewayTimeout` (504) and the `BudgetSource` that selected the deadline. This MUST hold under Viceroy 0.21.0 and on hosted Fastly. The only exceptions are operations the host cannot interrupt. Those are first-time backend registration, writing a request body, reading the inbound body to settle a forwarded GET/HEAD (EZ-1.5), the Image Optimizer send of EZ-8, and a response-body read if the host cannot report body readiness (Open question 6). An overrun in one of those MUST be reported as the same 504 when the operation returns, and the capability footnote MUST name each exception that applies.
- **EZ-2.7** A platform-side timer MUST NOT fail a request before its own deadline for any budget up to Fastly's 2-minute runtime limit. A platform-side timer that fires at or after the deadline MUST be reported as the EZ-2.6 504 with the deadline's `BudgetSource`, never as an internal error. This holds under Viceroy 0.21.0 and on hosted Fastly.
- **EZ-2.8** In a batch, a slot whose own deadline passes before the observation cutoff MUST yield its 504 item at that deadline, without waiting for a sibling to finish or for a platform timer. A slot whose deadline is the cutoff follows EZ-6.16. The behavior when the observation cutoff passes is otherwise unchanged from F (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:621-651`).

Caching and safety (blocking):

- **EZ-2.9** A single send and a one-slot batch of the same request MUST be indistinguishable in caching, validation, and errors. Cross-cutting requirement 1 owns the rule for the other send modes. Image Optimizer requests are governed by EZ-8.
- **EZ-2.10** EdgeZero MUST NOT send a request over a connection whose settings differ from what the request asks for (TLS settings, Host authority, connect timeout), or whose platform timers could end it before its deadline (EZ-2.7). If it cannot get such a connection, the request MUST fail before sending with a 500-class internal error, and the reason MUST be logged and kept off the wire.

Backend limit and retirement (follow-up):

- **EZ-2.11** One reused sandbox MUST NOT block without end on its own registrations. When the sandbox already holds 200 distinct backend configurations and a request needs another, that request MUST fail promptly with `EdgeError::ServiceUnavailable` (503) and nothing sent, and the sandbox MUST be retired after the current request. A registration that returns after the deadline follows EZ-3.4.
- **EZ-2.12** An application serving through the reusable Fastly lifecycle (`edgezero_adapter_fastly::lifecycle::serve_custom`, `edgezero_adapter_fastly::serve_app`, `edgezero_adapter_fastly::serve_app_with_hooks`) MUST be able to set a threshold of distinct backend configurations at which the sandbox stops being reused, with a documented default, and MUST be able to retire the sandbox from its own callback. Retirement takes effect after the current request completes. It MUST NOT produce an SDK-generated error response. The serving result MUST tell retirement apart from an error, and MUST keep reporting the request count that TS logs today (`ts:crates/trusted-server-adapter-fastly/src/main.rs:144-149`). A client the application keeps for the sandbox MUST NOT carry any request's in-flight exchange into the next request.

Observation (blocking):

- **EZ-2.13** An application MUST be able to read, for the inbound request it is serving, the number of backend registrations EdgeZero performed, the number of distinct backend configurations it used, and the total and longest registration time. A registration that takes 50 ms or more MUST be logged at `warn`. None of this reaches the wire.

Client supply (blocking):

- **EZ-2.14** EdgeZero MUST document and keep this precedence for the `HttpClient` a handler receives from `RequestContext::http_client` (`edgezero:crates/edgezero-core/src/context.rs:372-374`). A client registered with `RouterBuilder::with_state` wins (`edgezero:crates/edgezero-core/src/router.rs:348-354`, `:407-411`). On Fastly, a client inserted through the `prepare` hook of `send_request_with_registries_and_hooks` comes next (`edgezero:crates/edgezero-adapter-fastly/src/request.rs:446-448`). The adapter default comes last. The router-state rule MUST hold on all four adapters. A client inserted through `prepare` MUST keep reaching the handler. EZ-17 owns the helpers' shape.

Invariants and documentation (blocking):

- **EZ-2.15** Axum MUST keep its cross-request pooling. Cloudflare and Spin keep their platform-managed connections. No new capability row is required for pooling, which no adapter can promise.
- **EZ-2.16** EdgeZero MUST NOT retain paths, queries, headers, or bodies across requests. In a reused sandbox it MAY retain origin host names and backend configuration. Its guide MUST tell applications not to let untrusted input choose outbound hosts, because every distinct origin consumes service-wide backend capacity.
- **EZ-2.17** EdgeZero's Fastly guide and capability matrix MUST describe the reuse behavior, the deadline guarantee of EZ-2.6 and its exceptions (Fastly `outbound-deadlines` stays `BestEffort`), the limit and retirement behavior once EZ-2.11 and EZ-2.12 land, and the observation of EZ-2.13. The guide MUST record that the outbound design's dynamic-backend model is superseded, without editing `docs/superpowers/specs/`. The CHANGELOG MUST list each behavior change.

Per-sandbox accounting (follow-up):

- **EZ-2.18** In a reused sandbox, an application MUST be able to read the number of distinct backend configurations the sandbox holds and the number of registrations it performed since it started.

#### Acceptance criteria

Blocking criteria come first. Viceroy 0.21.0 never pools connections, so reuse is visible there only as registration counts.

Unit (any host):

1. Given a client in router state, another inserted through Fastly `prepare`, and the adapter default, when a handler calls `RequestContext::http_client`, then it receives the router-state client. Given only the `prepare` client and the default, it receives the `prepare` client. The router-state case holds on Axum, Cloudflare, and Spin against their defaults.

Viceroy 0.21.0, one inbound request per case:

2. Given one inbound request that runs two batches to one loopback origin with per-call timeouts of 800 ms and cutoffs leaving 640 ms and 655 ms, when both complete, then the EZ-2.13 observation reports one registration and one distinct configuration.
3. Given one inbound request whose sends differ only in scheme, port, Host authority, TLS settings, or connect timeout, then the observation reports one distinct configuration per variant.
4. Given a loopback origin that delays its response head by 500 ms and a request with a 100 ms timeout, when sent, then it fails with 504 and `BudgetSource::PerCallTimeout` between 100 and 105 ms after dispatch. (At F, by reading the code, Viceroy's 75 ms first-byte timer fires first and the request fails with an internal error at about 75 ms.)
5. Given an origin that sends its head at once and then one body byte every 300 ms, and a buffered request with a 500 ms timeout, then it fails with 504 between 500 and 505 ms. Where EdgeZero documents that the host cannot report body readiness, it fails at the first read that returns after 500 ms instead. The run records which.
6. Given a batch with a 1 s cutoff, one slot to an origin that never answers with a 200 ms timeout, and two fast slots, then the silent slot's 504 item arrives between 200 and 205 ms and the batch completes.

Hosted Fastly, single-request mode. Who produces each result follows EZ-Q1.4. EdgeZero produces criteria 1 to 6. Criteria 11, 12, 13b and 14 need settings Trusted Server's configuration cannot express (a controllable slow origin, a 90 s timeout, a one-slot batch to a test origin, a second EdgeZero commit), so EdgeZero provides a qualification build at the pinned commit and TS deploys it to a TS staging service. TS produces criteria 7 to 10 and 13a with its own builds, and criterion 15. The baseline is a Trusted Server build of `main` at `a9037770a` with TS PR #1123, which registers its own stable backends. The candidate is TS's cut build at the EdgeZero commit TS pins. Criteria 7 to 10 replay a fixed page-view mix at a fixed rate with build order randomized, and report p50, p95, p99, and sample counts per POP. A bound measured inside the guest from dispatch or a deadline keeps its stated tolerance on hosted Fastly. The thresholds in criteria 7 to 10 are Trusted Server's proposed product thresholds (Open question 8).

7. **M1, handshakes per bidder per auction.** TS-controlled staging endpoints for one provider and the origin log new TCP connections and full versus resumed TLS handshakes. After a 5-minute warm-up, the candidate's new handshakes per request to the origin and to that provider MUST NOT exceed the baseline's by more than 10 percent.
8. **M2, latency.** Per bidder, the candidate's p95 slot latency and p95 response-head latency MUST each be no more than 10 ms above the baseline's.
9. **M3, registration.** In the candidate, the EZ-2.13 observation for each inbound request MUST show no more registrations than the distinct origin configurations that request used, The total registration time per inbound request is recorded with its distribution and serves as EZ-3's reference. It has no pass condition, because the pre-cut build does not time its own registrations and the platform sets that latency.
10. **M5, pool exhaustion.** At peak replay rate, the candidate's gateway failures, as a share of that origin's requests, MUST be no more than 10 percent above the baseline's share or 0.1 percentage points above it, whichever is larger. Where EZ-16 has landed, the same bound applies to failures whose provider cause is a connection limit.
11. **M6, deadline in production.** Criteria 4 and 5 pass against a staging origin with the same delays, with the windows they state.
12. **Long budgets.** A request with a 90 s per-call timeout to an origin that sends its head at 80 s succeeds (EZ-2.7).
13. **Rolling deploy.**
    - 13a. While the baseline and the candidate are active together during a staged rollout to one TS staging service, both serve the same origins, the candidate reports zero EZ-2.10 internal errors, and neither build's requests fail at registration.
    - 13b. When the pinned commit changes backend configuration relative to the EdgeZero commit of the previous passing record, the qualification builds of the two commits active together on one staging service both serve the same origins with zero registration failures. The first record has no previous commit, and 13a alone applies.
14. **Caching parity.** A cacheable GET sent twice with `send`, and twice as a one-slot batch, produces the same number of origin receipts in each mode (one per pair), and the second response of each pair is served from cache in both modes. This runs on hosted Fastly only, because Viceroy's plain send path does not cache (`viceroy:src/wiggle_abi/req_impl.rs:946-977`).

Trusted Server end-to-end check (Viceroy 0.21.0, single-request mode):

15. One auction with three bidder hosts, two providers configured on one of them, and one origin fetch reports four registrations for the inbound request.

Follow-up criteria (reusable-sandbox mode). Viceroy 0.21.0 serves at most six requests per reused guest. Cases 16 and 17 stay on one guest. Cases 18 and 19 expect a new guest.

16. Given a reused guest and three sequential requests to the same loopback origin, the EZ-2.18 accounting reports one registration. A fourth request to a second loopback port raises it by one. (At F it reports three after the first three requests.)
17. Given one reused guest where a request on the adapter default client, a request on a client kept for the sandbox, and a `FastlyOutboundClient::new()` send after delivery all target one origin, the accounting reports one registration.
18. Given a reused guest whose first requests send to 200 distinct loopback ports, when a later request needs a 201st, then it fails with 503 within 5 ms and nothing is sent, and the next request is served by a new guest.
19. Given a retirement threshold of 2, when two requests use two loopback ports, then the third request is served by a new guest. Given a callback that asks for retirement on its first request, a new guest serves the second request. In both cases the serving result reports retirement, no error, and the request count served.
20. **M4, limit model, hosted Fastly in reuse mode.** Report distinct backends against sandbox ordinal, retirements, 503s from EZ-2.11, and registrations of 50 ms or more. Then run K concurrent sandboxes with the same N origins, and again with disjoint origins, to learn whether identical configurations in different sandboxes share capacity and whether the 200 applies per node or per service. M1 is repeated in reuse mode, and the candidate's new handshakes per request after each sandbox's first 5 requests MUST NOT exceed the baseline's.
21. Trusted Server end to end, Viceroy 0.21.0 in reuse mode. Two sequential auctions with three bidder hosts and one origin register four configurations in total, and the second auction registers none.

#### Out of scope

- Guaranteeing connection reuse where Fastly does not pool, or across services.
- Per-request control of pool size, keepalive, or connection lifetime.
- Bounding first-time registration or request-body writes in time (EZ-2.6 covers only how overruns are reported).
- Exact deadlines for Image Optimizer requests (EZ-8).
- A capability row for pooling.

#### Open questions

1. **Default retirement threshold (EZ-2.12).** TS prefers 100 distinct backends until M4 reports the capacity model. If identical configurations share capacity across sandboxes, the default can rise toward 160. If not, it should fall, and the guide should say reuse suits only applications with few distinct origins.
2. **Overrun during uninterruptible operations.** How far past its deadline can a deadline-only request run during an operation EdgeZero cannot interrupt (EZ-2.6 exceptions)? TS prefers at most twice its budget.
3. **Operator visibility.** Should operators be able to tell from Fastly's own tooling which origin host a registered backend serves? TS prefers yes. It makes M3 and M4 attributable, and origin hosts are not secret.
4. Removed.
5. **Per-backend connection cap.** Fastly's SDK defaults each backend pool to 200 connections (`fastly-sdk:src/backend/builder.rs:132`). Sharing connections across requests makes that cap reachable. TS prefers keeping the default and revisiting it if M5 shows connection-limit failures.
6. **Body readiness.** Can EdgeZero on Fastly SDK 0.13.1 wait for response-body data with a timeout? EZ-2.6 and EZ-6.21 both depend on the answer. TS expects yes. The SDK's async I/O interface lists normal bodies as objects whose readiness can be awaited with a timeout (`fastly-sys:src/service0_1_0.rs:317-325`, `:460-471`), and Viceroy 0.21.0 implements timed waits over body items (`viceroy:src/async_io.rs:14-46`; `viceroy:src/sandbox/async_item.rs:154`). Hosted behavior is **Unverified** until M6 runs.
7. **Proving EZ-2.10.** How will EdgeZero show that a request is never sent over a connection with the wrong settings, given that the clash is not observable from outside except through criterion 13? TS prefers that EdgeZero's own tests cover it and that TS's hosted runs report zero EZ-2.10 errors.
8. **Staging thresholds for TS to confirm.** The proposed product thresholds are M1 handshakes at most 10 percent above the pre-cut build, M2 p95 slot and response-head latency at most 10 ms above it, M3 registrations per inbound request no more than its distinct origin configurations, and M5 gateway-failure share at most 10 percent or 0.1 percentage points above it, whichever is larger. TS prefers these because they allow measurement noise while still catching a build that loses pooling or adds a registration round trip to every request.

#### Interim for Trusted Server (follow-ups only)

Until EZ-2.4, EZ-2.5, EZ-2.11, EZ-2.12, and EZ-2.18 land, Trusted Server keeps `reusable-sandbox` off in production, as it is today (`ts:crates/trusted-server-adapter-fastly/Cargo.toml:13-18`), and serves single-request sandboxes.

### EZ-3. Cold backend registration never fails a request on its own

**Priority:** Blocking
**Adapters:** Fastly for the slack. EZ-3.3 holds on all four adapters. Axum, Cloudflare, and Spin have no slack and otherwise do not change.
**Related:** EZ-2 (EZ-2.6 must hold before the slack is removed, so both ship together). EZ-1 (the 408 of EZ-1.5). EZ-6 (begun requests and non-blocking batch observation follow the same rule). EZ-Q1 (gates these criteria).

#### Need

Trusted Server has no dispatch slack today. Its auction launches each provider and then collects completions, turning late ones into timeouts where its deadline policy says so (`ts:crates/trusted-server-core/src/auction/orchestrator.rs:440-613`, `:558-570`). After the cut every bidder goes through one `start_batch_until`. In a single-request sandbox no backend is registered yet, so the batch performs one first-time registration per bidder, one after another. F charges all of them, plus each earlier slot's preparation and send, against one 25 ms allowance measured from batch entry. A 12-bidder auction in which each registration costs a few milliseconds (**Unverified**, measured by M3) exhausts it partway through the list. The remaining slots fail with an internal error while more than 1900 ms of a 2000 ms auction remain. In TS's post-cut design an internal error is a transport error, so those bids are lost without a timeout signal.

Single sends are exposed too. DataDome protection, off by default (`ts:trusted-server.example.toml:599`), runs with a 1500 ms default budget (`ts:crates/trusted-server-core/src/integrations/datadome.rs:284-286`) and fails open on error (`ts:trusted-server.example.toml:598`). If its registration takes more than 25 ms, F returns an internal error and TS skips bot protection for that request.

#### Gap at F

- `BATCH_DISPATCH_SLACK_MAX` is 25 ms (`edgezero:crates/edgezero-core/src/time.rs:8-9`), exported publicly (`edgezero:crates/edgezero-core/src/lib.rs:112-115`).
- Before every send, and before each first-time registration in a batch, F fails the request with `EdgeError::Internal` if more than 25 ms passed since the call began, even when the deadline is far away (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:1338-1357`, `:449`, `:521`, `:1179`, `:767-780`). In a batch the 25 ms is shared by all slots (`:576`, `:585-588`). For a single send it includes the registration (`:565-566`).
- A public test hook exists only to exercise this (`:224-252`, `:2531-2532`), and a Viceroy test asserts the internal error (`edgezero:crates/edgezero-adapter-fastly/tests/contract.rs:1295-1321`).
- The outbound design justifies the slack as protection against platform timers armed with a stale budget, and concedes it detects rather than prevents overruns when a backend is new (`edgezero:docs/superpowers/specs/2026-05-21-outbound-http-design.md:5191-5269`).
- The other adapters size their timers from the budget remaining at dispatch and have no slack (`edgezero:crates/edgezero-adapter-axum/src/outbound.rs:478-484`; `edgezero:crates/edgezero-adapter-cloudflare/src/outbound.rs:636-646`; `edgezero:crates/edgezero-adapter-spin/src/outbound.rs:468-488`).

#### Requirements

- **EZ-3.1** A Fastly request or batch slot MUST NOT fail because EdgeZero's preparation or backend registration took time while the request's deadline was still in the future. The dispatch-slack internal error MUST no longer occur.
- **EZ-3.2** Before a response arrives, the only time-based failures MUST be `EdgeError::GatewayTimeout` (504) with the `BudgetSource` that selected the deadline or the connect-timeout cause of EZ-4.6, and the 408 of EZ-1.5 when the inbound read deadline expires while a forwarded GET/HEAD body is being settled.
- **EZ-3.3** On every adapter, a request whose deadline has passed before it is sent MUST fail with 504 and its `BudgetSource` without being sent, and on Fastly without a new backend registration.
- **EZ-3.4** A backend registration that returns after the deadline MUST fail its request with 504 and the request's `BudgetSource`, and the registered backend MUST remain usable by later requests.
- **EZ-3.5** EZ-2.6 MUST hold with the slack removed.
- **EZ-3.6** In a batch, a slot whose backend was registered before SHOULD NOT wait behind other slots' first-time registrations. Results keep input order, and each slot's elapsed time is still measured from batch entry.
- **EZ-3.7** `BATCH_DISPATCH_SLACK_MAX` and the slack test hook MAY be removed from the public API. Nothing in TS uses them.
- **EZ-3.8** EdgeZero's capability matrix footnote on Fastly deadlines (`edgezero:docs/guide/capabilities.md:260-266`) and the CHANGELOG MUST stop describing the 25 ms bound and MUST describe the EZ-2.6 guarantee instead.

#### Acceptance criteria

Unit (any host). How a test makes time pass inside EdgeZero is EdgeZero's choice.

1. Given a 500 ms budget, when more than 25 ms passes inside the send call before dispatch, then the request is sent and no internal error occurs.
2. Given a batch of 20 slots to 20 new origins whose registrations each take 5 ms, and a 1 s cutoff, then all 20 are sent and none fails with an internal error.
3. Given a batch with a 1 s cutoff where the first slot's registration runs first and takes 20 ms, and the second slot has a 10 ms timeout, then the second slot yields 504 with `BudgetSource::PerCallTimeout`, is not sent, causes no registration, and the batch continues.
4. Given a batch ordered new, known, new, known, then both known-origin slots SHOULD be sent before the first registration, the run records the order, and results come back in input order.

Viceroy 0.21.0:

5. Given a 1 s budget, more than 25 ms passing before sending, and a loopback origin that answers 200, when the request is sent, then it returns 200. (At F it fails with the dispatch-slack internal error.)
6. Trusted Server end to end, single-request mode. A 12-bidder auction against 12 loopback ports completes with no internal-error slots.

Hosted Fastly, single-request mode, with the same baseline and candidate as EZ-2. EdgeZero produces criteria 1 to 5. Trusted Server produces criteria 6 and 7 with its own builds (EZ-Q1.4).

7. **M7, slack failures.** On the candidate, the number of slots and single sends that fail with an internal error before sending, including DataDome calls, MUST be zero across the replay, reported per 1000 auctions by bidder count. The candidate's per-slot 504 rate MUST NOT exceed the baseline's by more than 1 percentage point. Both bounds MUST hold for a 12-bidder auction profile.

#### Out of scope

- Making first-time registration faster or interruptible.
- Bounding request-body writes.

#### Open questions

1. **Cap on first-time registrations per batch.** TS prefers no cap. EZ-3.3 already stops registering for slots whose deadline passed, and EZ-2.11 bounds a reused sandbox's total. A per-batch cap would fail live slots for the same reason the slack did.
2. **M7 threshold for TS to confirm.** The proposed bound on the per-slot 504 rate is 1 percentage point above the pre-cut build. TS prefers it because a larger rise would mean bids lost to timeouts that the pre-cut build received.

### EZ-4. Connect timeout independent of the total budget

**Priority:** Blocking
**Adapters:** All four. Fastly Native. Spin BestEffort. Axum and Cloudflare BestEffort, with the setting accepted and the connect phase bounded by the total budget (cross-cutting requirement 2).
**Related:** EZ-2 (requests that differ in connect timeout do not share connections). EZ-6, EZ-9, EZ-15 (the connect timeout travels with begun requests, redirect hops, and, once EZ-15 lands, fire-and-forget sends). EZ-16 (logs tell a connect timeout from a DNS failure).

#### Need

Trusted Server sets connect to 1 s on every Fastly backend (`ts:crates/trusted-server-adapter-fastly/src/backend.rs:238-242`) and sets a provider's first-byte and between-bytes timers to its whole transport budget (`ts:crates/trusted-server-core/src/auction/plan.rs:385-398`). DataDome protection passes its 1500 ms as both body timers (`ts:crates/trusted-server-core/src/integrations/datadome/protection.rs:238-239`). Publisher origin fetches use a 15 s first-byte timeout (`ts:crates/trusted-server-core/src/publisher.rs:98`). An unreachable origin therefore fails in about 1 s, and bidders get their full budget to answer.

F gives the connection a quarter of the budget and the response head the remaining three quarters. A 1000 ms bidder call gets 250 ms to connect and 750 ms to first byte, so a bidder loses a quarter of its think time even on a reused connection, and a slow TLS handshake fails at 250 ms with most of the budget left. DataDome gets 375 ms and 1125 ms. An origin fetch migrated with a 15 s timeout gets 3.75 s to connect, so an unreachable origin holds the page for 3.75 s where TS fails in 1 s today.

TS needs the response head to get the whole remaining budget, and it needs to keep a 1 s connect bound on origin, integration, and bidder requests.

#### Gap at F

- `OutboundRequest` has a total timeout and a deadline, and no connect setting (`edgezero:crates/edgezero-core/src/outbound.rs:49-67`, `:71-90`).
- Fastly reserves a quarter of the budget for connect (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:718-733`). A connect timeout is reported with the budget's `BudgetSource`, as if the whole budget were spent (`:77`, `:931-933`). Viceroy 0.21.0 stores no connect timeout for dynamic backends and enforces F's first-byte share, so at F a 1000 ms request whose head arrives at 900 ms fails at about 750 ms with an internal error (`viceroy:src/wiggle_abi/req_impl.rs:617-644`; `viceroy:src/upstream.rs:370-375`).
- Spin sets connect, first byte, and between bytes to the remaining budget (`edgezero:crates/edgezero-adapter-spin/src/outbound.rs:67-74`, `:468-488`). Only a `NotSupported` refusal is ignored with a `debug` log. `Immutable` and `Other` refusals fail the request as an internal error (`:490-505`). A connect timeout before the deadline is reported with an unspecified source (`:1341-1357`).
- Axum bounds the whole request with one timeout (`edgezero:crates/edgezero-adapter-axum/src/outbound.rs:478-484`), and reqwest 0.13.4 offers a connect timeout only per client (reqwest documentation for `RequestBuilder` and `ClientBuilder`, fetched 2026-10-17). Cloudflare races the fetch against the total budget (`edgezero:crates/edgezero-adapter-cloudflare/src/outbound.rs:636-646`).
- The capability matrix marks `outbound-flexible-phase-budget` `BestEffort` on Fastly and Spin (`edgezero:docs/guide/capabilities.md:244`, `:281-282`). The outbound design defers a configurable split "pending a real use case" (`edgezero:docs/superpowers/specs/2026-05-21-outbound-http-design.md:7213-7221`). TS is that use case.

#### Requirements

- **EZ-4.1** An application MUST be able to set a connect timeout on an `OutboundRequest`, independently of `OutboundRequest::timeout` and `OutboundRequest::deadline`. The setting MUST survive `OutboundRequest::into_parts` and `from_parts`, and MUST apply on every send mode (cross-cutting requirement 1). A request built with `OutboundRequest::from_request` has none.
- **EZ-4.2** The connect timeout bounds establishing a new connection. Each adapter's documentation MUST say whether DNS resolution and the TLS handshake fall inside it. It MUST NOT extend the total budget. It MUST NOT reduce the time allowed for the response head. It does not apply when the request uses an existing pooled connection.
- **EZ-4.3** Without a connect timeout, no adapter may bound the connect phase more tightly than the total budget. On Fastly the response head MUST get the whole remaining budget. A request that connects in 10 ms and receives its head at 90 percent of its budget MUST succeed.
- **EZ-4.4** With a 1 s connect timeout on Fastly, a request to an unreachable origin MUST fail within 1 s plus 50 ms whatever its total budget. This restores TS's current fast failure.
- **EZ-4.5** A zero connect timeout MUST be rejected before sending with F's standard outbound preflight rejection (400). This deliberately differs from a zero total timeout, which F reports as 504 with `BudgetSource::PerCallTimeout` (`edgezero:crates/edgezero-core/src/time.rs:176-181`), because a zero connect timeout is invalid input rather than an exhausted budget. A connect timeout longer than the total budget MUST behave as if unset.
- **EZ-4.6** When the connect phase times out before the deadline and a connect timeout was set, the request MUST fail with `EdgeError::GatewayTimeout` (504) and a timeout cause the application can tell apart from the per-call timeout, the deadline, and the cutoff. When the deadline has passed, the deadline's `BudgetSource` MUST win. When no connect timeout was set and the platform reports a connect timeout before the deadline, the source MUST be `BudgetSource::Unspecified`.
- **EZ-4.7** In a batch, each slot's connect timeout MUST be independent of the cutoff and of other slots.
- **EZ-4.8** On Fastly the connect timeout MUST be passed to the platform in milliseconds, and a connect failure MUST occur within the timeout plus 50 ms on hosted Fastly (Native). Spin MUST pass it to the host and, for any refusal of the setting, fall back to the total budget without an error (BestEffort). This changes F's handling of `Immutable` and `Other` refusals. Axum and Cloudflare MUST accept the setting without an error, with the connect phase bounded by the total budget (BestEffort). Axum MAY honor it exactly and then report Native.
- **EZ-4.9** EdgeZero's capability matrix MUST gain a row for the connect timeout with the levels of EZ-4.8, and Fastly's `outbound-flexible-phase-budget` MUST become Native. The guides MUST say that connect timeouts should be configuration constants, because on Fastly requests with different connect timeouts do not share connections (EZ-2.1). The CHANGELOG MUST list the removal of Fastly's fixed connect share.
- **EZ-4.10** Removed.
- **EZ-4.11** An application MUST be able to read, from a built request, the connect timeout it carries.

#### Acceptance criteria

Unit (any host):

1. Given a request with a 1 s connect timeout, when it goes through `into_parts` and `from_parts` and through a redirect-hop rebuild, then the connect timeout read back under EZ-4.11 is 1 s.
2. Given a connect timeout of zero, when the request is sent on any adapter, then it fails with 400 before anything is sent.
3. Given each kind of Spin host refusal of the setting, the request completes within its budget and reports no error for the refusal.
4. Each adapter's capability metadata reports the EZ-4.9 levels.

Axum and workerd:

5. Given a request with a 1 ms connect timeout to a loopback origin that accepts at once, the request succeeds (BestEffort).

Spin 4.1:

6. Given a 200 ms connect timeout to a non-routable address and a 2 s budget, the request fails before the budget with the connect-timeout cause, or, if the host ignores the setting, at the budget with the budget's source. The run records which.

Viceroy 0.21.0:

7. Given a loopback origin that accepts at once and sends its head after 900 ms, a request with a 1000 ms timeout and no connect timeout succeeds. (At F it fails at about 750 ms with an internal error.)

Hosted Fastly, single-request mode. Viceroy 0.21.0 does not enforce connect timeouts, so these criteria need hosted Fastly. Criterion 8 targets an address Trusted Server's configuration cannot express, so it runs on EdgeZero's qualification build deployed by TS. TS produces criterion 9 with its own builds, against the EZ-2 baseline (EZ-Q1.4). The bounds in criterion 8 are measured inside the guest and keep their stated tolerance.

8. **M8, connect.** A request to the non-routable `192.0.2.1` with a 200 ms connect timeout and a 2 s total budget fails with 504 and the connect-timeout cause within 250 ms of dispatch. Without a connect timeout it fails with the budget's source between 2000 and 2050 ms after dispatch.
9. **Bidder timeouts.** Across the EZ-2 replay, no bidder slot in the candidate fails with a per-call, deadline, or cutoff timeout before its provider timeout has elapsed. The candidate's connect-timeout failures per integration, as a share of that integration's requests, MUST be no more than 0.1 percentage points above the baseline's connection-failure share for the same integration (Open question 3).

Trusted Server end-to-end check:

10. A TS unit test over its request builders reads, under EZ-4.11, a 1 s connect timeout on every origin, integration, and bidder `OutboundRequest`. No client double is needed.

#### Out of scope

- A DNS timeout setting.
- Separate first-byte or between-bytes settings (EZ-14 covers idle time).
- Exact connect bounds on Axum, Cloudflare, and Spin.

#### Open questions

1. **Default.** TS prefers unset, meaning bounded by the total budget as on Axum and Cloudflare, with TS setting 1 s explicitly. A 1 s default would match Fastly's SDK default (`fastly-sdk:src/backend/builder.rs:153`) and TS today, but would apply on Fastly alone.
2. **Axum exactness.** TS prefers BestEffort. Axum is a development target, and honoring per-request connect timeouts there adds connection pools for little gain.
3. **Criterion 9 threshold for TS to confirm.** The proposed bound is 0.1 percentage points above the pre-cut build's connection-failure share per integration. TS prefers it because the cut keeps TS's 1 s connect bound, so a larger rise would point to a regression rather than to origin behavior.

### EZ-5. Encoded response passthrough

**Priority:** Blocking
**Adapters:** Fastly, Axum, Cloudflare, Spin. All four must behave the same. Spin may be marked Unsupported only if its host decodes responses itself.
**Related:** EZ-6 (a begun request keeps the choice), EZ-9 (every hop keeps the choice), EZ-15 (a fire-and-forget send keeps it once EZ-15 lands), EZ-14 (an idle bound applies to the stream as delivered, encoded or decoded), EZ-25.1 (owns the forwarding example in the guide)

#### Need

Today TS receives upstream bytes still encoded on Fastly and Axum, which are its production and dev targets. The Fastly adapter copies every upstream header (`ts:crates/trusted-server-adapter-fastly/src/platform.rs:410-411`) and never turns on SDK decompression, which is off by default (`fastly-sdk:src/http/request.rs:131`). The Axum adapter builds `reqwest` without its decompression features (`ts:Cargo.toml:99`, `ts:crates/trusted-server-adapter-axum/src/platform.rs:292-299`). TS's Cloudflare and Spin adapters already decode (`ts:crates/trusted-server-adapter-cloudflare/src/platform.rs:395-415`, `ts:crates/trusted-server-adapter-spin/src/platform.rs:338-349`). So the regression lands on Fastly.

TS relies on the upstream coding in these places:

1. **Publisher origin.** TS narrows the client `Accept-Encoding` to gzip, deflate, and br (`ts:crates/trusted-server-core/src/publisher.rs:290-326`, called at `:3128` and `:4740`). It then routes on the response `Content-Encoding` (`:1622-1650`, `:6760`). It relays non-rewritten bodies raw (`:5420-5422`, `:5459`). It decodes rewritten bodies with the upstream coding and re-encodes them with the same coding (`:2602-2605`, `:2923-2928`). The one exception is a template-cache response, which TS emits as identity (`:813-820`).
2. **First-party proxy and creative rewrite.** The redirect loop asks for `gzip, deflate, br` (`ts:crates/trusted-server-core/src/proxy.rs:454`, `:1363-1366`). HTML and CSS are re-encoded with the input coding, and `Content-Encoding` is kept (`:653-699`, `:487-515`, `:459-481`).
3. **Asset proxy.** It forwards the client `Accept-Encoding` together with `If-None-Match`, `If-Modified-Since`, `If-Match`, `If-Unmodified-Since`, `Range`, and `If-Range` (`proxy.rs:90-102`, `:1146-1150`). It streams the response with the upstream representation headers intact. It strips only a short first-party denylist (`:1176-1194`, `:1003-1009`) and may then apply its own `Cache-Control` policy (`:1196-1200`). Origins that store objects with `Content-Encoding` metadata, such as S3, return that coding whatever the request asks for. So a range request returns a 206 whose `Content-Range` counts encoded bytes.
4. **Relays that keep the coding.**
   - GPT forwards the client `Accept-Encoding` and rebuilds the response from `Content-Encoding`, `ETag`, and `Last-Modified`. It adds `Vary: Accept-Encoding` only when the response is encoded (`ts:crates/trusted-server-core/src/integrations/gpt.rs:159-166`, `:194-253`).
   - The JS asset proxy does the same and also forwards `If-None-Match` and `If-Modified-Since` (`ts:crates/trusted-server-core/src/integrations/js_asset_proxy.rs:290-299`, `:365-410`).
   - The Prebid bundle keeps the upstream `Content-Encoding` (`ts:crates/trusted-server-core/src/integrations/prebid.rs:1218-1237`).
   - Sourcepoint forwards the client `Accept-Encoding` for paths it does not rewrite, and it skips rewriting encoded bodies (`ts:crates/trusted-server-core/src/integrations/sourcepoint.rs:862-875`, `:945-953`).
   - Lockr API, Permutive, and DataDome `/js/*` forward the client `Accept-Encoding` and return the upstream response unchanged (`ts:crates/trusted-server-core/src/integrations/lockr.rs:237`, `:264`; `permutive.rs:255`, `:269`; `datadome.rs:766`, `:789`).
   - Didomi forwards the client `Accept-Encoding` and returns the upstream body and representation headers, adding CORS or private cache headers (`didomi.rs:278-290`, `:506-516`).

On PR 275 every one of these sites receives decoded bytes, with `Content-Encoding` and `Content-Length` removed. The regressions on Fastly are these:

- Reader HTML ships to browsers uncompressed. TS sees no coding, so its encoder emits identity.
- Creative HTML and CSS, and every relayed JS, JSON, SVG, or font body, also ship uncompressed. Their `Content-Length` is gone, so the downstream response becomes chunked.
- A 206 over an encoded representation cannot be decoded. Asset range requests fail with a gateway error or return a corrupt prefix.
- Strong validators no longer describe the bytes served. GPT, the JS asset proxy, and the asset proxy forward the upstream `ETag` next to a decoded body.

After the cut, TS asks for the original coding on every site that relays bytes or runs its own rewrite codec. Those are the publisher origin, every first-party proxy hop, the asset proxy, GPT, the JS asset proxy, the Prebid bundle, Sourcepoint, Lockr API, Permutive, DataDome `/js/*`, and Didomi. TS's rewrite pipeline then sees the real upstream coding again. TS keeps decoding on for sites that consume decoded text or JSON. These are the auction providers, EC pull sync, the Fastly management API, DataDome protection, the GTM script rewrite (which asks for identity, `ts:crates/trusted-server-core/src/integrations/google_tag_manager.rs:825-830`), the Lockr and Permutive SDK fetches, and DataDome `tags.js`.

#### Gap at F

- EdgeZero decodes a single bare `gzip`, `deflate`, or `br` unconditionally, then removes `Content-Encoding` and `Content-Length` (`edgezero:crates/edgezero-core/src/outbound.rs:1252-1297`, removal at `:1280-1286`; classification at `edgezero:crates/edgezero-core/src/compression.rs:84-126`). No request option turns this off (`outbound.rs:49-67`).
- Unknown and stacked codings, and malformed `Content-Encoding` values, already pass through encoded. Both headers are kept and only the decoded cap is skipped (`outbound.rs:1278`, `:1294`, `:1397-1401`; `compression.rs:94-105`; `edgezero:docs/guide/capabilities.md:395-398`, `:415-420`). Passthrough for recognized codings should behave the same way.
- Every adapter already asks its provider for raw bytes. Axum disables `reqwest` decoding (`edgezero:crates/edgezero-adapter-axum/src/outbound.rs:145-154`). Cloudflare asks fetch for manual response encoding (`edgezero:crates/edgezero-adapter-cloudflare/src/outbound.rs:620-634`). Fastly never enables SDK decompression. **Unverified** whether the Spin host ever decodes. A Spin 4.1 run against a gzip origin would verify it. TS's own Spin adapter decodes in the guest, which is consistent with a raw host.
- A 206 is treated as a normal payload (`outbound.rs:1475-1484`), so a 206 with a recognized coding reaches the decoder.
- HEAD and 304 responses keep `Content-Encoding` and the encoded `Content-Length` (`outbound.rs:1472-1474`). In decoding mode, a HEAD therefore describes the encoded representation while the matching GET describes the decoded one.
- EdgeZero adds `Accept-Encoding: identity` only when the request has none, for example at `edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:752-760`. `OutboundRequest::from_request` keeps the browser's value (`outbound.rs:927-934`), so a forwarding app makes EdgeZero decode what the browser negotiated. The guide's forwarding example does exactly that (`edgezero:docs/guide/proxying.md:12-52`).

#### Requirements

- **EZ-5.1** An application MUST be able to choose, per request, whether EdgeZero decodes a recognized response content coding or delivers the response with its original coding. Decoding stays the default. In decoding mode, behavior is unchanged from F except EZ-5.8 and, if adopted, EZ-5.9 and EZ-5.10. The CHANGELOG MUST list those changes.
- **EZ-5.2** When the application asks for the original coding, a gzip, deflate, or br response MUST arrive byte for byte as the upstream sent it, with `Content-Encoding` and `Content-Length` unchanged. EdgeZero MUST NOT spend decoder memory on it.
- **EZ-5.3** Identity responses, and responses whose coding EdgeZero does not recognize, MUST behave identically under both choices.
- **EZ-5.4** Caps MUST still apply when the original coding is kept:
  - The encoded cap (`max_encoded_response_bytes`) MUST apply to the declared `Content-Length` before the body is read and to the bytes as they stream.
  - In buffered mode, the final cap (`max_response_bytes`) MUST apply to the encoded bytes.
  - The decoded cap MUST NOT apply to a body EdgeZero does not decode, which matches F's rule for unknown codings.
  - A request whose `max_brotli_window_bits` is invalid MUST be rejected the same way under either choice.
  - Chunk shaping (`max_chunk_bytes`) MUST apply unchanged.
- **EZ-5.5** Keeping the original coding MUST leave `ETag`, `Last-Modified`, `Vary`, `Content-Range`, `Accept-Ranges`, `Cache-Control`, and digest fields exactly as received.
- **EZ-5.6** The choice is an option under cross-cutting requirement 1, in buffered and streamed mode alike. While EdgeZero holds a begun or batched response that kept its coding, cross-cutting requirement 3 bounds it by the caps as EZ-5.4 states them.
- **EZ-5.7** `Accept-Encoding` handling MUST NOT depend on the choice. EdgeZero adds `identity` only when the request carries no `Accept-Encoding`. A value the application set is forwarded unchanged.
- **EZ-5.8** EdgeZero MUST NOT decode a 206 response under either choice. It MUST deliver a 206 with its original `Content-Encoding`, `Content-Length`, and `Content-Range`.
- **EZ-5.9** In decoding mode, HEAD and 304 responses SHOULD carry the same representation metadata the matching GET would. A recognized coding and its encoded length SHOULD therefore be removed from them.
- **EZ-5.10** In decoding mode, when EdgeZero decodes, it SHOULD weaken a strong `ETag` and remove `Accept-Ranges` and `Content-Digest`, because those describe the encoded representation. RFC 9110 sections 8.8.1 and 8.8.3.3 treat content coding as part of the selected representation.
- **EZ-5.11** An application MUST be able to tell, before sending, which choice a request carries, so its tests can check what each call site asked for.
- **EZ-5.12** Parsing JSON from a response that kept a recognized coding MUST parse the bytes as delivered, without decoding them. A parse failure is F's JSON decode gateway error (`BadGatewayDecodeReason::Json`, `outbound.rs:1688-1693`). Its internal message SHOULD say that the body is content-encoded.
- **EZ-5.13** Where the capability matrix marks an adapter Unsupported for this choice, a request that asks for the original coding MUST be rejected under cross-cutting requirement 2.
- **EZ-5.14** The capability matrix MUST list encoded response passthrough per adapter (cross-cutting requirement 9). The forwarding example that EZ-25.1 owns MUST show a relay that keeps the original coding.

#### Acceptance criteria

1. **Unit.** Given gzip, deflate, and br fixtures that declare `Content-Length`, when they are delivered with the original coding and a decoded cap of 1 byte, then the bytes and both headers are unchanged and no cap error occurs.
2. **Unit.** Given the original coding is kept, when the declared length exceeds the encoded cap, then the request fails with the encoded-cap limit before the body is read. When the encoded bytes exceed the final cap in buffered mode, it fails with the final-cap limit (`ResponseLimitReason::BufferedBody` at F).
3. **Unit.** Given a 206 with `Content-Encoding: gzip` and `Content-Range: bytes 10-19/100`, in decoding mode, then the response arrives undecoded with all three headers unchanged.
4. **Axum, Viceroy 0.21.0, workerd, Spin 4.1.** Given an origin that returns a gzip body with `Content-Length`, a strong `ETag`, and `Vary: Accept-Encoding`, when the application asks for the original coding, buffered and streamed, then the bytes and all four headers match the origin. In decoding mode, the bytes are decoded. On Spin 4.1 this holds where the run shows the host passes encoded bodies through. If the run shows the host decodes, the run records that, Spin is marked Unsupported, and the check becomes that a request asking for the original coding is rejected before sending under cross-cutting requirement 2 (EZ-5.13).
5. **Same environments, with the same Spin condition as criterion 4.** Given the request sets no `Accept-Encoding`, the origin sees `identity`. Given the request sets `gzip, br`, the origin sees exactly that, under both choices. Where Spin is Unsupported, the Spin run checks decoding mode only.
6. **Viceroy 0.21.0.** Given a two-slot batch where one slot keeps the original coding, then that slot's body is encoded and the other slot's body is decoded.
7. **workerd.** Given the application relays a passthrough response to its client, then the client receives the exact compressed bytes and `Content-Encoding`, and Workers does not re-encode them. The run records whether the origin `Content-Length` reached the client, and the caveat at `edgezero:docs/guide/capabilities.md:422-426` is updated to match.
8. **Trusted Server, Viceroy 0.21.0.** Run the TS integration suite against origin fixtures.
   - A gzip HTML page reaches the client with `Content-Encoding: gzip` and decodes to the rewritten document with the TSJS tag.
   - A br SVG relay is byte-identical, with the origin `Content-Length` and `ETag`.
   - `Range: bytes=0-99` against a gzip-stored asset returns 206 with the origin `Content-Range` and `Content-Length: 100`.
   - A JS asset revalidation with `If-None-Match` returns 304.
   - A `/first-party/proxy` HTML creative keeps `Content-Encoding: br`.

#### Out of scope

- Re-encoding at response egress. EdgeZero compressing a response from the client's `Accept-Encoding` would not replace this requirement. A relay would still lose `ETag`, `Content-Range`, and `Content-Length` fidelity, and it would pay decode and encode cost on bodies it never reads.
- Deciding to decode after seeing the response headers. TS keeps its own rewrite codec for this cut.
- Any change to request-body encoding. F already sends request bodies with their declared coding.

#### Open questions

1. Should forwarding an inbound request keep the original coding by default, since forwarding is a proxy role? Trusted Server's preferred answer is no. One default everywhere is easier to reason about, and the forwarding example can set the choice explicitly.
2. Should EZ-5.9 and EZ-5.10 become MUST? Trusted Server's preferred answer is to keep them SHOULD. TS keeps the original coding on every relay, so it does not depend on them, but other applications that relay in decoding mode would benefit.

### EZ-6. Begin a request without waiting, and observe a batch without blocking

**Priority:** Blocking, except EZ-6.31, which is Follow-up and required before TS enables reusable sandboxes in production.
**Adapters:** Axum, Cloudflare, Fastly, Spin. The behavior must hold on all four. Fastly may be BestEffort where EZ-6.21 says so.
**Related:** EZ-1 (a begun GET/HEAD built from an inbound request is accepted the same way), EZ-2 (no Fastly platform timer fires before a request's deadline, and EZ-2.8 defers to EZ-6.16 when a slot's deadline is the cutoff), EZ-3 (EZ-3.3 owns the passed-deadline rule), EZ-4, EZ-5, EZ-7, EZ-11, EZ-12, EZ-14 (their per-request options apply to begun requests and batch slots), EZ-8 (Image Optimizer is Unsupported on begun requests and batches), EZ-9 (begun requests and batches never follow redirects), EZ-14 (lifetime of a streamed origin body), EZ-15 (fire-and-forget is separate from abandoning a begun request), EZ-24 (the test double models arrival-time judgment)

#### Need

On every auctioned navigation TS runs the origin fetch and the bidder fan-out at the same time, and it keeps every bid that arrived on time even when it looks at the bid late.

Today on Fastly the publisher does this:

1. On the overlapped path it starts the origin fetch, with a streamed response, before EC KV preload and bid construction (`ts:crates/trusted-server-core/src/publisher.rs:4834-4873`). The overlapped path is a navigation GET with EC preload and no template-cache key (`ts:crates/trusted-server-core/src/publisher.rs:4821-4846`, `:3109-3116`). Only the Fastly adapter supports it (`ts:crates/trusted-server-adapter-fastly/src/platform.rs:503-505`).
2. It reads the EC snapshot from KV (`ts:crates/trusted-server-core/src/publisher.rs:4874-4888`).
3. It builds, signs, and sends every bidder request without waiting (`ts:crates/trusted-server-core/src/auction/orchestrator.rs:1586-1834`). The in-flight bids are carried across the origin wait (`:27-48`). Auctions run only for GET navigations (`ts:crates/trusted-server-core/src/publisher.rs:3215-3222`).
4. It waits for the origin response head (`ts:crates/trusted-server-core/src/publisher.rs:5153-5154`). When the overlap does not apply, it sends the origin afterwards, and on an auctioned GET the bids are still in flight during that send (`:5155-5176`).
5. It streams the page and holds the tail at `</body>` (`ts:crates/trusted-server-core/src/publisher.rs:3780`, `:3944`), where it collects the auction (`:4017`, `:4058`, `:4120`). The collection loop drains every in-flight bid (`ts:crates/trusted-server-core/src/auction/orchestrator.rs:2127-2297`). Non-HTML responses collect before their body streams (`ts:crates/trusted-server-core/src/publisher.rs:4195-4239`). A template-cache hit streams the cached template and collects at the seam (`:5108-5125`).
6. Collection keeps bids that arrived while the origin was slow, on purpose. The code says dropping them "would discard SSP responses that already arrived" (`ts:crates/trusted-server-core/src/auction/orchestrator.rs:2290-2296`).
7. After the bidders it makes one mediator call with the smaller of the remaining auction time and the mediator timeout, and skips mediation when no time remains (`ts:crates/trusted-server-core/src/auction/orchestrator.rs:2349-2364`).
8. After the response is delivered, inside the same Fastly serving callback, it runs EC pull sync, which sends partner requests concurrently and waits for them (`ts:crates/trusted-server-core/src/ec/pull_sync.rs:225-300`, called from `ts:crates/trusted-server-adapter-fastly/src/main.rs:618-643`).

The numbers come from the example config. The navigation auction has 500 ms (`ts:trusted-server.example.toml:340`), and the `/auction` endpoint 2000 ms (`:282`). The commented-out mock mediator example uses 1000 ms (`:674`). The origin has 15 s first-byte and between-bytes timers (`ts:crates/trusted-server-core/src/publisher.rs:98`, `:4517-4518`).

On PR 275's APIs, TS will begin the origin request for the overlapped navigation GET only. Every other origin forward, including POST, PUT, PATCH, DELETE, and OPTIONS, uses `HttpClient::send` with the inbound body streamed upstream, and those methods never run an auction. TS sends the bidders as one `start_batch_until` batch with the auction deadline as the cutoff, keeps working (KV, origin wait, page streaming), checks the batch at body-chunk boundaries, collects it at `</body>`, and then makes the mediator call with one `HttpClient::send`. Pull sync after delivery becomes a batch or plain sends inside the serving callback.

Without this requirement TS has two bad choices. It can serialize the origin behind the auction, which adds the auction time to TTFB, or it can lose every bid whenever the origin head or the `</body>` chunk arrives after the cutoff. On Axum, Cloudflare, and Spin it would also send no bid at all unless it polled the batch continuously.

#### Gap at F

- There is no way to begin one request without waiting. On Fastly `send` waits for the response head (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:450-453`, `:1279-1290`). Batches refuse streamed responses (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:2580-2584`), and the origin must stream.
- On Axum, Cloudflare, and Spin a batch sends nothing until it is polled. Each slot is an unpolled future (`edgezero:crates/edgezero-adapter-axum/src/outbound.rs:200-206`; `edgezero:crates/edgezero-adapter-cloudflare/src/outbound.rs:389-395`; `edgezero:crates/edgezero-adapter-spin/src/outbound.rs:434-440`), although the trait promises that every eligible request is attempted first (`edgezero:crates/edgezero-core/src/outbound.rs:428`).
- Completion is judged when the application looks, not when the response arrived. On Fastly, once the cutoff has passed the driver does not examine pending slots and reports every one as unresolved (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:621`, `:649-651`). A slot examined after its own budget becomes a timeout even if the host received it in time (`:1002`). On the other adapters the completion instant is taken when the slot's future is polled to completion (`edgezero:crates/edgezero-adapter-axum/src/outbound.rs:203`; `edgezero:crates/edgezero-adapter-cloudflare/src/outbound.rs:392`; `edgezero:crates/edgezero-adapter-spin/src/outbound.rs:437`).
- There is no non-blocking way to observe a batch. `OutboundBatch::next`, `collect`, and `HttpClient::send_all_until` all wait (`edgezero:crates/edgezero-core/src/outbound.rs:229-252`, `:343-399`, `:588-596`). Repeated termination exists in code and tests (`:349-351`, `:3035-3040`).
- On Fastly a buffered slot's body is read to the end before any other slot can be observed (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:838-875`, `:1069-1072`; `edgezero:docs/guide/capabilities.md:276-279`).
- The guide says elapsed time includes "any delayed guest observation" (`edgezero:docs/guide/capabilities.md:442-444`), and EdgeZero's design spec records that a provider-ready response can stay unresolved (`edgezero:docs/superpowers/specs/2026-05-21-outbound-http-design.md:1821-1826`).

#### Requirements

Beginning one request.

- **EZ-6.1** An application MUST be able to begin one outbound request, keep working, and collect its response later. A request is on its way once EdgeZero has handed it to the host, the network stack, or the adapter's runtime, so that it proceeds with no further EdgeZero call from the application. When begin returns, the request MUST be on its way. Beginning MUST NOT wait for connection establishment or response headers. This MUST hold on all four adapters.
- **EZ-6.2** A begun request MUST support streamed response mode, where collection returns at the response head with a lazily read body, and buffered response mode, where collection returns the whole body under the request's response caps.
- **EZ-6.3** A begun request MUST accept a buffered request body and a bodiless GET/HEAD built from an inbound request (EZ-1). Any other streamed upload MUST be rejected before anything is sent, as an invalid request (400 class), the category F uses for a streamed upload in a batch. `HttpClient::send` MUST keep accepting streamed uploads.
- **EZ-6.4** For the same request and the same origin behavior, a begun request MUST produce the same status, headers, body, error category, and timeout cause as `HttpClient::send`. Options and client defaults apply to a begun request as cross-cutting requirements 1 and 2 say, including options accepted at BestEffort.
- **EZ-6.5** A begun request's time budget MUST be computed from the moment begin is called, with the same rules as `send`. Time spent settling a bodiless GET/HEAD (EZ-1) counts against it.
- **EZ-6.6** The application MUST be able to ask whether a begun request's response is ready without blocking.

Batches.

- **EZ-6.7** When `start_batch_until` returns, every eligible slot MUST be on its way (EZ-6.1), on all four adapters. A batch that is never polled MUST still send every eligible request.
- **EZ-6.8** If an adapter configuration cannot put a request on its way without being polled, beginning the request or the batch slot MUST fail with an internal error (500 class) before anything is sent. It MUST NOT silently defer sending.
- **EZ-6.9** The application MUST be able to check a batch without blocking. A check returns a newly finished slot, the batch termination, or "nothing new". It MUST work inside a handler and inside a streamed response body that EdgeZero is writing to the client, on all four adapters. It MUST follow the same rules as `OutboundBatch::next`, including at most one result per slot, a clean end before `Completed`, and repeated termination.
- **EZ-6.10** `OutboundBatch::next` MUST stay safe to cancel. A `next` that is waiting MUST return no later than the cutoff plus 5 ms, even when no slot finishes.

Arrival-time judgment.

- **EZ-6.11** Arrival is as cross-cutting requirement 5 defines it. While the application waits on any EdgeZero operation (a begun request's response head, a read from a response body EdgeZero produced, a single send, a batch wait, or a non-blocking check), EdgeZero MUST record the arrival of every other in-flight request in the same request lifetime (cross-cutting requirement 4). Axum, Cloudflare, and Spin MUST also record arrivals whenever the application awaits anything else. While the application does CPU work without awaiting, an adapter records an arrival no later than the application's next await, and the guide MUST say so. A buffered slot whose body has not ended by the cutoff is unresolved at the cutoff, and EdgeZero MUST NOT wait past the cutoff to finish reading it.
- **EZ-6.12** A slot is on time if and only if its recorded arrival is strictly before the observation cutoff. An arrival at the cutoff is late.
- **EZ-6.13** A slot's or begun request's own budget MUST be judged at its recorded arrival. A response that finished within its budget MUST be delivered as received, even when the application collects it after the budget and after the cutoff. Collection itself MUST NOT turn such a response into a timeout.
- **EZ-6.14** When EdgeZero first observes a slot at or after the slot's budget deadline, whether or not a response is waiting, it MUST treat the slot as a timeout at the budget deadline, because an earlier arrival cannot be proven. EZ-6.16 decides the outcome when that deadline is the cutoff.
- **EZ-6.15** Finished slots MUST be delivered in order of recorded arrival, with ties in input order. A slot's elapsed time MUST be its arrival minus the batch start.
- **EZ-6.16** After the cutoff, the batch MUST first deliver every slot that arrived on time, then report `Cutoff` if any slot is unresolved and `Completed` otherwise. Nothing first observed at or after the cutoff may be delivered, except a timeout whose budget deadline is before the cutoff. A slot whose budget deadline equals the cutoff has one outcome. It is not delivered, and the batch reports `Cutoff` with that slot unresolved. EZ-2.8's per-slot timeout item applies only to a slot whose deadline is before the cutoff.

Bounded background work.

- **EZ-6.17** Responses that arrive while the application is busy elsewhere follow cross-cutting requirement 3. A cap failure MUST become that slot's outcome at the moment the cap was exceeded.
- **EZ-6.18** Receiving one slot's body MUST NOT stall the application. While a bidder's body trickles in, a streamed origin body that the application is reading MUST keep flowing, and other slots MUST still be recorded at arrival. The only exception is the Fastly fallback in EZ-6.21.

Cancellation and request lifetime.

- **EZ-6.19** Dropping or cancelling a begun request or a batch releases it as cross-cutting requirement 6 says. Cancelling a batch MUST report the slots it never delivered. Results recorded but not yet delivered are discarded by cancellation, so an application that wants them checks the batch first.
- **EZ-6.20** Outbound work the application starts inside its per-request lifecycle callback after delivery MUST behave as it does during dispatch, including deadlines, batches, non-blocking checks, and arrival recording. This holds with the per-request client, a client the application keeps for the sandbox, or a newly built client. Every begun request and batch slot still in flight when the callback returns MUST be released. The one exception is a fire-and-forget send, whose lifetime EZ-15 defines (cross-cutting requirement 4).
- **EZ-6.21** Per-adapter levels. Axum, Cloudflare, and Spin MUST be Native for beginning a request, checking a batch without blocking, and arrival-time recording. Fastly is BestEffort only in these respects. A cold backend registration may block begin. Arrivals are recorded only during EdgeZero operations. If the host does not report response-body readiness, Fastly MAY delay the application and other arrivals by up to one slot body's transfer time, and the capability note MUST state that bound. Every other requirement in EZ-6 holds on Fastly as written.

Interaction with response writing.

- **EZ-6.22** A begun request and a batch MUST be usable from inside a streamed response body for as long as that body lives.
- **EZ-6.23** When EdgeZero stops writing a response (client disconnect, write deadline, body error, or request cancellation), every begun request and batch owned by that response body MUST be released per EZ-6.19.
- **EZ-6.24** On Fastly, a batch wait inside a response body that EdgeZero is writing SHOULD return by the response write deadline, so a cutoff set past that deadline cannot hold the response. The response then ends with the existing write-deadline outcome.
- **EZ-6.25** A streamed response from a begun request MUST keep enforcing its own deadline on each body read. The guide MUST state that a streamed page lives until the earlier of the origin request's deadline (from begin) and the response write deadline (30 s from the start of writing by default, `edgezero:crates/edgezero-core/src/response_egress.rs:14`), and that a hold inside the body counts against both.

Sequential calls and invariants.

- **EZ-6.26** `HttpClient::send` and begin MUST work while a batch from the same request lifetime is in flight and after it finishes, including inside a streamed response body. A request whose deadline has already passed follows EZ-3.3 on Fastly and fails the same way, with a gateway timeout naming the request deadline and nothing sent, on the other adapters, as F already does (`edgezero:crates/edgezero-core/src/time.rs:176-182`).
- **EZ-6.27** Batches MUST keep refusing streamed uploads and streamed response mode. Begun requests and batches MUST NOT follow redirects; a 3xx is returned as the final response.
- **EZ-6.28** Image Optimizer is Unsupported on begun requests and batch slots. Those send modes MUST reject it as cross-cutting requirement 2 says, before any other outbound preflight check.
- **EZ-6.29** Removed.
- **EZ-6.30** EdgeZero's capability matrix MUST gain rows for beginning a request, checking a batch without blocking, and arrival-time recording, at the levels in EZ-6.21. The guide text that says elapsed time includes delayed observation (`edgezero:docs/guide/capabilities.md:442-444`) and that ready handles can stay unobserved (`:276-279`) MUST be corrected.
- **EZ-6.31** Follow-up, required before TS enables reusable sandboxes. On a reused Fastly sandbox, abandoned requests MUST NOT accumulate. A request still in flight when its callback returned MUST NOT be observed by a later request in the same sandbox, the next request MUST still send, and abandoned work MUST be gone once its backend answers or its platform timer fires.

#### Acceptance criteria

Unless a criterion says otherwise, the scenarios use three bidders A, B, and C, a 500 ms auction, bidder requests with a 1,000 ms per-call timeout, and an origin request with a 15 s timeout. The batch starts at 34 with cutoff 534, so each bidder's budget is 500 ms, set by the cutoff. Times are milliseconds from the inbound request. Elapsed values hold within 5 ms on Axum, workerd, Spin 4.1, and Viceroy 0.21.0, and exactly in unit runs with an injected `MonotonicClock`. On hosted Fastly the origins apply the stated delays after receiving each request. A time measured inside the guest from a deadline or the cutoff (scenario 2's gateway timeout at 534, scenario 7's `Cutoff` by 539) keeps its 5 ms tolerance. Otherwise the pass condition is the set and order of delivered slots and the `Cutoff` or `Completed` outcome. Each elapsed value MUST be no less than the stated value minus 5 ms, and no more than the stated value plus that slot's measured connection setup and round trip plus 5 ms. The run reports both measurements for every slot. Origins are loopback or staging servers that behave as each scenario states.

1. Slow origin. Axum, workerd, Spin 4.1, Viceroy 0.21.0, hosted Fastly. Given the origin is begun at 2 and its head arrives at 700, the page body arrives in chunks at 700, 900, and 1,300, the batch starts at 34 with cutoff 534, A answers at 180, B at 320, and C at 900, when the application waits for the origin head and checks the batch at 701, then it gets A with elapsed 146, B with elapsed 286, and `Cutoff` with C unresolved. At F, the same flow loses A and B on Fastly and sends no bidder request on the other adapters.
2. Fast origin with mediator. Axum, workerd, Spin 4.1, Viceroy 0.21.0, hosted Fastly. Given the origin head at 120, a body chunk at 260, and A, B, C answering at 180, 320, and 400, when the application waits on the batch at 260, then it gets A, then B, then C and `Completed` at 400. A `send` at 400 built with `OutboundRequest::deadline` set to the cutoff and `OutboundRequest::timeout` set to 1,000 ms gets a 134 ms budget with the request deadline as its cause. If that origin stays silent, the send fails with a gateway timeout at 534.
3. CPU stretch. Axum, workerd, Spin 4.1, Viceroy 0.21.0. Given the application does CPU work from 520 to 560 without awaiting and B answers at 530, then B is either delivered with elapsed 496, where the adapter recorded its arrival during the CPU work, or not delivered, with the batch reporting `Cutoff` and B unresolved. B is never delivered with an elapsed time of 500 or more. The run records which.
4. Begin puts the request on its way. Every adapter. Given begin returns and the application then awaits an unrelated operation (a timer or a KV read) for 100 ms without any other EdgeZero outbound call, then the origin receives the request within that window.
5. Unpolled batch. Every adapter. Given a batch that the application does not check for 300 ms while it awaits an unrelated EdgeZero operation, then every bidder receives its request within that window.
6. Arrival during origin body reads. Viceroy 0.21.0 and hosted Fastly. Given the origin sends body chunks at 100 and 800 and B answers at 320, when the application only reads the origin body, then B is delivered with elapsed 286.
7. Trickling slot. Viceroy 0.21.0 and hosted Fastly. Given a bidder sends its head at 100 and its body at 600 while the origin streams a chunk every 50 ms, then the origin chunks keep reaching the application between 100 and 600, or, where the capability note records the body-readiness fallback, they stall for at most the bidder body's transfer time. The run records which. With a 534 cutoff, that bidder is unresolved and the batch reports `Cutoff` by 539.
8. Send during a live batch. Viceroy 0.21.0 and hosted Fastly. Same as scenario 1, but the origin is forwarded with `send` after the batch starts. A and B are delivered.
9. Caps while waiting. Every adapter. Given a bidder slot with a 1 MiB decoded cap and a 64 KiB buffered cap, whose gzip body decodes to 128 KiB and trickles in over 300 ms, then the slot fails with the buffered-cap error before the body ends.
10. Abandonment on a reused sandbox (EZ-6.31). Viceroy 0.21.0. Given a reused guest serving 6 requests, each beginning a request to an origin that answers after 2 s and dropping it, then request 6 still sends, no request observes another's arrivals, and no abandoned work remains 2 s after the last request. Hosted Fastly. With reuse limits raised so one sandbox can serve 1,000 requests at about 50 per second, the same holds for request 1,001. The run records how many requests each sandbox served.
11. Write deadline. Axum, workerd, Spin 4.1. Given a response with a 1 s write deadline whose body waits on a batch with a 3 s cutoff, then the response ends with the write-deadline outcome, and the batch's in-flight requests are released. On Viceroy 0.21.0 and hosted Fastly the run records the outcome, because EZ-6.24 is a SHOULD there.
12. Parity. Every adapter. For buffered uploads, a begun request and `send` produce the same outcomes on EdgeZero's existing outbound contract cases, including client defaults.
13. Upload rules. Unit and every adapter. Begin rejects a POST with a streamed body as an invalid request before sending. Begin accepts a bodiless GET built from an inbound request. `send` forwards a POST with a streamed body.
14. Timeout before the cutoff. Every adapter. Given a fourth bidder D in the scenario 1 batch with a 300 ms per-call timeout that never answers, then the batch delivers D as a gateway timeout naming the per-call timeout, with elapsed 300, before it reports `Cutoff`.
15. Arrival at the cutoff. Unit, injected clock. Given a slot whose response is complete exactly at the cutoff, then it is unresolved.
16. Waiting `next` ends at the cutoff. Every adapter. Given a batch with a 200 ms cutoff and no slot answering, then a waiting `next` returns `Cutoff` within 205 ms.
17. Cancellation report. Unit and every adapter. Given a batch with one slot recorded but not delivered and one still in flight, when it is cancelled, then both indices are reported as undelivered.
18. Check inside a streamed body. Every adapter. Given a streamed response body that checks the batch between chunks while two slots are still in flight, then each check returns "nothing new" in under 5 ms.
19. Work after delivery. Viceroy 0.21.0. Given a serving callback that, after delivery, starts a three-slot batch with a client it keeps and collects it, then all three results are delivered with arrival-based elapsed values, and nothing from that batch is still in flight after the callback returns.

The Trusted Server end-to-end check runs in TS's integration suite on Viceroy 0.21.0, the Axum binary, and workerd, and in the Spin smoke (`ts:scripts/smoke-spin.sh`) on Spin 4.1. A navigation whose origin head arrives at 700 ms with a 500 ms auction contains the bids from bidders that answered at 180 and 320 ms and not one that answered at 600 ms. A page whose `</body>` arrives at 1,300 ms gives the same result, and TS ranks locally because no mediator time remains. TTFB tracks origin latency plus less than 20 ms. A POST navigation forwards its body through `send` and runs no auction. On Fastly, pull sync after delivery completes inside the serving callback.

#### Out of scope

- Streamed responses in batches. One begun request per streamed exchange is enough.
- Streamed uploads on a begun request. TS forwards those with `send`.
- Redirect following on begun requests and batches (EZ-9).
- Fire-and-forget sends (EZ-15).
- An idle policy for long streamed bodies (EZ-14).

#### Open questions

1. When EdgeZero first observes a slot late, should it credit the arrival to the previous time it looked? TS prefers no. Crediting the earlier time would admit responses that arrived after the cutoff, and TS treats a delivered result as on time.
2. Should a Fastly batch wait inside a response body be bounded by the write deadline as a MUST rather than the SHOULD in EZ-6.24? TS prefers SHOULD. TS's cutoff (500 ms) is far inside the 30 s write budget, so TS does not depend on it.
3. Does hosted Fastly report when a response body has data, so that a body read can be bounded and a trickling slot need not stall the application? TS prefers that EdgeZero establish the answer on hosted Fastly before the cut and record it, because EZ-2.6, EZ-6.18, and EZ-6.21's fallback all depend on it.

### EZ-7. Keep the `x-edgezero-proxy` header off relayed responses

**Priority:** Blocking
**Adapters:** Fastly, Axum, Cloudflare, Spin. Identical behavior on all four.
**Related:** EZ-6 and EZ-9 (begun requests and the final response of a followed chain honor the setting), EZ-15 (a fire-and-forget send honors it once EZ-15 lands), EZ-5 (same relay paths), EZ-24 (its test double applies the same default and opt-out), EZ-25.1 (owns the forwarding example)

#### Need

TS has no global response-header filter. Its Fastly adapter copies every header of a TS response to the client (`ts:crates/trusted-server-adapter-fastly/src/compat.rs:13-19`, `:49-55`). These paths return upstream headers wholesale, or minus a small denylist:

- The publisher fallback, on every route (`ts:crates/trusted-server-core/src/publisher.rs:5337-5370`, `:5420-5422`, `:5459`).
- The asset proxy, minus set-cookie, HSTS, and clear-site-data (`ts:crates/trusted-server-core/src/proxy.rs:1194`, `:1003-1009`, `:110-111`).
- `/first-party/proxy`, minus the CORS headers (`proxy.rs:747-761`, `:727-731`).
- GTM beacons and non-2xx GTM script responses, minus the first-party denylist (`ts:crates/trusted-server-core/src/integrations/google_tag_manager.rs:968-987`, strip at `:976`).
- Testlight (`ts:crates/trusted-server-core/src/integrations/testlight.rs:115-130`).
- Sourcepoint, after its own cache and `Location` adjustments (`ts:crates/trusted-server-core/src/integrations/sourcepoint.rs:934`, `:995`, `:1027`, `:1042`, `:1046`).
- Lockr API (`lockr.rs:237`), Permutive (`permutive.rs:255`), DataDome non-200 `tags.js` and `/js/*` (`datadome.rs:668-674`, `:789`), and Didomi (`didomi.rs:506-516`), which return the upstream response with its headers.

GPT, the JS asset proxy, the Prebid bundle, and the SDK fetches rebuild their responses from allowlists, so they would not leak (`gpt.rs:194-253`, `js_asset_proxy.rs:365-443`, `prebid.rs:1218-1253`).

On PR 275, every response on those paths gains `x-edgezero-proxy: fastly`. Every proxied page and asset then tells browsers which edge runtime served it, and any downstream cache that stores origin headers keeps the field too. Under the hard-cut decision TS does not strip headers itself, so EdgeZero must not add the field. Two secondary effects follow:

- The marker consumes response header budget. It costs 20 to 26 bytes depending on the adapter, plus one field (`edgezero:crates/edgezero-core/src/outbound.rs:3361-3403`). So a header-count cap of N admits only N-1 upstream fields.
- Removing it after the fact would mark the response's headers as modified (`outbound.rs:631-639`, `:653-656`). TS does not read that provenance today.

#### Gap at F

- Every adapter adds `x-edgezero-proxy: <adapter>` to every outbound response after header normalization, bodyless responses included. It overwrites any upstream field of that name and is charged to the header caps (`outbound.rs:1407-1422`; Fastly `edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:1016-1020`; Axum `edgezero:crates/edgezero-adapter-axum/src/outbound.rs:335-339`; Cloudflare `edgezero:crates/edgezero-adapter-cloudflare/src/outbound.rs:526-530`; Spin `edgezero:crates/edgezero-adapter-spin/src/outbound.rs:660-664`).
- `into_response` carries the field into the application's response (`outbound.rs:723-742`).
- No option on `OutboundRequest` or `HttpClient` turns it off.
- Only tests read the field. Those are the core unit test (`outbound.rs:3375`), an Axum contract test (`edgezero:crates/edgezero-adapter-axum/tests/contract.rs:56-79`), and the docs contract check (`edgezero:scripts/check_outbound_docs_contract.mjs:219`, `:224`). EdgeZero's design says the marker is not a loop-prevention mechanism (`edgezero:docs/superpowers/specs/2026-05-21-outbound-http-design.md:43-45`).
- The marker predates PR 275. TS never received it, because TS did not use EdgeZero's proxy client.

#### Requirements

- **EZ-7.1** An application MUST be able to receive outbound responses that carry no EdgeZero-added `x-edgezero-proxy` field. The setting is an option under cross-cutting requirement 1. For a followed redirect chain, it applies to the final response.
- **EZ-7.2** The application MUST be able to set this once for every request it sends through one client handle, including requests it begins, batches, or follows and, once EZ-15 lands, fire-and-forget sends. A per-request setting MUST override the client-wide setting.
- **EZ-7.3** When the marker is off, it MUST NOT be charged to `max_response_header_bytes` or `max_response_header_count`. Those caps then count only upstream fields.
- **EZ-7.4** When the marker is off, EdgeZero MUST neither add nor remove an `x-edgezero-proxy` field. An upstream field of that name is an ordinary upstream header.
- **EZ-7.5** When the marker is on, its value, overwrite behavior, and cap accounting MUST stay as at F.
- **EZ-7.6** An application SHOULD still be able to learn which adapter completed an exchange without the header.
- **EZ-7.7** An application MUST be able to tell, before sending, whether a request will carry the marker, after any client-wide setting is applied. Its tests can then check each call site.
- **EZ-7.8** The guide text that describes the marker (`edgezero:docs/guide/adapters/overview.md:78`, `edgezero:docs/guide/capabilities.md:389-390`) MUST describe how to keep it off. The forwarding example that EZ-25.1 owns MUST show it off for responses that reach browsers.

#### Acceptance criteria

1. **Unit.** Given an upstream response with N fields and a header-count cap of exactly N, when the marker is off, then the response succeeds. When it is on, it fails with the header-count limit. The same holds for header bytes.
2. **Unit.** Given a client handle set to omit the marker, when the application sends one request with no override, one with an explicit "on", one begun request, and one two-slot batch, then only the explicit "on" request carries the marker.
3. **Unit.** Given the marker is off and the upstream sends `x-edgezero-proxy: upstream`, then the field arrives unchanged.
4. **Axum, Viceroy 0.21.0, workerd, Spin 4.1.** Given 200, 204, HEAD 200, 304, and 302 responses from an origin, when the marker is off, then none carries the field. Under F's behavior, all of them carry it.
5. **Trusted Server, Viceroy 0.21.0.** Browser-facing responses from the publisher fallback, asset proxy, `/first-party/proxy`, GTM collect, Testlight, Lockr API, Permutive, DataDome `/js/*`, Sourcepoint, and Didomi carry no `x-edgezero-proxy`. TS's request client asks for the marker to be off once, and no TS call site sets it per request.

#### Out of scope

- Any loop-prevention or `Via` behavior.
- Filtering other upstream headers. TS's own allowlists and denylists stay TS business logic.

#### Open questions

1. Should the marker be off by default? Trusted Server's preferred answer is yes. PR 275 is already a hard migration with no compatibility aliases (`CHANGELOG.md:7-10`). No EdgeZero code outside tests consumes the field. Any forwarding application leaks it to browsers, and it silently consumes header caps. If the default stays on, EZ-7.2 still gives TS one place to turn it off.

### EZ-8. Fastly Image Optimizer on outbound requests

**Priority:** Blocking
**Adapters:** Fastly single sends must honor the options. Axum, Cloudflare and Spin are Unsupported, and so are Fastly begun requests, batches and fire-and-forget sends.
**Related:** EZ-1 (a bodiless GET/HEAD from an inbound request may carry the options, and the rejection comes before body settling), EZ-3 (EZ-3.3 owns the deadline-passed-before-sending rule), EZ-6 and EZ-15 (begun requests and, once EZ-15 lands, fire-and-forget sends reject the options), EZ-11 (EZ-8.6 rejects the options combined with cache tagging), EZ-2 (the options must not reduce connection reuse, and EZ-2's eligibility groups bound the uninterruptible window), EZ-5 (asset routes forward the client's `Accept-Encoding`, so unoptimized assets need encoded passthrough), EZ-9 (the options carry to redirect hops), EZ-16 (provider causes from this path reach logs), EZ-Q1 (owns the hosted Fastly criterion)

#### Need

Trusted Server serves publisher images through asset routes and asks Fastly Image Optimizer to transform them. `handle_asset_proxy_request` (`ts:crates/trusted-server-core/src/proxy.rs:1096-1204`) does this in order.

1. It builds the options from the route's profile table unless the incoming or rewritten path is SVG (`proxy.rs:1105-1117`, `ts:crates/trusted-server-core/src/asset_image_optimizer.rs:43-97`). The closed set covers region, a preserve-origin-query flag, format, quality, resize filter, width, height and crop (`ts:crates/trusted-server-core/src/platform/image_optimizer.rs:12-181`).
2. It strips the origin query by default (`proxy.rs:1118-1120`, `ts:crates/trusted-server-core/src/settings.rs:1738-1758`). Config validation rejects preserving the query on Image Optimizer routes (`settings.rs:1709-1719`) because Image Optimizer reads client query parameters as transformation inputs (`settings.rs:965-967`).
3. It sets `Host` to the URI authority (`proxy.rs:1151`, `:893-917`). S3 SigV4 signs `host`, `x-amz-content-sha256`, `x-amz-date` and an optional `x-amz-security-token` (`ts:crates/trusted-server-core/src/s3_sigv4.rs:62-149`, header set `:128-133`).
4. Only when the route has options, S3 auth, and the request is GET or HEAD (`proxy.rs:1015-1023`), it first sends a signed HEAD without Image Optimizer (`:1025-1078`), because Image Optimizer can mask S3 errors (`:1037-1039`). A failed HEAD answers a HEAD client directly, or triggers a signed streamed GET without Image Optimizer that returns the raw S3 error (`:1056-1077`).
5. It then signs the final request (`proxy.rs:1167-1169`), attaches Image Optimizer (`:1173-1175`), asks for a streamed response (`:1176-1179`) and sends (`:1181-1187`).

The Trusted Server Fastly adapter translates the options to the SDK (`ts:crates/trusted-server-adapter-fastly/src/platform.rs:190-305`), sends with the blocking `fastly::Request::send` (`:521-523`), and refuses Image Optimizer on its async path (`:527-536`).

After the cut Trusted Server deletes that adapter layer. The asset route builds an `OutboundRequest` with `stream_response()`, a `timeout`, the SigV4 headers set through `header(..)`, and the Image Optimizer options, then calls `HttpClient::send`. The S3 HEAD preflight and the raw-error GET stay Trusted Server logic and are plain signed sends without options. Without EZ-8 the options cannot be attached, so every optimized route serves the untransformed original at full size and in its original format. A local operator configuration (not named here) enables an Image Optimizer route with S3 SigV4 today, so this blocks the cut.

#### Gap at F

`OutboundRequest` carries a fixed portable option set and no way to attach provider options (`edgezero:crates/edgezero-core/src/outbound.rs:48-67`, parts `:69-89`). The Fastly SDK constrains what EdgeZero can offer.

- Image Optimizer works only on the blocking `Request::send` (`fastly-sdk:src/http/request.rs:606-635`, `send_with_image_opto` `:668-792`). `send_async` refuses it (`:861-865`). The streaming send never looks at it (`:918-947`). Converting a request to handles drops it (`:148-152`, `:2853-2865`).
- Cache pass silently disables it (`:641-660`), and it silently overrides surrogate keys (`:3134-3141`).
- It sends no request body (`:696`, `:761-763`).

At F the Fastly single send uses `send_async` (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:443-454`), and batch slots use handles (`:519-529`). The error mapper folds `ImageOptimizerUnsupported` into a generic 502 with the message "Fastly outbound request failed" (`:960-965`, `:91-94`). Viceroy cannot run Image Optimizer and answers its host call with `Unsupported` (`viceroy:src/component/image_optimizer.rs:7-15`, `viceroy:src/error.rs:248`), which the SDK reports as `ImageOptimizerUnsupported` (`fastly-sdk:src/http/request.rs:714-719`). The SDK's own `send_async` produces the same cause without reaching the host (`:861-865`).

#### Requirements

- **EZ-8.1** An application MUST be able to attach Fastly Image Optimizer options to an individual outbound request from portable code, without depending on the Fastly SDK and without compiling Fastly-only code for other targets. The options MUST stay attached wherever EdgeZero rebuilds or forwards the request, including redirect hops (EZ-9).
- **EZ-8.2** On every adapter and send mode that EZ-8.14 marks Unsupported, a request carrying the options MUST get cross-cutting requirement 2's rejection. The rejection MUST come before anything is sent and before any other outbound preflight check, including EZ-1's settling of an inbound body. It MUST name the option and the adapter. The options MUST NOT be dropped silently.
- **EZ-8.3** The options MUST express at least what Trusted Server uses. That is the seven SDK regions, the preserve-origin-query flag, output format (auto, AVIF, GIF, JPEG, JPEG XL, MP4, PNG, WebP), quality in 0 to 100, resize filter (nearest, bilinear, bicubic, Lanczos 2, Lanczos 3), width and height in pixels, and a crop to an aspect ratio with a default, smart, or explicit x and y percentage focus. Invalid values MUST be rejected when the application builds the options, as a client-class error. Invalid means quality above 100, a zero dimension, an offset above 100, smart focus combined with offsets, or only one offset. Trusted Server enforces all five rules today, at config load (`ts:crates/trusted-server-core/src/settings.rs:1429`, `:1485-1509`) and at runtime (`ts:crates/trusted-server-core/src/asset_image_optimizer.rs:229`, `:275-300`, `:316-343`).
- **EZ-8.4** On Fastly, a single send carrying the options MUST be processed by Image Optimizer, so the response body is the transformed image.
- **EZ-8.5** Only GET and HEAD may carry the options. A request with any other method, or with a streamed upload that would be uploaded, MUST get cross-cutting requirement 2's rejection before backend registration, because Fastly cannot honor the options there. A GET or HEAD whose body EZ-1 settles as bodiless is accepted. One that carries body bytes is rejected as EZ-1 requires.
- **EZ-8.6** A request carrying the options that also asks for cache bypass or cache tagging (EZ-11) MUST get cross-cutting requirement 2's rejection before sending. Fastly silently disables Image Optimizer under bypass and ignores tags under Image Optimizer. Only the default cache policy is valid with the options. This requirement owns that rule.
- **EZ-8.7** In a batch, a slot carrying the options MUST fail as that slot's own result with the EZ-8.2 rejection, and its siblings MUST proceed. F already isolates streamed slots this way (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:2573-2586`, `edgezero:crates/edgezero-adapter-fastly/tests/contract.rs:1262-1279`).
- **EZ-8.8** Begun requests (EZ-6) and, once EZ-15 lands, fire-and-forget sends MUST give a request carrying the options the EZ-8.2 rejection on every adapter. This requirement owns that rule.
- **EZ-8.9** An optimized response MUST get the same response handling as any other Fastly response. That covers response header caps, normalization, the bodyless rules for HEAD, 204 and 304, the proxy marker (EZ-7), the decoding policy (EZ-5), and buffered or streamed mode. A streamed optimized body MUST stay lazy, so the first bytes reach the application before the transform output ends.
- **EZ-8.10** A deadline that has passed before sending follows EZ-3.3. A response that completes after the deadline MUST be a gateway timeout with the budget's cause. EdgeZero may be unable to interrupt Image Optimizer while it works. An optimized request against a stalled origin MUST still fail no later than a documented bound of at most twice its budget, consistent with how EZ-2.2 bounds backend configurations. The capability footnote MUST record that bound and the hosted measurement.
- **EZ-8.11** A service without Image Optimizer, or Viceroy, MUST produce a gateway-class 502. Its diagnostic naming the missing entitlement MUST be available to logs and as a typed reason the application can match, so the application can tell this case from other upstream failures without parsing provider text. The error's display text does not change (cross-cutting requirement 7). If Fastly refuses options that EdgeZero itself built, the result MUST be an internal error.
- **EZ-8.12** Attaching the options MUST NOT reduce origin connection reuse. Optimized and unoptimized requests to the same origin with the same transport settings MUST be eligible to share connections under EZ-2.1.
- **EZ-8.13** On every send mode, and for every request whether or not it carries options, the URI an application reads back from a built `OutboundRequest` (`OutboundRequest::uri`) MUST be the request target sent. EdgeZero MUST NOT add, remove or reorder query parameters, and MUST NOT change application-set headers beyond its documented normalization (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:752-760`). `Host` stays derived from the URI authority unless the application sets a host authority override. Signatures computed after building a request, such as Trusted Server's signed HEAD preflight, raw-error GET and optimized GET, therefore stay valid. This is the single statement of that invariant in the document.
- **EZ-8.14** Capability levels. Fastly single send is BestEffort, because it needs a service entitlement and Viceroy cannot exercise it. It becomes Native when the EZ-Q1 record shows the hosted criterion below passing. Axum, Cloudflare and Spin are Unsupported on every send mode. Fastly begun requests, batches and fire-and-forget sends are Unsupported.
- **EZ-8.15** When EZ-24's published test double lands, it MUST record attached options and accept them, so application tests can assert them.
- **EZ-8.16** Documentation MUST add the capability row with its footnote, a guide section on attaching options, the GET/HEAD and default-cache restrictions, the uninterruptible window and its bound, the entitlement prerequisite, and a warning that Image Optimizer also reads transformation parameters from the request query, with options taking precedence (`fastly-sdk:src/http/request.rs:3094-3096`). EdgeZero does not filter the query.
- **EZ-8.17** An application MUST be able to read, from a built request, the Image Optimizer options attached to it, so its tests can check each call site.

#### Acceptance criteria

- Unit. Given options with quality 101, a zero width, offset 101, smart focus with offsets, or only an x offset, when the application builds them, then it gets a client-class error. Quality 0 and 100 succeed.
- Unit. Given a request with options attached, the application reads back options equal to the ones it attached. The same holds for the next request of a followed redirect (EZ-9).
- Axum. Given a local origin, when a GET with options is sent, then the result is cross-cutting requirement 2's rejection with status 400 naming the option and `axum`, and the origin accepts no connection. Given a GET built from an inbound request whose body is an unread stream, with options, then the result is the same rejection and the inbound body is not read. Given a two-slot batch with options on slot 0, then slot 0 fails with the rejection, slot 1 returns 200, and the batch completes.
- workerd and Spin 4.1. A single send and a batch slot with options each return the rejection, and no fetch or WASI request is made.
- Viceroy 0.21.0. Given a GET with options to a reachable origin, then the result is a 502 with the EZ-8.11 reason and the origin receives no request. This shows the options reached the platform, not that Image Optimizer ran, and the hosted criterion is the proof. Given POST with options, GET with a body, or GET with cache bypass, then each gets the rejection, and EZ-2.13's statistics show no registration. Once EZ-11 lands, the same holds for GET with cache tagging. Given a two-slot batch with options on slot 0, then slot 0 fails and slot 1 reaches the origin. Given an optimized GET and a plain GET to one origin with the same transport settings, then EZ-2.13's statistics report one distinct backend configuration.
- Viceroy 0.21.0. Given a plain send whose URI has percent-encoded path bytes and a query, then the origin records a request target equal to `OutboundRequest::uri`, and every header the application set arrives unchanged.
- Hosted Fastly, run under EZ-Q1 on Trusted Server's staging service with Image Optimizer enabled. Given a known JPEG and options for width 100 and WebP, when sent as GET, then the response is `image/webp` with a decoded width of 100 and the body streams. The same request as HEAD succeeds, and its status and headers are recorded in the capability footnote. Given an origin that accepts the connection and never answers, and a 1 s timeout, the request fails within the EZ-8.10 bound, and the measured time is recorded in the footnote.
- Trusted Server end to end, on a Fastly staging service. An optimized asset path returns a transformed image, the HEAD preflight carries no options, and a missing S3 object returns the raw S3 status from the preflight. Trusted Server unit tests read back the options on the final GET and find them equal to the profile-table expectation (EZ-8.17).

#### Out of scope

- Cloudflare Image Resizing (`worker:src/request_init.rs:139-140`). A later Cloudflare-specific requirement can add it.
- The full SDK option set (`fastly-sdk:src/image_optimizer.rs:672-760`).
- Filtering or rewriting the query for Image Optimizer.
- The S3 HEAD preflight, which stays Trusted Server logic.

#### Open questions

- **Q8.1** Should the option set grow beyond Trusted Server's subset now? Trusted Server prefers no. Ship the subset it uses and leave room to add fields when a consumer needs them.
- **Q8.2** Removed.
- **Q8.3** What should HEAD with options do if Image Optimizer treats it differently from GET? Trusted Server prefers passing HEAD through unchanged and documenting the hosted result.

### EZ-9. Redirect following with the application's target policy at every hop

**Priority:** Blocking
**Adapters:** All four, with identical behavior. On Spin, plain-HTTP origins and origins outside a narrowed host list must be declared.
**Related:** EZ-1 (EZ-9.7 owns settling the body once per chain), EZ-2 (each new origin adds a backend configuration on Fastly), EZ-3 (EZ-3.3 owns failing a hop whose deadline has passed), EZ-6 and EZ-15 (begun requests, batches and, once EZ-15 lands, fire-and-forget sends never follow), EZ-4, EZ-5, EZ-7, EZ-8, EZ-11, EZ-12 and EZ-14 (their options carry to every hop), EZ-10 (a replayed body is re-checked against the upload cap), EZ-24 (each hop reaches a test client as an ordinary send)

#### Need

Trusted Server follows redirects in `proxy_with_redirects` (`ts:crates/trusted-server-core/src/proxy.rs:1276-1559`). The maintainer decided to delete that loop and move following into EdgeZero. EdgeZero must reproduce this behavior or improve it deliberately.

- Limit. `MAX_REDIRECTS = 4` (`:1284`), and the loop runs `0..=MAX_REDIRECTS` (`:1292`), so at most 5 requests go out. At the limit it returns the last 3xx as the result, not an error (`:1470-1481`, test `:4305-4352`).
- Statuses. 301, 302, 303, 307 and 308 are followed (`:1415-1422`). Anything else is final.
- `Location`. A missing or empty value, or one that is not visible ASCII, returns the 3xx (`:1434-1446`). The value is parsed as an absolute URL and otherwise joined against the current URL (`:1484-1490`). A value that fails both is a 502 "invalid redirect".
- Methods. A 301 or 302 answering anything but GET, HEAD or POST is not followed (`:1452-1468`). 301, 302 and 303 become a bodyless GET, and HEAD stays HEAD (`:1534-1553`). 307 and 308 keep the method and replay the body (`:1380`).
- Body headers. Once the body is dropped, `Content-Type`, `Content-Length`, `Content-Encoding`, `Content-Language` and `Content-Location` are removed (`:76-82`, `:1372-1376`) and stay removed after a later 307 (test `:4223-4302`).
- Per-hop policy. The scheme must be http or https (`:1299-1304`). With `require_https` a non-HTTPS target is rejected (`:1305-1310`, redirect `:1492-1498`). A redirect to another scheme returns the 3xx (`:1499-1507`). A host is required (`:1312-1317`, `:1509-1516`). The domain allowlist rejects disallowed hosts (`:1319-1327`, `:1517-1526`). These rejections reach the client as 403 on the first-party proxy and the Prebid bundle (`ts:crates/trusted-server-core/src/error.rs:128-131`). GTM, GPT and testlight wrap them as integration errors, which are 502 (`ts:crates/trusted-server-core/src/integrations/google_tag_manager.rs:968-970`, `ts:crates/trusted-server-core/src/integrations/gpt.rs:282-284`, `ts:crates/trusted-server-core/src/integrations/testlight.rs:208-210`, status `error.rs:127`).
- Headers. The same curated set is rebuilt on every hop (`proxy.rs:1356-1371`). It contains no `Authorization` or `Cookie` today (`:67-73`).
- Budget. Each hop gets a fresh 15 s first-byte timer (`:1329-1344`, `ts:crates/trusted-server-core/src/platform/mod.rs:82`). There is no chain budget.
- Final response. `finalize_response` uses the final URL for its origin log line and pixel heuristics (`proxy.rs:747-761`, `:528-590`, `:633-642`).

| Caller                                                                                                | Follows | Target policy                                           | Body                  | Response             |
| ----------------------------------------------------------------------------------------------------- | ------- | ------------------------------------------------------- | --------------------- | -------------------- |
| First-party proxy (`proxy.rs:1577-1603`)                                                              | yes     | `proxy.allowed_domains` (open when empty), HTTP allowed | none                  | buffered, rewritten  |
| GTM (`ts:crates/trusted-server-core/src/integrations/google_tag_manager.rs:794-833`, send `:968-970`) | yes     | HTTPS only, GTM domains                                 | POST beacon, buffered | buffered, rewritten  |
| GPT (`ts:crates/trusted-server-core/src/integrations/gpt.rs:142-153`, send `:273-287`)                | no      | none                                                    | none                  | buffered passthrough |
| Testlight (`ts:crates/trusted-server-core/src/integrations/testlight.rs:199-210`)                     | yes     | open                                                    | POST JSON, buffered   | passthrough          |
| JS asset proxy (`ts:crates/trusted-server-core/src/integrations/js_asset_proxy.rs:279-306`)           | no      | none                                                    | none                  | streamed passthrough |
| Prebid bundle (`ts:crates/trusted-server-core/src/integrations/prebid.rs:1279-1286`)                  | yes     | HTTPS only, `proxy.allowed_domains`                     | none                  | passthrough          |

After the cut, GPT and the JS asset proxy use a plain `HttpClient::send`. The other four ask EdgeZero to follow, with a target policy that applies Trusted Server's scheme, HTTPS and allowlist rules. Without EZ-9, first-party creative proxying, GTM, testlight and the Prebid bundle return raw 3xx responses to browsers and creatives, and GTM and Prebid script loading breaks whenever the upstream redirects.

#### Gap at F

EdgeZero never follows redirects, and its outbound spec calls that a security property. Following requires a fresh request, so the application's target policy runs again (`edgezero:docs/superpowers/specs/2026-05-21-outbound-http-design.md:1454-1470`). Target policy is an explicit non-goal (`:40-42`). Axum disables reqwest redirects (`edgezero:crates/edgezero-adapter-axum/src/outbound.rs:145-155`), Cloudflare sets manual redirect mode (`edgezero:crates/edgezero-adapter-cloudflare/src/outbound.rs:426-429`), and Spin and Fastly do not follow (spec `:1466-1467`). An application cannot write a correct loop on F's API alone. `OutboundRequest` cannot be copied for replay, and `OutboundRequestParts` is non-exhaustive (`edgezero:crates/edgezero-core/src/outbound.rs:69-70`). A fragment in a raw-string target fails canonicalization (`:914-916`, `:1880-1884`), while parsing a `Uri` drops it silently (`http:src/uri/path.rs:28-30`).

#### Requirements

- **EZ-9.1** Following MUST be an explicit per-call choice. A plain `HttpClient::send`, a begun request (EZ-6), a batch, and, once EZ-15 lands, a fire-and-forget send MUST keep returning 3xx responses unfollowed.
- **EZ-9.2** A call that follows MUST supply a hop limit from 1 to 20, pending Q9.3, and a per-hop target policy. There MUST be no default that follows everything. A limit outside the range MUST be a client-class error when the call is configured.
- **EZ-9.3** Exactly 301, 302, 303, 307 and 308 are followable. Every other status is final.
- **EZ-9.4** `Location` handling.
  - A missing or empty `Location` makes the 3xx final, reported as "no location".
  - More than one `Location` field (pending Q9.1), a value that is not a URL reference, a resolved URL with user information, or a resolved http or https target EdgeZero cannot send for any other reason MUST fail the chain as a gateway-class 502 protocol failure, not a client error. A request the policy itself returns is not reclassified, so its own errors come back unchanged.
  - A relative value resolves against the URI of the hop that returned it. A fragment never reaches the wire.
  - A resolved target with a scheme other than http or https is not a resolution failure. It reaches the policy with no prepared next request. The policy can stop, reject, or return a request of its own. If the policy asks to follow a prepared request that does not exist, the chain fails as the 502 protocol failure above.
- **EZ-9.5** The policy MUST run once per redirect, before any network work for that hop. It MUST see the redirect number, the status, the URI that answered, the resolved target, the 3xx headers, and whether the hop is cross-origin. It MUST be able to follow with the next request EdgeZero prepared, when one exists, or with a request it builds or modifies, to stop and return the 3xx as final, or to reject with the application's own error value, after which nothing more is sent. The initial request is not passed to the policy, so the application checks it before the call. The policy MUST NOT be able to change how a hop's response body is handled (EZ-9.11).
- **EZ-9.6** Method and body rules, applied to the request of the hop that got the redirect.

  | Status   | Method of that hop                    | Next method                                              | Next body |
  | -------- | ------------------------------------- | -------------------------------------------------------- | --------- |
  | 301, 302 | GET, HEAD                             | same                                                     | empty     |
  | 301, 302 | POST                                  | GET                                                      | empty     |
  | 301, 302 | any method other than GET, HEAD, POST | not followed, final, reported as "method not rewritable" | none      |
  | 303      | HEAD                                  | HEAD                                                     | empty     |
  | 303      | any other                             | GET                                                      | empty     |
  | 307, 308 | any                                   | same                                                     | replayed  |

- **EZ-9.7** Replay. A buffered body MUST be replayed byte for byte. A streamed upload cannot be replayed, so a 307 or 308 after one is final, reported as "body not replayable", and the policy is not consulted. When the method rule drops the body, `Content-Type`, `Content-Length`, `Content-Encoding`, `Content-Language` and `Content-Location` MUST NOT be sent, and they stay unsent on later hops because the dropped body stays dropped. A bodiless GET/HEAD whose inbound body is an unread stream MUST be settled once for the whole chain, before the first hop and within the request's budget, the way EZ-1 settles it for a single send. A later 307 or 308 then replays an empty body. This requirement owns the once-per-chain rule.
- **EZ-9.8** Cross-origin hops. When the next target differs from the previous hop in scheme, host or effective port, `Authorization` and `Cookie` MUST NOT be sent to it, and a host authority override (`OutboundRequest::host_authority_override`) MUST NOT carry over. On a same-origin hop both carry over. EdgeZero MUST NOT add `Referer`, `Origin` or any other header beyond its documented normalization. Other credential-bearing headers, such as `x-amz-*` signatures or custom API-key headers, are not removed, and the documentation MUST say the policy must remove them. A request the policy returns is sent as the policy built it. This matches reqwest's cross-host rule for the two headers EdgeZero strips (`reqwest:src/redirect.rs:239-252`).
- **EZ-9.9** Every other request option MUST carry to every hop unchanged. That covers `timeout`, `deadline`, cache policy including tagging, the upload cap, every response cap, decoding settings, the proxy marker, Image Optimizer options, TLS verification mode, connect timeout and stream idle policy.
- **EZ-9.10** Budget. The per-call `timeout` MUST apply to each hop, which keeps Trusted Server's per-hop behavior. An absolute `deadline` MUST bound the whole chain, and a hop whose deadline has already passed is not sent (EZ-3.3). Each hop is an ordinary send, so on Fastly a chain waits hop by hop. On Fastly a chain's hops MUST be eligible to share connections exactly as single sends with the same origin and per-call timeout are (EZ-2.1, EZ-2.2).
- **EZ-9.11** Intermediate bodies. The body of a followed 3xx MUST NOT be delivered, MUST NOT be decoded, MUST NOT affect the chain's outcome, and MUST NOT cost more time or memory than a documented bound. EdgeZero MAY read a bounded, documented amount of it to keep the connection reusable. Such a body MUST NOT fail the chain for any reason. That covers malformed or empty compressed bodies, such as `Content-Encoding: br` with no bytes, which fails Brotli decoding at F (`edgezero:crates/edgezero-core/src/compression.rs:325-333`), and an oversized declared `Content-Length`. Response header caps still apply to each hop's headers. The caller's buffered cap and streamed or buffered mode apply to the final response only. For a chain with no redirect, the result MUST be indistinguishable from a plain send of the same request, with the same status, headers, bytes, and limit error.
- **EZ-9.12** At most the limit's number of redirects MUST be followed, so at most limit plus one requests are sent. A followable response that would exceed the limit MUST be returned as a successful final result, reported as "limit reached".
- **EZ-9.13** A successful result MUST report the final response, the final request URI, the number of redirects followed, and, when the final status is followable, why it was not followed. The reasons are no location, method not rewritable, body not replayable, limit reached, and stopped by policy. The reason is present exactly when the final status is one of the five followable statuses.
- **EZ-9.14** A hop failure MUST return that hop's error unchanged, with which request failed (0 for the initial request). A policy rejection MUST return the application's value unchanged, with the redirect number. No error is turned into a 3xx response. A following call MUST send each hop as an ordinary send through the client it was called on, including an application-supplied client, so a recording client sees every hop's method, URI, headers and body. No hop number or chain state reaches the client.
- **EZ-9.15** Abandoning a following call MUST release the in-flight hop as a plain send would, and nothing more is sent.
- **EZ-9.16** EdgeZero SHOULD log each followed hop at debug level with status, scheme, host, port and path, and MUST NOT log query strings, which can carry identifiers. Trusted Server appends `ts-ec` to proxied URLs (`ts:crates/trusted-server-core/src/proxy.rs:1206-1242`).
- **EZ-9.17** Removed.
- **EZ-9.18** When several rules could end the chain at one redirect, the first in this order wins. The order follows Trusted Server's loop (`ts:crates/trusted-server-core/src/proxy.rs:1415-1526`). Followable status (EZ-9.3), then `Location` presence ("no location"), then the method rule ("method not rewritable"), then the replay rule ("body not replayable"), then the limit ("limit reached"), then `Location` resolution (502 protocol failure, except a non-HTTP scheme, which goes on to the policy), then the policy (stop or reject).
- **EZ-9.19** Documentation MUST add a guide section on following redirects with the method table, the credential rules and what they leave in place, the budget rules, and a security note. The note says the policy is the only target policy, and an open policy also follows redirects to private or link-local addresses. The guide MUST record that this supersedes the outbound spec's "read `Location` and issue a new request" guidance for applications that use it, while plain sends keep that rule (`edgezero:docs/superpowers/specs/2026-05-21-outbound-http-design.md:1469-1470`). The capability matrix MUST list redirect following as Native on all four adapters, with a note that Spin needs plain-HTTP origins and origins outside a narrowed host list declared (`edgezero:docs/guide/capabilities.md:362-377`).

#### Acceptance criteria

- Unit, with a scripted upstream. For every status from 300 to 308 and each of GET, HEAD, POST, PUT, PATCH, DELETE and OPTIONS with a 5-byte buffered body (empty for GET and HEAD), the number of requests and the second request's method, body and body headers match EZ-9.6.
- Unit. A POST answered by 302 and then 307 sends POST, GET, GET, with the body only on the first request and no body headers on the later two. A POST with an empty buffered body and `Content-Type` answered by 307 sends `Content-Type` on both requests.
- Unit. A streamed upload answered by 307 sends one request and reports "body not replayable".
- Unit. A GET whose body is an empty unread stream, answered by 307, sends two requests with empty bodies and reports no stop reason.
- Unit. With limit 4 and five 302 responses to GET, five requests are sent and the result is a 302 with "limit reached" and four redirects followed. A PUT answered by 301 at the limit reports "method not rewritable". A 302 with no `Location` answering a PUT at the limit reports "no location". Limits 0 and 21 are rejected.
- Unit. `Authorization` and `Cookie` reach a same-origin hop and do not reach a hop that changes host, port, or only the scheme. A host authority override reaches a same-origin hop only. An `x-amz-date` header reaches both.
- Unit. Relative, `../x`, `//b.example/x` and absolute `Location` values follow. A fragment never reaches the wire. A missing or empty value reports "no location". Two `Location` fields, user information, or an unusable http target give a 502 protocol failure.
- Unit. A policy rejection returns the application's value with redirect number 1 and no second request. A policy stop returns the 3xx with "stopped by policy". A modified next request is sent exactly as returned. A `javascript:` target reaches the policy with no prepared request. A policy that asks to follow a prepared request there gets a 502 protocol failure.
- Unit. A 302 whose body errors when read, a 302 with `Content-Encoding: br` and an empty body, and a 302 declaring a 10 MiB `Content-Length` under a 1 MiB cap each lead to the final 200.
- Unit. A deadline that passes after the first hop fails the second hop as a gateway timeout attributed to the deadline, and the second hop is not sent. Every hop sees the original per-call timeout.
- Unit. Dropping a following call while hop 1 is in flight sends no hop 2.
- Unit. With logging captured at trace level, a two-hop chain whose URIs carry `?ts-ec=abc` logs neither `ts-ec` nor `abc`.
- Unit. A following call made on an application-supplied recording client records one ordinary send per hop, with each hop's method, URI, headers and body.
- Axum, workerd, Spin 4.1 and Viceroy 0.21.0. For each adapter, with a 200 and no redirect under a 10-byte buffered cap, bodies of 10 and 11 bytes and a declared `Content-Length: 11` give the same status, headers, bytes or limit error from a plain send and from a following call, for identity, gzip and passthrough encodings.
- Axum and Viceroy 0.21.0, with two local origins where A answers 302 to B and B answers 200. A plain send returns 302. A following call returns 200 with B as the final URI and one redirect followed.
- Viceroy 0.21.0. A two-hop chain to two loopback origins with one per-call timeout leaves EZ-2.13's statistics showing the same two backend configurations as two single sends to those origins with that timeout.
- Spin 4.1. A redirect to a plain-HTTP host that is not declared fails at that hop with Spin's denial, which F maps to 400 (`edgezero:crates/edgezero-adapter-spin/src/outbound.rs:1358-1364`). That Spin 4.1 reports this case as `HttpRequestDenied` is **Unverified**, and this criterion records what it reports.
- Trusted Server end to end. Port every redirect test in `ts:crates/trusted-server-core/src/proxy.rs:3978-4515`, the allowlist tests at `:6114-6258`, and the initial-target policy tests at `:6259-6325` to the following call, with identical recorded methods, URIs, bodies, header names and request counts. Use EZ-24 if it has landed and Trusted Server's own recording client otherwise (EZ-9.14). On the Axum dev server, a signed first-party proxy URL whose origin 302s to an allowed host returns the final 200, and one that 302s to a disallowed host returns 403.

#### Out of scope

- Loop detection beyond the hop limit (`edgezero:docs/superpowers/specs/2026-05-21-outbound-http-design.md:43-45`) and blocking private addresses (`:41-42`). Both stay application policy.
- Buffering a streamed upload so it can be replayed.
- An asynchronous policy.
- Following inside begun requests, batches or fire-and-forget sends.

#### Open questions

- **Q9.1** Should more than one `Location` field fail or use the first? Trusted Server prefers failing. `Location` is a singleton field, and a well-formed upstream never sends two. Trusted Server uses the first value today (`ts:crates/trusted-server-core/src/proxy.rs:1434-1438`).
- **Q9.2** Should EdgeZero strip credentials again when the policy's modified request changes origin? Trusted Server prefers no. The policy is application code, and stripping again would stop it from re-signing for a new origin.
- **Q9.3** Is 20 the right maximum limit? Trusted Server prefers 20. It bounds backend configurations per chain on Fastly, and Trusted Server uses 4.

## Follow-up requirements

### EZ-10. 413 Payload Too Large for request-body overflow

**Priority:** Follow-up
**Adapters:** Fastly, Axum, Cloudflare, Spin. The status and error category must be the same everywhere. Adapters differ only in what the origin may already have received when a streamed upload overflows (EZ-10.6).
**Related:** EZ-1, whose GET/HEAD body rejection stays 400 and whose EZ-1.13 owns the rule on unread bytes. EZ-16, whose provider detail shares EZ-10's log-only treatment (cross-cutting requirement 7). EZ-17, which must report an upload that overflowed after the request head left as sent. EZ-25, which owns the scaffold and guide fixes.

#### Need

TS answers request-body overflow with 413 on every endpoint that reads a body.

| TS endpoint                                                                           | Cap            | Behavior today                                                                  | Source                                                                                                                                       |
| ------------------------------------------------------------------------------------- | -------------- | ------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| `POST /auction`                                                                       | 256 KiB        | Early `Content-Length` check, then a post-read check. 413 `text/plain`          | `ts:crates/trusted-server-core/src/auction/endpoints.rs:46`, `:126-159`. Test `:1771-1791`                                                   |
| First-party sign and rebuild                                                          | 64 KiB each    | 413                                                                             | `ts:crates/trusted-server-core/src/proxy.rs:40-41`, `:1720-1721`, `:1909-1910`. Tests `:6343`, `:6389`                                       |
| Verify signature, rotate key, deactivate key                                          | 4 KiB          | 413                                                                             | `ts:crates/trusted-server-core/src/request_signing/endpoints.rs:101-102`, `:116-117`, `:306-307`, `:425-426`. Tests `:995`, `:1034`, `:1073` |
| Cache purge                                                                           | 4 KiB          | 413                                                                             | `ts:crates/trusted-server-core/src/cache_purge.rs:30`, `:141-142`                                                                            |
| Batch sync                                                                            | 2 MiB          | Early `Content-Length` check, then a post-read check. 413 JSON `body_too_large` | `ts:crates/trusted-server-core/src/ec/batch_sync.rs:143-158`, `:188-194`                                                                     |
| Integration POST proxies (datadome, didomi, permutive, sourcepoint, lockr, testlight) | 256 KiB        | 413                                                                             | `ts:crates/trusted-server-core/src/integrations/mod.rs:167`, `:174-222`                                                                      |
| GTM beacons                                                                           | 64 KiB default | Early `Content-Length` check and a read cap. 413                                | `ts:crates/trusted-server-core/src/integrations/google_tag_manager.rs:186-188`, `:290-292`, `:689-728`. Read-cap test `:2763-2817`           |

TS's request-too-large error maps to 413 (`ts:crates/trusted-server-core/src/error.rs:48-50`, `:129`, `ts:crates/trusted-server-core/src/http_util.rs:465-484`). After the cut TS reads every body through EdgeZero's readers, and each returns 400 on overflow. Without EZ-10:

- Every endpoint above answers an undeclared or understated overflow with 400 instead of 413. Tests that go through TS's early `Content-Length` checks keep passing, but the read-cap tests fail.
- After `RequestContext::json_within`, TS cannot tell overflow from malformed JSON, because both are 400 (`edgezero:crates/edgezero-core/src/context.rs:401-408`).
- A route that uses EdgeZero's extractors cannot fix the status with `App::set_error_response_renderer`, because EdgeZero reapplies the error's own status after the renderer runs (`edgezero:crates/edgezero-core/src/app.rs:334-344`, `edgezero:docs/guide/handlers.md:397-399`).
- A client POST forwarded to the publisher origin over TS's `max_request_body_bytes` cap fails with 400, where 413 is the accurate status.

#### Gap at F

- Every inbound reader returns 400 on overflow. That covers `Body::into_bytes_bounded` (`edgezero:crates/edgezero-core/src/body.rs:81-111`), `RequestContext::body_bytes` (`context.rs:202-258`, `:621-626`, `:663-668`), `json_within` and `form_within` (`:345-357`, `:396-408`), and the extractors built on them (`edgezero:crates/edgezero-core/src/extractor.rs:33-44`, `:69-84`, `:86-102`, `:430-439`, `:466`, `:507-516`). EdgeZero's inbound design states 400 for overflow and gives no reason against 413 (`edgezero:docs/superpowers/specs/2026-08-22-inbound-body-design.md:663-664`).
- No declared length is checked early. A 10 MiB declared upload against a 256 KiB cap is read up to the cap before it is refused.
- `max_request_body_bytes` overflow returns 400 on every adapter. See Axum `edgezero:crates/edgezero-adapter-axum/src/outbound.rs:229-235` and `:253-262`, Cloudflare `edgezero:crates/edgezero-adapter-cloudflare/src/outbound.rs:671-677` and `:871-878`, Fastly `edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:1137-1146` and `:1269-1277`, and Spin `edgezero:crates/edgezero-adapter-spin/src/outbound.rs:582-600`. On Fastly a streamed overflow is found after the request head has been sent (`:1180-1182`), and the upload is abandoned (`outbound-http-design.md:5713-5716`). On Spin even a buffered body over the cap is found only after the request has been issued (`outbound.rs:362-377`, `:565-567`), so Spin does not meet EZ-10.6 at F.
- EdgeZero has no error category with status 413 (`edgezero:crates/edgezero-core/src/error.rs:452-474`).
- The scaffold and demo fallback policies answer an over-cap unmatched request with 400 (`edgezero:crates/edgezero-cli/src/templates/core/src/lib.rs.hbs:104-107`, `edgezero:examples/app-demo/crates/app-demo-core/src/lib.rs:196-199`). EdgeZero's own unit test uses 413 (`edgezero:crates/edgezero-core/src/ingress.rs:980-999`).

#### Requirements

- **EZ-10.1** EdgeZero MUST give request-body overflow its own error category with status 413. Applications MUST be able to tell it apart from malformed payloads (400), the inbound read deadline (408) and every other error without matching message text. Its client-visible body MUST be stable, MUST NOT carry provider or internal detail, and MUST NOT depend on which cap fired.
- **EZ-10.2** Every inbound body cap EdgeZero enforces MUST report overflow in that category. That covers `RequestContext::body_bytes`, `json_within`, `form_within`, the `Json`, `ValidatedJson`, `ValidatedJsonWithin`, `Form`, `ValidatedForm` and `ValidatedFormWithin` extractors, and `Body::into_bytes_bounded`. It MUST hold for a declared overflow (`Content-Length` above the cap) and for an undeclared one, such as chunked HTTP/1.1, HTTP/2 or HTTP/3 with no length, or a length that understates the body. Malformed JSON or form data under the cap stays 400.
- **EZ-10.3** The existing sticky read semantics of the `RequestContext` readers MUST NOT change. Once a read overflows while reading the body, every later read of that body returns the same 413 and reads nothing more. A stricter cap checked against a body already read returns 413 and leaves the body usable at a larger cap.
- **EZ-10.4** When the admitted request's single normalized `Content-Length` exceeds a reader's cap, the reader SHOULD fail with 413 without reading any body byte. An inbound read deadline that has already passed still wins, with 408. Such an early failure is not a read, so it MUST NOT stop a later read at a larger cap from succeeding, and EZ-10.3's sticky rule does not apply to it.
- **EZ-10.5** Overflow of an outbound request's `max_request_body_bytes` MUST report the same 413 category on all four adapters, for buffered and streamed bodies. An upstream that answers 413 MUST stay an ordinary response with status 413, never this error.
- **EZ-10.6** These rules hold for what the origin may see:
  - A buffered body over the cap MUST fail before anything is sent, on every adapter.
  - On Axum and Cloudflare a streamed upload over the cap MUST fail before the origin is contacted.
  - On Fastly and Spin the origin may already have the request head and part of the body. The upload MUST then be aborted so the origin never receives a complete, well-formed request with a truncated body. EZ-17 reports such a request as sent.
- **EZ-10.7** Applications MUST be able to learn whether an overflow came from an inbound read or an outbound upload without parsing message text. That detail MUST NOT reach the wire, and the text and `Display` of existing errors MUST NOT change (cross-cutting requirement 7). Where EZ-16 has landed, the detail SHOULD be read the same way as EZ-16's provider detail.
- **EZ-10.8** An application's `App::set_error_response_renderer` MUST receive the overflow error, and the response status after rendering MUST be 413.
- **EZ-10.9** The scaffold and demo `AdmissionDecision::ReadBodyBeforeFallback` policies MUST answer an over-cap fallback request with 413.
- **EZ-10.10** A 413 response MUST NOT carry `Retry-After`, because the condition is not temporary (RFC 9110 section 15.5.14).
- **EZ-10.11** Unread request bytes after a 413 on connections EdgeZero owns follow EZ-1.13.
- **EZ-10.12** The handler and capability guides MUST give the statuses for inbound bodies, which are 413 for overflow, 400 for malformed payloads, and 408 for the read deadline. They MUST also state where Fastly and Spin streamed overflow happens relative to sending.

#### Acceptance criteria

1. Environment is unit. Given a 9-byte body with no declared length, when it is read with a cap of 8 through `body_bytes`, `json_within`, `form_within`, an extractor, or `Body::into_bytes_bounded`, then the result is the 413 category and the response status is 413. For the `RequestContext` readers, a second read at any cap returns the same 413 without reading. Malformed JSON under the cap stays 400.
2. Environment is unit. Given a request declaring `Content-Length: 10` with a 10-byte body, when it is read with a cap of 5, then the result is 413. With the inbound read deadline already passed, the result is 408. Record whether any body byte was read before the 413 (EZ-10.4 is SHOULD). Where none was, a later read of that request with a cap of 10 succeeds.
3. Environment is Axum, Viceroy 0.21.0, workerd and Spin 4.1. A chunked POST of 2 KiB to a route capped at 1 KiB gets 413. On Axum, an HTTP/1 request pipelined after it is either refused by a connection close or answered from the correct offset.
4. Environment is hosted Fastly, Trusted Server staging. An HTTP/2 POST with no `content-length` over the cap gets 413.
5. Environment is Axum, Viceroy 0.21.0, workerd and Spin 4.1. A forwarded 9-byte buffered body with `max_request_body_bytes(8)` gets 413, and the origin records no request.
6. Environment is Axum and workerd. A streamed forwarded upload over the cap gets 413, and the origin records no request. On Viceroy 0.21.0 and Spin 4.1, the origin never records a complete request.
7. Environment is Axum. An extractor route with a custom error renderer answers an oversized body with 413, and the response has no `Retry-After`.
8. Environment is Axum, using the generated scaffold and the demo app. An over-cap request to an unmatched route gets 413.
9. Trusted Server end to end. With TS reading bodies through EdgeZero's readers and mapping the overflow category to its request-too-large error, the read-cap 413 tests pass unchanged. They are `ts:crates/trusted-server-core/src/auction/endpoints.rs:1771-1791`, `proxy.rs:6343` and `:6389`, `request_signing/endpoints.rs:995`, `:1034` and `:1073`, and `integrations/google_tag_manager.rs:2763-2817`. New TS cases get 413 for an oversized chunked `POST /auction` with no `Content-Length` and for an understated length on batch sync.

#### Out of scope

- Changing default caps.
- Changing how malformed or duplicate `Content-Length` is handled. EdgeZero rejects those with 400 before admission (`ingress.rs:892-921`).
- Changing how an upstream's own 413 response is surfaced.
- Making a streamed upload overflow invisible to the origin on Fastly and Spin.

#### Open questions

1. Should an outbound upload overflow be 413, or a server-error status? TS prefers 413. The documented use of the upload cap is bounding a forwarded client upload (`edgezero:docs/guide/proxying.md:32-35`), where the client is at fault. An application that builds its own oversized body can tell the two apart through EZ-10.7 and remap.
2. Should readers reject a declared oversized length before reading (EZ-10.4 is SHOULD)? TS prefers yes. It saves reading up to the cap, and with it TS can drop its own early checks in the auction, batch-sync and GTM handlers.

#### Interim for Trusted Server

Until EZ-10 lands, TS maps the 400 from `RequestContext::body_bytes` and `Body::into_bytes_bounded` to its own 413. Today those two calls return 400 only for overflow, because read failures pass through with their own categories (`body.rs:99`, `context.rs:662`). TS reads JSON with `body_bytes` and then parses it, so malformed input stays a separate 400. TS keeps its early `Content-Length` checks, which are header checks, not transport plumbing.

### EZ-11. Cache tagging with surrogate keys

**Priority:** Follow-up
**Adapters:** Fastly must honor tags. Axum and Spin accept them with no effect, since they have no intermediary cache. Cloudflare is Unsupported.
**Related:** EZ-8 (EZ-8.6 rejects tags combined with Image Optimizer), EZ-9 (tags carry to redirect hops), EZ-6 and EZ-15 (EZ-11.4 owns the rule that tags behave the same on begun requests and, once EZ-15 lands, fire-and-forget sends)

#### Need

Trusted Server can let publisher origin HTML enter Fastly's cache and tag it for purge. `apply_origin_cache_intent` (`ts:crates/trusted-server-core/src/publisher.rs:4356-4394`) bypasses for non-shareable documents and tags shareable ones with `ts-template ts-template-readerurl-<hex>` (`:4384-4391`, keys `ts:crates/trusted-server-core/src/platform/template_cache.rs:46`, `:234-239`). Both origin send sites use it (`publisher.rs:4852-4872`, `:5160-5177`). The cache intent is one enum on purpose, because on Fastly a surrogate key overrides pass, so "bypass and tag" would silently mean "do not bypass" (`ts:crates/trusted-server-core/src/platform/http.rs:10-29`). On an invalid key the Fastly adapter falls back to bypass rather than store an object no purge can reach (`ts:crates/trusted-server-adapter-fastly/src/platform.rs:452-473`). It never sets a TTL (`:448-451`).

After the cut Trusted Server sets a cache policy of "default plus these tags" on the origin `OutboundRequest`, and falls back to `OutboundCachePolicy::Bypass` if the tags are invalid. Without EZ-11 it can only pick `Bypass` or the default, and the default would cache pages that no purge can remove.

#### Gap at F

`OutboundCachePolicy` has only `Bypass` and `PlatformDefault` (`edgezero:crates/edgezero-core/src/outbound.rs:109-115`). Fastly maps `Bypass` to cache pass (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:735-750`) and Cloudflare maps it to no-store (`edgezero:crates/edgezero-adapter-cloudflare/src/outbound.rs:419-436`). Axum and Spin have no intermediary cache (spec `edgezero:docs/superpowers/specs/2026-05-21-outbound-http-design.md:1045-1051`). The Fastly SDK supports tagging. A surrogate key overrides pass and extends backend `Surrogate-Key` headers (`fastly-sdk:src/http/request.rs:2481-2501`). Keys are printable ASCII without spaces, and invalid keys are silently ignored (`:2486-2490`). A cache override sets TTL and stale-while-revalidate only when they are provided (`fastly-sdk:src/http/request/handle.rs:1834-1845`), so a key alone sets neither. Whether the override's documented effect on cache directive headers (`:939-942`) changes origin freshness for a key-only override is **Unverified**. The tag reaches the host on handle-level sends too (`:943-954`, `:1815-1866`). Workers fetch has no cache-tag field in worker 0.8.5 (`worker:src/request_init.rs:116-166`).

#### Requirements

- **EZ-11.1** An application MUST be able to ask, per request, for the default cache behavior plus one or more purge tags. Asking for bypass and tags together MUST be impossible to express or MUST be rejected before sending.
- **EZ-11.2** Tags MUST be validated when the application sets them. There is at least one tag, each tag is non-empty, and each contains only bytes `0x21` to `0x7E`. Invalid tags MUST be a client-class error at that point and never silently ignored. Duplicates collapse while keeping order.
- **EZ-11.3** Tagging MUST behave exactly like the default policy plus tags on any object the intermediary cache stores for the exchange. It MUST NOT force caching, set a TTL, or add any origin-visible request header.
- **EZ-11.4** On Fastly, tags MUST take effect on every send mode, including single send, begun request, batch slot, redirect hop and, once EZ-15 lands, fire-and-forget send, and a purge by any one tag MUST remove the stored object. Capability Native. This requirement owns the same-semantics rule for tagging across send modes.
- **EZ-11.5** On Axum and Spin, tags MUST be accepted and have no effect. Capability Native, because no untagged object can be stored.
- **EZ-11.6** On Cloudflare, tags MUST get cross-cutting requirement 2's rejection, first in preflight and before any fetch, naming the option and the adapter. Capability Unsupported, because degrading to the default would store untagged objects.
- **EZ-11.7** Removed.
- **EZ-11.8** Tags MUST carry to every redirect hop (EZ-9.9).
- **EZ-11.9** Documentation MUST add the capability row, describe the three cache intents, and state that tagging never forces caching.

#### Acceptance criteria

- Unit. No tags, an empty tag, `a b`, a non-ASCII tag and `0x7F` are client-class errors. `ts-template` with a reader-URL tag is accepted. `a`, `a` collapses to `a`.
- Viceroy 0.21.0. A tagged single send and a tagged two-slot batch complete without error.
- workerd. A tagged request returns the rejection, and no fetch is made.
- Axum. A tagged request succeeds, and the origin receives no `surrogate-key` or `cache-control` header added by EdgeZero.
- Hosted Fastly. Given a cacheable origin path that counts the requests it receives, two fetches tagged `probe-key` leave the origin count at one. After a purge of `probe-key`, a third fetch raises it to two. The same holds when the fetches are begun requests and when they are batch slots.
- Trusted Server end to end. With readthrough enabled on a Fastly staging service, purging `ts-template` turns a cached page into a miss, and the origin sees the request again. Trusted Server unit tests assert the tagged policy at both origin send sites, using EZ-24 if it has landed or Trusted Server's own double otherwise.

#### Out of scope

- TTL overrides and forced caching.
- A purge API.
- Cloudflare cache tags.

#### Open questions

- **Q11.1** Which length limits apply to a tag and to the whole set? Trusted Server prefers 1024 bytes per tag and 16 KiB in total, rejected when set. These are **Unverified** Fastly limits. A hosted probe with an oversized key, or Fastly's surrogate key documentation, would confirm them. The SDK documents only the byte set (`fastly-sdk:src/http/request.rs:2486-2490`).
- **Q11.2** Should Cloudflare support tags through a raw `cf.cacheTags` property? Trusted Server prefers not now. worker 0.8.5 has no typed field, and whether the runtime honors it on subrequests is **Unverified**.

#### Interim for Trusted Server

Until EZ-11 lands, Trusted Server rejects `origin_readthrough_enabled = true` at config validation. Readthrough is off by default (`ts:crates/trusted-server-core/src/creative_opportunities.rs:419-426`), and no local operator configuration enables it. Document fetches keep today's bypass-or-default behavior.

### EZ-12. TLS certificate verification mode

**Priority:** Follow-up
**Adapters:** Fastly must honor the mode. It is Unsupported on HTTPS for Axum, Cloudflare and Spin.
**Related:** EZ-2 (EZ-2.1 requires matching TLS settings, and the mode is one of them), EZ-9 (the mode carries to redirect hops)

#### Need

`proxy.certificate_check` defaults to `true` and is documented as the switch "for local development with self-signed certificates" (`ts:crates/trusted-server-core/src/settings.rs:1762-1790`). It reaches four sites. Those are asset routes (`ts:crates/trusted-server-core/src/proxy.rs:1136`), every redirect hop (`:1337`), the publisher origin (`ts:crates/trusted-server-core/src/publisher.rs:4516`) and pull sync (`ts:crates/trusted-server-core/src/ec/pull_sync.rs:237`). Integrations and auction providers always verify (`ts:crates/trusted-server-core/src/integrations/mod.rs:53-66`, `ts:crates/trusted-server-core/src/auction/plan.rs:396`). On Fastly, `false` omits the certificate hostname check and keeps TLS, and keeps SNI for hostnames only (`ts:crates/trusted-server-adapter-fastly/src/backend.rs:243-254`, SNI `:245-247`). Fastly logs an INSECURE warning at startup (`ts:crates/trusted-server-adapter-fastly/src/app.rs:240-246`). Axum, Cloudflare and Spin ignore the setting (`ts:crates/trusted-server-adapter-axum/src/platform.rs:290-301`).

Fastly still validates the chain without the hostname check. That validation "is not optional" (`fastly-sdk:src/backend/builder.rs:176-190`, `:219-229`). So the setting's real effect is that an origin is accepted when its certificate chains to a public root but names a different host. The common case is an origin reached by IP or an internal name. Viceroy is stricter. With no certificate host given it checks the SNI hostname (`viceroy:src/wiggle_abi/req_impl.rs:535-569`), or the backend URI host when there is no SNI either (`viceroy:src/upstream.rs:188-192`), so locally the setting changes nothing.

After the cut Trusted Server asks for this mode at the four sites when `certificate_check = false`. Without EZ-12, such a Fastly deployment fails every request to an origin whose certificate names another host with a 502 (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:935-944`). No local operator configuration sets `false`.

#### Gap at F

Every HTTPS request verifies the certificate against the URI host (`edgezero:crates/edgezero-core/src/outbound.rs:869-873`, Fastly backend `edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:349-357`). For an IP-literal URI F sends no SNI (`edgezero:crates/edgezero-core/src/outbound.rs:1189-1192`). There is no per-request choice. reqwest 0.13.4 with rustls allows skipping only the hostname check when the client also uses an explicit root set instead of platform roots, and fails to build otherwise (`reqwest:src/async_impl/client.rs:734-747`). Workers fetch has no TLS control (`worker:src/request_init.rs:12-27`). WASI HTTP request options have only three timeouts (`spin-sdk:wit/deps/http@0.3.0/types.wit:384-416`).

#### Requirements

- **EZ-12.1** An application MUST be able to choose, per request, between full verification (the default) and chain-only verification. Chain-only still validates chain and expiry against the platform's trust roots and skips only the hostname match. A mode MUST NOT disable chain validation.
- **EZ-12.2** On `http` URIs the mode MUST have no effect on any adapter. SNI MUST be unchanged by the mode.
- **EZ-12.3** On Fastly, a chain-only request MUST succeed against a publicly trusted certificate for another name, and MUST fail with a gateway-class 502 against a self-signed, expired or otherwise invalid certificate. Capability Native.
- **EZ-12.4** On Fastly the mode is one of the TLS settings that EZ-2.1 requires to match for connection sharing. EZ-2.1 owns that rule.
- **EZ-12.5** On Fastly, EdgeZero MUST log a warning naming the host the first time a chain-only connection setup is used in a sandbox.
- **EZ-12.6** On Axum, Cloudflare and Spin, chain-only on an `https` URI MUST get cross-cutting requirement 2's rejection, first in preflight and before sending. Capability Unsupported.
- **EZ-12.7** The mode MUST carry to every redirect hop (EZ-9.9).
- **EZ-12.8** Documentation MUST add the capability row, state exactly what chain-only checks, and warn that any publicly trusted certificate can then impersonate the origin.

#### Acceptance criteria

- Unit. Chain-only on an `http` URI sends normally on every adapter.
- Axum, workerd and Spin 4.1. Chain-only on `https` returns the rejection and nothing is sent.
- Hosted Fastly. An origin reached by IP whose certificate is valid for a hostname fails with full verification (502) and succeeds with chain-only. An origin with a self-signed certificate fails in both modes. Given an origin that logs the connection each request arrives on, a full and a chain-only request to it in one sandbox arrive on different connections. Viceroy cannot show the mode difference, because it checks the SNI hostname or the URI host whenever no certificate host is given.
- Trusted Server end to end. With `certificate_check = false` on a Fastly staging service whose origin is an IP-literal HTTPS URL, publisher pages load.

#### Out of scope

- Disabling chain validation.
- Custom trust roots.
- Chain-only on Axum.

#### Open questions

- **Q12.1** Should Axum support chain-only? Trusted Server prefers not now. reqwest needs an explicit root set for it (`reqwest:src/async_impl/client.rs:734-741`), which means a new bundled root dependency or new configuration, and Trusted Server's Axum adapter has always verified in full.
- **Q12.2** Should EdgeZero add custom trust roots for self-signed development origins? Trusted Server prefers a separate follow-up. Fastly supports a CA certificate per backend (`fastly-sdk:src/backend/builder.rs:231-241`), and that is the safe answer to what `certificate_check = false` was meant to do.

#### Interim for Trusted Server

Until EZ-12 lands, Trusted Server rejects `certificate_check = false` at config validation on every adapter, because F always verifies the hostname and Axum, Cloudflare and Spin never honored the setting. Trusted Server also corrects the setting's doc comment, which claims self-signed support (`ts:crates/trusted-server-core/src/settings.rs:1765-1767`).

### EZ-13. Spin host authority override

**Priority:** Follow-up (Trusted Server recommends closing it as documentation and evidence)
**Adapters:** Spin
**Related:** EZ-9 (a cross-origin redirect hop drops the override), EZ-25 (documentation)

#### Need

`publisher.origin_host_header_override` sets the `Host` sent while connecting to `origin_url` (`ts:crates/trusted-server-core/src/settings.rs:44-47`, `:209-216`). The publisher backend passes it as the host override (`ts:crates/trusted-server-core/src/publisher.rs:4510-4520`). After the cut it becomes `OutboundRequest::host_authority_override` on the publisher fetch. Axum and Fastly support that natively, and Cloudflare is BestEffort (`edgezero:docs/guide/capabilities.md:236`).

On Spin the setting has never worked. Trusted Server's Spin client strips `host` as forbidden by WASI HTTP (`ts:crates/trusted-server-adapter-spin/src/platform.rs:410-428`, `:482`), so the override is dropped and the URI authority is sent. EdgeZero rejects the override on Spin instead, so with the setting present every publisher request on Spin would fail. One deployment's local configuration sets the override, and that deployment runs on Fastly.

#### Gap at F

Spin rejects any override with a 400 before body polling (`edgezero:crates/edgezero-adapter-spin/src/outbound.rs:1433-1441`, batches `:1417-1431`). It sets one authority from the URI (`:509-535`). The spec and the matrix record Spin as Unsupported (`edgezero:docs/superpowers/specs/2026-05-21-outbound-http-design.md:1056-1060`, `edgezero:docs/guide/capabilities.md:236`, `:255-258`). WASI HTTP 0.3 in spin-sdk 7.0.0 offers no way to send a `Host` other than the connection authority.

- A request has one authority, set as "the authority of the Request's target URI" (`spin-sdk:wit/deps/http@0.3.0/types.wit:336-344`).
- Request options carry only three timeouts. There is no connect-target, proxy or address override (`:384-416`).
- Whether a `host` field may be set is implementation-defined. Fields can be forbidden (`:112-114`, `:209-210`), and the host must reject requests with headers "not permitted to be sent" (`:300-304`). Trusted Server's comment says WASI HTTP 0.2 forbids `host` (`ts:crates/trusted-server-adapter-spin/src/platform.rs:412-413`). That is **Unverified** for Spin's host. The source to check is wasmtime's `wasi-http` crate, `crates/wasi-http/src/types.rs`, its forbidden-header check, which is not available locally.
- Even if `host` were accepted, nothing in the interface says the host would send it instead of the authority. That is also **Unverified**.

#### Requirements

- **EZ-13.1** Spin MUST keep rejecting a host authority override before anything is sent, on every send mode. It MUST NOT drop the override silently, and MUST NOT send the connection to the override's host.
- **EZ-13.2** The rejection MUST be cross-cutting requirement 2's rejection, first in preflight, naming the option and the adapter. The status stays 400.
- **EZ-13.3** EdgeZero MUST establish and record, on Spin 4.1, whether a component can set a `host` request field and what `Host` an origin receives for a request.
- **EZ-13.4** The capability matrix footnote MUST state that Unsupported is final for spin-sdk 7.0.0 and WASI HTTP 0.3.0, cite the interface evidence and the EZ-13.3 results, and name the change that would reopen it. That change is a WASI request option for the connect target or a proxy, or a Spin runtime setting that maps an authority to an address. The Spin guide (`edgezero:docs/guide/adapters/spin.md:92-94`) and the proxying guide (`edgezero:docs/guide/proxying.md:89-92`) MUST say the same.

#### Acceptance criteria

- Spin 4.1. A request with an override, sent alone or in a batch, returns the rejection with status 400, and no WASI request is made.
- Spin 4.1. A component that sets a `host` field records whether the host forbids it. A request to a local origin records the `Host` received, which is expected to equal the authority. Both results appear in the footnote.
- Trusted Server end to end, Spin 4.1. A Spin build whose config sets `publisher.origin_host_header_override` fails config validation with a message pointing to this limitation. A Spin build without it serves publisher pages.

#### Out of scope

- Implementing the override on Spin.
- A fallback that silently sends the URI authority instead of the override.

#### Open questions

- **Q13.1** Should an override equal to the URI authority be treated as absent on every adapter? Trusted Server prefers yes. It removes a Spin failure with no semantic content. The comparison must ignore a default port, as target canonicalization does (`edgezero:crates/edgezero-core/src/outbound.rs:1622-1627`). If adopted, EZ-13.1 applies only to an override that differs from the URI authority.
- **Q13.2** Should EZ-13 stay on the follow-up list? Trusted Server prefers closing it as documentation and evidence, since spin-sdk 7.0.0 offers no way to implement it.

#### Interim for Trusted Server

Trusted Server rejects `publisher.origin_host_header_override` at config validation on Spin builds. Fastly and Axum honor it natively.

### EZ-14. Idle-time bound for streamed response bodies

**Priority:** Follow-up
**Adapters:** Fastly, Axum, Cloudflare, Spin. Axum must be Native. The others may be BestEffort where host timers or cancellation cannot be shown to meet the requirement.
**Related:** EZ-2 (exact deadlines on Fastly, which this requirement must not undo for headers), EZ-4 (the connect bound stays separate), EZ-6 (a begun streamed request), EZ-6.27 (batches refuse streamed responses), EZ-9 (only the final response body of a followed chain is read), EZ-5 (the idle bound applies to the stream as delivered, encoded or decoded)

#### Need

TS today bounds connection setup and silence, not total stream time. Connect is 1 s (`ts:crates/trusted-server-adapter-fastly/src/backend.rs:240`). First byte and between bytes are 15 s each. The publisher origin sets them at `ts:crates/trusted-server-core/src/publisher.rs:98` and `:4517-4518`. The asset proxy and redirect loop set them at `ts:crates/trusted-server-core/src/proxy.rs:1137-1138` and `:1338-1339`. Integrations take the same 15 s default (`ts:crates/trusted-server-core/src/platform/mod.rs:82`, applied at `ts:crates/trusted-server-core/src/integrations/mod.rs:65` and copied into between bytes at `:157-158`). A streamed page or asset can take as long as it needs while bytes keep arriving.

On PR 275 a streamed body shares the request's total budget, measured from the start of the send. That budget is the per-call timeout, the deadline, or 30 s by default. Without this requirement:

- A streamed page, asset, or relayed script that takes longer than the budget is cut mid-body with a gateway timeout, however steadily the origin sends.
- Time the application spends between reads counts too, which includes TS's own rewrite work and a slow client draining the response. So a fast origin feeding a slow client is cut.
- A stalled origin is noticed only when the whole budget runs out. With a budget large enough for long transfers, for example 60 s, a stall ties up the request for 60 s instead of TS's 15 s today.

TS will ask for a header budget of about 15 s, a connect bound of 1 s (EZ-4), and an idle bound of 15 s on streamed bodies, optionally with an absolute deadline as a hard total.

#### Gap at F

- The budget is computed once from the send start (`edgezero:crates/edgezero-core/src/time.rs:138-187`, 30 s default at `:13`). Every body read checks it:
  - Fastly checks before and after each read (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:1203-1267`).
  - Axum checks per read and through `reqwest`'s per-request timeout, which covers the body (`edgezero:crates/edgezero-adapter-axum/src/outbound.rs:270-306`, `:478-484`; `edgezero:docs/superpowers/specs/2026-05-21-outbound-http-design.md:2281`).
  - Cloudflare checks per read (`edgezero:crates/edgezero-adapter-cloudflare/src/outbound.rs:439-496`).
  - Spin checks per read (`edgezero:crates/edgezero-adapter-spin/src/outbound.rs:829-870`).
- At F, Fastly sets the platform between-bytes timer to the whole budget (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:718-733`), and Spin sets every phase timer to the remaining budget (`edgezero:crates/edgezero-adapter-spin/src/outbound.rs:67-74`).
- Viceroy 0.21.0 enforces that between-bytes timer. It measures the gap since the last chunk Viceroy delivered to the guest, not since the last byte the origin sent (`viceroy:src/body.rs:235-260`, reset at `:310`). So a guest that pauses reading longer than the timer can fail its next read even when the origin is healthy. When the timer fires during a body read, F reports a 502 transport error, or a 504 if the budget has also passed (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:1219-1228`). Viceroy's first-byte timer covers connect and the response head, and its expiry reaches the application as an internal error (`viceroy:src/upstream.rs:351-375`; `viceroy:src/error.rs:327-329`; `edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:959`).
- No streamed-response option separates the header phase from the body phase (`edgezero:crates/edgezero-core/src/outbound.rs:91-95`, `:1200-1203`).
- EZ-2 makes the total deadline exact on Fastly, and EZ-4 adds a connect bound. Neither releases body reads from the total budget.

#### Requirements

- **EZ-14.1** An application MUST be able to ask, per streamed request, that the body be bounded by idle time instead of the request's total budget. The default MUST stay as at F.
- **EZ-14.2** The idle bound MUST apply only to streamed responses. A single send or begun request that asks for it in buffered mode MUST be rejected before sending as an invalid request (400 class). A batch slot that asks for it is refused by the batch's streamed-response rule (EZ-6.27) before the idle bound is considered. A zero idle duration MUST be rejected as an invalid request. A duration beyond EdgeZero's far-future clamp MUST be clamped, as F clamps other durations.
- **EZ-14.3** With an idle bound, the request's budget (per-call timeout, deadline, or default) MUST still bound everything up to the arrival of response headers, exactly as without it.
- **EZ-14.4** After headers arrive, each body read MUST produce data, end, or fail within the idle duration, measured from when that read starts waiting for upstream data. Time the application spends before its next read MUST NOT count toward the idle bound.
- **EZ-14.5** With an idle bound, an application-set absolute deadline MUST still end the body as a hard total. The per-call timeout and the 30 s default MUST NOT end the body.
- **EZ-14.6** An idle expiry MUST surface as a gateway timeout (504 class) whose timeout cause is distinguishable from the per-call timeout, the deadline, and the batch cutoff. A failure the platform reports after the idle duration has passed MUST be reported as that idle timeout, not as a 502 or an internal error.
- **EZ-14.7** The idle bound MUST track upstream data arrival. Decoding (EZ-5) or chunk shaping MUST NOT make a progressing stream look idle, and MUST NOT hide a stall.
- **EZ-14.8** On every adapter, a body that keeps producing data at intervals shorter than the idle duration MUST NOT be ended by any EdgeZero or platform timer. That holds even when the idle duration is longer than the per-call timeout, and even when the application pauses between reads. The two limits that still apply are the application's deadline and any platform execution limit the capability matrix documents, such as Fastly's runtime limit (EZ-2.7). On Fastly, asking for an idle bound no longer than the per-call timeout SHOULD NOT reduce connection reuse to that origin compared with the same request without it (EZ-2).
- **EZ-14.9** An application MUST be able to tell, before sending, whether a request carries an idle bound and its duration.
- **EZ-14.10** Requests that do not ask for an idle bound MUST behave exactly as at F on every adapter, apart from EZ-2's exact deadlines.
- **EZ-14.11** On a begun request (EZ-6), the idle bound MUST govern the body once the application reads it. With redirect following (EZ-9), it MUST govern the final response's body. Intermediate redirect bodies are never delivered.
- **EZ-14.12** The capability matrix MUST list idle-bounded streamed bodies per adapter (cross-cutting requirement 9). Axum is Native. Cloudflare and Spin are BestEffort until cancellation is shown to be bounded. Fastly is BestEffort until hosted Fastly shows that a slow reader does not trigger a platform timeout. The streaming guide MUST say that an idle bound without a deadline places no EdgeZero limit on total stream time, MUST name each adapter's platform execution limit, and MUST recommend a deadline for untrusted origins.

#### Acceptance criteria

1. **Unit.** Given an idle bound on a buffered single send, or a zero idle duration, then each request is rejected before sending as an invalid request. Given an idle bound on a batch slot, then the slot is refused by the batch's streamed-response rule.
2. **Axum.** Given an origin that sends headers and then 8 chunks 250 ms apart, a per-call timeout of 500 ms, and an idle bound of 400 ms, then the full body arrives in about 2 s.
3. **Axum.** Given the same setup with one 600 ms gap, then the read fails with the idle timeout about 400 ms after the previous chunk, and the cause differs from a per-call timeout.
4. **Axum.** Given a fast origin and an application that waits 1 s between reads, with a 400 ms idle bound, then no idle error occurs.
5. **Axum.** Given an idle bound and a deadline 1 s after the send, then a body still flowing at 1 s ends with a deadline timeout.
6. **Viceroy 0.21.0, workerd, Spin 4.1.** Run scenarios 2, 3, and 4 against an origin that trickles its body. Viceroy enforces the platform between-bytes timer and measures it from delivery to the guest, so scenario 2 shows that EdgeZero sets no platform timer that ends a progressing body early, and scenario 4 shows that a paused reader is not failed (EZ-14.8).
7. **Hosted Fastly.** Run scenarios 2 to 4 against a real origin. Add an idle bound longer than the per-call timeout, with chunks spaced between the two. No platform timer may end the body early.
8. **Trusted Server, Viceroy 0.21.0 and hosted Fastly.** An origin streams an HTML page over 20 s in chunks 2 s apart, and the client receives the full rewritten document. A 16 s stall mid-body ends the client body at about 15 s, and TS logs an idle timeout.

#### Out of scope

- An idle mode for buffered responses. A buffered send returns only after the whole body, so a total budget is the natural contract, and batches rely on it.
- EdgeZero's response write deadline, which the lifecycle requirements own.

#### Open questions

None.

#### Interim for Trusted Server (follow-ups only)

TS sets an explicit per-call timeout on each streamed site, sized for its longest expected transfer, together with EZ-4's 1 s connect bound. It raises its response write deadline through `App::set_response_egress_policy` (`edgezero:crates/edgezero-core/src/app.rs:466-476`) to match. TS accepts two costs until this lands. A mid-body stall is noticed only when that timeout expires, and a transfer longer than the timeout is cut.

### EZ-15. Explicit fire-and-forget send

**Priority:** Follow-up
**Adapters:** Axum, Cloudflare, Fastly, Spin. Lifetime after the callback returns may differ per platform, as EZ-15.11 says.
**Related:** EZ-6 (dropping a begun request cancels it, so fire-and-forget must be explicit; EZ-6.1 defines "on its way"), EZ-1 (a bodiless GET/HEAD built from an inbound request is accepted), EZ-2 (Fastly backend capacity), EZ-3 (EZ-3.3 owns the passed-deadline rule), EZ-8 (Image Optimizer is Unsupported on this send mode), EZ-16 (provider detail in logs)

#### Need

TS's Fastly Tinybird telemetry sends one NDJSON POST per auction and never waits for the answer (`ts:crates/trusted-server-adapter-fastly/src/tinybird.rs:133-147`). The backend has 2 s first-byte and between-bytes timers (`:19-20`). The request carries a bearer token (`:109-115`). TS awaits the sink (`ts:crates/trusted-server-core/src/auction/telemetry.rs:526-542`). Calls sit inside the `</body>` hold and the non-HTML collection, before the page tail is released (`ts:crates/trusted-server-core/src/publisher.rs:4283`, `:4227`). Others run on the dispatch-failure, skip, and abandon paths (`:4177`, `:4979`, `:4995`, `:5025`). Only Fastly has a sink today. The no-op sink (`ts:crates/trusted-server-core/src/auction/telemetry.rs:500`) is the default on the other adapters (`ts:crates/trusted-server-core/src/platform/types.rs:473`).

On PR 275's APIs, TS wants one telemetry sink on every adapter that sends the POST and returns at once, wherever the row is produced. Without this requirement, a normal `send` before delivery holds the page tail for the Tinybird round trip. Dropping a begun request is a cancellation (cross-cutting requirement 6), so it cannot stand in for fire-and-forget.

#### Gap at F

- `HttpClient` has no fire-and-forget operation (`edgezero:crates/edgezero-core/src/outbound.rs:567-615`).
- Fastly `send` waits for the response head (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:450-453`). On Axum, Cloudflare, and Spin a dropped, unpolled batch sends nothing (`edgezero:crates/edgezero-adapter-axum/src/outbound.rs:200-206`; `edgezero:crates/edgezero-adapter-cloudflare/src/outbound.rs:389-395`; `edgezero:crates/edgezero-adapter-spin/src/outbound.rs:434-440`).
- EdgeZero's guide warns that request cancellation does not cancel application-spawned background work (`edgezero:docs/guide/adapters/overview.md:252-255`), and EdgeZero offers no bounded alternative.

#### Requirements

- **EZ-15.1** An application MUST be able to send a request whose response no one observes. The call MUST return without waiting for the response, and when it returns the request MUST be on its way (EZ-6.1).
- **EZ-15.2** The call MUST reject, before sending and with an immediate error, a streamed request body that would be uploaded, and Image Optimizer options (Unsupported on this send mode, rejected as cross-cutting requirement 2 says, before any other preflight check). A GET or HEAD body that EZ-1 settles as bodiless is accepted. A request whose deadline has already passed fails as EZ-3.3 and EZ-6.26 say.
- **EZ-15.3** The request's budget MUST follow the same rules as `send`, with a 30 s default when the request sets no timeout or deadline. Each adapter MUST cap it at a documented platform lifetime, and the outcome log MUST say when that cap ended the request.
- **EZ-15.4** The request MUST keep running after the application's per-request lifecycle callback returns, until it finishes or its budget ends, wherever the platform allows. This is the lifetime cross-cutting requirement 4 leaves to EZ-15.
- **EZ-15.5** EdgeZero MUST NOT hold more than 64 KiB of the response. It reads and discards at most that much, then releases the response.
- **EZ-15.6** EdgeZero MUST log each outcome, success at `debug` and failure at `warn`, with the target host, the method, the error category, and EdgeZero's own gateway reason or timeout source. EZ-16 provider detail follows EZ-16.7. The log MUST NOT contain the path, the query, any header value, or body bytes.
- **EZ-15.7** In-flight fire-and-forget sends MUST be bounded, by default to 16 per request lifetime and 64 per sandbox or server instance. A call over either bound MUST fail immediately with a service-unavailable error (503 class) and send nothing. The application MUST be able to change both bounds.
- **EZ-15.8** The call MUST work inside a streamed response body that EdgeZero is writing, and after delivery inside the same lifecycle callback.
- **EZ-15.9** Follow-up within EZ-15, required before TS enables reusable sandboxes. On a reused Fastly sandbox, fire-and-forget sends MUST NOT accumulate beyond the per-instance bound, and finished ones MUST stop counting against it.
- **EZ-15.10** EdgeZero's documentation MUST say that dropping a begun request or a batch cancels it and that fire-and-forget uses this capability.
- **EZ-15.11** Per-adapter levels. Axum MUST be Native, and server shutdown MUST stop outstanding sends without waiting for them. Cloudflare, Fastly, and Spin MAY be BestEffort, with the capability matrix stating each platform's lifetime after the callback returns.

#### Acceptance criteria

1. Unit. A fire-and-forget POST with a streamed body fails immediately and sends nothing. A fire-and-forget GET built from a bodiless inbound GET is sent. With a per-request bound of 2, the third call fails with the service-unavailable error and only two requests go out.
2. Axum. Given a handler that makes a fire-and-forget call to an origin that answers after 200 ms and then returns 200, the client gets its response in under 50 ms, and the origin receives the request within 1 s. Given a server shutdown with one such send in flight, shutdown returns within 1 s.
3. workerd, Spin 4.1, and Viceroy 0.21.0. Given a handler that makes a fire-and-forget call to an origin that holds the request for 500 ms before answering, and then returns, the client's response completes before the origin answers, and the origin still receives the whole request. The Spin result sets the Spin capability level.
4. Viceroy 0.21.0 and hosted Fastly. Under Viceroy, given a reused guest serving 6 requests, each making one fire-and-forget call to an origin that answers after 1 s, every call reaches the origin and the sixth request still sends. On hosted Fastly, given 500 sequential requests, each making one such call, every call reaches the origin.
5. Axum, workerd, Spin 4.1, and hosted Fastly. A fire-and-forget call to a refused port logs one `warn` line with the host and an unreachable-category error. On Viceroy 0.21.0 the line carries the category Viceroy produces, and the run records it. No line contains a path, query, or header value.

The Trusted Server end-to-end check runs on the Axum binary, workerd, Spin 4.1, and Fastly staging. It points Tinybird at a mock that answers after 1 s. An auctioned page on each adapter finishes its tail within 50 ms of the auction finishing, and the mock receives one NDJSON POST per auction.

#### Out of scope

- A completion callback. TS needs only the log line.
- Running arbitrary application work after the callback returns. Only outbound requests are covered, within the budget and bounds above.

#### Open questions

1. Should the Fastly part be Blocking? The Fastly part is EZ-15.1 to EZ-15.8 and EZ-15.10 as they apply on Fastly, plus the Fastly clause of EZ-15.11. EZ-15.9 stays tied to reusable sandboxes. TS prefers that it stay Follow-up. Its interim sends the telemetry row with an ordinary `send` after delivery on Fastly, which holds nothing the client waits on. Promotion matters only if telemetry must leave before delivery.
2. Are 16 per request lifetime and 64 per instance the right defaults? TS prefers them. TS sends a few telemetry rows per request, and the per-instance bound limits Fastly sandboxes and Axum servers.
3. Is 64 KiB the right response read limit? TS prefers it. Tinybird answers with a small JSON document.

#### Interim for Trusted Server

Until this lands, TS on Fastly carries the auction telemetry row out of the `</body>` hold and sends it with an ordinary `HttpClient::send` after delivery, inside its serving callback, as it does pull sync (cross-cutting requirement 4). On Axum, Cloudflare, and Spin TS keeps the no-op sink they use today. TS neither holds the page tail nor relies on dropping a begun request.

### EZ-16. Provider failure detail available to logs

**Priority:** Follow-up
**Adapters:** Fastly, Axum, Cloudflare, Spin. Fastly and Spin MUST be Native. Axum and Cloudflare may be BestEffort, because their providers expose less.
**Related:** EZ-17, EZ-21, EZ-10 (their not-sent source, framing reason, and payload limit reason should be read the same way once EZ-16 has landed), EZ-14 (an idle timeout can carry the same detail), cross-cutting requirement 7 (diagnostics stay off the wire)

#### Need

TS today logs the provider's own cause. Its Fastly client keeps the SDK's send error as the source of its error report on `send` (`ts:crates/trusted-server-adapter-fastly/src/platform.rs:520-523`, `:540-543`). On the select path that auction providers take, it formats the SDK error into the report's text (`:606-614`). TS logs the full report on auction provider failures (`ts:crates/trusted-server-core/src/auction/orchestrator.rs:1422-1446`) and on every request error (`ts:crates/trusted-server-adapter-fastly/src/app.rs:1003-1005`). The SDK reports the specific cause, for example a DNS error with its rcode and info code, a TLS alert with its id, an HTTP/2 stream error with frame type and error code, a refused connection, or an I/O error (`fastly-sdk:src/http/request.rs:3215-3349`).

On PR 275, TS can log only EdgeZero's category. Operators can no longer tell an NXDOMAIN from a refused connection, an expired certificate, or a TLS alert, because all four become one "destination could not be reached" category. Timeouts and transport failures still classify correctly, which is why this is a follow-up.

#### Gap at F

- Fastly reduces every SDK cause to eight failure kinds with six distinct messages, and it discards body-read, upload, and backend-registration error detail (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:40-49`, `:68-99`, `:877-899`, `:929-966`, `:1189-1201`, `:1219`).
- Axum keeps only `reqwest`'s yes/no predicates (`edgezero:crates/edgezero-adapter-axum/src/outbound.rs:413-432`, `:457-464`).
- Cloudflare replaces a rejected fetch with one generic failure (`edgezero:crates/edgezero-adapter-cloudflare/src/outbound.rs:648`, `:1488-1493`).
- Spin drops every error payload (`edgezero:crates/edgezero-adapter-spin/src/outbound.rs:1342-1410`).
- `EdgeError` carries no detail for these failures (`edgezero:crates/edgezero-core/src/error.rs:132-192`). The wire is category-only for gateway, timeout, limit, and internal errors (`error.rs:555-575`, `:599-650`). EdgeZero's design forbids putting provider text into outbound diagnostics (`edgezero:docs/superpowers/specs/2026-05-21-outbound-http-design.md:3036-3040`).
- Provider text really does carry request data. `reqwest`'s error `Display` appends ` for url (<url>)` (`reqwest:src/error.rs:279-281`). Spin's TLS alert payload carries a free-text `alert-message`, and its DNS payload carries the rcode as text (`spin-sdk:wit/deps/http@0.3.0/types.wit:84-94`).
- Viceroy 0.21.0 ignores send error detail on the pending-request wait (`viceroy:src/wiggle_abi/req_impl.rs:1141-1149`). Under Viceroy a refused connection reaches F as an internal error with no cause (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:95-97`, `:959`).

#### Requirements

- **EZ-16.1** When EdgeZero reports an outbound failure that came from the provider, the application MUST be able to read structured detail about it from the error, without parsing any text. That covers backend registration, sending, waiting for headers, request-body upload, and response-body reads. This is a MUST on Fastly and Spin. On Axum and Cloudflare it holds as far as the provider exposes the cause.
- **EZ-16.2** The detail MUST name the adapter, the phase in which the failure occurred, and the provider's cause from a closed set that EdgeZero documents per adapter. It MUST include the provider's numeric codes where the provider supplies them. These are DNS rcode and info code, TLS alert id, HTTP/2 frame type and error code, platform status, and I/O error kind. Where the provider supplies a code only as text, such as Spin's DNS rcode, the detail MUST carry it only when it matches a documented closed set of values, and otherwise MUST record it as unknown.
- **EZ-16.3** The detail MUST NOT contain provider-supplied text, URLs, header values, or body bytes. That includes Spin's TLS `alert-message` and `reqwest`'s formatted error.
- **EZ-16.4** Cross-cutting requirement 7 applies to the detail. Error responses that EdgeZero renders MUST be byte-identical with and without it.
- **EZ-16.5** Existing error text MUST NOT change. That covers each error's `Display`, `message()`, `kind()`, `status()`, and its existing reason or cause, for every variant, including `EdgeError::Internal`.
- **EZ-16.6** The detail MUST stay with the error wherever F lets the error travel. That includes body-stream errors after decoding and limits, batch slot results and batch failures, begun requests, redirect hops, and, once EZ-15 lands, fire-and-forget sends. It also covers an error the request context reports again later.
- **EZ-16.7** EdgeZero MUST NOT log the detail itself above debug level. Log policy and request correlation belong to the application.
- **EZ-16.8** Once EZ-16 has landed, the not-sent source (EZ-17), the framing reason (EZ-21), and the payload limit reason (EZ-10) SHOULD be readable from an error in the same way as this detail.
- **EZ-16.9** The capability matrix MUST list failure diagnostics per adapter (cross-cutting requirement 9). Fastly and Spin are Native. Axum and Cloudflare are BestEffort.

#### Acceptance criteria

1. **Unit.** Given a gateway, timeout, and internal error, each with detail attached, then rendering each as a response produces the same bytes as without detail. Its `Display` is byte-identical. Its status, kind, and reason are unchanged.
2. **Unit.** Given a body stream that fails with detail, when the error passes through decoding, limits, chunk shaping, and buffered collection, then the final error still carries the detail.
3. **Hosted Fastly.** Given a request to a closed port on a reachable host, then the error is a gateway error with reason `Unreachable`, and its detail names Fastly, the send phase, and the SDK's connection-refused cause.
4. **Viceroy 0.21.0.** Given the same request, then the error is an internal error, because Viceroy ignores send error detail. Its detail names Fastly, the send phase, and an internal cause.
5. **Spin 4.1.** Given a TLS alert with an id and a message, then the detail carries the id and not the message. Given a DNS failure whose rcode text is outside the documented set, then the detail records the rcode as unknown. Given an internal error that carries text, then no text appears in the detail.
6. **Axum.** Given a refused connection to a URL that contains `?token=secret`, then the detail names a connect failure and the refused I/O kind, and `secret` appears in neither the detail nor the rendered error.
7. **workerd.** Given a rejected fetch, then the detail names Cloudflare and the send phase. Its cause is the exception kind only when that kind is in the documented set, and otherwise it is unknown.
8. **Trusted Server, hosted Fastly staging.** Given an auction with one provider pointed at a closed port on a reachable host, then TS logs one line with the provider name and the refused cause, and the client-facing auction error body stays static.

#### Out of scope

- Changing any error category, status, or wire message.
- Exposing provider free text in any form.

#### Open questions

1. Should Cloudflare report the JavaScript exception kind (for example abort versus type error) when it is in a documented allowlist? Trusted Server's preferred answer is yes. That one distinction lets operators separate EdgeZero's own aborts from network failures.

#### Interim for Trusted Server (follow-ups only)

TS logs EdgeZero's category, the gateway reason, and the timeout cause for each failure. It accepts losing the DNS, TLS, and refused distinctions until this lands.

### EZ-17. Fastly send outcome tells the application what was committed

**Priority:** Follow-up
**Adapters:** Fastly. Axum, Cloudflare and Spin expose no request-level send helper with preparation and finalization hooks, so the requirement does not apply to them.
**Related:** EZ-18 (decides which body is delivered for buffered responses). EZ-19 (the fallback the report names carries the application's headers). EZ-20 (removes declared-KV open failure as a "no response attempted" cause). EZ-21 (the report carries the framing reason). EZ-16 (cause detail is read the same way once it lands). EZ-2 and EZ-22 (change standard entry-point serving behavior; this requirement does not). EZ-2.14 (refers to EZ-17.7 for the helpers' inputs).

#### Need

TS's Fastly entry point sends every response itself and never fails after sending. A post-commit streaming failure is logged and the stream dropped (`ts:crates/trusted-server-adapter-fastly/src/main.rs:668-724`, failure branch `:710-717`). Partner pull sync then runs for real browsers whatever happened to the stream (`ts:crates/trusted-server-adapter-fastly/src/main.rs:512-517`, `:536-541`, `:618-643`). When request conversion fails, TS sends a plain 500 itself (`ts:crates/trusted-server-adapter-fastly/src/main.rs:472-477`). In reuse mode, which is off in production (`ts:crates/trusted-server-adapter-fastly/Cargo.toml:13-18`), the sandbox keeps serving after a post-commit failure, because the callback returns `()` (`ts:crates/trusted-server-adapter-fastly/src/main.rs:131-142`).

On PR 275 TS calls `send_request_with_registries_and_hooks` from inside `serve_custom` or `run_custom`, and runs pull sync after it returns, inside the same callback, with the state `finalize` returned. Without this requirement:

1. A client that disconnects mid-page also cancels partner pull sync, although its EC `Set-Cookie` was already committed in the head.
2. TS cannot answer a request EdgeZero never attempted, because the same error also means "delivery failed after commitment". If TS sends nothing, the client gets a host-produced response. In reuse mode the sandbox also stops at the next wait (`fastly-sdk:src/http/serve.rs:357-364`, `:616-619`).
3. In reuse mode, TS cannot keep serving after a post-commit failure while stopping after a pre-send fault, short of matching error strings.
4. When framing fails after `finalize` added the EC cookie, EdgeZero delivers its 500 fallback and reports success, so TS would run pull sync for a cookie the browser never received.

#### Gap at F

- The helper returns `Result<Option<State>, fastly::Error>` (`edgezero:crates/edgezero-adapter-fastly/src/request.rs:557-586`). A declared-KV open failure and a head-conversion failure return `Err` before any response attempt (`:570`, `:632-652`, `:445-447`, `:904-906`). An admission abort returns `Err("ingress admission aborted")` (`:383`).
- A routed response runs `finalize`, then delivery, and a delivery error drops the state (`edgezero:crates/edgezero-adapter-fastly/src/request.rs:388-391`).
- Delivery errors collapse to four fixed messages with no commit information (`edgezero:crates/edgezero-adapter-fastly/src/response.rs:115-122`, `:252-257`, `:281-286`). A failure before the head reaches the host is reported as post-commit in one path (`:397-406`), and a host rejection of the head is treated as post-commit with no fallback (`:408-419`; test `:1547-1578`).
- A delivered fallback returns success (`edgezero:crates/edgezero-adapter-fastly/src/response.rs:281-284`, `:335`), although its terminal report says the fallback was delivered (`edgezero:crates/edgezero-core/src/response_egress.rs:846-867`).
- Detached responses run without the application's response-egress observer (`edgezero:crates/edgezero-core/src/app.rs:178-181`; `edgezero:crates/edgezero-core/src/response_egress.rs:661-676`). Only the admitted source-error path uses it (`edgezero:crates/edgezero-core/src/app.rs:86-102`, `:346-361`). An admission-policy error and an unsupported admission outcome also become detached responses (`edgezero:crates/edgezero-adapter-fastly/src/request.rs:486-495`).
- EdgeZero's own fixture labels every error as post-commit, including a declared-KV failure that sent nothing (`edgezero:tests/fixtures/reusable-app/crates/fixture-fastly/src/lib.rs:232-237`). The Fastly guide warns that an escaping error "is not proof that any response was sent" (`edgezero:docs/guide/adapters/fastly.md:444-446`, `:513-515`).

#### Requirements

- **EZ-17.1** After `send_request_with_registries_and_hooks`, `send_with_registries_and_hooks` or `FastlyService::send_request_with_hooks` returns, the application MUST be able to tell which of four things happened. No response was attempted. The application's admission or detached-egress policy chose to abort. A detached response was attempted, for a head limit, a framing violation, an admission refusal, an admission-policy error, or a source error. A routed response was attempted.
- **EZ-17.2** For a routed response, the value `finalize` returned MUST reach the caller in every case, including a delivery failure after commitment and a delivered fallback.
- **EZ-17.3** For every attempted response, the caller MUST learn the commit state of the body EdgeZero last tried to send. That state is one of not committed (the head never reached the host), unknown (the host rejected the head, and whether the client saw anything is unknown), or committed (the host accepted the head). The caller MUST also learn which body that was, the application's or EdgeZero's fallback, and the terminal outcome. The terminal outcome MUST equal the `ResponseEgressReport` that the request's `ResponseEgressCompletion` received for the same attempt and, for routed responses and admitted source errors, the report the application's observer received. Detached responses keep F's rule that the application's observer is not invoked.
- **EZ-17.4** "No response was attempted" MUST be reported if and only if EdgeZero started no response attempt, invoked no observer or completion, and sent nothing. The caller MAY then send its own native response, and in reuse mode the sandbox MUST be able to keep serving afterwards.
- **EZ-17.5** After an attempted response that was not committed, the caller MAY send its own native response. EdgeZero MUST document that the attempt's terminal report has already reached the completion, and the observer where EZ-17.3 says it does, and that the native response is not accounted by EdgeZero. After an unknown or committed outcome, EdgeZero MUST document that the caller must not send another response.
- **EZ-17.6** Two "no response attempted" causes MUST be distinguishable as categories. One is an explicitly required store that could not be opened. The other is a request head that could not be converted. The human-readable text MUST be fixed and contain no URI, header value or store name. Cause detail MUST be readable by the application without parsing text and MUST NOT reach the wire (cross-cutting requirement 7). Where EZ-16 has landed, it SHOULD be read the same way as EZ-16's detail.
- **EZ-17.7** `prepare` and `finalize` closures written against F MUST keep working with the helpers. Only what the helpers report changes.
- **EZ-17.8** `run_app`, `run_app_with_hooks`, `serve_app`, `serve_app_with_hooks` and `FastlyService::send` MUST keep F's serving behavior for every scenario in EZ-17.1, except where EZ-2 (sandbox retirement) and EZ-22 (initialization failure) change it.
- **EZ-17.9** Applications MUST be able to exercise every outcome in EZ-17.1 and EZ-17.3 in their own tests without a live host, through EdgeZero's `test-utils` lifecycle support at F (`FastlyService::run_request_with_hooks_for_test`, `edgezero:crates/edgezero-adapter-fastly/src/request.rs:231-249`) or its successor.
- **EZ-17.10** The Fastly guide's custom-lifecycle section and its complete example MUST describe the outcomes, the send permissions in EZ-17.4 and EZ-17.5, and a reuse-mode serving policy that keeps serving after post-commit failures.

#### Acceptance criteria

1. Viceroy 0.21.0. Given a routed handler whose body stream fails after its first chunk, and a `finalize` that returns a state value carrying the request's outbound client, when the request is delivered, then the client sees a partial body, and the caller learns routed, application body, committed, source error, and receives the state. An outbound request the callback then makes with that client completes before the callback returns (cross-cutting requirement 4).
2. Viceroy 0.21.0. Given a routed response that `finalize` gives a `Trailer` field, when it is delivered, then the client receives the 500 fallback, and the caller learns routed, fallback body, committed, fallback completed, and receives the `finalize` value.
3. Viceroy 0.21.0. Given an admission policy that refuses, the caller learns detached, application body, committed, `Completed`, and `finalize` never runs. Given a policy that aborts, the caller learns aborted, no response is sent, and `finalize` never runs.
4. Unit. For each failure point (deadline before the head is prepared, head preparation rejected, deadline before commit, host rejects the head, failure after the host accepted the head, fallback cannot start, fallback rejected by the host), the reported commit state, body and outcome match EZ-17.3, and the completion saw exactly one report equal to the reported one.
5. Viceroy 0.21.0. Given `FastlyService::new(&app).with_kv(name).require_kv()` on a store Viceroy does not link, the caller learns no response attempted, store category, sends a native 500, and the client receives it.
6. Unit and Viceroy 0.21.0. The standard entry points produce F's serving results for the scenarios above, apart from EZ-2 and EZ-22.
7. Trusted Server, Viceroy 0.21.0. Pull sync reaches its partner stub after a mid-body origin abort. No pull-sync call is made when a framing failure delivers the fallback. A request-conversion failure gets TS's own 500, if Viceroy can deliver such a request.

#### Out of scope

- Changing the standard entry points' policy of ending the sandbox on post-commit failures.
- Supplying or retaining the outbound client (EZ-2).
- Any response-returning or whole-body compatibility path.

#### Open questions

1. Should the standard entry points keep serving after a post-commit failure now that the cause is known? TS's preferred answer is no change in this requirement. TS uses the custom path and sets its own policy.
2. Should "unknown" be folded into "committed"? TS's preferred answer is no. Production behavior after a host rejection of the head is unverified, and folding it in would let a caller double-send or wrongly skip a send.
3. Should only EZ-17.2 be promoted to Blocking? TS's preferred answer is no. It changes the same report, so a partial version would be an API TS migrates twice. If TS needs pull sync after disconnects at the cut, promote all of EZ-17.

#### Interim for Trusted Server

TS logs every helper error and returns `Ok(())` from its callback. In reuse mode the sandbox then keeps serving after post-commit failures and stops at the next wait when nothing was sent. TS skips pull sync when the helper returns an error and runs it when the helper reports success. It accepts that pull sync may run after a delivered fallback, and that a head-conversion failure gets the host's response instead of TS's 500.

### EZ-18. `Content-Length` on buffered responses

**Priority:** Follow-up. TS's preferred answer on promotion is in open question 1.
**Adapters:** all four. Axum already meets EZ-18.1 at F. Fastly has the added parity requirement EZ-18.4.
**Related:** EZ-5 (F already sends an application- or upstream-supplied length, which EZ-18.2 and EZ-18.3 keep, so EZ-5 does not wait for EZ-18). EZ-17 (reports which body was delivered). EZ-19 (decorated responses get the same framing). EZ-Q1 (its criteria join the record when EZ-18 is present at the pinned commit).

#### Need

TS sends every buffered Fastly response with the SDK's `Response::send_to_client` (`ts:crates/trusted-server-adapter-fastly/src/main.rs:720-722`, after `ts:crates/trusted-server-adapter-fastly/src/compat.rs:13-43`). In the SDK's default Automatic framing mode, "a `Content-Length` is used when the size of the body can be determined before it is sent" (`fastly-shared:src/lib.rs:271-272`), and `send_to_client` sends the body as one unit (`fastly-sdk:src/http/response.rs:1735-1738`, `:1816-1821`). Under Viceroy that yields an exact-size body (`viceroy:src/wiggle_abi/resp_impl.rs:164-168`; `viceroy:src/body.rs:54-70`, `:412-427`), which hyper frames with `content-length` (`hyper-0.14.26:src/proto/h1/role.rs:840-883`). So today TS's auction JSON (`ts:crates/trusted-server-core/src/auction/formats.rs:516-527`), its 413 bodies (`ts:crates/trusted-server-core/src/auction/endpoints.rs:136-158`), discovery JSON, error pages and tsjs bundles carry a fixed length. HEAD and 304 responses go out the same way.

TS's cut spec keeps "same response status and headers" on Fastly (`ts:docs/superpowers/specs/2026-10-17-edgezero-pr275-hard-cutover-design.md:49-52`), and its Fastly response-diff gate allows only the listed Fastly differences (`:712-719`).

The tsjs bundles set no `Content-Length` (`ts:crates/trusted-server-core/src/http_util.rs:330-336`) and ask the Fastly host to compress them with `x-compress-hint: on` (`ts:crates/trusted-server-core/src/publisher.rs:597`). They are not at risk at F, where they go out chunked in Automatic mode. The hint matters only as a constraint on how buffered framing is restored (EZ-18.5).

The only reason left to promote the buffered part is header parity. TS knows of no functional breakage on its path from chunked buffered responses. Without EZ-18, TS lists "buffered responses lose `Content-Length` on Fastly" as an intended difference and normalizes it in its response diff.

#### Gap at F

- Core never derives a length from a buffered body. It only canonicalizes a length the application set (`edgezero:crates/edgezero-core/src/response_egress_framing.rs:80`, `:115-138`).
- Fastly commits the head (`edgezero:crates/edgezero-adapter-fastly/src/response.rs:408`) before it writes any body bytes (`:434`, `:486`), using the streaming send (`edgezero:crates/edgezero-adapter-fastly/src/fastly_response_abi.rs:64-70`). It selects manual framing only when the body is transmitted and a `Content-Length` header is already present (`edgezero:crates/edgezero-adapter-fastly/src/response.rs:159-161`), so HEAD, 204, 205 and 304 stay in Automatic mode even with a length. In Automatic mode a streamed response "will receive a `Transfer-Encoding: chunked`" (`fastly-shared:src/lib.rs:272-274`). Under Viceroy the body size is unknown, the guest's framing headers are stripped, and hyper sends `transfer-encoding: chunked` (`viceroy:src/body.rs:412-417`; `viceroy:src/http/framing.rs:68-70`; `viceroy:src/http/filtering.rs:3-8`; `hyper-0.14.26:src/proto/h1/role.rs:840-854`).
- EdgeZero's 500 and 504 fallbacks are buffered bodies without a length, so they are chunked too (`edgezero:crates/edgezero-adapter-fastly/src/response.rs:342-351`).
- HEAD and 304 at F use the same commit as other responses, while TS uses `send_to_client`. Under Viceroy hyper writes the same head for both (no `content-length` for an empty HEAD or 304, `hyper-0.14.26:src/proto/h1/role.rs:476-503`, `:840-892`), so only hosted Fastly can show whether the two differ. hyper does write a length for a HEAD whose body has a known non-zero size (`hyper-0.14.26:src/proto/h1/role.rs:490-499`, `:869-881`).
- A write deadline is checked before the head is prepared and before commit (`edgezero:crates/edgezero-adapter-fastly/src/response.rs:364`, `:389`). The only window in which a deadline truncates a buffered response is between commit and the body write (`:423-431`).
- In manual framing mode a `Content-Length` that disagrees with the body truncates it or hangs up the connection (`fastly-shared:src/lib.rs:285-287`). How manual framing interacts with host compression is unverified.
- Cloudflare sends a buffered body without a declared length as an unsized stream (`edgezero:crates/edgezero-adapter-cloudflare/src/response.rs:518-530`; test `:1050-1090`). Spin forwards headers and streams the body (`edgezero:crates/edgezero-adapter-spin/src/response.rs:367-389`). Axum already sends the length (`edgezero:crates/edgezero-adapter-axum/src/response.rs:278-299`).
- The streaming guide says EdgeZero "does not guarantee a particular HTTP/1 transfer coding" (`edgezero:docs/guide/streaming.md:167-169`).

#### Requirements

- **EZ-18.1** On every adapter, a transmitted response whose body is fully buffered MUST reach the client with `Content-Length` equal to the body's byte length, including `0`, and without `Transfer-Encoding`, on HTTP/1.1. On HTTP/2 it MUST carry `content-length`. This applies to application responses, rendered error responses, detached responses and EdgeZero's fallbacks.
- **EZ-18.2** An application `Content-Length` that matches a buffered body MUST be kept. A mismatch MUST keep F's behavior, a 500 fallback before commit.
- **EZ-18.3** Streamed bodies keep F's behavior. Without a declared length they use the provider's transfer framing. With a declared length, that length is sent and enforced, and a stream that runs long or short is aborted.
- **EZ-18.4** On Fastly, the framing the client receives for buffered responses and for bodiless responses (HEAD, 204, 205, 304) MUST match what the Fastly SDK's `Response::send_to_client` produces for the same status, headers and body, on HTTP/1.1 and HTTP/2. That is TS's pre-cut behavior, so the HEAD and 304 `Content-Length` behavior is whatever the host does for TS today.
- **EZ-18.5** When a response carries `x-compress-hint` and the Fastly host compresses it, the client MUST receive valid framing and a body that decompresses to the original bytes. EdgeZero MUST NOT cause the host to send a length that disagrees with the compressed body. This holds for buffered responses and for streamed responses with a declared length.
- **EZ-18.6** Except where EZ-18.4 requires otherwise on Fastly, EdgeZero MUST NOT add a length to a bodiless response (HEAD, 204, 205, 304) that the application did not supply. On Fastly, a HEAD whose application body has a known size gets the framing EZ-18.4's reference produces for that body. An application-supplied length on HEAD or 304 is passed to the host as at F.
- **EZ-18.7** On Fastly, a write deadline that expires after the head is committed and before a buffered body has been handed to the host SHOULD NOT truncate the response. The head and body SHOULD reach the host together, so an expiry before that point produces the 504 fallback.
- **EZ-18.8** The capability matrix MUST gain a row for buffered-response `Content-Length`. Axum is Native. Fastly is BestEffort until criterion 3 passes on hosted Fastly over HTTP/1.1 and HTTP/2, then Native. Cloudflare is BestEffort until a deployed Workers check shows the header reaching the client. Spin is BestEffort until a Spin 4.1 check shows it. `docs/guide/streaming.md` MUST state that buffered bodies carry `Content-Length`.

#### Acceptance criteria

1. Viceroy 0.21.0, HTTP/1.1. Given a buffered 200 route, a streamed route without a length, a rendered 404, and a routed response with a `Trailer` field, then the buffered route, the 404 and the 500 fallback (21 bytes, `internal server error`) carry `content-length` and no `transfer-encoding`, and the streamed route carries `transfer-encoding: chunked`.
2. Viceroy 0.21.0, HTTP/2 with prior knowledge, which Viceroy's hyper accepts by default (`hyper-0.14.26:src/server/conn.rs:950-955`). A buffered route carries `content-length`.
3. Hosted Fastly, HTTP/1.1 and HTTP/2, on TS's staging service. For a buffered 200, a buffered 200 with `x-compress-hint: on` requested with `Accept-Encoding: gzip`, HEAD and conditional-GET 304 of a buffered route, and a 204, the framing equals that of TS's pre-cut build. The compressed body decompresses to the expected SHA-256.
4. Viceroy 0.21.0. Given a buffered response whose response-owned `ResponseEgressDeadline` is already in the past, the client receives the 504 fallback (32 bytes, `response write deadline exceeded`) with `content-length: 32`.
5. workerd. A buffered route carries `content-length`. Spin 4.1. The same. Axum. Unchanged from F. EdgeZero records a passing run of each for the pinned commit. Its CI runs only the Fastly fixture smoke at F (`edgezero:.github/workflows/test.yml:399-401`).
6. Trusted Server, Viceroy 0.21.0 and hosted Fastly. Auction JSON, discovery JSON, a tsjs bundle and an error page match the pre-cut build's `content-length` and `transfer-encoding`. Publisher HEAD and 304 match the pre-cut build.

#### Out of scope

- Manual framing for HEAD and 304 beyond the parity in EZ-18.4.
- Any change to how streamed bodies are framed.
- Guessing a length for a streamed body.

#### Open questions

1. Promote the buffered part to Blocking? TS's preferred answer is Follow-up with the interim below. The only reason to promote is header parity and the response-diff gate. No functional breakage is known. If the TS maintainer holds the parity goal for the cut, promote EZ-18.1 and EZ-18.4.
2. Should the HEAD and 304 parity in EZ-18.4 be a MUST without hosted evidence? TS's preferred answer is yes. It is TS's current wire behavior, and criterion 3 checks it on hosted Fastly.

#### Interim for Trusted Server

TS lists "buffered responses lose `Content-Length` on Fastly, chunked on HTTP/1.1" as an intended Fastly difference in its cut spec, and its response diff normalizes `content-length` and `transfer-encoding` for buffered routes.

### EZ-19. Application headers on detached and egress-fallback responses

**Priority:** Follow-up
**Adapters:** all four, Native. The per-request form (EZ-19.2) needs a hook that runs before ingress checks, which only Fastly's canonical path has (`prepare`).
**Related:** EZ-17 (reports when these responses were delivered). EZ-18 (they still carry a length). EZ-21 (a set that breaks framing is diagnosable). EZ-22 (an initialization-failure response has no configured set).

#### Need

Every Fastly response TS sends today goes through TS's finalize steps, because TS sends every response itself (`ts:crates/trusted-server-adapter-fastly/src/main.rs:486-506`, `:557`, `:668-689`). Those steps add geo headers or `X-Geo-Info-Available: false`, `X-TS-Version`, `X-TS-Fastly-Version`, `X-TS-ENV: staging`, cookie cache privacy, and the operator's `response_headers` (`ts:crates/trusted-server-adapter-fastly/src/middleware.rs:187-237`), plus sandbox counter headers on `private, no-store` responses (`ts:crates/trusted-server-adapter-fastly/src/main.rs:238-269`).

On PR 275's canonical path, responses EdgeZero synthesizes skip `finalize`. These are 414 (absolute URL over 8192 bytes), 431 (over 100 header values or 64 KiB of headers), 400 framing rejections (`edgezero:crates/edgezero-core/src/ingress.rs:21-23`), admitted source errors, and the 500 and 504 egress fallbacks. A 500 caused by a bad operator header then carries no `X-TS-Version`, so an operator cannot tell which build produced it, and 414 and 431 lack the operator's headers. TS's spec accepted this on Axum, Cloudflare and Spin (`ts:docs/superpowers/specs/2026-10-17-edgezero-pr275-hard-cutover-design.md:408-411`) before Fastly moved onto the canonical lifecycle.

#### Gap at F

- `finalize` runs for routed responses only (`edgezero:crates/edgezero-adapter-fastly/src/request.rs:384-387`, `:475-477`, `:481-483`, `:498-502`).
- `App::set_error_response_renderer` receives only the error, so it cannot add per-request values such as geo, and egress fallbacks bypass it (`edgezero:crates/edgezero-core/src/app.rs:410-423`). Its doc says fallbacks bypass the renderer because "the application supplied those responses directly" (`:415-416`). That is true of admission refusals but not of fallbacks, which the adapter builds.
- Each adapter builds the fallback with no extension point (`edgezero:crates/edgezero-adapter-fastly/src/response.rs:342-351`; `edgezero:crates/edgezero-adapter-cloudflare/src/response.rs:418-427`; `edgezero:crates/edgezero-adapter-spin/src/response.rs:792-801`; `edgezero:crates/edgezero-adapter-axum/src/response.rs:704-732`, which also holds Axum's second fallback builder).
- The fallback runs under a separate 1 s budget and must not run application policy (`edgezero:crates/edgezero-core/src/response_egress.rs:17`; `edgezero:docs/superpowers/specs/2026-09-08-response-egress-design.md:454-461`).

#### Requirements

- **EZ-19.1** An application MUST be able to configure, once per app, header fields that EdgeZero adds to every response it synthesizes outside routed dispatch. These are detached ingress errors (head limits, framing, admission-start errors), admitted source errors, and egress fallbacks, including a HEAD fallback, which gets the headers and no body.
- **EZ-19.2** On Fastly's canonical path, the application MUST be able to supply the set for one request from `prepare`, before head limits are checked. A per-request set replaces the app-level set for that request.
- **EZ-19.3** The set MUST NOT be applied to routed responses, admission refusals (the policy builds those), or responses the host produces before the guest runs.
- **EZ-19.4** A set field whose name is already present on the synthesized response MUST be skipped, so a value EdgeZero or the renderer set (for example `Allow` on a 405, `Retry-After`, or `Cache-Control`) stays the only value of that name. Several values of one name within the set are all added, in order.
- **EZ-19.5** The set MUST NOT contain `Content-Length`, `Transfer-Encoding`, `Trailer`, `TE`, `Connection`, `Keep-Alive`, `Proxy-Connection`, `Upgrade`, `Proxy-Authenticate`, `Proxy-Authorization`, `Content-Type` or `Content-Encoding`, and MUST be bounded in field count and bytes. A violating set MUST be rejected when the application configures it, never at request time. EdgeZero documents the bounds.
- **EZ-19.6** Application code MUST NOT run while a fallback is built or sent. The fallback's status, body and 1 s budget MUST NOT change, and adding the set MUST NOT make a deliverable response undeliverable.
- **EZ-19.7** When application configuration itself fails (EZ-22), no configured set exists, and the initialization-failure response carries none. EdgeZero MUST document this next to that response.
- **EZ-19.8** The renderer's doc MUST distinguish admission refusals (application-built) from fallbacks (adapter-built).

#### Acceptance criteria

1. Viceroy 0.21.0. Given an app-level set, when a request arrives with a 9 KiB absolute URL, then the 414 carries the set. When a request arrives with about 72 KiB of headers in 60 lines, over EdgeZero's 64 KiB limit and under hyper's 100-line limit and 417,792-byte read buffer (`hyper-0.14.26:src/proto/h1/role.rs:27`; `hyper-0.14.26:src/proto/h1/io.rs:30`; Viceroy does not lower either), then the 431 carries the set. Do not use 101 header lines, because Viceroy's hyper answers those with its own 431 before the guest runs (`hyper-0.14.26:src/error.rs:564`; `hyper-0.14.26:src/proto/h1/role.rs:443-453`).
2. Viceroy 0.21.0. Given a routed response that carries a `Trailer` field, the 500 fallback carries the set and its body is unchanged. A HEAD request to the same route gets the fallback's headers with the set and no body.
3. Viceroy 0.21.0. Given a per-request set supplied in `prepare`, the 414 carries that set and not the app-level one.
4. Unit, each adapter. A fallback carries the app-level set. A rendered 405 keeps its own `Allow` and no second one. A set holding `Cache-Control` leaves a synthesized response's own `Cache-Control` as its only value.
5. Unit. Configuring a set containing any name in EZ-19.5, or exceeding the bounds, fails at configuration time.
6. Trusted Server, Viceroy 0.21.0. The 414 and 431 carry `X-TS-Version`, `X-TS-Fastly-Version`, geo and operator headers. The framing-failure 500 carries the same headers.

#### Out of scope

- Running application code for synthesized responses.
- Changing the fallback's status or body.
- Decorating admission refusals.

#### Open questions

1. Should a per-request set replace the app-level set or merge with it by name? TS's preferred answer is replace. One rule is easier to reason about, and TS builds the full set per request anyway.
2. Should admission refusals be decorated? TS's preferred answer is no. The policy builds them and can add headers itself.

#### Interim for Trusted Server

TS accepts that detached and fallback responses carry no TS headers on Fastly, as it already accepts on the other three adapters. TS may add `X-TS-Version` to detached errors through `App::set_error_response_renderer`. Fallbacks stay bare.

### EZ-20. A declared store that cannot open fails at use, not on every request

**Priority:** Follow-up
**Adapters:** Fastly MUST. Cloudflare and Spin SHOULD. Axum keeps its boot-time failure, which happens before it accepts any request.
**Related:** EZ-17 (an explicitly required store keeps the "no response attempted" outcome).

#### Need

TS's Fastly `Hooks::stores()` declares the KV store `trusted_server_kv` (`ts:crates/trusted-server-adapter-fastly/src/app.rs:1347-1362`; `ts:edgezero.toml:23-25`). Neither TS's `fastly.toml` (`ts:fastly.toml:25-42`) nor its Viceroy template (`ts:crates/trusted-server-integration-tests/fixtures/configs/viceroy-template.toml:9-59`) links it, and TS reads no EdgeZero KV store on Fastly (only Cloudflare and Spin call `kv_store_default()`, `ts:crates/trusted-server-adapter-cloudflare/src/platform.rs:613`, `ts:crates/trusted-server-adapter-spin/src/platform.rs:721`). Passing `stores()` to the canonical helper therefore fails every request except TS's health probe, which short-circuits first (`ts:crates/trusted-server-adapter-fastly/src/main.rs:173-176`). One unlinked store in a deployment would take down every route, including routes that never touch KV.

#### Gap at F

- Fastly opens every declared KV id per request and returns an error, with no response, on the first failure (`edgezero:crates/edgezero-adapter-fastly/src/request.rs:570`, `:632-652`, `:908-923`).
- Declared config stores that fail to open become failing handles, classified as invalid configuration, and the request proceeds (`edgezero:crates/edgezero-adapter-fastly/src/request.rs:654-677`; `edgezero:crates/edgezero-core/src/config_store.rs:503-510`). The capability guide describes that for config only (`edgezero:docs/guide/capabilities.md:192-194`).
- F's Fastly KV open maps a store that does not exist to `KvError::Unavailable` (503) and an SDK open error to `KvError::Internal` (500) (`edgezero:crates/edgezero-adapter-fastly/src/key_value_store.rs:30-35`; `edgezero:crates/edgezero-core/src/key_value_store.rs:823-844`). EZ-20.2 reverses that.
- Cloudflare and Spin also fail the request on a declared KV store that cannot open (`edgezero:crates/edgezero-adapter-cloudflare/src/request.rs:758-783`; `edgezero:crates/edgezero-adapter-spin/src/request.rs:690-716`). Axum fails at boot (`edgezero:crates/edgezero-adapter-axum/src/dev_server.rs:491-496`).

#### Requirements

- **EZ-20.1** On Fastly's canonical path with declared store metadata, a declared KV store that cannot be opened MUST NOT fail the request, skip `prepare`, or prevent dispatch. Handlers MUST still find the store by its declared id, and its operations MUST fail when used.
- **EZ-20.2** An unlinked or missing declared store MUST fail its operations with an internal-class error (500 when rendered), because it is a deployment error. A provider failure to open MUST fail them with an unavailable-class error (503). Error text MUST NOT contain the store name or provider text.
- **EZ-20.3** A request that never uses a declared KV store SHOULD incur no extra provider work for it.
- **EZ-20.4** EdgeZero MUST log an open failure at most once per store id per sandbox, as it does today for config.
- **EZ-20.5** Store availability MUST NOT be cached across requests. The once-per-sandbox log state of EZ-20.4 is the only state kept.
- **EZ-20.6** A store the application explicitly requires (`FastlyService::new(&app).with_kv(name).require_kv()`) MUST keep failing the request before anything is sent, reported as EZ-17's "no response attempted".
- **EZ-20.7** Cloudflare and Spin SHOULD follow EZ-20.1 to EZ-20.5.
- **EZ-20.8** The capability guide MUST describe declared-KV failure behavior next to config's.

#### Acceptance criteria

1. Viceroy 0.21.0. Given an app declaring a KV id Viceroy does not link, a route that ignores KV returns 200, and a route that reads the store returns 500, in the same request sequence.
2. Unit. A failed open of each class yields the error class in EZ-20.2, and neither message contains the store name.
3. Viceroy 0.21.0. Given the setup in criterion 1 in one reused sandbox, two requests to the KV route produce one open-failure log line in total, and requests to the KV-free route produce none.
4. workerd and Spin 4.1, if EZ-20.7 is adopted. Same as criterion 1.
5. Trusted Server, Viceroy 0.21.0. With `TrustedServerApp::stores()` passed to the helper against TS's Viceroy template, every route serves the pre-cut responses.

#### Out of scope

- Changing Axum's boot-time failure.
- Changing config or secret store behavior.

#### Open questions

1. Should Cloudflare and Spin change in the same release? TS's preferred answer is yes, so the three serverless adapters share one rule for declared KV, as they already do for config.

#### Interim for Trusted Server

TS passes `StoresMetadata::default()` to the helper on Fastly and keeps `Hooks::stores()` only for its manifest-parity test.

### EZ-21. Framing failure reason in logs

**Priority:** Follow-up
**Adapters:** all four, Native.
**Related:** EZ-16 and EZ-10 (read the same way once they land). EZ-17 (the report it returns carries the reason). EZ-19 (a set that breaks framing is reported the same way).

#### Need

TS's framing failures come from operator `response_headers` or integration headers that name `trailer` or `content-length`. TS's framing-header guard (D8 in TS's cut spec) rejects those at deploy and skips them at runtime (`ts:docs/superpowers/specs/2026-10-17-edgezero-pr275-hard-cutover-design.md:88`, `:529-541`, `:604-605`). TS sets lengths from final bytes on its rewrite paths (`:399-403`), so a wrong length there would be a TS bug that also needs diagnosing. Each of these becomes a 500 fallback. An operator who adds a header and starts serving 500s needs to know which rule fired.

#### Gap at F

- The log line says only "response-egress framing validation failed before commit" (`edgezero:crates/edgezero-core/src/response_egress.rs:650-656`), and the report records only a conversion error (`edgezero:crates/edgezero-core/src/response_egress.rs:291-301`; test `edgezero:crates/edgezero-adapter-fastly/src/response.rs:1310-1317`).
- A policy panic, a deadline normalization failure and a Fastly head-preparation failure produce the same outcome (`edgezero:crates/edgezero-core/src/response_egress.rs:620-638`; `edgezero:crates/edgezero-adapter-fastly/src/response.rs:375-385`).
- F defines six framing rules with fixed text, `ResponseEgressFramingError` (`edgezero:crates/edgezero-core/src/response_egress_framing.rs:23-39`), but every adapter discards the rule, including when the fallback itself fails framing (`edgezero:crates/edgezero-adapter-fastly/src/response.rs:316-322`; `edgezero:crates/edgezero-adapter-cloudflare/src/response.rs:392-396`; `edgezero:crates/edgezero-adapter-spin/src/response.rs:766-770`; `edgezero:crates/edgezero-adapter-axum/src/response.rs:635-636`, `:683-686`).

#### Requirements

- **EZ-21.1** When a response fails framing validation before commit, the rule that failed MUST be available to the application as one stable name per rule F defines (content-length mismatch, invalid connection header, invalid content-length, unsupported status, unsupported trailers, unsupported tunnel), and per rule added later. It MUST appear in the terminal report that the completion receives, and that the observer receives where EZ-17.3 says it does, and in EdgeZero's log line for that event.
- **EZ-21.2** Applications MUST be able to read the reason from the terminal report without parsing message text. It MUST NOT reach the wire, and existing error `Display` text and the fallback body MUST NOT change (cross-cutting requirement 7). Where EZ-10 or EZ-16 has landed, the reason SHOULD be read the same way as their detail.
- **EZ-21.3** This MUST hold for routed responses, detached responses and the fallback itself, on all four adapters.
- **EZ-21.4** A policy panic, a deadline normalization failure and a host rejection of the head MUST remain distinguishable from framing failures in the report or the log.
- **EZ-21.5** The reason MUST NOT include header values, `Connection` tokens or body bytes. The log line MAY include the route's method and registered pattern, which are bounded (`edgezero:crates/edgezero-core/src/router.rs:40-68`; `edgezero:docs/superpowers/specs/2026-09-08-response-egress-design.md:851-853`).

#### Acceptance criteria

1. Unit, each adapter. For each framing rule, a response that breaks it yields a report naming that rule, and a policy panic yields a report naming none.
2. Viceroy 0.21.0. Given a routed response with a `Trailer` field, the runtime log line and the report both name "unsupported trailers", and the client receives the unchanged 500 fallback.
3. Trusted Server, Viceroy 0.21.0. With TS's framing-header guard disabled, an operator `trailer` header produces the 500 fallback and TS's completion log names "unsupported trailers".

#### Out of scope

- Naming the offending header in the reason.
- Reasons for post-commit failures, which F already logs by category (`edgezero:crates/edgezero-core/src/response_egress_framing.rs:212-256`).

#### Open questions

1. At what level does EdgeZero log the framing reason? TS's preferred answer is `error`, the level F already uses for this event (`edgezero:crates/edgezero-core/src/response_egress.rs:651`). The names are EdgeZero's own rules with no provider data, so a debug-only rule for provider detail need not apply.

#### Interim for Trusted Server

TS's framing-header guard prevents the known sources. TS accepts undiagnosed framing 500s from any other source.

### EZ-22. Fallible application state built and retained with the routes

**Priority:** Follow-up
**Adapters:** core behavior on all four adapters. The outcome of a failed assembly is specified per entry point.
**Related:** EZ-23 (TS assembles its test apps from explicit settings with the same policies), EZ-17 (reports the failure response as a detached response), EZ-19 (the failure response carries no app-configured headers)

#### Need

TS builds one application state per application lifetime. On Fastly it holds `settings`, `orchestrator`, `registry`, `default_kv_store`, and `auction_telemetry_sink` (`ts:crates/trusted-server-adapter-fastly/src/app.rs:186-192`). The auction plan is compiled at `:223` and held inside the orchestrator and registry. Building the state can fail, because it reads the config and secret stores and validates the plan for the target (`ts:crates/trusted-server-adapter-fastly/src/app.rs:200-239`).

`Hooks::routes()` cannot fail, so every TS adapter turns a state failure into a router that answers every route with the error status (`ts:crates/trusted-server-adapter-fastly/src/app.rs:1042-1068`, `ts:crates/trusted-server-adapter-cloudflare/src/app.rs:410-435`, `ts:crates/trusted-server-adapter-axum/src/app.rs:579-603`). Spin's version always answers 503 and keeps `/health` at 200 (`ts:crates/trusted-server-adapter-spin/src/app.rs:508-538`, `:529-531`). EdgeZero's retention paths treat such a router as a successful app, so retaining it would pin one transient config-store failure for a whole sandbox or isolate. TS avoids this only on Fastly, with its own retained pair of app and state (`ts:crates/trusted-server-adapter-fastly/src/sandbox.rs:57-73`) and a failed build that serves one request and is dropped (`ts:crates/trusted-server-adapter-fastly/src/main.rs:382-412`).

TS's Fastly entry point reads the state outside the router. It reads `settings` before dispatch to resolve the trusted client IP (`ts:crates/trusted-server-adapter-fastly/src/main.rs:414-427`), after dispatch for finalize headers and EC finalization (`:482-555`), and after the response is sent for pull sync (`:517`). When state is missing it reloads settings from the config store again in the same request (`:489-498`, `:526-553`).

Cloudflare and Spin rebuild everything on every request, because `run_app` assembles the app per request (`edgezero:crates/edgezero-adapter-cloudflare/src/lib.rs:172-183`, `edgezero:crates/edgezero-adapter-spin/src/lib.rs:187-201`) and TS builds its state inside `routes()` (`ts:crates/trusted-server-adapter-cloudflare/src/app.rs:449-458`, `ts:crates/trusted-server-adapter-spin/src/app.rs:553-562`). Each Cloudflare request re-parses `TRUSTED_SERVER_CONFIG`, re-reads secrets, recompiles the auction plan, and rebuilds the integration registry (`ts:crates/trusted-server-adapter-cloudflare/src/app.rs:129-160`, `:196-217`). Cloudflare state needs the request's `worker::Env`, which TS passes to the input-less `routes()` through a thread-local (`ts:crates/trusted-server-adapter-cloudflare/src/app.rs:52-70`, `ts:crates/trusted-server-adapter-cloudflare/src/lib.rs:27-30`). Spin state comes from asynchronous store reads that TS blocks on inside `routes()` (`ts:crates/trusted-server-adapter-spin/src/app.rs:84-107`).

On PR 275's APIs TS wants one shape on every adapter. It builds its state, which may fail and may need provider bindings or asynchronous reads, assembles an app with the same name and `configure` policies `App::build` applies, retains it where the adapter allows, and reads the state from the retained app in entry-point code. TS serves Fastly through `serve_custom` with `lifecycle::Sandbox<App>`, so a failed assembly on that path must also answer the current request and must not be retained.

#### Gap at F

- `App::build::<A>(platform)` takes the router from `A::routes()`, runs `A::configure`, and rejects changed platform metadata (`edgezero:crates/edgezero-core/src/app.rs:151-164`). Its doc calls it the sole adapter-facing assembly path (`:143-146`). `routes()` returns a bare `RouterService` and cannot fail (`:610-611`). Only `configure` can fail (`:554-556`).
- There is no way to assemble an app from a router the application built itself with `configure` and the platform check applied. The only alternative, `App::with_name_and_platform` (`:488-514`), skips both.
- State registered with `RouterBuilder::with_state` lives in the router (`edgezero:crates/edgezero-core/src/router.rs:337-355`) and reaches handlers through `State<T>` (`edgezero:crates/edgezero-core/src/extractor.rs:619-653`), but neither `RouterService` (`edgezero:crates/edgezero-core/src/router.rs:458-600`) nor `App` (`edgezero:crates/edgezero-core/src/app.rs:38-50`) lets entry-point code read it.
- The `app!` macro registers only an infallible state expression (`edgezero:crates/edgezero-macros/src/app.rs:61-66`, `:358-360`). The demo works around per-request assembly with a process-wide `OnceLock` (`edgezero:examples/app-demo/crates/app-demo-core/src/lib.rs:280-298`).
- When assembly fails, Fastly `serve_app_with_hooks` ends the serving loop without a response for the current request (`edgezero:crates/edgezero-adapter-fastly/src/lib.rs:255`, `edgezero:docs/guide/adapters/fastly.md:439-446`). On the custom path, `Sandbox::initialize` returns the error to the callback and leaves how to respond to the caller (`edgezero:crates/edgezero-adapter-fastly/src/lifecycle.rs:67-85`), and EdgeZero offers no way to answer without an assembled app, so the fixture sends its own native 503 (`edgezero:tests/fixtures/reusable-app/crates/fixture-fastly/src/lib.rs:203-215`).
- Cloudflare `run_app` returns a generic `worker::Error` on a failed assembly (`edgezero:crates/edgezero-adapter-cloudflare/src/lib.rs:66-74`), and Spin returns an untyped error (`edgezero:crates/edgezero-adapter-spin/src/lib.rs:102-108`, `:199`). Neither sends the error's own status.
- The retention guidance for Cloudflare and Spin keeps only a successfully assembled app (`edgezero:docs/guide/adapters/cloudflare.md:342-378`, `edgezero:docs/guide/adapters/spin.md:338-375`), which is correct and must stay true.

#### Requirements

- **EZ-22.1** An application's route building MUST be able to fail with a typed `EdgeError`. When it fails, `App::build` MUST return that error unchanged, MUST NOT apply `configure`, and MUST NOT produce an app.
- **EZ-22.2** App state MUST have one source. The value an application registers with `RouterBuilder::with_state` MUST be the value handlers receive through `State<T>` and the value entry-point code reads from the assembled app, with shared interiors shared.
- **EZ-22.3** Code that holds an assembled app, such as a Fastly custom serving callback or a Cloudflare or Spin retention wrapper, MUST be able to read registered state of a given type without a request, and MUST get nothing when no such state was registered.
- **EZ-22.4** An application's `configure` MUST be able to read the registered state when it chooses policies, for example ingress head limits taken from settings.
- **EZ-22.5** An application MUST be able to assemble an app from a router it built itself. That assembly MUST apply the same three things `App::build` applies to a router from `routes()`, which are the application's name, its `configure` policies, and the platform-metadata check. This covers state that needs provider bindings, asynchronous store reads, or explicit test settings.
- **EZ-22.6** Every EdgeZero entry point or documented recipe that retains an app MUST retain only an app whose route building and `configure` both succeeded. A failure MUST leave nothing retained, and the next request MUST retry assembly.
- **EZ-22.7** Fastly `serve_app` and `serve_app_with_hooks` MUST answer the current request when assembly fails, with the error's HTTP status (for example 503 for `EdgeError::service_unavailable`) and EdgeZero's standard error body, through EdgeZero's response delivery, and MUST keep serving so the next request retries. EZ-17 reports this response as a detached response. Because `configure` did not run, the response carries none of the application's configured error rendering or EZ-19 headers, and the guide MUST say so.
- **EZ-22.8** Fastly `run_app`, `run_app_with_hooks`, and `run_app_with_config`, Cloudflare `run_app`, and Spin `run_app` and `run_app_with_platform` SHOULD answer a failed assembly as EZ-22.7 does instead of failing with a host-generic error. Axum `run_app` MUST keep failing startup before it binds the listener.
- **EZ-22.9** The failure response MUST NOT expose internal error detail. Internal errors MUST render as F renders them today, "internal server error" (`edgezero:crates/edgezero-core/src/error.rs:555-575`). EdgeZero MUST log the full error once per failed attempt on its boot log target.
- **EZ-22.10** Applications declared through the `app!` macro SHOULD be able to declare fallible state with the outcomes of EZ-22.1 and EZ-22.6.
- **EZ-22.11** An assembled app MUST stay safe to retain and share across overlapping requests, as it is at F (`edgezero:crates/edgezero-core/src/app.rs:987-995`).
- **EZ-22.12** A Fastly custom serving callback whose assembly failed MUST be able to answer the current request with that error's HTTP status and EdgeZero's standard error body through EdgeZero's response delivery, without an assembled app, and keep serving so the next request retries. EZ-17 reports this response as a detached response, and EZ-22.9 applies to it.

#### Acceptance criteria

1. Environment is unit, host. Given an application whose route building fails with a 503 error and whose `configure` counts calls, when `App::build` runs, then it returns the 503 error and the count is 0.
2. Environment is unit, host. Given state registered as a shared value, when a handler extracts it through `State<T>` during canonical dispatch and entry-point code reads it from the assembled app, then both see the same shared value.
3. Environment is unit, host. Given a `configure` that sets ingress head limits from registered state, when the app is assembled, then the app's head limits are the ones in the state.
4. Environment is unit, host. Given a router the application built itself, when it is assembled through the EZ-22.5 path, then `configure` runs once, the name is the application's, and a `configure` that changes platform metadata fails exactly as it does under `App::build`. The existing cases at `edgezero:crates/edgezero-core/src/app.rs:1022-1127` pass for both paths.
5. Environment is unit, host. Given `lifecycle::Sandbox<App>` with an assembly that fails once and then succeeds, when two requests arrive, then after the first nothing is retained and one build was attempted, and after the second the app is retained and two builds were attempted.
6. Environment is Viceroy 0.21.0. Given TS's composition, `serve_custom` with `lifecycle::Sandbox<App>`, and an application whose assembly fails while a flag is set, when request one arrives with the flag set and request two arrives in the same sandbox with the flag cleared, then request one gets 503 with EdgeZero's JSON error body and nothing is retained, and request two gets 200 with the same instance identity, the next ordinal, and two builds recorded.
7. Environment is workerd and Spin 4.1. Given the documented retention recipe and a build that fails once, when two requests arrive in one instance, then the first gets the failure, nothing is retained, and the second succeeds and is retained (EZ-22.6). Only if EZ-22.8 is adopted, given a failing route build, when `run_app` serves a request, then the client gets 503 with the JSON error body, not a host-generic 500. EdgeZero records a passing run of this criterion for the pinned commit.
8. Environment is unit, host, only if EZ-22.10 is adopted. Given an `app!` application with fallible state that fails, when `App::build` runs, then it fails and nothing is assembled.
9. Environment is Viceroy 0.21.0 with TS's reusable-sandbox feature. Given a config key pointing at a missing blob, when request one arrives, then EdgeZero's 503 is served through EZ-22.12. Given the blob restored, when request two arrives in the same sandbox, then it gets 200 with the same `x-ts-sandbox-instance` and `x-ts-sandbox-builds` one higher (`ts:crates/trusted-server-adapter-fastly/src/sandbox.rs:21-31`). TS no longer keeps its own app-and-state pair or its startup-error router.
10. Environment is workerd. Given TS's Cloudflare entry with the retention recipe, when ten sequential requests report the same isolate identity, then the state is built once. TS no longer keeps the Cloudflare thread-local.
11. Environment is Viceroy 0.21.0. Given `serve_app` and an assembly that fails while a flag is set, when request one arrives with the flag set and request two arrives in the same sandbox with the flag cleared, then request one gets 503 with the JSON error body and request two gets 200 with two builds recorded.

#### Out of scope

- Refreshing retained state when configuration changes. Staleness stays an application decision, as the guides already say (`edgezero:docs/guide/adapters/overview.md:229-233`).
- Asynchronous route building inside EdgeZero's entry points. EZ-22.5 covers asynchronous state, because the application can finish its own reads before assembling.
- A framework-owned retention cell for Cloudflare and Spin. The documented recipe stays application-owned.
- Passing platform metadata to route building.

#### Open questions

1. Should route building see the platform metadata, as `configure` does? TS prefers no for now. Nothing in TS needs it, and `configure` can choose platform-dependent policy after state exists.
2. Should Fastly `serve_app` stop serving after some number of consecutive assembly failures? TS prefers no. `Serve` request and lifetime limits already bound the loop, and stopping only moves the retry to a cold sandbox.

#### Interim for Trusted Server

TS keeps its own retained app-and-state pair on Fastly. It assembles with `App::with_name_and_platform(router, name, FASTLY_PLATFORM)` followed by an explicit `TrustedServerApp::configure(&mut app)?`, because `App::build` cannot return state, and it keeps the startup-error router as the failed-initialization value that serves one request. On Cloudflare and Spin TS keeps assembling per request through `run_app`, as it does at v0.0.9, and does not adopt the retention recipe, because the startup-error router would be retained. No transport code is involved.

### EZ-23. Axum dev server applies application policies, and tests can run real Axum ingress

**Priority:** Follow-up
**Adapters:** Axum. The test facility may serve an app assembled for another adapter's platform, without that adapter's provider request context.
**Related:** EZ-22 (TS assembles test apps from explicit settings with production policies), EZ-2 (router state already decides which outbound client a handler gets), EZ-24 (route tests supply the scripted client), EZ-6 (handlers that begin outbound requests must work under the test facility), EZ-1, EZ-10, EZ-18 (TS checks bodiless forwarding, 413, and `Content-Length` framing on host here)

#### Need

TS's Axum route tests drive the full router through `EdgeZeroAxumService::new(router)` and `tower::ServiceExt::ready().call(request)` (`ts:crates/trusted-server-adapter-axum/tests/routes.rs:1-11`, `:42-49`, `:86-100`), 28 tests in 972 lines. The cross-adapter parity suite uses the same service for Axum (`ts:crates/trusted-server-integration-tests/tests/parity.rs:15`, helpers at `:74-124`, `:224-243`, `:272-289`) and sends Cloudflare and Spin requests straight to `router.oneshot` (`:126-222`, `:244-271`, `:290-316`). TS's Axum binary hands a router to `AxumDevServer::with_config(router, config)` (`ts:crates/trusted-server-adapter-axum/src/main.rs:29-45`) so it binds the `PORT` its integration runner allocates (`ts:crates/trusted-server-integration-tests/tests/environments/axum.rs:59-67`).

On PR 275 TS wants its route and parity tests to run through real Axum ingress, meaning admission, head limits, framing, and streamed bodies, with production `configure` policies and a scripted outbound client in place of the network. Without that the tests either stop compiling or move to `router.oneshot`, which never exercises admission, head limits, response framing, or egress policy on any adapter, and TS loses its host-side check for EZ-1, EZ-10, and EZ-18.

The binary does not need this requirement. EdgeZero's `run_app` assembles through `App::build` and reads its bind address from `EDGEZERO__ADAPTER__HOST` and `EDGEZERO__ADAPTER__PORT` (`edgezero:crates/edgezero-adapter-axum/src/dev_server.rs:498-502`, `:687-689`). Any programmatically configured server, though, silently drops `configure` today.

#### Gap at F

- The adapter's request service is private (`edgezero:crates/edgezero-adapter-axum/src/lib.rs:29-30`, `edgezero:crates/edgezero-adapter-axum/src/service.rs:45-77`), and its tower entry point exists only in the adapter's own tests (`:337-355`). At v0.0.9 it was public (`edgezero@v0.0.9:crates/edgezero-adapter-axum/src/service.rs:24-37`, `:126`).
- `AxumDevServer::new` and `with_config` take a `RouterService` (`edgezero:crates/edgezero-adapter-axum/src/dev_server.rs:86-102`, `:160-168`) and serve it as `App::new(router)` (`:133-140`), which installs default policies and the default name (`edgezero:crates/edgezero-core/src/app.rs:323-326`) and never runs `configure`. Only `run_app` assembles through `App::build` (`edgezero:crates/edgezero-adapter-axum/src/dev_server.rs:440-445`, `:491`).
- Normalized head limits come from the app (`edgezero:crates/edgezero-adapter-axum/src/service.rs:225`), so a programmatic server always enforces the defaults of 65536 header bytes, 100 headers, and 8192 target bytes (`edgezero:crates/edgezero-core/src/ingress.rs:21-23`, `:409-418`). The adapter's raw ingress settings still apply, with `AxumIngressConfig` defaults of 64 KiB raw head, 100 fields, a 10 s head timeout, and 256 connections (`edgezero:crates/edgezero-adapter-axum/src/ingress_config.rs:28-38`).
- Serving on an already-bound listener exists but only for the adapter's own tests (`edgezero:crates/edgezero-adapter-axum/src/dev_server.rs:143-158`). The `test-utils` feature is empty, and the test module holds only an environment mutex (`edgezero:crates/edgezero-adapter-axum/src/lib.rs:35-36`, `edgezero:crates/edgezero-adapter-axum/src/test_utils.rs:1-14`).
- A client registered with `RouterBuilder::with_state` already wins over the adapter's per-request outbound client (EZ-2.14, `edgezero:crates/edgezero-core/src/router.rs:407-411`). The adapter still builds a real outbound client during request conversion even then, and fails the request if that build fails (`edgezero:crates/edgezero-adapter-axum/src/request.rs:67-75`).
- Handlers run on a local executor inside the serving thread (`edgezero:crates/edgezero-adapter-axum/src/dev_server.rs:371-422`). In-process dispatch outside that thread has no local executor for work a handler spawns.

#### Requirements

- **EZ-23.1** Every way to run an Axum dev server programmatically, including a custom bind address and custom store wiring, MUST serve an assembled application with all of its `configure` policies applied. A construction path MUST NOT drop them silently.
- **EZ-23.2** The policies in EZ-23.1 are at least ingress head limits, the admission policy, the error renderer, the response egress policy and observer, the detached-response decision, config extraction limits, the application clock, and the application name.
- **EZ-23.3** For the same application and the same configuration, `run_app` and the programmatic path MUST behave identically.
- **EZ-23.4** Under the adapter's test utilities, an application MUST be able to run its assembled app in-process, from a standard `#[tokio::test]`, through the same ingress, admission, head limits, inbound read deadlines, response framing, streaming, write deadlines, and egress completion as the production server. Work a handler spawns, including EZ-6's begun requests and the chunks of a streamed response they feed, MUST complete as in production.
- **EZ-23.5** A test MUST be able to learn the server's address, stop it without blocking or leaking connections, and read the serving result.
- **EZ-23.6** Any form of the facility that bypasses part of the production path MUST document what it skips.
- **EZ-23.7** When router state supplies an outbound client (EZ-2.14), the real outbound transport MUST NOT be needed or initialized.
- **EZ-23.8** The dev server and the test facility MUST accept an app assembled with any platform metadata, so host parity tests can run a Cloudflare or Spin app's router and policies through Axum ingress. The docs MUST say that provider request contexts are absent in that mode.
- **EZ-23.9** The dev server SHOULD log one startup warning when the app's normalized head limits exceed the raw ingress budget, because the raw parser rejects first.
- **EZ-23.10** Removed.
- **EZ-23.11** Removed.

#### Acceptance criteria

1. Environment is Axum, native. Given an application whose `configure` limits the request target to 64 bytes, when a 100-byte target arrives through the programmatic dev server and through the test facility, then both answer 414. Given the same router served without `configure` at F, the request succeeds, which shows the gap.
2. Environment is Axum, native. Given a `configure` that installs an error renderer adding a marker header, when an unmatched path arrives, then the 404 carries the marker.
3. Environment is Axum, native. Given an admission policy that aborts, when a request arrives, then the connection closes with no response bytes and no handler or middleware runs.
4. Environment is Axum, native. Given a client that uploads 1 MiB with chunked framing, when a handler reads the body, then it sees every byte. Given a client that sends headers and then stops sending the body past the inbound read budget, then it gets 408.
5. Environment is Axum, native. Given a handler that streams three chunks 50 ms apart, when a client reads, then the first chunk arrives before the third is produced and the wire uses chunked transfer coding.
6. Environment is Axum, native. Given a buffered 5-byte response, when it is served, then it carries `content-length: 5`.
7. Environment is Axum, native. Given a HEAD request to a route that returns a body, then the wire carries headers and no body bytes.
8. Environment is Axum, native. Given raw limits of 4 header fields, when a request with 5 fields arrives, then it is rejected before admission and the server keeps serving.
9. Environment is Axum, native. Given a handler mid-stream, when the test stops the server, then the handler's resources are released and the serving result reports success.
10. Environment is Axum, native. Given an outbound client supplied through router state on a host with no outbound network access, when a handler sends a request, then the supplied client sees it, no socket opens to the target, and no request fails because the real transport could not start.
11. Environment is Axum, native. Given an app assembled with Cloudflare's platform metadata, when it runs on the test facility, then its `configure` policies apply.
12. Environment is Axum, native. Given a plain `#[tokio::test]`, when a handler spawns local work (and, once EZ-6 lands, begins an outbound request against a supplied client), then it completes, and a streamed body fed by that work delivers every chunk.
13. Environment is Axum, native. Given TS's route tests and parity suite on the test facility with apps assembled from explicit settings with production policies (EZ-22.5), and Cloudflare and Spin parity moved off `router.oneshot` per EZ-23.8, then a 9000-byte request target, which TS does not override, gets 414 from all three apps, and a bodiless GET whose inbound body is an unread stream reaches a local origin with no request body, which is TS's host check for EZ-1.

#### Out of scope

- A general embeddable request service outside test utilities, which would bypass the bounded HTTP/1 ingress the adapter owns (`edgezero:docs/guide/adapters/axum.md:82-88`).
- A host test facility for Cloudflare or Spin. EZ-23.8 covers host parity through Axum ingress.
- Any change to the capability matrix. `raw-ingress-head-limits` and `raw-ingress-framing-validation` stay Unsupported for Axum (`edgezero:docs/guide/capabilities.md:75-76`).

#### Open questions

1. Should the test facility be usable outside the adapter's test utilities, for example to run an app under a different supervisor? TS prefers no until someone asks for it outside tests.

#### Interim for Trusted Server

TS moves its Axum route tests and all three adapters' parity helpers onto `router.oneshot`, which compiles at F and covers routing, middleware, and handlers but not admission, head limits, framing, or egress. TS accepts that loss in unit tests and keeps the ingress-level checks for EZ-1, EZ-10, and EZ-18 in its integration suite, which already launches the real Axum binary over TCP (`ts:crates/trusted-server-integration-tests/tests/environments/axum.rs:59-67`). The binary moves to `run_app::<TrustedServerApp>()` with `owns_logging` set, and the runner sets `EDGEZERO__ADAPTER__PORT`. No transport code is added.

### EZ-24. Published outbound test double that behaves like production

**Priority:** Follow-up
**Adapters:** core. The double must behave the same on host, `wasm32-wasip1`, and `wasm32-unknown-unknown`.
**Related:** EZ-6 (begun requests, non-blocking checks, and arrival-time rules must hold through the double), EZ-9 (redirect following must reach the double as one ordinary send per hop), EZ-15 (fire-and-forget sends must be recorded once they exist), EZ-1, EZ-5, EZ-7, EZ-10 (request checks, response handling, and the marker must match adapters as those requirements change them), EZ-23 (route tests supply the double). No acceptance criterion of EZ-1 to EZ-9 depends on this double.

#### Need

TS deletes its outbound layer and every test double built on it. Test code holds 23 `impl PlatformHttpClient` or `impl PlatformBackend` blocks across nine files, among them `ts:crates/trusted-server-core/src/platform/test_support.rs:136`, `:155`, `:186`, `:496`, six in the orchestrator's tests (`ts:crates/trusted-server-core/src/auction/orchestrator.rs:3029-3681`), and others in `proxy.rs:2391-2452`, `publisher.rs:16332` and `:20834`, `integrations/prebid.rs:3720`, `integrations/sourcepoint.rs:1175`, and `test_support.rs:252-381` under `ts:crates/trusted-server-core/src/`, plus `app.rs:2717-2894` and `tinybird.rs:281-318` under `ts:crates/trusted-server-adapter-fastly/src/`. All of them move onto one scripted `OutboundHttpClient`.

TS's tests need that client to:

- record each request's parts, so one test per call site pins method, URI, timeout, deadline, response mode and cap, cache policy, authority override, request body cap, and the options other EZ requirements add;
- script responses, transport errors, timeouts with the right budget source, and response-limit errors;
- script batch completions at arrival times, including cutoff and a mid-batch driver failure, so the auction's rule that a bid arriving at deadline minus 1 ms is kept and a bid arriving at the deadline is dropped is tested exactly as production applies it;
- overlap an origin request begun under EZ-6 with an auction batch checked without blocking at body-chunk boundaries, and show that a late check never turns an on-time bid into a cutoff;
- script the hops of a redirect-following call (EZ-9) and observe the per-hop target policy through the order of recorded requests;
- reject what adapters reject before sending, such as a GET or HEAD carrying body bytes, a streamed response in a batch, or an option a send mode does not support.

Without a published double TS writes its own, as EdgeZero's scaffold does (`edgezero:crates/edgezero-cli/src/templates/core/src/handlers.rs.hbs:332-444`), and the copy drifts from adapter behavior. A hand-written double that does not apply the shared request checks and response policy behaves differently from every adapter for a GET with a streamed body or a gzip response.

#### Gap at F

- The core test-utilities feature exposes environment guards and in-memory store fixtures (`NoopKvStore`, `InMemorySecretStore`) only (`edgezero:crates/edgezero-core/src/lib.rs:47-50`, `edgezero:crates/edgezero-core/Cargo.toml:57-60`, `edgezero:crates/edgezero-core/src/key_value_store.rs:951`, `edgezero:crates/edgezero-core/src/secret_store.rs:177`). No outbound double and no controllable clock are published, although `MonotonicClock::new` accepts any time source (`edgezero:crates/edgezero-core/src/time.rs:46-53`).
- The public pieces an application can assemble a double from exist, for example `OutboundBatch::from_driver` (`edgezero:crates/edgezero-core/src/outbound.rs:268-287`), `OutboundResponse::new` (`:801-828`), `OutboundRequest::into_parts` (`:1025-1047`), `validate_for_dispatch` (`:1544-1583`), `apply_response_body_policy` (`:1252-1300`), and `finish_batch_item` (`:1307-1330`). Adapter-identical behavior from them requires repeating adapter-private steps, such as the batch eligibility checks copied into all four adapters (`edgezero:crates/edgezero-adapter-axum/src/outbound.rs:104-121`, `edgezero:crates/edgezero-adapter-cloudflare/src/outbound.rs:1511-1516`, `edgezero:crates/edgezero-adapter-spin/src/outbound.rs:1421-1426`, `edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:2577-2582`) and each adapter's response handling order (`edgezero:crates/edgezero-adapter-axum/src/outbound.rs:307-397`).
- At F, lazy adapters take a slot's completion time when its exchange future is polled to completion (`edgezero:crates/edgezero-adapter-axum/src/outbound.rs:200-203`), and core drains such futures only when the batch is polled (`edgezero:crates/edgezero-core/src/outbound.rs:313-329`). EZ-6 replaces that with arrival time. A double modeled on F would pin the old behavior.
- The scaffold's double emits every batch item at once with fixed elapsed values and empty bodies (`edgezero:crates/edgezero-cli/src/templates/core/src/handlers.rs.hbs:337-380`). It cannot express latency, cutoff, driver failure, or streaming.
- CI runs no core tests on a wasm target. The wasm matrix runs adapter contract tests only (`edgezero:.github/workflows/test.yml:220-241`, `:366`).

#### Requirements

- **EZ-24.1** EdgeZero MUST publish, behind an opt-in that production builds do not enable, a scripted outbound client usable wherever an `HttpClient` is accepted, and a controllable clock that the application, its requests, and the scripted client share.
- **EZ-24.2** The scripted client MUST behave identically on host, `wasm32-wasip1`, and `wasm32-unknown-unknown`, and MUST NOT depend on real time, threads, or a particular async runtime.
- **EZ-24.3** It MUST record every dispatched request, in dispatch order, with every field `OutboundRequest::into_parts` exposes (including fields other EZ requirements add), the request body bytes, the dispatch instant, the effective budget and its source, and the send mode, meaning single send, begun request, batch slot with its batch and index, or fire-and-forget send once EZ-15 exists.
- **EZ-24.4** It MUST reject before sending exactly what adapters reject before sending, with the same error category and status. That includes the GET and HEAD body rules of EZ-1, batch eligibility, the unsupported-option rejection of cross-cutting requirement 2, request body caps, and an already-exhausted budget. When another requirement changes these rules for adapters, the scripted client MUST follow without separate work by the application.
- **EZ-24.5** For a scripted upstream response it MUST give the application what an adapter gives for the same upstream bytes. That means the same header limits and hop-by-hop normalization, bodyless and 205 handling, decoding with encoded, decoded, and final caps, and streamed or buffered delivery, with the same statuses, headers, bytes, and `ResponseLimitReason`. It MUST apply EZ-7's marker default and opt-out exactly as adapters do, with the value `test` when the marker is present.
- **EZ-24.6** A test MUST be able to script, per request, a response (status, headers, and a buffered or chunked body with per-chunk timing), a transport error, or no answer. It MUST be told about requests nothing scripted and scripts nothing consumed. An unscripted request MUST fail with an internal error naming its method and URI, without a panic.
- **EZ-24.7** Time in the double MUST be deterministic. A scripted reply arrives at its dispatch instant plus its latency. Lateness, budgets, the cutoff boundary, delivery order, and elapsed time MUST follow cross-cutting requirement 5 and EZ-6.11 to EZ-6.16 as adapters apply them to recorded arrivals. A non-blocking check MUST return only what has already arrived and MUST NOT move the clock. A streamed body chunk that would arrive after the budget deadline MUST end the body with a gateway timeout carrying that budget's source. How the clock advances while the application waits is EdgeZero's choice, provided every wait ends.
- **EZ-24.8** A test MUST be able to script a batch driver failure after a given number of items, delivered to the application as the batch failure F already defines.
- **EZ-24.9** The scripted client MUST NOT follow redirects itself. A redirect-following call (EZ-9) MUST reach it as one ordinary send per hop, with the application's target policy applied between hops, so tests observe each hop. Begun requests and batches never follow redirects.
- **EZ-24.10** A test MUST keep access to the recordings and the script after handing the client to the application.
- **EZ-24.11** Dropping or cancelling a begun request or a batch through the double MUST behave as cross-cutting requirement 6 requires of adapters. Scripts for abandoned requests MUST NOT be reported as unconsumed.
- **EZ-24.12** EdgeZero's generated application template MUST use the published double instead of its own.

#### Acceptance criteria

All criteria run on host. EdgeZero records passing runs for the pinned commit on `wasm32-wasip1` under a WASI preview 1 runner (the pinned wasmtime or Viceroy 0.21.0) and, for criteria 3 to 8, on `wasm32-unknown-unknown` under a JavaScript host such as workerd or Node. Criterion 10 runs natively.

1. Environment is as above. Given a GET with every request option set, when it is sent, then the recording shows identical parts. Given a POST with a 3-byte body, then the recording shows those bytes.
2. Environment is as above. Given a GET whose inbound body carries bytes, when it is forwarded, then it is rejected with the same error as on an adapter. Given a batch slot that asks for a streamed response, then that slot fails with the adapters' batch-eligibility error and its siblings proceed.
3. Environment is as above. Given a 100 ms timeout and a reply scripted at 150 ms, when the request is sent, then it ends as a gateway timeout with per-call-timeout source, recorded at 100 ms. Given no answer and a 2 s deadline, then it ends as a gateway timeout with request-deadline source at 2 s.
4. Environment is as above. Given three batch slots arriving at 30, 10, and 20 ms, when the batch is collected, then items come in arrival order, indices 1, 2, 0, with elapsed 10, 20, and 30 ms, followed by normal completion.
5. Environment is as above. Given a 100 ms cutoff, a slot arriving at 99 ms, and a slot arriving at 100 ms, when the batch is collected, then the first is kept with elapsed 99 ms, the second is unresolved, and the batch ends at the cutoff. Given the 99 ms slot observed only by a check at 101 ms, then it is still kept with elapsed 99 ms.
6. Environment is as above. Given a driver failure scripted after one item, when the batch is collected, then the application gets one item and then the failure, with exactly one slot filled.
7. Environment is as above, once EZ-6 lands. Given a slot arriving at 10 ms, when the application checks without blocking at 0 ms, then nothing is returned. When the clock is advanced to 15 ms and it checks again, then the slot is returned with elapsed 10 ms.
8. Environment is as above, once EZ-6 lands. Given an origin request begun under EZ-6 whose head arrives at 300 ms, and a batch with a 250 ms cutoff whose slots arrive at 50, 100, and 200 ms, when the application checks the batch at 60, 120, and 210 ms, then all three bids are kept. Given a single check at 260 ms instead, then the same three bids are still kept. When the application then waits for the origin, the wait ends with the origin's head.
9. Environment is as above, once EZ-9 lands. Given a 302 then a 200 scripted for a redirect-following call, then two requests are recorded in hop order. Given a target policy that rejects the second target, then one request is recorded and the call returns EZ-9's rejection.
10. Environment is Axum, native. Given the same gzip body served by a local origin to the Axum adapter and scripted for the double, then status, decoded bytes, and headers match except the proxy marker value, and an over-limit body fails with the same `ResponseLimitReason` on both.
11. Environment is as above. Given an unscripted request, when it is sent, then it fails with an internal error, the test is told about it, and nothing panics.
12. Environment is as above. Given a begun request dropped before its reply arrives, then the test is not told about an unconsumed script for it.
13. Environment is host, Trusted Server. TS has no scripted client of its own. Its per-call-site option tests, batch outcome mapping tests, and cutoff boundary tests run on the published double, and its orchestrator tests that used stub clients migrate to it.

#### Out of scope

- Emulating a specific adapter's capability rejections, such as Spin refusing an authority override (`edgezero:crates/edgezero-adapter-spin/src/outbound.rs:1433-1441`).
- Returning hand-built responses that skip the shared response handling.
- Real-time scheduling or any dependency on wall-clock sleeps.

#### Open questions

1. Should the double's marker value be `test`? TS prefers yes, so tests can tell double-produced responses from adapter ones. If EZ-7.6 exposes which adapter completed an exchange, the double reports `test` there too.
2. Which part of EZ-24 is the minimal core the head proposes to promote? TS's answer is EZ-24.1 to EZ-24.10, with EZ-24.2 limited to host and `wasm32-wasip1`. EZ-24.11, EZ-24.12, and the `wasm32-unknown-unknown` part of EZ-24.2 stay follow-up.

#### Interim for Trusted Server

TS writes its own scripted `OutboundHttpClient` as test code in `ts:crates/trusted-server-core/src/test_support.rs`, following the same arrival-time rules, and deletes it when EZ-24 lands. It is test code, not transport code.

### EZ-25. Documentation drift fixes

**Priority:** Follow-up
**Adapters:** documentation for core, Fastly, Axum, Cloudflare, and Spin
**Related:** EZ-1, EZ-5, EZ-7 (EZ-25.1 owns the forwarding example and must show what EZ-1.14, EZ-5.14, and EZ-7.8 require), EZ-22 and EZ-23 (EZ-25.24, EZ-25.25, and EZ-25.37 change again when they land), EZ-2 (EZ-25.7 describes backend identity at F)

#### Need

TS ports four adapters by reading these guides, and several of the items below produce a wrong port. The forwarding example in `proxying.md` is the template for TS's publisher origin fetch. As written it does not compile, and once fixed it returns 400 for every publisher GET (EZ-1). The reusable-app README shows a six-argument lifecycle helper that does not exist. The Fastly guide describes delivery through the SDK's `stream_to_client`, which the same guide says the standard path cannot use (`edgezero:docs/guide/adapters/fastly.md:347-349`), so a reader cannot tell which commit behavior applies to TS's finalizer and post-send work. The CHANGELOG says Rust 1.95.0 is still the workspace toolchain while `.tool-versions` pins 1.98.1, and TS pins 1.95.0 (`ts:rust-toolchain.toml:2`). The capability declaration example cannot pass the gate on Fastly, Cloudflare, or Spin, and it is the first thing TS copies into its manifest.

#### Gap at F

EdgeZero checks some documentation in CI with `scripts/check_outbound_docs_contract.mjs` (`.github/workflows/test.yml:78`, `.github/workflows/format.yml:228`) and `scripts/check_outbound_legacy_api.sh` (`.github/workflows/test.yml:75`). The docs contract check pins the stale phrase "accounts synchronous body-handle writes through `stream_to_client` close" (`edgezero:scripts/check_outbound_docs_contract.mjs:1610`), pins the Limits table rows exactly (`expectedLimitRows`, `:199-240`, failing on any difference at `:1670-1673`), and requires `axum.md` to contain `EdgeZeroApp::build::<App>(edgezero_adapter_axum::AXUM_PLATFORM)` (`:1069-1078`), which appears only in the Testing sample (`edgezero:docs/guide/adapters/axum.md:280`). Nothing compiles the guides' Rust snippets, none of which is marked `ignore`, which is why EZ-25.2, EZ-25.14, EZ-25.16, and EZ-25.17 are compile errors no check caught. Each item below cites the code that contradicts the guide.

#### Requirements

Each item gives the location at F, the current text, and the evidence. The requirement is that the guide states what the evidence shows. The suggested wording is TS's proposal, and EdgeZero may word it differently, provided the result passes this requirement's acceptance criteria and EdgeZero's documentation checks. No item edits `docs/superpowers/specs/`.

Outbound guides:

- **EZ-25.1** `docs/guide/proxying.md:9-53`, the "Forward An Inbound Request" example, which this item owns. At F it fails for GET and HEAD, because ingress always hands the router a lazy `Body::Stream` (for example `edgezero:crates/edgezero-adapter-axum/src/request.rs:43-49`), `from_request` keeps that body (`edgezero:crates/edgezero-core/src/outbound.rs:927-934`), and `validate_for_dispatch` rejects any GET or HEAD `Body::Stream` (`:1561-1574`). The example MUST compile, MUST forward a bodiless GET or HEAD once EZ-1 lands, and MUST show what EZ-1.14, EZ-5.14, and EZ-7.8 require. Until EZ-1 lands, the guide MUST say that the example works only for methods that carry a body, and it MUST NOT show a workaround that drops a GET or HEAD body that carries bytes (EZ-1.8). Suggested wording, in both cases, for the sentence at `:9-10` is "This example preserves the inbound method, forwards the inbound body for methods that carry one, replaces the target URI, removes a private header, and converts the outbound response back into a core response. GET and HEAD requests are forwarded without a body." Until EZ-1 lands, add after it "At this release, use it only for methods that carry a body. A GET or HEAD forwarded this way fails with 400."
- **EZ-25.2** `docs/guide/proxying.md:32`. Now `OutboundRequest::from_request(ctx.into_request(), target)?`. `RequestContext::into_request` returns `Result<Request, EdgeError>` (`edgezero:crates/edgezero-core/src/context.rs:381`), so the sample does not compile. Suggested replacement is `OutboundRequest::from_request(ctx.into_request()?, target)?`.
- **EZ-25.3** `docs/guide/proxying.md:144-145`. Replace the first sentence, through `results.termination`. Now "`client.send_all_until(requests, observation_cutoff).await` collects the same driver into an index-aligned `Vec<Option<OutboundSlotResult>>` plus `results.termination`." The return type is `OutboundBatchResults { slots, termination }` (`edgezero:crates/edgezero-core/src/outbound.rs:133-140`). Suggested replacement is "`client.send_all_until(requests, observation_cutoff).await` collects the same driver into `OutboundBatchResults`, whose `slots` field is an index-aligned `Vec<Option<OutboundSlotResult>>` and whose `termination` field records why collection stopped."
- **EZ-25.4** `docs/guide/streaming.md:116-128` and `docs/guide/capabilities.md:415-420`. The decompression text never says that every adapter sends `accept-encoding: identity` when the request sets none (`edgezero:crates/edgezero-adapter-axum/src/outbound.rs:59-61`, `edgezero:crates/edgezero-adapter-cloudflare/src/outbound.rs:266-268`, `edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:754-757`, `edgezero:crates/edgezero-adapter-spin/src/outbound.rs:356-358`), so the example at `streaming.md:122-128` asks for identity and rarely exercises decompression. After the sentence ending at `streaming.md:118` and after the paragraph ending at `capabilities.md:420`, suggested addition is "Every adapter sends `accept-encoding: identity` when the request has no `accept-encoding` header, so set one explicitly to receive a compressed response." In the example, after `OutboundRequest::get("https://api.example.com/data")?` at `streaming.md:123`, insert `.header("accept-encoding", "gzip, br")?`. EZ-5.14 governs this text once EZ-5 lands.
- **EZ-25.5** `docs/guide/streaming.md:58-60`. Now "Spin's `dispatch_app::<A>(&app, req)` returns a raw `SpinResponse` whose body and transmission lifetime remain owned by the adapter coordinator." The signature is `-> anyhow::Result<SpinResponse>` (`edgezero:crates/edgezero-adapter-spin/src/lib.rs:215`). Suggested replacement is "Spin's `dispatch_app::<A>(&app, req)` returns `anyhow::Result<SpinResponse>`, and the response body and transmission lifetime remain owned by the adapter coordinator."
- **EZ-25.6** The guides and CHANGELOG MUST NOT describe Fastly delivery as `stream_to_client`. They MUST describe what EdgeZero guarantees about commit, framing selection, and body-handle close. Naming the hostcalls is optional. At F the adapter commits through a raw status-returning hostcall, not the SDK's `stream_to_client` (`edgezero:crates/edgezero-adapter-fastly/src/fastly_response_abi.rs:64-70`, called from `edgezero:crates/edgezero-adapter-fastly/src/response.rs:128-137`), appends every header value (`fastly_response_abi.rs:19-34`), and selects manual framing when a transmitted body declares `Content-Length` (`response.rs:159-161`). Locations and suggested wording follow.
  - `docs/guide/adapters/fastly.md:468-470`. Now "append every header value, commit once through `stream_to_client`, account accepted body bytes, and finish or abandon the body handle exactly once." Suggested replacement is "append every header value, select manual framing when a transmitted body declares `Content-Length`, commit the head once, account accepted body bytes, and finish or abandon the body handle exactly once."
  - `docs/guide/adapters/fastly.md:69`. Replace "owns `stream_to_client` through final body-handle close." with "owns the streaming response through final body-handle close."
  - `docs/guide/adapters/fastly.md:201`. Replace "EdgeZero owns the low-level `stream_to_client` lifetime and writes portable" with "EdgeZero owns the streaming response lifetime and writes portable".
  - `docs/guide/streaming.md:45-46`. Replace "Fastly `stream_to_client`," with "Fastly an adapter-owned streaming body handle,".
  - `docs/guide/capabilities.md:223`. Replace "Fastly accounts synchronous body-handle writes through `stream_to_client` close" with "Fastly accounts synchronous body-handle writes through close". EZ-25.44 covers the docs contract check that pins the old phrase.
  - `docs/guide/capabilities.md:284-285`. Replace "the low-level `stream_to_client` response lifetime" with "owns the streaming response lifetime".
  - `CHANGELOG.md:253` is EZ-25.32.
  - Keep `fastly.md:347-349`, which correctly says the SDK API is incompatible.
- **EZ-25.7** `docs/guide/adapters/fastly.md:194-195`. Now "derived from the canonical target, TLS identity, and provider timer budget". At F the backend identity also includes the wire authority (`edgezero:crates/edgezero-adapter-fastly/src/outbound.rs:254-262`). Suggested replacement is "Outbound HTTP uses deterministic **dynamic backends** derived from the canonical target, TLS identity, wire authority (including any `host_authority_override`), and provider timer budget." EZ-2's documentation replaces this text when EZ-2 lands.
- **EZ-25.8** `docs/guide/capabilities.md:11-12`. Now `required = ["outbound-deadlines"]`. The gate rejects a required capability that is only BestEffort (`edgezero:crates/edgezero-cli/src/adapter.rs:423-428`), and Cloudflare, Fastly, and Spin report `OutboundDeadlines` as BestEffort (`edgezero:crates/edgezero-adapter-cloudflare/src/cli.rs:146`, `edgezero:crates/edgezero-adapter-fastly/src/cli.rs:2052`, `edgezero:crates/edgezero-adapter-spin/src/cli.rs:153`), so the example passes only on Axum. Suggested replacement for the two lines is `required = ["outbound-cache-bypass"]` and `optional = ["outbound-deadlines", "outbound-http"]`. `outbound-cache-bypass` is Native everywhere (`edgezero:docs/guide/capabilities.md:240`).
- **EZ-25.9** `docs/guide/capabilities.md:236` and footnote ⁸ at `:294-299`. The Cloudflare `outbound-authority-override` cell points at ⁸, which never mentions authority override. Suggested addition to ⁸ is "Cloudflare sends the authority override as the `Host` header, but no deployed wire probe yet proves that the final request keeps that authority separate from the connection target, so `outbound-authority-override` also remains `BestEffort`." The code sets `Host` at `edgezero:crates/edgezero-adapter-cloudflare/src/outbound.rs:269-273`, and `proxying.md:89-92` already says the cell awaits a deployed wire probe.
- **EZ-25.10** `docs/guide/capabilities.md:383-393`, "Limits And Accounting". The 30 s default budget is not listed. `DEFAULT_NO_DEADLINE_BUDGET` is 30 s (`edgezero:crates/edgezero-core/src/time.rs:13`) and applies with `BudgetSource::Default` when neither timeout nor deadline is set, and the earliest candidate wins (`:150-166`). Suggested row is "| `timeout` / `deadline` | Per-request budget. The earliest of timeout, deadline, and batch cutoff wins | 30 s when neither is set |". EZ-25.44 covers the docs contract check that pins the table.

Fastly lifecycle guide:

- **EZ-25.11** `docs/guide/adapters/fastly.md:400-401`. Now "`FnMut(&mut edgezero_core::Response) -> State`". The crate root does not export `Response` (the root re-exports start at `edgezero:crates/edgezero-core/src/lib.rs:53`), and the type is `edgezero_core::http::Response` (`edgezero:crates/edgezero-core/src/http.rs:36`). Suggested replacement is "`FnMut(&mut edgezero_core::http::Response) -> State`".
- **EZ-25.12** `docs/guide/adapters/fastly.md:460-461`. Now "injects fresh `Kv`, `Config`, `Secrets`, and `AppConfig` registries". Only config, KV, and secret registries are inserted (`edgezero:crates/edgezero-adapter-fastly/src/request.rs:395-415`), and `AppConfig` is an extractor over the config registry. Suggested replacement is "injects fresh KV, config, and secret registries, which back the `Kv`, `Config`, `Secrets`, and `AppConfig` extractors,".
- **EZ-25.13** `docs/guide/adapters/fastly.md:588-590`. Now "Use `lifecycle::run_custom(Request::from_client(), callback)` for the single-request branch, initializing the Fastly ABI before receiving that request." `run_custom` initializes the ABI inside its body, after its argument has already received the request (`edgezero:crates/edgezero-adapter-fastly/src/lifecycle.rs:148-156`), and the fixture calls `fastly::init()` first (`edgezero:tests/fixtures/reusable-app/crates/fixture-fastly/src/lib.rs:277-282`). Suggested replacement is "For the single-request branch, call `fastly::init()` and then `lifecycle::run_custom(Request::from_client(), callback)`, because the argument receives the request before `run_custom` runs. It creates". Whether `from_client` needs prior initialization is **Unverified**. A Viceroy run that omits `fastly::init()` would settle it.
- **EZ-25.14** `docs/guide/adapters/fastly.md:330-343`. The sample calls `ctx.request()`, which `RequestContext` does not have (its public methods at `edgezero:crates/edgezero-core/src/context.rs:262-616` include `extensions` at `:336` but no `request`), imports neither `Response` nor `EdgeError`, and returns nothing. `FastlyRequestContext` lives in request extensions (`edgezero:crates/edgezero-adapter-fastly/src/context.rs:11-15`). Suggested replacement for the whole sample is

  ```rust
  use edgezero_adapter_fastly::context::FastlyRequestContext;
  use edgezero_core::context::RequestContext;
  use edgezero_core::http::Response;
  use edgezero_core::{Body, EdgeError};

  async fn handler(ctx: RequestContext) -> Result<Response, EdgeError> {
      let client_ip = ctx
          .extensions()
          .get::<FastlyRequestContext>()
          .and_then(|fastly_ctx| fastly_ctx.client_ip);
      Ok(Response::new(Body::from(format!("{client_ip:?}"))))
  }
  ```

  `Body` is a root re-export (`edgezero:crates/edgezero-core/src/lib.rs:56`) and converts from `String` (`edgezero:crates/edgezero-core/src/body.rs:216`).

- **EZ-25.15** `tests/fixtures/reusable-app/README.md:201`. Now "`send_request_with_registries_and_hooks(app, stores, req, &runtime.env, prepare, finalize)`". The helper takes five arguments (`edgezero:crates/edgezero-adapter-fastly/src/request.rs:557-563`). Suggested replacement is "`send_request_with_registries_and_hooks(app, stores, req, prepare, finalize)`".

Core and cross-adapter guides:

- **EZ-25.16** `docs/guide/handlers.md:389`. Now `use edgezero_core::{Body, EdgeError, Response};`. `Response` is not a root export (`edgezero:crates/edgezero-core/src/lib.rs:53-115`). Suggested replacement is `use edgezero_core::http::Response;` and `use edgezero_core::{Body, EdgeError};`.
- **EZ-25.17** `docs/guide/handlers.md:229-242`. The sample imports `use edgezero_core::{EdgeError, Text};`, but `Text` lives at `edgezero_core::response::Text` (`edgezero:crates/edgezero-core/src/response.rs:50`) and is not re-exported. It also uses `#[action]` without importing it and an application type `MyAppConfig` the guide never defines. Suggested replacement for the imports is `use edgezero_core::action;`, `use edgezero_core::extractor::{AppConfig, Kv};`, `use edgezero_core::response::Text;`, and `use edgezero_core::EdgeError;`. Because `MyAppConfig` is the application's own type, mark the fence `rust,ignore` and add the line `// MyAppConfig is the application's own config type, defined with #[derive(AppConfig)].` as its first line.
- **EZ-25.18** `docs/guide/middleware.md:61`. Now "Middleware are applied in order before routes are matched." The route is resolved before admission (`edgezero:crates/edgezero-core/src/app.rs:116-118`), the chain is built only for a matched entry (`edgezero:crates/edgezero-core/src/router.rs:417-418`), and 404 and 405 return without it (`:370-381`). Suggested replacement is "Middleware run in registration order after the route is resolved and the request is admitted, wrapping the matched handler. Unmatched paths (404) and unsupported methods (405) return without running middleware."
- **EZ-25.19** `docs/guide/architecture.md:104` and `:113-116`. The diagram shows "into_core_request(): Provider Request → Core Request" and "RouterService matches routes ... Response built and returned". Canonical runners convert the head first and attach the body after admission (`edgezero:crates/edgezero-adapter-fastly/src/request.rs:446-508`), and the core returns an owned `ResponseEgressEnvelope` (`edgezero:crates/edgezero-core/src/app.rs:111-145`, `:236-256`). Suggested replacement for line 104 is `│  - Head normalization: Provider head → Core request head    │`, and for lines 113-116 is
  ```
  │  - App resolves the route and runs ingress admission        │
  │  - Middleware chain executes (matched routes only)          │
  │  - Handler runs with extracted params                       │
  │  - Response returned in an owned egress envelope            │
  ```
  Each replacement line is 63 characters, the box width.
- **EZ-25.20** `docs/guide/routing.md:187-190`. Now "the Fastly, Cloudflare, and Spin entry points call `App::build::<A>(platform)` per request". That holds only for the default `run_app` (`edgezero:crates/edgezero-adapter-fastly/src/lib.rs:205`, `edgezero:crates/edgezero-adapter-cloudflare/src/lib.rs:181`, `edgezero:crates/edgezero-adapter-spin/src/lib.rs:199`). `serve_app` builds once per sandbox (`edgezero:crates/edgezero-adapter-fastly/src/lib.rs:255`), and `dispatch_app` never builds. Suggested replacement for lines 187-190 is "When that happens depends on the adapter. Axum builds the router once at startup, so the conflict surfaces before any traffic. The default Fastly, Cloudflare, and Spin `run_app` entry points call `App::build::<A>(platform)` per request, and Fastly's retained `serve_app` builds on the first request in each sandbox, so a conflicting route deploys successfully and then panics on the first request. With `dispatch_app`, the panic happens wherever the caller builds the app."
- **EZ-25.21** `docs/guide/getting-started.md:94-105`. The tree shows `src/main.rs` for Cloudflare and Spin. Cloudflare scaffolds `src/lib.rs` plus a host stub `src/main.rs` (`edgezero:crates/edgezero-adapter-cloudflare/src/cli.rs:85-95`), and Spin scaffolds `src/lib.rs` and `runtime-config.toml` with no `main.rs` (`edgezero:crates/edgezero-adapter-spin/src/cli.rs:90-106`). Suggested replacement shows the Cloudflare subtree as `Cargo.toml`, `wrangler.toml`, and `src/` holding `lib.rs` (Workers fetch entrypoint) and `main.rs` (host-only stub), and the Spin subtree as `Cargo.toml`, `runtime-config.toml`, `spin.toml`, and `src/lib.rs`.
- **EZ-25.22** `docs/guide/adapters/cloudflare.md:230-231`. Now "Missing bindings log a one-time warning and the id is dropped from the registry." The id keeps a failing handle (`edgezero:crates/edgezero-adapter-cloudflare/src/request.rs:855-862`). Suggested replacement is "Missing bindings log a one-time warning, and the id keeps a failing handle whose reads return `ConfigStoreError::Unavailable`."
- **EZ-25.23** `docs/guide/adapters/overview.md:195`. Now "when `configure` or `app_state` reads values". No `app_state` API exists. The macro argument is `state = <expr>` (`edgezero:crates/edgezero-macros/src/app.rs:61-66`). Suggested replacement is "when `configure` or the `app!` `state` expression reads values".

Axum guide:

- **EZ-25.24** `docs/guide/adapters/axum.md:259-261`. The text says wiring `App::build::<App>(edgezero_adapter_axum::AXUM_PLATFORM)` and `AxumDevServer` by hand remains the fallback. At F `AxumDevServer` takes a `RouterService` and wraps it in `App::new`, so application policies are discarded (`edgezero:crates/edgezero-adapter-axum/src/dev_server.rs:93-168`), while `AxumDevServerConfig::ingress` still applies (`edgezero:docs/guide/adapters/axum.md:104-113`). Until EZ-23 lands, suggested replacement is "`AxumDevServer::with_config(router, config)` accepts a router, not an `App`, and serves it through `App::new` with default application policies, so policies installed by `Hooks::configure`, such as `App::set_ingress_head_limits`, the admission policy, and the error renderer, do not apply. `AxumDevServerConfig::ingress` still applies. Use `run_app` when those policies matter." After EZ-23 lands, the text MUST name the delivered way to run a programmatically configured Axum server and state that every `configure` policy, including ingress head limits, applies.
- **EZ-25.25** `docs/guide/adapters/axum.md:266-299`, Testing. The sample calls `app.router()` and `router.oneshot`, which skip admission, head limits, response framing, and egress policy (`edgezero:crates/edgezero-core/src/router.rs:562-569` against `edgezero:crates/edgezero-core/src/app.rs:267-284`), and it relies on `use super::*` for `Body`. Until EZ-23 lands, suggested addition after the block is "`router.oneshot` exercises routing, middleware, and handlers only. It skips ingress admission, head limits, response framing, and egress policy." After EZ-23 lands, the sample MUST run the request through real Axum ingress using the delivered test facility, and MUST still contain `EdgeZeroApp::build::<App>(edgezero_adapter_axum::AXUM_PLATFORM)` (EZ-25.44).
- **EZ-25.26** `docs/guide/adapters/axum.md:488-489`. Now "constructs one app per server startup and serves clones of its router." The service holds one `Arc<App>` (`edgezero:crates/edgezero-adapter-axum/src/service.rs:45-77`). Suggested replacement is "constructs one app per server startup and serves every connection from that shared `Arc<App>`, including its policies."

Toolchain and CHANGELOG:

- **EZ-25.27** `CHANGELOG.md:408-409`. Now "Rust `1.95.0`, Node `24.12.0`, and Fastly CLI `15.1.0` remain the workspace toolchain." `.tool-versions` pins `rust 1.98.1`, `nodejs 24.20.0`, and `fastly 16.0.0` (`edgezero:.tool-versions:1-3`), the scaffold pins `rust 1.98.1` (`edgezero:crates/edgezero-cli/src/generator.rs:629`), and the same CHANGELOG says main's 1.98.1 toolchain was merged (`edgezero:CHANGELOG.md:16-17`). Suggested replacement is "The workspace toolchain is Rust `1.98.1`, Node `24.20.0`, and Fastly CLI `16.0.0`, matching `.tool-versions`. See the dependency-majors migration guide for the moved pins."
- **EZ-25.28** `docs/guide/adapters/axum.md:435`. Now `FROM rust:1.95.0 as builder`. Suggested replacement is `FROM rust:1.98.1 as builder`. Whether the `rust:1.98.1` image is built on the same Debian release as the `debian:bookworm-slim` runtime stage is **Unverified**. `docker run --rm rust:1.98.1 cat /etc/os-release` would settle it, and `rust:1.98.1-bookworm` keeps the stages matched either way.
- **EZ-25.29** `docs/guide/adapters/axum.md:162`, `:403`, and `crates/edgezero-adapter-axum/src/ingress_config.rs:150`, `:170`. Each says an allocation audit was done against Rust 1.95.0, while the workspace builds on 1.98.1. These are audit records. EdgeZero SHOULD re-run the receive-scratch, header-map, and config-decoder audits on 1.98.1 and change "1.95.0" to "1.98.1". If it does not, the suggested change in all four places is to replace "and Rust 1.95.0" with "and Rust 1.95.0 (not yet repeated on the workspace's Rust 1.98.1)".

Migration guides, CHANGELOG, and links:

- **EZ-25.30** `CHANGELOG.md:338-342`. Now "Fastly `runtime_env_config` now returns `FastlyRuntimeConfig` ... call `emit_boot_diagnostics()`". None of the three names exists in `crates/` (`git grep -n 'runtime_env_config\|FastlyRuntimeConfig\|emit_boot_diagnostics' -- crates` is empty), and `CHANGELOG.md:28-30` already says selector-store setup is retired. Suggested replacement for the paragraph is "Fastly `runtime_env_config`, `FastlyRuntimeConfig`, and `emit_boot_diagnostics()` are removed with no compatibility alias. Custom entrypoints open logical store aliases directly and take logging policy from baked `Hooks::logging_for("fastly")`. Managed runners initialize logging before application configuration. Axum now returns logger-setup errors instead of ignoring them, and preinstalled backends must use `owns_logging = true`."
- **EZ-25.31** `CHANGELOG.md:385`, the migration table row "Fastly `runtime_env_config(...) -> EnvConfig` | `FastlyRuntimeConfig`, using `.env` and `emit_boot_diagnostics()` after logger setup". Suggested right cell is "Removed. Use `run_app`, `run_app_with_hooks`, or `request::send_request_with_registries_and_hooks` with baked `Hooks::stores()` and `Hooks::logging_for("fastly")`".
- **EZ-25.32** `CHANGELOG.md:253`. Now "Fastly's manual `stream_to_client`". Suggested replacement is "Fastly's adapter-owned streaming body handle", for the reason in EZ-25.6.
- **EZ-25.33** `docs/guide/manifest-store-migration.md:173-174`. Now "Fastly additionally exposes a public `request::dispatch_with_registries` for custom entry points." No public `dispatch_with_registries` exists. Cloudflare and Spin keep crate-private ones (`edgezero:crates/edgezero-adapter-cloudflare/src/request.rs:671`, `edgezero:crates/edgezero-adapter-spin/src/request.rs:622`), and Fastly's public helpers are at `edgezero:crates/edgezero-adapter-fastly/src/request.rs:534` and `:557`. Suggested replacement is "Fastly additionally exposes public `request::send_with_registries_and_hooks` and `request::send_request_with_registries_and_hooks` for custom entry points."
- **EZ-25.34** `docs/guide/dependency-majors-migration.md:34-35`. Now "including `worker = "=0.8.5"`". The workspace requirement is `worker = { version = "=0.8.5", default-features = false, features = ["http"] }` (`edgezero:Cargo.toml:120`), and the scaffold and fixture match it. Suggested replacement is "Keep the audited SDK requirements exact, including `worker = { version = "=0.8.5", default-features = false, features = ["http"] }` for Cloudflare applications."
- **EZ-25.35** `docs/guide/dependency-majors-migration.md:65-70`. The scaffold pin table has no `spin` row, but the generator now emits `spin 4.1.0` (`edgezero:crates/edgezero-cli/src/generator.rs:626-628`). Suggested row is "| `spin` | not pinned | **4.1.0** | Spin adapter |".
- **EZ-25.36** `docs/guide/dependency-majors-migration.md:72-75`. Replace the whole paragraph, which says "the deploy action's `install-fastly.sh` fails closed when `.tool-versions` and its own `versions.json` disagree, so an app that moves the Rust pin and leaves the Fastly one behind gets a red deploy". The script compares `versions.json` with the EdgeZero action checkout's `.tool-versions` (`edgezero:.github/actions/deploy-fastly/scripts/install-fastly.sh:42-47`, root set at `edgezero:.github/actions/deploy-fastly/action.yml:182`), not the application's. Suggested replacement is "The deploy action installs the Fastly CLI pinned by its own `versions.json` and fails closed when that pin disagrees with the EdgeZero action repository's `.tool-versions`. It does not read the application's `fastly` line, so keep that line equal to the action's pin for local runs to match deployment."
- **EZ-25.37** `docs/guide/configuration.md:698-703`, the `app!` argument table. It has no `configure` row although the macro accepts `configure = <expr>` (`edgezero:crates/edgezero-macros/src/app.rs:42-49`). Suggested row is "| `configure` | `configure = <expr>` | Emits `Hooks::configure`, calling `(expr)(&mut App) -> Result<(), EdgeError>`. Errors propagate through `App::build`. Defaults to `Ok(())` |". When EZ-22.10 lands, the table MUST also describe fallible state.
- **EZ-25.38** `docs/guide/configuration.md:547`. Now "| `echo_stdout` | Fastly, Axum | Mirror logs to stdout |". The Axum runner builds its logger from the level only (`edgezero:crates/edgezero-adapter-axum/src/dev_server.rs:447-451`), and `axum.md:236` already says `echo_stdout` has no effect there. Suggested replacement is "| `echo_stdout` | Fastly | Mirror logs to stdout. The Axum runner ignores it |".
- **EZ-25.39** `docs/guide/blob-app-config-migration.md:191`, `:377`, and `docs/guide/configuration.md:443`. They name `ConfigOutOfDate`, but extraction raises `EdgeError::StoreExtraction`, whose wire kind for a missing blob, missing secret, deserialization, or validation failure is `config_out_of_date` (`edgezero:crates/edgezero-core/src/error.rs:84-101`, `:184-186`). Suggested replacements are "a `config_out_of_date` error (`EdgeError::StoreExtraction` with reason `MissingSecret`) at runtime" for "`ConfigOutOfDate` at runtime" at `:191`, the heading "### `config_out_of_date` at runtime" at `:377`, and "is a `config_out_of_date` error" for "is a `ConfigOutOfDate` error" at `configuration.md:443`. No link uses the old anchor.
- **EZ-25.40** Broken anchors, checked against a local VitePress build of `docs/`. `docs/guide/capabilities.md:106` links `/guide/adapters/axum#bounded-http1-ingress`, which should be `#bounded-http-1-ingress` for the heading at `axum.md:83`. `docs/guide/cli-reference.md:408` links `#config-gc`, which should be `#edgezero-config-gc`. `docs/guide/deploy-action-adoption.md:138-139` links `./adapters/fastly.md#migrating-a-custom-entrypoint`, a heading this PR removed, which should link `./adapters/fastly.md#entrypoint` (heading at `fastly.md:56`).
- **EZ-25.41** `docs/guide/cli-reference.md:588`. Now "(KV in-memory; config in `.edgezero/local-config-<id>.json`)". Local KV is a redb file (`edgezero:crates/edgezero-adapter-axum/src/dev_server.rs:231-243`). Suggested replacement is "(KV in `.edgezero/kv-<slug>-<hash>.redb`; config in `.edgezero/local-config-<id>.json`)".

Checks that keep the guides honest:

- **EZ-25.42** Every complete Rust item in `docs/guide/` SHOULD compile in EdgeZero's CI against the current workspace. A fragment SHOULD either compile inside a documented wrapper or be marked `ignore` with a one-line reason. EZ-25.2, EZ-25.14, EZ-25.16, and EZ-25.17 are compile errors that this would have caught.
- **EZ-25.43** Drift of the kinds in EZ-25.6, EZ-25.27, and EZ-25.30 SHOULD be caught automatically. The check MUST NOT flag the old-version column of a migration table, the audit records EZ-25.29 keeps, or the explanation at `fastly.md:347-349`.
- **EZ-25.44** The fixes MUST leave EdgeZero's existing documentation checks passing. The docs contract check MUST accept EZ-25.6's corrected phrase, MUST add EZ-25.10's row to its expected Limits rows, and MUST keep its `axum.md` build-string assertion satisfied by EZ-25.25's sample.

#### Acceptance criteria

1. Environment is EdgeZero CI on the recorded commit. Given the fixes, when `docs/guide` is searched for `stream_to_client`, then only `adapters/fastly.md:347` matches.
2. Environment is EdgeZero CI on the recorded commit. Given the fixes, when `CHANGELOG.md` and `docs/guide/adapters/axum.md` are searched for `1.95.0`, then only the audit statements EZ-25.29 keeps match.
3. Environment is EdgeZero CI on the recorded commit. Given EZ-25.42, when CI runs, then every changed snippet that is a complete item compiles, including the forwarding example, and every changed fragment either compiles inside its documented wrapper or is marked `ignore` with a reason.
4. Environment is EdgeZero CI on the recorded commit. Given a manifest using EZ-25.8's declaration, when the capability gate runs for each of the four adapters, then it passes.
5. Environment is EdgeZero CI on the recorded commit. Given the fixes, when EdgeZero's documentation checks run (`scripts/check_outbound_docs_contract.mjs`, `scripts/check_outbound_legacy_api.sh`, and the docs formatter), then they pass.
6. Environment is Axum, Trusted Server. TS's publisher forwarding, written from the corrected example once EZ-1 lands, forwards a bodiless GET whose inbound body is an unread stream to a local origin with no request body. TS's Fastly custom lifecycle compiles against the five-argument helper the README now shows.

#### Out of scope

- Edits to `docs/superpowers/specs/`, which EdgeZero treats as immutable (`edgezero:.github/workflows/test.yml:72-73`). Where a spec is stale, the guides carry the current text.
- New capability cells. EZ-25.9 adds footnote text to an existing BestEffort cell, and EZ-25.8 changes only the example declaration.

#### Open questions

1. Should guide snippets compile in CI (EZ-25.42)? TS prefers yes, starting with `proxying.md`, `handlers.md`, and `adapters/fastly.md`, where every compile error in this list lives.
2. Should the axum allocation audits be re-run on Rust 1.98.1 (EZ-25.29)? TS prefers yes, because the audit text promises the numbers hold for the pinned toolchain.

#### Interim for Trusted Server

TS ports from the code and from the corrected texts in this section, not from the guides as they stand at F. No transport code is involved.

## Qualification gate

### EZ-Q1. Recorded passing qualification run of the canonical Fastly composition

**Priority:** Gate
**Adapters:** all four, with hosted Fastly for the criteria that name it
**Related:** EZ-1 to EZ-9 (their acceptance criteria are what this record covers). EZ-2, EZ-6, and EZ-15 (their reuse-mode parts gate later). The Trusted Server cutover spec holds the run plan that produces the record.

#### Need

Fastly is TS's production target, and the cut moves TS onto `run_custom` with `send_request_with_registries_and_hooks`, a composition TS has never run. EdgeZero's capability guide says "the production `run_app` composition has not yet been exercised end to end under Viceroy" (`edgezero:docs/guide/capabilities.md:284-288`). The fixture README says its cases "describe fixture coverage, not a recorded pass for the current pinned SDK/runtime graph" (`edgezero:tests/fixtures/reusable-app/README.md:285-287`). TS will not ship the cut to production without a record, for the exact commit it pins, showing that the blocking requirements hold in the environments their criteria name.

#### Gap at F

CI run 38934086784 passed EdgeZero's Fastly smoke at F with Viceroy 0.21.0, Rust 1.98.1, and Fastly SDK 0.13.1 (`edgezero-ci:run 38934086784`). It does not meet the need. Its artifact expires after 14 days (`edgezero:.github/workflows/test.yml:405-411`), it records no source revision (`edgezero:tests/fixtures/reusable-app/README.md:315-317`), its runner label does not pin an image, its cases do not cover the blocking requirements in this document, and no hosted Fastly run exists. The hosted egress tool contacts no outbound backend (`edgezero:tests/fixtures/reusable-app/README.md:69-71`).

#### Requirements

- **EZ-Q1.1** For the exact EdgeZero commit TS pins, there MUST be a recorded result for every acceptance criterion of the blocking parts of EZ-1 to EZ-9 and of any requirement promoted to blocking, each in the environment that criterion names.
- **EZ-Q1.2** Every such result MUST pass. Each of those criteria is pass-or-fail, and a measurement it names is judged against the bound it states. A leg or value that a criterion says only to record passes when the record contains it. A blocking criterion without a pass condition is a defect in this document, to be fixed before the record is produced.
- **EZ-Q1.3** The record MUST name the EdgeZero commit, the toolchain (Rust 1.98.1, Viceroy 0.21.0, Fastly SDK 0.13.1, Spin 4.1, and the workerd version, where used), the runner image, and the TS baseline and candidate builds behind any comparison, and for each criterion its environment, result, and any measurements with sample counts. A record covers that one commit only.
- **EZ-Q1.4** EdgeZero produces the results for criteria in unit, Viceroy 0.21.0, workerd, Spin 4.1, and Axum that do not need Trusted Server, and assembles and keeps the record. For hosted Fastly criteria whose settings TS's configuration cannot express, EdgeZero MUST provide a qualification build that TS can deploy to a TS staging service, serving single-request sandboxes (`run_custom`) unless a criterion says otherwise. These are EZ-1 criterion 2 and the hosted leg of criterion 6, EZ-2 criteria 11, 12, 13b, and 14, EZ-4 criterion 8, the hosted legs of EZ-6 scenarios 1, 2, 6, 7, 8, and 11, and EZ-8's hosted criterion. TS produces its own end-to-end checks and the remaining hosted Fastly results with its own build, in single-request mode unless a criterion says otherwise.
- **EZ-Q1.5** Criteria of reuse-mode follow-ups (EZ-2's reuse-mode part and the reuse-mode legs of EZ-6 and EZ-15) are not part of the gate for single-request production. They MUST pass in a record before TS enables reuse mode.
- **EZ-Q1.6** A follow-up present at the pinned commit, as its CHANGELOG states and TS confirms, adds its acceptance criteria to those EZ-Q1.1 requires, and EZ-Q1.2 applies to them. The reuse-mode criteria of EZ-Q1.5 stay out of the gate even when present. An absent follow-up's interim applies instead.
- **EZ-Q1.7** The record MUST stay retrievable for as long as TS pins that commit.

#### Acceptance criteria

1. Trusted Server review. A record that names a commit other than the pin is rejected.
2. Trusted Server review. A record that lacks a result for any criterion EZ-Q1.1 or EZ-Q1.6 requires, records a result in an environment other than the one the criterion names, or records a failing result is rejected.
3. Trusted Server review. A record whose toolchain differs from EZ-Q1.3, that names no runner image, or that omits measurement sample counts or the builds behind a comparison is rejected.
4. Trusted Server review. A record stored only where it expires within 90 days, such as a CI artifact, is rejected.

#### Out of scope

The run plan. Which party runs which leg, the case list, scripts, and evidence formats belong in the Trusted Server cutover spec, which consumes this record and does not hold them yet. EdgeZero is asked for no runs beyond the acceptance criteria in this document.

#### Open questions

1. Where EdgeZero keeps the record. Trusted Server prefers a release asset attached to the tag TS pins, because CI artifacts expire.

## Review log

- **Round 1 (Claude Code, five fresh reviewers by section plus one whole-document).** Thirteen
  blocking findings, almost all contradictions between sections (synchronous versus async begin,
  two owners for in-flight request state, an unbounded background drain, drop and fire-and-forget
  semantics, stale references to Fastly internals, tests that depended on a follow-up, and an
  unreachable qualification case). All were resolved, then the TS maintainer asked for requirements
  rather than implementation, and every section was rewritten to observable behavior and acceptance
  criteria.
- **Round 2 (Claude Code, three fresh reviewers on a different model).** Thirteen blocking findings.
  Resolved by defining when a response arrives and when a request's lifetime ends, rejecting only
  options marked Unsupported (BestEffort options are accepted), correcting the Viceroy 0.21.0 timer
  facts, making EZ-Q1 gate every blocking criterion with hosted evidence from TS's own composition
  and a pre-cut TS baseline, splitting EZ-2 into a blocking part and a reuse-mode follow-up because
  production runs single-request sandboxes, and removing the remaining mechanism and harness text.
- **Round 3 (Claude Code, two fresh reviewers).** Every round 2 blocking finding was confirmed
  resolved in the text. New blocking findings were all in how EZ-Q1 assigned runs. Resolved by
  separating EdgeZero-side Viceroy evidence from Trusted Server end-to-end checks, running hosted
  criteria that TS configuration cannot express on an EdgeZero qualification build that TS deploys,
  gating reuse-mode criteria only before TS enables reuse mode, fitting Viceroy cases inside its
  six-request guest limit, and defining fire-and-forget lifetime as the one exception to request
  lifetime.
- **Round 4 (Claude Code, one fresh reviewer on the round 3 changes).** Every round 3 finding was
  resolved. One blocking finding, that two hosted measurements had no pass condition. Resolved by
  giving every blocking measurement a pass condition. EZ-Q1 had grown into a full run plan, which is
  Trusted Server's rollout work rather than an EdgeZero requirement, so it now states only what the
  record must show, and the run plan moves to the Trusted Server cutover spec.
- **Round 5 (Claude Code, one fresh reviewer on the round 4 changes).** Round 4's blocking finding
  was resolved. One new blocking finding, that EZ-Q1.5 and EZ-Q1.6 disagreed on a reuse-mode
  follow-up present at the pinned commit, and that nothing rejected a record missing a present
  follow-up's results. Fixed with the reviewer's replacement text and checked against the head by the
  spec author, without a further agent round. Its should-fix items were applied (EZ-6 hosted timing,
  the definition of "every adapter", four measurement bounds, conditional workerd and Spin
  criteria, and the record's contents).
- **Declined findings that explain a requirement's meaning.**
  - EZ-9. A followed 3xx body never fails a redirect chain, including its declared length, and is read only within the bound EZ-9.11 sets.
  - EZ-6. A begun request does not accept streamed uploads. TS begins only its overlapped navigation
    `GET`. A Fastly body read the host cannot interrupt is bounded by one slot body's transfer time,
    not by the earliest deadline.
  - EZ-14. Forcing every redirect hop to the default body policy would drop a streamed caller's idle
    bound on the final hop.
  - EZ-17. Promoting only part of EZ-17 would make TS migrate the same report twice.
  - EZ-Q1. A record covers exactly one EdgeZero commit. There is no carry-forward to a later pin.
