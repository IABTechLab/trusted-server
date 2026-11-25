# Native EC persistence

Native EC persistence uses the shared core `RecordEcKvStore` over a prepared EdgeZero record binding. TS does not implement DynamoDB atomics, generation allocation or cursor decoding. The identity graph still owns consent, entry validation, collision handling and caller-specific conflict/recovery decisions.

Ordinary and record operations on that binding share value rows. An ordinary replacement clears metadata and invalidates old generations. Configuration and secret snapshots remain immutable; identity rows stay live.

```mermaid
flowchart TD
    Configure[Declare EC layout during App configuration] --> Prepare[EdgeZero prepares the selected record binding]
    Prepare --> Acquire[Acquire shared EC view without changing routing]
    Acquire --> Ready[Application ready]
    Ready --> Eligible{Eligible request with required consent?}
    Eligible -->|No| NoCookie[Do not issue an identity]
    Eligible -->|Yes| Persist[Await native conditional persistence]
    Persist -->|Acknowledged| Cookie[Finalize response and identity cookie]
    Persist -->|Failed or uncertain| NoCookie
```

## Select the prepared binding

Build with `aws-dynamodb-ec` or `celld-ec` for the corresponding default-off EdgeZero connector. Set `TRUSTED_SERVER__EC_RECORDS_ENABLED=true` at native boot to declare EC placement before provider preparation. This is a layout declaration, not a provider connection selector. Absence or `false` keeps that declaration off; malformed values fail startup.

Published application config selects the logical prepared store, not an endpoint or credentials:

```toml
[ec.storage]
provider = "records"
store = "trusted_server_kv"
```

A configured record reference must already have the declared layout and capability before initialization. A missing declaration or raw binding is an error, not permission to construct a provider late. Legacy `TRUSTED_SERVER__EC_DYNAMODB_ENABLED` selection is rejected with migration guidance. Omitted-provider and `provider="fastly"` compatibility references remain Fastly-only; Cloudflare and Spin reject unsupported EC storage during initialization.

Add this KV entry to the native store-bindings TOML already used for the application's config and secret stores:

```toml
version = 1

[kv.trusted_server_kv]
provider = "aws-dynamodb-records"
region = "us-east-1"
table = "example-retained-ec"
namespace = "example-staging"
```

Use the existing native binding-file selection mechanism. Keep its config/secret mappings in the same document. A raw `aws-dynamodb` binding does not satisfy the record requirement. Missing capability, a mismatched layout or failed selected-provider preparation is fatal before readiness, not permission to fall back to memory.

For celld, use the same logical ID with `provider = "celld-records"`, its endpoint/store and access policy in that native binding document. Run the EdgeZero-owned `services/celld-store` application on official, unmodified celld 0.6.3. The matching EdgeZero revision must provide record API version 1, scoped pagination and no-expiry support. Ordinary KV and records share its rows and retained namespace allocator. Old or KV-only services fail read-only preparation; no fork listener, downgrade or migration is used.

Region, table, namespace, credentials and endpoint behavior belong to EdgeZero's binding/provider configuration. The old `TS_EC_DYNAMODB_REGION`, `TS_EC_DYNAMODB_TABLE`, `TS_EC_DYNAMODB_NAMESPACE` and `TS_EC_DYNAMODB_LOCAL_ENDPOINT` do not configure another TS-owned provider. Tests use an explicitly isolated DynamoDB Local process with dummy credentials. No hosted IAM or deployment qualification is implied.

Fastly uses the same shared adapter and the logical `trusted_server_kv` declaration. Its compatibility `ec_store`/`provider="fastly"` references name logical IDs, not arbitrary physical store names. The application rejects undeclared IDs at startup. Request handling, response finalization and separately gated post-delivery pull sync retain the prepared handle; none reopens a store from settings. Link the desired physical Fastly store under that logical ID through the normal deployment workflow; these changes do not perform that operation.

## Key placement and generation lifetime

The application declares one stable `trusted-server-ec` layout:

- EC roots retain their full `<ip-hash>.<suffix>` keys and group by the complete `<ip-hash>.` prefix.
- Completion markers retain `__ts_ec_withdrawal_complete__:<ec-id>:<expiry>` and group by the marker namespace plus EC ID, ending in `:`.
- Layout resolution establishes placement. The graph still validates identity contents and marker expiry. A malformed marker is not silently accepted as proof of withdrawal.

DynamoDB uses a namespace/layout-qualified partition key for each declared group. Value and retained per-key generation rows have different sort-key kinds. There is no namespace-wide counter updated by every mutation. Add/CAS use native conditions, and deletion or TTL of a value must not reset its retained generation history. Failed conditional attempts may consume unused generations.

This is an explicit new record format, not a migration of the old `ec:{namespace}` and `ec-allocator:{namespace}` scheme. Reserve the table/namespace for the intended environment and retain allocator continuity through backup, restore and retirement. Do not restore an old allocator independently into an active dataset.

TTL visibility is provider-native. DynamoDB records remain visible until native physical deletion; TS consent and tombstone policy is not replaced by the TTL collector. No portable exact expiry deadline or new expiry envelope is added.

## Listing and errors

EC cluster counting supplies a complete hash partition, including its dot delimiter. Marker listing supplies its complete marker partition. The shared adapter follows native continuation through short or empty pages until the requested cap or authoritative end. Existing graph callers continue to reject saturated cleanup conservatively.

Strong partition Query and authoritative exact existence are separate from ordinary arbitrary-prefix/global Scan. An operator exposing ordinary global enumeration must account for its Scan cost and permission. Never scope a new policy to the obsolete single identity partition and global allocator.

Provider preparation and runtime operations use EdgeZero's bounded, single-attempt policies. Definitive Add/CAS rejection is normal contention. Ambiguous completion is an error, not a false rejection or an automatic replay. TS retains its explicit reread/merge/retry/defer policy; no new cookie is issued merely because an uncertain mutation might have committed.

## Eligibility and route boundaries

Persistence does not grant consent. Native eligibility uses checked client metadata and existing browser/navigation rules, not invented Fastly TLS or JA4 signals. Health checks, static assets, preflights and rejected authentication must not mint identities. Missing qualifying consent remains blocking. Synthetic local fixtures are not real publisher CMP acceptance.

Read-only identify does not create identities. Withdrawal remains writable for privacy-hardened clients, writes an authoritative tombstone, removes partner IDs and expires the browser cookie. Finalization is awaited before committing response headers.

Native admin EC management, native S2S batch parity with Fastly, and native post-send pull sync remain separate work.

## Executed local acceptance and remaining deployment work

The native lifecycle suite now runs through the shared adapter over real persistent records. It covers persistence-before-cookie, app recreation, consent/route exclusions, withdrawal, orphan recovery, cancellation, metadata/body coherence, stale tokens and native page traversal. The explicit `record_dynamodb` target exercises independent native Add/CAS, allocator-only Query pages, concurrent identity updates and withdrawal through DynamoDB Local.

The real celld fixture exercises route persistence, process restart, withdrawal, stale-token rejection, concurrent updates and outage through the same adapter. The production-binary tests prepare records from native bindings and the published logical selector, exercise actual HTTP issuance and withdrawal, restart against retained service state and refuse readiness on service outage. They require `CELLD_EC_TEST_BINARY` pointing to official celld and `EDGEZERO_CELLD_STORE_SERVICE` pointing to `services/celld-store` in the same SDK revision used by the Rust dependencies. Missing prerequisites fail rather than skip. The stock-celld workflow checks out that dependency tag, verifies the official runtime digest and executes all three ignored integration cases through a nonzero sentinel check. Historical modified-celld receipts do not prove this path.

Fastly's WASM/Viceroy suite exercises the shared lifecycle and keeps response-finalization and post-delivery permissions separate. The separate SDK Viceroy independent-CAS race has a known emulator atomicity failure, deferred from this stock-celld integration. Do not describe that failed check as passing or as production Fastly qualification.

These checks do not qualify a rebuilt application image, retained infrastructure, workload credentials, installed IAM/boundary policies, capacity, fleet failover, restore or real browser consent transport. The historical `test-native-ec-image.mjs` fixture targets the previous TS-owned backend and must be adapted before it can count as new-format image acceptance.

Review exact-table permissions, namespace/group scoping, retained state ownership and effective denies before any deployment. The application task role owns data access; do not move it to the execution role or remove unrelated boundaries. Do not add migration, apply infrastructure, publish images or deploy from these local test receipts.
