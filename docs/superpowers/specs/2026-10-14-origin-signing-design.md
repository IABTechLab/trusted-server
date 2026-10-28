# Design Spec: An origin signer is a module a source's auth table names

**Status:** Proposed, 2026-10-14. Raised as draft pull request jwrosewell/trusted-server#43, and as draft pull request IABTechLab/trusted-server#1391, for Tech Lab review.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-14-source-modules-design.md`, `2026-10-13-publisher-object-storage-origin-design.md`, `2026-05-19-asset-s3-auth-fastly-io-design.md` and `2026-10-14-no-modules-crate-design.md`, all in IABTechLab/trusted-server#1342, which holds every design document of the stack
**Related PRs:** jwrosewell/trusted-server#43, IABTechLab/trusted-server#1391
**Issues:** IABTechLab/trusted-server#1338
**Last updated:** 2026-10-15

> **Why this spec exists.** Core signs reads from the publisher's origin and from each asset route's origin with one cloud vendor's signing scheme, and its settings know that scheme's credentials, so an origin that wants another scheme has to add a branch to core.

## 1. Problem

`crates/trusted-server-core/src/s3_sigv4.rs` signs a read from object storage with AWS Signature Version 4, and the `auth` table of a source's `origin` settings is a typed enum with one variant, `s3_sigv4`, holding that scheme's region and credential key names. The publisher path and the asset routes sign through that one function, the deploy check knows the three credential fields by name, and the configuration page masks them because core declares them.

So core knows one vendor's scheme. A signed read from another vendor's storage, or with another scheme, is a change to core's settings, its signing call and its secret declarations, for every deployment.

## 2. Design

**Core keeps an origin authentication interface.** `trusted_server_core::origin_auth` declares `OriginSigner`, which signs the headers of a request for a method on a final address, and removes every header in the scheme's own namespace so a reader's header there cannot reach the origin beside the signature. Core calls it where it signed before, in the asset route path, in the preflight that path runs ahead of the image optimizer, and in the publisher path, through the `OriginSigning` a `SourceLocation` carries. The publisher path is core's own, so it signs on all four adapters. Asset routes and the preflight run on the Fastly adapter alone, because only that adapter calls `handle_asset_proxy_request`.

Three things differ from what core did before. First, the `HEAD` preflight, which asked an S3 origin for the object before the image optimizer did, now runs for any signed asset route that uses the optimizer, whatever its signer. Second, removing the headers in the signer's namespace is each signer's duty, so core calls `remove_namespace_headers` before `sign` on the publisher path and the asset route path, and does not check afterwards that none is left. Third, the asset route path's call to `remove_namespace_headers` is new, because that call was made on the publisher path alone. The asset route path starts its outbound headers from eleven named request headers (`ASSET_PROXY_FORWARD_HEADERS` in `crates/trusted-server-core/src/proxy.rs`) and none of them is in the `x-amz-` namespace, so for `s3_sigv4` the call removes nothing. The preflight signs its `HEAD` and its `GET` without calling `remove_namespace_headers`, which also makes no difference for `s3_sigv4`, because it signs copies of those same headers and `Host`.

**A source's `auth` table names the signer.** The table keeps its shape. `type` names the signer, `origin_query` says whether the reader's query is kept on the signed request, and every other key belongs to the signer named.

```toml
[source.origin]
url = "https://bucket.storage.example.com/site"

[source.origin.auth]
type = "s3_sigv4"
region = "example-region-1"
access_key_id = "origin_access_key_id"
secret_access_key = "origin_secret_access_key"
# session_token = "origin_session_token"
# origin_query = "strip"
```

Core reads `type` and `origin_query` and hands the rest of the table to the signer as written. It trims `type` and refuses a table that names no signer, or one whose `type` could not be a crate's module name, which is parts joined by `.`, each of lower case letters, digits, `_` or `-`. `origin_query` is `preserve` or `strip`. On the publisher's source, leaving it out keeps the reader's query. On an asset route the value in the `auth` table wins, then the `origin_query` of an enabled image optimizer, then the route's default, which is `strip` when the image optimizer is enabled and `preserve` otherwise.

**A crate offers a signer on its builder.** `IntegrationBuilder::with_origin_signer(&MODULE)` offers an `OriginSignerModule`, being the name an `auth` table writes, the settings of its table that hold the name of a secret, each with the key name it takes when left out, and the function that builds the signer from its table. The offer is not a selection. A signer is built where a source names it, when the registry builds the sources from the settings, so a `type` nothing offers is refused at startup naming the signers on offer, and a table the signer cannot use is refused there naming the setting, as `[proxy.asset_routes /assets/ source.origin.auth] s3_sigv4 region must not be empty`.

**The signer's secrets are looked up as core's are, and masked through the settings load's record.** The secret settings a signer declares are declared to the settings load at both places an `auth` table can be written, `source.origin.auth` and `proxy.asset_routes[].source.origin.auth`, so each is looked up in the default secret store as the settings load. A setting with a default key name that the table leaves out is written in before the lookup, so it is looked up under that name as it was when core's settings typed it. A written setting that is empty, only spaces or not text is refused by `ts config validate` naming its path. On a running adapter, the configuration page masks these values through the record the settings load keeps of the secrets it looked up, and through nothing else. Core's own secret declarations no longer name a signer's settings, and the values sit as plain text in the signer's table and no longer in `Redacted` fields, which mark themselves for the page. Every adapter loads its settings with the builders it links, so each masks the settings of the signers it links.

**AWS signing is a shared vendor crate.** `crates/shared/aws`, the package `trusted-server-shared-aws`, holds the signer and its settings. It offers `s3_sigv4`, reading `region`, `access_key_id`, `secret_access_key` and `session_token` as before, with the same defaults, and signs with header-based authentication and `UNSIGNED-PAYLOAD` as before. The changes to the settings a deployment writes are listed under "What changes for settings that name `s3_sigv4`". The crate's builder, `trusted_server_shared_aws::builder()`, supplies no page module and carries no deploy rules of its own. Each adapter lists the builder in `shipped_module_builders()` in its `app.rs`, and the `ts` tool lists it in `linked_builders()` in `crates/trusted-server-cli/src/app_config.rs`, which is how `2026-10-14-no-modules-crate-design.md` has each build list the modules it links. Core checks an `auth` table against the signer it names at deploy validation, for every signer a builder offers.

What leaves core: the signer, its settings type, the credential fields in core's secret declarations, the credential check in the deploy validation, and the preflight's knowledge that the signed origin is S3. What stays: the `https` requirement of a signed source, the `405` for a method that is not a read, the reader headers a signed read never carries, and `origin_query`.

**Nothing consent related changes.** This change touches no permission, consent or identifier code. The ten reader headers that a signed publisher read never carries, being `Cookie`, `Authorization`, `Proxy-Authorization`, `Referer`, `Range` and the five conditional headers, are the same ten as before, and the publisher path's signing step removes no other reader header, so the rest reach the origin as they did before, apart from any the signer removes from its own namespace. The asset route path still forwards only its eleven named headers, which include no cookie.

**When each mistake in an `auth` table is refused.** Three stages refuse a mistake in an `auth` table.

1. Reading the settings, which `ts config validate`, `ts config push` and an adapter's start all do first, refuses a table with no `type`, a `type` that is empty or could not be a module name, and a signed `url` that is not `https`, each naming the setting. That stage passes any other `type`, whether a builder offers it or not.
2. The deploy check, which `ts config validate` and `ts config push` run on the settings that were read, looks at each table whose `type` is a signer that the builders linked into the `ts` tool offer. It refuses a secret setting that is empty, only spaces or not text, naming its path, and it builds the signer from the table, so whatever the signer cannot use is refused naming the setting. It leaves alone a table whose `type` none of those builders offers.
3. An adapter's start builds the adapter's state, and with it the signer for every table, so that stage refuses what the signer cannot use and also a `type` that no builder the adapter links offers, naming the signers on offer.

**The gap.** A `type` that could be a module name but that no builder offers passes the first two stages and is refused only by the third. A misspelled `type`, the removed spelling `s3_sig_v4`, and the name of a signer that the `ts` tool does not link all pass `ts config validate` and `ts config push`, are stored, and then stop the adapter when it builds its state. That adapter answers each request with its startup error response, which is a `500` with the body "An internal error occurred" on Fastly, Axum and Cloudflare and a generic `503` on Spin, and it logs the full error. The exceptions are `GET /health`, which Fastly and Spin still answer with `200`, and the debug probes that Fastly answers before it builds the application. Before this change core refused any `type` other than `s3_sigv4` and `s3_sig_v4` while the settings were read, so the gap is new. The skip is deliberate, and the test `deploy_validation_leaves_a_signer_s_settings_alone_without_its_builder` in `crates/trusted-server-core/src/config.rs` pins it for a table whose signer's builder is not among those offered.

**Open question for the reviewer.** Should deploy validation close this gap or keep it? Closing the gap means refusing a `type` that no builder the `ts` tool links offers, so a misspelled `type` and the removed `s3_sig_v4` fail `ts config validate`. A deployment whose adapter links a signer of its own could then no longer push with the stock `ts` tool, which does not link that crate. The integration guide already describes the same limit for a module a deployment adds. Keeping the gap leaves a signer of the deployment's own pushable with the stock `ts` tool, and leaves a mistaken `type` to stop the adapter when it builds its state.

**When signing fails for a request.** A signer that cannot sign a request returns its error, and core does not send the request it could not sign. The asset route path, its preflight and the publisher path hand that error to the adapter, which answers with the status the error's kind gives, as it does for any other error from the same handler. The `s3_sigv4` signer returns `502 Bad Gateway` when the request has no `Host` header and `400 Bad Request` when a header value is not valid text, with the bodies "An internal error occurred" and "Invalid header value". The asset route path marks that answer private and not stored.

**What changes for settings that name `s3_sigv4`.** Three things change for a `trusted-server.toml` document that writes an `s3_sigv4` table:

1. The spelling `s3_sig_v4` is no longer read, and the removal is a breaking change. Before this change core read it as an alias and wrote `s3_sigv4` into the pushed configuration. A document that still carries it now passes the first two stages above and stops the adapter, as the gap describes.
2. A blank `session_token` is refused. Before this change, reading the settings trimmed a blank `session_token` to none, so it passed `ts config validate` and the signer used no token. `ts config validate` now refuses a written `session_token` that is empty or only spaces, naming its path, as it already refused a blank `access_key_id` or `secret_access_key`. The signer still reads a blank value that it is handed as no token.
3. A deprecated `secret_store` key is kept. Before this change a `secret_store` key was accepted and left out of the pushed configuration. A table now keeps every key it is written with, so `secret_store` is pushed as written, and the signer ignores it and logs a warning each time it builds.

**Rollout.** A build that composes its own list of modules adds `trusted_server_shared_aws::builder()` to the list. Without it a `type` of `s3_sigv4` stops that build's adapter at startup, naming the signers on offer, and a `ts` tool built the same way skips every `s3_sigv4` table at deploy validation. The keys of an `s3_sigv4` table and what they mean are unchanged, so the new build reads a configuration the old tool pushed, the old build reads one the new tool pushed, and the tool and the adapter build can be upgraded in either order. The exception is the spelling `s3_sig_v4`. The old tool stored it as `s3_sigv4` and the new tool stores it as written, so a document that carries it is corrected to `s3_sigv4` before it is pushed with the new tool.

## 3. What is not in this change

- A source module from a vendor crate signing through a signer another crate offers. Core's `origin` source is handed the signers on offer when it is built. A vendor source that signs holds its own signer.
- Refusing a `type` that no builder offers at deploy validation, which the open question above leaves to the reviewer.

## 4. Tests

- An `auth` table reads its `type` and `origin_query` and keeps the rest for the signer, trims `type`, serializes as written, and refuses a missing or malformed `type` naming the setting.
- A `type` nothing offers is refused with the names on offer, and with none on offer says so. A table the signer cannot use is refused naming the setting, for the publisher's source and for an asset route's, naming the route.
- Two builders offering one signer name are refused.
- A signer's secret settings are declared at both `auth` paths, a left out setting with a default is written in before the lookup, a written setting naming no key fails deploy validation naming its path, and a loaded configuration masks every one on the configuration page and serves none.
- Deploy validation builds the table of a signer on offer, so a table the signer cannot use fails it naming the setting, and it leaves a table alone when its signer's builder is not among those offered.
- The asset route path signs the final address and `Host`, strips the transform query before signing, preflights a signed origin with a `HEAD` before the image optimizer, signing each request as its own method, and returns the origin's own error.
- The publisher path signs exactly the address and `Host` it sends, as a `GET` or a `HEAD`, carries none of the reader's private headers or of its headers in the signer's namespace, strips the query when the table says so, and refuses a method that is not a read.
- The `s3_sigv4` signer signs the published example to the published signature, signs a session token, refuses a request without a `Host`, reads a blank `session_token` that it is handed as no token, discards a deprecated `secret_store`, and canonicalizes paths and queries as before. Through the registry, it signs a publisher source and an asset route, refuses a table it cannot use at startup and at deploy validation, is refused where no build offers it, and its credentials are looked up at both `auth` paths and masked.
- Each adapter's test `the_linked_modules_are_offered_in_hook_order` lists `shared.aws` after the other builders that adapter links, for Fastly, Axum, Cloudflare and Spin.

Four cases have no test that can fail:

1. The asset route test that sends a reader's header in the signer's namespace cannot fail, because the route builds its outbound headers from eleven named request headers and that header is never among them. The test passes with the route's call to `remove_namespace_headers` removed, and no test gives that call a header it would remove.
2. The test `deploy_validation_leaves_a_signer_s_settings_alone_without_its_builder` in `crates/trusted-server-core/src/config.rs` shows deploy validation leaving a table alone when no builder offers its signer, with the core fixture signer and a blank key name. No test sends deploy validation a `type` that names no signer at all, such as a misspelling or `s3_sig_v4`, or shows the same settings then refused when the adapter builds its state.
3. No deploy validation test gives an `s3_sigv4` table an empty `session_token`. The refusal is tested only through the core fixture signer's `token_name`, a setting declared the same way.
4. No test in the `ts` tool's crate names the AWS builder or an `s3_sigv4` table, so nothing fails if the builder leaves the list in `linked_builders()`, and `ts config validate` would then skip every `s3_sigv4` table.

## Review log

1. **Round 1, 2026-10-15.** A review in a fresh session, against `split/48-aws-signing` at `a3c17125b`. Issues found, two of them blocking. The section on AWS signing named a modules crate that no longer exists and deploy rules the builder does not carry, and the spec left out that a `type` nothing offers passes `ts config validate` and `ts config push` and only stops the adapter at startup, while it listed the removed `s3_sig_v4` spelling under what is not in the change. This revision corrects both, states which stage refuses each mistake, what a failed signature answers, the rollout, the adapters that run each path, the settings that change and the tests that are missing or cannot fail, leaves closing the gap as an open question, and corrects the changelog, integration guide and configuration guide sentences that repeated the stale claims.
2. **Round 2, 2026-10-15.** A lighter review in a fresh session, against this page at `5c43f6f9a`, of the round 1 fixes and the new text. No blocking issues once the pull request number was corrected. This revision names IABTechLab/trusted-server#1342 as the specs pull request, says that the preflight signs without calling `remove_namespace_headers`, leaves `GET /health` and the Fastly debug probes out of the startup error answer, limits the missing test for an empty `session_token` to deploy validation, and corrects the Tinybird entry in the changelog.

## Revision record

| Date       | Change                                                                                                                                                                                                                                                                                                     |
| ---------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-10-14 | First draft.                                                                                                                                                                                                                                                                                               |
| 2026-10-15 | Fixes from review round 1. The stale modules crate and deploy rules claims, the stages that refuse a mistake, the gap and open question for a `type` nothing offers, the breaking removal of `s3_sig_v4`, failure answers, rollout, the adapters that run each path, the missing tests and the review log. |
| 2026-10-15 | Corrections from review round 2. The specs pull request number, the preflight's signing without removing namespace headers, the requests an adapter answers whatever its startup error, the deploy validation test for an empty token and the Tinybird changelog entry.                                    |
