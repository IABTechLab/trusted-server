mock_provider "aws" {
  override_during = plan

  mock_resource "aws_appconfig_application" {
    defaults = { id = "app1234" }
  }
  mock_resource "aws_appconfig_environment" {
    defaults = { environment_id = "env1234" }
  }
  mock_resource "aws_appconfig_configuration_profile" {
    defaults = { configuration_profile_id = "cfg1234" }
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

run "draft_cannot_apply_workload" {
  command         = plan
  expect_failures = [terraform_data.draft_gate]

  assert {
    condition     = length(local.deployment_blockers) == 3 && contains(local.deployment_blockers, "scoped-operator-and-locked-state-qualification")
    error_message = "The source-controlled qualification blockers must not become an operator tfvars override."
  }
  assert {
    condition     = local.envelope_valid
    error_message = "The CLI envelope fixture must have the supported structure and selected publisher binding."
  }
}

run "public_https_retains_qualification_gate" {
  command = plan
  variables {
    reviewer_ipv4_cidrs = ["0.0.0.0/0"]
  }
  expect_failures = [terraform_data.draft_gate]

  assert {
    condition     = local.envelope_valid
    error_message = "Public ingress does not bypass the envelope or qualification boundary."
  }
}

run "reject_other_broad_ingress_ranges" {
  command = plan
  variables {
    reviewer_ipv4_cidrs = ["10.0.0.0/8"]
  }
  expect_failures = [var.reviewer_ipv4_cidrs, terraform_data.draft_gate]
}

run "reject_tag_only_image" {
  command = plan
  variables {
    application_image = "public.ecr.aws/example/app:latest"
  }
  expect_failures = [var.application_image, terraform_data.draft_gate]
}
