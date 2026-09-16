---
name: planning-prebid-aws
disable-model-invocation: true
description: Plan Prebid Server Go deployments on AWS through an interactive requirements interview. Use when choosing deployment architecture or AWS services, or generating Terraform, runtime configuration, deployment scripts, and runbooks for a new or revised PBS deployment. Stops before cloud changes and live traffic.
---

# Planning Prebid Server Go on AWS

Produce an approved deployment design and locally checked implementation files. Prebid Server Go is fixed; ask for its release and image source, not its implementation language.

## Authority

This workflow permits planning and, after design approval, file generation and safe local checks. Cloud provisioning, credential writes, application deployment, production traffic changes, and teardown require a separate execution workflow with explicit authorization. Generated scripts and CI jobs must remain inactive. Ask before authenticated AWS inspection or state access, naming the account, role, regions, and read scope. Keep real credentials out of the conversation and generated files.

## 1. Establish the baseline

Inspect repository instructions, Git status, existing infrastructure, runtime files, and supplied documents. Preserve unrelated changes. Identify the target repository and deployment directory before proposing edits.

For read-only discovery from any existing `trusted-server.toml`, and for designing the operator commands and PBS config/secret delivery, read [configuration and secrets](references/configuration-and-secrets.md). Select the authoritative config source before inferring requirements; examples and disabled integrations are not active deployment inputs.

Record requirements in one decision record, initially in the conversation, then in the approved deployment plan:

| Requirement          | Value                   | Status                             | Evidence or decision owner | Blocks                                    |
| -------------------- | ----------------------- | ---------------------------------- | -------------------------- | ----------------------------------------- |
| One row per decision | Answer or open question | Confirmed, proposed, or unresolved | Source or person           | Design, generation, live rollout, or none |

Treat supplied examples as evidence of intent, not accepted requirements for this deployment. Reconcile conflicting inputs with the user. Inspect existing answers before asking again.

Done when the current deployment, requested outcome, reusable resources, selected Trusted Server config source or its absence, and unanswered decisions are identified.

## 2. Interview by decision

Ask two to four related questions per turn. Start with questions that change the architecture; explain the consequence of unfamiliar choices. Offer a recommendation the user can accept rather than requiring AWS expertise.

Cover these topics, skipping already confirmed answers:

- Purpose: disposable test, live pilot, or ongoing production; retained provider and migration scope.
- Availability: acceptable interruption, host/AZ/region failure tolerance, maintenance windows, recovery time, and loss tolerance for required data.
- Workload: absolute peak eligible auction QPS, allocation, regional mix, bidder fan-out, payload sizes, burst duration, and caller latency budget.
- Operations: fixed headroom versus automatic scaling, budget ceiling, owner and backup, existing CI and AWS platform, regions, DNS, network restrictions, and bidder IP allowlists.
- Security: permitted callers/publishers, public or private access, data residency, retention, and the privacy policy owner.
- Auction behavior: caller, bidder set, formats, consent and identity, account settings, stored requests, and cache dependencies. Read [Prebid Go requirements](references/prebid-go.md) before resolving these inputs.

Translate "production ready" into measurable availability, security, capacity, and recovery requirements. Scaling and availability are separate decisions. Offer a measurement plan for unknown traffic rather than inventing capacity.

Done when every topic is confirmed, explicitly inapplicable, or recorded as an unresolved blocker. Continue a provisional design around unknowns, but pause affected file generation until architecture-changing decisions are approved.

## 3. Recommend and obtain approval

Read [architecture decisions](references/architecture.md). For Terraform state/authentication decisions and HCL generation, read [Terraform guidance](references/terraform.md). For a two-region standalone-host pilot, consult [the worked example](examples/two-region-pilot.md); its values remain conditional.

Present one recommended design and only alternatives that resolve a real tradeoff. Include:

- A top-to-bottom Mermaid diagram and the failure boundaries.
- Each selected AWS service, its requirement, and whether to reuse or create it.
- Capacity assumptions, cost drivers and estimate date, accepted limitations, and blockers.
- Runtime, infrastructure, secret, and traffic-control ownership.
- Use the experimental `ts prebid server` commands for supported local checks, secret writes, and EC2 status. Record the selected YAML/secret delivery path and unsupported operations explicitly. Deployment, rollback, and other runtime support need a separately approved implementation; avoid competing wrappers for implemented commands.
- Target files and checks, with cloud-dependent checks separated from local checks.

Ask the user to approve the architecture, assumptions, target files, and accepted limitations. Approval to generate files is not approval to execute them. Reopen approval if later findings change topology, cost commitments, or ownership.

Done when the user explicitly approves the design and file scope. If blockers remain, agree on a bounded draft and label it incomplete rather than producing apparently deployable infrastructure.

## 4. Generate the approved files

Read [file generation and validation](references/file-generation.md). Follow existing repository conventions and generate only artifacts used by the selected design. Keep the decision record in the deployment plan; reference it from the runbook rather than repeating it.

Read the [PBS CLI usage and descriptor schema](../../../crates/trusted-server-cli/README.md) before generating inputs consumed by `ts prebid server`. Its current descriptor supports EC2/Compose only. Keep other architecture choices available, but mark their CLI integration deferred rather than generating unsupported fields.

Verify version-specific PBS fields and adapter bindings against the selected release. Verify AWS/Terraform behavior and pricing against current primary documentation. Record source links, versions, and verification dates in the deployment plan. Unavailable evidence remains a named blocker; do not invent image digests, configuration keys, prices, or benchmark results.

Done when every approved artifact exists, has a named owner and check, and every unresolved input is visible and prevents unsafe use where applicable. Every generated operator command must have documented inputs, access requirements, output, failure behavior, and a recovery action; proposing command names alone is not implementation.

## 5. Validate and hand off

Run the applicable safe checks in the loaded generation references and inspect the final diff. Report changed paths, exact commands, results, checks not run, and remaining blockers. Separate these states:

- Draft: unresolved generation inputs or incomplete artifacts.
- Locally checked: applicable local checks passed; cloud and integration behavior remain unverified.
- Ready for deployment review: file scope complete, blockers for generation cleared, local evidence recorded, and cloud plan/live checks listed for the authorized operator.

This skill never establishes production readiness from generated files alone. End with the next approval or evidence needed, not a provisioning command executed on the user's behalf.
