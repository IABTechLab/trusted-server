# Hashed-Email Capability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let `liveramp` and `id5` providers send a signed-in visitor's hashed email, which Trusted Server reads from a publisher status endpoint at most once per collection phase that needs it (#1567).

**Architecture:** Core owns the request-scoped intake: `[ec.hashed_email]` settings, the `HashedEmail` and `HashedEmailPath` types, `IdentityProvider::hashed_email_path`, and one status call in `ProviderCoordinator::dispatch` before lookups are built. The providers crate owns the capability, as it does Lockr: `HashedEmailSettings` under `[ec.identity_providers.settings.hashed_email]`, the `UsesHashedEmail` marker trait, and how each kind puts the value in its request.

**Spec:** `docs/superpowers/specs/2026-10-30-s2s-eid-collection-design.md`, section 8.7.

## Core

- `settings.rs`: `Ec::hashed_email: Option<HashedEmailSource>` with `endpoint` (`https`) and `timeout_ms` (1–1000, default 100), skipped from serialization when unset.
- `ec/provider.rs`: `HashedEmail` holds the opaque value (non-empty, at most 512 bytes) with a redacting `Debug`. `HashedEmailPath` parses the jq path subset (`.key`, `."quoted key"`, `[index]`) and reads a string from a `serde_json::Value`. `IdentityProvider::hashed_email_path` defaults to `None`.
- `ec/coordinator`: `ProviderCoordinator::with_hashed_email_source`. It rejects a provider that has a path when no source is configured. `dispatch` splits into a preparation pass, an optional status call and a build pass:
  - The preparation pass covers eligibility, consent, region, cookies, opt-outs and due sources. `HashedEmail` sources that are due but not refresh-due are held back.
  - The status call is made only when a prepared provider with a path has a due source that is not refresh-due. It is a `GET` carrying the whole `Cookie` header, sent through the transport with `timeout_ms`. The lookups' cutoff is the budget less the call's time, and `with_hashed_email_source` rejects a `timeout_ms` not below the budget when a provider has a path.
  - The build pass drops held-back sources when the provider got no value, then generates or builds requests as before.
- Fastly `app.rs` and the providers crate's `build_phase_coordinator` pass `settings.ec.hashed_email` to both coordinators. Fastly's publisher path copies the lookup inputs out of the request (`PageIdentityDispatch`) and joins `dispatch` with `handle_publisher_request`, so the status call runs beside the origin fetch.

## Providers

- `common.rs`: `HashedEmailSettings { enabled, path }`, `UsesHashedEmail`, and a helper that returns the parsed path, or `None` when disabled. It rejects an enabled table with no path or an invalid one.
- `lib.rs`: `uses_hashed_email(kind)`. Deploy validation fails when a provider enables the capability while `[ec.hashed_email]` is unset.
- `liveramp.rs` and `id5.rs`: see their plans.

## Tasks

- [x] Spec section 8.7 and the config, logs and acceptance updates.
- [x] Core types, the trait method and settings, with tests: path parsing, extraction, value limits and settings validation.
- [x] Coordinator status call, with tests:
  - no source configured
  - a failing or non-JSON status reply
  - a value present
  - a refresh made without a status call
  - one status call shared by two providers
  - an `Anonymous` source looked up without a value
- [x] Capability settings in the providers crate, with tests: an unsupported kind rejects the table, and an enabled table needs a path.
- [x] LiveRamp envelope lookup and refresh; ID5 `pd`.
- [x] Example config and the configuration guide.
- [x] Every CI gate.
