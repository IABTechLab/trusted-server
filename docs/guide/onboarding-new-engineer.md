# New Engineer Onboarding

This page orients a new engineer on the Trusted Server codebase. It answers the
questions the reference guides assume you already know: what problem the system
solves, how one request flows through it, what the ad-tech vocabulary means
here, and what to do in your first week.

Read [What is Trusted Server?](/guide/what-is-trusted-server) for the product
framing and [Getting Started](/guide/getting-started) for setup. This page is
the bridge between them and the rest of the documentation.

## Project overview

Trusted Server is an open-source edge computing framework from IAB Tech Lab
that moves advertising operations out of third-party browser JavaScript and
into WebAssembly running on edge platforms.

### The problem it solves

- **Privacy restrictions.** Third-party cookie deprecation and tracking
  prevention limit traditional advertising.
- **Third-party dependency.** Publishers have little control over the
  third-party scripts running on their pages.
- **Performance.** Multiple third-party scripts slow page load.
- **Data control.** Publishers need control over how and with whom data is
  shared.

### What edge position buys

- **First-party context.** Ads and assets are served from the publisher's own
  domain.
- **Consent enforcement.** Consent is read and enforced before any demand call
  is made.
- **Better performance.** Server-side processing reduces client-side
  JavaScript.
- **Identity at the edge.** The Edge Cookie (EC) ID is derived server-side
  rather than written by page JavaScript.

## Architecture at a glance

```
                        User's browser
                              │
                              ▼
┌──────────────────────────────────────────────────────────┐
│           Edge runtime (adapter + core)                  │
│                                                          │
│  Adapter (Fastly / Cloudflare / Spin / Axum)             │
│  • Entry point, routing, platform bindings               │
│  • Client IP, TLS signals, EC request state              │
│                          │                               │
│         ┌────────────────┼────────────────┐              │
│         ▼                ▼                ▼              │
│  ┌───────────┐   ┌──────────────┐   ┌──────────────┐     │
│  │  Proxy    │   │  Publisher   │   │ Integrations │     │
│  │           │   │              │   │              │     │
│  │ /first-   │   │ Origin fetch │   │ Prebid, GPT, │     │
│  │ party/*   │   │ Ad-stack gate│   │ APS, consent │     │
│  │ Creative  │   │ Auction      │   │ vendors, ... │     │
│  │ rewriting │   │ HTML rewrite │   │              │     │
│  └───────────┘   └──────────────┘   └──────────────┘     │
│                                                          │
│  Storage layer                                           │
│  • KV stores    • Config stores    • Secret stores       │
└──────────────────────────────────────────────────────────┘
                              │
              ┌───────────────┴───────────────┐
              ▼                               ▼
      Publisher origin                 Demand partners
```

The core crate is runtime-agnostic; anything platform-specific lives in an
adapter. A test enforces that core never imports the Fastly SDK.

### Technology stack

| Layer          | Technology                               |
| -------------- | ---------------------------------------- |
| Language       | Rust {{RUST_VERSION}}                    |
| Runtime        | WebAssembly (`wasm32-wasip1` for Fastly) |
| Edge platforms | Fastly Compute, Cloudflare Workers, Spin |
| Dev server     | Axum (native)                            |
| Client library | TypeScript (tsjs)                        |
| Build tools    | Cargo, esbuild                           |

## The request path

This is the single most useful thing to understand. A publisher page request
travels roughly this route. Line numbers drift; use the function names.

| Step | Where                                                           | What happens                                                                                                     |
| ---- | --------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| 1    | `adapter-fastly/src/main.rs` — `main`                           | `/health` short-circuits before anything else loads, then `edgezero_main` runs                                   |
| 2    | `main.rs` — `edgezero_main`                                     | Client IP is resolved and sanitized; trusted TLS headers are re-injected; client and device signals are captured |
| 3    | `adapter-fastly/src/app.rs` — `build_ec_request_state`          | Device signals, bot gate, geo, and the EC context are assembled                                                  |
| 4    | `app.rs` — `run_pre_route_filters`                              | Integration request filters run before routing                                                                   |
| 5    | `core/src/publisher.rs` — `handle_publisher_request`            | The main publisher path                                                                                          |
| 6    | `core/src/creative_opportunities.rs` — `evaluate_ad_stack_gate` | Decides whether the ad stack runs at all for this request                                                        |
| 7    | `publisher.rs` — auction dispatch                               | Bids are requested **before** the origin response is sent, while the original client headers are intact          |
| 8    | `core/src/html_processor.rs` — `create_html_processor`          | One streaming pass rewrites URLs and injects integration scripts                                                 |
| 9    | `publisher.rs`                                                  | The response is finalized, EC state is written, and the body is streamed                                         |

Two details in that list surprise people:

- **The auction starts before the origin responds.** It is not triggered by the
  browser. Step 7 happens while step 8 is still waiting on the publisher.
- **The ad-stack gate can turn everything off.** If step 6 declines, no auction
  runs and no scripts are injected. When ads are missing, check the gate before
  suspecting the auction.

See [Architecture](/guide/architecture) for the component view and
[Auction Orchestration](/guide/auction-orchestration) for the auction itself.

## Key concepts

### First-party proxying

Instead of loading ad creatives and vendor scripts directly from third-party
domains, Trusted Server proxies them through first-party endpoints:

```
Before:  Browser → ad-server.example/creative.html
After:   Browser → publisher.example/first-party/proxy?tsurl=...
```

Everything stays under the publisher's domain, which avoids third-party cookie
restrictions and tracking prevention. Proxy URLs are signed, so the endpoint
cannot be used as an open relay. See
[First-Party Proxy](/guide/first-party-proxy).

### Edge Cookie identity

The EC ID is a privacy-preserving identifier derived at the edge with
HMAC-SHA256 rather than written by page JavaScript. It is deterministic for the
same inputs, non-reversible, and publisher-controlled. Rotating the passphrase
resets the identity graph. See [Edge Cookies](/guide/edge-cookies).

### Integration modules

Each vendor lives in its own module under
`crates/trusted-server-core/src/integrations/` and registers the hooks it
needs — proxying, request filtering, attribute rewriting, script rewriting,
HTML post-processing, or head injection. Browser-side counterparts live in
`crates/trusted-server-js/lib/src/integrations/`. See the
[Integration Guide](/guide/integration-guide) before adding one.

### Request signing

Ed25519 signing authenticates outbound API requests. Public keys are published
at `/.well-known/trusted-server.json`, and rotation is supported with a grace
period. See [Request Signing](/guide/request-signing) and
[Key Rotation](/guide/key-rotation).

## Where the code lives

| Path                                           | What it is                                                              |
| ---------------------------------------------- | ----------------------------------------------------------------------- |
| `crates/trusted-server-core/`                  | Nearly all the logic. Start here.                                       |
| `crates/trusted-server-core/src/publisher.rs`  | The main request path. Large — navigate by function, not top to bottom. |
| `crates/trusted-server-core/src/auction/`      | Auction orchestration, providers, and bidders.                          |
| `crates/trusted-server-core/src/ec/`           | Edge Cookie identity.                                                   |
| `crates/trusted-server-core/src/consent/`      | Consent parsing and enforcement.                                        |
| `crates/trusted-server-core/src/integrations/` | One module per vendor integration.                                      |
| `crates/trusted-server-js/lib/src/`            | The browser-side TypeScript that ships to the page.                     |
| `crates/trusted-server-adapter-*/`             | Per-runtime entry points. The Fastly adapter is production.             |
| `crates/trusted-server-cli/`                   | The `ts` operator CLI.                                                  |

## Vocabulary

The reference guides use these terms without defining them. Here is what each
one means _in this codebase_.

| Term                        | Meaning here                                                                                                                                       |
| --------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Impression**              | One opportunity to show one ad in one slot on one page view.                                                                                       |
| **Ad slot** / **placement** | A region of the page that can hold an ad. Identified by a DOM element ID.                                                                          |
| **Creative**                | The actual ad markup that gets rendered — HTML, an image, or a script. Arrives from a bidder and renders inside a sandboxed iframe.                |
| **Bid**                     | An offer to buy one impression at a price.                                                                                                         |
| **Auction**                 | Collecting bids for the page's slots and choosing winners.                                                                                         |
| **Header bidding**          | Running an auction among several demand sources before calling the ad server, so they compete rather than being asked in a fixed order.            |
| **SSP**                     | Supply-side platform — sells the publisher's inventory. A demand source from our perspective.                                                      |
| **DSP**                     | Demand-side platform — buys on behalf of advertisers, usually via an SSP.                                                                          |
| **CPM**                     | Cost per thousand impressions, the usual unit of a bid price.                                                                                      |
| **OpenRTB**                 | The IAB-standard JSON format for bid requests and responses. Our types live in the `trusted-server-openrtb` crate.                                 |
| **Prebid**                  | The open-source header-bidding framework. We ship a browser shim plus an optional first-party bundle.                                              |
| **GPT**                     | Google Publisher Tag, the browser library that requests ads from Google Ad Manager.                                                                |
| **GAM**                     | Google Ad Manager, the ad server that decides what finally renders.                                                                                |
| **EC ID**                   | Edge Cookie ID. See [Edge Cookies](/guide/edge-cookies).                                                                                           |
| **Consent string**          | An encoded record of what a user agreed to. TCF covers GDPR, GPP is the newer multi-jurisdiction container, GPC is a browser-level opt-out signal. |
| **CMP**                     | Consent management platform — the vendor that shows the consent banner and produces the consent string.                                            |

## Your first week

**Get it running.** Follow [Getting Started](/guide/getting-started). The Axum
dev server is the fastest path and needs no cloud account.

**Read one request end to end.** Follow the request-path table above through the
source with the files open. That single exercise explains more than any other.

**Make a small change.** Pick something with a test beside it, change it, and
watch the test fail. `cargo test-axum` is the quickest loop.

**Learn the tooling you will need later.**
[Dev Proxy](/guide/ts-dev-proxy) serves a publisher's real hostname from your
local build, which is the only practical way to reproduce most production
issues. [GPT Diagnostics](/guide/integrations/gpt-diagnostics) explains
`?ts_console=1`, the first thing to reach for when ads do not render.

### Reading these docs locally

This documentation is a [VitePress](https://vitepress.dev) site that lives in
`docs/`. You are probably reading the published version, but running it locally
gives you full-text search over every guide and lets you preview any change you
make before opening a pull request.

```bash
cd docs
npm ci
npm run dev
```

The site is served at `http://localhost:5173/trusted-server/`. The
`/trusted-server/` suffix matters: the site sets a `base` path for GitHub
Pages, so the bare `http://localhost:5173` redirects rather than serving the
home page. VitePress prints the correct URL when it starts. Pages reload as you
save, so you can keep it open while you read. Stop it with `Ctrl+C`.

Each page maps to one Markdown file under `docs/guide/`, so the fastest way to
find the source of something you are reading is to search the repository for a
phrase from the page.

If you edit a page, run these before opening a pull request:

```bash
npm run format:write   # apply Prettier formatting
npm run lint           # ESLint
npm run build          # production build; fails on broken internal links
```

`npm run build` is the one that matters most: it fails the build on a dead
internal link, so it catches a mistyped `/guide/...` path that would otherwise
ship. `npm run preview` serves the built output if you want to check the
production result.

Version numbers such as the Rust and Viceroy versions on this page are not
written literally in the Markdown. Each entry in `.tool-versions` gets a
double-brace placeholder named after the tool in upper case followed by
`_VERSION`, and `docs/.vitepress/config.mts` substitutes it at build time. So
tool versions in the docs cannot drift from the pinned toolchain. If you need
to cite one in a page, use the placeholder rather than typing the number.

### Local origin stub

To exercise the first-party proxy against a fully local origin, point the
publisher origin at a local server and sign an asset URL:

```bash
# Terminal 1: serve a file from a local origin
export TRUSTED_SERVER__PUBLISHER__ORIGIN_URL=http://localhost:9090
mkdir -p /tmp/ts-origin
printf 'hello from origin\n' > /tmp/ts-origin/hello.txt
python3 -m http.server 9090 --directory /tmp/ts-origin
```

```bash
# Terminal 2: with the server running, sign the URL and fetch it
curl -s "http://127.0.0.1:7676/first-party/sign?url=http://localhost:9090/hello.txt"
```

Request the signed path that comes back; the response body should be
`hello from origin`.

## Common issues

| Issue                                       | Solution                                                                                                                                |
| ------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| Bare `cargo build` / `cargo test` fails     | The adapters target different architectures. Use the aliases in `.cargo/config.toml`, such as `cargo test-axum` or `cargo test-fastly`. |
| `ts config validate` rejects a valid config | `ts` is built from this repository. Reinstall it with `cargo install-cli` after pulling changes to configuration types.                 |
| Rust build fails in the JS step             | The Rust build runs the JavaScript build, so Node from `.tool-versions` is required even for Rust-only work.                            |
| `cargo test-fastly` fails on Viceroy        | Install the pinned version: `cargo install viceroy --version {{VICEROY_VERSION}} --locked --force`.                                     |
| Tests pass locally but fail in CI           | Run `cargo fmt --all -- --check` and the target-matched clippy aliases; CI denies warnings.                                             |
| No bids and no ads                          | Check consent state and the ad-stack gate before suspecting demand.                                                                     |

## When something does not work

| Symptom                          | Look at                                                                                   |
| -------------------------------- | ----------------------------------------------------------------------------------------- |
| Build or config error            | [Error Reference](/guide/error-reference)                                                 |
| Tests fail or will not run       | [Testing](/guide/testing) — check you used the right alias                                |
| Ads not rendering                | `?ts_console=1` via [GPT Diagnostics](/guide/integrations/gpt-diagnostics), then the gate |
| Behaviour differs on a real site | [Dev Proxy](/guide/ts-dev-proxy)                                                          |
| Config rejected on push          | [Configuration](/guide/configuration) and [CLI](/guide/cli)                               |

## Contributing

Read [CONTRIBUTING.md](https://github.com/IABTechLab/trusted-server/blob/main/CONTRIBUTING.md)
for the pull-request process and commit-message conventions, and `CLAUDE.md`
for coding standards and the full CI gate list. Run the gates that match what
you changed before opening a pull request.
