# PBS example runbook

This runbook describes local checks and the separately authorized operations that would be needed for a real deployment. It does not authorize AWS access, Terraform apply, secret writes, traffic changes, or runtime deployment.

## Preconditions

- Work only in `deploy/pbs-example`.
- Use Terraform `1.16.2` and the committed AWS provider lock files in both the root and regional module. Both lock AWS `6.64.0` for Linux and macOS on AMD64 and ARM64.
- Local wiring checks require the Node.js version in `.tool-versions` and Docker with the Compose plugin. The development certificate test also needs OpenSSL. Both tests use only built-in Node.js modules; no npm install is needed.
- Use fictional values from `terraform.tfvars.example`, `deployment.example.yaml`, and `runtime/secret-bindings.example.json` only for local validation.
- Replace the fictional AWS account, profile, certificate, hosted-zone, AMI, and CIDR inputs before any authorized cloud plan. Terraform renders actual instance and secret identifiers after apply.
- Confirm the approved Trusted Server egress CIDRs. Do not use the documentation CIDR as a real allowlist.
- Confirm the PBS v4.7.0 image digest and every adapter binding again before release preparation.
- Keep state, saved plans, credentials, and AWS provider output out of Git and public logs. Input-file errors echo the full escaped path to stderr, including under `--json`; directory layouts and partner names in paths can therefore enter logs even though credential contents are withheld.

## Local validation

From the repository root:

```bash
terraform fmt -check -recursive deploy/pbs-example
terraform -chdir=deploy/pbs-example init -backend=false -input=false -lockfile=readonly
terraform -chdir=deploy/pbs-example validate
terraform -chdir=deploy/pbs-example test \
  -filter=tests/root_unit_test.tftest.hcl
terraform -chdir=deploy/pbs-example/modules/regional init \
  -backend=false -input=false -lockfile=readonly
terraform -chdir=deploy/pbs-example/modules/regional validate
terraform -chdir=deploy/pbs-example/modules/regional test \
  -filter=tests/security_unit_test.tftest.hcl
```

`init -backend=false` downloads the locked provider but does not access the configured local state or AWS. Both selected test files use mocked AWS providers. The regional suite applies the mTLS changes against the mock to test enabling and disabling verification. Confirm that the root file runs eight tests and the module file runs eleven. Root assertions require distinct instance IDs in each regional descriptor and verify the regional inputs reach the correct ALB. Module tests also cover resource-name boundaries and partition-specific SSM policy ARNs. Regional assertions cover IMDSv2, encrypted root volumes, private subnet/AZ placement, the TLS policy, scoped ALB ingress/egress, alarms, and secret access. `validate` and mocked tests do not prove AWS permissions, quotas, AMI existence, certificates, subnet availability, or capacity.

The `PBS example checks` workflow runs these Terraform checks and the JSON, shell, Compose wiring, and certificate checks below. The CLI test suite separately validates the committed deployment descriptor and its referenced inputs on every PR. The example owner remains responsible for local validation of adapted inputs. Provider upgrades must refresh both locks with `terraform providers lock -platform=darwin_arm64 -platform=darwin_amd64 -platform=linux_amd64 -platform=linux_arm64` from each directory and rerun both suites.

Check the PBS descriptor without AWS access. On Apple Silicon macOS, use `cargo run_cli_macos` instead of `cargo run_cli_linux`. On another host, use `cargo run --package trusted-server-cli --target "$(rustc -vV | awk '/host:/ { print $2 }')" --`:

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

Validate the JSON input, smoke command wiring, and development certificates without starting containers:

```bash
bash -n deploy/pbs-example/scripts/smoke-runtime.sh
bash -n deploy/pbs-example/scripts/make-dev-mtls-certs.sh
env -u NODE_OPTIONS -u NODE_PATH node --test deploy/pbs-example/scripts/test-smoke-runtime.mjs
env -u NODE_OPTIONS -u NODE_PATH node --test deploy/pbs-example/scripts/test-make-dev-mtls-certs.mjs
```

The wiring command clears inherited Node.js preload options and module paths. The script passes only `PATH` and `HOME` plus explicit test inputs to subprocesses. It parses the binding JSON, renders production and smoke configurations, uses fake Docker lifecycle and curl commands, and checks that inherited selectors cannot replace the dummy inputs. It does not pull images, start PBS, or prove runtime health.

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

## Optional ALB mutual TLS

These steps require separate approval for the AWS and Fastly changes. Use the [authorized Terraform workflow](#authorized-terraform-workflow) for each regional change.

### Issue a certificate and prepare S3

1. The PBS operator owns the private CA and keeps its certificate and key offline. The Trusted Server operator generates the client key locally and sends only the certificate signing request to the PBS operator. Issue an end-entity certificate with the `clientAuth` extended key usage and about a one-year lifetime.
2. Upload the public CA bundle to S3. Use one or more PEM CA certificates with no blank lines, and grant the Terraform principal `s3:GetObject` on the bundle. Upload each revision under a new object key, such as `client-ca/bundle-v2.pem`. Changing the bytes at the same key does not change Terraform's inputs.
3. If using connection logs, create a bucket in each ALB's region with SSE-S3 before setting `east_connection_logs_bucket` or `west_connection_logs_bucket`. Add a bucket policy that grants the log delivery service `s3:PutObject`. For example, with no log prefix:

   ```json
   {
     "Version": "2012-10-17",
     "Statement": [
       {
         "Effect": "Allow",
         "Principal": {
           "Service": "logdelivery.elasticloadbalancing.amazonaws.com"
         },
         "Action": "s3:PutObject",
         "Resource": "arn:aws:s3:::pbs-example-east-connection-logs/AWSLogs/123456789012/*"
       }
     ]
   }
   ```

   Replace the fictional bucket and account ID with the approved bucket and the ALB's account ID. Connection logging is independent of verification. To stop it, unset that region's log bucket input and apply before deleting the bucket.

Keep both the CA key and the client key out of Terraform variables, plans, and state. The client key belongs in Fastly Secret Store, not in S3 or Secrets Manager.

### Enable verification and check each region

1. Provision the active slot, `auction-providers-blue` unless Trusted Server sets `auction.mtls_slot = "green"`: its `.private-key` in Fastly Secret Store, and its `.certificate` as a PEM chain, leaf first, and `.hosts` as one HTTPS origin per line in Fastly Config Store.
2. Push the provider config with `mtls = true` only after every Trusted Server version serving traffic supports the setting. Older versions reject the config, which breaks all of their requests, not only auctions.
3. Wait until every Fastly sandbox serves the new config. With `reusable-sandbox`, running sandboxes keep the old config until they are recycled. The ALB does not ask for a client certificate before verify mode, so this step cannot be observed at the ALB. Then set both the CA bundle bucket and key for just one region, for example `east_client_ca_bundle_s3_bucket` and `east_client_ca_bundle_s3_key`. Confirm the plan creates a trust store and switches that region's HTTPS listener to `verify` without replacing its ACM certificate or changing the ingress allowlist. Apply the reviewed plan.
4. Confirm auctions routed to that region succeed. A `/status` request through the public HTTPS listener also needs a client certificate. Probe from an allowed network using an approved client identity. The target group's HTTP health check still reaches PBS without a client certificate. Use a regional endpoint covered by the ACM certificate or direct the probe to that region's ALB while preserving the shared hostname for TLS.
5. If connection logs are enabled for that region, open them in the regional log bucket under `AWSLogs/<account-id>/`. Match the probe or auction by time and listener. Check `tls_verify_status` for `Success` with the expected leaf certificate serial. A probe without a certificate should fail, with a `Failed:<error_code>` result in the logs. Do not rely on target health alone to confirm the public TLS handshake.
6. Repeat the plan, apply, and checks for the other region once the first is working.

### Rotate or roll back

For a leaf renewal under the same CA, issue a fresh key and certificate and switch Trusted Server's rotation slot. Write the new key, chain and hosts to the idle slot, for example `auction-providers-green`, wait a few minutes for Fastly to propagate them, then push the Trusted Server config with `auction.mtls_slot = "green"`. Check auctions and the new serial in the connection logs. Do not rewrite the active slot in place: its key and chain are separate writes that reach Fastly POPs at different times, and every `mtls` auction fails until both arrive. Keep the old slot's entries until the switch is confirmed, so switching back is the rollback.

For a CA rotation, first apply a bundle containing both CAs under a new S3 key and check both regions. Switch Trusted Server to the other rotation slot with a certificate from the new CA, and verify auctions. Then apply a bundle containing only the new CA under another new key. This example has no certificate revocation list inputs. To revoke trust, replace the bundle to remove the affected issuing CA. If its other clients still need access, rotate them to a new CA first. Reissuing a leaf alone does not revoke its predecessor. Bundle changes update the trust store in place. If you need to force replacement with the same trust store name, turn verification off first. AWS cannot create another store with that name before deleting the old one.

For rollback, unset both CA bundle inputs for each region and apply. Verify that the listeners return to `off`. Terraform detaches the trust store before deleting it. Confirm auctions still work before Trusted Server stops presenting the certificate. Remove `mtls` from provider config before deploying an older binary that rejects the field. With `reusable-sandbox`, existing sandboxes keep their loaded settings and backends until recycled.

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

Before relying on secret writes, the runtime owner must arrange an authorized sandbox check of the real AWS CLI and Secrets Manager contract. Use a throwaway secret and dummy values to confirm that `VersionId` equals `ClientRequestToken`, an identical retry is idempotent, and changed values with the same token are rejected. The pinned AWS CLI `2.36.45` was checked with isolated local configuration files: `aws configure get cli_history` and `default.cli_history` both returned exit 1 with empty stdout when unset, without API calls. Repeat that local check on CLI upgrades. Process tests also cover this shape with a fake AWS executable. Neither check establishes real Secrets Manager behavior; no live verification is implied by the local checks.

An authorized operator still needs to verify TLS and DNS, IAM permissions, public ingress, NAT and bidder egress, real adapter credentials, AMI contents, reboot and host replacement, interrupted runtime replacement, secret rotation, representative load, regional failover, alarm delivery, and the Trusted Server auction path.
