# PBS example runbook

This runbook describes local checks and the separately authorized operations that would be needed for a real deployment. It does not authorize AWS access, Terraform apply, secret writes, traffic changes, or runtime deployment.

## Preconditions

- Work only in `deploy/pbs-example`.
- Use Terraform `1.16.2` and the committed AWS provider lock file.
- Use fictional values from `terraform.tfvars.example` only for local validation.
- Replace the fictional AWS account, profile, certificate, hosted-zone, AMI, CIDR, instance, and secret identifiers before any authorized cloud plan.
- Confirm the approved Trusted Server egress CIDRs. Do not use the documentation CIDR as a real allowlist.
- Confirm the PBS v4.7.0 image digest and every adapter binding again before release preparation.
- Keep state, saved plans, credentials, and AWS provider output out of Git and public logs.

## Local validation

From the repository root:

```bash
terraform fmt -recursive deploy/pbs-example
terraform -chdir=deploy/pbs-example init -backend=false -input=false
terraform -chdir=deploy/pbs-example validate
```

`init -backend=false` downloads the locked provider but does not access the configured local state or AWS. `validate` checks configuration structure, not AWS permissions, quotas, AMI existence, certificates, subnet availability, or capacity.

Check the PBS descriptor without AWS access:

```bash
cargo run_cli_linux prebid server check \
  --deployment deploy/pbs-example/deployment.yaml \
  --json
```

Expected result: the descriptor, image, bindings, regions, and regional YAML merge pass local validation. This command does not retrieve secrets, contact AWS, pull the PBS image, or start PBS.

Check Compose with dummy values:

```bash
docker compose \
  --env-file deploy/pbs-example/runtime/examples/compose.env \
  -f deploy/pbs-example/runtime/compose.yaml \
  config --quiet
```

Expected result: Compose renders successfully without pulling or starting the image. The dummy environment file is not a credential.

Validate the JSON input:

```bash
python3 -m json.tool deploy/pbs-example/runtime/secret-bindings.json >/dev/null
```

## Authorized Terraform workflow

These steps remain deferred. They require an approved AWS account, role, region scope, cost limit, and operator authorization.

1. Replace fictional variables with approved values in a protected local tfvars file.
2. Recheck the AWS identity and account against the variables.
3. Run `terraform init` with the local backend and review any lock-file change.
4. Run `terraform plan -out=pbs-example.tfplan`.
5. Review ALB exposure, CIDR rules, NAT gateways, IAM secret access, Route 53 records, instance placement, and expected cost.
6. Store the saved plan privately with its checksum. Do not place it in Git or a public CI log.
7. Apply only the reviewed saved plan after a separate approval.

Failure behavior: stop on wrong-account identity, an unexpected resource action, a certificate or zone mismatch, broad ingress, missing egress approval, or a secret policy broader than the declared bidder bindings. Reconcile the inputs and create a new plan. Never reuse a stale saved plan after source, state, credentials, or assumptions change.

## Secret value workflow

Terraform creates regional secret metadata and grants the EC2 role read access. It never writes credential values.

After an authorized operator has created the infrastructure and verified the declared secret ARNs, write a complete value through the existing CLI:

```bash
ts prebid server secrets set examplebidder \
  --deployment /secure/path/deployment.yaml \
  --region us-east-1 \
  --file /secure/path/examplebidder.json \
  --request-token 11111111-2222-4333-8444-555555555555 \
  --yes --json
```

Inputs: an approved profile, the exact descriptor, one complete JSON payload, a fresh retry UUID, and operator approval. The payload must contain only the declared keys and must not appear in command arguments or logs.

Output: a nonsecret Secrets Manager version identifier and the target region. The command does not deploy PBS, refresh Compose, or prove bidder authorization.

Failure behavior: preserve the retry UUID and input file. If the result is uncertain, retry the identical logical write rather than creating a new version. On partial regional success, record each region separately.

## Runtime release and rollback

The current CLI has no deploy or rollback command. A future approved runtime implementation must:

1. Resolve baseline plus regional overrides into one YAML file per region.
2. Retrieve and validate the selected immutable secret version.
3. Render `/run/pbs/secrets/examplebidder.env` with restrictive permissions, without shell evaluation.
4. Start or replace Compose with the pinned PBS image and read-only YAML.
5. Check ALB target health, `/status`, logs, and an isolated auction fixture.
6. Drain the old consumer before retiring it.
7. Record the release digest, resolved-config checksum, secret version, target, and result.
8. Use the recorded prior release and compatible secret version for rollback.

Do not manually edit generated runtime environment files. A secret update alone does not change a running container.

## Deferred evidence

An authorized operator still needs to verify TLS and DNS, IAM permissions, public ingress, NAT and bidder egress, real adapter credentials, AMI contents, reboot and host replacement, interrupted runtime replacement, secret rotation, representative load, regional failover, alarm delivery, and the Trusted Server auction path.
