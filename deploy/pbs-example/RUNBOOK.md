# PBS example runbook

This runbook describes local checks and the separately authorized operations that would be needed for a real deployment. It does not authorize AWS access, Terraform apply, secret writes, traffic changes, or runtime deployment.

## Preconditions

- Work only in `deploy/pbs-example`.
- Use Terraform `1.16.2` and the committed AWS provider lock file.
- Use fictional values from `terraform.tfvars.example`, `deployment.example.yaml`, and `runtime/secret-bindings.example.json` only for local validation.
- Replace the fictional AWS account, profile, certificate, hosted-zone, AMI, and CIDR inputs before any authorized cloud plan. Terraform renders actual instance and secret identifiers after apply.
- Confirm the approved Trusted Server egress CIDRs. Do not use the documentation CIDR as a real allowlist.
- Confirm the PBS v4.7.0 image digest and every adapter binding again before release preparation.
- Keep state, saved plans, credentials, and AWS provider output out of Git and public logs.

## Local validation

From the repository root:

```bash
terraform fmt -recursive deploy/pbs-example
terraform -chdir=deploy/pbs-example init -backend=false -input=false
terraform -chdir=deploy/pbs-example validate
terraform -chdir=deploy/pbs-example test \
  -filter=tests/root_unit_test.tftest.hcl
terraform -chdir=deploy/pbs-example/modules/regional init \
  -backend=false -input=false
terraform -chdir=deploy/pbs-example/modules/regional test \
  -filter=tests/security_unit_test.tftest.hcl
```

`init -backend=false` downloads the locked provider but does not access the configured local state or AWS. Both selected test files use mocked AWS providers and explicit plan commands. Confirm that the root file runs five tests and the module file runs two tests. Regional assertions cover IMDSv2, encrypted root volumes, private subnet/AZ placement, the TLS policy, scoped ALB ingress/egress, alarms, and secret access. `validate` and mocked tests do not prove AWS permissions, quotas, AMI existence, certificates, subnet availability, or capacity.

Check the PBS descriptor without AWS access:

```bash
cargo run_cli_linux prebid server check \
  --deployment deploy/pbs-example/deployment.example.yaml \
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

Validate the JSON input and smoke command wiring without starting containers:

```bash
python3 -m json.tool \
  deploy/pbs-example/runtime/secret-bindings.example.json >/dev/null
bash -n deploy/pbs-example/scripts/smoke-runtime.sh
deploy/pbs-example/scripts/test-smoke-runtime.py
```

The wiring test requires Python 3 and Docker Compose. It renders production and smoke configurations, uses fake Docker lifecycle and curl commands, and checks that inherited selectors cannot replace the dummy inputs. It does not pull images, start PBS, or prove runtime health.

### Separately approved local runtime smoke

```bash
deploy/pbs-example/scripts/smoke-runtime.sh
```

The smoke script pulls the pinned image if needed, starts it with dummy values bound only to `127.0.0.1:18080`, requires `/status` to return `ok`, rejects either dummy credential appearing in startup logs, and removes its container and network on exit. It sends no auction request and contacts no bidder. Set `PBS_SMOKE_PORT` only when port `18080` is unavailable. The script forces the checked-in baseline and dummy secret file regardless of inherited `PBS_CONFIG_FILE` or `PBS_SECRET_ENV_FILE` values. Shared deployment Compose still binds all host interfaces so the ALB can reach PBS; the PBS security group restricts ingress.

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

## Generate operator inputs

After an authorized apply, run these commands from `deploy/pbs-example`:

```bash
terraform output -raw deployment_descriptor_yaml > deployment.generated.yaml
terraform output -raw secret_bindings_json > runtime/secret-bindings.generated.json

ts prebid server check --deployment deployment.generated.yaml --json
```

Inputs: the applied local state and unchanged Terraform source. Access: local state reads only; the CLI check makes no AWS call. Outputs: ignored, nonsecret files containing actual instance IDs and secret ARNs.

Failure behavior: stop if either output is empty, references the wrong account or region, or fails the CLI check. Do not repair generated identifiers manually. Reconcile Terraform source and state, then render both files again.

`ts prebid server status --deployment deployment.generated.yaml` is a separate authenticated EC2 read. Run it only after checking the account, role, and regions and obtaining cloud-read authorization.

## Secret value workflow

Terraform creates regional secret metadata and grants the EC2 role read access. It never writes credential values.

After an authorized operator has created the infrastructure and verified the declared secret ARNs, write a complete value through the existing CLI:

```bash
ts prebid server secrets set examplebidder \
  --deployment deployment.generated.yaml \
  --region us-east-1 \
  --file /secure/path/examplebidder.json \
  --request-token 11111111-2222-4333-8444-555555555555 \
  --yes --json
```

Inputs: an approved profile, the exact descriptor, one complete JSON payload, a fresh retry UUID, and operator approval. The payload must contain only the declared keys and must not appear in command arguments or logs.

Output: a nonsecret Secrets Manager version identifier and the target region. The command does not deploy PBS, refresh Compose, or prove bidder authorization.

Failure behavior: preserve the retry UUID and input file. A failed or unverifiable response means the write is not confirmed and its outcome may be uncertain. Reuse the token only for the original identical payload; use a new token only for separately intended changed values. Withheld provider errors prevent distinguishing a rejected write from a lost response. On partial regional success, record each region separately.

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
