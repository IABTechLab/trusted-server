# New Engineer Onboarding

This page orients a new engineer on the Trusted Server codebase. It answers the
questions the reference guides assume you already know: what the system does,
how one request flows through it, what the ad-tech vocabulary means here, and
what to do in your first week.

Read [What is Trusted Server?](/guide/what-is-trusted-server) for the product
framing and [Getting Started](/guide/getting-started) for setup. This page is
the bridge between them and the rest of the documentation.

## What the system does

A publisher puts Trusted Server in front of their site at the CDN edge. Every
page request passes through it before reaching the publisher's origin. That
position lets Trusted Server do three things a page script cannot:

1. **Serve advertising infrastructure as first-party.** Third-party ad and
   identity scripts are proxied through the publisher's own domain, so they are
   not blocked as cross-site requests and do not depend on third-party cookies.
2. **Generate and hold identity at the edge.** The Edge Cookie (EC) ID is
   derived server-side rather than written by browser JavaScript.
3. **Run the ad auction before the page is sent.** Bids can be collected while
   the origin response is still being fetched, so the auction does not have to
   wait for the browser to parse the page.

The rest of the system exists to make those three things safe: consent
enforcement, creative sanitization, request signing, and per-integration
rewriting of the publisher's HTML.

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

## Vocabulary

The reference guides use these terms without defining them. Here is what each
one means _in this codebase_.

| Term                        | Meaning here                                                                                                                                                          |
| --------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Impression**              | One opportunity to show one ad in one slot on one page view.                                                                                                          |
| **Ad slot** / **placement** | A region of the page that can hold an ad. Identified by a DOM element ID.                                                                                             |
| **Creative**                | The actual ad markup that gets rendered — HTML, an image, or a script. Arrives from a bidder and is rendered inside a sandboxed iframe.                               |
| **Bid**                     | An offer to buy one impression at a price.                                                                                                                            |
| **Auction**                 | Collecting bids for the page's slots and choosing winners.                                                                                                            |
| **Header bidding**          | Running an auction among several demand sources before calling the ad server, so they compete rather than being asked in a fixed order.                               |
| **SSP**                     | Supply-side platform — sells the publisher's inventory. A demand source from our perspective.                                                                         |
| **DSP**                     | Demand-side platform — buys on behalf of advertisers, usually via an SSP.                                                                                             |
| **CPM**                     | Cost per thousand impressions, the usual unit of a bid price.                                                                                                         |
| **OpenRTB**                 | The IAB-standard JSON format for bid requests and responses. Our types live in the `trusted-server-openrtb` crate.                                                    |
| **Prebid**                  | The open-source header-bidding framework. We ship a browser shim plus an optional first-party bundle.                                                                 |
| **GPT**                     | Google Publisher Tag, the browser library that requests ads from Google Ad Manager.                                                                                   |
| **GAM**                     | Google Ad Manager, the ad server that decides what finally renders.                                                                                                   |
| **EC ID**                   | Edge Cookie ID. A privacy-preserving identifier derived at the edge with HMAC-SHA256 rather than written by page JavaScript. See [Edge Cookies](/guide/edge-cookies). |
| **Consent string**          | An encoded record of what a user agreed to. TCF covers GDPR, GPP is the newer multi-jurisdiction container, GPC is a browser-level opt-out signal.                    |
| **CMP**                     | Consent management platform — the vendor that shows the consent banner and produces the consent string.                                                               |
| **First-party proxy**       | Serving a third-party asset through the publisher's own domain. See [First-Party Proxy](/guide/first-party-proxy).                                                    |

## Where the code lives

| Path                                           | What it is                                                                         |
| ---------------------------------------------- | ---------------------------------------------------------------------------------- |
| `crates/trusted-server-core/`                  | Nearly all the logic. Start here.                                                  |
| `crates/trusted-server-core/src/publisher.rs`  | The main request path. Large — navigate by function, not by reading top to bottom. |
| `crates/trusted-server-core/src/auction/`      | Auction orchestration, providers, and bidders.                                     |
| `crates/trusted-server-core/src/ec/`           | Edge Cookie identity.                                                              |
| `crates/trusted-server-core/src/consent/`      | Consent parsing and enforcement.                                                   |
| `crates/trusted-server-core/src/integrations/` | One module per vendor integration.                                                 |
| `crates/trusted-server-js/lib/src/`            | The browser-side TypeScript that ships to the page.                                |
| `crates/trusted-server-adapter-*/`             | Per-runtime entry points. The Fastly adapter is production.                        |
| `crates/trusted-server-cli/`                   | The `ts` operator CLI.                                                             |

The core crate is runtime-agnostic; anything Fastly-specific belongs in the
adapter. A test enforces that core never imports the Fastly SDK.

## Your first week

**Get it running.** Follow [Getting Started](/guide/getting-started). The Axum
dev server is the fastest path and needs no cloud account.

Three things that are easy to trip over:

- Bare `cargo build` and `cargo test` fail at the workspace root, because the
  adapters target different architectures. Use the aliases in
  `.cargo/config.toml` — `cargo test-axum`, `cargo test-fastly`, and so on. A
  bare `cargo test` fails while linking with missing `fastly` symbols.
- `ts` is built from this repository. After pulling changes that touch
  configuration types, reinstall it with `cargo install-cli`, or
  `ts config validate` will reject a valid `trusted-server.toml`.
- The Rust build runs the JavaScript build, so Node from `.tool-versions` is
  required even if you are only touching Rust.

**Read one request end to end.** Follow the table above through the source with
the file open. That single exercise explains more than any other.

**Make a small change.** Pick something with a test next to it, change it, and
watch the test fail. `cargo test-axum` is the quickest loop.

**Learn the tooling you will need later.**
[Dev Proxy](/guide/ts-dev-proxy) serves a publisher's real hostname from your
local build, which is the only practical way to reproduce most production
issues. [GPT Diagnostics](/guide/integrations/gpt-diagnostics) explains
`?ts_console=1`, the first thing to reach for when ads do not render.

## When something does not work

| Symptom                          | Look at                                                                                            |
| -------------------------------- | -------------------------------------------------------------------------------------------------- |
| Build or config error            | [Error Reference](/guide/error-reference)                                                          |
| Tests fail or will not run       | [Testing](/guide/testing) — check you used the right alias                                         |
| Ads not rendering                | `?ts_console=1` via [GPT Diagnostics](/guide/integrations/gpt-diagnostics); then the ad-stack gate |
| Behaviour differs on a real site | [Dev Proxy](/guide/ts-dev-proxy)                                                                   |
| Config rejected on push          | [Configuration](/guide/configuration) and [CLI](/guide/cli)                                        |

Consent is worth calling out: if the consent gate closes, there are no bids and
no ads, and the symptom looks like a broken auction rather than a consent
decision. Check consent state before debugging demand.

## Contributing

Read [CONTRIBUTING.md](https://github.com/IABTechLab/trusted-server/blob/main/CONTRIBUTING.md)
for the pull-request process and commit-message conventions, and `CLAUDE.md`
for coding standards and the full CI gate list. Run the gates that match what
you changed before opening a pull request.
