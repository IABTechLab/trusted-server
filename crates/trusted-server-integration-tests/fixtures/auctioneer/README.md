# Production PBS baseline

This harness executes supplied default Fastly release WASM and the matching
Linux x86_64 release CLI. It prepares the production fixture topology from the
Auctioneer integration plan while upstream engine work is pending. It does not
qualify Auctioneer, APS, shared engine equivalence, live GPT/GAM, or deployment.

All publisher, bidder, asset, Viceroy and Chromium processes run inside one
Linux amd64 worker on one owned internal Docker network. The runtime has a
read-only root, read-only product inputs, writable work/results mounts and
bounded temporary storage. The coordinator publishes no ports, starts no host
HTTP server/browser and mounts no Docker socket. Builds and downloads finish
before isolation.

Prepare an amd64 Linux Rust/Node builder image with repository-pinned Rust
1.98.1 at `/opt/rustup` and Node 24.20.0 at `/opt/node`. Supply its immutable
local image ID as `BUILDER_IMAGE`. Build from a clean committed checkout through
`git archive`; this freezes the worker's source at `/source`, including its
compile-time `CARGO_MANIFEST_DIR`. The final image pins Playwright 1.58.2's Noble
image by its enforced manifest digest, installs NSS tooling, stages the lockfile-built bidder-only Prebid bundle
and pinned PUC, and verifies the native worker's ELF dependencies and test entry. Supply the
qualified Linux amd64 Noble image reference in `PLAYWRIGHT_IMAGE`, including
its `@sha256:` manifest digest. See the official [Playwright Docker
documentation](https://playwright.dev/docs/docker) for its image source; the
Dockerfile refuses a different digest or a mutable tag.

```bash
test -z "$(git status --porcelain)"
git archive HEAD | docker build --platform linux/amd64 \
  --build-arg "BUILDER_IMAGE=$BUILDER_IMAGE" \
  --build-arg "PLAYWRIGHT_IMAGE=$PLAYWRIGHT_IMAGE" \
  --build-arg "HARNESS_COMMIT=$(git rev-parse HEAD)" \
  --iidfile /tmp/example-worker-image \
  -f crates/trusted-server-integration-tests/fixtures/auctioneer/Dockerfile -
```

Produce both input binaries with the unchanged `scripts/release/phase1-build.sh`
and retain their successful producer logs, feature/profile facts and SBOMs.
Use separate fresh producer workspaces/output directories. The Linux CLI
producer also runs its GNU baseline and runtime distribution checks. A native
macOS CLI cannot run in this worker. Input metadata records the product source
separately from the harness image's source; a harness-only revision can reuse
already qualified unchanged product artifacts.

Create an input JSON file from those producer records, using actual digests and
paths relative to that file. `testUtils` describes the supplied product binaries,
not the separately compiled native fixture worker. This metadata is an input
contract, not a replacement for producer evidence or a signed attestation.

```json
{
  "producer": "scripts/release/phase1-build.sh",
  "productCommit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "variant": "default",
  "profile": "release",
  "testUtils": false,
  "wasm": {
    "path": "trusted-server-fastly.wasm",
    "sourceCommit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
  },
  "cli": {
    "path": "ts-x86_64-unknown-linux-gnu",
    "sourceCommit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "sha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
  }
}
```

Run from the repository root, choosing a fresh output directory:

```bash
node crates/trusted-server-integration-tests/fixtures/auctioneer/network.mjs \
  --image "$(cat /tmp/example-worker-image)" \
  --artifacts /tmp/example-producer-inputs.json \
  --out /tmp/example-production-evidence
```

The coordinator validates input hashes/native platform and inspects actual
network, root and mount settings before and after execution. Rust uses the
supplied CLI for config initialization and local push, preserving generated
config blobs; it never calls the development guest/CLI builder. The same guest
runs positive and wrong-SAN/untrusted-CA bidder cases. Original HTTPS URLs,
ports, Host, SNI and verification remain enabled. Rust explicitly injects its
owned CA into Viceroy after environment filtering. Chromium has a disposable
owned NSS database and explicit home/browser cache, with certificate errors
still enforced.

Browser controls execute real `/auction`, public Prebid targeting and GPT
fixture targeting, publisher navigation with Rust-injected bids, no-bid and
refresh. Only the external GPT callback API is a labelled fixture; the runner
never fulfills auction responses or assigns browser bid state. PUC/render bridge
and creative DOM/assets are served through actual runtime/fixture listeners.
This remote baseline does not exercise the engine's finalized targeting map.

The browser guard is installed before pages, blocks service workers and records
unmatched HTTP/redirect/WebSocket requests. Three fresh-context controls must
each detect their attempted off-list authority and reject the case; network
errors alone do not pass them. Positive cases require zero violations. Browser
and bidder TLS rejection cases must also pass.

Results retain worker/browser output, input/image/source identities, actual
container topology, accepted configuration snapshots/digests, CLI logs, TLS receipts/fingerprints, NSS and
executable hashes, response digests, public/GPT targeting and creative
observations. On failure the coordinator retains diagnostics and removes only
its owned container/network. Do not count a missing or failing production run
as accepted Auctioneer evidence.

Portable helper checks, without Docker or production artifact acceptance:

```bash
node --test crates/trusted-server-integration-tests/fixtures/auctioneer/network.test.mjs
node --test crates/trusted-server-integration-tests/browser/helpers/trace-gpt-fixture.test.cjs \
  crates/trusted-server-integration-tests/browser/initial-render/pages.test.cjs
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml \
  --features html-eof-runtime --test html_eof_runtime
```
