# Secret-Store Backed Config References Design

> **Status:** Proposal
> **Issue:** [#684](https://github.com/IABTechLab/trusted-server/issues/684)
> **Scope:** Secret-bearing application config values, runtime resolution policy, and provisioning validation
> **Date:** May 2026

## Summary

Trusted Server currently represents several secret-bearing application settings as inline string values in `trusted-server.toml`. `Redacted<String>` prevents accidental display in logs and debug output, but it does not express that a value should be backed by a provider secret store, and it does not prevent plaintext secret material from being uploaded as part of the canonical runtime configuration payload.

This design introduces a first-class secret reference model for application config fields that should be backed by a provider secret store.

In v1:

- Secret-bearing config fields may be authored either as inline strings or as secret refs.
- Inline strings remain supported for local development, tests, and migration workflows.
- Fastly production provisioning rejects inline secret values for the v1 secret-bearing fields.
- Production provisioning verifies referenced secret stores and secret items exist before applying config.
- Runtime resolves secret refs eagerly for each loaded config snapshot, but missing runtime secrets do not make the entire service fail to load.
- Missing runtime secrets are logged and cause only the affected capability to fail closed or degrade.
- Secret refs participate in the config hash; resolved secret values do not.

This proposal intentionally keeps v1 narrow. It does not add automatic secret generation, handler IDs, or request-signing migration into the generic secret-ref model.

## Scope

### In scope

- A first-class `SecretString` / `SecretRef` representation for selected application config fields.
- TOML syntax for inline secrets and secret refs.
- Provider default secret-store configuration for Fastly provisioning.
- A fixed Fastly runtime alias for the default application secret store.
- Runtime secret-ref resolution semantics.
- Feature-scoped runtime fail-closed behavior for missing secrets.
- CLI/provisioning validation that rejects inline production secrets and missing referenced secrets.
- Config-hash behavior for secret refs.
- Initial v1 secret-bearing field list.

### Out of scope

- Automatic generation of `publisher.proxy_secret`, `edge_cookie.secret_key`, handler passwords, request-signing keys, or runtime API tokens.
- Adding `id` to `[[handlers]]`.
- Moving request-signing private keys or runtime API tokens into the generic `SecretString` model.
- Storing `handlers.username` in a secret store.
- Hot reload or background refresh of resolved secrets.
- Cross-provider secret-store resource design beyond the Fastly provider shape needed for v1.
- A derive macro or annotation system for secret field descriptors.
- Rewriting local authoring files automatically.

## Current state

Several application settings are secret material but are currently authored directly in `trusted-server.toml`:

```toml
[publisher]
proxy_secret = "..."

[edge_cookie]
secret_key = "..."

[[handlers]]
username = "..."
password = "..."
```

Known semantics today:

- `publisher.proxy_secret` is secret material.
- `edge_cookie.secret_key` is secret material.
- `handlers.password` is secret material.
- `handlers.username` may be sensitive and should remain redacted, but it is not secret-store material for v1.
- Request-signing private keys already live in `signing_keys` and are out of scope for v1.
- The Fastly runtime API token already lives in `api-keys/api_key` for request-signing rotation and is out of scope for v1.

The config model uses `Redacted<String>` for these values. Redaction protects display surfaces, but it does not distinguish local inline development values from production secret-store references.

## Goals

- Make secret-store-backed config values explicit in the schema.
- Keep local development simple by allowing inline dev secrets.
- Prevent production provisioning from uploading inline secret material in canonical runtime config.
- Let production provisioning verify secret-store readiness before apply.
- Keep runtime availability resilient if a secret is deleted or corrupted out-of-band after provisioning.
- Ensure affected runtime behavior fails closed rather than bypassing protections.
- Keep the first implementation small and understandable.

## Non-goals

- Requiring secret refs for all local development workflows.
- Inferring default secret refs from omitted fields.
- Automatically generating or rotating secret values.
- Introducing a broad secret-management CLI surface in v1.
- Reworking request-signing storage in v1.
- Designing a multi-provider secrets abstraction beyond the existing platform secret-store interface.

## V1 secret-bearing fields

V1 applies the secret-ref model to these application config fields:

| Field                    | V1 behavior                                                   | Default shorthand key recommendation |
| ------------------------ | ------------------------------------------------------------- | ------------------------------------ |
| `publisher.proxy_secret` | Inline for local/dev; ref required by production provisioning | `publisher/proxy_secret`             |
| `edge_cookie.secret_key` | Inline for local/dev; ref required by production provisioning | `edge_cookie/secret_key`             |
| `handlers[].password`    | Inline for local/dev; ref required by production provisioning | No derived default in v1             |

`handlers.username` remains `Redacted<String>` and inline-only in v1.

Request-signing resources remain on their current dedicated path:

- private signing keys in the `signing_keys` runtime alias
- runtime Fastly API token in the `api-keys` runtime alias under `api_key`

## TOML representation

### Inline local/dev form

Inline strings remain accepted by the config parser and runtime:

```toml
[publisher]
domain = "example.com"
cookie_domain = ".example.com"
origin_url = "https://origin.example.com"
proxy_secret = "dev-proxy-secret"

[edge_cookie]
secret_key = "dev-edge-cookie-secret"

[[handlers]]
path = "^/admin"
username = "admin"
password = "dev-password"
```

This form is intended for local development, tests, and migration only. Fastly production provisioning must reject it for v1 secret-bearing fields.

### Default-store secret ref shorthand

The common production form uses the default application secret-store alias:

```toml
[publisher]
domain = "example.com"
cookie_domain = ".example.com"
origin_url = "https://origin.example.com"
proxy_secret = { secret = "publisher/proxy_secret" }

[edge_cookie]
secret_key = { secret = "edge_cookie/secret_key" }

[[handlers]]
path = "^/admin"
username = "admin"
password = { secret = "handlers/admin/password" }
```

`secret` means “read this key from the default application secret-store runtime alias.”

### Explicit store override form

Advanced cases may use an explicit runtime store alias:

```toml
[publisher]
proxy_secret = { store = "publisher_secrets", key = "proxy_secret" }
```

Rules:

- `{ secret = "..." }` and `{ store = "...", key = "..." }` are the only supported ref shapes in v1.
- `secret` must not be combined with `store` or `key`.
- `key` must not be empty.
- `store`, when present, must not be empty.
- The `store` value is a runtime secret-store alias, not necessarily the provider's underlying resource name.

### No omitted-field default refs in v1

V1 does not infer secret refs from omitted fields.

This is invalid for required secret-bearing fields:

```toml
[publisher]
domain = "example.com"
cookie_domain = ".example.com"
origin_url = "https://origin.example.com"
# proxy_secret omitted
```

Fields remain visibly required. Tooling may later scaffold explicit refs, but omission does not imply a default.

## Fastly provider configuration

Provider-specific resource names remain outside canonical runtime application config. For Fastly, the source config may include:

```toml
[providers.fastly.secrets]
store_name = "customer_ts_secrets"
```

Provisioning uses this provider setting to create or locate the underlying Fastly Secret Store resource and bind it to a fixed runtime alias.

### Fixed runtime alias

The default application secret store uses a fixed runtime alias:

```text
ts_secrets
```

Provisioning may link any underlying Fastly Secret Store resource as `ts_secrets`. Runtime code opens the alias, not the underlying provider resource name.

A shorthand ref:

```toml
proxy_secret = { secret = "publisher/proxy_secret" }
```

therefore resolves as:

```text
store alias = ts_secrets
key = publisher/proxy_secret
```

An explicit ref:

```toml
proxy_secret = { store = "publisher_secrets", key = "proxy_secret" }
```

resolves as:

```text
store alias = publisher_secrets
key = proxy_secret
```

If an explicit store alias is used, that alias must be available to the runtime. V1 provisioning should validate and/or document this requirement rather than silently treating `store` as an underlying provider resource name.

## Suggested config type model

Redaction and secret-store semantics should be separate concepts.

Suggested shape:

```rust
pub enum SecretString {
    Inline(Redacted<String>),
    Ref(SecretRef),
}

pub enum SecretRef {
    DefaultStore { key: String },
    Store { store: String, key: String },
}
```

The runtime-facing resolved value should not expose raw strings without redaction:

```rust
pub enum ResolvedSecretString {
    Available(Redacted<String>),
    Unavailable { reference: SecretRef },
}
```

Names are illustrative. The important distinction is:

- parsed/raw settings can contain inline values or refs
- resolved settings know whether each secret is available
- logging/debug output never prints resolved secret values

## Runtime model

### Two-phase settings model

Runtime should conceptually distinguish raw config from resolved config:

```text
RawSettings      -> contains SecretString inline values and refs
ResolvedSettings -> contains available redacted values or unavailable secret markers
```

A loaded runtime config snapshot is produced from canonical application config plus a best-effort eager secret-resolution pass.

### Eager but non-fatal resolution

For each loaded config snapshot, runtime should:

1. Parse and validate the canonical TOML payload.
2. Identify all v1 secret refs.
3. Attempt to resolve refs through `RuntimeServices.secret_store()`.
4. Store resolved redacted values when available.
5. Store unavailable markers when refs cannot be resolved.
6. Log a warning or error for each unavailable secret ref.
7. Continue serving with the resolved snapshot.

Missing secret refs at runtime must not make the entire service fail config loading. This protects production availability when a secret is deleted or changed out-of-band through the Fastly UI/CLI after a successful provisioning flow.

### Feature-scoped fail-closed behavior

Missing secrets must affect only the capability that requires the secret, and that capability must fail closed.

Expected behavior:

| Missing secret           | Runtime behavior                                                                                                                                             |
| ------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `handlers[].password`    | Matching protected route must not bypass auth. It should deny access or return an auth/config error for that route.                                          |
| `publisher.proxy_secret` | First-party proxy signing, encryption/decryption, and URL-rewrite behavior requiring the proxy secret should fail or be disabled. Unrelated routes continue. |
| `edge_cookie.secret_key` | Edge Cookie generation/validation should fail or be disabled. Unrelated request handling continues.                                                          |

The exact HTTP status codes can be implementation-specific, but behavior must not silently skip security checks or generate weak fallback secrets.

### No runtime generation

Runtime must never generate missing secret values.

If a referenced secret is unavailable, runtime logs the problem and marks the field unavailable. Secret creation belongs to an explicit provisioning or operations workflow, not request handling.

## Provisioning behavior

Fastly provisioning is the production hard gate.

### Inline secret rejection

`ts provision fastly plan` should surface inline secret values as blocking production issues.

`ts provision fastly apply` must fail before any mutating action if any v1 secret-bearing field is inline:

- `publisher.proxy_secret`
- `edge_cookie.secret_key`
- `handlers[].password`

There is no v1 `--allow-inline-secrets` production escape hatch.

Rationale: local developers should not be uploading local inline config to production, and production provisioning should prevent accidental plaintext secret upload into the canonical runtime config store.

### Referenced secret verification

Provisioning should inspect secret refs and verify that referenced secret stores and secret items exist before applying the canonical app config.

For shorthand refs, provisioning checks the default Fastly secret-store resource configured under `[providers.fastly.secrets]` and linked as `ts_secrets`.

For explicit store override refs, provisioning should verify the referenced runtime alias is available or report that the alias must be provisioned separately.

Example plan output:

```text
Application secret references:

- publisher.proxy_secret -> ts_secrets/publisher/proxy_secret
  status: present

- edge_cookie.secret_key -> ts_secrets/edge_cookie/secret_key
  status: missing
  action: create the secret before apply

- handlers[0].password -> ts_secrets/handlers/admin/password
  status: missing
  action: create the secret before apply
```

`apply` must fail before mutation if any referenced required secret is missing.

### No secret generation in v1

Provisioning does not generate any secrets in v1.

Specifically, v1 does not generate:

- `publisher.proxy_secret`
- `edge_cookie.secret_key`
- handler passwords
- request-signing keypairs
- runtime API tokens beyond existing request-signing behavior

Existing request-signing bootstrapping behavior may remain as-is; it is not part of this generic secret-ref scope.

## Config hashing and canonicalization

Secret refs participate in canonical runtime config and therefore in the config hash.

Changing this:

```toml
proxy_secret = { secret = "publisher/proxy_secret" }
```

to this:

```toml
proxy_secret = { secret = "publisher/proxy_secret_v2" }
```

must change the config hash.

Changing only the resolved value stored in the secret store at the same ref must not change the config hash.

Rules:

- Canonical config may contain secret refs.
- Canonical config must not contain resolved secret values when refs are used.
- Config hash includes ref data: store alias and key.
- Config hash excludes resolved secret values.
- Local/dev canonicalization may technically include inline dev values, but production provisioning rejects inline v1 secrets before upload.

## Local development behavior

Local development supports both inline values and refs.

Inline values keep the simple path simple:

```toml
[publisher]
proxy_secret = "dev-proxy-secret"
```

Refs allow developers to exercise production-like behavior with Viceroy/local secret-store fixtures:

```toml
[publisher]
proxy_secret = { secret = "publisher/proxy_secret" }
```

Local runtime behavior should use the same resolution semantics as production:

- inline values are immediately available
- refs resolve through the configured local/platform secret-store abstraction
- missing refs log warnings and mark the field unavailable
- affected features fail closed

`ts config validate` may warn when inline v1 secrets are present, but inline dev config remains valid.

## Validation rules

### Parser/schema validation

- V1 secret-bearing fields accept either a string or one supported ref table shape.
- Unknown keys inside ref tables are rejected.
- Empty strings are rejected for inline secret values.
- Empty `secret`, `store`, or `key` values are rejected.
- Mixed ref shapes are rejected.

Invalid examples:

```toml
proxy_secret = { secret = "publisher/proxy_secret", key = "proxy_secret" }
proxy_secret = { store = "publisher_secrets" }
proxy_secret = { key = "proxy_secret" }
proxy_secret = { secret = "" }
```

### Provisioning validation

Fastly production provisioning additionally rejects:

- inline values for v1 secret-bearing fields
- missing required referenced secret stores
- missing required referenced secret items
- shorthand refs when the default application secret store cannot be provisioned or linked as `ts_secrets`

## Migration path

1. Add secret refs to `trusted-server.toml` for v1 secret-bearing fields.
2. Create/populate the corresponding Fastly Secret Store items out-of-band or through a future CLI helper.
3. Configure the underlying Fastly secret-store resource:

   ```toml
   [providers.fastly.secrets]
   store_name = "customer_ts_secrets"
   ```

4. Run provisioning plan.
5. Fix any missing secret refs reported by provisioning.
6. Apply provisioning.

Example migration from inline to ref:

```toml
# Before
[publisher]
proxy_secret = "prod-secret-value"

# After
[publisher]
proxy_secret = { secret = "publisher/proxy_secret" }
```

The secret value itself is uploaded to the provider secret store under `publisher/proxy_secret`. The canonical application config contains only the ref.

## Implementation notes

This section is intentionally high-level and non-prescriptive.

Potential implementation direction:

- Introduce a reusable secret config module in `trusted-server-core`.
- Replace selected `Redacted<String>` fields in raw config structs with `SecretString`.
- Add a resolved settings layer or resolved secret wrapper so runtime call sites can distinguish available from unavailable secrets.
- Add secret field discovery helpers for provisioning.
- Extend Fastly provider config with `providers.fastly.secrets.store_name`.
- Add a core constant for the default application secret-store runtime alias, likely `ts_secrets`.
- Keep request-signing-specific constants and provisioning logic unchanged for v1.

The implementation should avoid broad refactors. A manual descriptor or helper list for the initial fields is preferred over introducing a derive macro.

## Acceptance criteria

- [ ] Define a first-class secret value/reference representation for v1 fields.
- [ ] Support inline strings and supported ref table shapes in TOML parsing.
- [ ] Keep inline v1 secrets valid for local/dev runtime use.
- [ ] Add Fastly provider configuration for the default application secret store.
- [ ] Use a fixed runtime alias, `ts_secrets`, for shorthand refs.
- [ ] Include secret refs, but not resolved secret values, in canonical config hashing.
- [ ] Resolve refs eagerly per loaded config snapshot without failing the entire service for missing runtime secrets.
- [ ] Log missing runtime secrets clearly.
- [ ] Ensure affected runtime features fail closed when a required secret is unavailable.
- [ ] Reject inline v1 secrets during Fastly production provisioning before mutation.
- [ ] Verify referenced secret stores and secret items during Fastly provisioning.
- [ ] Fail provisioning before mutation when required refs are missing.
- [ ] Do not add automatic secret generation in v1.
- [ ] Do not add `handlers.id` in v1.
- [ ] Do not migrate request-signing secrets into the generic secret-ref model in v1.
- [ ] Document local/dev inline-secret support and production ref requirements.

## Open questions

The following are intentionally deferred from v1:

1. Should future CLI commands generate secret values and upload them to provider secret stores?
2. Should handler password generation ever be supported, and if so how should generated human credentials be revealed or stored?
3. Should a later version add `handlers.id` to support stable default handler password keys?
4. Should request-signing runtime API token and signing keys eventually use the generic secret-ref descriptor model?
5. Should other providers use the same fixed default alias name or define provider-specific defaults?
6. Should runtime periodically retry missing secret refs or wait for the next config load/request snapshot?
