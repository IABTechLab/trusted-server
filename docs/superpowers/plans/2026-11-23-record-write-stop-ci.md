# Record-write shutdown verification implementation plan

**Goal:** Close #1579 with repeatable production-binary CI proof for request-time
and after-response record-write shutdown, plus missing-secret startup rejection.

**Architecture:** Extend the existing image scripts and their retained JSON
proof. Use the current drained scheduler and DynamoDB Local; change no production
Rust code, Dockerfile or dependency. Base the change on #1646.

**Tools:** Pinned Node 24.20.0, Rust 1.98.1, production Docker image, Docker,
DynamoDB Local and the existing prepared-store fixtures.

## Tasks

- [x] Confirm the unchanged current-head startup and record image scripts pass.
      Demonstrate abrupt pending-write termination and secret-check bypass fail.
- [x] Review the spec and this plan independently, resolving all findings before
      implementation. Preserve port probes and the ready lifecycle assertion; add the approved
      missing-secret seccomp ordering check.
- [x] In `scripts/test-native-ec-image.mjs`, centralize the three-second grace and
      500 ms Docker observation allowance. Bound signal, drain observation,
      response completion and process exit from one monotonic signal-time deadline;
      pass remaining time into Docker commands and response waits, assert total
      elapsed time immediately at exit observation and retain it in JSON evidence.
      Preserve a separate longer cleanup watchdog.
- [x] Retain cloned exact acknowledged native rows, keyed to each scenario and
      complete native key. After shutdown, read the matching
      key directly from DynamoDB and compare the full row and generation.
      Demonstrate that delayed exit and post-exit row corruption fail.
- [x] Add optional fixture configuration for a cookie-only after-response
      SharedID provider: generation off, dedicated cookie, registered fictional
      source and five-second task budget. In the shutdown scenario, mint a record
      without the provider cookie and assert its source is absent before the
      returning navigation supplies a distinctive UID for the follow-up write.
- [x] Add a separate returning-navigation shutdown case. Pause only that UID's
      conditional write, await the entire response while still paused, verify
      the seed has no UID, send SIGTERM, observe draining while the selected write is still pending and the process is
      alive after another 250 ms hold, record full response-body completion plus
      signal/drain/hold/release/exit ordering
      and the held key/condition/UID/expected generation, release, require
      normal bounded exit and verify UID/generation in DynamoDB after exit.
      Demonstrate that disabling scheduling/draining makes the case fail. If
      #1636 proves necessary, stop before merging it.
- [x] In `scripts/test-native-container.mjs`, preserve repeated HTTP port probes
      and nonzero/non-OOM exit checks; reject the ready lifecycle
      event. Deny bind/listen with EPERM for the missing-secret case and require
      the initialization error. Demonstrate a disposable listener-before-prepare
      mutation fails that diagnostic, without pushing the mutation.
- [x] Keep `.github/workflows/native-container.yml` path filtering and existing
      production-image steps. Update descriptive step text only if needed.
- [x] Run both targeted scripts on the unchanged production image with the new
      harnesses. Run meaningful disposable controls, restore exact sources and
      fixtures, and re-run affected positive cases. Check changed JS syntax and
      format the changed Markdown documents.

Implementation, independent review and initial publication are complete in #1654,
stacked on `feat/1574-axum-shared-identity` with `Closes #1579`. Follow-up commits
run targeted local checks and wait for their own hosted results before handoff.
Commits are signed and GitHub Verified; cleanup removes only owned resources.

## Targeted commands

Build the unchanged production image using the repository Dockerfile:

```sh
docker build --build-arg NATIVE_FEATURES=aws-dynamodb-ec,geo-cloudfront \
  --build-arg TRUSTED_SERVER__GIT_VERSION="$(git rev-parse HEAD)" \
  -t ts-record-stop:test .
node scripts/test-native-container.mjs ts-record-stop:test
node scripts/test-native-ec-image.mjs --image ts-record-stop:test \
  --dynamodb http://127.0.0.1:8000 --output /tmp/record-stop-proof.json
node --check scripts/test-native-container.mjs
node --check scripts/test-native-ec-image.mjs
```

Start the pinned DynamoDB Local image exactly as the workflow does, and stop its
owned container afterwards. Linux image mode uses host networking. On Docker
Desktop, a disposable Linux harness may share its network namespace with the
app and store instead; report that topology and local architecture separately
from hosted Ubuntu evidence. Tests must never contact AWS or read operator
configuration. Negative controls must fail at the intended assertion, rather
than during setup or unrelated checks.

Grace enforcement with a write held past the configured grace is not tested;
retain it as a possible follow-up in the PR description.
