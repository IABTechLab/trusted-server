# Writing an Identity Provider

This guide is for engineers who add a vendor to Trusted Server's server-side identity framework. Operators configuring a provider that already exists want the [Configuration Reference](/guide/configuration#identity-providers-ec-identity) and the [Edge Cookies](/guide/edge-cookies) guide instead.

Every example uses a fictional provider, `exampleid`, that calls `api.exampleid.example.com`. No vendor ships with the framework. Vendors arrive in their own specs.

For the protocol, the storage layout and the reasoning behind each rule, see the [design spec](https://github.com/IABTechLab/trusted-server/blob/main/docs/superpowers/specs/2026-10-20-server-side-identity-providers-design.md). Section numbers below point into it.

## What a provider is

A provider turns request signals into OpenRTB EIDs. It does that in one or both of two ways:

- **Active resolution.** It builds an HTTPS request to the vendor from the visitor's signals, then parses the vendor's reply into IDs.
- **Passive capture.** It reads a response that Trusted Server already proxies for the vendor, with no outbound call. Capture is revoke-only (see [Capture](#capture)).

A provider does no I/O. It builds requests and parses bytes that are already buffered. The coordinator does the rest:

| The coordinator handles                                                   | You write                                 |
| ------------------------------------------------------------------------- | ----------------------------------------- |
| Consent checks against your policy before any byte leaves                 | A `ConsentPolicy` in the descriptor       |
| Real-browser gate, eligibility, refresh timing, per-visitor burst cap     | Nothing                                   |
| HTTPS, host allow-list, no `Cookie` header, redirects, timeouts, body cap | Declared hosts                            |
| Backoff on failure and the circuit breaker                                | Nothing                                   |
| The single KV write, ownership, expiry and opt-outs                       | A `Resolution` that says what you learned |
| Hold mode versus after-send dispatch                                      | Nothing (the operator picks the mode)     |

## The pieces

Implement `IdentityProvider`. Only `descriptor` is required. You add a capability by declaring it in the descriptor and overriding the matching method (`request` and `response` for resolving, `capture` for capturing). Every default body returns a `not_implemented` marker, so a capability that is declared but not implemented fails in the harness.

Implement `IdentityProviderConfig` on your integration's config type. Its `build_descriptor` builds the descriptor from config alone, with no secrets, so `ts config validate` and the running provider see the same descriptor. Your constructor calls it and stores the result.

## The descriptor

```rust
ProviderDescriptor::builder("exampleid".parse()?)
    .with_source("exampleid.example.com".parse()?)
    .with_resolves(
        Resolves::new()
            .with_endpoint("https://api.exampleid.example.com/v1/fetch")
            .with_needs([Input::ClientIp, Input::UserAgent])
            .with_wants([Input::PageUrl])
            .with_private_state(),
    )
    .with_consent_policy(
        ConsentPolicy::strict()
            .with_tcf_purposes([4])
            .with_tcf_vendor(9999),
    )
    .build()?
```

- `with_source` declares each source domain you may write. A source also needs an `[[ec.partners]]` entry.
- `Resolves::with_endpoint` must be an `https` URL. Its host becomes the only host you may call. `with_host` adds more.
- `with_needs` inputs are required: without them the provider is skipped. `with_wants` inputs are passed when available.
- `with_private_state` lets you keep a small JSON blob between calls (see [Private state](#private-state)).
- `with_expiring_ids` or `with_expiring_source` marks IDs as expiring. A set for an expiring source must then carry an expiry.
- `Resolves::with_refresh_grace` is for an owner without private state whose refresh accepts an expired ID. An expired record it wrote stays a refresh anchor until its expiry plus the grace, so a backoff or a quiet spell does not turn the next call into an `Initial`. It needs the `Refresh` kind and a grace above zero.
- `ConsentPolicy::strict()` is the default. It blocks when any consent signal cannot be decoded, requires TCF Purpose 1 where TCF applies, and blocks on every US opt-out, `Sec-GPC: 1` and `DNT: 1`. A provider that resolves must declare the purposes its GVL entry lists with `with_tcf_purposes` (Purpose 1 is always required) and its GVL ID with `with_tcf_vendor`. A vendor with no GVL ID calls `without_tcf_vendor` instead, so purpose consent alone lets it run. That and relaxations such as `allowing_dnt` need a documented legal basis in the vendor spec.
- `build()` fails with `Config` for a descriptor with no source or no capability, a missing or non-`https` endpoint, invalid provenance, or a resolving policy that declares no purposes or names no vendor without `without_tcf_vendor`.

The descriptor must never read a secret field. Deploy validation runs before secrets resolve and would see key names. `check_descriptor_ignores_secrets` enforces this in tests.

### Provenance defaults

`with_provenance(source, EidProvenance)` sets default `inserter`, `matcher` and `mm` for a source. The default is applied at output only, and only to a stored entry that sets none of the three. An entry that sets any of them is the vendor's complete statement, and no default mixes in. Defaults are not stored, so a config change applies at once. A blank `inserter` or `matcher`, one over 255 bytes, or an `mm` below 0 fails `build()`.

## Reading inputs

You get only what the descriptor declared. Read a required input with `?` and a wanted one with `.ok()`:

```rust
let ip = input.client_ip()?;
let page = input.page_url().ok();
```

An accessor fails in one of two ways:

- `UndeclaredInput`: you read something the descriptor never declared. This is a provider bug. Even if you hide the error with `.ok()`, the read is recorded, the coordinator logs it as `Config`, and the harness fails your test.
- `InputUnavailable`: the input was declared but is missing this time. This is a normal skip, handled like returning `None` from `request`.

Always available: `consent()`, `publisher_domain()`, `now()`, `random_bytes()`, `stored(source)` and the declared `endpoint()`. When the sources in one request have different kinds, use `input.is_initial(&source)` or `input.is_refresh(&source)` rather than matching on the kind.

## Building a request

`request` returns `Ok(Some(request))`, or `Ok(None)` to skip this time. A skip is not a failure: the coordinator waits `refresh_after_sec` before asking again.

```rust
fn request(
    &self,
    input: &ResolveInput<'_>,
) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
    let consent = input.consent();
    let body = FetchBody {
        partner: self.partner_id,
        ip: input.client_ip()?.to_string(),
        ua: input.user_agent()?,
        page: input.page_url().ok().map(Url::as_str),
        gdpr: consent.gdpr_applies(),
        consent: consent.raw_tc_string(),
        signature: input.state::<ExampleIdState>()?.map(|state| state.signature),
    };
    ProviderRequest::post_json(input.endpoint()?, &body).map(Some)
}
```

The coordinator refuses to send a request that is not `https`, that targets an undeclared host, that carries a `Cookie` header, or whose headers are invalid. Use `try_with_call_context` to pass data from `request` to `response`, such as a nonce. It stays in memory, is never sent, and is limited to 1,024 bytes of JSON.

## Parsing a response

`response` receives the buffered, already decoded body and returns a `Resolution`:

```rust
fn response(
    &self,
    response: &ProviderResponse,
    input: &ResolveInput<'_>,
) -> Result<Resolution, Report<IdentityProviderError>> {
    let reply: FetchReply = response.success_json()?;
    let Some(uid) = reply.uid else {
        return Ok(Resolution::no_match());
    };

    let mut uid = ResolvedUid::new(uid).with_atype(1);
    if let Some(link_type) = reply.link_type {
        uid = uid.with_ext(json!({ "linkType": link_type }));
    }
    let mut eid = ResolvedEid::new(input.primary_source(), uid);
    if let Some(max_age) = reply.max_age_sec {
        eid = eid.with_refresh_in(Duration::from_secs(max_age));
    }
    let resolution = Resolution::found([eid]);
    Ok(match reply.signature {
        Some(signature) => resolution.replacing_state(&ExampleIdState { signature }),
        None => resolution,
    })
}
```

Return `Rejected` (through `success_json` or `IdentityProviderError::rejected`) for a refusal and `Malformed` for a body you cannot parse. Both back the visitor off. Do not return an error for "this visitor is unknown": that is `Resolution::no_match()`.

| Constructor                         | Meaning                                                                                                                                                                                                  |
| ----------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Resolution::found(entries)`        | One or more `ResolvedEid` entries                                                                                                                                                                        |
| `Resolution::no_match()`            | Nobody recognized. `with_retry_in` sets the wait (default 24 hours)                                                                                                                                      |
| `Resolution::invalidate(source)`    | The stored ID for `source` is no longer valid                                                                                                                                                            |
| `found(..).also_invalidate(source)` | Return some sources and drop others in one response                                                                                                                                                      |
| `Resolution::opted_out()`           | The visitor opted out with the vendor. Sticky until a re-grant: consent that was denied when the opt-out was recorded is granted again, the vendor issues again (capture-only), or an operator clears it |

## Complete provider EIDs

A `ResolvedEid` is one OpenRTB EID entry: a source, one or more `ResolvedUid`s, and EID-level fields. A result may hold several entries for one source, in the order bidders should see them, and several UIDs per entry.

```rust
let first = ResolvedEid::new(&source, ResolvedUid::new("uid-a").with_atype(1).with_ext(json!({ "kind": "a" })))
    .with_uid(ResolvedUid::new("uid-b"))
    .with_inserter("exampleid.example.com")
    .with_matcher("exampleid.example.com")
    .with_mm(2);
let second = ResolvedEid::new(&source, ResolvedUid::new("uid-c")).with_ext(json!({ "segment": "x" }));
let resolution = Resolution::found([first, second]);
```

- UID-level fields: `atype` and `ext`.
- EID-level fields: `inserter`, `matcher`, `mm` and `ext`.
- Timing (`with_expires_in`, `with_expires_at`, `with_refresh_in`, `with_refresh_at`) applies to the source's whole set. When several entries of a source set it, the earliest time wins.
- `with_refresh_until` is for a provider with private state. It marks when an ID stops being refreshable, so an expired record stays as a refresh anchor until then.

### Limits

The coordinator groups a result's entries by source and checks each group with the browser parser's field rules, plus per-source caps the browser parser does not have. It applies them before the result joins the write.

- An entry for a source you do not claim, or that the operator narrowed away, is discarded.
- A UID that is empty, blank or over 512 bytes is dropped. An entry left with no UID is discarded.
- A blank or over-255-byte `inserter` or `matcher`, an `ext` that is not an object or exceeds 2,048 bytes as compact JSON, and an `mm` or `atype` below 0 are dropped as fields. The entry is kept.
- An entry equal to an earlier kept entry of the same source is dropped.
- A source keeps at most **8 entries** and **32 UIDs** across them. The rest are dropped.
- A source's entry set over **4,096 bytes** (`MAX_ENTRY_SET_BYTES`, measured as stored, without provenance defaults) is discarded whole, never trimmed.

Each `ext` may hold 2,048 bytes and each `inserter` or `matcher` 255 bytes, but those field caps do not guarantee the set cap. A set whose legal fields add up to more than 4,096 bytes is discarded whole.

A discarded set leaves the stored set as it is, as if the result had not named the source. A result all of whose sets are discarded counts as `Malformed`: the visitor backs off, and the result's removals still apply. The harness fails a fixture that hits any limit, so keep your test responses at realistic sizes.

### Replace or fill

What a result does depends on who owns the source:

- For a source the provider **owns** (the `owner` key of its `[[ec.partners]]` entry), a result replaces the source's whole entry set. An entry the next result omits is removed.
- For an **unowned** source, a result only fills an absent or expired record. It is never replaced by its writer, and browser values and other writers win.

A source your result does not name keeps its record either way.

### Vendor `ext` data and privacy

Vendor `ext` data is stored only in KV, never in the `ts-eids` cookie. It reaches bidders, the `x-ts-eids` response header and the `__Host-ts-eids-s` mirror under the same consent gates as the UIDs. The mirror is not `HttpOnly`, so every script on the page can read it. If you store segments or match metadata in `ext`, say exactly what they hold in your vendor spec. If a vendor's IDs must not reach page scripts, the operator sets the source's `bidstream_enabled = false` or keeps the provider off.

Never log UIDs, provider state, consent strings or URLs with a query string. Log errors with `current_context()` only.

## Private state

A provider declared `with_private_state()` can keep up to 1,024 bytes of JSON in the visitor's row, for things like a refresh token or a signature. Read it with `input.state::<T>()` and replace it with `Resolution::replacing_state(&value)` or `clearing_state()`.

- State lives in KV only. It is never sent to bidders, put in a cookie, or returned by the admin lookup (which shows `[redacted]`).
- A state that does not decode as `T` returns `InvalidState`, and the coordinator clears it so the next call starts fresh. Use a strict type: a `#[serde(default)]` type that decodes anything fails the harness.
- An oversized or unserializable replacement rejects the result's positive part whole (no ID, no state), while its removals still apply.
- A stateful result applies whole. If another writer stored a newer ID while your call was in flight, your ID, state and scheduling are all dropped together.

## Capture

A capture-only provider declares `with_captures(Captures::new().with_path(Method::POST, "/integrations/examplecapture/api/token"))` and no `resolves`, so it is never dispatched. Every capture path must lie under the registering integration's own proxy routes, or startup fails. It overrides one method:

```rust
fn capture(
    &self,
    input: &CaptureInput<'_>,
) -> Result<Resolution, Report<IdentityProviderError>> {
    let Ok(reply) = input.response().json::<TokenReply>() else {
        return Ok(Resolution::no_match());
    };
    Ok(Resolution::found(reply.tokens.into_iter().filter_map(|token| {
        let source = input.source_named(&token.source)?;
        Some(ResolvedEid::new(source, ResolvedUid::new(token.value)))
    })))
}
```

Capture is **revoke-only**. A capture result can do three things, and no more:

- Its removals (`invalidate`, `also_invalidate`) and an opt-out (`opted_out`) are written to KV.
- A positive result clears the provider's opt-out (the vendor issuing again after the visitor re-consented), provided consent allows it.
- Its positive IDs are only counted in the `capture_unpersisted` counter, by provider and source and never by value. They are not stored. The browser's own path carries them through ordinary browser ingest.

More rules:

- Capture never changes private state, and never touches backoff or the circuit breaker. A proxied call that held nothing is not a failure. A state change in a capture result is ignored with a warning, and the harness fails it.
- Return `Ok(Resolution::no_match())` for any body you do not recognize.
- `CaptureInput` never exposes request bodies or request headers.
- **Real-browser gate.** Removals, opt-outs and global revokes apply from any client, as consent withdrawals do. Only a clear needs a real browser: a positive result from a client that fails the real-browser check never clears an opt-out. A script replaying a copied `ts-ec` cookie through the proxy can therefore revoke for that visitor, as it could already withdraw consent, but cannot lift a revocation.
- **A capture-only provider must list its `sources`** in `[ec.identity.providers.<id>]`, and it **cannot own a source**. Its opt-out removes browser-ingested records of every listed source, so the operator names only the sources it is the delivery path for.
- `revoke_scope` controls how far a captured opt-out reaches (see [Configuration Reference](/guide/configuration#per-provider-keys)). With `"provider"` (the default) it removes only that provider's records and, for a capture-only provider, the records of its listed sources that no provider wrote. With `"global"`, a captured opt-out runs the consent-withdrawal tombstone, from any client.

## Registering a provider

An integration's `register` function registers a provider through the existing builder. `try_with_identity_provider` wraps a fallible constructor and turns a failure into `TrustedServerError::Integration`:

```rust
Ok(Some(
    IntegrationRegistration::builder(EXAMPLEID_INTEGRATION_ID)
        .try_with_identity_provider(ExampleIdProvider::new(&config))?
        .build(),
))
```

`with_identity_provider(Arc<dyn IdentityProvider>)` takes an already built provider. An integration can register more than one. Add a `validate_identity_integration` line for your config type next to the existing per-type list in the deploy validator, so `ts config validate` builds your descriptor.

Provider IDs match `^[a-z][a-z0-9_-]{0,62}$`, and two providers cannot share an ID. At most 32 providers can be enabled, generic pull partners included.

## Testing with the harness

`ProviderHarness` drives a provider the way the coordinator does, so vendor tests stay short. It lives in `trusted_server_core::ec::identity::test_support`, behind `cfg(any(test, feature = "test-utils"))`. A provider in its own crate enables `test-utils` as a dev-dependency feature of `trusted-server-core`. `ExampleIdProvider` is a crate-private fixture in the core crate, so the snippets in this guide are illustrative: substitute your own provider type and config.

```rust
let provider = ExampleIdProvider::new(&example_config()).expect("should build the provider");
let mut harness = ProviderHarness::new(&provider)
    .with_client_ip("192.0.2.10")
    .with_user_agent("ExampleBrowser/1.0");

let request = harness
    .request()
    .expect("should build a request")
    .expect("should not skip the request");
assert_eq!(
    request.url().host_str(),
    Some("api.exampleid.example.com"),
    "should call the declared host"
);

let resolution = harness
    .respond(StatusCode::OK, &json!({ "uid": "example-uid", "signature": "example-signature" }))
    .expect("should parse the response");
assert_eq!(
    resolution.eids()[0].uids()[0].id(),
    "example-uid",
    "should return the uid"
);
```

- `respond_raw(status, headers, bytes)` covers empty and non-JSON bodies. `request_json::<T>()` decodes the built body. `capture(&CaptureFixture)` runs a capture (build the fixture with `CaptureFixture::new(Method::POST, path).with_response(status, &json)`). `state_of::<T>(&resolution)` decodes the replacement state.
- `with_stored(StoredId::for_test(&source, "example-uid"))` makes a source a refresh. `HashedEmail::for_test` takes a `[u8; 32]` digest. Both are available under `test-utils`. `StoredId::with_eids` gives a refreshing provider a stored full set.
- The `Resolution` getters show what the provider set. Provenance defaults are applied only by `output_eids(&resolution)`, which returns the EIDs bidders would see after the section 4.5 rules and your descriptor's defaults.
- `check_conformance(&cases)` runs the checks every provider must pass; `check_conformance_with_state::<S>` also round-trips your state type. Both panic with a message naming the failure. `try_check_conformance` and `try_check_conformance_with_state::<S>` return `Result<(), Report<ConformanceFailure>>` instead (`ConformanceFailure` is `#[non_exhaustive]`). Use them to assert that a deliberately broken provider fails, because a panic aborts the whole `wasm32-wasip1` test run.
- `check_descriptor_ignores_secrets(&placeholder_config, &resolved_config)` fails if your descriptor depends on a secret.

`check_conformance` verifies that inputs hold only what the descriptor declares, that a resolver returns a request for at least one listed setup, that requests go only to declared hosts over HTTPS with valid headers and no `Cookie`, that entries are for declared sources and no fixture hits a limit, that expiries are reported and present for expiring sources, that nothing returns state without `with_private_state()`, that `capture` never changes state, and that a bare-JSON-string stored state is never accepted.

```rust
#[test]
fn example_provider_conforms() {
    let provider = ExampleIdProvider::new(&example_config()).expect("should build the provider");
    ProviderHarness::new(&provider).check_conformance_with_state::<ExampleIdState>(
        &ConformanceCases::default().with_resolve_case(
            HarnessSetup::default().with_client_ip("192.0.2.10").with_user_agent("ExampleBrowser/1.0"),
            StatusCode::OK,
            &json!({ "uid": "example-uid", "signature": "s" }),
        ),
    );
}
```

Add a fixture response for each outcome the vendor can return, and assert it through the `Resolution` getters.

## Next steps

- [Edge Cookies](/guide/edge-cookies) for the operator checklist and rollback steps
- [Configuration Reference](/guide/configuration#identity-providers-ec-identity) for every operator key
