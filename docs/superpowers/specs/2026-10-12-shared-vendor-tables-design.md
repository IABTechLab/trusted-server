# Design Spec: A vendor's modules share one table

**Status:** Proposed, 2026-10-12. Implemented on the fork as jwrosewell/trusted-server#21, ready for review, answering IABTechLab/trusted-server#1284. This pull request (#1084) carries every design document of the stack in one place.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`
**Related PRs:** #1094, jwrosewell/trusted-server#21
**Last updated:** 2026-10-12

> **Why this spec exists.** A vendor with several modules, say a device module, a geo module and an identifier module, calls one service with one key.

## 1. Problem

A vendor with several modules, say a device module, a geo module and an identifier module, calls one service with one key. Each module is selected by its own section and reads its own table, so the endpoint and the key are written once in each table, and a module cannot be built on what another module's table says. Nothing in core lets a vendor say that its modules run together or not at all.

## 2. Design

```toml
[shared.example]
endpoint = "https://api.example.com/v1"
license_key = "example_license_key"
```

One `[shared.<vendor>]` table per vendor, named by the vendor's folder under `crates/shared`, holding what every module of that vendor reads. Core declares the tables and reads none of them. A vendor's crates read their own as typed settings with `Settings::shared_config::<T>(vendor)`, which refuses a table that cannot be read as `T` or fails its validation, naming the table.

A builder marked with `IntegrationBuilder::with_shared_settings(vendor)` is run by the table rather than by a selection. A document without the table runs none of that vendor's modules, a selection naming one of them is refused with the absent table named, and a builder the table runs is selected by no section. A table that no builder reads is refused when the registry is built, the one place every builder is known, and a table whose name is not a folder name is refused when the settings load.

A secret in the table is declared with the builder's secret settings at its path inside the table, as a module's own secrets are declared in its own table, resolved from the secret store when the table is present.

**Why.** A vendor with several modules, a device module, a geo module and an identifier module, say, has one service they all call and one key for it. Without this the key is written once in each module's table, and a module cannot be built on what another module's table says. The table is also what runs the vendor's builder, so the deployment runs the vendor's modules together or not at all.

## 3. What is not in this change

- A vendor's shared crate. The stock build has no vendor with several modules sharing a service, so the probe is the one reader.
- The `/_ts/config` view masks a secret in the table through the builder's secret settings, as it masks a module's own, and nothing further is added for it here.

## 4. Acceptance

The seam probe gains a second builder, `shared_builder`, which `[shared.seam-probe]` runs and whose route answers the table's marker. The Axum adapter's test serves the route with the table, finds it absent without, and is refused at startup for the table with no builder reading it.

The pull request's description records how it was checked, with every commit linted and tested on its own, in order, the full local check on its head and CI green on all 19 checks, and 5 deliberate faults made one at a time and each caught by a test.

## Revision record

| Date       | Change                                                          |
| ---------- | --------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand. |
