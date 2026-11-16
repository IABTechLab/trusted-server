output "table_arn" {
  description = "Exact table ARN for operator and retained boundary review."
  value       = aws_dynamodb_table.ec.arn
}

output "ec_storage" {
  description = "Non-secret native KV record binding to pass to workload ec_storage, not published ec.storage. No credential or execution-role changes."
  value = {
    provider  = "aws-dynamodb-records"
    region    = var.aws_region
    table     = aws_dynamodb_table.ec.name
    namespace = var.namespace
  }
}

output "record_partition_pattern" {
  description = "Reserve these record groups, including their retained allocator rows, for the named owner."
  value       = local.record_partition_pattern
}
