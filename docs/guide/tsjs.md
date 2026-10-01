# Trusted Server JavaScript

`tsjs` is the browser runtime produced by
`crates/trusted-server-js/lib`. It supplies shared registry/render behavior
and integration modules selected by the server-side registry.

## Module and bundle inventory

There are 12 integration source modules: `creative`, `datadome`, `didomi`,
`google_tag_manager`, `gpt`, `gpt_diagnostics`, `lockr`, `osano`,
`permutive`, `prebid`, `sourcepoint`, and `testlight`. The build emits
13 named bundles because it also emits `core`.

Those counts do not mean a page downloads 13 scripts. The server computes one
content-addressed unified bundle from `core` plus the enabled immediate
modules. Its URL includes the exact module-set hash, so different enabled sets
do not share incorrect browser or edge-cache bytes.

## Loading modes

- Immediate modules are compiled into the synchronous unified bundle.
- Prebid's integration shim is deferred and loaded separately after the
  immediate bundle.
- `gpt_diagnostics` is excluded from both unified and deferred registry
  output. When the server's request decision enables diagnostics, it injects a
  dedicated synchronous standalone tag.

The first-party external Prebid.js artifact is a separate concern. When
configured, `GET /integrations/prebid/bundle.js` proxies the operator-built
bundle; it is not the deferred integration shim.

## Runtime contract

Modules register against the shared `window.tsjs` namespace. The registry
orders initialization and avoids duplicate installation. Server-injected
bootstrap state may exist before the compiled bundle loads; module setup must
therefore be idempotent and preserve the server's handoff state.

Dynamic script guards use one shared DOM insertion dispatcher. An integration
may decide whether a candidate script is replaced, removed, or left alone, but
must not install a competing global `appendChild` or `insertBefore` wrapper.
See [GPT](/guide/integrations/gpt) for the slot/bootstrap handoff and
[Integration Guide](/guide/integration-guide) for extension boundaries.
