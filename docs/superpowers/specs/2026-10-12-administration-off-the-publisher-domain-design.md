# Design Spec: Administration leaves the publisher's domain

**Status:** Proposed, 2026-10-12. Implemented on the fork as jwrosewell/trusted-server#8, ready for review, answering IABTechLab/trusted-server#1271. This pull request (#1084) carries every design document of the stack in one place.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`
**Related PRs:** #1094, jwrosewell/trusted-server#8
**Last updated:** 2026-10-12

> **Why this spec exists.** Four things are true of a deployment today.

## 1. Problem

Four things are true of a deployment today.

1. One shared password can replace the signing keys and empty the cache, from anywhere. `POST /_ts/admin/keys/rotate`, `POST /_ts/admin/keys/deactivate` and `POST /_ts/admin/cache/purge` sit on the publisher's own domain behind HTTP Basic authentication, with one username and one password in the settings, no roles and no rate limit. The purge handler's own comment calls an unbounded flush behind one shared static credential an origin stampede lever.
2. The running service holds a token for its own platform's API, read from a secret store named `api-keys`, so that it can write its own config store and secret store. A fault in the service is a fault with write access to the service's settings and keys.
3. Any reader's identity record can be read with that password, at `GET /_ts/admin/ec/{id}`.
4. The gate costs the publisher's own site. A `[[handlers]]` pattern broad enough to cover `/_ts` answers `/_ts/page-bids` with `401` for every reader, bot protection skips every path beneath `/admin/` on the publisher's site, and every admin route claims every HTTP method so that a `GET` does not carry the password to the origin.

## 2. Design

| **What replaces each thing.**                                           | Removed                                                                                                                                                                    | In its place |
| ----------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------ |
| `POST /_ts/admin/keys/rotate` and `POST /_ts/admin/keys/deactivate`     | `ts keys rotate` and `ts keys deactivate`, which run the same rotation code in core and write the two stores through the `fastly` CLI with the token in `FASTLY_API_TOKEN` |
| `POST /_ts/admin/cache/purge`                                           | `ts cache purge --service-id <id>` with `--all` or `--page <url>`, which runs `fastly service purge --key` with the surrogate key core tags a cached page with             |
| `GET /_ts/admin/ec`, `GET /_ts/admin/ec/{id}` and `GET /_ts/admin/eids` | `GET /_ts/data` (jwrosewell/trusted-server#4), which shows a reader what is held against that reader's own Edge Cookie                                                     |
| The Fastly management API client and the `api-keys` secret              | Nothing. The service writes none of its stores                                                                                                                             |
| `[request_signing] config_store_id` and `secret_store_id`               | `--config-store-id` and `--secret-store-id` on `ts keys`. Only the removed handlers read the two settings                                                                  |
| `[[handlers]]` and HTTP Basic authentication                            | Nothing in this server. A password on a path of the publisher's own belongs at the origin or in the platform                                                               |

**What the two commands take care of.** The routes ran inside the service, where a store is read locally. The commands reach the same stores through the platform's API, so three things are handled that the routes did not have to handle.

- **A read that fails is not a missing entry.** The rotation code in core takes any failed read for an entry that does not exist. Through an API a read can also fail for a bad token or a network fault, and read that way a rotation would write a key list holding the new key alone, and a deactivation could take the current key out of the active set. The command's stores tell a `404` from any other failure and write nothing once a read has failed.
- **A delete can be run again.** The service's management client took a `404` on a delete for success, which is how a delete tried again after a partial failure finished. The command does the same, after asking for the store, because the API also answers `404` for a store that does not exist.
- **A value is never read as a flag.** Every value is joined to its flag with `=`. A key being retired may begin with `-`, since the rule for retiring is the looser one the deactivate route applied, kept so that a key created under an earlier rule can still be removed.

Neither command takes `current-kid` or `active-kids` for a key. They are the two entries the config store keeps its key list in, and named as a key either would be written over or deleted.

`closed_path_response`, in core's new `closed_paths.rs`. Every path beneath `/_ts/admin`, and the `/admin/keys` alias beside it, is still answered `404` by the server before the publisher fallback, on all four adapters. The matcher is the one that was there before, which tests a path as sent and after each of up to four percent-decoding rounds. An operator's script that was not updated sends its password and its key payload to a path that answers `404`, and never to the origin. Each adapter has a test that sends the credentials a caller would, and removing the check from any one adapter fails that adapter's tests (faults 8 to 11 below).

## 3. What is not in this change

- Four debugging aids that also live on the publisher's domain, all off unless a publisher switches them on. They are of the same kind as what is removed here and are left for a decision of their own.
  - `GET /_ts/set-tester` and `GET /_ts/clear-tester`, which set and clear a marker cookie when `[tester_cookie]` is on.
  - `GET /_ts/debug/ja4`, which shows a caller the TLS fingerprint of its own connection when `[debug] ja4_endpoint_enabled` is on.
  - `[debug] auction_html_comment`, which writes auction internals into a page as an HTML comment.
  - `[debug] inject_adm_for_testing`, which switches on a test path that carries raw creative markup.
- `ts keys` and `ts cache purge` drive the Fastly CLI alone, as the routes they replace were served by the Fastly adapter alone.

## 4. Upgrade

- A configuration that carries `[[handlers]]` is refused when it is validated and when the service starts, with directions. Loading it and ignoring the list would leave an operator who gated a staging site believing it was still gated.
- A `[[handlers]]` entry could also gate one of this server's own routes, `GET /_ts/debug/ja4` for instance. Nothing here replaces that, so a route of the server's that should not be public stays switched off.
- A configuration that carries `config_store_id` or `secret_store_id` under `[request_signing]` is refused the same way.
- A build before this change refuses a configuration without a handler covering `/_ts/admin`, and one with `[request_signing]` and no store ids. So no one configuration starts both builds, and the new configuration and the new build go live together. A staged deploy reads a configuration key of its own (`ts deploy --staging`, `ts config push --staging`), which lets the pair be checked together first.
- `ts cache purge` takes `--service-id` and `FASTLY_API_TOKEN` where it took `--service <url>` and `TRUSTED_SERVER_ADMIN_PASSWORD`. It runs `fastly service purge`, which the Fastly CLI has had since version 14.0.0. The repository pins 15.1.0.

If refusing is too abrupt for a live deployment, the alternative is small. The refused keys could load, be ignored and log a warning for one release, and be refused in the next. This pull request refuses, because an ignored `[[handlers]]` protects nothing and says nothing to whoever reads the configuration.

## 5. Acceptance

The pull request's description records how it was checked, with every commit linted and tested on its own, in order, the full local check on its head and CI green on all 19 checks, and 36 deliberate faults made one at a time and each caught by a test.

## Revision record

| Date       | Change                                                          |
| ---------- | --------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand. |
