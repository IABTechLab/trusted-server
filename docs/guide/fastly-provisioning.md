# Fastly Provisioning Map

Fastly provisioning is driven by `trusted-server.toml`. The CLI validates the config, compares the required Fastly resources against the existing service, and then plans create, update, and bind actions.

Use this page when you want to understand which config changes produce Fastly infrastructure changes and which changes only update runtime configuration.

## Summary

Most Trusted Server config changes do not create new Fastly resources. They update the canonical app config stored in Fastly Config Store `ts_config_store`, item `ts-config`.

Only these config surfaces can change the Fastly resource plan:

| Config change                                                                              | Fastly provisioning effect                                                                                 |
| ------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------- |
| Any valid `trusted-server.toml` content change                                             | Updates `ts_config_store` item `ts-config`. Creates and binds `ts_config_store` if missing.                |
| `[request_signing] enabled = true`                                                         | Creates and binds `jwks_store`, `signing_keys`, and `api-keys` if missing. May bootstrap signing material. |
| `[consent] consent_store = "<name>"`                                                       | Creates and binds a Fastly KV store named `<name>` if missing.                                             |
| `request_signing.config_store_id` or `request_signing.secret_store_id` differs from Fastly | Adds warnings telling you to update IDs after provisioning. Does not choose resource names.                |

All other publisher, integration, handler, proxy, header, and auction settings are application config. They are deployed by updating `ts_config_store/ts-config`.

## Provisioning commands

Preview changes before applying them:

```bash
ts provision fastly plan --service-id svc_123
```

Apply the plan:

```bash
ts provision fastly apply --service-id svc_123
```

Use JSON output when you want to inspect exact actions in automation:

```bash
ts provision fastly plan --service-id svc_123 --json
```

See [Trusted Server CLI](/guide/cli#ts-provision-fastly-plan) for the full command reference.

## Application config store

The app config store is always planned.

| Fastly resource   | Type    | Name or key       | Planned when                                                              |
| ----------------- | ------- | ----------------- | ------------------------------------------------------------------------- |
| Config Store      | Store   | `ts_config_store` | The store is missing.                                                     |
| Config Store item | Item    | `ts-config`       | The item is missing or its value differs from the canonical local config. |
| Resource link     | Binding | `ts_config_store` | The current service version has no matching resource link.                |

The CLI stores the canonical TOML, not the raw file bytes. Reordering or formatting changes that canonicalize to the same config should not produce an app config item update.

Config changes that map only to `ts_config_store/ts-config` include:

| Config area                     | Example                                                                            |
| ------------------------------- | ---------------------------------------------------------------------------------- |
| Publisher settings              | `[publisher] domain`, `cookie_domain`, `origin_url`                                |
| Integrations                    | `[integrations.prebid]`, `[integrations.gpt]`, `[integrations.google_tag_manager]` |
| Proxy behavior                  | `[[first_party_proxy.origins]]`, proxy allowlists                                  |
| Handlers                        | `[[handlers]]`                                                                     |
| Response headers                | `[response_headers]`                                                               |
| Auction and ad-serving settings | Prebid bidders, GAM, APS, creative settings                                        |

These changes affect runtime behavior after the new config item is written. They do not create separate Fastly backends, domains, dictionaries, or stores.

## Request signing resources

Request signing resources are planned only when request signing is enabled:

```toml
[request_signing]
enabled = true
config_store_id = "..."
secret_store_id = "..."
```

When enabled, provisioning manages these resources:

| Fastly resource   | Type     | Name or key                              | Purpose                                            |
| ----------------- | -------- | ---------------------------------------- | -------------------------------------------------- |
| Config Store      | Store    | `jwks_store`                             | Stores public JWKS material and key state.         |
| Config Store item | Item     | `current-kid`                            | Current signing key ID.                            |
| Config Store item | Item     | `active-kids`                            | Comma-separated active key IDs.                    |
| Config Store item | Item     | `<kid>`                                  | Public JWK JSON for a signing key.                 |
| Secret Store      | Store    | `signing_keys`                           | Stores private signing keys.                       |
| Secret Store item | Secret   | `<kid>`                                  | Private Ed25519 signing key bytes, base64 encoded. |
| Secret Store      | Store    | `api-keys`                               | Stores runtime API credentials.                    |
| Secret Store item | Secret   | `api_key`                                | Runtime Fastly API token used for key rotation.    |
| Resource links    | Bindings | `jwks_store`, `signing_keys`, `api-keys` | Make the stores available to the service version.  |

If `jwks_store` and `signing_keys` are empty, `plan` warns that `apply` will bootstrap the first Ed25519 keypair. `apply` writes the public key material to `jwks_store` and the private signing key to `signing_keys`.

If `api-keys/api_key` is missing, `apply` requires exactly one runtime API token source:

```bash
FASTLY_RUNTIME_API_KEY=runtime-token ts provision fastly apply --service-id svc_123

ts provision fastly apply --service-id svc_123 --runtime-api-key runtime-token

ts provision fastly apply --service-id svc_123 --reuse-management-api-key
```

Prefer `FASTLY_RUNTIME_API_KEY` because it avoids putting the token in shell history.

### Request signing IDs

The CLI uses fixed Fastly store names for request signing:

- `jwks_store`
- `signing_keys`

The config fields `request_signing.config_store_id` and `request_signing.secret_store_id` are runtime IDs used by key rotation code. They do not control which stores provisioning creates.

After provisioning, update these fields if the plan or apply output warns that the configured IDs differ from Fastly:

```toml
[request_signing]
config_store_id = "<actual jwks_store ID>"
secret_store_id = "<actual signing_keys ID>"
```

## Consent KV store

Consent KV provisioning is controlled by the `[consent] consent_store` setting:

```toml
[consent]
consent_store = "consent_store"
```

When `consent_store` is set, provisioning manages:

| Fastly resource | Type    | Name or key              | Planned when                                               |
| --------------- | ------- | ------------------------ | ---------------------------------------------------------- |
| KV Store        | Store   | Value of `consent_store` | The KV store is missing.                                   |
| Resource link   | Binding | Value of `consent_store` | The current service version has no matching resource link. |

Changing the `consent_store` value changes the target KV store name. The CLI plans a new KV store and binding for the new name if it does not already exist. It does not delete the old KV store.

Leaving `consent_store` unset means provisioning does not create or bind a consent KV store.

## Service version changes

Fastly resource bindings are attached to service versions. Provisioning may need to update the target service version when resource links change.

| Situation                                                     | Plan or apply behavior                                                                             |
| ------------------------------------------------------------- | -------------------------------------------------------------------------------------------------- |
| No binding changes are needed                                 | The latest version remains the target version. No activation is needed.                            |
| Binding changes are needed and the latest version is unlocked | The latest version is updated and activated after binding changes.                                 |
| Binding changes are needed and the latest version is locked   | `apply` clones the latest version, applies binding changes to the clone, then activates the clone. |

Updating a Config Store item or Secret Store item does not itself require cloning a service version. Creating or updating a resource link does.

## Action types in JSON output

JSON plan and apply output describes each change with an action and resource kind:

| Action   | Meaning                                                                  |
| -------- | ------------------------------------------------------------------------ |
| `create` | Create a store or create an item in an existing store.                   |
| `update` | Update an existing config item or resource link.                         |
| `bind`   | Create a resource link between the store and the Fastly service version. |

Resource kinds are:

| Resource kind | Fastly resource               |
| ------------- | ----------------------------- |
| `config`      | Fastly Config Store or item   |
| `secret`      | Fastly Secret Store or secret |
| `kv`          | Fastly KV Store               |

## What provisioning does not do

`ts provision fastly apply` does not:

- Deploy the Wasm package. Use `fastly compute publish` for deployment.
- Create Fastly services or domains.
- Create Fastly backends for each integration setting.
- Delete stores or old resource bindings when config settings are removed or renamed.
- Rotate existing request-signing keys unless the stores are empty and bootstrap is required.
- Upload `trusted-server.toml` as raw text. The CLI writes canonical runtime config.

## Examples

### Change a Prebid server URL

```toml
[integrations.prebid]
enabled = true
server_url = "https://prebid.example/openrtb2/auction"
```

Provisioning effect:

- Update `ts_config_store/ts-config` if the canonical config changes.
- No new Fastly stores or bindings.

### Enable request signing

```toml
[request_signing]
enabled = true
config_store_id = ""
secret_store_id = ""
```

Provisioning effect:

- Ensure `ts_config_store/ts-config` is current.
- Create or bind `jwks_store`.
- Create or bind `signing_keys`.
- Create or bind `api-keys`.
- Bootstrap key material if signing stores are empty.
- Require a runtime Fastly API token if `api-keys/api_key` is missing.
- Warn to update `config_store_id` and `secret_store_id` after store IDs are known.

### Enable consent persistence

```toml
[consent]
consent_store = "publisher_consent"
```

Provisioning effect:

- Ensure `ts_config_store/ts-config` is current.
- Create or bind KV store `publisher_consent`.
- No request-signing stores unless request signing is also enabled.

### Rename the consent KV store

```toml
[consent]
consent_store = "publisher_consent_v2"
```

Provisioning effect:

- Update `ts_config_store/ts-config`.
- Create or bind KV store `publisher_consent_v2`.
- Leave the old KV store in Fastly. Remove or migrate it manually if it is no longer needed.

## Related docs

- [Trusted Server CLI](/guide/cli)
- [Fastly Setup](/guide/fastly)
- [Configuration](/guide/configuration)
- [Request Signing](/guide/request-signing)
