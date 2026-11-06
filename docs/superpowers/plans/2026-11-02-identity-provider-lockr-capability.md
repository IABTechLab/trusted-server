# Lockr Cookie Capability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Status:** Implemented; checkboxes are not updated.

**Goal:** Read vendor IDs that Lockr AIM writes into first-party cookies on the publisher domain, as an explicit, reusable capability of the provider kinds Lockr delivers (ID5, LiveRamp, UID2), instead of ad-hoc cookie names per provider.

**Architecture:** Everything lives in `crates/trusted-server-identity-providers/src/common.rs`; core only passes the configured provider ID to builds (`ProviderCommon::id`) for logging.

- `LockrSettings` (`enabled`, default `false`; `cookie_names`, default the vendor's known Lockr key), parsed from `[ec.identity_providers.settings.lockr]` as a field of each capable kind's settings.
- `ReadsLockr`: a trait a `ProviderKind` implements to declare the capability, with `LOCKR_COOKIE_NAMES` (`id5id` for ID5, `_lr_env` for LiveRamp, `__uid2_advertising_token` for UID2). `reads_lockr(kind)` in `lib.rs` lists the same kinds.
- `CookieReader`: holds the own cookie names and, when Lockr is enabled, the Lockr names not already among them. `read` hands each value to the provider's existing parser: own cookies first, then Lockr cookies. Lookup kinds look up only when neither yields an ID.
- A read from a Lockr cookie logs at debug with the provider ID, the cookie name and origin `lockr`.

**Gating:** kinds without the capability (ConnectID, LiveIntent) have `deny_unknown_fields` settings without a `lockr` field, so a `lockr` table fails the provider build at startup. Lockr enabled with an empty `cookie_names` is rejected.

**Own-cookie settings:** every cookie-reading kind takes a `cookie_names` list (UID2's `token_cookie_name` and ConnectID's `cookie_name` are renamed). Lockr names move out of them into `[settings.lockr]`.

## Not ported from the reference provider

- The reference captures proxied Lockr API responses: a `revoke-consent` call opts the visitor out of Lockr's sources, and token replies are mapped to sources with Lockr provenance (`inserter`, `mm`) on ID5. The framework has no response-capture hook or opt-out state, and Lockr's default ID5 key is the same `id5id` cookie Prebid writes, so a Lockr-read ID cannot be told apart for provenance. Neither is ported.

## Tasks

- [ ] Add `ProviderCommon::id` in core and set it in `build_kind`.
- [ ] Rename UID2 `token_cookie_name` and ConnectID `cookie_name` to `cookie_names`.
- [ ] Add `LockrSettings`, `ReadsLockr` and `CookieReader` to `common.rs`; wire ID5, LiveRamp and UID2; use `CookieReader::own` for ConnectID.
- [ ] Tests: capability gating per kind, empty Lockr list rejected, Lockr disabled ignores the cookie, a custom Lockr name is read, own cookie before Lockr, no cookie leaves ID5 to the lookup.
- [ ] Update `trusted-server.example.toml` and the provider plans.
