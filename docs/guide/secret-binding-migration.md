# Migrate app secret bindings

This is a breaking config migration. Credentials remain raw native values, but app config no longer chooses secret names. TS derives each binding from a fixed purpose and its effective identity and destination. The new binary never retries legacy names or names from the earlier label-bearing design draft.

::: danger Unsafe downgrade
Restoring the old binary restores the config-only secret-substitution and mutable-destination attack. Prefer rolling forward or disabling affected exporters. Rollback requires the old binary's compatible config snapshot and retained legacy bindings, not just an old package.
:::

Local native lookup and HTTPS regression tests do not certify a production rollout. Secure Fastly replacement, provider permissions and propagation, four-adapter operator walkthroughs, and migration/rollback rehearsals remain deployment gates tracked in [#1479](https://github.com/IABTechLab/trusted-server/issues/1479). These instructions are not a claim that those environments have been verified.

## Removed properties

Remove these properties everywhere, including disabled features, JSON blobs, commented examples you intend to enable, and CLI overlay scripts. Presence is a migration error even if the property contains an inline value, old label, or generated key. Internal resolved Rust fixture fields are not public TOML syntax.

| Removed config property                                               | Fixed CLI selector | Replacement activation                                               |
| --------------------------------------------------------------------- | ------------------ | -------------------------------------------------------------------- |
| `publisher.proxy_secret`                                              | `publisher.proxy`  | Always required                                                      |
| `ec.passphrase`                                                       | `ec.passphrase`    | Always required                                                      |
| `ec.partners[*].api_token`                                            | `ec.inbound`       | Set boolean `api_token_enabled = true` if inbound auth was used      |
| `ec.partners[*].ts_pull_token`                                        | `ec.pull`          | Existing `pull_sync_enabled`                                         |
| `handlers[*].password`                                                | `handler.password` | Each configured handler                                              |
| `trusted_client_ip.shared_secret`                                     | `clientip.shared`  | Section present                                                      |
| `trusted_forwarder.shared_secret`                                     | `forwarder.shared` | Section present                                                      |
| `tinybird.auction_token_secret`                                       | `tinybird.auction` | Tinybird enabled                                                     |
| `integrations.datadome.server_side_key_secret_name`                   | `datadome.server`  | Integration and Protection enabled                                   |
| `integrations.datadome.protection_test_bypass.credential_secret_name` | `datadome.bypass`  | Integration, Protection, and bypass enabled                          |
| `proxy.asset_routes[*].auth.access_key_id`                            | `s3.access-id`     | S3 auth                                                              |
| `proxy.asset_routes[*].auth.secret_access_key`                        | `s3.secret`        | S3 auth                                                              |
| `proxy.asset_routes[*].auth.session_token`                            | `s3.session`       | Set boolean `session_token_enabled = true` for temporary credentials |

Both new flags default to `false` and accept booleans only, not strings. Do not forget inbound authentication when removing `api_token`. An enabled role missing its binding fails loading, rather than silently disabling authentication. Disabled roles make no native reads. Existing pull boolean/string activation follows its existing normalization; the string `"false"` does not enable it.

Deprecated Tinybird/DataDome/S3 store selectors remain ignored, deserialize-only inputs; remove them. Tinybird's deprecated `access_token_secret` is still discarded, and `access_enabled = true` is still rejected. It is not an additional role. Trusted-client-IP credentials already came from the native store; the old guide's inline-value description was wrong.

## Endpoint and credential compatibility

Endpoint syntax is stricter, including on disabled features. Pull `pull_sync_url`, DataDome `protection_api_origin`, and authenticated S3 `origin_url` require HTTPS DNS names without queries, fragments, percent escapes, userinfo, IP literals, trailing dots, or whitespace. Authenticated S3 origins cannot contain a path. Tinybird `api_host` takes only a DNS hostname and optional port, not a scheme or path, and follows the same hostname restrictions. Remove or replace invalid parked endpoints before upgrading.

The old binary preserved pull URL query parameters. Obtain a query-free endpoint from the partner before cutover if yours requires one. A nonempty `pull_sync_allowed_domains` may contain only the endpoint hostname, compared case-insensitively without trailing dots. Remove backup hosts and trailing dots from old allowlists. Pull sync now always verifies certificates, regardless of `proxy.certificate_check`.

Partner-issued pull tokens need nonempty RFC 6750 Bearer representation, not a new 32-byte minimum. Enabled DataDome test-bypass credentials must have at least 32 bytes; a shorter value now fails the whole config load instead of disabling the bypass with a warning.

## New-schema example

```toml
[[handlers]]
path = "^/_ts/admin"
username = "admin"

[publisher]
domain = "example.com"
cookie_domain = ".example.com"
origin_url = "https://origin.example.com"

[ec]
ec_store = "ec_identity_store"

[[ec.partners]]
name = "Example partner"
source_domain = "partner.example.com"
api_token_enabled = true
pull_sync_enabled = true
pull_sync_url = "https://sync.example.com/pull"
pull_sync_allowed_domains = ["sync.example.com"]

[tinybird]
enabled = true
api_host = "api.tinybird.example.com"
auction_dataset = "auction_events_raw"

[[proxy.asset_routes]]
prefix = "/private/"
origin_url = "https://assets.example.com"

[proxy.asset_routes.auth]
type = "s3_sigv4"
region = "us-east-1"
session_token_enabled = true
origin_query = "strip"
```

The S3 hostname is fictional; production must use the approved HTTPS S3 origin that validates SigV4. No secret properties, generated keys, labels, store overrides, owner IDs, or generation IDs belong in this config. `s3_sig_v4` remains a deserialize alias for `s3_sigv4`; both use the same effective route policy. A nonempty pull allowlist may contain only the endpoint hostname. It is no longer mandatory and cannot authorize a retargeted credential.

## Provision first, cut over in isolation

1. Assign an operator and rollback deadline. Inventory all active purposes, identities, endpoints, route policies, provider resources, retained states, and legacy names without printing values. Separate cross-role credential reuse. Verify config writers cannot write native secrets, change trusted deployment links, or run code with secret-writer permissions.
2. Save access-controlled old binary/config/deployment records for the approved rollback window. Create and review a new-schema config from trusted source, not an unreviewed live mutable blob. Blindly provisioning a plan from attacker-edited config would grant its requested authority.
3. Select isolated resources through the existing deployment inputs. Old and new binaries require separate compatible config snapshots/stores during mixed rollout. Do not expose the new config to an old binary or assume both schemas work in a shared blob. Fastly `--staging` alone does not select isolated stores.
4. Generate and approve the complete plan with the matching CLI and deployment selection:

   ```bash
   ts secrets plan --adapter fastly --app-config trusted-server.toml \
     --no-env --out bindings.json
   ```

   Use `--adapter axum`, `cloudflare`, or `spin` for native rendering. Review all generated names, declarations, service/store IDs, contexts, and versions. Retain the secret-free plan with the release record. Plan checksums detect drift, not approval.

5. Supply existing raw credential bytes securely to the generated bindings using [interactive Fastly provisioning](./secrets#first-fastly-setup-with-tinybird) or [native provisioning](./secrets#axum-native-environment-input). No JSON wrapping is required. Do not copy legacy names into new TOML. Fastly secret-store entries and Cloudflare Worker secrets cannot be read back through their management APIs, so obtain each value from its source of record. If an `ec.passphrase` or `publisher.proxy` value is unrecoverable, migration becomes a rotation: passphrase-derived EC IDs change and outstanding encrypted first-party proxy and click URLs stop decoding. Values are no longer trimmed. The previous binary trimmed resolved S3, Tinybird, and DataDome server credentials, so stored surrounding whitespace or a trailing newline that worked before now fails validation or changes the outbound credential. `ts secrets set --stdin` rejects terminal input and trailing CR/LF for every role; use `printf '%s'` for piped input. Native runtime reads still preserve exact bytes. If validation rejects a former credential, arrange a recipient-side replacement rather than trimming bytes or weakening validation. Provision each required role, including separate inbound/pull tokens and all active S3 roles.
6. Add generated native declarations where required. Spin requires both application and component declarations with protected value sources. Regenerate/review plans after manifest changes. Confirm presence where supported; Cloudflare/Spin TS checks are explicitly unsupported and nonzero. Fastly metadata and Axum environment presence do not prove stored bytes or runtime behavior.
7. Deploy the matching new binary and config to the isolated version through the existing managed path. Recreate state, then exercise real approved requests and valid-checksum config-only mutations. Require zero unauthorized sends. Verify Tinybird at its approved dataset; do not interpret the fire-and-forget sink as response observation. Verify all four native mappings and config-writer permission separation before production cutover.
8. Switch traffic only when the release gates pass. Track retained/in-flight old states. Within the owned rollback window, remove legacy raw-name entries and unused declarations/links only after no active, staged, or rollback version depends on them. Revoke at recipients if prior exposure is suspected.

## Rotate a value without changing context

Replacing a value uses the same deterministic key. There is no independently selectable old/new generation. Re-running `set` explicitly replaces that binding; it does not allocate another name. Keep an external recipient overlap window where supported, recreate runtime state, verify the new value works, then revoke the old value at the recipient. If values cannot overlap, use a maintenance cutover.

Do not blindly retry a write whose outcome is uncertain. Reconcile through the provider and a fresh-state test, then explicitly repeat the same reviewed operation if needed. Provider replacement/propagation semantics need isolated proof. A confirmed write does not instantly update warm instances.

For S3, the access ID, secret key, and optional session token are separate writes and not transactional. Pause affected state creation and traffic, update and verify the full required set, recreate state, test GET/HEAD and signing preflight, then resume. If the required coordinated maintenance or provider swap is unavailable, rollout is blocked; do not accept partially updated credential sets.

Axum normally needs a process restart. Fastly needs fresh state/sandboxes, including reusable-sandbox mode. Cloudflare and Spin need reconstruction through their existing runner lifecycle. No new watcher, TTL, global refresh, or immediate revocation mechanism is added.

## Retire a context

Changing a subject, endpoint, route policy, or binding ABI requires a new reviewed plan and newly provisioned bindings. Reordering entries keeps every key, so it needs no new bindings. It does change affected entries' `field_path` and entry ID, as well as the effective config fingerprint. `ts secrets check` and `ts secrets set` therefore still require a regenerated, reviewed plan. For each change:

1. Compare old and new plans and approve the new destination/policy.
2. Provision the new keys/declarations before switching the matching config and recreating state.
3. Delete every retired binding/declaration within a named window, replace retained states, and account for in-flight operations.
4. Replay the retired config with a valid checksum against freshly created state. It must fail loading with no old credential export.
5. Revoke the retired credential at the recipient if exposure is suspected or cached states cannot be trusted.

New keys do not revoke old context keys. Leaving the old key provisioned permits a config-only writer to select that complete old authorized identity on fresh state. Deleting it prevents fresh loads, not already captured in-flight use. Record deletion, state replacement, replay evidence, and recipient revocation separately.

## Rollback record

Before any rollback, identify the exact binary, compatible config snapshot, physical store mappings, native declarations, and values it needs. Restoring only the binary can fail startup or reconnect it to unintended stores. Keep old/new config stores separate during mixed rollout.

After the rollback deadline, remove legacy bindings. A later rollback then requires a fresh explicit authorization decision and secure reprovisioning; do not restore a raw-name fallback in the new binary. Document that any old-binary downgrade restores the original attack. Prefer a forward fix or disabling exporters, and include post-upgrade withdrawals/retained legacy data in the wider rollback decision.

Request-signing and management credentials remain on their existing separately audited paths. Do not migrate `signing_keys`, `jwks_store`, `current-kid`, or management API identities through this app-credential procedure. Signing reads now reject `current-kid` names in the reserved `ts1_` app-credential namespace before reading a secret, just as signing-key creation already does. This includes aliases under native name normalization. If a legacy signing key uses such a name, migrate it through the existing request-signing rotation procedure and select a nonreserved `current-kid` before upgrading. Deletion and deactivation remain available for legacy key cleanup.
