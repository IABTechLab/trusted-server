locals {
  identity_partition  = "ec:${var.namespace}"
  allocator_partition = "ec-allocator:${var.namespace}"

  # This allow document does not override an explicit deny in a boundary, SCP
  # or endpoint policy. It is also the input for the separate boundary review.
  task_policy = {
    Version = "2012-10-17"
    Statement = [
      {
        Sid      = "ValidateExactEcTable"
        Effect   = "Allow"
        Action   = ["dynamodb:DescribeTable"]
        Resource = aws_dynamodb_table.ec.arn
      },
      {
        Sid      = "IdentityRowsOnly"
        Effect   = "Allow"
        Action   = ["dynamodb:GetItem", "dynamodb:PutItem", "dynamodb:DeleteItem", "dynamodb:Query"]
        Resource = aws_dynamodb_table.ec.arn
        Condition = {
          "ForAllValues:StringEquals" = { "dynamodb:LeadingKeys" = [local.identity_partition] }
          Null                        = { "dynamodb:LeadingKeys" = "false" }
        }
      },
      {
        Sid      = "AllocateGenerationsOnly"
        Effect   = "Allow"
        Action   = ["dynamodb:UpdateItem"]
        Resource = aws_dynamodb_table.ec.arn
        Condition = {
          "ForAllValues:StringEquals" = { "dynamodb:LeadingKeys" = [local.allocator_partition] }
          Null                        = { "dynamodb:LeadingKeys" = "false" }
        }
      }
    ]
  }
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

  server_side_encryption {
    enabled = true
  }

  # The allocator lives as long as this namespace can be used. Do not put this
  # table in disposable service state or remove this protection to stop ECS.
  lifecycle {
    prevent_destroy = true
  }
}

resource "aws_iam_role_policy" "ec" {
  name   = "trusted-server-native-ec"
  role   = var.task_role_name
  policy = jsonencode(local.task_policy)
}
