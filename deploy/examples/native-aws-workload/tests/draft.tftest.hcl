mock_provider "aws" {
  override_during = plan

  mock_resource "aws_appconfig_application" {
    override_during = plan
    defaults        = { id = "app1234" }
  }
  mock_resource "aws_appconfig_environment" {
    override_during = plan
    defaults        = { environment_id = "env1234" }
  }
  mock_resource "aws_appconfig_configuration_profile" {
    override_during = plan
    defaults        = { configuration_profile_id = "cfg1234" }
  }
  mock_resource "aws_appconfig_hosted_configuration_version" {
    override_during = plan
    defaults        = { version_number = 1 }
  }
  mock_resource "aws_appconfig_deployment" {
    override_during = plan
    defaults        = { deployment_number = 1 }
  }
  mock_resource "aws_cloudwatch_log_group" {
    override_during = plan
    defaults        = { arn = "arn:aws:logs:eu-west-2:000000000000:log-group:/trusted-server/review/review-example" }
  }
}

mock_provider "aws" {
  alias           = "us_east_1"
  override_during = plan
}

mock_provider "random" {
  override_during = plan
  mock_resource "random_password" {
    defaults = { result = "FictionalCredential000000000000000000000000" }
  }
}

variables {
  run_id               = "review-example"
  owner                = "review-owner"
  aws_account_id       = "000000000000"
  aws_region           = "eu-west-2"
  availability_zones   = ["eu-west-2a", "eu-west-2b"]
  demo_hostname        = "ts-demo.example.com"
  publisher_hostname   = "publisher.example.com"
  route53_zone_id      = "Z0000000000000"
  certificate_arn      = "arn:aws:acm:eu-west-2:000000000000:certificate/00000000-0000-0000-0000-000000000000"
  reviewer_ipv4_cidrs  = ["203.0.113.0/24"]
  application_image    = "000000000000.dkr.ecr.eu-west-2.amazonaws.com/review-app@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
  agent_image          = "public.ecr.aws/example/agent@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
  config_envelope_file = "example-envelope.json"
  geo_front_door       = null
  ec_storage = {
    provider  = "dynamodb"
    region    = "eu-west-2"
    table     = "example-retained-ec"
    namespace = "example-staging"
  }
  application_secrets = {
    handler_password = {
      arn        = "arn:aws:secretsmanager:eu-west-2:000000000000:secret:review-handler-AbCdEf"
      version_id = "11111111111111111111111111111111"
    }
    publisher_proxy_secret = {
      arn        = "arn:aws:secretsmanager:eu-west-2:000000000000:secret:review-proxy-AbCdEf"
      version_id = "22222222222222222222222222222222"
    }
    ec_passphrase = {
      arn        = "arn:aws:secretsmanager:eu-west-2:000000000000:secret:review-ec-AbCdEf"
      version_id = "33333333333333333333333333333333"
    }
    browser_forwarder_secret = {
      arn        = "arn:aws:secretsmanager:eu-west-2:000000000000:secret:review-forwarder-AbCdEf"
      version_id = "55555555555555555555555555555555"
    }
  }
  runtime_boundaries = {
    execution = "arn:aws:iam::000000000000:policy/native-demo/review-example/execution-boundary"
    task      = "arn:aws:iam::000000000000:policy/native-demo/review-example/task-boundary"
  }
  agent_token = {
    arn        = "arn:aws:secretsmanager:eu-west-2:000000000000:secret:review-agent-AbCdEf"
    version_id = "44444444444444444444444444444444"
  }
}

run "valid_source_plans_without_qualification_gate" {
  command = plan
  assert {
    condition     = local.envelope_valid
    error_message = "The CLI envelope fixture must have the supported structure and selected publisher binding."
  }
}

run "public_https_plans_with_valid_envelope" {
  command = plan
  variables {
    reviewer_ipv4_cidrs = ["0.0.0.0/0"]
  }

  assert {
    condition     = local.envelope_valid
    error_message = "Public ingress must retain the envelope contract."
  }
}

run "reject_empty_direct_ingress" {
  command = plan
  variables {
    reviewer_ipv4_cidrs = []
  }
  expect_failures = [var.reviewer_ipv4_cidrs]
}

run "shared_permission_boundary" {
  command = plan
  variables {
    runtime_boundaries = {
      execution = "arn:aws:iam::000000000000:policy/example-boundary"
      task      = "arn:aws:iam::000000000000:policy/example-boundary"
    }
  }
  assert {
    condition     = aws_iam_role.execution.permissions_boundary == var.runtime_boundaries.execution && aws_iam_role.task.permissions_boundary == var.runtime_boundaries.execution
    error_message = "Execution and task roles may share a boundary while retaining separate inline permissions."
  }
}

run "broad_reviewer_cidr" {
  command = plan
  variables {
    reviewer_ipv4_cidrs = ["10.0.0.0/8"]
  }
  assert {
    condition     = aws_vpc_security_group_ingress_rule.reviewers["10.0.0.0/8"].cidr_ipv4 == "10.0.0.0/8"
    error_message = "A valid broad IPv4 range must not be rejected by an invented prefix-width policy."
  }
}

run "reviewer_cidrs_without_invented_count_limit" {
  command = plan
  variables {
    reviewer_ipv4_cidrs = [for index in range(33) : "10.${index}.0.0/16"]
  }
  assert {
    condition     = length(aws_vpc_security_group_ingress_rule.reviewers) == 33
    error_message = "Actual AWS quotas, not a hardcoded example count limit, govern ingress rule capacity."
  }
}

run "reject_invalid_reviewer_ipv4" {
  command = plan
  variables {
    reviewer_ipv4_cidrs = ["not-a-cidr"]
  }
  expect_failures = [var.reviewer_ipv4_cidrs]
}

run "reject_tag_only_image" {
  command = plan
  variables {
    application_image = "public.ecr.aws/example/app:latest"
  }
  expect_failures = [var.application_image]
}
