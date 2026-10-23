# Context-derived secret bindings

## Decision and status

For [issue #1037](https://github.com/IABTechLab/trusted-server/issues/1037), keep credentials as raw native secret values. Derive each physical secret key from its schema-owned purpose, actual identity, approved destination or route policy, and existing configured reference. Provisioning a value at that key authorizes that particular use. Changing an authorization-relevant input selects a different key, not the same credential with new permissions.

This is the selected design, not an implemented security fix. Implementation, migration tooling, and native runtime proof remain required before the issue can close.

The alternative considered was a protected JSON record containing the credential and its authorization, selected by new slot and owner IDs. Both approaches can enforce the required boundary. Context-derived bindings avoid a second stored policy representation, a credential-record parser, and new configuration identifiers. They instead make deterministic naming and normalization a provisioning compatibility contract. Operators need a reviewed secret-free binding plan to explain otherwise opaque names and retire former authorizations.

## Issue and investigation base

The issue requires field-owned secret scopes on Fastly, Axum, Cloudflare, and Spin. It separately requires preventing cross-purpose secret substitution and preventing mutable-config retargeting of legitimate pull credentials. It requires migration guidance and config-store-only attacker tests, and excludes a new config-envelope signature/authentication layer.

The issue was read in full on 2026-10-16, including its empty comments collection. It was open with `updatedAt=2026-10-16T20:01:20Z`. Investigation base is `5bc38862512c5c71085b5052bc3e528ee9b9987d`.

Source observations at that revision:

| Source under `crates/`                                                                                         | Current behavior relevant to this design                                                                                                                                                          |
| -------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `trusted-server-core/src/secret_resolution.rs`, `resolve_leaf`                                                 | Passes configured strings directly to the default secret store. Resolution clones and commits only on success; native lookup errors are suppressed.                                               |
| `trusted-server-core/src/config_payload.rs`, `settings_from_config_blob`                                       | Verifies the checksum, removes inactive references, resolves secrets, then constructs and validates settings.                                                                                     |
| `trusted-server-core/src/config.rs`, `TrustedServerAppConfig`                                                  | Normalizes during deserialization and describes twelve secret paths without authorization context.                                                                                                |
| `trusted-server-core/src/ec/registry.rs`, `validate_config_for_deploy`, `from_config`                          | Deploy validation rejects reused inbound reference strings; runtime separately rejects duplicate resolved token hashes. Pull validation requires its mutable allowlist.                           |
| `trusted-server-core/src/settings.rs`                                                                          | S3 defaults and aliases, Tinybird normalization, and effective asset-route query policy must be materialized before derivation. Existing trimming must not modify labels or resolved credentials. |
| `trusted-server-core/src/ec/pull_sync.rs`, `dispatch_pull_sync`                                                | Uses loose URL/token fields, inherits mutable proxy TLS policy, creates a backend before building the Bearer header, and has a request-builder `expect`.                                          |
| `trusted-server-core/src/integrations/datadome/protection.rs`; `trusted-server-adapter-fastly/src/tinybird.rs` | Export credentials to config-derived targets. Tinybird currently drops its pending handle after starting a fire-and-forget send.                                                                  |
| `trusted-server-core/src/proxy.rs`; `trusted-server-core/src/s3_sigv4.rs`                                      | S3 exports its access ID and optional session token, signs the final target, and has signing/preflight paths beyond the primary GET.                                                              |
| Adapter `app.rs` files; `trusted-server-adapter-fastly/src/main.rs`                                            | Settings-only builders, registry construction, retained state, diagnostic loads, and finalization fallback reloads all need the loaded-use boundary.                                              |

Pinned EdgeZero v0.0.9 at `f7176742d46aa4d1a04ce4588b606afda388fd30` exposes only path, kind, and optionality in secret metadata. Its CLI walks raw references in two places, and its Spin validator applies native-looking syntax and lowercased duplicate checks to those references. A companion lookup-plan change is required below.

These are source observations, not executable security proof. An isolated fictional encoding model supports the naming contract, but no shipping Rust implementation or provider runtime has verified it.

## Threat model and invariant

The attacker can replace the app-config blob, change ordinary fields, and recompute a valid checksum. Assume they know native key names and complete provisioned identities. They cannot change the executable, trusted deployment mappings, native secret contents, native bindings, or their access controls.

For every active secret field, core determines its purpose and authorization context. The only allowed lookup is the derived key for that context. An outbound credential is sent only using the same compiled target that determined its lookup. A missing or invalid binding fails loading before backend creation or outbound requests.

The security authority is the set of native keys provisioned by a trusted operator. The hash is not a secret, signature, encryption, or authentication of config writers. Calculating another key name grants nothing without a credential provisioned there.

This protects against config-only reassignment, not malicious code with process-wide secret-store access. It does not provide general config integrity, availability, immutable partner membership, or immediate revocation in retained states. Selecting another complete provisioned identity remains allowed. Secret-store writers, deployment administrators, compromised approved endpoints, trusted DNS/CA compromise, and credential values deliberately reused across separately authorized uses are outside the guarantee. Operators must not provision another role's credential value into an outbound binding.

## Config contract

Keep existing secret field names and reference strings. A reference such as `partner_pull_token` becomes an opaque label, not a physical key. It must be a nonempty, non-whitespace-only string. Preserve its bytes exactly; do not trim, case-fold, parse a prefix, or interpret a generated-looking name specially. Labels need not satisfy native secret-name grammar because they are never native names.

```toml
[[ec.partners]]
name = "Example partner"
source_domain = "partner.example.com"
api_token = "partner_api_token"
pull_sync_enabled = true
ts_pull_token = "partner_pull_token"
pull_sync_url = "https://sync.example.com/pull"
pull_sync_allowed_domains = ["sync.example.com"]
```

No `slot:` marker, `secret_owner_id`, physical binding override, or per-field store selector is introduced. The same label may be used in different scopes or identities. Supplying a complete derived key as the label just derives another key; it never bypasses derivation.

Retain existing optionality and ordinary identity validation. Disabled Tinybird, disabled DataDome Protection/bypass, and disabled pull sync do not resolve stale references. Absent/null optional leaves skip lookup; malformed present structures remain errors. An omitted partner array is empty. Normalize boolean/string pull activation consistently without treating `"false"` as true. Deploy validation, lookup planning, and runtime loading must share these activation rules.

Materialize defaults before planning lookups. S3 defaults remain the existing labels `access_key_id` and `secret_access_key`, resolved through separate contexts. Session token remains absent by default. `s3_sig_v4` and `s3_sigv4` use identical policy. Deprecated store selectors remain deserialize-only, ignored, and safely warned about. Tinybird's dormant `access_token_secret` remains discarded and `access_enabled=true` remains rejected.

Replace obsolete reference-based validation. `PartnerRegistry::validate_config_for_deploy` must no longer reject two inbound labels merely because their strings match when their compiled identities differ. Validate compiled lookup identity instead. Keep duplicate normalized partner-domain rejection and runtime duplicate resolved-token-hash rejection. Replace the mandatory mutable pull-allowlist rule with the endpoint assertion contract below.

Correct existing guide statements describing `trusted_client_ip.shared_secret` as inline during implementation. Runtime already resolves it from the store; this is not an additional inline-to-store migration.

## Deterministic naming contract

One TS-owned policy module compiles each active reference into a private request containing a fixed executable scope code, the exact label, a normalized subject vector, and a normalized export vector. Only core constructs this request. Adapters map native identities and perform lookup, not authorization.

Derive EdgeZero metadata and the CLI lookup plan from the same descriptors. Unknown secret metadata or unsupported descriptor kinds fail closed. Keep the generic `PlatformSecretStore` interface for native access and separate signing/management uses; do not create an authorization engine in each adapter.

### Encoding and versioning

Use full SHA-256, not a truncated digest or language-dependent hash:

```text
LP(s)    = u32 big-endian UTF-8 byte length || UTF-8 bytes
VEC(xs)  = u32 big-endian element count || LP(x1) || ... || LP(xn)

payload  = ASCII("trusted-server-secret-binding-v1\0")
           || LP(scope)
           || LP(reference_label)
           || VEC(subject)
           || VEC(export)

key      = "ts1_" || lowercase_unpadded_RFC4648_base32(SHA256(payload))
```

The domain separator ends with one NUL byte. The base32 alphabet is `abcdefghijklmnopqrstuvwxyz234567`. The complete key is 56 ASCII bytes. Check every string byte length and vector element count before conversion to u32; overflow is a safe configuration error, never truncation. Use UTF-8 byte lengths, not character counts.

Element counts and length prefixes distinguish all components, including empty optional values. Do not substitute delimiter-concatenated strings, generic object serialization, array indexes, or provider-specific hashing. The restricted key alphabet avoids Axum case/replacement aliases. Spin must still apply its existing native store/key mapping.

The encoding, subject/export shapes, defaults, and normalization rules together form the v1 provisioning ABI. Pin shared golden fixtures before integration, including every scope, optional markers, Unicode labels, and accepted/rejected normalization cases. CLI plans, unresolved serialization, runtime loading, and actual send targets must agree. A library upgrade must not silently change v1 names or expand endpoint equivalence. Intentional incompatible changes require a new version/namespace and migration, not an old-key retry.

One fictional model vector to confirm independently in the shipping implementation is:

```text
scope:    ecpull
label:    shared
subject:  ["partner", "partner.example.com"]
export:   ["pull", "https://sync.example.com/pull"]
key:      ts1_ais2qpyk56tzecpap4de43vv7yedwcm2wo2tf5udw3chx46rsepq
```

A matching model vector proves only those encoding bytes, not native compatibility, collision resistance, or exporter confinement.

### Complete context inventory

| Secret path                                                           | Fixed scope | Subject | Export                      | Activation                                                         |
| --------------------------------------------------------------------- | ----------- | ------- | --------------------------- | ------------------------------------------------------------------ |
| `publisher.proxy_secret`                                              | `proxy`     | App     | None                        | Required                                                           |
| `ec.passphrase`                                                       | `ecpass`    | App     | None                        | Required                                                           |
| `ec.partners[*].api_token`                                            | `ecin`      | Partner | None                        | Optional; absence disables inbound authentication for that partner |
| `ec.partners[*].ts_pull_token`                                        | `ecpull`    | Partner | Pull endpoint               | Required when pull sync is enabled                                 |
| `handlers[*].password`                                                | `handler`   | Handler | None                        | Required for each configured handler                               |
| `trusted_client_ip.shared_secret`                                     | `clientip`  | App     | None                        | Required when trusted-client-IP configuration exists               |
| `tinybird.auction_token_secret`                                       | `tinybird`  | App     | Events endpoint and dataset | Required when Tinybird auction telemetry is enabled                |
| `integrations.datadome.server_side_key_secret_name`                   | `ddserver`  | App     | Protection endpoint         | Required when integration and Protection are enabled               |
| `integrations.datadome.protection_test_bypass.credential_secret_name` | `ddbypass`  | App     | None                        | Required when integration, Protection, and bypass are enabled      |
| `proxy.asset_routes[*].auth.access_key_id`                            | `s3id`      | Route   | S3                          | Required for S3 auth                                               |
| `proxy.asset_routes[*].auth.secret_access_key`                        | `s3secret`  | Route   | S3                          | Required for S3 auth                                               |
| `proxy.asset_routes[*].auth.session_token`                            | `s3sess`    | Route   | S3                          | Optional for S3 auth                                               |

Vectors have these fixed shapes:

```text
App:       ["app"]
Partner:   ["partner", normalized_source_domain]
Handler:   ["handler", exact_validated_path_regex, exact_username]
Route:     ["route", normalized_prefix, normalized_https_origin,
            pattern_presence, pattern_value, target_presence, target_value,
            normalized_region, effective_origin_query]

None:      ["none"]
Pull:      ["pull", normalized_endpoint]
DataDome:  ["datadome", normalized_validate_request_endpoint]
Tinybird:  ["tinybird", normalized_events_endpoint, exact_dataset]
S3:        ["s3"]
```

For optional route pattern/target fields, presence is the literal `absent` or `present`; absence has an empty value component. Encode effective query policy as the literal `preserve` or `strip`. Use effective normalized route values after defaults, not raw aliases. Partner domains use the existing normalized source-domain function. Handler paths use exact validated regex strings, not regex equivalence. Dataset must be nonempty and is not silently trimmed into another value.

The subject is the identity, so there is no separately mutable owner ID. Reordering entries leaves keys unchanged. Changing a handler username, partner domain, or authenticated route policy changes keys. Equal purpose, subject, export, and label intentionally share one binding; existing collection uniqueness rules still apply. Copying a whole provisioned identity selects that identity, not another identity's credential.

All three S3 fields use the same compiled route subject, but distinct scopes. Bind prefix, origin, regex/rewrite policy, region, and effective query policy. Request-dependent object paths and permitted incoming queries remain dynamic; this is not a bucket/object authorization system. Image-optimizer behavior must not change the bound effective query policy. Any future send-affecting field requires a context review, not an assumption that the current inventory remains complete.

## Destination and send policy

The issue explicitly requires pull recipient protection. This design also retains protection for DataDome's own server key, Tinybird's own append token/dataset, and S3's own route credentials. Those are verified sibling export paths, broader than the literal pull-specific acceptance criterion. Do not silently remove them or claim that cross-scope names alone prevent their own-credential retargeting. S3 route/signing integration should be its own testable milestone.

### Endpoint normalization

For pull, DataDome, and Tinybird base endpoints, use one shared core parser:

- Require absolute HTTPS with an ASCII DNS hostname. Reject IP literals, internationalized host input, trailing-dot hosts, userinfo including empty userinfo, queries including empty `?`, fragments, backslashes, whitespace, and controls.
- Normalize scheme/host case and remove explicit port 443. Preserve other valid ports. An empty path becomes `/`.
- Paths contain only ASCII unreserved characters and `/`. Reject percent escapes and `.` or `..` segments before URL parsing can discard them. Preserve path case, trailing slash, and repeated slash distinctions.
- Hash and use the complete normalized scheme, hostname, port, and path. Host-only approval, suffix matches, and mutable allowlists are insufficient.

The same parser supplies both derivation inputs and actual exporter targets. Never hash one representation and rebuild the target from loose settings later.

### Export-specific rules

- Pull uses GET and appends exactly one validated runtime `ec_id` through URL query APIs. Config cannot supply fixed query parameters. The legacy allowlist may be omitted; a nonempty allowlist may contain only the endpoint's normalized hostname. It is a consistency assertion, not authority. Changing URL and allowlist together selects a new, unprovisioned key.
- DataDome binds the exact derived `/validate-request` endpoint and uses POST with a safely form-encoded credential.
- Tinybird binds the exact HTTPS `/v0/events` endpoint and dataset, then builds one encoded `name` query from that dataset. It uses POST NDJSON. Actual event emission remains Fastly-specific unless another adapter already implements it; this work does not add telemetry exporters.
- S3 binds an HTTPS origin and route policy. Apply the same hostname/port checks and existing prohibition on origin paths/queries. Check final target origin before signing or backend creation. GET/HEAD SigV4 only. Host overrides and general proxy rewriting cannot change the bound authority. Unauthenticated routes retain their existing behavior.

All credential-bearing flows require certificate and hostname verification independently of `proxy.certificate_check`. Backend reuse must not inherit weaker TLS settings. Each HTTP operation sends once and never follows redirects, even on the same host. Legitimate S3 HEAD/GET preflight operations may remain separate operations, all confined to the same bound origin; "one send" does not prohibit that existing flow.

Exporters that observe a response treat 3xx as unsuccessful and never retry at `Location`. Preserve Tinybird's existing fire-and-forget behavior in this change. Its guarantee is no automatic redirect following, not observed classification of a response whose pending handle is dropped. Claiming observed delivery/3xx failure would require a separately specified response-observation change.

Validate header, form, and signing representation before backend creation. Pull and Tinybird require nonempty RFC 6750 b64token values plus existing length/placeholder checks. Build sensitive headers without panics. Safely encode DataDome form data, validate S3 header material, and redact access IDs as well as secret/session keys. Preserve credential bytes exactly. Native provider value limits still apply; there is no new JSON-record size limit.

## Loading and runtime boundary

### Unresolved and resolved normalization

Separate reference/config normalization from credential handling. Validate original endpoint syntax before URL parsing or trimming can erase forbidden input. Preserve label bytes in `TrustedServerAppConfig::deserialize` and unresolved serialization. Materialize S3 defaults and effective route/query policy before planning; never hash an incomplete raw tree.

After lookup, preserve credential bytes through settings construction, runtime preparation, and exporter construction. Address current S3 normalization, Tinybird label/dataset normalization, DataDome startup normalization, and repeated `prepare_runtime` calls explicitly. Existing normalization cannot be reused unchanged if it trims a resolved credential or changes the compiled target. Apply activation consistently before secret reads without letting inactive cleanup hide malformed present structures.

### Private construction and snapshots

The loader returns a nonserializable `LoadedConfig` containing settings and outbound bindings as one successful snapshot. Its fields are private; access is read-only. A private resolved-use representation couples credentials, purpose/subject association, compiled target, and fixed send policy. Only successful core loading constructs enabled uses. No four separately named public exporter types are required.

Registry/state builders and exporter constructors consume loaded bindings, not loose credential/URL fields in plain `Settings`. A settings-only helper may construct disabled exporters, but cannot enable credential export without a validated use. Resolved-settings shadow fields may remain temporarily for runtime validation, with redaction, but are not outbound authority. CLI serialization uses only unresolved config; loaded objects never serialize into envelopes.

Prove this boundary first through a thin production load/pull-export path before substantial sibling integration. Cover all adapter state builders, registry rebuilds, retained Fastly state, diagnostic loads, Fastly fallback finalization, Tinybird construction, DataDome constructors, and S3 signing/image-optimizer preflight branches. Do not leave private construction as an unenforced convention.

After checksum verification, validate unresolved structure, materialize defaults, and compile active requests and targets. Read only derived keys in the trusted mapped default store, validate values and ordinary settings, then atomically publish the snapshot. Preserve existing successful-state retention, but never combine settings from one load with credentials/targets from another. An in-flight request retains its captured state. No global credential cache, watcher, TTL, or new reload mechanism is introduced. Memoization is limited to identical requests within one load.

```mermaid
flowchart TD
    A[Untrusted config with valid checksum] --> B[Validate and compile purpose identity and target]
    B --> C[Derive reserved native key]
    P[Trusted operator provisions raw credential] --> D[Native secret lookup]
    C --> D
    D -->|Present and valid| E[Publish credential and compiled target together]
    D -->|Missing or invalid| X[Reject load with zero backend and HTTP calls]
    E --> F[Verified TLS and one send per operation to the bound target]
```

Errors expose only safe schema paths and fixed categories. Do not attach labels, native lookup diagnostics, credentials, URLs/queries, usernames, request bodies, or header values. Audit `Debug`, traces, request-builder errors, panic paths, form payload logging, and header sensitivity. No raw-key retry, alternate-store retry, plaintext fallback, or prefix/suffix bypass exists.

Signing uses its existing fixed signing-store identity and separate `current-kid`; management uses its fixed credential identity and API host. Keep these separately audited paths outside app-config derivation. Do not redesign signing management or accidentally route app-config fields through a generic raw-reference extractor.

## Native provisioning and CLI

Keep the logical store `trusted_server_secrets`. Trusted runtime/deployment mappings select its physical store. Separate field stores are unnecessary and would not isolate Cloudflare, whose TS adapter ignores the store argument.

| Adapter    | Provisioning and proof                                                                                                                                                                                                                                                                                              |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Fastly     | Raw credential at the derived key in the trusted mapped store. Preserve startup and reload mappings. Verify actual Viceroy reads plus isolated platform smoke and TLS/backend reuse.                                                                                                                                |
| Axum       | Raw credential in the environment variable produced by TS's store/key mapping, which uppercases and replaces hyphen/dot/space. Verify real environment reads and final-name collisions with fixed trusted store identity.                                                                                           |
| Cloudflare | Raw Worker secret at the derived key. Verify actual Worker binding lookup, HTTPS/no-follow behavior, and a config-only publishing role without Worker deployment/secret rights. A constrained publishing service may be necessary. Host tests do not execute `worker::Env`.                                         |
| Spin       | Raw value supplied securely to the application/component variable produced by TS's existing store/key escaping. Declare both levels and test actual component lookup. `secret=true` alone is not encrypted storage; use a protected provider. Keep one logical store to avoid the current single-secret-store gate. |

A host-usable renderer must share TS adapters' actual native mapping implementation or prove equivalence. EdgeZero's generic secret-store behavior is not interchangeable with these TS bridges. Reserve the generated namespace and reject final native collisions with other declarations, including legacy/signing/management bindings where they share a flat namespace. Do not silently truncate, rename, or merge distinct contexts. Deduplicate only after confirming exact request and store/key equality.

### Lookup-plan prerequisite

The pinned EdgeZero CLI's `run_adapter_typed_checks` and `typed_secret_checks` both walk raw leaves; validate, push, and diff consume those paths. Its Spin adapter lowercases those leaves, validates native-looking names, and rejects duplicates without TS's store/key escaping. Old labels passing this check would still be validation of the wrong names.

Add a default-compatible opt-in hook returning active `field_path`, `store_id`, and derived key. The exact public API can follow upstream conventions. For TS plans, reconcile both raw walkers with active/defaulted compiled lookups while retaining metadata and logical store declaration validation. Native validation uses generated lookups and final TS rendering; serialized labels remain unchanged. Other applications retain existing behavior when the hook is absent. Do not disable Spin validation globally.

Before substantial exporter/tooling work, demonstrate the hook through real validate/push/diff consumers and TS's host-usable renderer with fictional data. Cover omitted optional fields, defaults, inactive cleanup, metadata completeness, repeated identical requests, distinct contexts, and final binding collisions. Publish and pin the companion dependency before claiming CLI parity.

The 56-byte key does not prove provider acceptance. Under the inspected TS Spin mapping, its name with `trusted_server_secrets` is 92 bytes. Verify actual generated names, declarations, value-size limits, secure provisioning, and config-writer IAM separation in isolated native environments. Unavailable environments remain blockers, not assumed passes.

### First-release operator tooling

Generate a secret-free binding plan from unresolved config using the shared core compiler. Include schema paths, normalized subject/export contexts, derived keys, final native identities/declarations, and the reviewed config/version identity. Retain the reviewed plan as access-controlled operational configuration metadata; it is not a credential inventory service. It must contain no credential values and must make policy changes and retirement reviewable.

Native provider tools perform writes/deletes through secure operator input. Reuse existing CLI facilities where possible. Do not add record-validation commands, provider write orchestration, credential discovery, automatic rotation, or persistent runtime authorization/inventory state. Never print values in normal output, command arguments, diffs, or committed generated files.

## Migration, rotation, and retirement

1. Inventory active uses and approved subjects/destinations without printing values. Separate cross-role credential reuse and verify config writers cannot modify native secrets or deployment bindings.
2. Generate the plan from reviewed existing config. A trusted operator approves its normalized contexts. Blindly provisioning a plan derived from attacker-edited live config would grant the attacker new authority.
3. Copy existing raw values to derived native names through secure provider input. Secret contents and existing reference strings need not change. Update declarations, examples/local fixtures, and retain the reviewed secret-free plan.
4. Deploy to an isolated version, exercise valid and attacker-modified blobs, and confirm newly created state uses derived bindings. Switch traffic only after release gates pass.
5. Remove legacy raw-name entries after an explicitly owned rollback window. Revoke credentials at recipients if earlier exposure is suspected.

For an endpoint, subject, label, or derivation-version change:

1. Review the old/new plans and provision the new binding/declarations.
2. Switch config, recreate state, and verify the new authorized flow.
3. Remove every retired key/declaration within an owned, time-bounded window using native tools. The old context remains authorized for fresh loads while its key exists; provisioning a new key does not revoke it.
4. Replace retained old states and account for in-flight requests. Verify that replaying retired config fails on newly created state. Revoke at external recipients when exposure is suspected.

Ordinary value rotation at an unchanged context updates the raw value at the same name and requires state recreation. For coordinated S3 changes, stage new labels across all credential scopes, provision them first, then change config references together. Native multi-key updates are not a transaction.

Axum normally requires process restart. Fastly requires new state/sandboxes, including reusable-sandbox mode. Cloudflare and Spin follow existing runner app-state construction. No immediate warm-instance refresh or global revocation guarantee is added.

Unchanged labels can preserve the config shape during mixed rollout while old and new native names coexist, but old binaries remain vulnerable. Strict endpoint rules can still force config changes. Rollback requires retained legacy names and restores the original attack. Prefer rolling forward or disabling affected exporters. The new binary never reads legacy names.

## Acceptance and proof contract

These are required executable results, not passing claims:

| Issue criterion                                           | Decision                                                                                 | Required evidence                                                                                                                                                                 |
| --------------------------------------------------------- | ---------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| No unrestricted physical key from app config              | Typed contexts and derived keys only                                                     | Spy-store coverage of all twelve paths; arbitrary raw/generated-key labels never become direct lookups                                                                            |
| Consistent scopes on all four adapters                    | Shared descriptors/encoder; trusted native mapping                                       | Golden vectors, native name/read tests, then Viceroy/Fastly, Axum HTTPS, Wrangler/workerd, and Spin smoke                                                                         |
| Unrelated values cannot become pull Bearer tokens         | Scope is in the hashed context                                                           | Substitute publisher, inbound, handler, and sibling-role labels with valid checksums; no unrelated value returned, backend created, or HTTP send                                  |
| Blob edits cannot redirect a pull credential              | Exact subject/endpoint are in the key; fixed TLS/no-follow policy                        | Change URL and allowlist together, host/port/path, syntax, and TLS toggle; no lookup of the old key and zero sends. Approved 3xx sends once per operation and never to `Location` |
| Migration documents provisioning and required moves       | Raw-value copy, approved plans, native declarations, retirement, unsafe rollback warning | Fictional four-adapter operator walkthrough, including retired-context replay                                                                                                     |
| Config-only attacker rejected before any outbound request | Atomic loading and private resolved-use construction                                     | Production envelope reconstruction with valid-checksum mutations, backend/HTTP counters, and native/runtime negative sends                                                        |

Also test:

- Field reordering, label reuse across contexts, complete authorized identity selection, generated-name labels, framing boundaries, UTF-8 lengths, checked integer conversion, optional markers, and native declaration collisions.
- Malformed/optional/inactive structures, S3 defaults/auth alias/effective query policy, partner normalization, preserved resolved-token collision checks, handler subject changes, DataDome/Tinybird retargeting, and every bound S3 route-policy mutation.
- Exact label/credential-byte preservation through all normalization and repeated runtime preparation; schema-only safe diagnostics with fictional sentinel values.
- CLI validate/push/diff and unchanged unresolved serialization; default-compatible behavior for other EdgeZero users; actual send method/authority/path/query equal to compiled context, including Host/SNI and Tinybird dataset encoding.
- Mandatory TLS despite proxy toggles/backend reuse, no credential-bearing redirect following, DataDome form encoding, all S3 signing/HEAD/GET preflight branches, and Tinybird no-follow without claiming observed responses.
- State recreation and in-flight snapshots, same-name value rotation, staged S3 rotation, policy change/retirement, replay rejection, and rollback behavior.

Two high-risk facts remain unproven: every export-capable path can be confined to its loaded credential/target snapshot, and every native mapping/transport preserves that authorization. The cheapest next proofs are the production-loader/store/backend spy slice and the actual EdgeZero/TS renderer path, followed by native lookup and send tests.

Release requires the published dependency pin, migration walkthrough, real native reads, isolated valid/invalid HTTPS and redirect tests on all four adapters, and verified config-writer IAM separation. Host routes using Noop stores, source inspection, and a toy hash demonstration do not meet those gates. A missing provider environment is a reported blocker, not a pass.
