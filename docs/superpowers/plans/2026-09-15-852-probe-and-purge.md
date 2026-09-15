# Issue #852 implementation plan — part 2 of 3: probe and purge

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the operator the two things the readthrough gate depends on — evidence that an
origin is safe to share, and a way to purge what gets cached.

**Architecture:** A new `ts origin probe-shareability` command that compares origin responses
across four axes and reports four response-header verdicts, all blocking. Plus a purge surface in
two halves: a reader-facing surrogate key attached at template-cache insert, and two consumers of
it — an authenticated admin endpoint and a CLI command.

**Tech Stack:** Rust 2024, `reqwest` with `rustls-tls` (host target), `wasm32-wasip1` via Viceroy,
Fastly `NamedRoute` routing, `fastly::http::purge`.

**Spec:** `docs/superpowers/specs/2026-09-15-852-template-and-origin-caching-design.md` — read
"Origin shareability probe" and "Purge" before starting.

**Part 2 of 3.** Part 1 (`2026-09-15-852-predicate-split-and-observability.md`) must be complete
first. Part 3 is the readthrough gate, which will not be safe to enable without the probe this
part builds.

---

## Why the probe's verdicts are blocking

Not a style choice. Part 3's gate is decided **before** the origin responds, and no post-response
hook is reachable on this adapter (Viceroy stubs the HTTP Cache ABI — see
`adapter-fastly/src/template_cache.rs:6-15`). So none of `template_cache_ttl`'s response-side
refusals — `Set-Cookie` (`publisher.rs:6147`), CSP nonce (`:6117`), absent freshness (`:6100`) —
can be applied to the readthrough path. This probe is the only control. Build the verdicts as
pass/fail with a non-zero exit, not as advisory output.

---

## File structure

| File                                                                  | Responsibility                 | Change                                                                               |
| --------------------------------------------------------------------- | ------------------------------ | ------------------------------------------------------------------------------------ |
| `crates/trusted-server-cli/Cargo.toml`                                | CLI deps                       | Add `reqwest` to the non-wasm block                                                  |
| `crates/trusted-server-cli/src/run.rs`                                | Command enum                   | Add `Origin` and `Cache` variants                                                    |
| `crates/trusted-server-cli/src/commands/origin/`                      | Probe                          | Create                                                                               |
| `crates/trusted-server-cli/src/commands/cache/`                       | Purge CLI                      | Create                                                                               |
| `crates/trusted-server-cli/tests/support/`                            | Fixture server                 | Add a portable loop-accept server                                                    |
| `crates/trusted-server-core/src/platform/template_cache.rs`           | Key + trait                    | `request_path` field, reader-facing key, canonicalization, `purge_url_surrogate_key` |
| `crates/trusted-server-core/src/publisher.rs`                         | Key construction, test doubles | Populate `request_path`; implement the new trait method on two doubles               |
| `crates/trusted-server-adapter-fastly/src/{template_cache.rs,app.rs}` | Purge impl + route             | New trait method; `NamedRoute` entry; handler                                        |
| `crates/trusted-server-adapter-{axum,cloudflare,spin}/src/app.rs`     | 501 routes                     | Register the path                                                                    |
| `crates/trusted-server-core/src/settings.rs`                          | `ADMIN_ENDPOINTS`              | Add the path                                                                         |
| `crates/trusted-server-integration-tests/tests/parity.rs`             | Parity                         | Authenticated POST helpers                                                           |
| `scripts/template-cache-local-test.sh` + `.github/workflows/test.yml` | Harness                        | Purge leg + workflow step                                                            |

---

# Section A — Origin shareability probe

## Task A1: Add an HTTP client the CLI can use on Linux

**Files:** `crates/trusted-server-cli/Cargo.toml`

The CLI's existing HTTP stack (`hyper`, `rustls`, `tokio` with `net`) is under
`[target.'cfg(target_os = "macos")'.dependencies]` (`:42`). The `cfg(not(target_arch = "wasm32"))`
block has `tokio` **without** `net` and no HTTP client. CI runs the CLI suite on Linux as well as
macOS, so the probe needs a client in the portable block.

- [ ] **Step 1: Add the dependency**

In `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`:

```toml
reqwest = { workspace = true }
```

`reqwest` is already a workspace dependency with `default-features = false` and
`features = ["json", "rustls-tls"]` (root `Cargo.toml:93`), already in `Cargo.lock`, and already
built natively by the Axum adapter and the integration-tests crate. No new TLS backend is linked.

- [ ] **Step 2: Verify both targets still build**

Run: `cargo check-fastly` — expected clean (the CLI is not in this alias; this confirms nothing
leaked into the wasm build).

Run: `cargo check --package trusted-server-cli --target $(rustc -vV | sed -n 's/^host: //p')` —
expected clean.

- [ ] **Step 3: Commit**

```bash
git add crates/trusted-server-cli/Cargo.toml Cargo.lock
git commit -m "Add a portable HTTP client to the operator CLI

The existing hyper/rustls stack is macOS-only, and the shareability probe
must run on Linux CI."
```

## Task A2: Add a loop-accept fixture server for CLI tests

**Files:** `crates/trusted-server-cli/tests/support/`

The only existing fixture server is reachable solely from `tests/proxy_e2e.rs`, which is
`#![cfg(target_os = "macos")]` for the dependency reason above.

**It must loop-accept.** A single-accept fixture already caused a CI flake in this repo, fixed in
PR #823: a browser opens several sockets including request-less preconnects, and the one-accept
server lost the race. The probe opens N connections by design via `--repeat`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn fixture_server_answers_repeated_requests() {
    let server = FixtureServer::start(|_req| FixtureResponse::html("<html></html>"));

    for _ in 0..3 {
        let body = reqwest::blocking::get(server.url("/")).expect("should fetch").text().expect("should read body");
        assert_eq!(body, "<html></html>", "every request must be answered, not just the first");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --package trusted-server-cli --target $(rustc -vV | sed -n 's/^host: //p') fixture_server_answers`
Expected: FAIL — `FixtureServer` not found.

- [ ] **Step 3: Implement**

A `std::net::TcpListener` on port 0 in a spawned thread, looping on `accept()` until a shutdown
flag is set, answering each connection from a caller-supplied closure. Expose `url(path)` built
from `local_addr()`. Plain `std::net` and `std::thread` — no async runtime, so it works on every
target the CLI tests run on. The response builder needs to set arbitrary status, headers, and
body, since later tasks assert on `Vary`, `Set-Cookie`, `Cache-Control` and CSP.

- [ ] **Step 4: Run to verify it passes**

Run the same command. Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-cli/tests/support/
git commit -m "Add a portable loop-accept fixture server for CLI tests

Single-accept fixtures have flaked in this repo before, and the probe opens
several connections by design."
```

## Task A3: Wire the `ts origin probe-shareability` command skeleton

**Files:** `crates/trusted-server-cli/src/run.rs`, `crates/trusted-server-cli/src/commands/origin/`

- [ ] **Step 1: Add the command**

Follow the `audit` and `dev` pattern — TS-local, not delegated to `edgezero_cli`. Add an `Origin`
variant with a `#[command(subcommand)] OriginCommand`, one variant `ProbeShareability`, and args:

```rust
pub(crate) struct ProbeShareabilityArgs {
    /// URL to probe. May be repeated.
    #[arg(long, required = true)]
    pub(crate) url: Vec<String>,
    /// How many times to repeat the self-identity comparison.
    #[arg(long, default_value_t = 3)]
    pub(crate) repeat: u32,
    /// Extra cookie to send in the cookie-bearing arm, as name=value. May be repeated.
    #[arg(long)]
    pub(crate) cookie: Vec<String>,
    /// Emit machine-readable output.
    #[arg(long)]
    pub(crate) json: bool,
}
```

Dispatch it in `run()`'s `match` alongside the existing arms.

- [ ] **Step 2: Verify it is reachable**

Run: `cargo run --package trusted-server-cli --target $(rustc -vV | sed -n 's/^host: //p') -- origin probe-shareability --help`
Expected: the help text renders.

- [ ] **Step 3: Commit**

```bash
git add crates/trusted-server-cli/src/run.rs crates/trusted-server-cli/src/commands/
git commit -m "Add the ts origin probe-shareability command skeleton"
```

## Task A4: Implement the four comparison axes

**Files:** `crates/trusted-server-cli/src/commands/origin/`

Each axis fetches the URL twice and compares the bodies. All four are blocking.

| Axis              | Arm A      | Arm B                                      | Compare                |
| ----------------- | ---------- | ------------------------------------------ | ---------------------- |
| Self-identity     | bare       | bare, repeated `--repeat` times            | raw bytes              |
| Cookie            | bare       | with the TS cookie set plus any `--cookie` | raw bytes              |
| `Accept-Encoding` | `gzip`     | `identity`                                 | bytes **after decode** |
| `User-Agent`      | desktop UA | mobile UA                                  | raw bytes              |
| RSC / router      | bare       | `rsc: 1` plus configured `next-router-*`   | raw bytes              |

The TS cookie set for the cookie arm is `ts-ec`, the consent cookies from
`CONSENT_COOKIE_NAMES` (`core/src/cookies.rs:20`), and the tester cookie — representative of what
a real repeat visitor carries.

**The RSC axis is specific to this change and easy to miss.** RSC fetches are not navigations —
`http_util.rs:73-82` requires `Sec-Fetch-Dest: document` — so they never set the bypass and
**already flow through the readthrough cache today**, while HTML navigations are PASS. Removing
the bypass puts both representations under one cache key for the first time. If the origin varies
on `rsc` / `next-router-*` without declaring it in `Vary`, the cache can serve a flight payload to
an HTML navigation. Recorded in the #1009 measurement findings as a risk nobody had considered.
Drive this axis from the operator's configured `template_cache_vary` list rather than a fixed set,
since the varying headers are publisher-specific.

- [ ] **Step 1: Write the failing tests**

One test per axis against the fixture server, each asserting the axis reports a difference when
the fixture varies on that input and reports identical when it does not. For the
`Accept-Encoding` axis, the fixture must actually gzip one arm so the test proves the comparison
happens after decode, not before.

Plus one test that a non-self-identical origin (fixture returns a counter in the body) fails the
self-identity axis — that is the most common real-world failure and must not be reported as a
cookie problem.

- [ ] **Step 2: Run to verify they fail**

Run: `./scripts/test-cli.sh` (or the explicit host-triple command). Expected: FAIL.

- [ ] **Step 3: Implement**

Report per axis: identical or differing, and on difference the byte offset of the first
divergence plus a short context window from each side. Keep the window small and escape it — it
is publisher HTML and may be large or binary-ish.

- [ ] **Step 4: Run to verify they pass**

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-cli/src/commands/origin/
git commit -m "Compare origin responses across the four shareability axes

Self-identity is first because an origin that is not stable against itself
cannot be shared on any axis, and reporting that as a cookie failure would
send the operator after the wrong thing."
```

## Task A5: Implement the four response-header verdicts

**Files:** `crates/trusted-server-cli/src/commands/origin/`

| Verdict                   | Fails when                                                | Why it blocks                                                                                                                               |
| ------------------------- | --------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------- |
| Positive shared freshness | no positive `Cache-Control`/`Surrogate-Control` freshness | Readthrough would store on a platform default TTL where the template cache refuses (`NoPositiveFreshness`, `publisher.rs:6100`)             |
| No `Set-Cookie`           | the response carries one                                  | Cached and replayed to every later cookieless reader — cross-reader session fixation. Template cache refuses at `:6147`; readthrough cannot |
| No CSP `nonce`            | CSP contains `'nonce-`                                    | A shared nonce silently defeats the origin's own XSS defence. Template cache refuses at `:6117`                                             |
| `Vary` coverage           | a varying axis is not named in `Vary`                     | Readthrough keys on URL plus origin `Vary` only                                                                                             |

- [ ] **Step 1: Write the failing tests**

One per verdict, fixture-driven. The `Vary`-coverage test is the interesting one: a fixture that
varies on `User-Agent` **and** declares `Vary: User-Agent` must pass, while the same fixture
without the declaration must fail.

- [ ] **Step 2–4: Run, implement, run**

Same loop as A4.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-cli/src/commands/origin/
git commit -m "Report the four blocking response-header verdicts

These are the only control standing between the readthrough gate and
cross-serving: the gate is decided before the origin responds, so none of the
template cache's response-side refusals are reachable there."
```

## Task A6: Output, exit code, and stated limits

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn probe_exits_non_zero_when_any_verdict_fails() { /* fixture sets Set-Cookie */ }

#[test]
fn probe_json_output_names_every_axis_and_verdict() { /* --json shape */ }
```

- [ ] **Step 2: Implement**

Human-readable by default; `--json` for CI. **Non-zero exit on any blocking failure**, so it can
gate a deploy.

Print the limits every run, not only on failure:

- Runs from one client IP, so origin personalization keyed on the forwarded client address (geo,
  rate-class) is **undetectable** by this tool.
- The verdict covers the sampled URLs only, not the origin as a whole.

- [ ] **Step 3: Run, then commit**

```bash
git add crates/trusted-server-cli/src/commands/origin/
git commit -m "Gate on the probe verdict and state what the probe cannot see

A clean result on one URL from one IP is not a statement about the origin."
```

---

# Section B — Purge key plumbing

## Task B1: Add `request_path` to the cache key

**Files:** `crates/trusted-server-core/src/platform/template_cache.rs`, `publisher.rs:4370`

`TemplateCacheKey.url` is the **origin-rewritten** target URI (`publisher.rs:4372`, built at
`:4182`), not the URL an operator types. The struct has `url`, `request_host`, `request_scheme`
and **no path field**. Without one, a "reader-facing" key would have to be reconstructed from the
origin path, which is the coupling this section exists to remove.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn reader_facing_key_ignores_origin_rewriting() {
    let mut a = key();
    let mut b = key();
    a.url = "https://origin.internal.example/article".to_owned();
    b.url = "https://other-origin.example/article".to_owned();
    a.request_path = "/article".to_owned();
    b.request_path = "/article".to_owned();

    assert_eq!(
        a.reader_url_surrogate_key(),
        b.reader_url_surrogate_key(),
        "two origins behind one reader-facing URL must share the reader-facing purge key"
    );
    assert_ne!(
        a.url_surrogate_key(),
        b.url_surrogate_key(),
        "the origin-derived key must stay distinct; core uses it to evict one bad object"
    );
}
```

- [ ] **Step 2–4:** run (fails), add `pub request_path: String` populated from the **pre-rewrite**
      request at `publisher.rs:4370`, run again.

Note `to_cache_key()` must include `request_path` in its canonical input, since it changes the
emitted bytes. Bump `TEMPLATE_SCHEMA_VERSION` — the version table at `template_cache.rs:30-36`
documents why, and a missed bump reads yesterday's template against today's key shape.

- [ ] **Step 5: Commit**

## Task B2: Extract `url_surrogate_key` and define canonicalization

**Files:** `crates/trusted-server-core/src/platform/template_cache.rs`

`url_surrogate_key()` (`:147`) is a raw SHA-256 over exact bytes, and
`punctuation_distinct_urls_have_distinct_surrogate_keys` (`:961`) makes byte-exactness
load-bearing. Both the endpoint and the CLI must hash the same string as insert, or a purge
returns 200 and invalidates nothing — the worst failure mode on an incident path.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn reader_url_canonicalization_is_stable_across_operator_spellings() {
    for (a, b) in [
        ("https://example.com/article", "https://example.com/article/"),
        ("https://Example.COM/article", "https://example.com/article"),
        ("https://example.com:443/article", "https://example.com/article"),
        ("https://example.com/article?", "https://example.com/article"),
    ] {
        assert_eq!(
            reader_url_surrogate_key(a),
            reader_url_surrogate_key(b),
            "{a} and {b} name the same page and must purge together"
        );
    }

    assert_ne!(
        reader_url_surrogate_key("https://example.com/a"),
        reader_url_surrogate_key("https://example.com/b"),
    );
}
```

Decide and document the query-string rule explicitly: a query is **significant** (different query,
different page) but an empty `?` is not.

- [ ] **Step 2–4:** run, implement `pub fn reader_url_surrogate_key(url: &str) -> String` as a free
      function with `TemplateCacheKey::reader_url_surrogate_key()` delegating to it, run again.

Use a distinct prefix, `ts-template-readerurl-`, and assert in the existing surrogate-key test
that it never collides with `ts-template-url-`. Without the distinct prefix the two derivations
can alias when a staging edge host equals the configured origin host — over-purge rather than a
read leak, but a purge reporting success against an unrelated object.

- [ ] **Step 5:** add it to `surrogate_keys()` (`:138`) so it is attached at insert. The `Vec`
      already exists; an extra entry is free.

- [ ] **Step 6: Commit**

## Task B3: Add `purge_url_surrogate_key` to the platform trait

**Files:** `platform/template_cache.rs:675`, and all five implementors

`purge_url` takes `&TemplateCacheKey` (`:675`), which a handler holding only a URL cannot build —
it needs `origin_identity`, `template_fingerprint`, `vary_values` and `schema_version`.

Implementors: `UnavailableTemplateCache` (`template_cache.rs:698`),
`adapter-fastly/src/template_cache.rs:163`, `adapter-fastly/src/app.rs:2840`, and the two test
doubles at `publisher.rs:8843` and `:9112`. The doubles are spelled
`impl crate::platform::PlatformTemplateCache`, which a naive grep misses.

- [ ] **Step 1–4:** test on the Fastly impl and the null object, add
      `async fn purge_url_surrogate_key(&self, key: &str) -> Result<(), TemplateCacheError>`,
      implement across all five, run.

`UnavailableTemplateCache` must **not** silently succeed — a no-op purge reporting success is
worse than an error. Return the same unsupported signal the endpoint turns into a 501.

- [ ] **Step 5: Commit**

---

# Section C — Admin purge endpoint

## Task C1: Register the route on Fastly with its guards

**Files:** `adapter-fastly/src/app.rs:1128`, `core/src/settings.rs:3214`

```
POST /_ts/admin/cache/purge
Content-Type: application/json

{"scope": "all"}  |  {"scope": "url", "url": "https://example.com/page"}
```

Auth is inherited: `AuthMiddleware` (`app.rs:1305`) runs `enforce_basic_auth` (`core/src/auth.rs:79`)
before route matching, and fails closed when no handler regex covers the path (`auth.rs:93-100`).

- [ ] **Step 1: Write the failing tests**

- purge-all succeeds; purge-url succeeds
- unauthenticated request rejected
- **non-POST returns 405 and does not reach the origin**
- wrong `Content-Type` rejected
- oversized body rejected
- no legacy `/admin/cache/purge` alias resolves

- [ ] **Step 2–4: Implement with every guard**

- **Register the path as a string literal** in `NAMED_ROUTES`. The
  `admin_endpoints_match_fastly_router` check (`settings.rs:7727`) scans literal `path:` entries
  only; a named constant silently skips coverage.
- **Add the path to `Settings::ADMIN_ENDPOINTS`** (`settings.rs:3214`). Its doc comment says to.
  It feeds live config validation at `:3265`, so an operator `trusted-server.toml` whose handler
  regexes do not cover the new path will start failing validation — **call this out as a migration
  note in the PR description.**
- **Register for all methods and 405 in-handler.** Do not rely on `primary_methods`: non-primary
  methods on a named path fall through to the publisher (`app.rs:42`), and `enforce_basic_auth`
  leaves the `Authorization` header in place so it "still reaches the publisher origin"
  (`auth.rs:60-66`). A `GET` would authenticate, fall through, and ship the shared admin
  credential upstream. Follow the `/auction` `OPTIONS` precedent (`app.rs:1209-1213`).
- **Enforce `Content-Type: application/json` exactly.** Basic-auth credentials are attached
  automatically by browsers, so a cross-origin form POST with `enctype="text/plain"` sends no
  preflight; method alone does not stop CSRF.
- **Cap the request body.** The `url` field is attacker-supplied and only ever hashed.
- **Audit-log the authenticated principal for `{"scope":"all"}`.** It is an unbounded cache-flush
  and origin-stampede lever behind one shared static credential. There is no HTTP-handler
  rate-limit primitive here (`ec/rate_limiter.rs` is partner batch/pull-sync only), so either add
  a minimum interval or record the accepted risk explicitly in the PR.
- **Define the partial-failure answer.** `purge_all` returns a single `Result` from one
  `purge_surrogate_key` call (`adapter-fastly/src/template_cache.rs:285`); an operator mid-incident
  needs to know whether to retry. Say so in the response body.
- Response is `private, no-store`.
- Replay is not a concern — purge is idempotent. State that rather than leaving it unaddressed.

`{"scope":"url"}` hashes the **reader-facing** key from Task B2, so the handler needs no origin
rewriting.

- [ ] **Step 5: Commit**

## Task C2: Register 501 on the other three adapters

**Files:** `adapter-axum/src/app.rs:310`, `adapter-cloudflare/src/app.rs:515-545`, `adapter-spin/src/app.rs:211` and `:854`

An unregistered path falls through to origin and 404s, which reads to a CMS webhook as "endpoint
does not exist" rather than "not supported here".

- [ ] **Step 1–4:** test, register, run.

Axum's `named_routes()` returns `[NamedRoute; 16]` — a fixed-size array that must become **17**.
Spin needs both the route list (`:211`) and the router (`:854`). Cloudflare uses a builder chain
and already has `StatusCode::NOT_IMPLEMENTED` at `:281`.

- [ ] **Step 5: Commit**

## Task C3: Add authenticated POST helpers to the parity suite

**Files:** `crates/trusted-server-integration-tests/tests/parity.rs`

The suite configures `path = "^/_ts/admin"` with basic auth (`:16-17`), and the existing
`axum_post`/`cf_post`/`spin_post` helpers send **no credentials** — an unauthenticated probe gets
401 and never reaches the handler, so a naive 501 test would pass for the wrong reason.

- [ ] **Step 1–4:** add credential-carrying helpers (`spin_post_with_headers` at `:200` is a usable
      template), then assert 501 on all three adapters.

If the helper pulls a new dependency, this crate has its own lockfile with a shared-direct-dep
alignment gate — fix with a targeted `cargo update -p <crate> --precise <version>`, never a full
update.

- [ ] **Step 5: Commit**

---

# Section D — Purge CLI command

## Task D1: `ts cache purge`

**Files:** `crates/trusted-server-cli/src/run.rs`, `crates/trusted-server-cli/src/commands/cache/`

```
ts cache purge --all
ts cache purge --url <url>
```

With Task B2 landed this is a thin wrapper: hash the typed URL with the shared free function,
call the Fastly purge API. No config load, no origin logic.

- [ ] **Step 1: Write the failing test**

Assert the CLI computes the **same** key as core for the same logical page — the regression that
keeps the two from drifting:

```rust
#[test]
fn cli_and_core_agree_on_the_reader_facing_purge_key() {
    let url = "https://example.com/article";
    assert_eq!(cli_purge_key(url), trusted_server_core::platform::reader_url_surrogate_key(url));
}
```

- [ ] **Step 2–4:** run, implement, run.

**Token scope.** `adapter-fastly/src/management_api.rs:12` records that today's token is
write-scoped with **no purge permission**, and `edgezero_cli` exposes no credential helper — only
whole-command runners. Read `FASTLY_API_TOKEN` with a `--token` override, document the required
scope, and fail with an actionable message naming the missing scope.

**If the scope cannot be granted, drop this commit alone.** Nothing else depends on it; the admin
endpoint stands on its own.

- [ ] **Step 5: Commit**

## Task D2: Extend the local harness with a purge leg

**Files:** `scripts/template-cache-local-test.sh:17-18`, `.github/workflows/test.yml`

Viceroy 0.17 implements `purge_surrogate_key` against the same in-process cache it serves reads
from (verified in `docs/superpowers/plans/2026-08-08-1009-measurement-findings.md:156-158`), so
store → hit → purge → miss is end-to-end testable without a Fastly service. This is the strongest
test in the whole plan.

- [ ] **Step 1:** the script accepts only `inline|esi` today and CI invokes those two literals, so
      a new `purge` mode needs a **matching workflow step**. Add both.

- [ ] **Step 2:** add the mode to the AGENTS.md gate list entry created in part 1, Task 10.

- [ ] **Step 3: Commit**

---

## Final verification for part 2

- [ ] **Full gate set**

```bash
cargo fmt --all -- --check
cargo clippy-fastly && cargo clippy-axum && cargo clippy-cloudflare && cargo clippy-cloudflare-wasm && cargo clippy-spin-native && cargo clippy-spin-wasm
cargo clippy-cli && cargo clippy-codegen
cargo test-fastly && cargo test-axum && cargo test-cloudflare && cargo test-spin
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
./scripts/test-cli.sh
./scripts/template-cache-local-test.sh purge
cd docs && npm run format && cd ..
```

- [ ] **Confirm the probe actually blocks**

Run it against the fixture origin with a `Set-Cookie` response and confirm a non-zero exit. A
probe that reports a failure and exits 0 is worse than no probe, because part 3's safety argument
rests on it.

- [ ] **Record the `ts-origin` staging verdict**

Part 3 needs it and it cannot be tested under Viceroy. Timebox it here, while the purge trait is
already open: on a staging Fastly service, set a surrogate key on an origin fetch via
`Request::set_surrogate_key`, then purge it and confirm the object is gone. Write the result into
the PR description either way — part 3's rollback section depends on the answer.
