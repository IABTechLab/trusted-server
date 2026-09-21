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

override_resource {
  target          = aws_subnet.public["us-east-1a"]
  override_during = plan
  values = {
    id = "subnet-00000000000000001"
  }
}

override_resource {
  target          = aws_subnet.public["us-east-1b"]
  override_during = plan
  values = {
    id = "subnet-00000000000000002"
  }
}

override_resource {
  target          = aws_subnet.private["us-east-1a"]
  override_during = plan
  values = {
    id = "subnet-00000000000000003"
  }
}

override_resource {
  target          = aws_subnet.private["us-east-1b"]
  override_during = plan
  values = {
    id = "subnet-00000000000000004"
  }
}

override_resource {
  target          = aws_security_group.alb
  override_during = plan
  values = {
    id = "sg-00000000000000001"
  }
}

override_resource {
  target          = aws_security_group.pbs
  override_during = plan
  values = {
    id = "sg-00000000000000002"
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

  # Explicit instance paths keep Terraform 1.16.2 failure diagnostics from serializing
  # the whole instance map, which contains provider-marked sensitive attributes.
  assert {
    condition = alltrue([
      aws_instance.pbs["us-east-1a"].metadata_options[0].http_tokens == "required",
      aws_instance.pbs["us-east-1b"].metadata_options[0].http_tokens == "required",
    ])
    error_message = "PBS hosts should require IMDSv2 session tokens."
  }

  assert {
    condition = alltrue([
      aws_instance.pbs["us-east-1a"].root_block_device[0].encrypted,
      aws_instance.pbs["us-east-1b"].root_block_device[0].encrypted,
    ])
    error_message = "PBS host root volumes should be encrypted."
  }

  assert {
    condition = alltrue([
      aws_instance.pbs["us-east-1a"].subnet_id == aws_subnet.private["us-east-1a"].id,
      aws_instance.pbs["us-east-1b"].subnet_id == aws_subnet.private["us-east-1b"].id,
    ])
    error_message = "PBS hosts should launch in the private subnet of their own AZ."
  }

  assert {
    condition     = aws_lb_listener.https.ssl_policy == "ELBSecurityPolicy-TLS13-1-2-2021-06"
    error_message = "The ALB listener should pin the TLS 1.3/1.2 policy."
  }

  assert {
    condition = (
      aws_vpc_security_group_egress_rule.alb_to_pbs.security_group_id == aws_security_group.alb.id &&
      aws_vpc_security_group_egress_rule.alb_to_pbs.referenced_security_group_id == aws_security_group.pbs.id &&
      aws_vpc_security_group_egress_rule.alb_to_pbs.ip_protocol == "tcp" &&
      aws_vpc_security_group_egress_rule.alb_to_pbs.from_port == var.pbs_port &&
      aws_vpc_security_group_egress_rule.alb_to_pbs.to_port == var.pbs_port &&
      aws_vpc_security_group_egress_rule.alb_to_pbs.cidr_ipv4 == null &&
      aws_vpc_security_group_egress_rule.alb_to_pbs.cidr_ipv6 == null &&
      aws_vpc_security_group_egress_rule.alb_to_pbs.prefix_list_id == null
    )
    error_message = "ALB outbound traffic should reach only the PBS security group on the PBS TCP port."
  }

  assert {
    condition     = length(aws_vpc_security_group_ingress_rule.trusted_server_https) == length(var.trusted_server_cidr_blocks)
    error_message = "The ALB should have one ingress rule for each approved caller CIDR."
  }

  assert {
    condition = alltrue([
      for cidr, rule in aws_vpc_security_group_ingress_rule.trusted_server_https :
      rule.security_group_id == aws_security_group.alb.id &&
      rule.cidr_ipv4 == cidr && contains(var.trusted_server_cidr_blocks, cidr) &&
      rule.ip_protocol == "tcp" && rule.from_port == 443 && rule.to_port == 443
    ])
    error_message = "Every ALB ingress rule should permit only HTTPS from its approved caller CIDR."
  }

  assert {
    condition     = length(aws_secretsmanager_secret.bidder) == 1
    error_message = "The example should create metadata for only the declared example bidder secret."
  }

  assert {
    condition = alltrue([
      for alarm in aws_cloudwatch_metric_alarm.instance_cpu :
      alarm.treat_missing_data == "missing"
    ])
    error_message = "Missing CPU samples should not trigger high-CPU alarms."
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
