# Trusted Server v2: Encypher provenance as a module

## Status

Complete, as a draft pull request:
https://github.com/IABTechLab/trusted-server/pull/1448, branch
`encypher/v2-provenance-module` on the `erik-sv` fork, based on
`feature/55-scripts-in-crates` at 5be114773 (row 55 of the stack tracked in
#777). Signing against the live Encypher API has not been exercised.

## Current Goal

None open. The port is implemented, verified locally and published for review.

## Overview

Encypher's text provenance contribution signed article pages from inside core,
through an HTML stream processor factory, a request context type and core
changes of its own. The stack in #777 moves every vendor out of core and lets a
module change a page only through a middleware. This work moves the
contribution onto those interfaces as `crates/provenance/encypher`
(`provenance.encypher`) and adds the neutral core seams it needs. Design:
`docs/superpowers/specs/2026-10-24-encypher-provenance-module-design.md`.

## Objectives

1. Port every existing Encypher behavior (gates, policy and plan caches, sign
   call, render, fail-open, logging) to a vendor-owned module crate.
2. Add only vendor-neutral core seams: an awaited asynchronous finish for
   middleware stream processors, an optional platform timer, response facts
   for middleware, runtime KV on the adapters that lacked it.
3. Remove obsolete paths in the port: the `enabled` flag, `path_prefixes`,
   `[integrations.encypher]`, the stream processor factory, the shared-template
   refusal and the synchronous publisher body path.
4. Keep the permission contract: no permission required, no gated value read,
   no page the origin marks private sent.
5. Publish a reviewable pull request against the stack head without changing
   any upstream branch.

## Work Breakdown

- [x] Inspect epic #777, PR #1423 head and the existing Encypher branches
- [x] Create an isolated worktree and branch from 5be114773
- [x] `PlatformTimer` on `RuntimeServices`, with neutral documentation
- [x] Fastly opens the KV store linked as `trusted_server_kv`
- [x] `StreamProcessor::async_finish` and `finish`, composed in order by every
      composite processor
- [x] Every awaiting publisher path ends its processor with `finish`; the
      synchronous `stream_publisher_body` path removed
- [x] `MiddlewareContext::status` and `origin_private`, read before core's
      cache policy
- [x] Axum runtime KV from `TRUSTED_SERVER_KV_PATH`, on the blocking pool
- [x] Timers on Cloudflare, Spin and Axum
- [x] `crates/provenance/encypher`: config, secret setting, middleware,
      ported processor and tests, publisher-path tests
- [x] Module offered on all four adapters and the CLI
- [x] Auction-hold regression test with an asynchronously finished document
- [x] Documentation: configuration guide, Axum guide, example config, design
- [x] Data-handling wording narrowed to what the gates prove
- [x] All repository CI gates run locally
- [x] Axum runtime smoke with a controlled origin
- [x] Draft pull request opened and its body updated with evidence

## Success Criteria

| Criterion                                | Result                                                                                    |
| ---------------------------------------- | ----------------------------------------------------------------------------------------- |
| Signing works through the publisher path | Met in process: buffered and streaming finalizers sign against a stub origin and stub API |
| Every failure passes the page through    | Met: ported failure tests; runtime smoke `reason=http_error` with the page intact         |
| A private origin page is never sent      | Met: in-process test and runtime smoke `gate=origin_private`                              |
| Core names no vendor or platform         | Met: `scripts/lint-core-platform-names.py` exit 0                                         |
| CI gates green                           | Met locally, see Completion Notes                                                         |
| Live API signing                         | Not met: not exercised, see limits                                                        |

## Completion Notes

Tests were ported from the earlier Encypher branches and written alongside the
port. The work was not strictly test-first, and no baseline run of the stack
was recorded before the change.

Commands run in the worktree, with exit codes:

- `cargo fmt --all -- --check`: 0
- `cargo clippy-fastly`, `clippy-axum`, `clippy-cloudflare`,
  `clippy-cloudflare-wasm`, `clippy-spin-native`, `clippy-spin-wasm`,
  `clippy-cli`, `clippy-codegen`: 0 each
- `cargo test -p trusted-server-provenance-encypher`: 65 passed
- `cargo test -p trusted-server-core --lib`: 3020 passed; the auction-hold
  regression test added after it: 1 passed
- `cargo test-fastly`, `test-axum`, `test-cloudflare`, `test-spin`: no
  failures (run before the rustfmt-only change and the auction-hold test)
- `cargo check-cloudflare`, `check-spin`, `build-fastly`: no errors
- `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity`: 0
- `scripts/lint-wasm-clock.sh`: 0
- `python3 scripts/lint-core-platform-names.py`: 0
- `cd crates/trusted-server-js/lib && npx vitest run`: 0; `npm run format`: 0
- `cd docs && npm run format`: 0; prettier check of markdown outside `docs/`: 0
- `cargo test -p trusted-server-core --test template_build_digest`: 0; its
  clippy with `-D warnings`: 0
- docs-proposal: `uvx --from shellcheck-py shellcheck scripts/docs-proposal/*.sh`
  (shellcheck is not installed on the host): 0;
  `node --test scripts/docs-proposal/hunks.test.mjs`: 0;
  `scripts/docs-proposal/test.sh`: 0

Runtime smoke, Axum dev server, throwaway script and mock origin, no real
credentials. The module was selected with a `[[serve]]` entry on `/news/`,
runtime KV on, and `[geo] assume_single_jurisdiction = true`, which the stack
requires once `[ec] module` is set. Observed:

```text
[edgezero] config validate (typed): edgezero.toml + trusted-server.toml OK (strict)
page=public status=200 harbor_text=1 marker=0
page=private status=200 harbor_text=1 marker=0
INFO  [trusted_server_provenance_encypher] encypher: registered for domain=publisher.example.com
INFO  [trusted_server_provenance_encypher::processor] encypher: outcome=passed_through reason=http_error
DEBUG [trusted_server_provenance_encypher] encypher: document not held, gate=origin_private
```

`scripts/smoke-axum.sh` itself does not set the `[geo]` value and stops at
that check. This branch leaves `scripts/` and the core settings, config and
Edge Cookie code unchanged from 5be114773; the script was not run on the base
commit.

Limits:

- Live API: the API address must be `https`, and the Axum client trusts only
  the public certificate authorities built into it, so the runtime smoke could
  not reach a local signing stub. The client was not weakened. Signing is proven
  in process only.
- Personalized pages: the module skips pages whose origin set a cookie or
  forbade shared caching, and cannot detect others. A deployment places it on
  public article routes.
- Spin's declared KV store also holds the settings; the module's keys are
  namespaced.
