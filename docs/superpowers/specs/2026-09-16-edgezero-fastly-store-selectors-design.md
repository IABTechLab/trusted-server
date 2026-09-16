# EdgeZero Fastly Store Selector Alignment

## Problem

Trusted Server currently depends on EdgeZero `v0.0.8`, whose Fastly staging
deployment flow persists and reads service-scoped runtime store selectors.
Staging can therefore fall back to the logical secret-store name
`trusted_server_secrets` even when the selected GitHub Environment sets the
canonical selector to the physical store `ts_secrets`. Runtime settings then
fail while resolving required secret references such as
`publisher.proxy_secret`.

EdgeZero PR 381 restores canonical deploy-time selectors and makes staged
deployments link the selected physical Config, KV, and Secret Stores. Trusted
Server needs to consume that behavior and remove guidance for the superseded
service-scoped selector model.

## Design

All EdgeZero workspace dependencies will temporarily use the upstream branch
`fix/fastly-environment-store-selectors`. Keeping the dependencies on one branch
ensures the adapter, CLI, core, and platform-specific crates resolve to the same
revision while PR 381 is under review. A follow-up will replace the branch with
the release tag that contains the merged change.

Trusted Server's local Fastly runtime configuration will use the canonical
selector:

```text
EDGEZERO__STORES__SECRETS__TRUSTED_SERVER_SECRETS__NAME=ts_secrets
```

The Fastly and CLI guides will describe GitHub Environment or deploy-process
variables as deployment inputs. They will no longer instruct operators to
construct a selector containing a Fastly service ID or to persist that key
manually. EdgeZero remains responsible for applying the selected store mapping
to production or to the staged version during deployment.

## Failure Handling

Trusted Server will keep failing startup when a required secret reference
cannot be resolved. This change fixes the deployment mapping and resource links
rather than weakening runtime validation or falling back to another store.

The pull request will state that its branch dependency is temporary and link to
EdgeZero PR 381. If upstream changes its CLI API before merge, Trusted Server's
compile and CLI tests will expose the mismatch before deployment.

## Validation

Validation will cover:

- lockfile resolution of every EdgeZero crate to one upstream revision;
- Trusted Server CLI parsing and behavior tests on the host target;
- Fastly adapter compilation and tests where supported by the local toolchain;
- Rust formatting and target-matched Clippy for changed Rust surfaces;
- searches confirming service-scoped store-selector instructions are removed
  from current operator documentation and local Fastly configuration.

The issue will record the staging failure, expected canonical selector behavior,
and the dependency on EdgeZero PR 381. The pull request will close that issue.
