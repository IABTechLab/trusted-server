# EdgeZero PR 275 Hard Cutover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move Trusted Server onto EdgeZero PR 275 in PR #1506 (issue #1507),
with no compatibility layer. Every outbound request uses EdgeZero's
`HttpClient`, Fastly runs EdgeZero's canonical lifecycle, and every inbound
body goes through a bounded reader.

**Architecture:** The plan repins the six EdgeZero crates to PR 275's head and
fixes the nine API changes. It then adds a scripted `OutboundHttpClient` and
two `RuntimeServices` fields. The outbound sites are ported in four groups
while the old layer still compiles: integrations, proxies, auction, then
publisher. Fastly then moves onto `send_request_with_registries_and_hooks`.
Last, the old layer, its doubles, and every harness that depends on it are
deleted, and the docs are updated. Tasks follow the spec's commit sequence,
steps 1 to 11, with steps 2, 3, and 8 split.

**Tech Stack:** Rust 1.98.1 (2024 edition), EdgeZero PR 275 (`edgezero-core`,
the Fastly, Axum, Cloudflare, and Spin adapters, `edgezero-cli`), Fastly
Compute on `wasm32-wasip1` with Viceroy 0.21.0, Cloudflare Workers through
wrangler, Spin 4.1.0, Axum, `error-stack`, and Node 24.20.0 for the harness
scripts.

**Spec:**
[`docs/superpowers/specs/2026-10-17-edgezero-pr275-hard-cutover-design.md`](../specs/2026-10-17-edgezero-pr275-hard-cutover-design.md),
cited below as "Spec §N" and "D1" to "D20". Its requirements are in
[`docs/superpowers/specs/2026-10-17-edgezero-pr275-requirements.md`](../specs/2026-10-17-edgezero-pr275-requirements.md).
Read the spec section a task names before starting the task. The spec holds
the per-site detail, and this plan does not repeat it.

## How to read the citations

- Spec citations of the form `crates/...:<line>` are Trusted Server at
  `51730bdaf`. `main` has moved one commit since then, `4d55a3c6d` (#1405,
  SDK URL ownership). That commit touches the Lockr and Permutive integrations
  and adds `integrations/sdk_url_test_support.rs`, so line numbers in those
  two files can be off by a few lines. Search by function name.
- `edgezero:` citations are EdgeZero at H, `9b31a697`. The public API of the
  runtime crates at PR 275's head on 2026-11-08, `5ea8d93c`, matches H for
  every item this plan uses. Read EdgeZero from a scratch clone checked out at
  the pinned rev, never from the user's EdgeZero checkout.

## Working rules

- Worktree: `.claude/worktrees/spec-edgezero-pr275` (`$W`), branch
  `spec/edgezero-pr275-migration`, PR #1506. Source
  `$(git -C "$W" rev-parse --git-dir)/prfection-lib.sh` and run every
  command that writes files through `prf_run "$W" <cmd>`.
- Use the worktree's own `target/`. Never share a `CARGO_TARGET_DIR` with
  another worktree.
- A failing verification step stops the task. Report the command, its exit
  code, and the output. Never weaken a test or a CI step to get green.

## Global Constraints

- EdgeZero source: `git = "https://github.com/IABTechLab/edgezero"`,
  `rev = "<PR 275 head>"`, which was `5ea8d93c940bc021c552d29ddd86dd14bdadcd3e`
  when this plan was written. Features and `default-features` stay as they are
  (Spec §1, D5).
- `spin-sdk`: `version = "=7.0.0"` from
  `git = "https://github.com/stackpop/spin-rust-sdk.git"` at
  `rev = "6c59ed5b4fcb377072763aee0b27a3abdd8edb72"`, with the features
  EdgeZero's `Cargo.toml:106` uses (Spec §1).
- No shim, dual path, or feature flag remains after Task 9 (D1). The old
  `http_client` and the new `outbound_client` exist side by side only from
  Task 3B to Task 9.
- Every outbound request sets `.connect_timeout(Duration::from_secs(1))` and a
  constant `.timeout(..)`: the site's configured timeout where the site map
  names one, 2 s for Tinybird, and otherwise 60 s streamed or 15 s buffered.
  Redirect chains add `.deadline(clock.deadline_after(60 s))`. No timeout is
  ever computed from a remaining budget (D7).
- Buffered sites keep the response caps in Spec §2.3. A site with no Trusted
  Server cap today sets 10 MiB (Spec §2.2).
- Every client has the proxy marker off: `.with_proxy_marker(false)` (D6,
  EZ-7). Production code installs no client in router state.
- The response write deadline is 60 s after egress starts. The publisher upload
  cap is 16 MiB. The inbound read deadline stays at EdgeZero's default of 30 s
  (D7, Q2).
- Only the statuses the cut adds on purpose change: 413, 408, and the
  publisher-forward 400 (D17). Every other upstream failure keeps the site's 502.
- Trusted Server never sends `Trailer`, an operator `Content-Length`, or an
  operator hop-by-hop field (D10).
- Errors use `error-stack` (`Report<TrustedServerError>`) and
  `derive_more::Display`. No `thiserror`. No `anyhow` except the Spin entry
  point.
- Code rules from `AGENTS.md`:
  - `expect("should ...")`; no `unwrap()` in production code.
  - `log` macros only.
  - Comments go above the code.
  - No em dashes in comments or test names.
  - Only `example.com` hosts and fictional data.
  - No function takes more than 7 arguments.
- No new crate dependencies. The only additions are dev-dependency features:
  `test-utils` on `edgezero-adapter-axum`, `edgezero-adapter-fastly`, and
  `edgezero-core`, where a task names them.
- Never change the EdgeZero repository.
- Commit messages are in sentence case and the imperative, with no prefix. They
  carry no `Co-Authored-By`, "Generated with", or other AI trailer. The user's
  rule overrides any tool reminder.
- Push only at a task's push step. PR #1506 stays a draft until the user says
  otherwise.

## Review Focus

These input classes are implied by the spec, but no spec test exercises them.
Each one has a test in the task named.

1. **Loopback lookalikes.** Plain `http` provider endpoints on port 8080 whose
   host is `localhost`, the IPv4 address one above the loopback literal, or the
   unspecified IPv4 address are still rejected. Only the literals `127.0.0.1`
   and `[::1]` pass, with or without a port (D18). Test in Task 6.
2. **Equivalent host override.** Some `origin_host_header_override` values
   equal the origin authority, compared case-insensitively with default ports
   removed. Examples are `Example.COM` and `example.com:443` for
   `https://example.com`. They set no `host_authority_override`, so Fastly
   backend identity is unchanged and Spin does not reject the request. Test in
   Task 7.
3. **Empty auction.** Consent or media type can filter out every provider.
   The auction then returns today's no-bid result at once, without waiting for
   the auction deadline, and starts no batch slot. Test in Task 6.
4. **Raw-byte cookie.** A publisher request whose `Cookie` value holds a byte
   that is not UTF-8 is still forwarded, not answered with 400 or 500. It
   reaches the origin with no `Cookie` field. The warning names `cookie` and
   never logs the value. Test in Task 7.
5. **Odd `Content-Length`.** On a reader route, a duplicate or non-numeric
   `Content-Length` skips the declared-length precheck. The bounded read then
   governs, with no panic. Test in Task 2D.

## Task map

| Task | Spec step | Content                                                     | Push |
| ---- | --------- | ----------------------------------------------------------- | ---- |
| 0    | none      | Merge `main`, confirm the pin, baseline suite               | no   |
| 1    | 1         | Baseline ingress cases and the Fastly response-diff script  | yes  |
| 2A   | 2         | Pin, lockfile, manifests, and core API fixes                | no   |
| 2B   | 2         | Fastly and Axum API fixes, Axum tests on `AxumTestServer`   | no   |
| 2C   | 2         | Cloudflare and Spin API fixes, `configure` policies         | no   |
| 2D   | 2         | `read_request_body` and every inbound reader                | yes  |
| 3A   | 3         | Scripted client and test clock                              | no   |
| 3B   | 3         | `RuntimeServices` fields, request helpers, `outbound_error` | no   |
| 3C   | 3         | `oneshot` client helper, parity suite on `AxumTestServer`   | yes  |
| 4    | 4         | Integrations O10 to O15, `certificate_check` rule           | no   |
| 5    | 5         | Proxies O6 to O9: redirects, asset proxy, Image Optimizer   | no   |
| 6    | 6         | Auction O1 to O3, loopback endpoints, fan-out everywhere    | yes  |
| 7    | 7         | Publisher origin O4 and O5, scope owner, input caps, O19    | yes  |
| 8A   | 8         | Async store writes and the management API                   | no   |
| 8B   | 8         | Fastly canonical lifecycle and post-delivery work           | yes  |
| 9    | 9         | Delete the old layer, port the deadline harnesses, guard    | yes  |
| 10   | 10        | Egress validation, Axum ingress variables, observation log  | no   |
| 11   | 11        | Documentation and `CHANGELOG.md`                            | yes  |
| 12   | none      | Full verification and the PR handoff                        | yes  |

After each spec step, only the tests that the spec lists as red may fail. After
Task 2D those are the `cargo test` job's template-cache harnesses,
`test-fastly-eof`, the Viceroy browser and trace jobs, the Fastly EC lifecycle
job, and the publisher-forward integration cases. They turn green in Task 7
(Spec "Commit sequence" step 2). The Task 2A to 2C commits compile only the
crates they name. Task 2D restores every target. PR #1506 is squash-merged, so
these intermediate commits never reach `main` on their own.

---

### Task 0: Prepare the branch

**Files:** none. The task merges `main` into the branch.

**Interfaces:** Produces the pinned rev `$EZ_REV` that Task 2A writes, and the
baseline results that Task 12 compares against.

- [ ] **Step 1: Merge `main`.** Run
      `prf_run "$W" git fetch origin && prf_run "$W" git merge --no-edit origin/main`.
      Expected: a clean merge that brings `4d55a3c6d` in, with no conflict,
      since the branch changes only documentation.
- [ ] **Step 2: Confirm the PR 275 head.** Run
      `gh pr view 275 --repo IABTechLab/edgezero --json headRefOid --jq .headRefOid`.
      Set `EZ_REV` to the value. If it is not `5ea8d93c…`, diff the public API
      of the runtime crates between `5ea8d93c` and `EZ_REV` in the scratch
      clone. Use
      `git diff 5ea8d93c "$EZ_REV" -- crates/edgezero-core/src crates/edgezero-adapter-*/src | grep -E '^[+-]\s*pub (async )?(fn|struct|enum|trait|type|const|use)'`.
      Then record every changed item the spec cites in the PR body's
      verification section. Expected: the diff is empty, or every changed item
      is listed.
- [ ] **Step 3: Baseline suite.** Run every CI gate in `AGENTS.md` ("CI
      Gates", 1 to 13) at the merged head and save each command's exit code
      and summary line to `$SCRATCH/baseline.md`. Expected: every gate exits 0.
      Report any failure to the user before Task 1, together with a
      reproduction at `origin/main`.
- [ ] **Step 4: Commit.** The merge commit is the commit. Do not push yet.

### Task 1: Baseline ingress cases and the response-diff script (Spec "Commit sequence" step 1)

**Files:**

- Modify: `crates/trusted-server-integration-tests/tests/integration.rs`
- Create: `scripts/fastly-response-diff.sh`,
  `scripts/fastly-response-diff.mjs`,
  `scripts/fastly-response-diff.requests.json`
- Modify: `scripts/README.md` (one entry for the script)

**Interfaces:**

- Produces the ignored runtime tests below, which Tasks 2D and 7 extend.
- Produces the diff script, which Task 12 runs with
  `scripts/fastly-response-diff.sh --base <main sha> --head HEAD`.

- [ ] **Step 1: Write the ingress cases.** Follow the pattern of
      `test_cloudflare_enabled_auction_reaches_telemetry`: `#[ignore]` with the
      runtime requirement, then `RuntimeEnvironment` and the origin helpers in
      `common/runtime`. Add these tests:
  - `post_auction_with_json_body_reaches_handler_fastly` and `_axum`. A
    `POST /auction` with a valid JSON body gets 200 and an auction response,
    not the empty-body parse error.
  - `publisher_post_body_reaches_origin_byte_for_byte_fastly`, `_cloudflare`,
    and `_axum`. The origin stub records a body equal to the 4096 sent bytes,
    including byte values 0 to 255.
  - `head_root_returns_origin_status_{fastly,cloudflare,axum}` and
    `options_root_returns_origin_status_{fastly,cloudflare,axum}`. The status
    equals the origin stub's.
  - `chunked_post_auction_over_cap_returns_413_{fastly,cloudflare,axum}`. The
    body is 256 KiB plus 1 byte, chunked, with no `Content-Length`. Expected:
    status 413 with a `text/plain` body.
- [ ] **Step 2: Write the response-diff script.**
  - `fastly-response-diff.sh --base <rev> --head <rev>` builds both revs in two
    temporary detached worktrees under `$TMPDIR` and serves each under Viceroy
    on its own port with the integration fixture config. It runs the `.mjs`
    and removes the worktrees on exit, through a `trap`.
  - `fastly-response-diff.mjs base_url head_url requests.json expected.json`
    sends each request to both servers. It normalizes `x-ts-version`,
    `x-ts-sandbox-*`, `date`, `age`, and generated IDs, and with `--staging`
    also geo, `x-ts-fastly-version`, and `x-ts-env`. It compares the status,
    the sorted headers, and the SHA-256 of the body. It exits 1 when a
    difference is not listed in `expected.json`, and also when an expected
    difference does not occur.
  - `requests.json` holds the Spec "Runtime" list:
    - a publisher page
    - `HEAD /` and a conditional `GET /` that gets 304
    - `POST /auction`
    - a first-party proxy URL
    - an asset
    - a tsjs bundle
    - an admin endpoint
    - `TRACE`
    - a trace action
  - Each request carries an `id`. `expected.json` maps an `id` to the Fastly
    differences items it exercises, and starts empty.
- [ ] **Step 3: Run them against `v0.0.11`.**
  1. Build what the cases need: `cargo build-fastly --release` and the Axum
     binary, plus `crates/trusted-server-adapter-cloudflare/build.sh`.
  2. Run
     `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test integration -- --include-ignored post_auction_with_json publisher_post_body head_root options_root chunked_post_auction`.
     Expected: every case passes.
  3. Run `scripts/fastly-response-diff.sh --base HEAD --head HEAD`. Expected:
     exit 0 and no differences.
- [ ] **Step 4: Commit and push.** Commit with the message "Add baseline ingress
      cases and a Fastly response diff for the EdgeZero cut", then run
      `prf_run "$W" git push`.

### Task 2A: Pin, lockfile, manifests, and core API fixes (Spec §1, §6, §7)

**Files:**

- Modify: `Cargo.toml` (the six EdgeZero entries and `spin-sdk`), `Cargo.lock`,
  `edgezero.toml` (`[capabilities.outbound] hosts = ["*"]`)
- Modify: `crates/trusted-server-adapter-cloudflare/wrangler.toml:12`,
  `wrangler.ci.toml:6` (add the `enable_weak_ref` and `enable_request_signal`
  compatibility flags)
- Modify, in core: `src/integrations/mod.rs`, `integrations/google_tag_manager.rs`,
  `proxy.rs`, `publisher.rs`, `trace/actions.rs`, `trace/dispatch.rs`,
  `trace/routes.rs`, `forwarder.rs`, `auth.rs`, `http_util.rs`, and the core
  test files in `$S/inventory.tsv`. `$S` is the session scratch `ez275v2/`
  directory, or regenerate the inventory with the probe tools.

**Interfaces:**

- Produces `TracePreDispatchHook::handle(&self, request: &mut PreDispatchRequest)`.
- Produces `prepare_trusted_forwarder(parts: &mut RequestParts, ..)`,
  `action_response(request: &PreDispatchRequest, ..)`,
  `matches_trusted_origin(parts: &RequestParts, ..)`, and
  `MetadataSupplier::supply(&self, parts: &RequestParts, ..)`. The rest of each
  signature is unchanged.
- Produces `enforce_basic_auth(parts: &mut RequestParts, ..)` and
  `sanitize_trusted_client_ip_headers(parts: &mut RequestParts)`.

- [ ] **Step 1: Repin.** Set the six EdgeZero entries to `rev = "$EZ_REV"` and
      `spin-sdk` to the Global Constraints spec. Run
      `cargo update -p edgezero-core`, then
      `cargo update -p 'registry+https://github.com/rust-lang/crates.io-index#spin-sdk@7.0.0'`.
      Restore the `colored` 3.1.1 and `winapi-util` 0.1.11 edges to
      `windows-sys 0.61.2` by hand. Expected: running `cargo metadata --locked`
      exits 0, and the lock moves only the entries in the Spec §1 table and the
      git `spin-sdk` stack.
- [ ] **Step 2: Add the manifest and wrangler changes.** Add the
      `edgezero.toml` and wrangler entries from Spec §1, then run
      `cargo check-fastly 2>&1 | tail -3`. Expected: the 24 core lib errors of
      Spec "Problem" (`Body` variants, `from_stream`, `PreDispatchHook`).
- [ ] **Step 3: Fix the core lib.**
  - `Body` matches move to `into_parts()`, `into_stream()`, `as_bytes()`, and
    `is_stream()` (Spec §7).
  - `BodyChunkSource::next_chunk` converts once to a `BodyStream` or `Bytes`
    and holds that.
  - Publisher body streams use `Body::from_stream` with `EdgeError` items that
    pass the origin error through. Foreign `io::Error` streams use
    `Body::from_external_stream`.
  - Retype the hook and its helpers (Spec §6). The trace action body read
    becomes the `Content-Length` check followed by
    `request.body_bytes(0).await`, mapped to the trace JSON errors.
  - Run `cargo check -p trusted-server-core --target wasm32-wasip1`. Expected:
    exit 0.
- [ ] **Step 4: Fix the core tests.**
  - The 17 hook test calls drive the hook through
    `RouterService::builder().pre_dispatch_hook(..).build().oneshot(request)`.
  - A test that checks a continue outcome adds a route whose handler records
    the request.
  - Run
    `cargo check -p trusted-server-core --all-targets --all-features --target wasm32-wasip1`.
    Expected: exit 0.
- [ ] **Step 5: Commit.** Commit with "Repin EdgeZero to PR 275 and port core
      to its body and hook APIs". Adapters do not compile yet.

### Task 2B: Fastly and Axum API fixes, Axum tests on `AxumTestServer` (Spec §7, §8)

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/{app.rs,middleware.rs,compat.rs,main.rs,platform.rs}`
- Modify: `crates/trusted-server-adapter-axum/{Cargo.toml,src/main.rs,src/app.rs,src/middleware.rs,src/platform.rs,tests/routes.rs}`
- Modify: `crates/trusted-server-cli/tests/proxy_trusted_server.rs`,
  `crates/trusted-server-cli/Cargo.toml` (adds the dev-dependency feature)

**Interfaces:**

- Consumes the Task 2A signatures.
- Produces `TrustedServerApp::routes() -> Result<RouterService, EdgeError>` on
  each adapter.
- Produces `fn startup_error(report: &Report<TrustedServerError>) -> EdgeError`
  in `crates/trusted-server-core/src/error.rs`. It maps the root status to
  `EdgeError::internal`, `service_unavailable`, `bad_gateway`, or
  `bad_request` (Spec §7, "Startup failures").

- [ ] **Step 1: Write the failing Axum startup test.** Add
      `axum_main_exits_non_zero_when_settings_fail` to
      `crates/trusted-server-adapter-axum/tests/routes.rs`. It runs the binary
      with an invalid `TRUSTED_SERVER__` setting and asserts a non-zero exit
      and the logged error line. Run `cargo test-axum` and expect a failure,
      because the binary keeps serving 500s.
- [ ] **Step 2: Fix the API sites.**
  - `into_request()?` at the 15 roots.
  - `parts()`, `headers()`, and `extensions()` in place of
    `request()`/`request_mut()`.
  - `routes()` returns `Err(startup_error(&report))` after
    `log::error!("{report:?}")`. Delete the Axum `startup_error_router`, but
    keep Fastly's until Task 8B.
  - Fastly `run_custom` and `serve_custom` callbacks return `Ok(())` on every
    path. That stays a stopgap until Task 8B.
- [ ] **Step 3: Assemble the Axum dev server.** In Axum `main`, call
      `App::build::<TrustedServerApp>(AXUM_PLATFORM)`. Pass it to
      `AxumDevServer::with_config(app, config)`. On error, log it and
      `std::process::exit(1)`.
- [ ] **Step 4: Move the Axum route tests and the CLI proxy test to
      `AxumTestServer`.** Build each app with
      `App::from_router::<TrustedServerApp>(routes_with_settings(..), AXUM_PLATFORM)`,
      listen on `127.0.0.1:0`, and attach the stores the test needs. Add
      `features = ["test-utils"]` on `edgezero-adapter-axum` in both crates'
      dev-dependencies.
- [ ] **Step 5: Verify.** Run `cargo check-fastly`,
      `cargo check-fastly --all-targets --all-features`, `cargo test-axum`, and
      `./scripts/test-cli.sh`. Expected: all exit 0, and the new startup test
      passes.
- [ ] **Step 6: Commit.** Commit with "Port the Fastly and Axum adapters to
      EdgeZero PR 275 APIs".

### Task 2C: Cloudflare and Spin API fixes and `configure` policies (Spec §5, §7, §8)

**Files:**

- Modify: `crates/trusted-server-adapter-cloudflare/src/{app.rs,middleware.rs,platform.rs}`,
  `tests/routes.rs`
- Modify: `crates/trusted-server-adapter-spin/src/{lib.rs,app.rs,middleware.rs,platform.rs}`,
  `tests/routes.rs`
- Modify: the `Hooks::configure` implementation in each of the four adapters'
  `app.rs`
- Create: `crates/trusted-server-core/src/edge_app.rs` with
  `pub const RESPONSE_WRITE_DEADLINE: Duration = Duration::from_secs(60);`,
  declared in `crates/trusted-server-core/src/lib.rs`

**Interfaces:**

- Produces
  `pub fn install_edge_policies(app: &mut App) -> Result<(), EdgeError>` in
  `crates/trusted-server-core/src/edge_app.rs`. Every adapter's `configure`
  calls it.
  - It calls `app.set_response_egress_policy(..)`, whose deadline is egress
    start plus `RESPONSE_WRITE_DEADLINE`.
  - It also calls
    `app.set_synthesized_response_headers(SynthesizedResponseHeaders::new(..)?)`
    with `x-ts-version`.
  - Fastly's `configure` adds `x-ts-fastly-version` when it is known.

- [ ] **Step 1: Write the failing policy test.** Add
      `installed_egress_policy_sets_write_deadline_sixty_seconds_after_egress_start`
      to `edge_app.rs`. It builds an `App`, calls `install_edge_policies`, and
      asserts the policy's deadline equals the egress start plus 60 s (test 17,
      unit part). Run `cargo test-fastly -- edge_app::tests`. Expected: a
      compile failure, because the function is not defined yet.
- [ ] **Step 2: Implement `install_edge_policies`** and call it from all four
      `configure` implementations.
- [ ] **Step 3: Fix the Cloudflare and Spin sites.**
  - `ConfigStore::get` converts the `ConfigValue` to `String` at the boundary.
    The Spin test store implements `get_bounded`.
  - `SpinConfigStore::for_label` replaces `open`.
  - The Spin entry point returns
    `anyhow::Result<edgezero_adapter_spin::SpinResponse>`.
  - Delete the Cloudflare and Spin `startup_error_router`s and the Spin
    `/health` startup test at `app.rs:1531`. `routes()` returns `Err` as in
    Task 2B, and Spin always uses `service_unavailable`.
- [ ] **Step 4: Add the synthesized-header tests.** Add
      `canonical_not_found_carries_x_ts_version` and
      `canonical_method_not_allowed_carries_allow_and_x_ts_version` to each
      adapter's route tests. They assert a 404 for an unknown path, and a 405
      with a sorted `Allow` for a wrong method on a known path. Both carry
      `x-ts-version`.
- [ ] **Step 5: Verify.** Run `cargo check-cloudflare`, `cargo check-spin`,
      `cargo test-cloudflare`, `cargo test-spin`, `cargo test-axum`, and the
      core test above. Expected: all exit 0.
- [ ] **Step 6: Commit.** Commit with "Port the Cloudflare and Spin adapters to
      EdgeZero PR 275 and install egress policies".

### Task 2D: `read_request_body` and every inbound reader (Spec §4)

**Files:**

- Modify: `crates/trusted-server-core/src/http_util.rs`, `error.rs`
- Modify the readers: `auction/endpoints.rs`, `ec/batch_sync.rs`,
  `cache_purge.rs`, `proxy.rs` (sign and rebuild),
  `request_signing/endpoints.rs`, `integrations/mod.rs`, the six integration
  proxies, and `integrations/google_tag_manager.rs` (beacon)
- Modify: `crates/trusted-server-integration-tests/tests/integration.rs`

**Interfaces:**

- Produces
  `pub async fn read_request_body(headers: &HeaderMap, body: Body, max: usize) -> Result<Bytes, Report<TrustedServerError>>`
  in `http_util.rs`.
- Produces `TrustedServerError::RequestTimeout { message: String }`, with
  status 408.
- Deletes `request_body_bytes` and `collect_body_bounded`.

- [ ] **Step 1: Write the failing helper tests** in `http_util.rs`:
  - `declared_length_over_max_returns_413_without_polling`. The body is a
    stream that panics when polled.
  - `stream_crossing_max_returns_413`.
  - `read_after_deadline_returns_408`, which uses `Body::from_stream` with an
    item of `EdgeError::request_timeout("expired")`.
  - `client_caused_stream_error_returns_400`, with `EdgeError::bad_request`.
  - `host_read_error_returns_502`, with `EdgeError::bad_gateway`.
  - `duplicate_content_length_falls_through_to_bounded_read` and
    `non_numeric_content_length_falls_through_to_bounded_read` (Review Focus 5).
    Both bodies are under `max` and return their bytes.
  - `body_under_max_returns_exact_bytes`.

  Each test asserts the `TrustedServerError` variant and its `status_code()`.
  Run `cargo test-fastly -- http_util::tests` and expect a compile failure,
  because the helper is not defined yet.

- [ ] **Step 2: Implement it.** First, a single parsable `Content-Length`
      over `max` returns `RequestTooLarge` without touching the body.
      Otherwise await `body.into_bytes_bounded(max)` and map the `EdgeError`
      status:
  - 413 becomes `RequestTooLarge`.
  - 408 becomes `RequestTimeout`.
  - Another 4xx becomes `BadRequest`.
  - A 5xx becomes `Proxy`, logged at error.
  - 408 and 413 are logged at warn.

  Body bytes are never logged.

- [ ] **Step 3: Move every reader to the helper.** Use the cap in the Spec §4
      table for each reader. Keep the `text/plain` 413 body on `/auction` and
      the `body_too_large` error body on batch sync.
- [ ] **Step 4: Write the reader tests (test 8).** For each reader, add a
      declared-over-cap 413, a streamed-over-cap 413, a 408, a 400, and a 502
      through its route. Add
      `batch_sync_stream_past_declared_length_and_cap_returns_413`. In
      `integration.rs`, add `get_root_with_body_returns_400_{fastly,cloudflare,axum}`
      (EZ-1 criterion 13).
- [ ] **Step 5: Verify.** Run `cargo test-fastly`, `cargo test-axum`,
      `cargo test-cloudflare`, `cargo test-spin`, and all eight clippy
      aliases. Run the integration cases from Task 1 on Cloudflare and Axum,
      plus the new GET case. Expected: all exit 0. The Fastly
      publisher-forward cases are allowed red until Task 7. Also run
      `./scripts/async-html-finalization-local-test.sh fastly --release` and
      record whether Fastly hands a GET a body handle (Spec step 2's open
      check).
- [ ] **Step 6: Commit and push.** Commit with "Read every inbound body through
      a bounded EdgeZero reader", then push.

### Task 3A: Scripted client and test clock (D12, Spec test 3)

**Files:**

- Create: `crates/trusted-server-core/src/test_support/scripted_client.rs`
- Modify: `crates/trusted-server-core/src/test_support.rs`
  (`#[cfg(any(test, feature = "test-utils"))] pub mod scripted_client;`)
- Modify: `crates/trusted-server-core/Cargo.toml`, enabling
  `edgezero-core/test-utils` from the core `test-utils` feature if the build
  needs it

**Interfaces:**

- `pub struct TestClock`:
  - `new() -> Self`
  - `monotonic_clock(&self) -> MonotonicClock`
  - `outbound_time(&self) -> OutboundTime`. Its wait factory resolves when
    `advance` moves the clock past the deadline.
  - `advance(&self, by: Duration)`
- `pub struct ScriptedHttpClient` (`Clone`, `Send + Sync`):
  - `new(clock: &TestClock) -> Self`
  - `forbidding() -> Self`. It panics with `"should not send <METHOD> <uri>"`
    on any request.
  - `respond_to(&self, method: Method, uri: &str, reply: ScriptedReply) -> &Self`.
    Replies queue first in, first out per method and URI. An unmatched request
    fails with `EdgeError::internal("unscripted request")` and is still
    recorded.
  - `http_client(&self) -> HttpClient`, which is
    `HttpClient::with_client(self.clone()).with_proxy_marker(false)`.
  - `requests(&self) -> Vec<RecordedRequest>`
- `pub struct ScriptedReply`:
  - `status(StatusCode) -> Self`
  - `header(self, name: &str, value: &str) -> Self`
  - `body(self, bytes: impl Into<Bytes>) -> Self`
  - `chunks(self, chunks: Vec<Bytes>) -> Self`
  - `after(self, delay: Duration) -> Self`, the arrival time on the test clock
  - `held(self) -> (Self, ReplyRelease)`
  - `error(EdgeError) -> Self`
- `pub struct ReplyRelease` with `release(self)`.
- `pub struct RecordedRequest`. Its public fields are copied from
  `OutboundDispatchRequest::into_parts()` (`edgezero:crates/edgezero-core/src/outbound.rs:330-362`):
  - `method`, `uri`, `headers`, and `body: Bytes`
  - `timeout`, `connect_timeout`, `deadline`, and `cache_policy`
  - `response_decoding` and `response_mode`
  - `max_decoded_response_bytes` and `max_encoded_response_bytes`
  - `image_optimizer` and `proxy_marker`
  - `host_authority: String` and `has_host_authority_override: bool`, read
    from the `OutboundRequest` before `into_parts`.

Use the EdgeZero Axum adapter as the model for the provider half:
`schedule_batch` and `schedule_response`
(`edgezero:crates/edgezero-adapter-axum/src/outbound.rs:220-320`) and its test
`ResponseClient` (`edgezero:crates/edgezero-adapter-axum/src/outbound/testing.rs`).
The progress runtime is `IndependentOutboundProgress`.

- [ ] **Step 1: Write the failing client tests** in `scripted_client.rs`:
  - `records_method_uri_headers_and_options`
  - `replies_in_fifo_order_per_method_and_uri`
  - `chunked_reply_streams_each_chunk`
  - `held_reply_resolves_only_after_release`
  - `batch_drops_reply_arriving_after_cutoff`. Two slots answer at 100 ms and
    600 ms, and the cutoff is 500 ms. The first is resolved and the second is
    `Unresolved`.
  - `error_reply_fails_only_its_slot`
  - `forbidding_client_panics_on_any_request`, with
    `#[should_panic(expected = "should not send")]`

  Run `cargo test-fastly -- scripted_client` and expect a compile failure.

- [ ] **Step 2: Implement the client and the clock.**
- [ ] **Step 3: Verify.** Run the tests from Step 1. Expected: pass. Then run
      `cargo clippy-fastly`. Expected: exit 0.
- [ ] **Step 4: Commit.** Commit with "Add a scripted EdgeZero outbound client
      for tests".

### Task 3B: `RuntimeServices` fields, request helpers, and `outbound_error` (Spec §2.1, §2.2, §2.11)

**Files:**

- Modify: `crates/trusted-server-core/src/platform/types.rs`,
  `platform/mod.rs` (re-export `HttpClient` and `MonotonicClock`)
- Create: `crates/trusted-server-core/src/outbound.rs`, declared in `lib.rs`
- Modify: every `RuntimeServices::builder()` caller (41 calls in 16 files, found
  with `git grep -n 'RuntimeServices::builder()'`)
- Modify: the four adapters' per-request service builders

**Interfaces:**

- `RuntimeServices` gains:
  - `outbound_client(&self) -> &HttpClient` and
    `clock(&self) -> &MonotonicClock`
  - the builder methods `.outbound_client(HttpClient)` and
    `.clock(MonotonicClock)`, both required by `build()`, as the other
    required services are.
- `outbound.rs` holds these constants:
  - `CONNECT_TIMEOUT` (1 s)
  - `STREAMED_TIMEOUT` (60 s)
  - `BUFFERED_TIMEOUT` (15 s)
  - `REDIRECT_CHAIN_DEADLINE` (60 s)
  - `DEFAULT_BUFFERED_RESPONSE_BYTES: u64` (10 MiB)
  - `PUBLISHER_UPLOAD_BYTES: u64` (16 MiB)
- It also holds these functions:
  - `pub fn buffered(request: OutboundRequest, timeout: Duration, max_response_bytes: u64, decoding: ResponseDecoding) -> OutboundRequest`.
    It sets the connect timeout, the timeout, the cap, and the decoding.
  - `pub fn streamed(request: OutboundRequest, timeout: Duration) -> OutboundRequest`.
    It sets the connect timeout, the timeout, `.stream_response()`, and
    `ResponseDecoding::Original`.
  - `pub enum OutboundRequestSource { ClientForward, Internal }`
  - `pub fn outbound_error(error: EdgeError, context: TrustedServerError, source: OutboundRequestSource) -> Report<TrustedServerError>`.
    The spec's two-argument signature cannot tell the publisher-forward 400
    from a request Trusted Server built itself, so the source argument is
    added.

- [ ] **Step 1: Write the failing tests (test 18).**
  - `outbound_error_maps_payload_too_large_to_413`
  - `outbound_error_maps_request_timeout_to_408`
  - `outbound_error_maps_bad_request_to_400_for_client_forward`
  - `outbound_error_maps_bad_request_to_site_502_for_internal`
  - `outbound_error_keeps_site_502_for_gateway_timeout_bad_gateway_response_too_large_and_internal`
  - `outbound_error_attaches_edge_error_detail`
  - `buffered_sets_one_second_connect_timeout_and_constant_timeout`
  - `streamed_sets_original_decoding_and_stream_response`

  Each one asserts the status and the attached `EdgeError` display. Run
  `cargo test-fastly -- outbound::tests` and expect a compile failure.

- [ ] **Step 2: Implement the helpers and the fields.**
  - `outbound_error` attaches the `EdgeError`, its budget source, and
    `provider_failure_detail()`. It logs the detail with `{:?}` at warn, never
    provider text or URLs (Spec §9).
  - The adapters set `.outbound_client(ctx.http_client().ok_or_else(..)?.with_proxy_marker(false))`
    and `.clock(ctx.monotonic_clock())`.
  - A missing client is `EdgeError::internal("request has no outbound client")`,
    not a panic.
  - Test fixtures use `ScriptedHttpClient`.
  - `ForbiddenLifecycle` in `parity.rs` uses `ScriptedHttpClient::forbidding()`.
- [ ] **Step 3: Verify.** Run `cargo test-fastly`, `cargo test-axum`,
      `cargo test-cloudflare`, `cargo test-spin`, and the eight clippy
      aliases. Expected: exit 0.
- [ ] **Step 4: Commit.** Commit with "Carry EdgeZero's outbound client and
      clock in RuntimeServices".

### Task 3C: `oneshot` client helper and the parity suite on `AxumTestServer` (Spec §2.1, §8)

**Files:**

- Modify: `crates/trusted-server-core/src/test_support.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs` (the `route` test
  helper)
- Modify: `crates/trusted-server-adapter-cloudflare/tests/routes.rs`,
  `crates/trusted-server-adapter-spin/tests/routes.rs`
- Modify: `crates/trusted-server-integration-tests/tests/parity.rs`,
  `crates/trusted-server-integration-tests/Cargo.toml` (dev-dependency
  features)

**Interfaces:**

- Produces
  `pub fn insert_outbound_client(request: &mut Request, client: HttpClient)` in
  `test_support.rs`. It inserts into the request extensions, where
  `ctx.http_client()` reads.

- [ ] **Step 1: Write the failing parity test (test 19).** Add
      `axum_test_server_parity_rejects_9000_byte_target_with_414` for Axum,
      Cloudflare, and Spin. Add
      `bodiless_get_with_unread_stream_reaches_origin_without_body`. That test
      supplies EdgeZero's Axum client, `AxumOutboundClient`, against a
      loopback origin that records the request body. It runs with
      `HTTP_PROXY`, `HTTPS_PROXY`, and `ALL_PROXY` unset, and asserts an empty
      recorded body.
- [ ] **Step 2: Implement the helper.** Use it in the Fastly, Cloudflare, and
      Spin `route` helpers. Move the `cf_*` and `spin_*` parity helpers and
      `discovery_route_body_is_json_parity` to `AxumTestServer`. Assemble each
      app under its own `PlatformMetadata` and put a `ScriptedHttpClient` in
      router state.
- [ ] **Step 3: Verify.** Run
      `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity`,
      `cargo test-fastly`, `cargo test-cloudflare`, and `cargo test-spin`.
      Expected: exit 0.
- [ ] **Step 4: Commit and push.** Commit with "Run the parity suite on
      EdgeZero's Axum test server", then push.

### Task 4: Integrations O10 to O15 and the `certificate_check` rule (Spec §1, §2.3, D9)

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/{sourcepoint.rs,lockr.rs,permutive.rs,datadome.rs,datadome/protection.rs,didomi.rs}`
- Modify: `crates/trusted-server-core/src/config.rs`
  (`validate_settings_for_deploy`), `settings.rs:1916` (doc comment)
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs:241` (replace
  `warn_if_certificate_check_disabled`)
- Modify: the three integration fixture configs, `scripts/integration-tests.sh:76`,
  `scripts/integration-tests-browser.sh:39`, `.github/workflows/test.yml:282`,
  and `.github/actions/setup-integration-test-env/action.yml:94`, `:104`, and
  `:135`

**Interfaces:**

- Consumes `outbound::{buffered, streamed, outbound_error}` and
  `services.outbound_client()`.
- Produces
  `fn reject_disabled_certificate_check(settings: &Settings) -> Result<(), Report<TrustedServerError>>`,
  called from `validate_settings_for_deploy`.

- [ ] **Step 1: Write the failing tests.**
  - Readback tests (test 3) for each of the eight sites, with
    `ScriptedHttpClient`. Each asserts the method, URI, and headers, the
    1 s connect timeout, the timeout from Spec §2.3, the cap, the decoding,
    the cache policy, and `proxy_marker == OutboundProxyMarker::Inherit`
    under a client with the marker off. `Accept-Encoding` must match what
    Trusted Server set, and be absent when it set none.
  - `sourcepoint_drops_non_utf8_request_header`
  - `validate_rejects_certificate_check_false`
  - `runtime_ignores_certificate_check_false_with_warning`
- [ ] **Step 2: Port each site** to `services.outbound_client()`, keeping
      today's method and headers. Copied client headers go through
      `headers_mut()`, never `header()`.
- [ ] **Step 3: Remove `certificate_check = false`** from the fixtures, the
      scripts, and CI (Spec §1).
- [ ] **Step 4: Verify.** Run `cargo test-fastly`, `cargo test-axum`,
      `cargo test-cloudflare`, `cargo test-spin`, and the clippy aliases.
      Expected: exit 0.
- [ ] **Step 5: Commit.** Commit with "Send integration requests through
      EdgeZero's client".

### Task 5: Proxies O6 to O9 (Spec §2.6, §2.7)

**Files:**

- Modify: `crates/trusted-server-core/src/proxy.rs` (`proxy_request`,
  `send_asset_origin_request`, `handle_asset_proxy_request`, the S3
  preflight)
- Modify: `integrations/{google_tag_manager.rs,prebid.rs,testlight.rs,gpt.rs,js_asset_proxy.rs}`
- Modify: `crates/trusted-server-core/src/asset_image_optimizer.rs`

**Interfaces:**

- Produces
  `fn redirect_policy(allowed_domains: Vec<String>, require_https: bool) -> impl FnMut(&mut OutboundRedirectContext<'_>) -> Result<OutboundRedirectAction, TrustedServerError> + 'static`
  in `proxy.rs`.
- Produces
  `fn image_optimizer_options(profile: &ImageOptimizerProfile, request: &AssetRequest) -> Result<OutboundImageOptimizerOptions, EdgeError>`
  in `asset_image_optimizer.rs`. It maps Trusted Server's region aliases to
  EdgeZero's seven regions.

- [ ] **Step 1: Port the redirect tests and add the new ones (test 4).** Port
      today's redirect, allowlist, and initial-target tests to the scripted
      client. Add:
  - `redirect_chain_past_deadline_fails_with_site_502`, which advances the
    test clock 61 s across hops.
  - `location_with_userinfo_fails_with_site_502`
  - `cross_origin_hop_strips_authorization_and_cookie`
  - `non_ascii_location_is_percent_encoded_and_followed`
  - `follow_disabled_uses_send_with_forward_headers`
- [ ] **Step 2: Add the asset and Image Optimizer readback tests (test 3,
      EZ-8).** Image Optimizer options read back on a final GET and on a final
      HEAD, and never on the S3 preflight. The preflight is buffered with
      `Original`. Also add `image_optimizer_builder_error_serves_unoptimized_with_warning`.
- [ ] **Step 3: Implement.**
  - `proxy_request` checks the initial target as today.
  - Then it calls
    `client.follow(request, OutboundRedirectLimit::new(4)?, redirect_policy(..))`
    with `.timeout(BUFFERED_TIMEOUT)` and
    `.deadline(clock.deadline_after(REDIRECT_CHAIN_DEADLINE))`.
  - `OutboundFollowFailure::Policy(e)` maps back to `e`, and `Hop` goes
    through `outbound_error`.
  - `finalize_response` receives `final_uri()`.
  - Delete `proxy_with_redirects` and `ProxyRedirectPolicy` here, with their
    tests.
- [ ] **Step 4: Write the SigV4 host test (test 12).** Add
      `sigv4_signed_asset_reaches_origin_with_signed_host_path_and_query` to
      `integration.rs`, under Viceroy, for a GET without Image Optimizer and a
      HEAD preflight with it.
- [ ] **Step 5: Verify.** Run the adapter tests and clippy aliases, and the
      SigV4 case. Expected: exit 0.
- [ ] **Step 6: Commit.** Commit with "Follow redirects and proxy assets
      through EdgeZero's client".

### Task 6: Auction O1 to O3 and the provider contract (Spec §2.4, D18)

**Files:**

- Modify: `crates/trusted-server-core/src/auction/{provider.rs,orchestrator.rs,types.rs,plan.rs,mod.rs,endpoints.rs,telemetry.rs,test_support.rs,openrtb/test_executor.rs,openrtb/tests.rs}`
- Modify: `crates/trusted-server-core/src/integrations/{adserver_mock.rs,aps.rs,prebid.rs}`
- Modify: the four adapters' state builders, at the `validate_for_target`
  calls
- Modify: `crates/trusted-server-integration-tests/src/bin/generate-viceroy-config.rs`,
  `scripts/generate-integration-viceroy-configs.sh`,
  `scripts/integration-tests-browser.sh`,
  `.github/workflows/integration-tests.yml:39-40`,
  `scripts/template-cache-local-test.sh`, and
  `browser/tests/nextjs/mobile-trace-live.spec.ts:381-389`

**Interfaces:**

- `ProviderRequestOutcome::Ready { request: OutboundRequest, parse_state: Option<Box<dyn Any + Send + Sync>> }`.
  The `parse_state` type is unchanged.
- `AuctionProvider::parse_response`, `parse_response_with_context`, and
  `parse_response_with_context_and_state` take `response: OutboundResponse`.
- `DispatchedAuction` holds `batch: OutboundBatch` and
  `slot_providers: Vec<usize>`, an index into the provider list.
- In `plan.rs`, `fn validate_fastly_backend_bound(plan: &AuctionPlan) -> Result<(), Report<TrustedServerError>>`
  counts distinct `(scheme, host, port, timeout_ms)` tuples and rejects more
  than 160 when the auction is enabled. The limits 200 and 40 move with it.
- In `plan.rs`, `fn is_loopback_http_endpoint(url: &Url) -> bool` is true only
  for `http` with host `127.0.0.1` or `[::1]`.

- [ ] **Step 1: Write the failing unit tests.**
  - `bidders_leave_together_in_one_batch`
  - `bid_after_cutoff_becomes_provider_timeout`
  - `mediator_runs_after_batch_with_auction_deadline`
  - `bidder_timeout_is_configured_value_not_remaining_budget`
  - `two_auctions_with_different_budgets_record_identical_request_options`
  - `abandoned_auction_cancels_pending_slots`, which asserts the slot's
    `ReplyRelease` is never awaited.
  - `delayed_collection_keeps_slot_elapsed_and_response_time`
  - `empty_auction_returns_without_waiting_for_deadline` (Review Focus 3).
    It asserts the clock did not advance and no request was recorded.
  - `bidder_requests_carry_consent_and_attested_client_ip_headers`
  - `fastly_backend_bound_accepts_160_and_rejects_161_tuples`
  - `providers_sharing_a_tuple_count_once`
  - `disabled_auction_skips_backend_bound`
  - `loopback_http_endpoints_are_accepted`
  - `loopback_lookalike_endpoints_are_rejected` (Review Focus 1), with the
    `localhost`, `127.0.0.2`, and `0.0.0.0` cases. The URL-host linter
    rejects the last two hosts, so each of those lines carries an
    `// allow-domain: <host>` comment. Name those suppressions in the PR body
    for review.
  - `cloudflare_and_spin_load_multi_provider_plan`, which replaces
    `startup_rejects_multi_provider_auction_plan`.
- [ ] **Step 2: Implement.**
  - Providers return `Ready`.
  - `dispatch_planned_auction` calls
    `client.start_batch_until(requests, auction_deadline)`, where
    `auction_deadline = services.clock().deadline_after(timeout)`.
  - Collection loops `batch.next()` until `Finished`, using `batch.check()`
    where today's code polls without waiting.
  - `OutboundBatchFailure` marks every unresolved slot as a provider error.
  - The mediator uses `send` with `.timeout(mediator)` and
    `.deadline(auction_deadline)`.
  - Delete `validate_for_target`, `AuctionTargetId`, the target descriptors,
    the fan-out gate, `canonicalize_transport_timeout_ms`, the name
    prediction, and `AuctionContext::transport_timeout_ms` (Spec §2.4).
  - `parse_planned_aps_response` takes an `OutboundResponse`, as the Prebid
    parser does.
  - The cfg(test) paths `run_routed`, `run_parallel_mediation`,
    `run_parallel_only`, and `run_providers_parallel`, and the cfg(test)
    `AuctionProvider` impls for APS and Prebid, move to the scripted client.
    Delete any path whose tests a ported test already covers.
- [ ] **Step 3: Switch the test bidders to loopback.**
  - `--bidder-origin-url` becomes `--provider-endpoint-origin`. It rewrites
    the scheme and authority of every provider endpoint.
  - Delete `inject_controlled_bidder_backends` and its tests.
  - Strip TLS from `template-cache-local-test.sh` (Spec §1).
  - Add `/api/trace-bidder` to the forbidden evidence list.
- [ ] **Step 4: Write the runtime auction tests (test 5).** Add
      `twelve_bidder_auction_registers_twelve_backends_fastly` to
      `integration.rs`, with 12 loopback bidders on 12 ports. Assert the bids
      and the observation's configuration count across two auctions. Add
      `multi_provider_auction_runs_{cloudflare,spin}`, and
      `spin_runtime_auction_reports_one_timeout_and_cutoff`. The loopback
      bidders record request framing.
- [ ] **Step 5: Verify.** Run the adapter tests and clippy aliases,
      `scripts/template-cache-local-test.sh`, and the new runtime cases.
      Expected: exit 0.
- [ ] **Step 6: Commit and push.** Commit with "Run auctions as EdgeZero
      batches on every adapter", then push.

### Task 7: Publisher origin O4 and O5 (Spec §2.5, D9)

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`
  (`handle_publisher_request`, `rewrite_origin_request`,
  `buffer_publisher_response_async`,
  `publisher_response_into_streaming_response`, the HTML decode bound)
- Modify: `crates/trusted-server-core/src/settings.rs` (readthrough, `origin_url`
  userinfo, the Spin warning), `creative_opportunities.rs`
- Modify: the four adapters' publisher call sites
- Modify: `crates/trusted-server-core/src/test_support/async_html_finalization.rs`
  (O19)
- Modify: `.github/workflows/integration-tests.yml:230-231` (the
  `spin_runtime_` filter)

**Interfaces:**

- `pub struct PublisherOriginOptions { pub edge_header: EdgeCacheHeader, pub input_cap: Option<u64> }`
  replaces the `edge_header` argument, which keeps the signature at 7
  arguments.
- `handle_publisher_request(settings, services, kv, ec_context, auction, req, origin: PublisherOriginOptions) -> Result<(PublisherResponse, OutboundScopeOwner), Report<TrustedServerError>>`
- `buffer_publisher_response_async(publisher_response, method, settings, integration_registry, orchestrator, services, input_cap: u64)`
- The adapters' `input_cap` values:
  - Cloudflare: `Some(min(max_buffered_body_bytes, 10 MiB))`
  - Spin: `Some(min(max_buffered_body_bytes, 8 MiB))`
  - Axum: `Some(max_buffered_body_bytes)`
  - Fastly: `None`
- Each adapter calls `owner.attach(response)?` exactly once, after the
  response function returns.

- [ ] **Step 1: Write the failing ownership tests (test 20).**
  - `raw_buffered_passthrough_and_range_attach_owner_once_when_origin_call_is_inside_scope`,
    for `send`, and for `begin` followed by `collect`.
  - `origin_call_outside_scope_fails_attach_with_already_owns_scope`
- [ ] **Step 2: Write the failing cap tests (test 21).** These run with the
      16 MiB default for `max_buffered_body_bytes`:
  - `raw_body_at_cap_passes_and_one_over_fails_502_{axum,cloudflare,spin}`
  - `source_error_mid_raw_body_fails_502_before_egress`
  - `identity_html_at_cap_passes_and_one_over_fails_502_{cloudflare,spin}`
  - `gzip_html_under_cap_encoded_over_cap_decoded_fails_502`
  - `rewrite_growing_past_output_cap_fails`
- [ ] **Step 3: Write the failing request tests.**
  - `origin_request_carries_inbound_body_and_upload_cap`
  - `upload_over_16_mib_returns_413`
  - `host_override_differing_from_authority_sets_host_authority_override`
  - `equivalent_host_override_sets_none` (Review Focus 2)
  - `spin_ignores_host_override_with_warning`
  - `non_utf8_cookie_is_dropped_and_request_forwarded` (Review Focus 4)
  - `validate_rejects_origin_url_with_userinfo`
  - `validate_rejects_origin_readthrough_enabled`
  - `runtime_treats_readthrough_as_false_with_warning`
- [ ] **Step 4: Implement (Spec §2.5).**
  - The owner is created before dispatch.
  - The auction and both origin calls are constructed inside `owner.run`.
  - `rewrite_origin_request` stops writing `Host`.
  - Requests use `streamed(..)` with `STREAMED_TIMEOUT` and the mapped cache
    policy. With an `input_cap`, they also set `.max_encoded_response_bytes`.
  - The HTML decode is bounded cumulatively at `input_cap`.
  - Raw bodies are collected with `into_bytes_bounded(cap)`.
  - Until Task 8B, Trusted Server's Fastly delivery converts the response body
    with `into_stream()`, so the attached owner lives until the body ends.
  - O19 uses `buffered(..)` with a 64-byte cap and `Bypass`. On Fastly it
    also sets `.deadline`.
- [ ] **Step 5: Write the runtime tests.** Test 2 adds
      `spin_runtime_post_auction_with_json_body`,
      `spin_runtime_publisher_post_body_reaches_origin`, and
      `spin_runtime_head_root`. Test 6 adds
      `navigation_bids_include_180_and_320_ms_and_exclude_600_ms_{fastly,axum,cloudflare,spin}`,
      with the overlapped path on Fastly and the plain path elsewhere. Test 7
      adds the seven Viceroy encoded-passthrough cases.
- [ ] **Step 6: Verify.** Run every adapter test and the clippy aliases. Run
      the integration suite with `--include-ignored`, the HTML EOF checks in
      Spec "Runtime", and `scripts/template-cache-local-test.sh`. Expected:
      exit 0. The Task 2D red set is now green.
- [ ] **Step 7: Commit and push.** Commit with "Fetch the publisher origin
      through EdgeZero's client inside the response scope", then push.

### Task 8A: Async store writes and the management API (Spec §2.9)

**Files:**

- Modify: `crates/trusted-server-core/src/platform/traits.rs`
  (`PlatformConfigStore::put` and `delete`, `PlatformSecretStore::create` and
  `delete` become `async` under `#[async_trait(?Send)]`)
- Modify: all 37 implementers, found with
  `git grep -nE 'impl (PlatformConfigStore|PlatformSecretStore) for'`, and
  their callers in `request_signing/rotation.rs` and the admin handlers
- Modify: `crates/trusted-server-adapter-fastly/src/management_api.rs`,
  `platform.rs`
- Modify: `docs/guide/integration-guide.md` (the snippet's store writes),
  `crates/trusted-server-integration-tests/tests/documentation_snippets.rs`
  (adds `async-trait` at the workspace requirement)

**Interfaces:**

- `FastlyManagementApiClient::make_request(&self, client: &HttpClient, method: Method, path: &str, body: Option<Bytes>) -> Result<Bytes, Report<TrustedServerError>>`,
  with `buffered(.., BUFFERED_TIMEOUT, 1 MiB, Decode)`.

- [ ] **Step 1: Write the failing test (test 14).**
      `rotation_sends_expected_method_headers_and_body` uses the scripted client.
      `non_2xx_management_response_truncates_body_to_200_chars`.
- [ ] **Step 2: Implement.** Make the four trait methods `async`. Update
      every implementer and caller with `.await`. The Fastly implementations
      hold the request's `HttpClient`.
- [ ] **Step 3: Verify.** Run every adapter test, the clippy aliases, and
      `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test documentation_snippets`.
      Expected: exit 0.
- [ ] **Step 4: Commit.** Commit with "Make platform store writes async and
      send management calls through EdgeZero".

### Task 8B: Fastly canonical lifecycle and post-delivery work (Spec §3, §2.8)

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/main.rs` (the callback)
- Create: `crates/trusted-server-adapter-fastly/src/delivery.rs`,
  `crates/trusted-server-adapter-fastly/src/post_delivery.rs`
- Modify: `app.rs` (delete the hand-built `App` and `startup_error_router`),
  `middleware.rs` (the per-request synthesized set), `tinybird.rs`, `compat.rs`
  (delete `to_fastly_response*` and `stream_to_client`)
- Modify: `crates/trusted-server-core/src/proxy.rs` (delete
  `stream_asset_body`)
- Modify: `crates/trusted-server-adapter-fastly/Cargo.toml`, adding the
  `test-utils` dev feature on `edgezero-adapter-fastly`

**Interfaces:**

- `pub(crate) struct FinalizeState { partner_registry: Arc<PartnerRegistry>, ec_state: EcFinalizeState }`
- `pub(crate) fn handle_delivery<S>(delivery: FastlyRequestDelivery<S>, counters: &SandboxCounters, post: impl FnOnce(S))`.
  It implements the Spec §3 outcome table, with one arm per row.
- `pub(crate) type TelemetryQueue = Arc<Mutex<Vec<String>>>`
- `pub(crate) struct QueueingTelemetrySink`, which implements today's
  telemetry sink trait by pushing NDJSON rows.
- `pub(crate) fn run_post_delivery(state: FinalizeState, queue: TelemetryQueue, settings: &Settings)`.
  It sends the telemetry rows first, then runs pull sync with
  `HttpClient::with_client(FastlyOutboundClient::new()).with_proxy_marker(false)`
  and `MonotonicClock::default()`, under `futures::executor::block_on`.
- `fn per_request_synthesized_headers(settings: &Settings, geo: Option<&GeoInfo>) -> SynthesizedResponseHeaders`
  in `middleware.rs`. It leaves out invalid values with a warning, and falls
  back to the version headers on a bounds error.

- [ ] **Step 1: Write the failing outcome tests (test 9).** With
      `delivery::testing::{deliver_with_hooks, response_delivery}`, add one
      test per outcome-table row, named
      `delivery_<outcome>_<action>`, for example
      `delivery_routed_committed_application_body_runs_post_delivery`. Also
      add `assembly_error_delivery_is_handled_like_any_other`, and
      `unconvertible_request_head_gets_native_500` (EZ-17 criterion 7), if
      Viceroy can deliver such a head. If it cannot, record that in the PR
      body.
- [ ] **Step 2: Write the failing post-delivery tests (test 13).**
  - `telemetry_rows_queue_during_request_and_send_after_delivery`
  - `no_post_delivery_when_not_committed_or_fallback_body`
  - `pull_sync_group_ends_at_fifteen_second_cutoff`
  - `axum_cloudflare_and_spin_send_no_telemetry`, using the no-op sinks
- [ ] **Step 3: Implement the callback.**
  - Native short-circuits.
  - Assembly with `App::from_router`.
  - The native request work.
  - `send_request_with_registries_and_hooks(app, TrustedServerApp::stores(), req, prepare, finalize)`.
  - Outcome handling.
  - Post-delivery work.
  - `prepare` and `finalize` follow Spec §3 exactly, including the
    `TraceTerminalResponse` early return.
  - Delete the `oneshot` dispatch, `into_core_request`,
    `edge_error_response`, `send_edgezero_response`,
    `send_response_to_client`, `open_trusted_server_config_store`, and
    `ConfigStoreHandle`.
- [ ] **Step 4: Write the Viceroy tests.** These go in `integration.rs` or
      `cargo test-fastly`, as the existing EC lifecycle tests do:
  - `finalize_sets_ec_cookies_geo_headers_cache_policy_and_counters`
  - `trace_response_passes_untouched`
  - `reused_sandbox_keeps_serving_after_delivery_failure`, under
    `cargo test-fastly-reuse`
  - `missing_config_blob_gets_503_then_recovers_in_same_sandbox` (EZ-22
    criterion 9), under `cargo test-fastly-reuse`
  - `head_limit_414_431_and_framing_500_carry_synthesized_headers` (test 11).
    It uses a `test-utils`-only route that answers with `Trailer`, and its
    431 case uses 60 header lines totaling about 72 KiB.
  - `realistic_first_party_proxy_request_is_admitted_{fastly,cloudflare,axum}`
    (test 11)
  - `publisher_page_streaming_35_seconds_completes` (test 17)
  - `pull_sync_reaches_partner_after_origin_aborts_mid_body`
  - `framing_fallback_delivery_makes_no_pull_sync_call` (EZ-17 criterion 7)
- [ ] **Step 5: Verify.**
  - Run `cargo test-fastly`, `cargo test-fastly-reuse`, `cargo clippy-fastly`,
    and the integration suite.
  - Run `./scripts/async-html-finalization-local-test.sh fastly --release --auction`.
  - Run `scripts/fastly-response-diff.sh --base <Task 0 merge sha> --head HEAD`,
    with `expected.json` filled in from Fastly differences items 1 to 24 for
    the requests that exercise them.

  Expected: exit 0.

- [ ] **Step 6: Commit and push.** Commit with "Run Fastly on EdgeZero's
      canonical lifecycle", then push.

### Task 9: Delete the old outbound layer and port the deadline harnesses (Spec §2.10, §8)

**Files:**

- Delete: `crates/trusted-server-core/src/platform/{http.rs,backend_naming.rs,pending_response.rs,image_optimizer.rs}`,
  the HTTP doubles in `platform/test_support.rs`,
  `crates/trusted-server-adapter-fastly/src/{backend.rs,platform/deadline.rs}`,
  the four adapters' outbound clients, the Spin decoder, and the Cloudflare
  encoding strip
- Modify: `crates/trusted-server-core/src/platform/{traits.rs,types.rs,mod.rs}`,
  removing `http_client`, `backend`, and the three capability methods with
  their 39 call lines
- Create: `crates/trusted-server-adapter-fastly/src/deadline_network.rs`
- Modify: `crates/trusted-server-adapter-fastly/tests/deadline-origin.mjs`
  (the inline `deadline_fixture` config store, the filter, and the count)
- Modify: `crates/trusted-server-adapter-cloudflare/tests/runtime/deadline.rs`,
  `crates/trusted-server-adapter-spin/tests/runtime/deadline.rs`
- Modify: `clippy.toml` (the `disallowed-methods` entry for
  `edgezero_core::body::Body::into_bytes`), the crate roots (the `cfg_attr`
  allow), and each `tests/*.rs` root
- Modify: `crates/trusted-server-core/src/integrations/registry.rs:264`
  (the doc comment)
- Modify: `docs/guide/integration-guide.md` (the snippet uses
  `outbound_client` and `clock`)

**Interfaces:** Deletes everything in the Spec "Dependents inventory" pattern
except the four guide documents.

- [ ] **Step 1: Port the Fastly deadline cases** into `deadline_network::tests`
      with these names:
  - `network_stalled_headers_end_at_cutoff`
  - `network_trickling_body_ends_at_cutoff`
  - `network_head_skips_declared_length`
  - `network_fast_sibling_completes`
  - `network_expired_snapshot_drops_reply_landed_during_sleep`
  - `network_reply_observed_during_wait_before_cutoff_is_kept`

  Each case builds `HttpClient::with_client(FastlyOutboundClient::new())` with
  `http://127.0.0.1:<port>` URIs read from the `deadline_fixture` store.

- [ ] **Step 2: Port the Cloudflare and Spin `deadline-runtime` examples** to
      `start_batch_until` and `send`. Keep every case in
      `scripts/test-worker-auction-deadlines.mjs`.
- [ ] **Step 3: Delete the old layer and its doubles,** then add the clippy
      guard. First move every remaining user in the Spec "Ported tests"
      paragraph to the scripted client. That covers the users of
      `StubHttpClient`, `NoopHttpClient`, and `StubBackend`, and the bespoke
      doubles it lists by line. `body_as_reader` returns an error for a stream, and the two
      production uses get a scoped `#[allow]` with a comment (Spec §4,
      "Guard").
- [ ] **Step 4: Verify.**
  - Run the Spec "Dependents inventory" command. Expected: it prints only
    `docs/guide/architecture.md`, `docs/guide/auction-orchestration.md`,
    `docs/guide/integration-guide.md`, and
    `crates/trusted-server-core/src/auction/README.md`.
  - Run the scoped `transport_timeout_ms` grep. Expected: it prints nothing.
  - Run every adapter test and clippy alias, `deadline-origin.mjs`, the
    worker deadline job's build and run commands, and
    `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test documentation_snippets`.
    Expected: exit 0.
- [ ] **Step 5: Commit and push.** Commit with "Delete Trusted Server's outbound
      layer", then push.

### Task 10: Egress validation, Axum ingress variables, and the observation log (Spec §5, §8, §3, D10, D16)

**Files:**

- Modify: `crates/trusted-server-core/src/config.rs`,
  `response_privacy.rs` (`apply_response_headers_with_cache_privacy`),
  `publisher.rs` (`make_response_bodiless`, and its test at `:26560-26575`)
- Modify: `crates/trusted-server-adapter-axum/src/main.rs` (pass
  `AxumIngressConfig::from_env(&EnvConfig::from_env())?` as `ingress`)
- Modify: `scripts/smoke-axum.sh:101-111`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs` (log
  `FastlyOutboundObservation::current()` at info on the target
  `trusted_server::outbound_observation`, after post-delivery work)

**Interfaces:**

- `const FORBIDDEN_OPERATOR_RESPONSE_HEADERS: [&str; 10]` in
  `response_privacy.rs`: `trailer`, `content-length`, `connection`,
  `keep-alive`, `te`, `transfer-encoding`, `upgrade`, `proxy-connection`,
  `proxy-authenticate`, and `proxy-authorization`.

- [ ] **Step 1: Write the failing tests (tests 15 and 16).**
  - `make_response_bodiless_removes_trailer_for_head_and_304`
  - `validate_rejects_operator_trailer_and_connection_entries`
  - `runtime_skips_malformed_connection_entry_and_serves_real_status`
  - `runtime_skip_of_connection_x_example_serves_x_example`
  - `publisher_head_and_conditional_304_return_real_status_{fastly,axum,cloudflare,spin}`
  - `axum_ingress_variable_raises_limit`
  - `axum_invalid_ingress_variable_fails_startup`
- [ ] **Step 2: Implement** the changes above.
- [ ] **Step 3: Verify.** Run every adapter test and clippy alias,
      `scripts/smoke-axum.sh`, and the integration cases. Expected: exit 0.
- [ ] **Step 4: Commit.** Commit with "Reject operator framing headers and
      read Axum ingress limits from the environment".

### Task 11: Documentation and `CHANGELOG.md` (Spec §10)

**Files:** Modify the Spec §10 list:

- `docs/guide/architecture.md`
- `docs/guide/auction-orchestration.md`
- the prose after the snippet in `docs/guide/integration-guide.md`
- the guide pages on the EdgeZero version, outbound behavior, redirects,
  timeouts, header rules, inbound limits, and CLI manifest discovery
- `crates/trusted-server-core/src/auction/README.md`
- `scripts/README.md`
- `CHANGELOG.md`

- [ ] **Step 1: Rewrite the documents.**
- [ ] **Step 2: Verify.** The Spec "Dependents inventory" command prints
      nothing. Run `cd docs && npm run lint && npm run format && npm run build`,
      the Markdown Prettier gate (CI gate 8), and
      `ts dev lint domains --changed-vs origin/main`. Expected: all exit 0.
- [ ] **Step 3: Commit and push.** Commit with "Document the EdgeZero PR 275
      cut", then push.

### Task 12: Full verification and the PR handoff

**Files:** The PR #1506 body.

- [ ] **Step 1: Run the full suite** at the head: every `AGENTS.md` CI gate
      (1 to 13), the Spec "Runtime" list, the HTML EOF checks, the response
      diff against the Task 0 merge commit, and the four smoke scripts.
      Record each command, exit code, and summary line.
- [ ] **Step 2: Update the PR body.** Add `PRfection phase: implementation`,
      `Closes #1507` (with `Closes #1513` kept), the plan link, the pinned
      `EZ_REV`, the verification record, and the hosted legs the maintainer
      runs by hand (Spec "EdgeZero qualification"). Also list the Spec "Before
      deploy" steps the maintainer runs before production: `ts config validate`
      for each environment, and the response diff's read-only staging subset.
- [ ] **Step 3: Read the PR back.** Confirm the head SHA, that `isDraft` is
      unchanged, and that every check is reported. Ask the user before marking
      the PR ready for review.
