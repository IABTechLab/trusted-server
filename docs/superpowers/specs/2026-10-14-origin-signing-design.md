# Design Spec: An origin signer is a module a source's auth table names

**Status:** Proposed, 2026-10-14. Implemented on a working branch of the fork, with no pull request raised yet.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-14-source-modules-design.md`, `2026-10-13-publisher-object-storage-origin-design.md`, `2026-05-19-asset-s3-auth-fastly-io-design.md`
**Related PRs:** none yet
**Issues:** IABTechLab/trusted-server#1338
**Last updated:** 2026-10-14

> **Why this spec exists.** Core signs reads from the publisher's origin and from each asset route's origin with one cloud vendor's signing scheme, and its settings know that scheme's credentials, so an origin that wants another scheme has to add a branch to core.

## 1. Problem

`crates/trusted-server-core/src/s3_sigv4.rs` signs a read from object storage with AWS Signature Version 4, and the `auth` table of a source's `origin` settings is a typed enum with one variant, `s3_sigv4`, holding that scheme's region and credential key names. The publisher path and the asset routes sign through that one function, the deploy check knows the three credential fields by name, and the configuration page masks them because core declares them.

So core knows one vendor's scheme. A signed read from another vendor's storage, or with another scheme, is a change to core's settings, its signing call and its secret declarations, for every deployment.

## 2. Design

**Core keeps an origin authentication interface.** `trusted_server_core::origin_auth` declares `OriginSigner`, which signs the headers of a request for a method on a final address, and removes every header in the scheme's own namespace so a reader's header there cannot reach the origin beside the signature. Core calls it where it signed before, in the asset route path, in its preflight ahead of the image optimizer, and in the publisher path, through the `OriginSigning` a `SourceLocation` carries.

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

Core reads `type` and `origin_query` and refuses a table that names no signer, or one in a form no crate could offer. The rest of the table is handed to the signer as written.

**A crate offers a signer on its builder.** `IntegrationBuilder::with_origin_signer(&MODULE)` offers an `OriginSignerModule`, being the name an `auth` table writes, the settings of its table that hold the name of a secret, each with the key name it takes when left out, and the function that builds the signer from its table. The offer is not a selection. A signer is built where a source names it, when the registry builds the sources from the settings, so a `type` nothing offers is refused at startup naming the signers on offer, and a table the signer cannot use is refused there naming the setting, as `[proxy.asset_routes /assets/ source.origin.auth] s3_sigv4 region must not be empty`.

**The signer's secrets are looked up and masked as core's are.** The secret settings a signer declares are declared to the settings load at both places an `auth` table can be written, `source.origin.auth` and `proxy.asset_routes[].source.origin.auth`, so each is looked up in the default secret store as the settings load and masked on the configuration page. A setting with a default key name that the table leaves out is written in before the lookup, so it is looked up under that name as it was when core's settings typed it. A written setting that names no key is refused by `ts config validate` naming its path.

**AWS signing is a shared vendor crate.** `crates/shared/aws`, the package `trusted-server-shared-aws`, holds the signer and its settings. It offers `s3_sigv4`, reading `region`, `access_key_id`, `secret_access_key` and `session_token` as before, with the same defaults, and signs with header-based authentication and `UNSIGNED-PAYLOAD` as before. The stock build lists its builder in `crates/trusted-server-modules`, so every adapter and the `ts` tool offer it, and its own deploy rules run with the stock modules'.

What leaves core: the signer, its settings type, the credential fields in core's secret declarations, the credential check in the deploy validation, and the preflight's knowledge that the signed origin is S3. What stays: the `https` requirement of a signed source, the `405` for a method that is not a read, the reader headers a signed read never carries, and `origin_query`.

## 3. What is not in this change

- A source module from a vendor crate signing through a signer another crate offers. Core's `origin` source is handed the signers on offer when it is built. A vendor source that signs holds its own signer.
- The deprecated spelling `s3_sig_v4`, which core's settings read as an alias. A `type` is matched as written, and a document carrying the old spelling is refused naming the names on offer.

## 4. Tests

- An `auth` table reads its `type` and `origin_query` and keeps the rest for the signer, serializes as written, and refuses a missing or malformed `type` naming the setting.
- A `type` nothing offers is refused with the names on offer, and with none on offer says so. A table the signer cannot use is refused naming the setting, for the publisher's source and for an asset route's, naming the route.
- Two builders offering one signer name are refused.
- A signer's secret settings are declared at both `auth` paths, a left out setting with a default is written in before the lookup, a written setting naming no key fails deploy validation naming its path, and a loaded configuration masks every one on the configuration page and serves none.
- The asset route path signs the final address and `Host`, strips the transform query before signing, removes a reader's header in the signer's namespace, preflights a signed origin with a `HEAD` before the image optimizer, signing each request as its own method, and returns the origin's own error.
- The publisher path signs exactly the address and `Host` it sends, as a `GET` or a `HEAD`, carries none of the reader's private headers, strips the query when the table says so, and refuses a method that is not a read.
- The `s3_sigv4` signer signs the published example to the published signature, signs a session token, refuses a request without a `Host`, and canonicalizes paths and queries as before. Through the registry, it signs a publisher source and an asset route, refuses a table it cannot use at startup and at deploy validation, is refused where no build offers it, and its credentials are looked up at both `auth` paths and masked.
