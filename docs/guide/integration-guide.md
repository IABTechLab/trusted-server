# Integration Development

Trusted Server integrations are platform-neutral registrations assembled by
`IntegrationRegistry`. Adapter crates provide I/O through
`RuntimeServices`; integration code must not import Fastly, Cloudflare,
Axum, or Spin SDK types.

## Choose the narrowest hook

- `IntegrationProxy` owns explicit method/path endpoints and receives
  `Settings`, `RuntimeServices`, and an EdgeZero-neutral request.
- `IntegrationAttributeRewriter` inspects selected HTML attributes.
- `IntegrationScriptRewriter` handles one declared selector.
- `IntegrationHeadInjector` inserts deterministic head markup.
- `IntegrationHtmlPostProcessor` is for bounded whole-document work that
  cannot be performed during streaming.
- `IntegrationRequestFilter` makes an early request decision.

Build one `IntegrationRegistration` with only the hooks the feature needs.
Use `with_deferred_js()` only for a separately loaded integration module and
`without_js()` when another asset path owns delivery. Proxy routes should be
namespaced and bounded; do not introduce a general outbound proxy.

## Compiling core-neutral fixture

The fixture below registers an attribute rewriter and constructs every
required `RuntimeServices` service without an adapter dependency. The
documentation test extracts this exact fence and compiles it as an isolated
crate.

<!-- documentation-snippet:runtime-services:start -->

```rust
use std::net::IpAddr;
use std::sync::Arc;

use error_stack::Report;
use trusted_server_core::integrations::{
    AttributeRewriteAction, IntegrationAttributeContext,
    IntegrationAttributeRewriter, IntegrationRegistration,
};
use trusted_server_core::platform::{
    BackendNamingPolicy, ClientInfo, GeoInfo, PlatformBackend,
    PlatformBackendSpec, PlatformConfigStore, PlatformError, PlatformGeo,
    PlatformSecretStore, RuntimeServices, StoreId, StoreName,
    UnavailableHttpClient, UnavailableKvStore,
};

struct ReadOnlyStore;

impl PlatformConfigStore for ReadOnlyStore {
    fn get(
        &self,
        _store: &StoreName,
        _key: &str,
    ) -> Result<String, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    fn put(
        &self,
        _store: &StoreId,
        _key: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    fn delete(
        &self,
        _store: &StoreId,
        _key: &str,
    ) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
}

impl PlatformSecretStore for ReadOnlyStore {
    fn get_bytes(
        &self,
        _store: &StoreName,
        _key: &str,
    ) -> Result<Vec<u8>, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    fn create(
        &self,
        _store: &StoreId,
        _key: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    fn delete(
        &self,
        _store: &StoreId,
        _key: &str,
    ) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
}

struct FixtureBackend;

impl PlatformBackend for FixtureBackend {
    fn naming_policy(&self) -> BackendNamingPolicy {
        BackendNamingPolicy::Axum
    }

    fn predict_name(
        &self,
        spec: &PlatformBackendSpec,
    ) -> Result<String, Report<PlatformError>> {
        if spec.host.is_empty() {
            Err(Report::new(PlatformError::Backend))
        } else {
            Ok("fixture-origin".to_owned())
        }
    }

    fn ensure(
        &self,
        spec: &PlatformBackendSpec,
    ) -> Result<String, Report<PlatformError>> {
        self.predict_name(spec)
    }
}

struct FixtureGeo;

impl PlatformGeo for FixtureGeo {
    fn lookup(
        &self,
        _client_ip: Option<IpAddr>,
    ) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(None)
    }
}

struct AssetRewriter;

impl IntegrationAttributeRewriter for AssetRewriter {
    fn integration_id(&self) -> &'static str {
        "example"
    }

    fn handles_attribute(&self, attribute: &str) -> bool {
        matches!(attribute, "src" | "href")
    }

    fn rewrite(
        &self,
        _attribute: &str,
        value: &str,
        _context: &IntegrationAttributeContext<'_>,
    ) -> AttributeRewriteAction {
        value
            .strip_prefix("https://assets.example/")
            .map(|path| AttributeRewriteAction::replace(format!("/assets/{path}")))
            .unwrap_or_else(AttributeRewriteAction::keep)
    }
}

pub fn registration() -> IntegrationRegistration {
    IntegrationRegistration::builder("example")
        .with_attribute_rewriter(Arc::new(AssetRewriter))
        .build()
}

pub fn runtime_services() -> RuntimeServices {
    let store = Arc::new(ReadOnlyStore);
    RuntimeServices::builder()
        .config_store(store.clone())
        .secret_store(store)
        .kv_store(Arc::new(UnavailableKvStore))
        .backend(Arc::new(FixtureBackend))
        .http_client(Arc::new(UnavailableHttpClient))
        .geo(Arc::new(FixtureGeo))
        .client_info(ClientInfo::default())
        .build()
}
```

<!-- documentation-snippet:runtime-services:end -->

Production adapters replace every unavailable or fixture service with the
target implementation. The builder deliberately panics when a required service
is omitted, so adapter startup must construct the complete service graph.

## Proxy implementation rules

An `IntegrationProxy::handle` implementation receives the complete runtime
service graph. Register or predict backends through `services.backend()`,
send through `services.http_client()`, bound request and response bodies, and
return `Report<TrustedServerError>` with integration context. The registry
strips internal identity headers before dispatch; an integration must opt into
any explicit forwarding behavior.

Streaming support is an adapter capability. Request
`PlatformHttpRequest::with_stream_response()` only when the caller has
checked `supports_streaming_responses()`; unsupported adapters must reject
the request instead of silently buffering it.

## Asynchronous HTML processor completion

An integration can register an HTML stream processor that optionally awaits HTTP,
KV or other request services after its synchronous EOF call. Use the existing
`IntegrationHtmlStreamProcessorFactory` and `.with_html_stream_processor(...)`.
Article signing is only one possible consumer; core implements the processor
lifecycle, not a signing protocol or background job system.

Factories declare `requires_async_finish()`. Sessions implement the matching
`StreamProcessor::requires_async_finish()` and optional `async_finish(...)`.
The default callback returns `None`, creating no future. Factory metadata is
available before template lookup, so requested EOF work cannot be skipped by a
processed cache hit. The factory and session share request-local
`IntegrationDocumentState` with the existing parser hooks.

A non-retaining session can make a KV lookup at EOF and append a fixed footer.

::: warning Do not copy the KV lookup to Fastly
The generic KV interface has no hard timeout, and Fastly installs no cooperative
timer, so an unbounded KV call can hold the response indefinitely. The example
skips the lookup when `input.deadline.has_cooperative_timer()` is false. On
Fastly, use an HTTP lookup whose transport timeouts are capped with
`input.deadline.remaining()` instead.
:::

<!-- documentation-snippet:html-eof:start -->

```rust
use error_stack::Report;
use futures::future::LocalBoxFuture;
use std::io;
use trusted_server_core::integrations::{
    IntegrationHtmlStreamContext, IntegrationHtmlStreamProcessorFactory,
};
use trusted_server_core::streaming_processor::StreamProcessor;
use trusted_server_core::streaming_processor::finish::{
    StreamFinish, StreamFinishError, StreamFinishInput,
};

/// Append a fixed footer when an example KV key is present.
pub struct ExampleFooterProcessor;

/// Create a request-local footer processor with matching EOF capability.
pub struct ExampleFooterFactory;

impl IntegrationHtmlStreamProcessorFactory for ExampleFooterFactory {
    fn integration_id(&self) -> &'static str {
        "example_footer"
    }

    fn requires_async_finish(&self) -> bool {
        true
    }

    fn create(&self, _context: IntegrationHtmlStreamContext) -> Box<dyn StreamProcessor> {
        Box::new(ExampleFooterProcessor)
    }
}

impl StreamProcessor for ExampleFooterProcessor {
    fn process_chunk(&mut self, bytes: &[u8], _is_last: bool) -> io::Result<Vec<u8>> {
        Ok(bytes.to_vec())
    }

    fn requires_async_finish(&self) -> bool {
        true
    }

    fn async_finish<'a>(
        &'a mut self,
        input: StreamFinishInput<'a>,
    ) -> Option<LocalBoxFuture<'a, Result<StreamFinish, Report<StreamFinishError>>>> {
        Some(Box::pin(async move {
            if !input.deadline.has_cooperative_timer() {
                return Ok(StreamFinish::Unchanged);
            }
            let enabled = input
                .services
                .kv_store()
                .get_bytes("example:footer")
                .await
                .map_err(|_| Report::new(StreamFinishError::Unavailable))?
                .is_some();
            if !enabled {
                return Ok(StreamFinish::Unchanged);
            }
            input.replacement.append(input.output)?;
            input.replacement.append(b"<!--example-footer-->")?;
            Ok(StreamFinish::Replace)
        }))
    }
}
```

<!-- documentation-snippet:html-eof:end -->

The factory and session both return `true` from `requires_async_finish()` for
their entire lifetimes. Their capabilities must match; a mismatch fails the request.
Leave `buffer_output_until_eof()` at its default `false` for this append
operation. KV must be available on the runtime; unavailable lookup falls back to
the synchronous tail. Fastly consumers must use host operations with independently
bounded latency and cap configurable transport timeouts with `input.deadline.remaining()`;
this generic KV interface does not provide a hard timeout. Checking the remaining
budget alone cannot interrupt a blocking KV call.

Operators must explicitly accept the possible delivery cost:

```toml
[publisher]
allow_async_html_finish = true
html_finalization_timeout_ms = 5000
```

Permission defaults to false. Enabling an integration requiring async completion
without permission produces a configuration error naming the integration and the
setting. Work is not silently omitted. Permission alone, with no async factories,
keeps ordinary processing unchanged.

Chunk callbacks remain synchronous. At EOF, core validates/drains the decoder and
finishes the HTML parser, sending ready bytes through downstream processors as
ordinary chunks. It then completes each processor in registration order: synchronous
EOF, optional awaited finish, accepted output passed downstream, then downstream EOF.
The auction and encoder finish afterward. Ready bytes are released before optional
I/O; a callback is never run inside a parser handler.

`StreamFinishInput` supplies actual `response_status`, borrowed `output`, `services`,
`deadline` and a separate bounded `replacement` writer. The output is the un-emitted
synchronous tail of this stage. It is the complete retained output only when the
factory explicitly requests `buffer_output_until_eof()`. Return `StreamFinish::Unchanged`
to retain it, or write a complete candidate and return `Replace`. To append a tail,
write the existing `output` followed by the new bytes. Empty replacement is intentional.
The session and future may be non-`Send`; start I/O inside the returned future.

Core buffers a stage's output only when its factory explicitly requests retention.
The holder and replacement candidate each use `max_buffered_body_bytes` as their cap.
These caps apply per holder; simultaneous holders and codec/adapter buffers can
use more memory in total. On collection overflow, core releases prefix/crossing/later output exactly once and
skips that stage's optional I/O, while still completing its synchronous EOF. It cannot
bound arbitrary private allocations inside an integration. Streaming stages cannot
replace bytes already delivered; EOF append does not imply safe insertion into an
already emitted head or before an earlier body close.

Callback errors, poisoned candidates, invalid assembly-marker multiplicity and timeout
discard the candidate and retain current stage output. Previously accepted stages stay
accepted. One absolute deadline covers every callback, starting after decoder/parser
EOF. On expiry, core drops the callback, skips later optional I/O, and still completes
all downstream synchronous EOF calls. Origin, parser, synchronous processing, codec and
sink errors remain fatal. Callback panics propagate. Cancellation does not undo remote
writes or guarantee that transport work is cancelled.

Axum, Workers and Spin provide cooperative timers. Fastly has none because its host
calls block; bound those calls with `deadline.remaining()` and transport timeouts.
A timer cannot preempt CPU work or blocking host calls. Avoid logging HTML, lookup URLs
or external response contents.

The latency contract depends on the adapter and the stage:

- Without async factories, delivery and auction timing keep their existing path.
- On Fastly, a non-retaining stage releases ready HTML before optional EOF I/O.
  With an auction seam, the body-close and following tail wait for all hooks before
  auction assembly; initial bytes can arrive promptly while page completion waits.
- A retaining stage delays first HTML delivery until EOF or overflow release.
- Axum, Workers and Spin currently buffer publisher responses. Enabled EOF I/O can
  add to TTFB there even without stage retention. This feature does not add streaming
  support to those adapters.

Use `stream_publisher_body_async`, `buffer_publisher_response_async`, or
`publisher_response_into_streaming_response` for opted-in HTML. The synchronous
pipeline and `stream_publisher_body` reject async processing before input/output
effects. The raw partial-HTML copy path performs no callback; the synchronous helper
leaves pending auction ownership with its caller, so pending auctions still require
an async entry point. Async cancellation retains the auction guard's warning; error
paths that can await emit abandonment telemetry.

Opted-in HTML bypasses shared-template reads/reservations/stores and receives
`Cache-Control: no-store, private`, including unchanged, failed and overflow-bypassed
work. Raw-origin readthrough remains independent. Cacheable asynchronous output needs
separate rules for output identity, external-data freshness, expiry, invalidation,
failure caching and callback execution on hits; this is tracked in
[#1397](https://github.com/IABTechLab/trusted-server/issues/1397).

When async factories are registered, HTML with 206 or any `Content-Range` bypasses
rewriting and EOF I/O while preserving encoded bytes, framing, validators and origin
cache policy. Independent cookie/diagnostics privacy still applies. HEAD, bodiless,
non-HTML and unsupported encodings run no callbacks. Complete error HTML is eligible;
inspect `response_status` to decide whether the integration applies. Without async
factories, existing partial-response handling continues.

Callers of `handle_publisher_request` supply the same enabled registry in
`AuctionDispatch::integration_registry` and response processing. This required field
allows cache admission to see async capabilities before lookup.

Replacement output passes through later stream processors, but is not parsed again.
Preserve CSP, Next.js/RSC semantics and opaque assembly markers. Core checks marker
multiplicity including split boundaries, not HTML structure. Each real consumer must
verify winning-auction placement after its changes and preserve the structural body
end; neighboring bytes alone cannot establish a marker's parsing context.

A fictional HTTP processor is available only with `test-utils`. See
[HTML EOF runtime checks](/guide/testing#html-eof-runtime-checks) for the runtime
harness, runtime/tooling prerequisites and CI coverage.

## Browser and script guards

Add a browser module only when browser state is required. Immediate modules
join the hashed unified bundle; deferred and standalone delivery are explicit
registry decisions. Dynamic script interception must register with the shared
DOM insertion dispatcher, remain idempotent, and leave unmatched elements
untouched. See [Trusted Server JavaScript](/guide/tsjs) and
[GPT's guarded handoff](/guide/integrations/gpt#server-slot-handoff).

## Registration checklist

1. Add typed settings with validation and a disabled default unless the
   integration is intentionally universal.
2. Register the exact capability predicates and route methods.
3. Add source and behavior parity records.
4. Add positive, negative, body-bound, header, and adapter-capability tests.
5. Document configuration, failure behavior, and runtime limitations.
6. Run the target aliases from [Testing](/guide/testing).
