mock_provider "aws" {
  mock_resource "aws_instance" {
    defaults = {
      id = "i-0123456789abcdef0"
    }
  }

  mock_resource "aws_eip" {
    defaults = {
      public_ip = "192.0.2.10"
    }
  }

  mock_resource "aws_secretsmanager_secret" {
    override_during = plan

    defaults = {
      arn = "arn:aws:secretsmanager:us-east-1:123456789012:secret:example-AbCdEf"
    }
  }
}

variables {
  alarm_actions              = []
  ami_id                     = "ami-0123456789abcdef0"
  availability_zones         = ["us-east-1a", "us-east-1b"]
  certificate_arn            = "arn:aws:acm:us-east-1:123456789012:certificate/11111111-1111-4111-8111-111111111111"
  instance_type              = "c7i.large"
  name                       = "pbs-example-us-east-1"
  tags                       = { ManagedBy = "Terraform" }
  trusted_server_cidr_blocks = ["192.0.2.0/24", "198.51.100.0/24"]
  vpc_cidr                   = "10.80.0.0/16"
}

run "plans_private_hosts_and_scoped_ingress" {
  command = plan

  assert {
    condition     = length(aws_instance.pbs) == 2
    error_message = "The regional module should plan one PBS host in each of two AZs."
  }

  assert {
    condition     = alltrue([for instance in aws_instance.pbs : instance.associate_public_ip_address == false])
    error_message = "PBS hosts should not receive public IP addresses."
  }

  assert {
    condition     = length(aws_security_group.alb.ingress) == length(var.trusted_server_cidr_blocks)
    error_message = "The ALB should have one ingress rule for each approved caller CIDR."
  }

  assert {
    condition = alltrue([
      for rule in aws_security_group.alb.ingress :
      length(rule.cidr_blocks) == 1 && contains(var.trusted_server_cidr_blocks, one(rule.cidr_blocks))
    ])
    error_message = "Every ALB ingress rule should use an approved caller CIDR."
  }

  assert {
    condition     = length(aws_secretsmanager_secret.bidder) == 1
    error_message = "The example should create metadata for only the declared example bidder secret."
  }

  assert {
    condition     = length(jsondecode(aws_iam_role_policy.runtime.policy).Statement[0].Resource) == length(aws_secretsmanager_secret.bidder)
    error_message = "The runtime role should read only the declared bidder secrets."
  }
}

run "scopes_optional_kms_decryption" {
  command = plan

  variables {
    secrets_kms_key_arn = "arn:aws:kms:us-east-1:123456789012:key/11111111-1111-4111-8111-111111111111"
  }

  assert {
    condition     = jsondecode(aws_iam_role_policy.runtime.policy).Statement[1].Resource == var.secrets_kms_key_arn
    error_message = "KMS decryption should be scoped to the selected regional key."
  }
}
