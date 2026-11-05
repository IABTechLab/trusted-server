output "table_arn" {
  description = "Exact table ARN for operator and retained boundary review."
  value       = aws_dynamodb_table.ec.arn
}

output "task_policy_json" {
  description = "Task allow document. Merge equivalent constrained allows into the reviewed boundary separately."
  value       = jsonencode(local.task_policy)
}

output "ec_storage" {
  description = "Non-secret ec.storage object to publish in the existing Trusted Server app-config envelope. No credential or execution-role changes."
  value = {
    provider  = "dynamodb"
    region    = var.aws_region
    table     = aws_dynamodb_table.ec.name
    namespace = var.namespace
  }
}

output "owned_partitions" {
  description = "Reserve both partition values for the named owner. Never reset the allocator while reusing the namespace."
  value = {
    identity  = local.identity_partition
    allocator = local.allocator_partition
  }
}
