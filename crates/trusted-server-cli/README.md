# Trusted Server CLI: experimental PBS commands

`ts prebid server` manages local configuration inputs and a small set of AWS operations for self-hosted Prebid Server Go. It sits beside `ts prebid client`, which builds browser JavaScript, and remains separate from the existing Trusted Server `ts config` and `ts deploy` commands.

The namespace is experimental; its interface may change without a deprecation cycle. There is no separate binary or crate.

## Build and try locally

Use this branch's executable, not an older installed `ts`:

```bash
cargo build_cli_linux
cargo run_cli_linux prebid server --help
cargo run_cli_linux prebid server inspect --config trusted-server.example.toml --json
cargo run_cli_linux prebid server check --deployment crates/trusted-server-cli/examples/pbs/deployment.yaml
```

On Apple Silicon macOS use `build_cli_macos` and `run_cli_macos`. Other hosts can run `cargo run --package trusted-server-cli --target "$(rustc -vV | awk '/host:/ { print $2 }')" -- prebid server --help`. The examples contain fictional resource identifiers, a fictional image digest, and a fictional adapter binding. They exercise local checks only and must not be used as real deployment settings.

## Commands and current limits

| Command                                                                       | What it does                                                                                                                         | Access                        |
| ----------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------- |
| `ts prebid server inspect --config <file>`                                    | Reports routed Prebid Server providers and bidders separately from browser Prebid fields; host-secret requirements remain unresolved | Local read-only               |
| `ts prebid server check --deployment <file>`                                  | Validates schema, targets, binding metadata, and regional YAML merging                                                               | Local read-only               |
| `ts prebid server secrets set <bidder> --deployment <file> --region <region>` | Writes a complete JSON value to an existing, declared Secrets Manager secret after identity and confirmation checks                  | AWS reads and one value write |
| `ts prebid server status --deployment <file>`                                 | Reports EC2 instance state and infrastructure health for the explicitly listed instances                                             | AWS reads                     |

Add `--json` anywhere under `ts prebid server` for a machine-readable report. Errors and operator notices go to stderr; failures exit with code 2. A partial status report still appears on stdout, with `complete: false` and exit code 2.

Not implemented: container deployment, rollback, runtime secret injection, caller updates, Terraform execution, ECS status, or PBS application health checks. `status` always reports the installed release and consumed secret versions as unknown. EC2 health is not PBS readiness.

`check` does not validate every PBS configuration field against the upstream Go schema, independently verify adapter documentation, retrieve secrets, pull images, start PBS, or prove a deployment ready. Bindings are operator-supplied metadata, not a bundled bidder catalog. Validate actual adapter mappings and startup against the pinned PBS release before use.

## Discovering requirements

`inspect` reads exactly the chosen file and never rewrites or publishes it. It discovers server demand from `[auction.providers.*]` entries using the `prebid-server` profile and bidders routed through `[auction.bidders.*]`. The JSON report groups routed bidders under each provider. Browser settings still come from `[integrations.prebid]`, including the runtime's array, indexed-map, and string encodings for `client_side_bidders`. The command reports explicitly supplied values only; it does not expand defaults, environment overrides, remote configuration, or request-time inputs. Confirm which source and environment are authoritative before relying on the report.

Account identifiers, endpoint values, and bid-parameter values are withheld. Parser errors also withhold source snippets. Server-side bidders, client-side bidders, and browser bundle adapters remain separate; listing a bidder does not establish partner authorization or a host-secret requirement. Disabled auctions and integrations remain disabled.

## Deployment descriptor

See [deployment.yaml](examples/pbs/deployment.yaml), [PBS YAML](examples/pbs/pbs.yaml), [regional overrides](examples/pbs/east.yaml), and [bindings.json](examples/pbs/bindings.json).

Version 1 requires:

- `schema_version: 1`, an explicit environment, and `runtime: ec2-compose`.
- An explicit 12-digit AWS account ID and AWS CLI profile. Environment and bidder identifiers use letters, digits, underscores, and hyphens. Profile names may also use periods.
- A digest-pinned PBS image and a baseline YAML path.
- A nonempty region map with optional override paths and explicit EC2 instance IDs for `status`.
- An optional binding-file path. Omit it when no host secrets are needed.

Paths resolve relative to the descriptor, not the working directory. No automatic descriptor discovery or production default exists. Unknown descriptor fields, unsupported runtime types, duplicate YAML keys, tags, and implicit YAML merge keys are rejected.

Mappings merge recursively with regional values taking precedence; sequences and scalars replace whole values. Rendering happens in memory and writes no generated files. The command does not read arbitrary operator `PBS_*` environment overrides.

### Binding schema

The binding file is a JSON or YAML mapping keyed by bidder identifier. Each entry declares:

- `verified_image`: the exact PBS image string matching the descriptor.
- `source`: an HTTPS reference used by the operator to verify the mapping.
- `secrets`: one complete Secrets Manager ARN for every descriptor region, matching its account and region.
- `keys`: credential key names mapped to `env`, `pbs_path`, and optional `required`, which defaults to true.

Version 1 supports string-valued credentials and one binding set across all selected regions. Each secret ARN must belong to exactly one bidder binding because updates replace the complete JSON object. A PBS destination cannot overlap another binding or a value already present in the resolved YAML, including a non-mapping parent. Names alone do not prove that PBS supports the mapping; the metadata records an operator decision.

## Setting a secret

Prerequisites: AWS CLI v2 on PATH, a trusted local AWS profile using short-lived credentials, and a previously provisioned secret with approved permissions. AWS CLI command history must be disabled; the tool checks both the selected profile and default history settings before submitting a value. It suppresses AWS stderr and disables configured endpoint URL overrides for API calls.

The command verifies the account through STS, describes the exact declared secret, and refuses replica writes or secrets scheduled for deletion. Interactive use reads a complete JSON object through a hidden terminal prompt, shows the target and retry UUID, and requires typing `yes` before writing. The prompt belongs in the operator's terminal, not an agent conversation.

For approved automation, supply a file or stdin, `--yes`, and a stable UUID identifying the logical write:

```bash
ts prebid server secrets set examplebidder \
  --deployment /secure/path/deployment.yaml \
  --region us-east-1 \
  --file /secure/path/credential.json \
  --request-token 11111111-2222-4333-8444-555555555555 \
  --yes --json
```

The command above is an interface example, not authorization to run it. Generate a new UUID for each logical update and reuse that UUID with identical values for retries. Reusing it with different values fails in Secrets Manager. `--yes` confirms only this value write; it does not authorize deployment, partner rotation, or future writes. Use protected CI approval gates before invoking it.

File and stdin inputs are mutually exclusive. `--stdin` requires `--yes` and `--request-token`. Input must contain only declared keys, include all required nonempty string values, and fit the Secrets Manager size limit. Optional keys may be omitted. Duplicate keys and non-string values are rejected. Quotes, dollar signs, newlines, and backslashes are encoded as JSON, never shell expressions.

Secret values never enter command arguments or reports. The AWS CLI receives JSON through a tool-owned temporary file, owner-only on Unix, which is removed on normal success and error paths. Operator-provided input files are not changed or deleted. Input-file errors print the full escaped path to stderr, including under `--json`; directory layouts and partner names in paths can enter logs even though credential contents are withheld. Run on a trusted host with protected temporary storage; abrupt process termination can leave temporary files requiring cleanup. Windows temporary-file ACL behavior has not been validated.

A successful write reports its version identifier. It does not create secret metadata, change infrastructure, replace containers, or rotate the bidder's credential. Check regional replication, separately replace consumers, and verify them before revoking old partner credentials. A failed or unverifiable response means the write is not confirmed; the outcome may be uncertain. Retain the displayed request token and reuse it only for the original identical payload. Use a new token only for separately intended changed values. Provider error details are withheld, so the CLI cannot distinguish a rejected write from a lost response.

## Ownership and sandbox compatibility

These commands do not adopt the existing sandbox's Terraform state, edit its files, or change its IAM roles. A descriptor must reference resources the operator has explicitly approved. The sandbox currently bootstraps runtime files through Terraform user data and has no runtime secret loader. Writing a secret therefore does not make that sandbox consume it.

Deployment and rollback require a separately approved move to versioned runtime releases. Until that exists, `ts prebid server` has no deployment or rollback subcommands and the skill must not promise them.

## Verification

```bash
./scripts/test-cli.sh
cargo fmt --all -- --check
cargo clippy --package trusted-server-cli --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
```

Unit tests cover local discovery, rendering, binding conflicts, account checks, confirmation, payload validation, retries, and status limitations. Unix process-level tests run the actual `ts` binary with a fake `aws` executable and require Python 3. They verify no AWS execution for local commands, private temporary requests, absence of credentials in arguments/output, cleanup, history/account refusal, and partial-report exit codes. They never contact AWS. The unset-history case covers exit 1 with empty stdout, but this fake response does not establish the real AWS CLI contract. An authorized runtime owner must verify real secret-write version IDs and retry behavior against a throwaway secret before operational use.

Process tests also send deeply nested flow sequences, flow mappings, and block mappings through `check`. The pinned `serde_yaml_ng` parser rejects them with sanitized errors before the recursive configuration walkers run. These tests guard the parser's depth-limit behavior; they are not proof against every possible YAML resource-exhaustion input.
