# Native EC persistence

The native Axum adapter can use a dedicated DynamoDB EC backend. Build with `aws-dynamodb-ec` and select it explicitly at startup. This is separate from ordinary byte KV and from immutable AppConfig or secret snapshots. A local database file on an ephemeral ECS volume is not a substitute for retained identity state.

Persistence does not grant consent. Keep the shared EC consent, bot, browser-navigation and withdrawal rules intact. A request can be eligible for an auction but ineligible for an EC. Missing qualifying consent in a jurisdiction that requires it must still prevent EC issuance. Consent capture and CMP integration are separate work.

```mermaid
flowchart TD
    Start[Native startup] --> Validate[Validate selected regional table]
    Validate --> Ready[Application ready]
    Ready --> Request[Checked request and real consent signals]
    Request --> Gate{Eligible publisher navigation and EC consent?}
    Gate -->|No| NoMint[Do not mint an identity]
    Gate -->|Yes| Allocate[Atomically allocate a fresh generation]
    Allocate --> Persist[Conditionally persist identity row]
    Persist -->|Confirmed| Finalize[Await shared EC response finalization]
    Persist -->|Failed or uncertain| NoMint
    Finalize --> Cookie[Commit response headers and ts-ec cookie]
```

## Native eligibility and route boundaries

Native eligibility uses checked effective client metadata from managed ingress and a user-agent platform/browser-engine heuristic. It does not invent Fastly TLS or JA4 headers. The browser heuristic is not spoof-resistant authentication. Only a GET publisher document navigation that passes integration request filters may issue a new identity. Health checks, static assets, preflights and rejected authentication do not mint ECs. Consent behavior is unchanged.

Native supplies the live graph and partner registry to existing auction, page-bids and integration routes, and to read-only identify. EID cookies and IDs supplied in auction bodies are supported. Identify does not create identities. The adapter awaits shared EC response finalization exactly once, including handled errors, before committing response headers.

Native admin EC management and lookup remain unsupported and require separate enablement. This work does not provide Fastly S2S batch or post-send pull-sync parity; those native routes and background tasks are not integrated here.

## Build and select the backend

From the repository root, add the EC feature to the features your native image already needs:

```sh
docker build --platform linux/amd64 \
  --build-arg TRUSTED_SERVER__GIT_VERSION="$(git rev-parse HEAD)" \
  --build-arg NATIVE_FEATURES=aws-appconfig-agent,aws-secrets-manager,aws-dynamodb-ec \
  -t trusted-server-native-ec:local .
```

This command is an operator example, not evidence of a qualified image. Rebuild the final runtime image from the new binary and record its source, dependency and image digests. A historical AppConfig/Secrets Manager image qualification does not cover EC persistence merely because environment variables were added.

Set these variables on the application container, not the AppConfig Agent:

```sh
TS_EC_DYNAMODB_ENABLED=true
TS_EC_DYNAMODB_REGION=us-east-1
TS_EC_DYNAMODB_TABLE=example-retained-ec
TS_EC_DYNAMODB_NAMESPACE=example-staging
```

`TS_EC_DYNAMODB_ENABLED=true` selects the provider. `false` or absence leaves it off. Region, table and namespace are required when selected. Use the same region as the table. The TS region setting is explicit; do not rely on an unrelated `AWS_REGION` value to select the EC table region. Selected-provider preparation must succeed before readiness. Do not treat a selected-provider failure as permission to fall back to memory or emit an unbacked cookie.

The provider uses the standard AWS workload credential chain. In ECS, the application uses the task role. Keep credential values out of deployment inputs, logs, Terraform and container environment. The execution role still handles image pull, logs and task startup secrets, not application DynamoDB calls. Without a local-test selection, production preparation ignores configured endpoint overrides and uses the regional AWS endpoint. Local-only acceptance can set `TS_EC_DYNAMODB_LOCAL_ENDPOINT=http://127.0.0.1:8000`. This seam accepts only loopback HTTP with an explicit port and root path, bypasses the AWS config and credential chain, and uses dummy credentials. Never set it in a deployed task. In-process fixtures can also inject a client through the test preparation API.

The table is a single-region base table. Do not add Global Tables or route writes to different regions and assume the same CAS guarantees. A single namespace partition and generation counter can become hot; `PAY_PER_REQUEST` is not a throughput qualification.

## Table and namespace ownership

The separate examples unit provides `deploy/examples/native-ec-dynamodb/`. This self-contained Terraform example creates a retained table and attaches exact-table permissions to a supplied task role. It does not create or deploy an ECS service.

| Item                | Required layout or owner                                                                                                |
| ------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| Table               | String hash key `pk`, string range key `sk`, `PAY_PER_REQUEST`, no indexes or replicas                                  |
| Identity partition  | `pk=ec:{namespace}`, `sk` is the exact EC or withdrawal-marker key                                                      |
| Allocator partition | `pk=ec-allocator:{namespace}`, `sk=generation`, numeric generation counter only, no TTL                                 |
| Service TTL         | Enabled on numeric `ttl_epoch_s` for expiring identity/marker rows                                                      |
| Logical expiry      | `expires_at_ms` filters lookup/list results and expiry-sensitive write conditions; held-row existence stays physical    |
| Retained owner      | Named team/person owns state, table, namespace reservations, retention, allocator continuity, costs and decommissioning |
| Workload owner      | Owns the application task role, provider-enabled image and task environment                                             |

The namespace must be nonempty, at most 512 UTF-8 bytes, and contain no Unicode control characters. The runtime does not trim or normalize it. Backend keys must be nonempty, at most 1024 bytes, and contain no control characters; withdrawal-marker keys are supported.

Reserve both partition values together for exactly one app/environment. Reuse the namespace across task replacements that should share identities. Use a distinct reservation for a separate environment. Do not share a namespace between unrelated applications or use an ephemeral task/run ID as its identity.

The internal allocator is not part of EC prefix listing. It has no `ttl_epoch_s` or logical expiration. Never delete, expire or reset it while the namespace can be used again. A delete/recreate cycle must not reuse a generation that an in-flight stale writer still holds. Failed conditional writes may consume unused generations; gaps are harmless.

Retain the allocator with the graph through backup, restore and migration. Rewinding it independently of rows can break stale-CAS protection. Do not restore old state into an active namespace without an explicit continuity plan. This example does not implement migration or disaster recovery.

The graph owns live-row, tombstone and withdrawal-marker lifetimes. DynamoDB TTL only collects expired rows eventually. It does not define when a row stops authorizing EC use. `expires_at_ms` uses the application's UTC wall clock, so logical expiry and expiry-dependent Add/CAS conditions assume adequately synchronized task clocks. The deployment owner must qualify clock synchronization and skew across tasks. The backend adds no runtime skew guard or clock configuration. Current graph defaults are 31,536,000 seconds for live rows and 86,400 seconds for tombstones and completion markers. Do not apply a blanket TTL or cleanup job to the allocator partition.

Lookup and prefix listing hide logically expired rows. Authoritative `key_exists` instead uses a strongly consistent exact-key read to confirm physical row presence, including logically expired rows. This means the identity is still held, not that it is logically readable. The read validates the expiry format; malformed rows are errors, not absence. Physical presence never authorizes identity use, EIDs or cookie issuance. It lets the graph write a fresh-generation withdrawal tombstone even when a CAS captured `:now` before expiry but arrives afterward. Treating the expired, still-held root as absent would skip that tombstone and let the delayed CAS match the old generation and revive the identity or partner IDs.

## Task permissions and the existing boundary

The runtime needs the following permissions on the exact table ARN:

| Actions                                                                         | Scope                                                                      |
| ------------------------------------------------------------------------------- | -------------------------------------------------------------------------- |
| `dynamodb:DescribeTable`                                                        | Exact table, no row condition; startup schema validation                   |
| `dynamodb:GetItem`, `dynamodb:PutItem`, `dynamodb:DeleteItem`, `dynamodb:Query` | Exact table and `dynamodb:LeadingKeys` equal to `ec:{namespace}`           |
| `dynamodb:UpdateItem`                                                           | Exact table and `dynamodb:LeadingKeys` equal to `ec-allocator:{namespace}` |

No task permission is needed for `Scan`, `CreateTable`, `DescribeTimeToLive`, `UpdateTimeToLive`, index creation or table deletion. Table creation and TTL configuration belong to the infrastructure operator. The allocator does not require `GetItem`. The example uses separate row and allocator statements, so ordinary identity deletion cannot delete the allocator through this policy. IAM partition scoping does not enforce a particular `sk` or stop a compromised task from modifying permitted rows.

The public native-demo design has two relevant policy layers. Its task inline policy grants runtime reads. The retained bootstrap task boundary has an explicit `Deny` with `NotAction` based on a read-action ceiling that excludes DynamoDB. Adding an inline allow alone still fails.

Before any deployment, the boundary and workload owners must review both changes:

1. Extend the task boundary's action ceiling with only the six required DynamoDB actions. Also add constrained boundary allows for the exact table and the two reviewed partitions. Preserve existing unrelated restrictions. An action-ceiling edit alone does not grant permission.
2. Add the equivalent exact-table, partition-scoped allow to the application's task inline policy. Review all effective denies, including organization and endpoint policies, with the actual task credentials.

The example's `task_policy_json` is the constrained allow input for that review, not a replacement boundary document. Do not replace the boundary, add administrator access, remove explicit denies indiscriminately, or move the application calls to the execution role. No execution-role change is needed. Existing Secrets Manager-only KMS statements do not authorize unrelated KMS use. The example uses default service-managed DynamoDB encryption and adds no task KMS calls; a customer-managed key needs a separate review.

## Retained infrastructure, disposable workload

Keep the EC Terraform root in its own retained state, separate from the service's disposable run state. Give it a unique locked backend key and an accountable retention owner. The example enables table deletion protection and Terraform `prevent_destroy`. Neither replaces a backup, retention or erasure policy.

Existing disposable-workload writers, manifests, inventory and cleanup code must not be assumed to know about DynamoDB. Provision this retained root through a separately reviewed operator path. Do not quietly add a retained table to automated disposable cleanup. If that ownership model changes, review creation grants, manifests, inventory, state and destruction together.

Stopping or replacing an ECS task must leave EC state available. Keep the selected backend live while issued identities and withdrawal obligations exist. Disabling it is not a safe persistence rollback; qualify a rollback that preserves those obligations before changing provider selection. Before deleting the task role, remove or transfer the retained root's policy attachment. Identity erasure, namespace retirement, backups and table removal require their own reviewed procedure. Never reset the generation allocator as part of routine row cleanup.

The workload still needs working regional HTTPS access to DynamoDB. This guide does not require a new NAT gateway or endpoint without checking the existing routing and endpoint policies. A read-only application root and ephemeral bindings volume can stay unchanged; neither stores the identity graph.

## Qualification before deployment

No AWS apply or live acceptance is implied by this guide. Local fixtures cannot prove installed IAM policies, capacity or real browser consent transport.

With an already-built `aws-dynamodb-ec,geo-cloudfront` native binary and DynamoDB Local running on loopback, run:

```sh
node scripts/test-native-ec-image.mjs \
  --binary /absolute/path/to/trusted-server-axum \
  --dynamodb http://127.0.0.1:8000
```

Alternatively, supply `--image LOCAL_IMAGE` instead of `--binary`. Linux image mode uses host networking and never pulls or builds an image. The fixture generates isolated envelopes and secrets, owns a unique local table, and writes JSON evidence. It calls the app executable directly, so it does not qualify a deployment bootstrap wrapper. Use `--help` for image executable and source-version assertions.

Before authorizing a deployment, retain evidence for the rebuilt image and these checks:

- Real conditional expressions against DynamoDB Local, including concurrent Add, stale CAS, delete/recreate, metadata, logical expiry before TTL deletion, marker keys and short or empty Query pages with continuation. A bounded list that cannot prove exhaustion must fail rather than return an unsafe short result.
- Native lifecycle tests proving persistence acknowledgement precedes new `Set-Cookie: ts-ec`, reuse survives application recreation, withdrawal writes a tombstone and expires the browser cookie, and an old EID update cannot undo withdrawal.
- Failure cases for denied, unavailable and uncertain writes. No phantom identity may be issued. Test readiness failure and current-thread runtime scheduling while storage is delayed.
- Existing route exclusions and consent rules. Health, static assets, preflight and rejected authentication must not mint identities. Missing qualifying consent must remain blocked. Synthetic local consent fixtures are not live-browser consent acceptance.
- The actual final image's provider selection, non-root/read-only execution, startup, readiness, normal HTTPS trust and shutdown behavior, plus affected Fastly/WASM compatibility after shared EC changes.

After separate AWS authorization, qualify the real task role with its retained boundary and inline policy. Prove allowed operations on the reviewed table/partitions and denial elsewhere, allocator retention, logical TTL and persistence across task replacement. Separately verify a real browser request carrying genuine qualifying consent. Do not relax privacy configuration or fabricate a live signal to make that acceptance pass.
