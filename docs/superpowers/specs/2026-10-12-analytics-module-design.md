# Design Spec: Auction telemetry is a module [analytics] selects

**Status:** Proposed, 2026-10-12. Implemented on the fork as jwrosewell/trusted-server#11, ready for review, answering IABTechLab/trusted-server#1274. This pull request (#1084) carries every design document of the stack in one place.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`, `2026-06-22-auction-prebid-metrics-tinybird-grafana-design.md`
**Related PRs:** #1094, jwrosewell/trusted-server#11
**Last updated:** 2026-10-12

> **Why this spec exists.** After the seam (#1094) a vendor's module is selected by name.

## 1. Problem

After the seam (#1094) a vendor's module is selected by name. A section named for the job says which module does it, and the module's settings sit in a table at its name. Auction telemetry does not follow that. Its section is named after its vendor, `[tinybird]`, and a flag inside the vendor's own table switches it on, so a second place to send auction events would need a second section and a second flag.

## 2. Design

**Auction telemetry is a module `[analytics]` selects.** An addition above the integration seam (IABTechLab/trusted-server#1094), on top of the diagnostic headers change (jwrosewell/trusted-server#10).

The Tech Lab issue for this change is IABTechLab/trusted-server#1274.

```toml
# Before
[tinybird]
enabled = true
api_host = "api.example.com"
auction_token_secret = "tinybird_auction_append_token"

# After
[analytics]
module = "tinybird"

[analytics.tinybird]
api_host = "api.example.com"
auction_token_secret = "tinybird_auction_append_token"
```

**Why.** After the seam (#1094) a vendor's module is selected by name. A section named for the job says which module does it, and the module's settings sit in a table at its name. Auction telemetry did not follow that. Its section was named after its vendor, and a flag inside the vendor's own table switched it on.

With this change a second place to send auction events is a second name `[analytics] module` can take, and nothing about the section has to change to add one.

- `Settings::telemetry()` returns the module's settings only when the section selects it. The Fastly sink is built from that, so it has no enabled flag of its own, and settings that select nothing build the sink that sends nothing.
- `module` takes `tinybird` or the same name in full, `analytics.tinybird`, as every other selector takes a short or a full name.
- The secret reference is `analytics.tinybird.auction_token_secret`. The references of a table nothing selects are cleared before secrets are resolved, so what an operator sees is the refusal below and not a failed secret lookup.
- An empty or absent token reference is refused as the settings load and again at deploy by the declared secret field, so the deploy validation's own duplicate check of it is removed, and both refusals are tested.

| **What is refused when the settings load.**                    | Configuration                                                                          | Refused with |
| -------------------------------------------------------------- | -------------------------------------------------------------------------------------- | ------------ |
| `[analytics]` with no `module`                                 | "[analytics] selects no module", and the modules this build has                        |
| `module` naming something else                                 | "[analytics] module names `…`, which this build does not have", and the modules it has |
| A key `[analytics.tinybird]` does not know, `enabled` included | The key's name                                                                         |
| `[tinybird]`                                                   | Where the table went, and what replaces `enabled`                                      |

## 3. What is not in this change

- The Tinybird code itself moving out of core's settings and the Fastly adapter into a crate of its own. The section now has the shape a module crate would be selected by, and that move is a change of its own.
- Access-log telemetry, which stays reserved and refused as before.

## 4. Upgrade

- A configuration that carries `[tinybird]` is refused with directions.
- An earlier version wrote the `[tinybird]` table into every stored configuration, whether telemetry was on or off. So a stored configuration from before this change is refused by a build with it, and the configuration is pushed again with this version's `ts config push`, which writes no such table. The removal of administration from the publisher's domain (jwrosewell/trusted-server#8) already needs the new configuration and the new build to go live together, so this adds no step to that upgrade.
- A build from before this change does not know `[analytics]`, so a configuration that selects telemetry cannot be rolled back to it unchanged.

## 5. Acceptance

The pull request's description records how it was checked, with every commit linted and tested on its own, in order, the full local check on its head and CI green on all 19 checks, and 13 deliberate faults made one at a time and each caught by a test.

## Revision record

| Date       | Change                                                          |
| ---------- | --------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand. |
