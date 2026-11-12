locals {
  # These missing qualifications cannot be removed through tfvars. Every AWS
  # resource depends on draft_gate. Remove a blocker only with reviewed proof.
  deployment_blockers = toset([
    "scoped-operator-and-locked-state-qualification",
    "provider-enabled-final-image-and-nonroot-volume-runtime",
    "alb-forwarding-host-scheme-auth-and-health-acceptance"
  ])

  tags = merge(var.extra_tags, {
    Application = "trusted-server"
    Purpose     = "disposable-review"
    RunId       = var.run_id
    Owner       = var.owner
    Lifecycle   = "manual"
  })

  # Both ingress modes use these settings without sharing resource identities.
  service_settings = {
    desired_count                      = 1
    health_check_grace_period_seconds  = var.ecs_health_grace_seconds
    deployment_minimum_healthy_percent = 0
    deployment_maximum_percent         = 100
  }
  target_health = {
    enabled             = true
    path                = "/health"
    protocol            = "HTTP"
    matcher             = "200"
    interval            = var.health_interval_seconds
    timeout             = var.health_timeout_seconds
    healthy_threshold   = var.healthy_threshold
    unhealthy_threshold = var.unhealthy_threshold
  }

  envelope_text = file(var.config_envelope_file)
  envelope      = try(jsondecode(local.envelope_text), null)
  app_config    = try(local.envelope.data, {})
  # Terraform checks publication shape, website and retained storage bindings.
  # The matching TS CLI/runtime owns authentication, signing, origin protocols,
  # secret-consumer semantics and the application checksum.
  envelope_valid = try(
    local.envelope.version == 1 &&
    can(regex("^[0-9a-f]{64}$", local.envelope.sha256)) &&
    local.app_config.publisher.domain == var.publisher_hostname &&
    jsonencode(local.app_config.ec.storage) == jsonencode(var.ec_storage),
    false
  )

  alb_subnet_cidrs = [cidrsubnet(var.vpc_cidr, 8, 0), cidrsubnet(var.vpc_cidr, 8, 1)]
  task_subnet_cidr = cidrsubnet(var.vpc_cidr, 8, 10)
  token_value_from = "${var.agent_token.arn}:::${var.agent_token.version_id}"

  # jsonencode supplies quoted/escaped strings valid in this restricted TOML
  # representation. Only identifiers/mappings belong here, never secret values.
  native_bindings = join("\n", concat([
    "version = 1",
    "",
    "[config.trusted_server_config]",
    "provider = \"aws-appconfig-agent\"",
    "default_key = \"trusted_server_config\"",
    "endpoint = \"http://127.0.0.1:2772\"",
    "access_token_env = \"TS_APPCONFIG_TOKEN\"",
    "",
    "[config.trusted_server_config.documents.trusted_server_config]",
    "application = ${jsonencode(aws_appconfig_application.review.id)}",
    "environment = ${jsonencode(aws_appconfig_environment.review.environment_id)}",
    "profile = ${jsonencode(aws_appconfig_configuration_profile.review.configuration_profile_id)}",
    "",
    "[secrets.trusted_server_secrets]",
    "provider = \"aws-secrets-manager\"",
    "region = ${jsonencode(var.aws_region)}",
    ""
    ], flatten([for key, ref in var.application_secrets : [
      "[secrets.trusted_server_secrets.entries.${jsonencode(key)}]",
      "secret_id = ${jsonencode(ref.arn)}",
      "version_id = ${jsonencode(ref.version_id)}",
      ""
  ]])))
}
