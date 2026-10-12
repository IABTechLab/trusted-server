# Design Spec: One app-config blob for each host

**Status:** Proposed, 2026-10-12. Implemented on the fork as jwrosewell/trusted-server#6, ready for review, answering IABTechLab/trusted-server#1270. This pull request (#1084) carries every design document of the stack in one place.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`, `2026-03-19-edgezero-migration-design.md`
**Related PRs:** #1094, jwrosewell/trusted-server#6
**Last updated:** 2026-10-12

> **Why this spec exists.** A Fastly service reads one application config blob, at the key its `__KEY` selector names.

## 1. Problem

A Fastly service reads one application config blob, at the key its `__KEY` selector names. An operator running the server for several publishers runs a service for each, with its own domains, stores, deploys and rollbacks, although the code is the same and only the settings differ. Two things stand in the way of one service reading a blob for each host. The key is settled for the service before a request is looked at, and a sandbox that serves several requests keeps the one application it built first, so the second publisher's request would be answered with the first publisher's application.

## 2. Design

**The problem.** A Fastly service reads one app-config blob, at the key its `__KEY` selector names or at the logical store ID. An operator who runs Trusted Server for several publishers therefore runs a service for each, with its own domains, stores, deploys and rollbacks, although the code is the same and only the settings differ.

Two things stood between that and one service reading a blob for each host.

1. The key is settled for the service, before a request is looked at.
2. A sandbox that serves several requests keeps the one application it built first. With a blob for each host, the second publisher's request would be answered by the first publisher's application.

```bash
ts config push --adapter fastly --key www.publisher-a.example --app-config publisher-a.toml
ts config push --adapter fastly --key www.publisher-b.example --app-config publisher-b.toml

fastly config-store-entry update --upsert \
  --store-id <runtime-env-store-id> \
  --key "EDGEZERO__SERVICES__${FASTLY_SERVICE_ID}__STORES__CONFIG__TRUSTED_SERVER_CONFIG__KEY" \
  --value '{host}'
```

The entry point puts the request's host in the placeholder's place before anything reads settings, so the debug probes, the application build and the finalizers all read the blob of the publisher the request is for.

| A request for                                               | Reads the blob at         | Is answered                                 |
| ----------------------------------------------------------- | ------------------------- | ------------------------------------------- |
| `www.publisher-a.example`                                   | `www.publisher-a.example` | From publisher A's config                   |
| `WWW.Publisher-A.example:443` or `www.publisher-a.example.` | `www.publisher-a.example` | From publisher A's config                   |
| `www.publisher-b.example`                                   | `www.publisher-b.example` | From publisher B's config                   |
| A host with no blob                                         | None                      | `421 Misdirected Request`, not to be stored |
| A `Host` that is not a host name, or no `Host`              | None                      | `421 Misdirected Request`                   |
| `/health`, for any host                                     | None                      | `200`, before any config is read            |
| Any host, where the store cannot say whether it has a blob  | None                      | `500`                                       |

A selector without the placeholder is the key as it stands and nothing is looked up. A service that reads one blob is unchanged.

**Why the placeholder goes in the selector that already exists.** A switch of its own beside the sandbox bounds is the obvious design. EdgeZero's staged deploy is why this is not built that way.

A staged deploy copies every runtime entry of the service into a store of its own and replaces only the config key selectors, pointing each at `<logical-store-id>_staging` ([`staging_entries_from_production`](https://github.com/stackpop/edgezero/blob/683202c66948146ec360f490126f1d60f920288b/crates/edgezero-adapter-fastly/src/cli.rs#L3579-L3603)). A separate switch would be copied with the rest, and a staged version would then choose by host and read the production blobs. The selector is replaced, so a staged version reads the one staging blob for every host and never a production one.

The selector is also read by [`runtime_env_config`](https://github.com/stackpop/edgezero/blob/683202c66948146ec360f490126f1d60f920288b/crates/edgezero-adapter-fastly/src/lib.rs#L259-L282), whose lookup panics on an error. A selector that cannot be read therefore fails the request, where a switch read on the side would have had to choose between failing and falling back to the logical store ID.

| **What keeps one publisher's settings from answering for another.**                                            | The risk                                                                                                                                                                                                                 | What prevents it |
| -------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------- |
| A host nobody configured is answered from a shared blob                                                        | There is no fallback. A host with no blob is answered `421`                                                                                                                                                              |
| A `Host` header is written to name another key in the store, such as the logical store ID or a chunk of a blob | Only letters, digits, hyphens and dots make a host name, and nothing else is looked up. Those keys each carry an underscore                                                                                              |
| A sandbox that serves several requests hands the first publisher's application to the second                   | The sandbox keeps each application under the key of the blob it was built from, and finds one by that key alone                                                                                                          |
| Something read from one blob is kept for the life of the process                                               | Every process-wide static in core, the modules and the Fastly adapter was read for this. Each is a constant, or is keyed by its content or by the store and key it was read from. None holds anything read from settings |
| A page stored for one publisher is served for another                                                          | The template cache key already carries the request's host and scheme                                                                                                                                                     |
| A staged version reads production blobs                                                                        | A staged deploy replaces the selector, as above                                                                                                                                                                          |
| The store cannot say whether a host has a blob                                                                 | The request is answered `500` and not served                                                                                                                                                                             |

**The reusable sandbox.** `Sandbox` was `lifecycle::Sandbox<RetainedApp>`, one application for the life of the sandbox. It is `lifecycle::Sandbox<RetainedApps>`, which keeps each application under its blob's key.

- EdgeZero's `Sandbox<T>` holds one value, builds it only when empty and hands it out by shared reference, with no way to replace it. So the holder is made once and its list sits behind a `RefCell`. One callback runs to completion before the next begins and no borrow is held across a build.
- It retains only a successful build and counts every attempt, as the framework's slot did. The counters probe, the response headers and the retirement line read the holder's count.
- It keeps the eight applications served most recently. Every application shares the sandbox's one memory bound, so the count is bounded. Eight is a judgment and not a measurement.
- A host whose blob is removed is answered `421` once the store no longer holds it, even while a sandbox still keeps its application, because the store is asked on every request.

**What the publishers on one service share.** - One secret store. Each publisher's blob names keys of its own in it.

- The request signing keys, which belong to the service.
- The sandbox's memory bound, where a sandbox serves several requests.

## 3. What is not in this change

- **Staging by host.** `ts deploy --staging` writes a fixed staging selector, so a staged version of a service that chooses by host reads one blob. A selector of `{host}_staging` works at runtime today, and a staged deploy would have to derive it from a production selector that carries the placeholder. That is a change to EdgeZero's Fastly CLI.
- **The other adapters.** Axum, Cloudflare and Spin read one application config. The Fastly adapter has the request in hand before it reads any settings, which is what makes this a small change there.
- **A publisher with several names** pushes a blob under each. A key that points at another key would save the copies.
- **The bound of eight** is a constant.

## 4. Acceptance

The pull request's description records how it was checked, with CI green on all 19 checks.

## Revision record

| Date       | Change                                                          |
| ---------- | --------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand. |
