# Roadmap

This page separates shipped behavior from active engineering and deferred
ideas. It does not assign release dates. GitHub issues and accepted design
documents are the source for committed future work.

## Shipped

- Fastly Compute, Cloudflare Workers, and Fermyon Spin adapters, plus the
  native Axum development adapter.
- Publisher-origin proxying and shared HTML/creative processing.
- Edge Cookie generation, consent evaluation, partner identity sync, and
  administrative EC routes.
- Server-side auctions with generic OpenRTB, Prebid Server, and APS profiles;
  optional `adserver_mock` mediation; and first-party creative delivery.
- Request signing, JWKS discovery, key-management routes, and signature
  verification.
- TSJS core and integration modules selected from the checked Rust and JS
  registries.

Shipped does not imply identical adapter capabilities. Consult the deployment
and API references for target-specific route, store, fan-out, and telemetry
limits.

## Active engineering

The repository currently tracks follow-up work for these verified gaps:

- complete config-store integration on adapters whose runtime cannot yet read
  the store written by `ts config push`;
- align health and startup-failure behavior across adapters;
- decide whether standard-profile `imp_ext` needs reserved-member protection;
- remove or implement dead adapter configuration surfaces;
- make Fastly staging deployments select the staging config blob; and
- retire the deprecated `POST /__ts/page-bids` alias after measured traffic has
  moved to `POST /_ts/page-bids`.

## Deferred or unshipped

The following are not current product capabilities:

- direct Google Ad Manager or Kargo integrations;
- a headless-browser creative-forensics or malvertising-blocking service;
- invalid-traffic scoring, automated fraud detection, or an LLM optimization
  framework;
- WebSocket auction transport;
- Akamai EdgeWorkers support; and
- a standalone cross-publisher identity service.

The excluded `business-use-cases.md` file contains unverified planning claims,
including quantified benefits and malvertising detection. Those claims are not
evidence of shipped functionality.

## Proposing work

Search the [issue tracker](https://github.com/IABTechLab/trusted-server/issues)
before opening a proposal. A proposal should identify the affected adapter or
core surface, define observable acceptance criteria, and avoid promising a
release date until maintainers schedule it.
