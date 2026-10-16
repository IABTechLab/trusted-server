# Lockr and Permutive SDK URL ownership

Status: Implemented and independently reviewed. Validation results are recorded below.

Issue: [#1199](https://github.com/IABTechLab/trusted-server/issues/1199).

## Problem and invariant

The browser Lockr guard claims any script URL containing `aim.loc.kr`, including
another vendor's query string, a path segment, or a lookalike hostname. The
server matcher is narrower but still searches the entire string. Both Permutive
matchers have the same host-substring defect. Full-string extension checks also
miss genuine SDK URLs with query strings.

With all six insertion guards installed, a GPT URL whose query mentions Lockr
has two claimants. GPT rewrites `script.src` at assignment time, preserving the
query. Lockr then claims the first-party URL at insertion and replaces GPT with
the configured Lockr SDK. Changing dispatcher order cannot fix that path.

```mermaid
flowchart TD
    A[GPT URL with Lockr text in its query] --> B[GPT src setter rewrites it first-party]
    B --> C[Dispatcher offers the URL to insertion guards]
    C --> D[Old Lockr substring matcher claims the query text]
    D --> E[Configured Lockr SDK replaces GPT]
```

For the URL cases covered by this change, each original vendor URL must have
exactly its expected claimant, or none for an unowned URL. Lockr and Permutive
must make the same ownership decision in browser and Rust code. Recognition
must depend on the parsed vendor hostname and SDK pathname, never a vendor name
elsewhere in the URL.

## Scope

Change only the Lockr and Permutive URL matchers, their tests, the shared URL
fixture, and the relevant integration documentation. Leave dispatcher behavior,
other vendors' matching rules, logging, configuration, upstream SDK serving,
and proxy destinations unchanged.

Issue #1247 and Lockr initialization, polling, and shim behavior are excluded.
In particular, do not edit `src/integrations/lockr/index.ts` in the JS library.

## Matching contract

Use the browser `URL` parser and Rust's existing `url::Url` dependency. Keep the
small parsing and matching logic local to each integration unless implementation
reveals enough shared policy to justify a helper. Do not redesign shared guards.

### Input forms

- Trim ASCII whitespace before parsing: space, tab, LF, CR, form feed, and vertical
  tab. Do not trim Unicode whitespace such as U+FEFF or U+0085. Include those
  boundary cases in the shared corpus.
- Require an explicit `http://`, `https://`, or `//` prefix, case-insensitive for
  schemes. Reject shorthand such as `https:host/path` and `https:/host/path`.
- Accept absolute HTTP and HTTPS URLs, including uppercase schemes and hosts.
  After the prefix check, malformed means rejected by the native URL parser;
  normal parser normalization is allowed.
- Accept protocol-relative URLs by adding `https:` before parsing. Ownership is
  independent of the page's HTTP/HTTPS scheme.
- Reject empty, malformed, non-network, relative-path, and bare-host inputs.
  A bare hostname is not an authority in a browser script attribute. The DOM
  resolves relative attributes before insertion handlers see them; on a normal
  publisher page they remain publisher URLs and must not be claimed.
- Match the parsed hostname, not the authority string. A port does not change
  hostname ownership. User information must not make an unrelated host match.
- Use exact hostname boundaries. Do not strip a trailing dot or invent extra
  hostname aliases. Rely on the parsers' normal hostname normalization.
- Inspect the parsed pathname without percent-decoding it. Query strings and
  fragments do not participate in SDK recognition. Preserve existing
  case-insensitive ASCII SDK path matching.

### Lockr

The hostname must be exactly `aim.loc.kr` or `identity.loc.kr`. The lowercased
pathname must contain `identity-lockr` and end in `.js`.

This retains the existing Rust SDK path family, including versioned and
trust-server SDK names, rather than introducing a new basename allowlist. It
intentionally stops claiming unrelated scripts or assets on the Lockr host.

### Permutive

The hostname must be exactly `cdn.permutive.com`, or have a nonempty subdomain
prefix before `.edge.permutive.app`. The lowercased pathname must end in
`-web.js`. Neither the bare `edge.permutive.app` hostname nor a lookalike suffix
such as `.edge.permutive.app.example.com` matches.

### Rewriting and compatibility

Recognition of query-bearing and fragment-bearing SDK URLs is new for the Rust
Lockr matcher, browser Lockr on `identity.loc.kr`, and both Permutive matchers.
Cover both Lockr hosts explicitly. Those URLs must now take the existing proxy
route. This does not forward their query or fragment to upstream: Lockr
and Permutive serve the SDK selected by existing integration configuration.
Keep `/integrations/lockr/sdk` and `/integrations/permutive/sdk` unchanged.

Malformed inputs fail without throwing or preventing another guard from running.
Narrowing non-network and bare-host strings is intentional. Preserve normal
absolute and protocol-relative SDK URLs, and case-insensitive matching.

## Files and test data

Runtime changes belong in:

- `crates/trusted-server-js/lib/src/integrations/lockr/script_guard.ts`
- `crates/trusted-server-js/lib/src/integrations/permutive/script_guard.ts`
- `crates/trusted-server-core/src/integrations/lockr.rs`
- `crates/trusted-server-core/src/integrations/permutive.rs`

Create `crates/trusted-server-js/lib/test/fixtures/sdk-url-ownership.json`. Each
case records a descriptive name, input URL, and expected integration ID or null.
An optional `insertionOwner` records a different expectation after native DOM URL
resolution. In particular, raw matchers reject shorthand schemes, while a script
or link URL getter normalizes them to absolute vendor URLs before insertion.
Rust tests and raw browser claimant tests use `owner`; DOM insertion tests use
`insertionOwner` when present. This distinguishes parser input policy from the
browser's existing attribute-resolution behavior.
Use fictional publisher/customer data. Public vendor hosts are necessary here
to test the actual hard-coded ownership rules, not illustrative customer URLs.
Import the same fixture in Vitest and Rust's inline test modules using the
existing `aps-renderer-v1.json` include/import convention.

The corpus must cover:

- Canonical URLs for all six guards: Permutive, Lockr, Sourcepoint, GTM,
  DataDome, and GPT.
- Both Lockr hosts and both Permutive host families.
- Real SDK paths with query strings, fragments, uppercase text, protocol-relative
  forms, and ports.
- Other vendors' URLs mentioning either affected vendor in queries or paths.
- Host prefixes, suffixes, path mentions, and user-information lookalikes.
- Unrelated resources on genuine vendor hosts and wrong SDK extensions.
- Already-rewritten first-party GPT URLs whose query mentions a vendor host.
- Empty/malformed, relative, bare-host, shorthand-scheme, and non-network inputs.
- Trailing-dot hostnames, percent-encoded SDK path text, the bare
  `edge.permutive.app` host, and ASCII/Unicode whitespace boundaries.

Replace stale comments pointing to Rust line numbers with pointers to this
shared corpus.

## Regression tests

### Matcher and server tests

For each corpus case, run both Rust matchers and assert whether the expected
owner equals that integration. Run the corresponding JS guards against the
same input URL so parser decisions cannot drift behind duplicated case lists.
Also assert server attribute rewrite actions for matching and nonmatching cases,
including `src` and `href`, while retaining disabled-rewrite coverage.

### All-guard ownership and insertion tests

Install all six real guards. Examine every registered handler against the
original corpus URL, using a fresh untouched element per handler and passing the
raw URL directly in the candidate. Do not initialize it through `script.src`,
which would let GPT's assignment patches hide a claimant. This also prevents
mutations by one handler influencing another. Assert the exact claimant ID list,
not only a successful final rewrite. Do this entirely in test code without adding a new
production dispatcher API.

Separately exercise actual insertion through both `script.src = url` and native
`Element.prototype.setAttribute.call(script, 'src', url)`, then append the node.
Assert the final first-party route or unchanged attribute. Account for ordinary
DOM relative-URL resolution separately from the raw matcher assertion.

Include the issue's three reproduced Lockr collision cases in
`test/integrations/lockr/url_collision.test.ts`. Force Lockr ahead of GPT/GTM in
test-only handler ordering as well as using normal dispatcher order. Reversing
installation alone is insufficient because priorities and IDs set dispatch order.
Ensure tests clean up guards, GPT's extra patches/observer, and dispatcher state.

Cover preload and prefetch script links for both affected integrations, including
query-bearing valid SDKs and false positives, through append and insertBefore.

## Delivery and validation

1. Record an independent fresh-context review of this spec, verify its findings,
   and revise the contract where warranted before implementation.
2. Add the shared corpus and regression tests. Confirm the relevant cases fail
   against the original matchers before fixing runtime code.
3. Apply the four matcher corrections and run focused JS and native Rust tests.
4. Update the Lockr/Permutive guides with SDK ownership and query behavior.
5. Run broader suites and repository-required handoff gates. Reuse passing checks
   while their relevant inputs remain unchanged. Record failures and unavailable
   checks explicitly and keep the pull request in draft.

Focused checks from the JS library directory:

```sh
npx vitest run test/integrations/lockr test/integrations/permutive test/shared/dom_insertion_dispatcher.test.ts
```

Focused checks from the repository root, using the pinned Rust 1.95 toolchain
and core's default feature set:

```sh
cargo test -p trusted-server-core integrations::lockr::tests
cargo test -p trusted-server-core integrations::permutive::tests
cargo fmt --all -- --check
```

At the combined checkpoint, run the full JS suite, JS lint/format/build, docs
format, and the CI gates specified in `AGENTS.md`, including target-matched
adapter tests/clippy and integration parity. Cargo builds depend on the JS
crate, whose build script installs missing npm dependencies and builds bundles;
coordinate those commands instead of running competing dependency installs.

## Independent review

A fresh Pi process using `openai-codex/gpt-6.1-sol` reviewed this spec, the full
issue, and current code without conversation history or edits. It found no
blocking defects. Its suggestions were accepted: bypass GPT assignment patches
in raw claimant tests, add explicit trailing-dot/encoded-path/bare-edge-host
fixture rows, and document query/fragment recognition on browser Lockr's
`identity.loc.kr` host. Its open questions about whitespace and malformed URL
spellings were resolved by the explicit common input rules above.

A second fresh-context correctness review inspected the implementation, tests,
fixture, and guides, traced the insertion and server rewrite paths, and ran the
focused JS tests. It reported no findings.

## Validation results

The regression tests failed against the original matchers before the fixes.
The final shared corpus contains 83 cases. The all-guard test expands them into
1,068 ownership and insertion checks across both handler orders and DOM paths.

| Check                                                                                                         | Result                                                                      |
| ------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------- |
| Full JS suite, `npx vitest run`                                                                               | Passed, 2,279 tests across 51 files; no reported type errors                |
| JS `npm run lint`, `npm run format`, `npm run build`                                                          | Passed                                                                      |
| Native Lockr and Permutive inline test modules                                                                | Passed, 11 and 9 tests, including the shared corpus and attribute rewriting |
| `cargo test-fastly`, `cargo test-fastly-reuse`, `cargo test-axum`, `cargo test-cloudflare`, `cargo test-spin` | Passed; existing ignored tests remain ignored                               |
| All eight target-specific clippy aliases listed in `AGENTS.md`                                                | Passed                                                                      |
| Integration parity test                                                                                       | Passed                                                                      |
| Native template build-digest tests and clippy                                                                 | Passed                                                                      |
| `cargo fmt --all -- --check`, wasm clock lint                                                                 | Passed                                                                      |
| Docs and repository Markdown formatting                                                                       | Passed                                                                      |
| URL-host lint on added lines                                                                                  | Passed                                                                      |

An additional full native-core run, `cargo test -p trusted-server-core`, had
2,940 passing tests and 12 pre-existing legacy settings-test failures. Cargo
injects `TRUSTED_SERVER__GIT_VERSION`, which the legacy environment settings
loader rejects as the unknown field `git_version`. This was reproduced in an
untouched archive of `5bc388625` with
`cargo test -p trusted-server-core settings::tests::test_origin_host_header_override_env -- --exact`.
No settings or version-handling changes are included here. The required Fastly
core suite and focused native integration tests pass.

No live vendor SDK fetch or production deployment smoke test was performed.

## Investigation evidence

At `5bc388625`, an in-memory esbuild bundle of the actual guard source executed
in JSDOM with all six guards installed reproduces GPT replacement via `script.src`,
Lockr query/path/lookalike false positives, Permutive lookalike false positives,
and the Permutive query-string false negative. The native setAttribute path
leaves GPT correctly owned under current ordering. This is diagnostic evidence,
not a substitute for the committed regression tests or full validation.
