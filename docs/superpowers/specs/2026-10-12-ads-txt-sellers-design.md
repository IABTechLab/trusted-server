# Design Spec: The sellers a deployment adds to the publisher's ads.txt

**Status:** Proposed, 2026-10-12. Implemented on the fork as jwrosewell/trusted-server#15, ready for review, answering IABTechLab/trusted-server#1278. This pull request (#1084) carries every design document of the stack in one place.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`
**Related PRs:** #1094, jwrosewell/trusted-server#15
**Last updated:** 2026-10-12

> **Why this spec exists.** A buyer's crawler that does not find a seller in a publisher's `ads.txt` refuses that seller's inventory.

## 1. Problem

A buyer's crawler that does not find a seller in a publisher's `ads.txt` refuses that seller's inventory. A publisher who sells through this server has sellers to declare that their own file does not know about, and the file passes through the server untouched. Every buyer's crawler reads the file, and a typo in it costs the publisher revenue silently.

## 2. Design

`crates/ads-txt/sellers`, a module that writes the lines the settings give at the end of the publisher's `ads.txt`.

```toml
[ads-txt]
modules = ["sellers"]

[ads-txt.sellers]
lines = ["ssp.example, 12345, DIRECT, f08c47fec0942fa0", "CONTACT=ads@publisher.example"]

[[fetch]]
media_type = "text/plain"
path = "/ads.txt"
middleware = ["ads-txt.sellers"]
```

The module is a middleware for `text/plain`, the first to use the entries for a media type other than HTML (jwrosewell/trusted-server#14). Named in a `[[fetch]]` entry on the file's path, it passes the origin's file through and writes the lines after its last one, on a line of their own.

**Why.** A publisher who sells through this server has sellers to declare that their own `ads.txt` does not know about, and a buyer's crawler that does not find a seller in the file refuses that seller's inventory. The publisher's file stays theirs: it is read from the origin as it is, and the deployment adds to it. A publisher whose origin has no file gets the origin's answer, because the module adds to the publisher's file and never invents one.

## 3. What is not in this change

- A file for a publisher whose origin has none, which #17 adds: where the origin answers `404` for a path a `[[fetch]]` entry names middleware for, the middleware runs on an empty document and its output is the file, so this module makes `ads.txt` from its lines alone there.
- Lines from a vendor's own system. `[ads-txt] modules` is a list so that a module supplying lines from elsewhere can be named beside `sellers`, and none exists yet.

## 4. Acceptance

Every buyer's crawler reads the file and a typo in it costs the publisher revenue silently, so each line is checked as the settings load, and by `ts config validate`, against the file's grammar.

| Line                                                                                                                                                                   | Accepted                                          |
| ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------- |
| `domain, account id, DIRECT or RESELLER` with an optional fourth field, the certification authority id                                                                 | Yes, the relationship in either case              |
| `CONTACT=`, `SUBDOMAIN=`, `INVENTORYPARTNERDOMAIN=`, `OWNERDOMAIN=` or `MANAGERDOMAIN=` with a value                                                                   | Yes                                               |
| A comment, an empty line, a record with two or five fields, no domain, no account id, another relationship, a trailing comma, a variable with no value or another name | Refused, naming the line and what is wrong        |
| An empty list                                                                                                                                                          | Refused                                           |
| A document that selects the module and names it in no `[[fetch]]` entry for `text/plain`                                                                               | Refused, because the lines would never be written |

The pull request's description records how it was checked, with every commit linted and tested on its own, in order, the full local check on its head and CI green on all 19 checks, and 11 deliberate faults made one at a time and each caught by a test.

## Revision record

| Date       | Change                                                          |
| ---------- | --------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand. |
