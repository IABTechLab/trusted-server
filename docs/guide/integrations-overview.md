# Integrations and Runtime Support

Trusted Server combines an edge adapter, server-side integrations, auction
providers, and browser modules. These layers are related but not
interchangeable: a deploy ID may register a settings builder, an auction-plan
component, or a mediator, while browser code can be bundled, deferred, or
loaded by a separate tag.

## Adapter support

| Adapter      | Release status | Health     | Startup status | Startup health | Provider fan-out | Trusted-client-IP handling     | Request normalization            |
| ------------ | -------------- | ---------- | -------------- | -------------- | ---------------- | ------------------------------ | -------------------------------- |
| `axum`       | development    | real       | `500`          | no             | multiple         | outermost sanitize             | none                             |
| `cloudflare` | development    | absent     | `500`          | no             | single           | outermost sanitize             | none                             |
| `fastly`     | production     | pre router | `500`          | yes            | multiple         | entry-point resolve + sanitize | none                             |
| `spin`       | experimental   | real       | `503`          | yes            | single           | outermost sanitize             | innermost Spin-header derivation |

This table summarizes the adapter implementations. “Startup health” means the
degraded startup router still serves `/health`; it does not
mean configuration loaded. “Provider fan-out” describes whether the adapter
can dispatch one auction to multiple configured providers. See the individual
[Fastly](/guide/fastly), [Axum](/guide/axum-dev),
[Cloudflare](/guide/cloudflare), and [Spin](/guide/spin) deployment guides for
their operational procedures.

## Integration inventory

| Integration                                                    | Operational status | Deploy ID | Registration       | Browser loading |
| -------------------------------------------------------------- | ------------------ | --------- | ------------------ | --------------- |
| [`adserver_mock`](/guide/integrations/adserver_mock)           | development        | yes       | auction mediator   | none            |
| [`aps`](/guide/integrations/aps)                               | development        | yes       | auction plan       | none            |
| [`creative`](/guide/creative-processing)                       | development        | no        | browser capability | bundled         |
| [`datadome`](/guide/integrations/datadome)                     | development        | yes       | settings builder   | bundled         |
| [`didomi`](/guide/integrations/didomi)                         | production         | yes       | settings builder   | bundled         |
| [`google_tag_manager`](/guide/integrations/google_tag_manager) | production         | yes       | settings builder   | bundled         |
| [`gpt`](/guide/integrations/gpt)                               | production         | yes       | settings builder   | bundled         |
| [`gpt_diagnostics`](/guide/integrations/gpt-diagnostics)       | development        | yes       | settings builder   | standalone      |
| [`lockr`](/guide/integrations/lockr)                           | production         | yes       | settings builder   | bundled         |
| [`nextjs`](/guide/integrations/nextjs)                         | production         | yes       | settings builder   | none            |
| [`osano`](/guide/integrations/osano)                           | development        | yes       | settings builder   | bundled         |
| [`permutive`](/guide/integrations/permutive)                   | production         | yes       | settings builder   | bundled         |
| [`prebid`](/guide/integrations/prebid)                         | production         | yes       | auction plan       | deferred        |
| [`sourcepoint`](/guide/integrations/sourcepoint)               | development        | yes       | settings builder   | bundled         |
| [`testlight`](/guide/integrations/testlight)                   | development        | yes       | settings builder   | bundled         |

The inventory summarizes compiled registration sets and reviewed maturity
records. A deploy ID means the deployment validator accepts
that identifier; it does not imply that every adapter implements the same
runtime capability. `creative` is a browser capability, not a deploy ID.

Browser loading has three distinct modes:

- `bundled` modules execute from the immediate unified `tsjs` bundle.
- `deferred` modules are fetched after that bundle; Prebid uses this mode.
- `standalone` modules use a dedicated tag decision and are not part of the
  unified bundle; GPT diagnostics uses this mode.
- `none` means the capability has no integration browser module.

See [Trusted Server JavaScript](/guide/tsjs) for the exact 12-module,
13-bundle model and [API Reference](/guide/api-reference) for route
families and adapter availability.

## How capabilities compose

Enabled integrations share ordered request, HTML, and browser pipelines. They
are not guaranteed to be independent. Registration predicates determine which
proxy routes, attribute or script rewriters, head injectors, request filters,
post-processors, auction providers, and browser modules are active. Validate
the complete configuration and test the resulting page rather than assuming
that any arbitrary combination is conflict-free.

Configuration is loaded when the process starts. Change it through the
[EdgeZero configuration lifecycle](/guide/edgezero), then restart or deploy the
adapter as required. Exact fields and defaults live in the
[Configuration Reference](/guide/configuration).

## Adding an integration

Use the [Integration Guide](/guide/integration-guide) for the runtime-neutral
extension path and a compiling `RuntimeServices` fixture. Add only the hooks a
capability needs; do not create a catch-all proxy or assume an adapter-specific
HTTP implementation in core code.
