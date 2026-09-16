mock_provider "aws" {
  mock_resource "aws_instance" {
    override_during = plan

    defaults = {
      id = "i-0123456789abcdef0"
    }
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

  mock_resource "aws_instance" {
    override_during = plan

    defaults = {
      id = "i-0fedcba9876543210"
    }
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
    condition     = length(module.east.instance_ids) == 2
    error_message = "The east provider mapping should plan two PBS instances."
  }

  assert {
    condition     = length(module.west.instance_ids) == 2
    error_message = "The west provider mapping should plan two PBS instances."
  }

  assert {
    condition     = length(local.deployment_descriptor.regions) == 2
    error_message = "The generated deployment descriptor should include both regions."
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
