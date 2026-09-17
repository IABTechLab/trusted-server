# File generation and validation

Generate files only after the design and target paths are approved. Use the repository's existing layout; the paths below are suggestions for a new deployment, not a template to copy wholesale. The authority boundary in `SKILL.md` applies to every check and generated tool.

## Artifact contract

| Artifact                   | Suggested location                           | Contents and owner                                                                                                                                                    |
| -------------------------- | -------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Deployment plan            | `docs/pbs-deployment-plan.md`                | Requirements record, approved architecture, sources, costs, blockers, file/check inventory; user approves                                                             |
| State bootstrap, if needed | `infra/bootstrap/`                           | Independently managed backend, access and locking setup; infrastructure owner                                                                                         |
| Deployment root            | `infra/<environment>/`                       | Providers, variables/validation, explicit regional modules, outputs, selected DNS configuration, nonsecret examples, provider lock file; Terraform owns AWS resources |
| Reusable infrastructure    | `infra/modules/`                             | Modules justified by repeated topology, not one module per AWS service                                                                                                |
| Runtime inputs             | `runtime/`                                   | Resolved PBS configuration, regional inputs, optional stored requests, image/release manifest; Git owns nonsecret content                                             |
| Standalone-host runtime    | `runtime/compose.yaml`, `runtime/Caddyfile`  | Compose/Caddy and boot service only for the approved host profile                                                                                                     |
| Managed-container runtime  | Existing ECS release/task-definition layout  | Task resource limits, health, logs, secrets references, service rollout settings; explicit Terraform/deployer ownership                                               |
| Operator interface         | Existing `ts prebid server` CLI and approved CI layout | CLI descriptor and documented invocations; any missing release stages need separate implementation approval                                                           |
| Runbook                    | `docs/pbs-runbook.md`                        | Preconditions, operator commands, rollout/recovery/rotation/teardown procedures and deferred tests                                                                    |

Each generated artifact must have a consumer. Add ignore rules for local credentials, runtime secret files, `.terraform`, state files, saved plans, and generated sensitive output. Track nonsecret examples. Use `example.com` hostnames and visibly fictional identifiers in examples.

For Terraform artifacts, follow [Terraform generation and review](terraform.md), including module tests under `tests/` and the saved-plan procedure in the runbook.

For the deployment descriptor, resolved PBS YAML, secret bindings, runtime delivery, and command behavior, follow [configuration and secrets](configuration-and-secrets.md). Generate inputs for the existing CLI rather than replacement wrappers. Document unimplemented release stages as deferred unless their implementation is separately approved.

## Generated deployment tools

Use the target-selection and approval contract in [operator commands](configuration-and-secrets.md#operator-command-contract), including descriptor-based interactive use and explicit automation inputs. New CI deployment jobs must remain inactive and approval-gated. File generation must not trigger existing auto-apply or deployment jobs; inspect those triggers before editing their watched paths.

The current `ts prebid server` CLI does not implement deployment or rollback. For a separately approved deployment implementation, implement or explicitly defer each release stage:

1. Serialize competing deployments to the same target.
2. Retrieve and verify an immutable release, image availability, and required secrets before replacing working capacity.
3. Record previous release and secret version identifiers without values.
4. Deploy with the approved outage/draining policy and bounded health deadlines.
5. Check HTTPS, application health, and an agreed auction fixture.
6. Follow [rotation and recovery](configuration-and-secrets.md#rotation-and-recovery) before restoring the previous release or reporting a recovery blocker.
7. Record target, release, timestamps, and outcome.

Define retry limits, interrupted-run handling, and behavior when the same release is requested twice. On standalone hosts, provide boot recovery, deployment locking, bounded logs, certificate persistence, and an explicit response to hung-but-running containers. A Compose unhealthy status alone is not a restart policy. On managed runtimes, express equivalent rollout/rollback controls through the platform rather than adding host scripts.

## Runbook and deferred evidence

Reference the approved decision record. Include operator prerequisites and explicit account/region targeting, initial deployment, configuration updates, secret rotation, failed-release recovery, host/task replacement, alert response, and teardown with retention rules. Clearly mark these as procedures not yet executed.

For a pilot, include the caller kill switch, its refresh deadline, staged allocation gates, and verification that traffic drained before teardown. DNS changes and host shutdown are not substitutes for caller rollback. For ongoing production, use the approved traffic and recovery policy rather than inventing an old-provider fallback.

List deferred checks with expected results and an owner: authenticated Terraform plan review, TLS/DNS, IAM and exposed ports, regional dependency availability, missing secrets, invalid configuration, reboot/replacement, interrupted deployment, credential rotation, representative load, failover, alert delivery, and applicable caller/auction checks. Every approved availability and scaling claim needs a matching test.

## Safe local checks

Run applicable checks on the generated paths, recording exact commands and results. Inspect project scripts before executing them. Use isolated dummy credentials, fake bidder endpoints, and fixtures; local startup must not contact production bidders or fetch real AWS secrets.

| Area              | Check                                                                                                                                                                              |
| ----------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Terraform         | Follow [Terraform local checks](terraform.md#tests-and-safe-local-checks), including inspected, explicitly selected mocked tests                                                   |
| Runtime structure | Parse YAML/JSON, verify manifest paths/checksums, and validate regional override structure                                                                                         |
| Compose branch    | `docker compose --env-file <dummy-env> -f <compose-path> config --quiet` and dummy-value round-trip checks                                                                         |
| Operator tool     | Language syntax/lint and [configuration/secrets contract tests](configuration-and-secrets.md#rotation-and-recovery), plus failed-release and interrupted/repeated invocation tests |
| PBS behavior      | Approved isolated container startup and smoke fixture against controlled bidder responses, if a suitable local runtime is available                                                |
| Final files       | Diff review for scope, secret exposure, unresolved placeholders, inactive deployment triggers, and one writer per mutable resource                                                 |

If a tool, image, network permission, or fixture is unavailable, mark that check not run with the next action. Never substitute a checklist for executed evidence or call a no-bid response proof of bidder success.

## Sources to verify during generation

- [SSM Run Command](https://docs.aws.amazon.com/systems-manager/latest/userguide/run-command.html)
