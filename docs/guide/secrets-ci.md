# Provision secrets from CI

Automation calls the same reviewed Fastly `ts secrets set` command as interactive provisioning. There is no second CI secret writer. Direct writes to Axum, Cloudflare, and Spin are unsupported; use their [native instructions](./secrets).

::: warning Trust boundary
A config-only publisher must not control the workflow, CLI executable, shell scripts, or checkout revision executed with secret-writer credentials. Environment approval alone does not make attacker-controlled code safe. Use an administrator-controlled provisioning repository and dedicated runner when repository permissions cannot enforce this split. Do not run this job on untrusted pull requests, including `pull_request_target` checkouts of PR code.
:::

## Deployment inputs

Use existing `edgezero.toml` and adapter manifests. Do not add a deployment descriptor or a raw secret-store override to the command.

| Input                                                     | Meaning                                                                                                                            |
| --------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| `--manifest`                                              | Explicit EdgeZero manifest path; otherwise `EDGEZERO_MANIFEST`, otherwise `edgezero.toml`                                          |
| `--app-config`                                            | Reviewed unresolved config; otherwise `<app.name>.toml` beside the manifest                                                        |
| `--no-env`                                                | Disable ordinary `TRUSTED_SERVER__...` config overlays, not provider authentication or deployment selectors                        |
| `fastly.toml` `service_id`                                | Existing service identity in the selected adapter manifest                                                                         |
| `FASTLY_SERVICE_ID`                                       | Existing environment service selection; a conflict with the manifest is rejected unless using supported explicit service selection |
| `--service-id`                                            | Explicit Fastly selection; review the resulting service in the plan                                                                |
| `FASTLY_API_TOKEN`                                        | Authentication for managed direct API calls; a Fastly CLI profile alone does not supply it                                         |
| `EDGEZERO__STORES__SECRETS__TRUSTED_SERVER_SECRETS__NAME` | Physical app-secret store name                                                                                                     |
| `EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME`   | Physical app-config store name                                                                                                     |
| `EDGEZERO__STORES__KV__TRUSTED_SERVER_KV__NAME`           | Physical KV store name                                                                                                             |

Selectors contain names, not provider IDs. Unset selectors retain the logical-name defaults; the plan shows the selection and resolves exact Fastly resource IDs read-only. `ts provision --adapter fastly` creates/reuses resources. Managed deployment links them under their logical IDs. Secret writes do neither.

`--staging` on lifecycle commands does not isolate stores. Use separately reviewed physical mappings for staging and production. Keep the manifest, service selection, selectors, config, overlay policy, CLI/compiler version, and complete plan fixed between review and execution. A recreated store with the same name is still a different provider resource and requires a new plan.

## Bash stdin example

Run this only in a trusted provisioning environment. It expects the token and approved deployment inputs to be injected by a protected secret mechanism, and `bindings.json` to have been generated and reviewed from this exact config/revision. The explicit selectors below deliberately require a reviewed value instead of relying on ambient defaults.

```bash
set +x
set -euo pipefail
: "${TINYBIRD_AUCTION_TOKEN:?Missing Tinybird credential}"
: "${FASTLY_API_TOKEN:?Missing Fastly provisioning credential}"
: "${FASTLY_SERVICE_ID:?Missing reviewed Fastly service}"
: "${EDGEZERO_MANIFEST:?Missing reviewed manifest path}"
: "${EDGEZERO__STORES__SECRETS__TRUSTED_SERVER_SECRETS__NAME:?Missing reviewed secret store}"
: "${EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME:?Missing reviewed config store}"
: "${EDGEZERO__STORES__KV__TRUSTED_SERVER_KV__NAME:?Missing reviewed KV store}"
printf '%s' "$TINYBIRD_AUCTION_TOKEN" |
  env -u TINYBIRD_AUCTION_TOKEN ts secrets set tinybird.auction \
    --adapter fastly --app-config trusted-server.toml --no-env \
    --plan bindings.json --stdin --yes
unset TINYBIRD_AUCTION_TOKEN
```

`printf` is a Bash builtin. The value is piped over stdin, not passed to a subprocess on argv. `env -u` removes the source token from the CLI child environment. Never enable `set -x`, use `echo`, or write the value to a temporary artifact. `printf '%s'` does not append a newline. The CLI preserves input bytes and rejects invalid Bearer representation rather than trimming it.

`--stdin` requires `--yes`. This replaces the interactive confirmation, not plan review. All preflight comparison and input validation still apply. A repeated role such as `ec.inbound` or `handler.password` also needs `--binding` with the tool-generated `entry_id` chosen from the reviewed plan. Do not use a raw key, an array position, or an operator-created label.

## Protected GitHub Actions example

This example belongs in the separately controlled provisioning repository. It assumes an isolated self-hosted runner labelled `secret-writer` already has a reviewed immutable `ts` executable at `/opt/trusted-server/bin/ts`, `jq`, and Bash. Do not share that runner with untrusted builds. Replace the approved revision and CLI digest through administrator-controlled Environment variables before use; the checks reject empty or malformed values. This is a workflow template, not an executed CI rehearsal.

The reviewed repository revision contains `trusted-server.toml`, `bindings.json`, `edgezero.toml`, and its adapter manifest. Generate the plan with the same CLI and protected deployment selection before approving that immutable revision. Keep the workflow itself under administrator control too.

```yaml
name: Provision reviewed Tinybird binding
on:
  workflow_dispatch:
permissions:
  contents: read
concurrency:
  group: fastly-secret-provisioning
  cancel-in-progress: false
jobs:
  provision:
    runs-on: [self-hosted, secret-writer]
    environment: secret-provisioning
    env:
      APPROVED_REVISION: ${{ vars.PROVISIONING_REVISION }}
      CLI_SHA256: ${{ vars.TS_CLI_SHA256 }}
      EDGEZERO_MANIFEST: edgezero.toml
      FASTLY_SERVICE_ID: ${{ vars.FASTLY_SERVICE_ID }}
      EDGEZERO__STORES__SECRETS__TRUSTED_SERVER_SECRETS__NAME: ${{ vars.TS_SECRET_STORE }}
      EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME: ${{ vars.TS_CONFIG_STORE }}
      EDGEZERO__STORES__KV__TRUSTED_SERVER_KV__NAME: ${{ vars.TS_KV_STORE }}
    steps:
      - name: Validate immutable inputs
        shell: bash
        run: |
          set +x
          set -euo pipefail
          [[ "$APPROVED_REVISION" =~ ^[0-9a-f]{40}$ ]]
          [[ "$CLI_SHA256" =~ ^[0-9a-f]{64}$ ]]
          printf '%s  %s\n' "$CLI_SHA256" /opt/trusted-server/bin/ts | sha256sum --check --status
      - name: Check out approved provisioning inputs
        uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683
        with:
          ref: ${{ env.APPROVED_REVISION }}
          persist-credentials: false
      - name: Provision exact reviewed context
        shell: bash
        env:
          FASTLY_API_TOKEN: ${{ secrets.FASTLY_PROVISIONING_TOKEN }}
          TINYBIRD_AUCTION_TOKEN: ${{ secrets.TINYBIRD_AUCTION_TOKEN }}
        run: |
          set +x
          set -euo pipefail
          : "${TINYBIRD_AUCTION_TOKEN:?Missing Tinybird credential}"
          : "${FASTLY_API_TOKEN:?Missing Fastly provisioning credential}"
          : "${FASTLY_SERVICE_ID:?Missing reviewed Fastly service}"
          : "${EDGEZERO_MANIFEST:?Missing reviewed manifest path}"
          : "${EDGEZERO__STORES__SECRETS__TRUSTED_SERVER_SECRETS__NAME:?Missing reviewed secret store}"
          : "${EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME:?Missing reviewed config store}"
          : "${EDGEZERO__STORES__KV__TRUSTED_SERVER_KV__NAME:?Missing reviewed KV store}"
          printf '%s' "$TINYBIRD_AUCTION_TOKEN" |
            env -u TINYBIRD_AUCTION_TOKEN /opt/trusted-server/bin/ts secrets set tinybird.auction \
              --adapter fastly --app-config trusted-server.toml --no-env \
              --plan bindings.json --stdin --yes
          unset TINYBIRD_AUCTION_TOKEN
```

Restrict Environment access and approved revisions to provisioning administrators, require reviewers who can assess target changes, and prevent config-only publishers from changing those controls. Pin action code to immutable commits. Do not download or build mutable CLI code after approval. GitHub masking is not a substitute for keeping values out of output, request dumps, panic reports, artifacts, and process arguments.

The concurrency group serializes jobs in this repository. If other repositories or humans can write the same binding, use an operational lock shared with them. Do not assume GitHub concurrency is a provider-wide lock.

## Failure, reruns, and verification

Preflight failures must cause no write and no secret prompt. Changed config, overlays, plan, versions, manifest, service, or resolved stores require a newly reviewed plan. Unsupported adapters reject direct writes before accepting a value.

Repeat execution at the same context replaces the same generated binding; it does not create a new generation or slot. Do not automatically retry uncertain writes. Reconcile the provider outcome and test a fresh runtime state before an explicit retry with the same approved inputs. Permission failures are not a reason to broaden the destination or copy values to another purpose.

A confirmed write means storage only. Run `ts secrets check` for Fastly metadata presence, then recreate state and exercise an approved request with recipient-side verification. It does not prove the stored bytes, external acceptance, TLS/no-follow behavior, or warm-state refresh. Follow [rotation and retirement](./secret-binding-migration) for recipient overlap, S3 maintenance, retained states, and rollback.

Before release, execute the Bash path through the real CLI with fictional provider transport, inspect argv/stdin/output, test missing inputs and plan/resource mismatches with zero mutation, then rehearse in isolated protected CI. Native/provider/IAM proof and these workflow rehearsals remain gates until recorded as executed evidence.
