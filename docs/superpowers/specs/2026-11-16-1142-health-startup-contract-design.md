# Cross-adapter health and startup-failure contract

Status: approved liveness/readiness design, implemented locally.
Issue: [#1142](https://github.com/IABTechLab/trusted-server/issues/1142).
Source baseline: Trusted Server `1810c763a`; EdgeZero `v0.0.11`, resolved in
`Cargo.lock` to `87456c708ffe9edf86b3a0f8b06dfd15d60b07b5`.

## Purpose and decision

Give operators the same answer to two different questions on every adapter:

- `/health`: can this running adapter answer a request?
- `/ready`: did this instance initialize the application needed to serve traffic?

Use `/health` for liveness and `/ready` for load-balancer admission and deployment
checks. Readiness does not poll origins, bidders, identity stores, or other
request-time dependencies. A later identity-store outage does not make an
otherwise initialized application unready. This follows AX-6 of the
[parent identity design](https://github.com/IABTechLab/trusted-server/blob/c7dc9de77d31692412aec2768ae3f11fe0d52bbb/docs/superpowers/specs/2026-11-11-server-side-identity-all-adapters-design.md).

All caught application-initialization failures produce a generic public `503`
on ordinary supported routes. Detailed diagnostics remain in server logs.
Ordinary errors after successful initialization retain their existing mapping.

## Alternatives considered

| Approach                                                        | Benefit                                                                    | Cost                                                                                     |
| --------------------------------------------------------------- | -------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------- |
| Separate liveness and readiness, with a common startup fallback | Preserves Fastly/Spin liveness and gives traffic routing a distinct signal | Adds `/ready`, reserves probe paths, and changes some startup responses                  |
| Make `/health` readiness everywhere                             | One probe path                                                             | Changes existing Fastly/Spin liveness semantics and gives no independent liveness signal |
| Document existing differences                                   | Least implementation work                                                  | Keeps different monitoring rules and error-derived startup statuses                      |

The first approach is selected. Keep the change confined to probe handling,
startup response classification, affected consumers, tests, and documentation.

## Current behavior

| Adapter    | Healthy `GET /health`                              | Failed initialization | Ordinary failed-startup response                                                  |
| ---------- | -------------------------------------------------- | --------------------- | --------------------------------------------------------------------------------- |
| Fastly     | Pre-initialization `200 ok`                        | Still `200 ok`        | Error-derived status and public message; some failures return an entrypoint `500` |
| Axum       | Router `200 ok`                                    | Startup error         | Error-derived status and public message                                           |
| Cloudflare | Publisher/integration fallback; no dedicated probe | Startup error         | Error-derived status and public message                                           |
| Spin       | Router `200 ok`                                    | Still `200 ok`        | Fixed `503`, `Service Unavailable\n`                                              |

Error-derived status is not necessarily `500`: the existing Fastly startup
router test uses a `BadRequest` error and expects `400`. Core's existing public
error conversion already suppresses server-error details. This work changes the
classification of initialization failures, not the general error system.

## Required response matrix

Each row applies to the application selected for the current process, isolate,
component invocation, or Fastly sandbox. Bodies below are exact UTF-8 bytes;
`\n` denotes one trailing newline. HEAD returns the GET status and contract
headers with an empty body. Transport-generated headers may differ.

| Adapter    | State                 | GET `/health` | GET `/ready`                   | Ordinary supported routes      |
| ---------- | --------------------- | ------------- | ------------------------------ | ------------------------------ |
| Fastly     | Initialized           | `200`, `ok`   | `200`, `ready`                 | Existing route behavior        |
| Fastly     | Initialization failed | `200`, `ok`   | `503`, `Service Unavailable\n` | `503`, `Service Unavailable\n` |
| Axum       | Initialized           | `200`, `ok`   | `200`, `ready`                 | Existing route behavior        |
| Axum       | Initialization failed | `200`, `ok`   | `503`, `Service Unavailable\n` | `503`, `Service Unavailable\n` |
| Cloudflare | Initialized           | `200`, `ok`   | `200`, `ready`                 | Existing route behavior        |
| Cloudflare | Initialization failed | `200`, `ok`   | `503`, `Service Unavailable\n` | `503`, `Service Unavailable\n` |
| Spin       | Initialized           | `200`, `ok`   | `200`, `ready`                 | Existing route behavior        |
| Spin       | Initialization failed | `200`, `ok`   | `503`, `Service Unavailable\n` | `503`, `Service Unavailable\n` |

### Paths, methods, and headers

- Recognize the exact URI paths `/health` and `/ready`, ignoring the query.
  Do not add prefix matching, slash normalization, redirects, or case folding.
  `/health/`, `/ready/`, and nested paths retain ordinary routing.
- Support GET and HEAD explicitly. EdgeZero does not implicitly map HEAD to GET.
  Other methods on these paths retain ordinary supported-method handling.
- Ordinary failed-startup coverage includes GET, POST, HEAD, OPTIONS, PUT, PATCH,
  and DELETE on `/` and nested paths. Unsupported methods retain existing boundary behavior: router method rejection
  where dispatch is reached, or the existing entrypoint response before dispatch.
  All HEAD failure responses have an empty transmitted body.
- Successful probes use `Content-Type: text/plain`; all startup-failure responses
  use `Content-Type: text/plain; charset=utf-8`.
- Probes and startup-failure responses use `Cache-Control: no-store`. They emit
  no `Set-Cookie`, `WWW-Authenticate`, or `Retry-After` header.
- Apply the compiled `x-ts-version` when available, without settings or store
  access. Do not add Fastly runtime/environment or geo headers to probes.
  Fastly liveness continues to omit `x-ts-fastly-version`.
- These operational responses bypass application authentication, configured
  response headers, trace capture, EC processing, integrations, and publisher
  forwarding. They do not consume caller credentials or reflect request data.
  Ordinary healthy routes keep their existing authentication and middleware.

## Readiness and failure boundaries

Successful startup means the adapter's existing settings loading, secret
resolution, target validation, auction-plan/orchestrator construction, and
integration-registry construction completed. Do not add a second initialization
path, new dependency checks, configuration refresh, or a global readiness flag.
Existing laziness and state lifetimes remain unchanged.

Fastly readiness follows the ordinary initialization path, including required
entrypoint configuration-store access. Catchable configuration-store-open and
application-build failures return the same unavailable response. A readiness
request must not return success merely because it reached the entrypoint.
Reuse `sandbox.initialize`: retain successful state, discard failed construction,
and retry on the next callback as today. Liveness must not initialize the app or
increment initialization-attempt counters.

Readiness describes the selected instance's configuration snapshot, not the
latest external configuration, another instance, current dependency availability,
or a guarantee that every publisher request will succeed. Cloudflare's existing
configuration capture and the broader shared-state work are outside this issue.

Caught initialization errors of every variant, including `BadRequest`, map to
the same public unavailable response. Log the existing detailed report once at
the failure boundary without adding request credentials or new secret-value
logging. Do not return error attachments, binding names, settings paths, or
configuration contents in the public body. Preserve existing runtime and SDK
dispatch error mappings outside application initialization.

### Supported platform differences

| Adapter    | Timing and boundary                                                                                                                                |
| ---------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| Fastly     | Preserve the existing native liveness fast path before settings and app construction. Readiness runs through initialization.                       |
| Axum       | Application/router construction happens before the listener binds. Probes answer after successful construction or selection of the failure router. |
| Cloudflare | Probes answer after the Worker and SDK have entered application dispatch; startup configuration errors caught by the app are covered.              |
| Spin       | Probes answer after the component and SDK have entered application dispatch; startup configuration errors caught by the app are covered.           |

No response is promised when the runtime cannot start, imports fail, the listener
cannot bind, execution traps or hangs, request conversion fails before dispatch,
or the SDK fails outside the application's caught initialization boundary.
Do not claim cheap pre-initialization liveness on every platform. Spin's own
`/.well-known/spin/health` remains a separate platform endpoint.

## Minimal implementation structure

Add one small `trusted-server-core::health` module for path/method classification,
response construction, and a pre-dispatch hook. The hook knows only initialized
versus failed startup and an optional existing pre-dispatch delegate. Check
operational probes first; otherwise delegate to the existing trace hook. Failed
routers need no trace delegate. Install one combined hook rather than replacing
trace behavior or trying to install two SDK hooks.

The pre-dispatch hook must run before route lookup and middleware. Its probe
responses are terminal and cannot be changed by application response finalizers.
Use explicit GET/HEAD classification here; do not rely on GET route registration
to handle HEAD. Healthy non-probe requests proceed unchanged. Failure routers
use the shared unavailable builder for their existing supported-method catchalls
and drop default-settings finalization that can mutate the response contract.

Cloudflare and Spin currently install their actual diagnostic log sinks only
with `test-utils`; the pinned SDK's logger initialization does not supply a
production sink for either. Make those existing adapter sinks available in
shipping builds before application construction so startup `log::error!` reports
are emitted. Keep fixture-only panic instrumentation separate. This is limited
to making the required startup diagnostics observable, not redesigning logging.
Verify diagnostic emission with production artifacts built without `test-utils`.

Fastly's native liveness uses the shared contract values through its necessary
SDK response conversion. Its readiness path uses the existing initialization
result and returns a shared terminal response before ordinary request services
or finalization. Audit native post-router finalizers too: a router-level bypass
alone is insufficient. Reuse the same response builder for pre-router startup
failures. Do not introduce new platform capabilities or an EdgeZero version
change solely for this contract.

## Compatibility and rollout

- `/ready` becomes a reserved GET/HEAD path on every adapter. Cloudflare also
  stops forwarding GET/HEAD `/health` to publishers. Publishers using those
  paths need to move their endpoints before upgrade.
- Axum `/health` remains green after caught initialization failure. Move
  load-balancer and deployment checks that need a usable app to `/ready`.
- Catch-all Basic-auth rules no longer challenge either probe. Probes disclose
  only their fixed operational result and compiled version.
- HEAD is explicitly local for both probes. Other methods remain ordinary.
- Fastly, Axum, and Cloudflare initialization errors become `503`; update
  monitors and fixtures that expect `500` or another mapped error status.
- Update application-usability waits in integration and HTML EOF harnesses to
  readiness; expected-failure fixtures wait only for transport/liveness.
  Preserve positive publisher-response checks rather than substituting probes.

## Acceptance and preservation evidence

| Scenario                                                                             | Required observation                                                                                           | Evidence surface                                                                        |
| ------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------- |
| Valid configuration on every adapter                                                 | Both probes 200; publisher still serves the controlled origin sentinel                                         | Adapter tests and actual production-artifact runtime tests                              |
| Missing configuration, malformed envelope, invalid settings, missing required secret | Health 200; ready and ordinary routes generic 503; diagnostic identifies the controlled failure in server logs | Real configuration-loading boundary per adapter, production builds without `test-utils` |
| Startup error variants with different normal status mappings                         | Same public 503 and body for each; normal runtime conversion unchanged                                         | Failure-router unit tests and core error tests                                          |
| Catch-all auth and hostile configured response headers                               | Anonymous probes preserve status/body/content type/no-store; protected ordinary route still challenges         | Healthy router tests plus runtime control                                               |
| GET and HEAD, healthy and failed startup                                             | Exact GET bytes and empty HEAD wire body; no cookie/auth challenge                                             | Core/router tests and real HTTP assertions                                              |
| Query and near-miss paths; other supported methods                                   | Queries do not change probes; near misses and non-probe methods retain ordinary routing                        | Classifier and router tests with controlled publisher/auth controls                     |
| No runtime dependency access                                                         | Probe handling performs no origin, bidder, or identity operation beyond existing initialization                | Counting services and local origin counters                                             |
| Identity dependency outage after valid initialization                                | Ready stays 200; no new dependency polling                                                                     | Injected failing services and a runtime control where supported                         |
| Fastly success, failure, retry, and reuse                                            | Liveness never initializes; ready uses normal startup; failure is retryable; retained success is reused        | Viceroy tests with reusable-sandbox off and on                                          |
| Trace and ordinary-route preservation                                                | Non-probe trace authentication/privacy and publisher method behavior unchanged                                 | Existing trace-boundary, parity, and adapter suites                                     |

Host-only `routes_with_settings` tests do not exercise Worker/Spin configuration
bindings or the shipping entrypoint. Pair them with fresh actual-runtime fixtures
for missing, invalid, and valid configuration. Build artifacts from the tested
revision; do not accept stale artifact reuse as evidence.

## Documentation and completion

The API reference owns the single normative response matrix. Adapter guides link
to it and state only their timing limits. Align the API route-availability tables,
adapter support tables, integration startup summary, smoke scripts, and operational
examples. Historical design documents keep their historical meaning.

Implementation is complete only after the matrix is covered by adapter and real
runtime tests, existing affected consumers are updated, the documentation matches
actual dispatch, and applicable repository checks pass. A reviewed specification
or implementation plan alone is not runtime verification.

## Review log

- Round 1: found that Cloudflare and Spin installed diagnostic log sinks only
  in test builds, leaving production startup errors unobservable. Added the
  narrow production logging requirement and verification without `test-utils`.
- Round 2: verified the revised logging boundary, response matrix, middleware
  bypass, Fastly lifecycle, and consumer migration against source. No remaining
  material issues prevented implementation planning.
