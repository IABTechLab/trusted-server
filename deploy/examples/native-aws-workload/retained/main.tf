locals {
  identity_partition  = "ec:${var.namespace}"
  allocator_partition = "ec-allocator:${var.namespace}"
}

resource "aws_dynamodb_table" "ec" {
  name                        = var.table_name
  billing_mode                = "PAY_PER_REQUEST"
  hash_key                    = "pk"
  range_key                   = "sk"
  deletion_protection_enabled = true

  attribute {
    name = "pk"
    type = "S"
  }

  attribute {
    name = "sk"
    type = "S"
  }

  ttl {
    attribute_name = "ttl_epoch_s"
    enabled        = true
  }

  # Omit server_side_encryption to use DynamoDB's default AWS-owned key.
  # Allocator rows never carry ttl_epoch_s. Retain them while the namespace
  # can be reused; TTL applies only to expiring identity rows.
  lifecycle {
    prevent_destroy = true
  }
}
