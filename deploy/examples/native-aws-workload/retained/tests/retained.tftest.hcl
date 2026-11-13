mock_provider "aws" {
  override_during = plan

  mock_resource "aws_dynamodb_table" {
    defaults = {
      arn = "arn:aws:dynamodb:eu-west-2:000000000000:table/example-retained-ec"
    }
  }
}

variables {
  aws_account_id = "000000000000"
  aws_region     = "eu-west-2"
  table_name     = "example-retained-ec"
  namespace      = "example-staging"
  owner          = "example-platform-team"
}

run "retained_table_contract" {
  command = plan

  assert {
    condition = (
      aws_dynamodb_table.ec.name == "example-retained-ec" &&
      aws_dynamodb_table.ec.billing_mode == "PAY_PER_REQUEST" &&
      aws_dynamodb_table.ec.hash_key == "pk" &&
      aws_dynamodb_table.ec.range_key == "sk" &&
      aws_dynamodb_table.ec.attribute == toset([{ name = "pk", type = "S" }, { name = "sk", type = "S" }])
    )
    error_message = "Retained EC storage requires only the string pk/sk table layout."
  }

  assert {
    condition = (
      aws_dynamodb_table.ec.deletion_protection_enabled &&
      one(aws_dynamodb_table.ec.ttl).enabled &&
      one(aws_dynamodb_table.ec.ttl).attribute_name == "ttl_epoch_s" &&
      length(aws_dynamodb_table.ec.server_side_encryption) == 0
    )
    error_message = "Keep deletion protection, identity TTL and default AWS-owned encryption."
  }

  assert {
    condition = output.ec_storage == {
      provider  = "dynamodb"
      region    = "eu-west-2"
      table     = "example-retained-ec"
      namespace = "example-staging"
      } && output.table_arn == aws_dynamodb_table.ec.arn && output.owned_partitions == {
      identity  = "ec:example-staging"
      allocator = "ec-allocator:example-staging"
    }
    error_message = "The retained output must supply the workload and envelope storage binding plus both reserved partitions."
  }
}
