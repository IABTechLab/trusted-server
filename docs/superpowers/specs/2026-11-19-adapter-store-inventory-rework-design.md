# Adapter store inventory rework (#1139)

## Contract

Keep `trusted_server_kv`: identity records need an EdgeZero-declared store.
Wire generic KV on Fastly, Cloudflare and Spin as in #1627; deliberately leave
Axum unsupported (`KvError::Unavailable`) until #1587 supplies the shared
EdgeZero record backend and #1574 enables it on Axum. Do not introduce a new
Axum provider or change EdgeZero. Missing optional KV must not prevent pages
from being served.

## Rework

- Merge `feat/1573-axum-production-serving` (#1635), never rebase; retain its
  prepared-store startup from #1531. Retarget #1627 to that branch. It is the
  appropriate base because it supplies the production Axum startup contract.
- Remove the Axum file KV environment variable, file database wrapper,
  initialization, file-specific dependency and tests. Keep the base's
  unavailable store, with a context-path regression asserting unavailable
  reads and writes. Prepared config and secrets remain immutable snapshots
  loaded once before listening, with no legacy environment fallback.
- Preserve Fastly context-registry KV, Cloudflare's named binding, Spin's
  declared KV and request-time config from `default`, including signing.
  Preserve the existing shared handle bridge and dead fixture cleanup.
- Reconcile every current inventory/guide/README claim and the original design
  and plan with the new Axum contract. Explain that generic KV is infrastructure;
  identity record migration is separate work. Record the maintainer decision in
  the PR. Keep unsupported request-signing responses accurate.

## Acceptance and preservation

| Boundary               | Required evidence                                                                                                                                                                                       |
| ---------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Axum request context   | Prepared snapshots used; generic KV reads/writes return `Unavailable`                                                                                                                                   |
| Axum production binary | Prepared-store startup and publisher page succeed; runtime KV reported unavailable                                                                                                                      |
| Fastly real binding    | Declared store opens and write/read crosses handles; absent binding gives `OpenFailed` while publisher pages succeed                                                                                    |
| Spin config/signing    | Real `spin up` reads distinguishable signing metadata from `default`; captured auction KID matches default metadata and signature verifies against its public key; matching private-key absence control |
| Cloudflare binding     | Existing named-binding and unsupported signing tests still pass                                                                                                                                         |
| Stack                  | Merged Axum builds; #1637 guard executed in a disposable merge with its complete prerequisites; sibling changes not shipped                                                                             |
| Delivery               | Existing PR updated, signed commits pushed, independent full-PR self-review reconciled; CI gates left to GitHub                                                                                         |

Use disposable synthetic configuration and controlled origins. Do not edit
`trusted-server.toml` or `.env`; preserve local setup, back up and restore any
necessary temporary changes. Temporary probe routes may establish generic KV
infrastructure through real runtimes because no production generic KV consumer
exists yet; they must not be shipped or described as identity storage proof.
Stop owned servers and remove owned scratch resources when complete.

## Alternatives

Renaming the file-store variable would satisfy a naming rule but still violate
the declared-store architecture. Implementing a new Axum store duplicates
#1587/#1574. Removing `trusted_server_kv` defeats the chosen identity direction.
Retaining the base's unavailable Axum store is the smallest compatible change.

## Review log

- Round 1: clarified guard prerequisites and disposable merge evidence, fresh-process
  regression isolation, retained startup dependencies, Worker runtime coverage,
  full-tree documentation reconciliation, and cryptographic signing controls.
- Round 2: independent reviews confirmed all blocking and optional findings
  addressed; no unresolved findings before implementation.
