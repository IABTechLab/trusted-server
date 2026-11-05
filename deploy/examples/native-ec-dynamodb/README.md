# Retained native EC DynamoDB example

This Terraform root creates one regional EC table and attaches a scoped inline policy to an existing ECS application task role. It does not create ECS, change a permissions boundary, grant operator permissions, or touch the execution role.

See [the deployment guide](../../../docs/guide/native-ec-persistence.md) before using it. No AWS deployment is authorized by this example.

## Ownership

Keep this root in retained state, separate from disposable workload state. Supply a separately owned S3 backend bucket, a unique state key, region, encryption and access controls. S3 lockfile locking is enabled. Do not copy an existing workload's backend key. Commit the provider lock file from an approved initialization before deployment.

The required `owner` owns the table, both reserved namespace partitions, allocator continuity, retention decisions, ongoing cost and eventual decommissioning. The task role remains owned by its workload. Removing the workload must not remove this table. Before retiring a role, remove or transfer this root's inline-policy attachment so retained state does not reference a deleted role.

The table has `PAY_PER_REQUEST` capacity, string keys `pk` and `sk`, service TTL `ttl_epoch_s`, default service-managed encryption, deletion protection and Terraform `prevent_destroy`. It has no indexes or replicas. TTL is not an immediate deletion guarantee. The backend enforces logical expiration with `expires_at_ms`.

Reserve both exact partition keys for one app/environment:

- `ec:{namespace}` contains identities, tombstones and withdrawal markers.
- `ec-allocator:{namespace}` contains the internal `sk=generation` counter. It has no TTL and is not an identity row. Never delete or reset it if the namespace may be reused.

Terraform does not seed or manage individual items. The runtime atomically allocates generations. Protect allocator continuity during backup, restore, migration and cleanup. Restoring an older allocator while serving newer rows can allow a stale generation to match again.

## Review and local checks

Copy `terraform.tfvars.example` to an approved, untracked input location and replace every illustrative identifier. Review the account, region, retained state key, task role, table/data lifetime and both namespace partitions. A namespace is not a secret and must not contain customer data.

The no-network format check is:

```sh
terraform fmt -check -recursive deploy/examples/native-ec-dynamodb
```

Provider initialization, planning and any application need separate authorization and approved credentials. This root pins Terraform and the AWS provider but intentionally supplies no live backend coordinates or credentials. It does not call AWS during the format check.

## IAM integration

`task_policy_json` contains exactly `DescribeTable`, `GetItem`, `PutItem`, `DeleteItem`, `Query` and `UpdateItem` on the table ARN. Row operations require the exact `ec:` partition. Only `UpdateItem` may access the exact allocator partition. `DescribeTable` is table-level and has no partition condition. There are no index ARNs, wildcard table grants, `Scan`, table creation or TTL-management permissions.

The root attaches the policy to `task_role_name`; that is not enough when the existing task boundary explicitly denies DynamoDB. Have the boundary owner review both its action ceiling and its constrained allow statements, then review the task inline policy. Do not remove the boundary, broaden the execution role, or weaken the deny to get a successful test. Other policies can still deny the calls.

`ec_storage` replaces the former `runtime_environment` output. Publish this non-secret object as `[ec.storage]` in the existing Trusted Server app-config BlobEnvelope before launching an application built with `aws-dynamodb-ec`. Use `terraform output -json ec_storage` only against the separately approved retained state; this guide does not run it. Keep the exact table and namespace unchanged. Config publication is the existing `ts config push`/AppConfig workflow, not container environment injection or another envelope format. The old enabled `TS_EC_DYNAMODB_ENABLED` launch contract now fails before readiness, even alongside new config. Remove the old selection and binding variables after preparing publication. External consumers of the removed output must migrate with this change. The ordinary AWS workload credential chain still supplies task credentials. Do not add access keys to Terraform or the task definition.

## Retirement

Stopping ECS leaves this table and state intact. The owner must separately approve data erasure, allocator retirement, backup disposition and table removal. Only reuse a namespace if its allocator history is preserved. A deliberate data deletion is not permission to reset generation history. `prevent_destroy` and deletion protection block ordinary table removal; they are not a backup policy and cannot prevent an authorized task from deleting identity rows.
