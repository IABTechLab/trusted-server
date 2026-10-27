output "table_arn" {
  description = "Exact table ARN for operator and retained boundary review."
  value       = aws_dynamodb_table.ec.arn
}

output "task_policy_json" {
  description = "Task allow document. Merge equivalent constrained allows into the reviewed boundary separately."
  value       = jsonencode(local.task_policy)
}

output "runtime_environment" {
  description = "Non-secret ECS app-container environment. No execution-role or credential changes."
  value = {
    TS_EC_DYNAMODB_ENABLED   = "true"
    TS_EC_DYNAMODB_REGION    = var.aws_region
    TS_EC_DYNAMODB_TABLE     = aws_dynamodb_table.ec.name
    TS_EC_DYNAMODB_NAMESPACE = var.namespace
  }
}

output "owned_partitions" {
  description = "Reserve both partition values for the named owner. Never reset the allocator while reusing the namespace."
  value = {
    identity  = local.identity_partition
    allocator = local.allocator_partition
  }
}
