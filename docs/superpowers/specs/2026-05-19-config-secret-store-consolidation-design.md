# Trusted Server Config & Secret Store Consolidation Design

> **Status:** Proposal
> **Related:** [#684](https://github.com/IABTechLab/trusted-server/issues/684)
> **Scope:** Runtime store naming, provider provisioning shape, secret/config namespace consolidation, and migration strategy
> **Date:** May 2026

## Summary

Trusted Server now has a platform abstraction for runtime config stores and secret stores, but the concrete runtime aliases and provider settings are still fragmented by feature.

Current Fastly-facing store aliases include:

- `ts_config_store` for the canonical application config payload at key `ts-config`
- `jwks_store` for request-signing public key metadata
- `signing_keys` for request-signing private keys
- `api-keys` for the runtime Fastly API token at key `api_key`

This design consolidates Trusted Server runtime storage around one Config Store alias and one Secret Store alias:

| Runtime store kind | Fixed runtime alias | Contents                                                                                       |
| ------------------ | ------------------- | ---------------------------------------------------------------------------------------------- |
| Config Store       | `ts_config_store`   | Canonical app config plus non-secret runtime metadata such as request-signing public key state |
| Secret Store       | `ts_secrets`        | App secret refs, request-signing private keys, and runtime management tokens                   |

This is not a proposal to physically merge Fastly Config Store and Secret Store resources. Fastly exposes those as distinct resource types, and Trusted Server should preserve that split. Consolidation means **one authoritative Config Store namespace and one authoritative Secret Store namespace** for Trusted Server runtime state.

## Goals

- Reduce feature-specific store-name drift.
- Keep `ts-config` as the single canonical application config payload key.
- Standardize a default application Secret Store alias for all Trusted Server secrets.
- Move request-signing runtime state into the same Config Store / Secret Store namespace over time.
- Keep provider-specific resource names out of canonical application config.
- Preserve operational safety during migration with compatibility shims for existing aliases.
- Keep runtime behavior resilient: missing or deleted secrets should not take down unrelated features.
- Make provisioning plan/apply responsible for validating required stores, resource links, and keys.

## Non-goals

- Combining Config Store and Secret Store into one physical provider resource.
- Changing request-signing cryptography, key formats, or signing protocol semantics.
- Removing compatibility aliases in the first implementation.
- Automatically generating app-level secrets such as `publisher.proxy_secret` or `edge_cookie.secret_key`.
- Designing a full secret-management CLI beyond the provisioning checks needed for this consolidation.
- Supporting arbitrary user-defined store topologies in v1.

## Current state

### Application config store

Runtime application config is loaded from a fixed Config Store alias and key:

```text
store alias = ts_config_store
key = ts-config
```

The underlying Fastly Config Store resource name is provider-specific and currently configured through:

```toml
[providers.fastly.application_config]
store_name = "customer_ts_config"
```

The provider section is removed before canonical application config is uploaded, so provider resource names do not participate in the runtime config hash.

### Request-signing stores

Request signing currently uses separate runtime aliases:

```text
Config Store alias = jwks_store
Secret Store alias = signing_keys
Secret Store alias = api-keys
API token key     = api_key
```

Provider settings can override the underlying Fastly resource names:

```toml
[providers.fastly.request_signing]
jwks_store_name = "customer_jwks"
signing_secret_store_name = "customer_signing_keys"
runtime_api_secret_store_name = "customer_api_keys"
```

This works, but it creates a separate store topology for one feature. New secret-backed config fields would add more drift unless Trusted Server standardizes on shared runtime namespaces.

### App-level secret-bearing config fields

Several settings are secret material but are currently authored inline in `trusted-server.toml`, including:

```toml
[publisher]
proxy_secret = "..."

[edge_cookie]
secret_key = "..."

[[handlers]]
password = "..."
```

A related secret-reference design introduces explicit secret refs such as:

```toml
[publisher]
proxy_secret = { secret = "publisher/proxy_secret" }
```

The shorthand `secret` form should resolve through the consolidated default Secret Store alias, `ts_secrets`.

## Target model

### Runtime aliases

Trusted Server should converge on these fixed runtime aliases:

```text
APPLICATION_CONFIG_STORE_NAME = "ts_config_store"
APPLICATION_CONFIG_KEY        = "ts-config"
APPLICATION_SECRET_STORE_NAME = "ts_secrets"
```

Feature code should not introduce new hardcoded runtime aliases unless there is a platform requirement that prevents use of the consolidated store.

### Store contents

The consolidated stores are namespaces, not single-purpose buckets.

#### `ts_config_store`

`ts_config_store` should contain non-secret runtime state:

| Key                           | Value                                                  |
| ----------------------------- | ------------------------------------------------------ |
| `ts-config`                   | Canonical application TOML payload                     |
| `request-signing/current-kid` | Current signing key ID                                 |
| `request-signing/active-kids` | Comma-separated or otherwise serialized active key IDs |
| `request-signing/jwks/<kid>`  | Public JWK for a signing key                           |

The exact request-signing key names may change during implementation, but they must be namespaced under a stable request-signing prefix rather than living at top-level keys like `current-kid`.

#### `ts_secrets`

`ts_secrets` should contain secret runtime state:

| Key                                        | Value                                                 |
| ------------------------------------------ | ----------------------------------------------------- |
| `publisher/proxy_secret`                   | Publisher proxy secret                                |
| `edge_cookie/secret_key`                   | Edge Cookie secret key                                |
| `handlers/<operator-chosen-name>/password` | Handler password, when operators use refs             |
| `request-signing/private-keys/<kid>`       | Request-signing private key material                  |
| `fastly/runtime-api-key`                   | Runtime Fastly API token used by management endpoints |

Handler password keys remain operator-chosen in v1. This design does not require adding `handlers.id` or deriving handler password keys automatically.

### Provider source config

Provider config should expose one underlying Fastly resource name for each runtime store kind:

```toml
[providers.fastly.application_config]
store_name = "customer_ts_config"

[providers.fastly.secrets]
store_name = "customer_ts_secrets"
```

`application_config.store_name` already exists. `secrets.store_name` is the new default Secret Store resource setting.

The existing per-request-signing provider fields should become compatibility fields:

```toml
[providers.fastly.request_signing]
jwks_store_name = "customer_jwks"
signing_secret_store_name = "customer_signing_keys"
runtime_api_secret_store_name = "customer_api_keys"
```

They should be accepted during migration, but new config should prefer the consolidated provider shape.

## Secret-ref interaction

Secret refs use the consolidated Secret Store by default.

```toml
[publisher]
proxy_secret = { secret = "publisher/proxy_secret" }
```

resolves as:

```text
store alias = ts_secrets
key = publisher/proxy_secret
```

Advanced explicit refs may still target another runtime alias when necessary:

```toml
[publisher]
proxy_secret = { store = "publisher_secrets", key = "proxy_secret" }
```

However, explicit alternate stores are an escape hatch. The default provisioning and documentation path should use `ts_secrets`.

Canonical application config stores refs, not resolved secret values. The config hash changes when a ref changes, but not when the provider secret value at that ref changes.

## Request-signing consolidation

Request signing is the main existing feature that must migrate from feature-specific stores.

### Target request-signing reads

Runtime request-signing reads should eventually use:

```text
current kid:  ts_config_store/request-signing/current-kid
active kids:  ts_config_store/request-signing/active-kids
public JWK:   ts_config_store/request-signing/jwks/<kid>
private key:  ts_secrets/request-signing/private-keys/<kid>
API token:    ts_secrets/fastly/runtime-api-key
```

This removes dedicated runtime aliases for `jwks_store`, `signing_keys`, and `api-keys`.

### Target request-signing writes

Runtime management endpoints that rotate, deactivate, or delete signing keys should write through the same consolidated stores.

Write operations still need provider management identifiers, not runtime aliases. Those identifiers are deployment/provider concerns and should not require feature-specific store settings in canonical application config.

The implementation may need an adapter-level mapping from fixed runtime aliases to provider resource IDs, or it may use provider config captured during provisioning. The important rule is that canonical application config should not grow additional provider store-name fields for each request-signing feature.

### Compatibility shim

The first implementation should not abruptly break existing deployments.

During a migration window, runtime should support legacy aliases as fallbacks:

| Target read                                     | Legacy fallback          |
| ----------------------------------------------- | ------------------------ |
| `ts_config_store/request-signing/current-kid`   | `jwks_store/current-kid` |
| `ts_config_store/request-signing/active-kids`   | `jwks_store/active-kids` |
| `ts_config_store/request-signing/jwks/<kid>`    | `jwks_store/<kid>`       |
| `ts_secrets/request-signing/private-keys/<kid>` | `signing_keys/<kid>`     |
| `ts_secrets/fastly/runtime-api-key`             | `api-keys/api_key`       |

Fallback reads should log deprecation warnings clearly enough for operators to discover incomplete migrations.

New writes should prefer consolidated keys. If rollback across versions is a hard requirement for a specific release, implementation can dual-write request-signing artifacts during the compatibility window; otherwise, migration docs must state that request-signing writes after migration are not guaranteed to be visible to older binaries.

## Provisioning behavior

Fastly provisioning is responsible for constructing and validating the store topology.

### Plan

`ts provision fastly plan` should report:

- the underlying Config Store resource that will be linked as `ts_config_store`
- the underlying Secret Store resource that will be linked as `ts_secrets`
- whether legacy request-signing stores are present
- whether request-signing data needs to be copied to the consolidated namespace
- whether required app secret refs exist in `ts_secrets`
- whether compatibility aliases are still required for the selected migration mode

Example output:

```text
Trusted Server stores:

- Config Store: customer_ts_config -> ts_config_store
  required keys:
  - ts-config: update
  - request-signing/current-kid: present
  - request-signing/active-kids: present

- Secret Store: customer_ts_secrets -> ts_secrets
  required keys:
  - publisher/proxy_secret: present
  - edge_cookie/secret_key: present
  - fastly/runtime-api-key: missing

Migration:
- legacy jwks_store detected: customer_jwks
- legacy signing_keys detected: customer_signing_keys
- apply can copy legacy request-signing keys into consolidated namespace
```

### Apply

`ts provision fastly apply` should fail before mutation when required consolidated resources cannot be created, linked, or verified.

For migration, apply may copy legacy request-signing data into consolidated keys when both of these are true:

1. the target consolidated key is missing
2. the corresponding legacy key exists

Apply must not overwrite an existing consolidated secret with a legacy value without explicit operator confirmation or a future force flag.

### App secret refs

Production provisioning should reject inline app-level secrets for fields that support secret refs and verify that referenced secret keys exist before uploading canonical config.

Missing app-level secret refs should be blocking. Provisioning must not invent fallback secrets.

### Runtime API token

The runtime Fastly API token should move from:

```text
api-keys/api_key
```

to:

```text
ts_secrets/fastly/runtime-api-key
```

Provisioning should keep existing CLI input paths for providing the runtime API token, but store it under the consolidated key for new deployments.

During migration, provisioning may copy or prompt for the token. It should never print token values.

## Runtime behavior

### Config loading

The application config payload remains authoritative at:

```text
ts_config_store/ts-config
```

If `ts-config` is missing or invalid, runtime config loading fails and the service should return the existing config-load failure response.

### Secret resolution

Secret refs in the canonical config should resolve through the default `ts_secrets` alias unless they specify an explicit store override.

Secret resolution should be eager per loaded config snapshot but non-fatal for unrelated features:

- log missing/unavailable secrets
- mark affected secrets unavailable
- fail closed only when a request reaches the affected capability
- never generate a fallback secret at runtime

### Request-signing runtime failures

Request signing is security-sensitive. If consolidated and legacy request-signing keys are both unavailable, request-signing operations should fail closed for that endpoint/capability rather than silently disabling verification or signing.

Unrelated routes should continue when possible.

## Migration strategy

### Phase 1: Introduce consolidated defaults

- Add `APPLICATION_SECRET_STORE_NAME = "ts_secrets"`.
- Add `[providers.fastly.secrets].store_name`.
- Provision new deployments with `ts_config_store` and `ts_secrets` only, except where compatibility aliases are still required.
- Make app-level secret refs resolve shorthand refs through `ts_secrets`.
- Keep existing request-signing aliases working.

### Phase 2: Migrate request-signing data

- Add request-signing read support for the consolidated key namespace.
- Prefer consolidated request-signing reads, with legacy fallback.
- Teach provisioning to detect legacy request-signing data and copy it to consolidated keys.
- Store new runtime API tokens under `ts_secrets/fastly/runtime-api-key`.
- Emit warnings when legacy request-signing provider fields are used.

### Phase 3: Deprecate feature-specific stores

- Update generated/example config to use only `[providers.fastly.application_config]` and `[providers.fastly.secrets]`.
- Warn when `[providers.fastly.request_signing]` store-name overrides are present.
- Stop requiring legacy aliases for new deployments.
- Keep fallback reads for at least one migration window.

### Phase 4: Remove legacy aliases

- Remove `jwks_store`, `signing_keys`, and `api-keys` runtime aliases after the documented migration window.
- Remove provider parsing for request-signing-specific store names.
- Remove fallback reads and associated warnings.

## Validation rules

Provider validation should enforce:

- `providers.fastly.application_config.store_name` is non-empty and trimmed.
- `providers.fastly.secrets.store_name` is non-empty and trimmed.
- Config Store and Secret Store names are validated independently because they are different Fastly resource kinds.
- Feature-specific request-signing store names, when present, are accepted only for compatibility and should warn.
- New feature work must not add provider store-name settings when it can use the consolidated aliases.

Provisioning validation should enforce:

- `ts_config_store` is linked to the intended Config Store resource.
- `ts_secrets` is linked to the intended Secret Store resource.
- Required app secret refs exist before canonical config upload.
- Required request-signing state exists or can be bootstrapped/migrated safely.
- Missing runtime API token blocks request-signing management endpoints that require it.

## Operational considerations

### Blast radius

Consolidating all Trusted Server secrets into one Secret Store increases the importance of namespace hygiene and access control. Operators should treat access to `ts_secrets` as access to all Trusted Server secret material.

Key names must be namespaced consistently so audit logs and operational tooling can distinguish app secrets, request-signing keys, and provider tokens.

### Rollback

Rollback safety depends on whether request-signing writes are dual-written during the compatibility window.

If implementation does not dual-write, migration docs must warn that rotating keys after migration can make older binaries unable to find the newest key material.

### Deletion outside provisioning

If an operator deletes a secret directly from Fastly after provisioning, runtime should not take down all unrelated traffic. The affected feature should fail closed and logs should identify the missing `<store>/<key>`.

## Acceptance criteria

- [ ] Define and document `ts_secrets` as the default Trusted Server Secret Store runtime alias.
- [ ] Keep `ts_config_store/ts-config` as the canonical app config location.
- [ ] Add provider config for one default Fastly Secret Store resource.
- [ ] Update examples/docs to prefer one Config Store and one Secret Store provider setting.
- [ ] Ensure app-level secret refs resolve shorthand refs through `ts_secrets`.
- [ ] Define namespaced request-signing keys for the consolidated Config Store and Secret Store.
- [ ] Teach provisioning to link/validate consolidated stores.
- [ ] Teach provisioning to detect and report legacy request-signing stores.
- [ ] Provide a safe migration path from `jwks_store`, `signing_keys`, and `api-keys` to consolidated keys.
- [ ] Keep compatibility fallback reads during the migration window.
- [ ] Emit warnings for legacy request-signing provider store-name settings.
- [ ] Do not upload inline app secrets in production canonical config.
- [ ] Do not generate app-level secrets as part of this consolidation.
- [ ] Document rollback implications for request-signing writes during migration.

## Open questions

1. Should the first request-signing migration implementation dual-write to legacy stores, or is a documented non-rollback boundary acceptable?
2. What exact namespaced key format should be used for request-signing public JWKs: `request-signing/jwks/<kid>` or a flatter provider-compatible variant?
3. How long should fallback reads for `jwks_store`, `signing_keys`, and `api-keys` remain supported?
4. Should provider validation warn or fail when new deployments specify `[providers.fastly.request_signing]` store-name overrides?
5. Should migration apply copy legacy secrets automatically when the target is missing, or require an explicit `--migrate-legacy-stores` confirmation?
