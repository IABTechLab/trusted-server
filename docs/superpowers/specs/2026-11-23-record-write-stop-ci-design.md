# Record-write shutdown verification

Issue #1579 (A8) requires CI to prove that an ECS-style stop completes an admitted
request and its pending identity record write, and missing secrets cause nonzero
startup failure before readiness. This change stacks on #1646 and changes tests
only.

## Existing coverage and gaps

The Native RC container workflow builds the production release executable and
runs it with prepared local configuration and synthetic secrets. Its record job
uses the real DynamoDB record provider against disposable DynamoDB Local. The
existing test pauses a request-time `PutItem`, sends SIGTERM, observes draining,
releases the write and checks a successful response and normal process exit.
It verifies the row before acknowledging the write, but does not read it after
exit or assert elapsed signal-to-exit time against the configured grace.

The existing startup test repeatedly probes the published health port, requires
nonzero, non-OOM exit and an initialization diagnostic, and rejects `phase=ready`.
For the missing-secret case, a disposable seccomp profile makes both `bind` and
`listen` fail with EPERM. The process must still report `Trusted Server
initialization failed`: an early bind failure must not substitute for secret
validation. Keep the port probes and ready lifecycle assertion as additional
checks. A disposable production-runner mutation that adopts the listener before
polling `prepare` must fail this test, without being pushed.

## Design

Keep the production Dockerfile, adapters, dependency pins and provider behavior
unchanged. Keep the existing path-filtered PR workflow: runtime, dependency,
fixture and workflow changes trigger the jobs; unrelated PRs do not.

For request-time shutdown, measure with a monotonic timer from immediately before sending SIGTERM. Capture
elapsed time immediately after exit observation, before storage reads, and retain
a separate longer cleanup watchdog.
Require process exit within the configured three-second grace plus a documented
500 ms allowance for Docker command and observation latency. Preserve the
before-response acknowledgement proof, then directly read the matching record
from DynamoDB after process exit and compare its complete value and generation
with the acknowledged write.

Add a separate after-response scenario on the same production binary. Configure
a supported SharedID provider with `collect = "after_response"`, generation off,
a dedicated cookie name, a fictional registered source and a five-second task
budget. Mint a record without that cookie and verify the source is absent. On a
returning navigation, supply a distinctive UID. Pause only the conditional
`PutItem` containing that UID; consume the entire response while the write is
still paused. Send SIGTERM, observe draining, and hold the selected write for
another 250 ms. Then assert the process is still running and that exact write
is still pending before release. Retain the exact acknowledged native
payload and response-complete, signal, drain, release and exit events. Require
successful normal exit within the same stop bound and
read and compare its full native row, UID and generation from DynamoDB afterwards.

The current shared application already schedules follow-up work through
EdgeZero's drained task handle. No merge of #1636 is planned. If runtime evidence
shows that dependency is necessary, stop for approval before any merge.

## Failure evidence and preservation

Use disposable local controls, never pushed: terminate immediately on the
pending-write SIGTERM; delay exit beyond the stop bound; corrupt a post-exit row;
disable after-response scheduling; bypass missing-secret rejection; and open a
listener before polling startup preparation. Under the missing-secret seccomp
profile that must fail at the required initialization diagnostic, rather than
pass because the listener was briefly open or emitted no readiness event.
Each new protection must reject its corresponding supported violation at the
intended assertion; unrelated setup failures do not qualify. Restore all
mutations before clean verification. Preserve
existing consent, withdrawal, conditional-write race, acknowledgement-loss,
restart, health and route checks. Cleanup must remove only owned resources.

Targeted local image scripts and runtime verification establish the local
contract; hosted CI must finish on the published draft revision. No AWS account,
browser, full local CI gate list or changes to operator files are required.

Grace enforcement for a write deliberately held past the configured grace is
not tested here. That timeout/cancellation case remains a possible follow-up.
