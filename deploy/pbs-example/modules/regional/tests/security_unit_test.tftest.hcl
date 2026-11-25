mock_provider "aws" {
  mock_data "aws_partition" {
    defaults = { partition = "aws" }
  }

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

  mock_resource "aws_lb" {
    defaults = {
      arn = "arn:aws:elasticloadbalancing:us-east-1:123456789012:loadbalancer/app/pbs-example/1111111111111111"
    }
  }

  mock_resource "aws_lb_target_group" {
    defaults = {
      arn = "arn:aws:elasticloadbalancing:us-east-1:123456789012:targetgroup/pbs-example/2222222222222222"
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

  # Keep plan-time overrides local to this run. The apply runs let the
  # mock fill in computed values during apply instead of re-generating them.
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

  override_resource {
    target          = aws_secretsmanager_secret.bidder["examplebidder"]
    override_during = plan
    values = {
      arn = "arn:aws:secretsmanager:us-east-1:123456789012:secret:example-AbCdEf"
    }
  }

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
    condition = (
      length(aws_lb_trust_store.clients) == 0 &&
      aws_lb_listener.https.mutual_authentication[0].mode == "off" &&
      aws_lb_listener.https.mutual_authentication[0].trust_store_arn == null
    )
    error_message = "With no CA bundle, the listener should set mTLS off and have no trust store."
  }

  assert {
    condition     = !aws_lb.main.connection_logs[0].enabled
    error_message = "With no connection log bucket, the ALB should set connection logging off."
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
      aws_vpc_security_group_egress_rule.alb_to_pbs.from_port == 8000 &&
      aws_vpc_security_group_egress_rule.alb_to_pbs.to_port == 8000 &&
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
    condition = (
      aws_lb_target_group.pbs.port == 8000 &&
      alltrue([for attachment in aws_lb_target_group_attachment.pbs : attachment.port == 8000]) &&
      alltrue([for rule in aws_security_group.pbs.ingress : rule.from_port == 8000 && rule.to_port == 8000])
    )
    error_message = "ALB targets and PBS ingress must match the runtime's fixed port 8000."
  }

  assert {
    condition     = aws_iam_role_policy_attachment.ssm.policy_arn == "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"
    error_message = "Commercial-region hosts should attach the commercial SSM managed policy."
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

run "rejects_overlong_resource_names" {
  command = plan

  variables {
    name = "abcdefghijklmnopqrstuvwxyzabc"
  }

  expect_failures = [var.name]
}

run "rejects_invalid_resource_names" {
  command = plan

  variables {
    name = "-pbs-example"
  }

  expect_failures = [var.name]
}

run "rejects_reserved_alb_prefix" {
  command = plan

  variables {
    name = "internal-pbs"
  }

  expect_failures = [var.name]
}

run "accepts_maximum_resource_name" {
  command = plan

  variables {
    name = "abcdefghijklmnopqrstuvwxyzab"
  }

  assert {
    condition     = aws_lb.main.name == "${var.name}-alb" && aws_lb_target_group.pbs.name == "${var.name}-pbs"
    error_message = "Names should preserve the full prefix and resource suffix."
  }
}

run "accepts_single_character_resource_name" {
  command = plan

  variables {
    name = "a"
  }

  assert {
    condition     = aws_lb.main.name == "a-alb" && aws_lb_target_group.pbs.name == "a-pbs"
    error_message = "A single alphanumeric character is a valid name prefix."
  }
}

run "plans_connection_logs_without_client_verification" {
  command = plan

  variables {
    connection_logs_bucket = "pbs-example-east-connection-logs"
  }

  assert {
    condition = (
      aws_lb.main.connection_logs[0].enabled &&
      aws_lb.main.connection_logs[0].bucket == var.connection_logs_bucket &&
      aws_lb_listener.https.mutual_authentication[0].mode == "off"
    )
    error_message = "The ALB should support connection logging without client certificate verification."
  }
}

run "enables_client_verification_and_connection_logs" {
  command = apply

  variables {
    # The collapsed prefix has a hyphen at the truncation boundary.
    name = "a--bcdefghijkl--mnopqrstuvwx"
    client_ca_bundle = {
      bucket = "pbs-example-east-trust"
      key    = "client-ca/bundle-v1.pem"
    }
    connection_logs_bucket = "pbs-example-east-connection-logs"
  }

  override_resource {
    target = aws_lb_trust_store.clients[0]
    values = {
      arn = "arn:aws:elasticloadbalancing:us-east-1:123456789012:truststore/pbs-example/1111111111111111"
    }
  }

  assert {
    condition = (
      length(aws_lb_trust_store.clients) == 1 &&
      aws_lb_trust_store.clients[0].ca_certificates_bundle_s3_bucket == var.client_ca_bundle.bucket &&
      aws_lb_trust_store.clients[0].ca_certificates_bundle_s3_key == var.client_ca_bundle.key
    )
    error_message = "The regional trust store should use the configured S3 CA bundle."
  }

  assert {
    condition = (
      length(var.name) == 28 &&
      length(aws_lb_trust_store.clients[0].name) <= 32 &&
      can(regex("^[A-Za-z0-9]+(-[A-Za-z0-9]+)*$", aws_lb_trust_store.clients[0].name))
    )
    error_message = "The trust store name should meet AWS rules even for a 28-character prefix with consecutive hyphens."
  }

  assert {
    condition = (
      aws_lb_listener.https.mutual_authentication[0].mode == "verify" &&
      aws_lb_listener.https.mutual_authentication[0].trust_store_arn == aws_lb_trust_store.clients[0].arn &&
      aws_lb_listener.https.certificate_arn == var.certificate_arn &&
      aws_lb_target_group.pbs.health_check[0].protocol == "HTTP" &&
      aws_lb_target_group.pbs.health_check[0].path == "/status"
    )
    error_message = "Verify mode should attach the trust store without changing the ACM certificate or target health check."
  }

  assert {
    condition = (
      aws_lb.main.connection_logs[0].enabled &&
      aws_lb.main.connection_logs[0].bucket == var.connection_logs_bucket
    )
    error_message = "The ALB should write connection logs to the configured bucket."
  }
}

run "disables_client_verification_and_connection_logs" {
  command = apply

  variables {
    name = "a--bcdefghijkl--mnopqrstuvwx"
  }

  assert {
    condition = (
      length(aws_lb_trust_store.clients) == 0 &&
      aws_lb_listener.https.mutual_authentication[0].mode == "off" &&
      aws_lb_listener.https.mutual_authentication[0].trust_store_arn == null
    )
    error_message = "Removing the CA bundle inputs should disable listener mTLS and remove the trust store."
  }

  assert {
    condition     = !aws_lb.main.connection_logs[0].enabled
    error_message = "Removing the connection log bucket should disable logging."
  }
}

run "uses_current_partition_for_ssm_policy" {
  command = plan

  override_data {
    target = data.aws_partition.current
    values = { partition = "aws-us-gov" }
  }

  assert {
    condition     = aws_iam_role_policy_attachment.ssm.policy_arn == "arn:aws-us-gov:iam::aws:policy/AmazonSSMManagedInstanceCore"
    error_message = "The SSM managed policy ARN should follow the provider's AWS partition."
  }
}

run "scopes_optional_kms_decryption" {
  command = plan

  override_resource {
    target          = aws_secretsmanager_secret.bidder["examplebidder"]
    override_during = plan
    values = {
      arn = "arn:aws:secretsmanager:us-east-1:123456789012:secret:example-AbCdEf"
    }
  }

  variables {
    secrets_kms_key_arn = "arn:aws:kms:us-east-1:123456789012:key/11111111-1111-4111-8111-111111111111"
  }

  assert {
    condition     = jsondecode(aws_iam_role_policy.runtime.policy).Statement[1].Resource == var.secrets_kms_key_arn
    error_message = "KMS decryption should be scoped to the selected regional key."
  }
}
