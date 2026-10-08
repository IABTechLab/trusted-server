# Onboarding

This page explains the codebase context that the reference guides assume: what
Trusted Server does, how a publisher request moves through it, which ad-tech
terms mean what here, and where to begin working.

Use [Getting Started](/guide/getting-started) to review the prerequisites and
run a local adapter. Repository-only maintainer onboarding remains in
`docs/internal/onboarding.md`, which VitePress intentionally does not publish.

## Mental model

Trusted Server is an edge reverse proxy with a runtime-neutral Rust core. For a
publisher-page request, it can:

1. derive an Edge Cookie (EC) identity and apply consent policy;
2. dispatch a server-side ad auction while fetching the publisher origin;
3. rewrite eligible HTML and creatives and inject selected browser modules; and
4. expose configured integrations and third-party assets through first-party
   routes.

Not every request enters the publisher pipeline. Health, administrative,
integration, TSJS, and configured asset routes can terminate earlier.

## Trace a Fastly publisher request

The adapters share the core publisher handler, but their entry points and
streaming capabilities differ. This trace uses Fastly to show the streaming
path. Navigate by function name; line numbers change.

| Step | Function or area                                     | Responsibility                                                                                            |
| ---- | ---------------------------------------------------- | --------------------------------------------------------------------------------------------------------- |
| 1    | `adapter-fastly/src/main.rs` — `main`                | Answer `/health` immediately; otherwise initialize logging and call `edgezero_main`                       |
| 2    | `main.rs` — `edgezero_main`                          | Load runtime configuration, sanitize forwarded client data, capture TLS/device signals, and dispatch      |
| 3    | EdgeZero router                                      | Select a named Trusted Server route or the fallback handler                                               |
| 4    | `adapter-fastly/src/app.rs` — `dispatch_fallback`    | Build EC request state, run pre-route integration filters, and distinguish asset/integration traffic      |
| 5    | `dispatch_fallback`                                  | For an eligible publisher navigation, generate an EC ID when needed and call the shared publisher handler |
| 6    | `core/src/publisher.rs` — `handle_publisher_request` | Use the consent context, match configured slots, and evaluate the server-side ad-stack gate               |
| 7    | `handle_publisher_request`                           | Dispatch eligible bidder requests before sending the request to the publisher origin                      |
| 8    | Publisher response pipeline                          | Classify the origin response, then rewrite or inject into eligible HTML                                   |
| 9    | Fastly entry point                                   | Apply final EC and filter effects, then send the buffered or streaming response                           |

Two ordering details matter when debugging:

- Bid requests are dispatched before the publisher-origin request is sent.
  Auction collection can wait at the HTML body seam without delaying the
  initial body prefix.
- The ad-stack gate controls both auction dispatch and ad-template injection.
  A closed gate can therefore resemble an auction failure even when no bidder
  was called.

See [Architecture](/guide/architecture) for component boundaries and
[Auction Orchestration](/guide/auction-orchestration) for the auction path.

## Vocabulary used in this repository

| Term                 | Meaning here                                                                                                 |
| -------------------- | ------------------------------------------------------------------------------------------------------------ |
| Impression           | One opportunity to show an ad in a slot                                                                      |
| Ad slot or placement | A configured page region that can receive an ad                                                              |
| Bid                  | A demand source's offer for an impression                                                                    |
| CPM                  | Bid price per thousand impressions                                                                           |
| Auction              | Collection and comparison of eligible bids for the page's matched slots                                      |
| Header bidding       | Competition among demand sources before the ad server makes its final decision                               |
| SSP                  | Supply-side platform; sells publisher inventory                                                              |
| DSP                  | Demand-side platform; buys inventory for advertisers                                                         |
| Creative             | The markup, image, script, or renderer selected for an ad                                                    |
| OpenRTB              | The IAB JSON protocol used by the server auction interfaces                                                  |
| Prebid               | The open-source header-bidding ecosystem; this repository supports browser and server-side integration paths |
| GPT                  | Google Publisher Tag, the browser library used to request ads from Google Ad Manager                         |
| GAM                  | Google Ad Manager, an ad server used by publishers                                                           |
| EC ID                | Edge Cookie identifier derived with HMAC-SHA256 plus a random suffix                                         |
| Consent signal       | Privacy input such as an encoded TCF, GPP, or US Privacy value, or the GPC header                            |
| CMP                  | Consent management platform; collects choices and exposes consent signals                                    |
| First-party proxy    | A Trusted Server route that fetches an allowed third-party resource through the publisher-facing service     |

## Code map

| Path                                           | Responsibility                                                  |
| ---------------------------------------------- | --------------------------------------------------------------- |
| `crates/trusted-server-core/`                  | Runtime-neutral request, identity, consent, proxy, and ad logic |
| `crates/trusted-server-core/src/publisher.rs`  | Publisher fallback, auction dispatch, and response processing   |
| `crates/trusted-server-core/src/auction/`      | Auction orchestration and provider implementations              |
| `crates/trusted-server-core/src/ec/`           | EC identity graph and administrative operations                 |
| `crates/trusted-server-core/src/consent/`      | Consent extraction, decoding, and enforcement                   |
| `crates/trusted-server-core/src/integrations/` | Server-side integration registry and handlers                   |
| `crates/trusted-server-js/lib/src/`            | Browser-side TypeScript and integration modules                 |
| `crates/trusted-server-adapter-*`              | Fastly, Axum, Cloudflare, and Spin runtime entry points         |
| `crates/trusted-server-cli/`                   | Native `ts` operator CLI                                        |

## First development loop

1. Install the versions in `.tool-versions` with asdf or another version
   manager. The pinned Node installation supplies npm, which a clean checkout
   needs to generate the browser bundles.
2. Follow [Getting Started](/guide/getting-started) and run the Axum adapter.
   It is the shortest local path and requires no edge account.
3. Use target-specific Cargo aliases. Bare workspace `cargo build` and
   `cargo test` do not cover the mixed native and WebAssembly targets correctly.
4. Re-run `cargo install-cli` after pulling CLI or configuration-schema changes
   so the installed `ts` binary matches the checkout.
5. Trace one request through the functions above, then make a focused change
   next to an existing test. Use [Testing](/guide/testing) to select the matching
   adapter gate.

## Triage map

| Symptom                           | Start here                                                                                          |
| --------------------------------- | --------------------------------------------------------------------------------------------------- |
| Build or test command fails       | [Testing](/guide/testing); confirm the adapter and target                                           |
| Configuration is rejected         | [Configuration](/guide/configuration), then the [CLI guide](/guide/cli)                             |
| Ads do not render                 | [GPT Diagnostics](/guide/integrations/gpt-diagnostics), then inspect the ad-stack and consent gates |
| Behavior differs on a real site   | [Dev Proxy](/guide/ts-dev-proxy)                                                                    |
| An adapter fails during startup   | [Error Reference](/guide/error-reference), then the adapter-specific guide                          |
| An integration route is uncertain | [Integrations Overview](/guide/integrations-overview) and [API Reference](/guide/api-reference)     |

For contribution scope, commit conventions, and required checks, read
[CONTRIBUTING.md](https://github.com/IABTechLab/trusted-server/blob/main/CONTRIBUTING.md)
and [AGENTS.md](https://github.com/IABTechLab/trusted-server/blob/main/AGENTS.md).
