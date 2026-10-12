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

## Asynchronous HTML finalization

An integration that needs complete transformed HTML plus an outside lookup can
register an `IntegrationHtmlFinalizerFactory` with `.with_html_finalizer(...)`.
The factory creates one request-local `IntegrationHtmlFinalizer` session and
receives the same `IntegrationDocumentState` as the synchronous HTML rewriters.
Factory construction performs setup only; start I/O inside the returned future.
The session and its future may be `?Send`.

Core completes decoding and every synchronous processor, then awaits terminal
sessions in registration order. Each session sees the previous committed HTML;
its replacement does not pass through the parser again. The auction seam and
encoder complete after the terminal phase. Existing `StreamProcessor` callbacks
remain synchronous, and pages with no registered finalizers keep their existing
streaming path.

`HtmlFinalizationInput` supplies the actual `response_status`, borrowed `html`, `services`, `deadline`, and a
separate bounded `replacement` writer. Append a complete candidate and return
`HtmlFinalization::Replace`, or return `Unchanged`. An error, timeout, invalid
assembly marker count, or output limit violation discards the candidate and
retains the current input. Ignoring an append error cannot commit partial output.
An empty `Replace` intentionally emits an empty document. Callback panics propagate
through the request and are not converted into optional-work failures. Preserve HTML structure,
CSP, Next.js/RSC semantics, and the opaque parser markers; core checks marker
multiplicity but does not parse or validate replacement structure.

Full 404/500 HTML remains eligible. A signing integration should inspect
`response_status` and return `Unchanged` for statuses it does not sign. When
finalizers are registered, HTML with status 206 or any `Content-Range` bypasses
rewriting and terminal I/O while preserving encoded body bytes and range/framing
metadata. Without registered finalizers, existing partial-response processing
continues. Non-document fetches keep their range request semantics. Independent
cookie and diagnostics privacy rules still apply.

Each real consumer must test a winning auction after replacement: the marker is
consumed once, and the bids script remains before the structural body end, outside
scripts, comments, and attributes. Unchanged bytes beside a marker cannot establish
its parsing context, so adjacent-byte checks are not a structural guarantee.

The publisher's `html_finalization_timeout_ms` is one budget for the whole chain,
starting after synchronous EOF. Use `deadline.remaining()` to bound outside calls.
Axum, Workers, and Spin provide cooperative timers. Fastly provides no timer
because host calls block; the consumer must set transport timeouts itself. Dropping
a future releases its borrows and discards its candidate, but cannot undo a remote
write or preempt work that does not yield. The deadline wins over a simultaneously
ready callback. Avoid logging HTML, lookup URLs, or external response contents.
The default backend first-byte timeout is 15 seconds; it is not a total-call
deadline. Use the remaining budget for supported transport limits and verify the
consumer's blocking-call behavior on Fastly before enabling it.

Enabled finalizers retain transformed HTML up to `max_buffered_body_bytes` and use
a separate candidate with the same cap. Collection overflow skips terminal I/O
and resumes normal delivery with the retained prefix and remaining bytes. Existing
input, decoder, and final output errors remain fatal. Enabled pages wait for EOF
within the cap; oversized pages also delay first-byte delivery until the cap is
crossed and ordinary streaming resumes.

Use `stream_publisher_body_async`, `buffer_publisher_response_async`, or
`publisher_response_into_streaming_response` to execute terminal callbacks.
`stream_publisher_body` and the generic synchronous pipeline perform synchronous
transformations only. Callers with a pending auction must use an async entry point
to collect or abandon it; `stream_publisher_body` borrows the response parameters
and leaves auction ownership with the caller, including on a partial-HTML bypass.
On async entry points, client cancellation drops the callback and retains the
existing auction guard's log warning; error paths that can await emit abandonment
telemetry.

Finalizer-enabled HTML bypasses shared-template reads, reservations, and stores and
uses inline auction assembly. Raw-origin readthrough remains independent. Processed
HTML receives `Cache-Control: no-store, private` even when a finalizer leaves it
unchanged, fails, or is skipped after collection overflow. Document GETs strip the
four conditional/range headers used by synthesized HTML. HEAD, bodiless responses,
unsupported encodings, and non-HTML responses run no terminal callbacks.

This first version therefore removes transformed-template and downstream HTML
caching for enabled pages, including page-level signing. Raw-origin caching and
an integration's own KV result cache can still reduce origin and lookup work.
Cacheable finalization needs a separate contract for reader-neutral output,
external-data/signing-key identity, expiry, invalidation, optional failure caching,
and whether callbacks execute on hits; a user-independent flag alone is insufficient.

Callers of `handle_publisher_request` provide the enabled integration registry in
`AuctionDispatch::integration_registry` and use that same registry for response
finalization. This metadata is required before a shared-template lookup can happen.
It is a source-compatibility change for external `AuctionDispatch` struct literals:
add the required field using the same request registry. An omitted or optional
registry could incorrectly admit a cached document before finalization is known.

A fictional HTTP consumer is available only through the existing `test-utils`
feature. To verify real asynchronous I/O and timeout fallback on local runtimes:

```bash
./scripts/async-html-finalization-local-test.sh axum
./scripts/async-html-finalization-local-test.sh cloudflare
./scripts/async-html-finalization-local-test.sh spin
./scripts/async-html-finalization-local-test.sh fastly
```

Each run builds a fresh feature-enabled artifact, generates temporary configuration
and example secrets, and retains diagnostics in the printed temporary directory.
It uses the installed runtime CLIs and, for Workers, the `worker-build` version
matching the locked `worker` SDK and a compatible `wasm-bindgen` CLI. The default
Cargo output is isolated in that directory; an explicit `CARGO_TARGET_DIR` permits
sharing the local build cache. Every run invokes Cargo and builds its fixture Worker
bundle into a fresh directory. Workers and Spin use opt-in adapter `test-utils`
features for local diagnostic sinks.

The repository pins Viceroy 0.17 for its normal test gate. Backend transport timeout
simulation requires Viceroy 0.19 or later. Use an isolated newer executable in `PATH`
for the Fastly hung-call case; do not interpret a hung test on 0.17 as evidence that
Fastly production ignores the configured timeout.

Add `--release` for production-profile artifacts. Add `--auction` to run a separate
enabled-auction control against a closed loopback
bid endpoint. It verifies slot/bid fallback assembly and marker removal without
external traffic. The auction telemetry clock now uses the platform-compatible
`web_time::Instant`; the combined Workers control passes after this targeted
correction. This does not verify every other clock path covered by #1240.

CI runs the actual Axum harness with `--auction`, including delayed success,
hung-call fallback, privacy, and auction assembly. It also checks process-group
cleanup and retains runtime diagnostics on failure. The other runtime harnesses
remain explicit local verification commands.

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
