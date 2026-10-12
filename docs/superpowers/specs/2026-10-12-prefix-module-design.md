# Design Spec: A module names the browser's cookies and the shared store keys

**Status:** Proposed, 2026-10-12. Implemented on the fork as jwrosewell/trusted-server#22, ready for review, answering IABTechLab/trusted-server#1285. This pull request (#1084) carries every design document of the stack in one place.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`
**Related PRs:** #1094, jwrosewell/trusted-server#22
**Last updated:** 2026-10-12

> **Why this spec exists.** Every cookie core writes is named `ts-<something>`, every first-party storage key the client script writes starts with `ts`, and every key a publisher's entries take in a shared store starts with `ts.<domain>`.

## 1. Problem

Every cookie core writes is named `ts-<something>`, every first-party storage key the client script writes starts with `ts`, and every key a publisher's entries take in a shared store starts with `ts.<domain>`. A deployment that is not Trusted Server, or one host serving several publishers, has no way to give the browser its own names or a shared store its own key space without editing core, and the client script holds a copy of the name it must agree with the server on.

## 2. Design

```toml
[prefix]
module = "example"
```

A prefix module, `PrefixModule`, which gives two things for the publisher a request resolved to: a public prefix that starts every name the browser holds, being cookies and first-party storage keys, and a server key prefix that starts every key in a store publishers share. Core's own module gives `ts` and `ts.<publisher.domain>` and runs when `[prefix] module` selects nothing, so a stock build's names are byte for byte what they were, with a test.

A module gives the parts and each use writes its own form, because what is allowed differs by use: `ts-ec` for a cookie, `ts-evidence` for a first-party storage key, `ts.example.com` for a store key. The public prefix carries no domain, because a browser already keeps each site's names apart from every other site's. The server key prefix carries the publisher's domain with a single leading `www.` folded, so `www.example.com` and `example.com` share keys and `clients.example.com` and `public.example.com` do not, and the domain comes from the publisher the request resolved to, never from the raw request, because a `Host` header can say anything and a prefix built from one would let any request start a key space of its own.

The registry resolves `[prefix] module` as it does `[geo]` and `[device]`, refuses a name nothing this deployment runs supplies, and asks the module once when it is built, so a module naming a value its call never carries is refused at startup. Every adapter puts the resolved prefixes on the request's services. Every cookie core writes or reads is named through the public prefix, the Edge Cookie state carries the prefix it was read under, a module's response header naming a cookie under that prefix is refused as one under `ts-` was, the page's bundle tag tells the client script the prefix as `data-ts-public-prefix` so the script holds no copy, and the template fingerprint includes the prefix so a page stored under one is never served under another. A prefix's first part starts with a letter or a single `_`, so no cookie name starts with `__`, where browsers give names such as `__Host-` rules of their own.

**Why.** A deployment that is not Trusted Server, or one host serving several publishers, needs its own names in the browser and its own key space in a shared store, and until now both were fixed in core as `ts`. The names a reader's browser holds and the keys a store holds are a deployment's identity, so the module that gives them is an extension point rather than a setting, and the default keeps the stock build unchanged.

## 3. What is not in this change

- A module that gives anything but `ts`. The stock build has no vendor with names of its own, so the probe is the one module.
- The cookie constants are renamed, `COOKIE_TS_EC` to `COOKIE_EC` and so on, because their value is no longer a whole name. A crate outside core that read them writes `services.public_prefix().cookie_name(COOKIE_EC)` instead.

## 4. Acceptance

The seam probe declares a prefix module giving `_probe` and `probe.<publisher.domain>`, and its report route answers the prefixes the request's services carry. The Axum adapter's tests select it with `[prefix] module` and read back both prefixes through the route, so the selection and what a request's services carry are proven by a caller core does not know about.

The pull request's description records how it was checked, with every commit linted and tested on its own, in order, the full local check on its head and CI green on all 19 checks, and 8 deliberate faults made one at a time and each caught by a test.

## Revision record

| Date       | Change                                                          |
| ---------- | --------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand. |
