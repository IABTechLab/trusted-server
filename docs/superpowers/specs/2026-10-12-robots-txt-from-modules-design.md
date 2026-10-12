# Design Spec: robots.txt from modules

**Status:** Proposed, 2026-10-12. Implemented on the fork as jwrosewell/trusted-server#9, ready for review, answering IABTechLab/trusted-server#1272. This pull request (#1084) carries every design document of the stack in one place.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`
**Related PRs:** #1094, jwrosewell/trusted-server#9
**Last updated:** 2026-10-12

> **Why this spec exists.** `/robots.txt` reaches the publisher's origin like any other path, so a deployment has no say in it.

## 1. Problem

`/robots.txt` reaches the publisher's origin like any other path, so a deployment has no say in it. A publisher who puts a staging or pre-launch site behind this server has nothing that keeps crawlers out of all of it. Rules about crawlers come from more than one place, the publisher's own text, a sitemap and rules a vendor keeps up to date, and no one of them should own the file. A `404` or an empty file reads to a crawler as permission to crawl everything, so a file built from a source that can fail has to fail closed, which is easier to get right once in core than in every module that wants to add a rule.

## 2. Design

**Why the server should answer this.** 1. **A site that is not ready to be seen needs one switch.** A publisher who puts a staging or pre-launch site behind this server has to keep crawlers out of all of it. `modules = "refuse_all"` does both halves of that from one setting. The file refuses every crawler, which asks a crawler not to fetch, and every response carries `X-Robots-Tag: noindex, nofollow`, which tells a crawler that fetched anyway not to index. The header is written last when a response is finalized and replaces any tag already there, so neither an origin page nor an operator's `[response_headers]` can loosen it. 2. **The file fails in the dangerous direction.** A `404` or an empty file reads to a crawler as permission to crawl everything. A file built from a source that can fail has to fail closed, and that is easier to get right once, in core, than in every module that wants to add a rule. 3. **Rules about crawlers come from more than one place.** A publisher's own text, a sitemap, and rules a vendor keeps up to date all belong in one file. No one of them should own it, so core assembles it and each source contributes.

```toml
[robots-txt]
modules = ["refuse_all"]
always_allow = ["/ads.txt"]
```

| Setting                   | Meaning                                                                                                                                                |
| ------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `modules`                 | What makes the file, in order, as one name or a list. Required, with no default, because a default would decide for the publisher what crawlers may do |
| `always_allow`            | Paths kept open to every crawler refused the whole site                                                                                                |
| `sitemap`                 | Added as a `Sitemap:` line                                                                                                                             |
| `top_text`, `bottom_text` | The publisher's own text, above and below the rules                                                                                                    |
| `[robots-txt.<name>]`     | The settings of the module selected as `<name>`, which that module reads and core does not                                                             |

`refuse_all` and `allow_all` are core's own names and need no module. Any other name is a contributor that a module declared with the new `with_robots_txt_contributor`, and core names none of them.

```rust
#[async_trait::async_trait(?Send)]
pub trait RobotsTxtContributor: Send + Sync + core::fmt::Debug {
    fn refresh(&self) -> Duration;
    fn fingerprint(&self) -> String;
    fn required_permissions(&self) -> PermissionSet { PermissionSet::none() }
    async fn contribute(&self, call: ModuleCall<'_>) -> Result<Contribution, Report<TrustedServerError>>;
}
```

A contributor returns a `Contribution`, which is groups of records, and core writes every byte. That is what lets the following hold whatever a contributor returns.

1. With `refuse_all` selected the file is the refusal and nothing else, whatever else is selected.
2. A path in `always_allow` stays open to every crawler a contribution refuses the whole site to, `*` included. Core adds a group of its own below the contributions and never edits them. A crawler that obeys only the first group it finds then still meets `Disallow: /` first, so the worst it can do is fail to read the one page.
3. The rules appear in the order `modules` names them, between the publisher's `top_text` above and the `Sitemap` line and `bottom_text` below.
4. A contributor cannot produce a malformed file, because it never writes one.

**How an answer is held.** A contributor is asked through the module context (jwrosewell/trusted-server#3), with the request's method, path, host and scheme. It is not given the query or anything about who asked, because its answer is served to every crawler.

Its answer is held in the key-value store and served until the contributor's own `refresh` is due. The key is a SHA-256 hash of the publisher's domain, the contributor's name and the contributor's `fingerprint`, so one publisher's rules are never another's, an answer given for other settings is never served, and nothing of the fingerprint is written to the store.

| Case                                                     | Answer                                                           |
| -------------------------------------------------------- | ---------------------------------------------------------------- |
| A held answer younger than the contributor's `refresh`   | The held answer, and the contributor is not asked                |
| The contributor answers                                  | Its answer, which is then held for 30 days                       |
| The contributor fails and an answer is held              | The held answer, past its age                                    |
| The contributor fails and nothing is held                | `503` with `Retry-After: 600` and `Cache-Control: no-store`      |
| The key-value store cannot be opened                     | Every request asks each contributor                              |
| The store gives an entry no lifetime, as Spin's does not | The answer is written without one, and kept until it is replaced |

Leaving out the rules of a contributor that failed would allow by omission whatever those rules refused, which is why one failure with nothing held fails the whole file.

**The route.** `GET` and `HEAD /robots.txt` are registered on all four adapters, only when the settings carry the section, beside each adapter's other fixed routes. On Fastly and Axum the path is a row of the adapter's named-route table, so a request for it passes what every named route's request passes there. `HEAD` is answered with the headers of the `GET` and no body. Every other method on the path goes to the publisher fallback as before.

- When the settings load: `modules` absent, empty or naming one thing twice, a key that is not a setting, a block for a contributor `modules` does not name, and an `always_allow` entry that is not a path on one line.
- When the registry is built: a name that nothing the deployment runs supplies, with a list of what can be named. The file therefore does not fail on the first crawler to ask for it.

## 3. What is not in this change

- No vendor's contributor. The extension point is proven by the seam probe, as the other module types are.
- A shared layer for holding answers. Each answer is read from and written to the key-value store directly. A cache that loads once for many concurrent requests would save the second and later requests of a burst from asking the contributor again, and belongs in an addition of its own.
- `ads.txt`. A publisher who sells advertising wants `/ads.txt` in `always_allow`, because Google's help for publishers says "The ads.txt file for a domain may be ignored by crawlers if the robots.txt file on a domain disallows" the path it is posted on ([AdSense Help](https://support.google.com/adsense/answer/7679060)). It is not added by default, because a domain with no `ads.txt` would be inviting crawlers to a path it does not have.

## 4. Acceptance

The pull request's description records how it was checked, with every commit linted and tested on its own, in order, the full local check on its head and CI green on all 19 checks, and 37 deliberate faults made one at a time and each caught by a test.

## Revision record

| Date       | Change                                                          |
| ---------- | --------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand. |
