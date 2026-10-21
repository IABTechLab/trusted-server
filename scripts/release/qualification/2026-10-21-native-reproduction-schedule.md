# Ordinary CI native reproduction scheduling and build cache

Status: candidate CI optimization, not release qualification. Commit
`3e3e30996` changes when native reproduction is measured; `b4c8e0c0f`
increases the ordinary-CI confined Cargo build wall from 25 to 45 minutes.
Neither change establishes Intel equality or authenticates tools or images.

## Selection and triggers

The Linux and Apple CLI fixtures require `TS_RELEASE_NATIVE_REPRODUCTION`
to be `measure` or `defer`. The workflow selects `measure` for `schedule`,
`workflow_dispatch`, or a pull request carrying `release-native-reproduction`;
other push and pull-request runs select `defer`. Deferred runs record
`status: deferred-to-scheduled-run` and `gate: measurement-only`. The fixtures
reject missing/unknown modes and reject `defer` on scheduled/dispatched runs.
Canonical native build, inspection, startup and evidence checks remain required.
Fastly canonical/reproduction equality and final-byte runtime checks are unchanged.

The cron is `23 4 * * *` (04:23 UTC daily). GitHub runs scheduled workflows
from the default branch; dispatch also requires the workflow file to exist on
that branch, although a dispatch can select another ref. These triggers do not
by themselves validate this unmerged branch. The PR label must be present
before the next push or another configured PR event: this workflow does not
subscribe to the `labeled` event. See GitHub's
[schedule](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#schedule)
and [dispatch](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#workflow_dispatch)
contracts. This report does not dispatch a run or change labels.

## Cache scope and limits

The pinned `actions/cache` v5.1.0 restore/save steps cache `target`, keyed by
runner OS, host target, pinned Rust version, lock/tool/manifest hashes and test
profile. The Linux Fastly producer job and Apple candidate jobs save only on
`refs/heads/main` and an exact-key miss, before ignored execution fixtures;
Linux CLI and JS handoff jobs restore without saving. Incremental directories
are removed before saving. This cache does not include `~/.cargo/registry`
and does not replace fresh confined application build roots.

Measurements retain `ci_build_provenance`: producer/probe and test-binary
SHA-256 values plus `build_cache` (matched key, `miss`, or `unreported`). These
are observed identities, not independently authenticated provenance. Main-only
writes in this workflow and GitHub's branch-scoped cache access reduce sharing;
they do not qualify cached code or make a digest proof of trusted compilation.
See GitHub's [cache access restrictions](https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching#restrictions-for-accessing-a-cache).
All measurements remain `release_qualification: false`.

## Observed baseline and pending validation

[Run 39540057202](https://github.com/IABTechLab/trusted-server/actions/runs/39540057202)
concluded success for head `a9994b4c2b50265de4bbe5b8bb42cd8cab4e43ae`, building
PR merge source `d0cd53343fcbe1fb6f06eafc009e306e24b1245d`. It predates both
optimizations and supplies the timing baseline, not evidence that they work.

| Candidate       | Job                                                                                                    | Observed result                                                                                                                       |
| --------------- | ------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------- |
| Fastly          | [119025502524](https://github.com/IABTechLab/trusted-server/actions/runs/39540057202/job/119025502524) | Candidate entry rebuild outcome `matched`                                                                                             |
| Linux CLI       | [119025502758](https://github.com/IABTechLab/trusted-server/actions/runs/39540057202/job/119025502758) | Native reproduction `identical`                                                                                                       |
| Apple ARM CLI   | [119029080050](https://github.com/IABTechLab/trusted-server/actions/runs/39540057202/job/119029080050) | Native reproduction `identical`; complete fixture 3,144.22 seconds                                                                    |
| Apple Intel CLI | [119029080038](https://github.com/IABTechLab/trusted-server/actions/runs/39540057202/job/119029080038) | Canonical Cargo execution 1,193 seconds; optional reproduction `build-failed` at its wall deadline; complete fixture 4,093.14 seconds |
| JS handoff      | [119025605842](https://github.com/IABTechLab/trusted-server/actions/runs/39540057202/job/119025605842) | Fresh offline JS fixture passed in 474.23 seconds; this is fixture time, not total job time                                           |

The Intel reproduction timeout previously occurred in run 39273777887 and
recurred here, despite `identical` measurements in intervening historical runs
39417433680 and 39420011152 (see the [Apple report](2026-10-20-apple-cli-candidates.md)).
`ToolSelection::build_wall` now supplies 45 minutes for ordinary
CI instead of the fixed 25-minute wall. This is a per-confined-build budget,
not the complete fixture or job budget, and production selection remains closed.

At this report's evidence check on 2026-10-21, the first run for
`b4c8e0c0fded27f02930edc6de990f17a63f5820`,
[39551207134](https://github.com/IABTechLab/trusted-server/actions/runs/39551207134),
was still in progress. Deferred reproduction, cache behavior, recorded producer
identities and unchanged mandatory Fastly equality require that run's completed
logs. A PR cache miss is expected when no eligible entry exists, not guaranteed;
main-only save steps must remain skipped on the PR. Intel equality under the
45-minute wall requires a later measured run; a deferred pass cannot establish it.
Every later equality claim must identify the measured run, job and built source.
