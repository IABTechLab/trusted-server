# Auction provider mTLS

## Goal and scope

Let an operator require that Trusted Server present a TLS client certificate when it sends OpenRTB requests to an auction provider, and let the self-hosted Prebid Server AWS example require that certificate. This covers [#1650](https://github.com/IABTechLab/trusted-server/issues/1650).

EdgeZero provides the mechanism ([IABTechLab/edgezero#523](https://github.com/IABTechLab/edgezero/issues/523), [#524](https://github.com/IABTechLab/edgezero/pull/524)). An application declares a client identity by name in its manifest and selects it on an outbound request. EdgeZero checks the request's origin against the identity's runtime hosts list before sending, and the adapter attaches the certificate. On Fastly the certificate chain and hosts list come from a Config Store, and the private key comes from a Secret Store as a handle that is never read as plaintext. Cloudflare, Spin and Axum reject identity requests until their adapters implement it ([edgezero#525](https://github.com/IABTechLab/edgezero/issues/525), [#526](https://github.com/IABTechLab/edgezero/issues/526), [#527](https://github.com/IABTechLab/edgezero/issues/527)).

In scope:

- An `mtls` switch on `[auction.providers.<id>]`, available to every OpenRTB profile.
- One fixed client identity declared in Trusted Server's manifest, and the request wiring.
- Optional mTLS in `deploy/pbs-example`, a development certificate script, and the operator documentation.
- Moving Trusted Server's EdgeZero dependencies to a tag that contains #524.

Out of scope:

- Per-publisher or per-provider certificates. One deployment uses one identity.
- Certificate issuance services, automated rotation, and certificate revocation lists in the example.
- Provisioning in `trusted-server-deployer`, which gets its own issue in that repository.
- mTLS on Cloudflare, Spin and Axum deployments, which follows the EdgeZero adapter issues.

## Why mTLS

The PBS example admits callers by a security group allowlist of `trusted_server_cidr_blocks`. On Fastly those are Fastly's published egress ranges, which every Fastly customer shares. The allowlist proves a request came from Fastly, not from this publisher's Trusted Server. With mTLS the load balancer also requires a client certificate issued by the PBS operator's CA.

## Configuration

```toml
[auction.providers.pbs-main]
protocol = "openrtb-2.6"
profile = "prebid-server"
endpoint = "https://pbs.example.com/openrtb2/auction"
mtls = true
```

- `ProviderConfig.mtls` is a boolean. It defaults to `false` when absent and is omitted from the serialized config while `false`, like `allow_insecure_http`. `ProviderConfig` rejects unknown fields, so a config that never sets `mtls` stays readable by older binaries, and a config that sets it is not. A rollback to a binary without this change must first push a config without `mtls` (see [Rollout and rollback](#rollout-and-rollback)).
- Plan compilation rejects `mtls = true` with an `http` endpoint, including one admitted by `allow_insecure_http` or the loopback exception. EdgeZero would refuse it per request; failing at compile time reports the mistake once, at startup.
- `ProviderPlan` carries the flag. Every provider with `mtls = true` uses the identity of the active rotation slot, `auction-providers-blue` or `auction-providers-green`, selected by `[auction] mtls_slot` (default `blue`). See [Rotation](#rotation).
- `AuctionConfig.mtls_slot` is omitted from the serialized config while it is `blue`, so a config that never switches slots stays readable by binaries that predate the field. `green` is serialized and rejected by those binaries.
- `validate_fastly_backend_bound` counts distinct backend configurations by scheme, host, port and timeout. A provider with mTLS registers a different Fastly backend than one without it, because EdgeZero folds the identity into the backend name, so the flag joins that tuple.

## Manifest

Trusted Server's tracked `edgezero.toml` always declares the identity:

```toml
[capabilities]
optional = ["outbound-client-identity"]

[capabilities.outbound.client-identities.auction-providers-blue]
[capabilities.outbound.client-identities.auction-providers-green]
# Each binds to the [stores.config] and [stores.secrets] defaults:
# trusted_server_config and trusted_server_secrets.
```

- The manifest is compiled into the binary and must stay free of operator configuration (release attestation rule R-CONFIG). So the identity names are fixed by Trusted Server, the operator turns mTLS on per provider with `mtls`, and selects the slot with `mtls_slot`. A declared identity that no provider selects costs nothing.
- `outbound-client-identity` is optional, not required, so Trusted Server still builds for Axum, Cloudflare and Spin. EdgeZero's CLI logs a warning for those adapters.
- EdgeZero's Fastly CLI check passes because both default stores resolve.

### Baking the manifest into the Fastly app

EdgeZero builds the identity policy from `Hooks::manifest()` when `App::from_router` assembles the app. The `app!` macro bakes the manifest for that hook, but each Trusted Server adapter implements `Hooks` by hand, and the trait's default returns `BakedManifest::Absent`. With that default the policy is empty, and every identity request fails as `Undeclared`.

So the Fastly adapter's `Hooks` impl returns the manifest:

- `trusted-server-core/build.rs` already loads `edgezero.toml` with `ManifestLoader`. It also writes the manifest's JSON to `OUT_DIR`, as `app!` does.
- A core function returns `Manifest::from_baked_json` of that JSON, cached in a `OnceLock`. There is no TOML parsing at runtime.
- `TrustedServerApp::manifest()` in the Fastly adapter returns it.

At this EdgeZero tag, the identity policy is the only thing on Fastly that reads `Hooks::manifest()` at runtime, so nothing else changes. Axum, Cloudflare and Spin keep the default. Their EdgeZero adapters read the manifest for geo and runtime variables, and those adapters reject `mtls` at startup anyway ([Adapter support](#adapter-support)).

## Request wiring

The generic OpenRTB provider builds its request in `auction/provider.rs`. When the provider plan has `mtls`, it adds `.client_identity(<active slot identity>)` to the `OutboundRequest`, next to the existing `OutboundCachePolicy::Bypass`. Nothing else in the request changes. EdgeZero matches the request's scheme, host and port against the hosts list and ignores the `Host` header.

Auction requests do not follow redirects, so EdgeZero's redirect re-check does not come into play.

## Privacy and consent

The client certificate is transport metadata. It identifies the Trusted Server deployment, not an end user, and is the same for every request. Bid request bodies, consent handling, cookie forwarding and the attested client-IP headers do not change ([Request wiring](#request-wiring)).

What the verifier learns:

- The certificate itself, on every connection. The guide asks for a service subject with no personal information.
- On an AWS ALB in verify mode, the listener forwards certificate details to the targets in `X-Amzn-Mtls-Clientcert-*` headers, including the URL-encoded leaf certificate.
- Optional ALB connection logs record each handshake's source IP (a Fastly egress address), the certificate subject and serial, and the verification result. They do not record request bodies. The operator owns the log bucket's access and retention.

The private key stays in the Fastly Secret Store and is read only as a handle.

## Adapter support

Each adapter's `app.rs` already compiles the auction plan during app assembly, and Fastly already runs an adapter-specific check on it (`validate_fastly_backend_bound`). Axum, Cloudflare and Spin add one call to a core helper, `reject_auction_mtls(&plan, adapter_name)`. It fails assembly with a configuration error naming the providers that set `mtls = true`. It checks every compiled provider, including when the auction is disabled, because `compile_auction_plan` compiles all providers before it applies `enabled`.

`ts config push` and `ts config diff` already validate per target adapter: for adapters other than Fastly they use `ProviderlessAppConfig`, which rejects enabled identity providers (`crates/trusted-server-cli/src/checked_app_config.rs`). That validation also rejects `mtls = true`, so the CLI cannot publish a config that an unsupported adapter would refuse at startup. Adapter-independent `ts config validate` still applies the https rule through plan compilation.

A misconfigured deployment therefore fails before deploy or at startup, not on each auction. EdgeZero's per-request `ClientIdentityError::Unsupported` remains the backstop. When an adapter gains support, its call is removed in the same change.

## Failure behavior

A typed client-identity error is a transport failure for that provider. Every case fails one batch slot; the other slots are unaffected, and no request is retried without the certificate.

| Stage                                                                             | Errors                                                                                                                                                   | Auction outcome                                                        |
| --------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------- |
| Core preflight, before any adapter I/O                                            | `ClientIdentity`: `Undeclared`, `RequiresHttps`                                                                                                          | Transport failure, no bids                                             |
| Fastly request preparation, before backend registration and before any origin I/O | `ClientIdentity`: `HostNotAllowed`; `MaterialMissing` for the certificate, hosts list, key or store; `MaterialInvalid` for the certificate or hosts list | Transport failure, no bids                                             |
| Fastly option validation                                                          | `ClientIdentity`: `Unsupported` (identity with Image Optimizer or surrogate keys; auction requests use neither)                                          | Transport failure, no bids                                             |
| Fastly SDK backend build (`builder.finish()`)                                     | A registration error with provider-failure detail                                                                                                        | Launch failure, as today; counts toward the all-launch-failed boundary |
| Origin rejects the handshake                                                      | Send failure                                                                                                                                             | Transport failure, as today                                            |
| Binding deadline expires                                                          | `GatewayTimeout`                                                                                                                                         | Timeout, as today                                                      |

`EdgeError::ClientIdentity` carries no provider-failure detail, so `settle_bidder_slot` settles it as a transport failure. EdgeZero's HTTP status for each error (500 or 503; 502 only for a redirect, which auction requests do not follow) does not reach the auction response.

The key is never parsed: EdgeZero opens it as a Secret Store handle and passes it to the Fastly SDK. What a key that does not match the certificate does depends on the host, and stock Viceroy does not compare them at registration. If the host rejects the pair when the backend is registered, that is a launch failure, nothing is cached, and a later request tries again. If registration succeeds, the handshake fails, which is a transport failure, and that backend stays registered in the sandbox. Only the staging run shows which path production Fastly takes. A Secret Store lookup that errors, which likely includes one past the read limit, is reported as `store is missing`; an absent key is `private key is missing`.

**Logging.** `log_request_failure` logs `Planned provider '<id>' request failed: {report:?}` once at warn level. Today `outbound_error` attaches the `EdgeError` with `attach_opaque`, so that log shows only the generic status and kind, and it cannot tell a missing key from a host mismatch. For a `ClientIdentity` error, `outbound_error` also attaches a printable diagnostic with the identity name and the `ClientIdentityError` display text, such as `outbound client identity auction-providers-blue: private key is missing` or `... https://pbs.example.com:443 is not in the identity's hosts` (the origin always includes the port). That text names the origin and the material slot, never key bytes or PEM contents.

## Fastly binding

Each slot has three entries in the stores that `trusted_server_config` and `trusted_server_secrets` link to, where `<identity>` is `auction-providers-blue` or `auction-providers-green`. The operator provisions the active slot:

| Store                                   | Entry                    | Contents                                                                                         |
| --------------------------------------- | ------------------------ | ------------------------------------------------------------------------------------------------ |
| Config Store (`trusted_server_config`)  | `<identity>.certificate` | PEM chain, leaf first, at most 8,000 characters                                                  |
| Config Store (`trusted_server_config`)  | `<identity>.hosts`       | The origin of every provider with `mtls = true`, one per line, such as `https://pbs.example.com` |
| Secret Store (`trusted_server_secrets`) | `<identity>.private-key` | Unencrypted PEM private key                                                                      |

- These entries sit beside the app-config blob in the same Config Store. EdgeZero's config-store garbage collection deletes only orphaned chunk entries, so it leaves them alone.
- Every hosts-list origin must be allowed by `[capabilities.outbound].hosts`. Trusted Server declares `["*"]`.
- Each slot's two Config Store entries count toward the store's 500-entry limit. EdgeZero caps the certificate entry at 8,000 bytes and the hosts entry at 4,000 bytes.

### Secret Store read budget

Fastly allows 5 Secret Store reads per Compute request by default, and Trusted Server spends some on its own secrets, such as request-signing keys. EdgeZero reads the private key once for each attempted backend registration, and a sandbox registers a backend the first time it sends to a new combination of origin, port, timer bucket and certificate; configured timeouts that round to the same bucket share a backend. A sandbox that already has that backend reads no secret, though it still checks the Config Store entries. A failed registration reads again on a later attempt. The 160-backend startup bound does not count backends a sandbox keeps from before a certificate rotation.

- Without the `reusable-sandbox` feature every request starts cold, so each auction spends one read per distinct mTLS backend combination it sends to.
- With one PBS endpoint, that is one read per auction request.
- Every additional mTLS origin, or the same origin with a timeout in a different timer bucket, adds a read to a cold auction. Enough of them, together with Trusted Server's own reads, exceed the limit, and the providers past the limit fail, likely as `store is missing`.
- The 160-backend startup bound counts backends, not Secret Store reads, so it does not enforce this budget.

The guide states this budget. The staging run in [Qualification](#qualification) measures it with the intended number of mTLS providers.

## Certificates

Public CAs are removing client-authentication certificates under the Chrome root program rules (Let's Encrypt stopped on July 8, 2026; DigiCert's last date is March 1, 2027), and Fastly does not issue client certificates. A private CA issues the client certificate, and the party that verifies it owns that CA: the publisher for a self-hosted PBS, the host for a third-party PBS.

The recommended flow, and the one the example follows, is an offline CA:

1. The PBS operator keeps a CA certificate and key offline, in a password manager or secrets manager.
2. Trusted Server's operator generates the client key and a certificate signing request locally.
3. The CA signs a leaf certificate with the `clientAuth` extended key usage and about a one-year lifetime.
4. The client key goes only into the Fastly Secret Store. The client certificate goes into the Fastly Config Store.
5. The CA certificate goes to S3 for the ALB trust store.

The PBS server certificate stays the public ACM certificate on the ALB, so Trusted Server needs no custom CA to verify it.

## PBS AWS example

mTLS in `deploy/pbs-example` is optional and off by default. Nothing changes for existing users who don't set the new inputs.

### Inputs

At the root, following the existing per-region pattern (`east_certificate_arn`, `west_certificate_arn`):

| Input                                                             | Purpose                                  |
| ----------------------------------------------------------------- | ---------------------------------------- |
| `east_client_ca_bundle_s3_bucket`, `east_client_ca_bundle_s3_key` | CA bundle for the us-east-1 trust store  |
| `west_client_ca_bundle_s3_bucket`, `west_client_ca_bundle_s3_key` | CA bundle for the us-west-2 trust store  |
| `east_connection_logs_bucket`, `west_connection_logs_bucket`      | Optional buckets for ALB connection logs |

- Each region's bucket and key are both set or both unset; validation rejects one without the other.
- AWS requires connection-log buckets in the load balancer's region, encrypted with SSE-S3, with a bucket policy that lets `logdelivery.elasticloadbalancing.amazonaws.com` write under `AWSLogs/<account-id>/`. The operator creates the bucket and its policy before setting the input. AWS does not document a region rule for the trust store bucket. Per-region inputs avoid depending on either.
- The principal running Terraform needs `s3:GetObject` on the CA bundle. The bundle is one or more PEM CA certificates in a row, with no blank lines between them.
- Terraform does not see a changed object at an unchanged S3 key, so a new CA bundle is uploaded under a new key, and the input changes to that key.
- The regional module receives one optional object per region and keeps its existing inputs.

### Resources

When a region's CA bundle inputs are set, that region gets:

- An `aws_lb_trust_store` loaded from the CA bundle. Its name derives from the regional name and meets AWS's rules: 1 to 32 characters, no leading, trailing or consecutive hyphens, unique in the region.
- `mutual_authentication { mode = "verify", trust_store_arn = ... }` on the existing HTTPS listener.

When a connection-log bucket is set, the ALB writes connection logs there. They record each handshake's result, including why a client certificate was rejected.

The regional module exports `alb_tls_settings`, the listener mode, CA bundle location and connection-log bucket, so the root tests can check that each region receives its own inputs.

In AWS provider 6.64.0, `mutual_authentication` is optional and computed, and the provider ignores a removed `connection_logs` block. Removing either block would leave the setting on in AWS. So the listener always renders `mutual_authentication`, with `mode = "off"` and no trust store when the inputs are unset, and the load balancer always renders `connection_logs` with `enabled` set from the input. The provider requires a `bucket` argument even when logs are off, so it gets an empty string then; the provider sends only `enabled = false` in that case. Turning mTLS off is then an explicit listener change. Without a lifecycle setting, Terraform orders the trust store deletion before the listener update, and the deletion waits for a detachment that never comes, so the trust store sets `create_before_destroy`, which puts the listener update first. Replacing the trust store under the same name, as opposed to updating its bundle in place, needs verification turned off first. An existing deployment without the inputs already has both settings off, so its plan should show no change. A deployment that turned verification or connection logs on outside Terraform sees them turned off; the README warns about that drift.

The listener keeps its ACM server certificate, and the security group allowlist stays. The CA certificate in S3 is public, so this keeps the example's rule that secret values never enter Terraform state. The client key never goes near AWS.

### Development certificates

`deploy/pbs-example/scripts/make-dev-mtls-certs.sh` (bash and `openssl`) writes the following into an ignored output directory:

- a throwaway CA certificate and key
- a client key and certificate with the `clientAuth` extended key usage
- the CA bundle file for S3

It refuses a non-empty output directory, creates files with `umask 077`, and prints that its output is for development only. The CA certificate has `CA:TRUE` and `keyCertSign`. The default output directory is ignored by git, including `.key` files; the root `.gitignore` covers only `*.pem`. A Node test, matching the existing `test-smoke-runtime.mjs` style, runs it in a temporary directory and checks the outputs with `openssl verify`.

### Documentation in the example

`README.md`, `RUNBOOK.md` and `DEPLOYMENT_PLAN.md` cover:

- who owns the CA, and the issuance and rotation steps
- uploading the CA bundle
- turning verify mode on and off, and reading connection logs
- the connection-log bucket policy and the CA bundle format
- that in verify mode a request to the public listener, including `/status`, needs a client certificate, while the target group's HTTP health check is unaffected
- the rule that key material stays out of Terraform

Revocation is documented as replacing the CA bundle or rotating the CA; revocation-list inputs are a follow-up. The `planning-prebid-aws` skill's Terraform reference offers the mTLS option.

## Rotation

The certificate and key rotate by switching slots, with no auction failures:

1. Issue a new certificate and key from the same CA. Check that the key matches the certificate and that the verifier accepts the pair.
2. Write the new key, chain and hosts to the idle slot, for example `auction-providers-green`. No request reads that slot, so these writes cannot fail an auction.
3. Wait for the entries to reach Fastly POPs. Staging measured 30 to 115 seconds per write; five minutes is a safe margin.
4. Push the config with `mtls_slot = "green"`.
5. Confirm with auctions, and in the ALB connection logs, that the new certificate is presented.
6. Keep the old slot's entries until the switch is confirmed. Switching back is the rollback.

Rewriting the active slot in place is not safe. EdgeZero reads the chain from the Config Store on every inbound request and opens the key from the Secret Store when it registers a backend, which without `reusable-sandbox` is every request. The two stores propagate independently, so between the writes each POP pairs a new key with an old chain or the reverse, and every `mtls` auction there fails until both writes arrive. A staging run of key-first rotation measured about three minutes of failures.

Slots avoid that window. During the config push each request reads one slot or the other, and both hold a complete, matching pair that the verifier trusts. EdgeZero names a backend after the identity and its chain, so the two slots never share a backend, and with `reusable-sandbox` a sandbox keeps the old slot's valid backend until it loads the new config.

Alternatives considered:

- **Key and chain in one Secret Store entry.** Fastly's `provide_client_certificate` takes the key only as a Secret Store handle, so EdgeZero would have to read the key as plaintext and rebuild a secret from it. This design keeps the key unread.
- **A version pointer inside one identity**, such as `auction-providers.active`. It needs an EdgeZero change and does what the Trusted Server config already does.
- **`reusable-sandbox` alone.** It shortens the window but does not remove it.

Changing CAs takes three steps, each applied and checked before the next: upload a bundle with both CAs under a new S3 key and point the input at it; switch the client certificate; upload a bundle with only the new CA under another new key.

## Rollout and rollback

Older binaries reject `mtls`, and Fastly loads the shared config before it assembles the app, so a config with `mtls` that reaches an older binary breaks every request in that sandbox that loads the config, not only auctions. Requests that skip settings, such as `/health`, are unaffected, and a warm sandbox keeps its previous config until it is recycled. Rollout therefore deploys the binary first.

Rollout:

1. Deploy the Trusted Server version that supports `mtls`, with no `mtls` in the config. Confirm every version serving traffic is on it.
2. Provision the three Fastly entries for the `blue` slot.
3. Push the config with `mtls = true`. With `reusable-sandbox`, a sandbox keeps its loaded settings until it is recycled, so allow for that before relying on the change.
4. Turn on verify mode in one region. Confirm that auctions routed there succeed and the connection logs show the certificate accepted.
5. Turn on verify mode in the other region.

Rollback reverses the order:

1. Turn verify mode off on the ALBs before Trusted Server stops presenting the certificate.
2. Push a config without `mtls`.
3. Only then deploy a binary older than this change.

## EdgeZero dependency

The seven EdgeZero workspace dependencies in the root `Cargo.toml` and the `edgezero-store-aws` dev-dependency in `trusted-server-adapter-axum` move from `rev = "9d02d7b3"` to `tag = "v0.0.12-rc-identity.202611.1"`. That tag is `rc/identity` with #522 and #524 merged. The bump, with any changes the newer EdgeZero requires, is its own commit ahead of the feature.

## Documentation

| Page                                                                             | Content                                                                                                                                            |
| -------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| `docs/guide/auction-orchestration.md`                                            | `mtls` in the provider table and configuration example; the https rule; the backend-bound note; failure behavior                                   |
| A new section on provider mTLS in the same guide, or its own page linked from it | Certificates and the offline CA flow, the three Fastly entries, the Secret Store budget, rotation, troubleshooting, and the adapter support status |
| `docs/guide/configuration.md` and `trusted-server.example.toml`                  | The `mtls` field, commented out in the example                                                                                                     |
| `fastly.toml` `[local_server]` and the integration-test Viceroy template         | Commented local entries for the identity                                                                                                           |
| `docs/guide/integrations/prebid.md`                                              | A short note and a link to the provider mTLS section                                                                                               |
| `docs/guide/error-reference.md`                                                  | The startup and CLI error from `reject_auction_mtls`, the plan-compilation error for an `http` endpoint, and the runtime log line with its reasons |

Examples use fictional hosts such as `pbs.example.com`, under the repository's domain policy.

## Testing

| Area              | Evidence                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| ----------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Config and plan   | `mtls` parses; it is omitted from serialized output while `false` and kept when `true`; the plan carries it; `mtls` with an `http` endpoint fails to compile; the Fastly backend bound counts providers that differ only in `mtls` as distinct                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| Request wiring    | A provider with `mtls = true` sends its request with the active slot's identity, `auction-providers-blue` by default and `auction-providers-green` with `mtls_slot = "green"`; one without sends none; other request properties are unchanged                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| Failure behavior  | A client-identity error in one provider's slot settles as that provider's transport failure with no bids, other providers still return bids, and the request is not retried. The log includes the identity name and reason                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| Adapter assembly  | Axum, Cloudflare and Spin fail assembly when any provider sets `mtls = true`, naming it, including with the auction disabled; they assemble normally otherwise. Fastly assembles with it. The CLI wrapper tests show `ProviderlessAppConfig` rejects it and `CheckedAppConfig` accepts it; the source shows `push` and `diff` pick the wrapper by target adapter                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| Manifest          | The tracked manifest declares both slot identities and lists `outbound-client-identity` as optional; the Fastly app's `Hooks::manifest()` is `Present` and its identity policy contains both                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| Fastly end to end | An integration test under Viceroy runs an auction with a `prebid-server` provider, routed by explicit bidder routes, against a local mock `/openrtb2/auction` that requires client certificates, beside a plain loopback bidder. The test generates its certificates, provisions the three local entries, and trusts its own server CA. With `mtls = true` the PBS provider's seats return bids. Without the identity, the mock rejects the handshake and only the plain bidder's bids return. With `mtls_slot = "green"` and only the green slot provisioned, the PBS provider bids, proving the switch selects green. With the key missing, the mock observes no handshake or request (the source orders the binding before any send), only the plain bidder's bids return, and the guest logs the identity name and reason once. Assertions attribute each bid's seat to its provider. The cases pass on stock Viceroy 0.22.0, the version the repository pins |
| PBS example       | Mocked `terraform test` cases: no trust store and `mode = "off"` without inputs; a trust store and verify mode with them; going from verify back to off; validation rejects half-set inputs; connection logs enabled only when a bucket is set; the trust store name at the 28-character regional name boundary. The development script test                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| Repository gates  | The gates in `AGENTS.md`, the integration tests, the PBS example checks, and the docs checks                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |

## Qualification

After merge, an operator deploys the PBS example on a staging account, points a staging Fastly service at it, and follows [Rollout and rollback](#rollout-and-rollback). The operator records on #1650:

- auctions complete with the client certificate
- a handshake without it is rejected, as shown in the connection logs
- a key-first certificate rotation works
- cold auction requests with the intended number of mTLS providers stay within Fastly's Secret Store read limit
- turning verify mode off works before the trust store is removed

This run does not gate the PR.

## Issue coverage

| #1650 item                                                             | Met by                                                                                                |
| ---------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| Config parsed, validated and documented; no behavior change without it | [Configuration](#configuration), [Adapter support](#adapter-support), [Documentation](#documentation) |
| OpenRTB requests present the identity                                  | [Request wiring](#request-wiring)                                                                     |
| Mock PBS integration test, with and without the identity               | [Testing](#testing)                                                                                   |
| Response correlation with the identity set                             | Batch slots stay index-aligned; covered by the failure-behavior and end-to-end tests                  |
| PBS example optional verify mode, tests, script and docs               | [PBS AWS example](#pbs-aws-example)                                                                   |
| Example configs and guides                                             | [Documentation](#documentation)                                                                       |
| Staging run against the real ALB                                       | [Qualification](#qualification), after merge                                                          |

#1650's description is rewritten to match this spec, with the setting on auction providers rather than `[integrations.prebid]`. The PR body says `Closes #1650`. GitHub closes issues only for PRs merged into the default branch, and this PR targets a stack branch, so #1650 is closed by hand when the change reaches `main`. The staging run is recorded on the issue.

## Branch and delivery

The branch `feat/1650-auction-provider-mtls` stacks on [#1540](https://github.com/IABTechLab/trusted-server/pull/1540) (`baseline/aws-examples-20261110`), the top of the baseline stack, which already contains the EdgeZero cutover from #1506.

There is one PR, with commits in this order:

1. This spec.
2. The implementation plan, written after the spec is approved.
3. The EdgeZero dependency bump.
4. Configuration, plan, manifest, baked hook, request wiring, diagnostics, and the adapter and CLI checks, in one commit. No commit accepts `mtls = true` on Fastly without presenting the certificate.
5. The Fastly integration test.
6. The PBS example.
7. Documentation.

A separate issue in `trusted-server-deployer` covers provisioning the three entries in deployments.

## Review log

- Round 1 (Pi, gpt-6.2-sol, fresh session, before implementation): found that removing the ALB blocks would not turn verification or logging off; that the Secret Store budget is one read per cold backend registration, not per identity; that rotation must write the key first and rollback must replace the config before older binaries; that the feature commits would briefly accept `mtls` on Fastly without presenting it, that disabled plans and `ts config push`/`diff` also needed the adapter check; that the auction log hid the identity reason; that the integration test needed the `prebid-server` profile and stock Viceroy; and AWS naming and bucket prerequisites. All addressed in this spec and the plan.
- Round 2 (Pi, gpt-6.2-sol, fresh session, against the implementation): two blocking findings. The rollout pushed `mtls` before the supporting binary was deployed, and the spec had no privacy and consent analysis. Added binary-first rollout and the Privacy and consent section, narrowed the failure reasons to those EdgeZero emits, and fixed test wording and stale plan text. No code defects.
- Round 3 (Pi, gpt-6.2-sol, fresh session): one blocking finding. The spec said a mismatched key always fails registration and also that the bad backend stays cached. Rotation and failure behavior now describe both host outcomes and keep key-first rotation. Also aligned the read budget with timer buckets and retained backends, the `:443` origin in host-mismatch reasons, the README warning about settings enabled outside Terraform, and the RUNBOOK wait for config adoption. No code defects.
- Round 4 (Pi, gpt-6.1-terra, fresh session): no blocking findings. Confirmed the round 3 fix. One nonblocking wording contradiction about timeouts in the same timer bucket, fixed in the spec and guide. The reviewer checked the cited EdgeZero source, the adapter, CLI and manifest tests, the Terraform blocks, the Viceroy assertions and the RUNBOOK against the spec, and stated that this was not an exhaustive line-by-line pass of the whole diff.
