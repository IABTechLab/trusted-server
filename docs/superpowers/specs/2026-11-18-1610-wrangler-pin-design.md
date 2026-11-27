# Pin Wrangler for the Cloudflare smoke

## Problem and scope

Issue #1610 reports that `scripts/smoke-cloudflare.sh` exits before building
because `.tool-versions` has no Wrangler entry. The script intentionally requires
an exact version. Cloudflare deadline and browser integration jobs currently
install 4.83.0, while the main integration job installs the latest release.

This change stacks on #1506 (`spec/edgezero-pr275-migration`), which owns the
cutover specification that records this failure. It changes tool selection and
documentation, without changing adapter behavior or Wrangler dependencies.

## Design

Add `wrangler 4.83.0` to `.tool-versions`. All three CI installations read that
entry from `$GITHUB_WORKSPACE/.tool-versions` with awk, require a nonempty value,
and install `wrangler@"$wrangler_version"`. The absolute manifest path also works
after the browser job changes its working directory. The smoke script keeps its
existing exact-version guard.

Keeping hardcoded CI versions would be smaller but leave independent version
sources and the unpinned integration job. Removing the smoke's version guard
would allow an unqualified runtime. Reading the common pin keeps local and CI
runtime selection consistent without adding a helper or dependency.

After a successful local smoke, remove the cutover Runtime list's known-failure
claim and its outstanding pin follow-up. Preserve historical run results and
other qualification requirements.

## Acceptance and preservation

| Scenario                       | Expected result                                | Evidence                                         |
| ------------------------------ | ---------------------------------------------- | ------------------------------------------------ |
| No Wrangler pin on the base    | Smoke exits 1 before a build                   | Execute the existing script with Wrangler 4.83.0 |
| Candidate with Wrangler 4.83.0 | Smoke exits 0                                  | Run the actual local Worker and CLI handoff      |
| Missing configuration binding  | HTTP 500 with the expected diagnostic          | Existing smoke negative case                     |
| Each missing required secret   | HTTP 500 with the expected diagnostic          | Three existing smoke negative cases              |
| Complete configuration         | HTTP 200, origin sentinel, rewritten asset URL | Existing smoke positive case                     |
| CI installations               | All select the manifest pin                    | Inspect and evaluate each installation block     |
| Cutover status                 | No current missing-pin failure or follow-up    | Inspect both current-status locations            |

Use Node 24.20.0, Rust 1.98.1 and Wrangler 4.83.0. Build the CLI and Worker from
this worktree; do not use stale artifacts from another branch. Keep local KV,
configuration, synthetic secrets, logs and listeners disposable. No deployed
Cloudflare access is required. Run repository gates before PR handoff and report
unavailable prerequisites separately from code failures.

## Review log

- Round 1: independent review found that the browser installation changes
  directory before reading the pin. Use the absolute workspace manifest path.
- Round 2: revised specification and plan approved; no blocking findings.
  Corrected the optional inline-code formatting observation.

- Implementation review: all three workflow installations and their
  missing-pin controls reviewed without blocking or optional findings.
- Final documentation review: cutover status matches the successful local
  smoke; obsolete follow-up removed, historical evidence preserved.
