# Terraform generation and review

Apply this reference when choosing Terraform state/authentication boundaries, generating HCL, or checking generated modules. The planning-only authority in `SKILL.md` governs all upstream guidance and commands below.

## Upstream guidance

Consult the matching HashiCorp skill as reference material, using an approved local installation if available or the linked source. Installation is a separate user decision. Record the revision consulted in the deployment plan; these skills were reviewed at `c2d65dfe492f74d360d35b859b88932222470bd8`.

| Branch                                               | Reference                                                                                                                                                                |
| ---------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Writing or reviewing HCL                             | [terraform-style-guide](https://github.com/hashicorp/agent-skills/blob/c2d65dfe492f74d360d35b859b88932222470bd8/plugins/terraform/skills/terraform-style-guide/SKILL.md) |
| Writing `.tftest.hcl` tests                          | [terraform-test](https://github.com/hashicorp/agent-skills/blob/c2d65dfe492f74d360d35b859b88932222470bd8/plugins/terraform/skills/terraform-test/SKILL.md)               |
| Refactoring existing resource addresses into modules | [refactor-module](https://github.com/hashicorp/agent-skills/blob/c2d65dfe492f74d360d35b859b88932222470bd8/plugins/terraform/skills/refactor-module/SKILL.md)             |

Verify examples against primary documentation and the selected Terraform/provider versions. Upstream examples do not authorize state access, integration tests, apply, or migration. Keep PBS-specific requirements here rather than adopting upstream example credentials, instance sizes, or runtime choices.

## State and backend

Identify the existing backend, state owner, Terraform version, and production/nonproduction boundaries before choosing storage. Reuse an approved backend. Keep bootstrap ownership separate from resources whose teardown it records.

For a newly selected S3 backend, require bucket versioning, encryption, blocked public access, restricted state access, and native locking with `use_lockfile = true` on a compatible Terraform version. Include the required read/write/delete permissions for the `.tflock` object; lock deletion does not require permission to delete the state object. Verify these details against the [S3 backend documentation](https://developer.hashicorp.com/terraform/language/backend/s3).

DynamoDB-based locking is deprecated. For an existing backend using it, propose a migration that accounts for every Terraform client, lock permissions, state recovery, and the cutover owner. Preserve current locking until the separately authorized migration establishes the replacement. File generation must not migrate state, disable locking, or replace the existing backend automatically.

Separate production and nonproduction state and access. Split regional state only when ownership, failure isolation, or infrastructure change lifecycles justify it. One pilot root with explicit regional provider mappings remains a valid approved choice; backend selection does not select ECS or impose East/West regions.

## Versions and module contracts

- Select a reproducible Terraform CLI version in the repository's toolchain and compatible `required_version` constraints. Verify feature support before selecting a version; sample version numbers are not defaults.
- Declare compatible provider constraints and generate `.terraform.lock.hcl` in each root. Commit the locks and review upgrades deliberately. The lock file records provider selections and checksums, not the CLI or remote module versions.
- Pin external registry modules to explicit versions for reproducible roots; pin Git sources with immutable commit references. A module's `version` argument applies only to registry sources. Local modules share their caller's repository revision. See [dependency locking](https://developer.hashicorp.com/terraform/language/files/dependency-lock) and [module sources](https://developer.hashicorp.com/terraform/language/modules/sources).
- Keep provider configurations in roots; reusable modules declare requirements and receive provider mappings. Test every selected regional mapping, including aliased providers.
- Expose settings expected to vary. Give every input a type and description, and every output a description. Validate real restrictions, resource bounds, and conditional requirements. Required unknowns must fail validation or an explicit preflight instead of silently choosing a deployment target.
- Use stable keys for independently named resource collections. Avoid index-driven address churn when membership changes. Keep module boundaries tied to responsibility and shared lifecycle rather than wrapping each AWS resource.
- Apply common ownership/cost tags through each AWS provider configuration where supported, including aliases. Check resource-specific exceptions and override behavior; `default_tags` is not proof every resource is tagged.

When refactoring existing infrastructure, obtain authorization for scoped state inspection and map old/new addresses before proposing `moved` blocks. Require a reviewed migration plan showing intended address moves without unintended resource destruction or replacement. Leave state mutation and migration execution to the authorized operator.

## Credentials and resource ownership

Prefer short-lived authentication through the approved AWS identity system: [Identity Center/SSO](https://docs.aws.amazon.com/cli/latest/userguide/cli-configure-sso.html) for operators and [OIDC federation](https://docs.aws.amazon.com/IAM/latest/UserGuide/id_roles_providers_create_oidc.html) for CI where supported. Scope CI trust to the intended repository, branch or protected environment, and audience. Separate plan/apply permissions where practical; the plan role still needs only the state/lock and read permissions its work requires. Keep cloud credentials unavailable to untrusted pull-request code. Record any platform exception for approval instead of generating permanent access-key secrets by default.

Terraform owns AWS resources, secret metadata, and access policy. An authorized external workflow owns credential values. Keep secret values out of user data, Terraform variables/data sources, plans, and state; `sensitive` masks display but does not exclude ordinary values from state. Output identifiers and endpoints only. Native secret references preserve this boundary for the selected runtime; ephemeral/write-only support is not a reason to route bidder credentials through Terraform. See [sensitive data](https://developer.hashicorp.com/terraform/language/manage-sensitive-data).

Prefer provider-native resources, image builds, and explicit deployment tools over provisioner side effects. Keep application releases outside `local-exec`, `remote-exec`, and shell-command wrappers such as `null_resource`. For ECS, assign task-definition revisions and the service's selected revision to one writer. If a deployer selects revisions outside Terraform, document and test drift handling so a later apply cannot silently revert a release.

## Tests and safe local checks

Generate module tests for input restrictions, conditional resources, capacity bounds, provider mappings, tags/security settings, secret references, and meaningful outputs as applicable to the approved topology. Include negative cases using `expect_failures` for custom validation. Use explicit mock values when assertions depend on computed attributes; plan-time values may otherwise remain unknown.

Before running tests:

1. Enumerate the exact test files and their run blocks, setup modules, providers, aliases, and external data sources. Inspect downloaded modules and executable hooks. Terraform can mix real and mocked providers in one suite.
2. Prefer explicit `command = plan` with mocked external providers. Plan mode alone is not credential-free: real providers and data sources can contact AWS. Mock every external provider used by the selected tests, including aliases and setup dependencies; exclude command-executing data sources and other unapproved side effects.
3. Fully mocked apply-mode tests may check computed results after the same inspection. Mocks work with both plan and apply. An omitted command defaults to apply, so require explicit commands; a real-provider apply test is a cloud integration test outside this workflow.
4. Select individual reviewed files with `terraform test -filter=tests/<file>.tftest.hcl`, repeating `-filter` for additional files. Filters take file paths, not run names or naming substrings. `-test-directory` alone is not isolation because Terraform also discovers tests in the root directory.
5. Run without cloud credentials or metadata-credential access and with network restrictions appropriate to the test environment. Confirm the expected tests actually ran; zero tests is not a pass. Record commands, counts, results, and remaining cloud-validation gaps.

Use the selected CLI's help and [test command documentation](https://developer.hashicorp.com/terraform/cli/commands/test) to verify flags. Consult [test syntax](https://developer.hashicorp.com/terraform/language/tests) and [mocking](https://developer.hashicorp.com/terraform/language/tests/mocking) rather than copying upstream CLI examples blindly.

Run `terraform fmt -check -recursive <infra-path>`. In each root, inspect dependency sources before `terraform init -backend=false`, then run `terraform validate`. Backend-disabled init still downloads providers/modules; follow dependency-download policy. Once dependencies are installed, keep the test run separate from downloads. Use existing configured lint/security scanners when present, or propose their addition rather than silently installing tools.

Validation and mocks prove configuration behavior, not IAM permissions, quotas, real bidder connectivity, or deployment capacity. If tooling or safe isolation is unavailable, mark checks not run. Live integration tests need a separately authorized test account, cost limits, and cleanup verification; automatic teardown is not a guarantee that all resources were removed.

## Saved-plan handoff

Generate the following procedure in the runbook or inactive, approval-gated CI. Do not execute it as a local validation step:

1. An authorized operator verifies the account/role, regions, backend/state target, source revision, toolchain, dependency locks, and input set before producing `terraform plan -out=<plan-file>`.
2. Store the plan as a protected, short-lived artifact with its checksum and provenance. Saved plans and JSON renderings can contain cleartext secrets. Keep them out of Git and public PR logs; restrict review access and retention.
3. Review additions, changes, deletions/replacements, IAM/network exposure, cost implications, and target identity. Tie approval to that artifact and source revision, not just a PR's earlier speculative plan.
4. The separately authorized apply stage verifies the artifact identity and uses `terraform apply <plan-file>` rather than silently generating another plan. Re-plan and re-review after relevant code/input/state changes, an expired approval window, or known drift. A saved plan is not a lock on AWS and does not guarantee success against out-of-band changes.

Authenticated plan/refresh, backend migration, state mutation, and real-resource apply/destroy remain execution tasks. See [saved-plan behavior and sensitivity](https://developer.hashicorp.com/terraform/cli/commands/plan).
