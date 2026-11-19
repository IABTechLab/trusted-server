# Adapter store inventory and wiring (issue #1139)

Current rework: [production Axum startup and unsupported KV](./2026-11-19-adapter-store-inventory-rework-design.md).
The baseline investigation below describes the original cutover; decisions and
acceptance now account for #1635. Historical review entries retain their context.

## Problem

Every adapter manifest declares stores. Several declarations have no working
path from the runtime binding to a consumer, and the guides describe behavior
the code does not have. The following was verified on the EdgeZero cutover
branch (`spec/edgezero-pr275-migration` at `47a539528`, EdgeZero pin
`f84ffbbb`).

1. **`trusted_server_kv` is declared but never reaches core.** `edgezero.toml`,
   the Fastly `Hooks::stores()`, `cloudflare.toml` and `wrangler.toml` declare
   it, yet every adapter hands core `UnavailableKvStore`:
   - **Fastly** declares the store, so EdgeZero puts a lazily opened handle into
     every request's `KvRegistry`. `build_per_request_services` ignores that
     handle and uses `AppState::default_kv_store`, which is always
     `UnavailableKvStore`. The managed deploy still makes operators select a
     physical store for it.
   - **Cloudflare and Spin** do not implement `Hooks::stores()`, so EdgeZero
     builds no registries and `ctx.kv_store_default()` and
     `ctx.config_store_default()` return `None`.
   - **Axum** constructs `UnavailableKvStore` unconditionally.

   No production code calls `RuntimeServices::kv_store()` today. A robots.txt
   answer cache (#1355, #1365) and an external integration both want it.

2. **Request-time config on Spin is a no-op.** Startup reads the app config
   from the `default` key-value label, and `spin.toml` tells operators to put
   request-signing metadata (`current-kid`, `active-kids`, public JWKs) in that
   same label. At request time, however, `build_runtime_services` gets no
   config handle and uses `NoopConfigStore`. As a result,
   `/.well-known/trusted-server.json`, `/verify-signature` and signed auction
   requests cannot read signing metadata on Spin.
3. **Request-time config on Cloudflare is a no-op as well.** No documented store
   holds signing metadata there. The API reference nevertheless marks discovery
   and `/verify-signature` as `real` on Cloudflare. In practice discovery
   answers the shared `500`, and `/verify-signature` answers `200` with
   `verified: false` (see `request_signing/endpoints.rs`).
4. **Stale and dead entries.**
   - `docs/guide/cloudflare.md` and `docs/guide/axum-dev.md` attribute
     unsupported EC lookup and key rotation to unwired request-time stores.
     Those features use other stores (`ec_identity_store`, `api-keys`), so the
     wiring here does not change them.
   - `crates/trusted-server-adapter-cloudflare/src/platform.rs` claims `run_app`
     injects stores "declared in `cloudflare.toml`". Nothing at runtime reads
     that file; only a test does.
   - `fastly.toml` declares a local `creative_store` KV store. The Viceroy
     integration template declares `creative_store`, `opid_store` and
     `ec_partner_store`. Nothing opens any of them, and `auction.creative_store`
     is deprecated.
   - Several guides, crate READMEs and `architecture.md` describe the KV and
     config wiring incorrectly. Task 6 of the plan lists them.

## Decisions

These decisions were recorded with the maintainer on 2026-11-18. D5 to D8
resolve design review findings within those decisions.

- **D1: Wire `trusted_server_kv` on Fastly, Cloudflare and Spin; leave Axum unsupported.**
  - Keep the declaration for identity storage. Axum deliberately remains
    `Unavailable` until #1587 → #1574 provide the EdgeZero-declared path.
    On the other adapters, when the binding exists, core's `kv_store()` reaches
    it. When it is absent, every request is still served and KV operations
    return a typed `KvError` (`Unavailable` or `OpenFailed`), as today. A
    missing store never fails startup or routing.
  - On Fastly, "optional" applies at runtime only: the managed deploy still
    links a physical store for every declared ID.
  - Why: the identity design makes record stores EdgeZero logical stores
    (ID-2), and pending work needs runtime KV.
  - Rejected alternatives:
    - Removal would contradict ID-2 and pending consumers.
    - Making the store required, as #1365 does, breaks every deployment that
      lacks the binding.
  - A comment on the issue says a declared Cloudflare store is required and
    fails before routing. That no longer holds at this EdgeZero pin, where
    declared Cloudflare and Spin KV handles open lazily.
- **D2: Spin request-time config reads the `default` label.**
  - Startup already reads this label, and `spin.toml` documents it as the home
    of the signing metadata.
  - Spin's config bridge ignores the requested store name, so every
    request-time config read is served from `default`. That covers
    `jwks_store`, and also DataDome's `datadome-ip-bypass` and any
    operator-named config store.
  - DataDome changes only in what is possible. A key missing from `default`
    fails with the same read error as before; an operator can now supply the
    bypass list there. The Spin guide documents this shared namespace and its
    key-collision risk.
- **D3: Request signing stays unsupported on Cloudflare.**
  - Documented results: discovery answers the shared `500`, and
    `/verify-signature` answers `200` with `verified: false`.
  - A Worker started with `request_signing.enabled = true` cannot sign outbound
    auction requests; the guide says so.
  - A Cloudflare config source for signing metadata would be a new operator
    contract and belongs in its own change.
- **D4: Remove the dead local stores.**
  - Remove `creative_store` from `fastly.toml`. Remove `creative_store`,
    `opid_store` and `ec_partner_store` from the integration Viceroy template.
  - Remove the local `counter_store` KV entry from both files too.
    `FastlyRateLimiter` opens `counter_store` as an Edge Rate Limiting counter,
    which is a separate Fastly service resource, not a KV store. Viceroy
    0.21.0 stubs rate counters, so the KV entry backed nothing. The
    integration config test now asserts the entry is absent.
- **D5: The Cloudflare KV binding stays `TRUSTED_SERVER_KV`, the binding the
  manifests already declare.**
  - The local smoke uses that namespace to hand the app-config envelope from
    `ts config push` to the operator, and the deploy guide reads the envelope
    from "the target KV namespace". The running
    Worker never reads config from KV (startup reads the
    `TRUSTED_SERVER_CONFIG` variable), so this is a staging copy.
  - The Cloudflare guide tells deployments to push config to a separate
    namespace. The smoke's handoff needs no reserved key, because it uses an
    isolated local namespace that its exit trap removes.
- **D6: Keep `cloudflare.toml` as a reference manifest.** A test pins its config
  ID to the runtime default. Add a header saying the Worker runtime does not
  read it, and correct the `platform.rs` comment that claims otherwise.
- **D7: Fastly operators must give `trusted_server_kv` a dedicated store.**
  Once the store is wired, generic KV writes land in it, so it must never be
  pointed at `ec_identity_store` or any other store. The Spin label override
  `EDGEZERO__STORES__KV__TRUSTED_SERVER_KV__NAME` must likewise never name
  `default`. Both guides say so.
- **D8: Real-binding evidence for KV is limited, because no consumer exists
  yet.**
  - Automated real-binding tests: the Fastly KV round trip under Viceroy, the
    Axum context-path unavailable contract and production prepared startup,
    and Spin discovery under
    `spin up`, which proves the `default` config path.
  - The Cloudflare and Spin KV bindings, the Spin signing private key, and the
    missing-binding cases are covered by runtime verification with a
    temporary probe build that is never committed.

## Design

### KV per adapter

| Adapter    | Binding source                                                                                                                                                                                                                                                                       | Absent binding                                      |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------- |
| Fastly     | `ctx.kv_store_default()`. EdgeZero registers it from the existing `Hooks::stores()` declaration and opens the store linked under `trusted_server_kv` on first use.                                                                                                                   | `KvError::OpenFailed`; requests are served.         |
| Cloudflare | `CloudflareKvStore::from_env(env, CLOUDFLARE_KV_BINDING)`, opened per request. The constant equals `wrangler.toml`'s `[[kv_namespaces]].binding`, and a test pins that. No `Hooks::stores()` is added, so no new `EDGEZERO__*` variable is needed.                                   | `UnavailableKvStore`, with one warning per isolate. |
| Spin       | `Hooks::stores()` declares only `kv: trusted_server_kv`. EdgeZero opens the label `trusted_server_kv` lazily and never opens `default`. `spin.toml` does not list the label; operators opt in through `key_value_stores` and runtime config. `ts provision` adds the label for them. | `KvError::OpenFailed`; requests are served.         |
| Axum       | No generic KV provider; prepared config and secrets are independent startup snapshots                                                                                                                                                                                                | `KvError::Unavailable`; requests are served         |

Axum declares config and secrets through `Hooks::stores()`, but no KV provider.
It loads these prepared stores once before listening; requests use immutable
snapshots. Adding an adapter-local file store would conflict with the shared
record backend and declared-store architecture.

One `KvHandle` to `PlatformKvStore` bridge (`KvHandleStore`) in core replaces
the private copies in the Cloudflare and Spin adapters, so Fastly does not add
a third.

### Spin config

`build_runtime_services` uses a `ConfigStoreHandleAdapter` over
`SpinConfigStore::for_label("default".to_owned())` on the wasm target, the
constructor startup uses. The host build keeps `NoopConfigStore`.

### Inventory

`docs/guide/edgezero.md` extends its logical-store table into a full inventory.
It has one row for each of the following:

- **Manifest declarations:**
  - `edgezero.toml`
  - Fastly `Hooks::stores()`
  - `fastly.toml` (`[local_server]`)
  - `wrangler.toml` and `wrangler.ci.toml`
  - `cloudflare.toml`
  - the `spin.toml` labels and variables
  - `axum.toml`
- **Fixed runtime names:**
  - `jwks_store` and `signing_keys`, and the operator-named key-rotation
    targets `request_signing.config_store_id` and
    `request_signing.secret_store_id`
  - `api-keys`
  - `counter_store`
  - `edgezero_logging`
  - `datadome-ip-bypass`
- **Operator-named stores:** `ec.ec_store` (`ec_identity_store` by default),
  DataDome `protection_excluded_ip_cidr_sources[].config_store`, and the
  deprecated `secret_store` selectors.
- **Cloudflare Worker secrets** read through `env.secret()`, which no manifest
  declares.
- **Axum prepared stores:** config and active secret references are captured
  once at startup; legacy environment bridges are not fallback inputs.

Each row states its purpose, its startup and request-time consumer on each
adapter, and the unsupported contract where one applies. The integration
Viceroy template's fixtures are listed as test-only.

## Acceptance

| Scenario                                                             | Before                                            | After                                                  | Evidence                                                                                                                                   |
| -------------------------------------------------------------------- | ------------------------------------------------- | ------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------ |
| Fastly with `trusted_server_kv` linked                               | `kv_store()` returns `Unavailable`                | Reads and writes reach the linked store                | Viceroy test: the registry is built from `TrustedServerApp::stores().kv`, and a write through the services is read back by a second handle |
| Fastly service without the store linked                              | `Unavailable`; request served                     | `OpenFailed`; request served                           | Runtime verification under Viceroy with the local store removed                                                                            |
| Cloudflare with and without `TRUSTED_SERVER_KV`                      | `Unavailable` both ways                           | Binding reached; absent binding gives `Unavailable`    | Test pinning the binding constant to `wrangler.toml`; runtime probe on `wrangler dev --local`                                              |
| Spin with and without the `trusted_server_kv` label                  | `Unavailable` both ways                           | Label reached; absent label gives `OpenFailed`         | Test that `stores()` declares only KV; runtime probe on `spin up`                                                                          |
| Axum production prepared startup                                     | Generic KV unavailable                            | Remains unavailable pending #1587 → #1574              | Context-path regression and production startup/page probes                                                                                 |
| Spin discovery with signing metadata in `default`                    | `500`                                             | `200` with the JWKS                                    | CI `spin_runtime_` integration test                                                                                                        |
| Spin signed auction with the private key in an encoded variable      | Fails (no `current-kid`)                          | Signed request reaches the bidder                      | Runtime verification with a stub bidder                                                                                                    |
| Cloudflare discovery and `/verify-signature`                         | `500` and `200 verified:false`, documented `real` | Same results, explained in the API reference and guide | Host route test, a documentation pin that cannot see the wasm path; runtime check on `wrangler dev`                                        |
| Every adapter without the new bindings: startup and publisher routes | Served                                            | Served, unchanged                                      | Existing suites and parity test; runtime publisher request on each adapter                                                                 |

## Out of scope

- Cloudflare request signing, key management and startup config (D3).
- EC record stores (EZI-2\*), and declaring the `counter_store` rate counter
  as a Fastly service resource.
- Any new consumer of `kv_store()`.

## Review log

- **Round 1, two independent agent reviews.** One checked feasibility against
  the source and the pinned EdgeZero; the other checked issue coverage. Their
  blocking findings, and the resolutions:
  - The Cloudflare KV binding doubles as the config-push handoff namespace
    (D5).
  - `/verify-signature` answers `200 verified:false`, not `500` (D3).
  - An unlinked-store Fastly test cannot be built, because EdgeZero's lazy
    handle is crate-private. It moved to runtime verification (D8).
  - The inventory missed several stores. It now covers every manifest entry,
    the fixed runtime names and the operator-named stores.
  - `cloudflare.toml` had no recorded decision (D6).
  - Spin config ignores store names, which affects DataDome (D2).
  - Spin needed real-binding evidence, now the CI `spin_runtime_` discovery
    test, and the Spin private key needed runtime evidence (D8).

  Their optional findings were also adopted:
  - Pin the binding name to both Wrangler manifests.
  - Build the Fastly registry from the declared ID.
  - Run Axum inline outside a Tokio runtime.
  - Test the Axum env var.
  - Drop registry branches that can never fire.
  - Document the dedicated-store requirement and the Spin label override
    (D7).
  - Remove the other dead fixtures (D4).
  - Correct the stale guides and READMEs.

- **Round 2, a fresh independent agent review of the revised spec and plan.**
  It raised one blocking finding: a `pub(crate)` binding constant is dead code
  on the host build. The constant is now `pub`. Its optional findings were
  also adopted:
  - cfg-gate the Spin config types.
  - Mark the Spin runtime test `#[ignore]` for the CI filter.
  - Document the `ts provision` label effect.
  - Add the key-rotation store IDs and the Worker secrets to the inventory.
  - Correct the Spin guide's CI note.
- **Round 3, two independent agent reviews of the opened PR.** One covered
  the runtime code, the other the tests, manifests and docs. Neither raised a
  blocking finding. The optional findings that were adopted:
  - Axum's store goes behind a `KvHandle`, so its key, value and TTL limits
    match the other adapters.
  - Axum opens the store during startup, not on the first request.
  - The dead local `counter_store` KV entry is removed and D4 corrected:
    `FastlyRateLimiter` uses a rate-counter resource, which Viceroy stubs.
  - A CI test covers the Spin signed auction, with the private key in an
    encoded variable.
  - Inventory rows, the Cloudflare guide and a test comment are corrected, and
    the request-signing guide links the inventory.

  Declined: opening the Cloudflare binding lazily. The per-request cost is a
  few JS property reads, and a lazy wrapper would be wasm-only code that host
  tests cannot run.

## Rework decision (2026-11-19)

The former Axum file-store decisions and tests in the historical review log
above are superseded. Keep `trusted_server_kv` for identity storage, leave Axum
unsupported until #1587 → #1574, and preserve #1635 prepared-store startup.
See the rework design and its review log for the current acceptance criteria.
