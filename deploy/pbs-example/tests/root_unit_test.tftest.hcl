mock_provider "aws" {
  mock_data "aws_partition" {
    defaults = { partition = "aws" }
  }

  mock_resource "aws_eip" {
    defaults = {
      public_ip = "192.0.2.10"
    }
  }

  mock_resource "aws_lb" {
    defaults = {
      dns_name = "east-alb.example.com"
      zone_id  = "Z00000000000000000001"
    }
  }

  mock_resource "aws_secretsmanager_secret" {
    override_during = plan

    defaults = {
      arn = "arn:aws:secretsmanager:us-east-1:123456789012:secret:example-AbCdEf"
    }
  }
}

mock_provider "aws" {
  alias = "west"

  mock_data "aws_partition" {
    defaults = { partition = "aws" }
  }

  mock_resource "aws_eip" {
    defaults = {
      public_ip = "192.0.2.20"
    }
  }

  mock_resource "aws_lb" {
    defaults = {
      dns_name = "west-alb.example.com"
      zone_id  = "Z00000000000000000002"
    }
  }

  mock_resource "aws_secretsmanager_secret" {
    override_during = plan

    defaults = {
      arn = "arn:aws:secretsmanager:us-west-2:123456789012:secret:example-GhIjKl"
    }
  }
}

override_resource {
  target          = module.east.aws_instance.pbs["us-east-1a"]
  override_during = plan
  values          = { id = "i-00000000000000001" }
}

override_resource {
  target          = module.east.aws_instance.pbs["us-east-1b"]
  override_during = plan
  values          = { id = "i-00000000000000002" }
}

override_resource {
  target          = module.west.aws_instance.pbs["us-west-2a"]
  override_during = plan
  values          = { id = "i-00000000000000003" }
}

override_resource {
  target          = module.west.aws_instance.pbs["us-west-2b"]
  override_during = plan
  values          = { id = "i-00000000000000004" }
}

variables {
  aws_account_id       = "123456789012"
  aws_profile          = "pbs-example"
  east_ami_id          = "ami-0123456789abcdef0"
  east_certificate_arn = "arn:aws:acm:us-east-1:123456789012:certificate/11111111-1111-4111-8111-111111111111"
  route53_zone_id      = "Z00000000000000000000"
  west_ami_id          = "ami-0fedcba9876543210"
  west_certificate_arn = "arn:aws:acm:us-west-2:123456789012:certificate/22222222-2222-4222-8222-222222222222"
}

run "plans_both_regional_provider_mappings" {
  command = plan

  assert {
    condition     = length(distinct(values(module.east.instance_ids))) == 2
    error_message = "The east provider mapping should plan two PBS instances."
  }

  assert {
    condition     = length(distinct(values(module.west.instance_ids))) == 2
    error_message = "The west provider mapping should plan two PBS instances."
  }

  assert {
    condition = (
      length(local.deployment_descriptor.regions) == 2 &&
      alltrue([for region in local.deployment_descriptor.regions : length(distinct(region.instance_ids)) == 2])
    )
    error_message = "The generated deployment descriptor should include two distinct instance IDs in each region."
  }

  assert {
    condition     = toset(keys(local.secret_bindings.examplebidder.secrets)) == toset(["us-east-1", "us-west-2"])
    error_message = "The generated binding should name an independent secret in each region."
  }

  assert {
    condition     = yamldecode(output.deployment_descriptor_yaml).pbs.bindings == "runtime/secret-bindings.generated.json"
    error_message = "The generated descriptor should reference the ignored generated binding file."
  }

  assert {
    condition     = jsondecode(output.secret_bindings_json).examplebidder.secrets.us-east-1 != jsondecode(output.secret_bindings_json).examplebidder.secrets.us-west-2
    error_message = "The rendered binding should contain distinct regional secret ARNs."
  }
}

run "plans_independent_regional_mtls_settings" {
  command = plan

  variables {
    east_client_ca_bundle_s3_bucket = "pbs-example-east-trust"
    east_client_ca_bundle_s3_key    = "client-ca/bundle-v1.pem"
    east_connection_logs_bucket     = "pbs-example-east-connection-logs"
    west_client_ca_bundle_s3_bucket = "pbs-example-west-trust"
    west_client_ca_bundle_s3_key    = "client-ca/bundle-v2.pem"
    west_connection_logs_bucket     = "pbs-example-west-connection-logs"
  }

  assert {
    condition = (
      module.east.alb_tls_settings.mode == "verify" &&
      module.east.alb_tls_settings.client_ca_bundle.bucket == var.east_client_ca_bundle_s3_bucket &&
      module.east.alb_tls_settings.client_ca_bundle.key == var.east_client_ca_bundle_s3_key &&
      module.east.alb_tls_settings.connection_logs_bucket == var.east_connection_logs_bucket
    )
    error_message = "The east ALB should use its own CA bundle and connection log bucket."
  }

  assert {
    condition = (
      module.west.alb_tls_settings.mode == "verify" &&
      module.west.alb_tls_settings.client_ca_bundle.bucket == var.west_client_ca_bundle_s3_bucket &&
      module.west.alb_tls_settings.client_ca_bundle.key == var.west_client_ca_bundle_s3_key &&
      module.west.alb_tls_settings.connection_logs_bucket == var.west_connection_logs_bucket
    )
    error_message = "The west ALB should use its own CA bundle and connection log bucket."
  }
}

run "rejects_east_ca_bundle_without_key" {
  command = plan

  variables {
    east_client_ca_bundle_s3_bucket = "pbs-example-east-trust"
  }

  expect_failures = [var.east_client_ca_bundle_s3_bucket]
}

run "rejects_west_ca_bundle_without_bucket" {
  command = plan

  variables {
    west_client_ca_bundle_s3_key = "client-ca/bundle-v1.pem"
  }

  expect_failures = [var.west_client_ca_bundle_s3_bucket]
}

run "rejects_duplicate_east_availability_zones" {
  command = plan

  variables {
    east_availability_zones = ["us-east-1a", "us-east-1a"]
  }

  expect_failures = [var.east_availability_zones]
}

run "rejects_empty_caller_allowlist" {
  command = plan

  variables {
    trusted_server_cidr_blocks = []
  }

  expect_failures = [var.trusted_server_cidr_blocks]
}

run "rejects_east_key_from_wrong_region" {
  command = plan

  variables {
    east_secrets_kms_key_arn = "arn:aws:kms:us-west-2:123456789012:key/11111111-1111-4111-8111-111111111111"
  }

  expect_failures = [var.east_secrets_kms_key_arn]
}

run "rejects_west_key_from_wrong_account" {
  command = plan

  variables {
    west_secrets_kms_key_arn = "arn:aws:kms:us-west-2:999999999999:key/22222222-2222-4222-8222-222222222222"
  }

  expect_failures = [var.west_secrets_kms_key_arn]
}
