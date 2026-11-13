resource "aws_appconfig_application" "review" {
  name        = var.run_id
  description = "Disposable reviewed TS config; no inline secret values"
  depends_on  = [terraform_data.draft_gate]
}

resource "aws_appconfig_environment" "review" {
  application_id = aws_appconfig_application.review.id
  name           = var.run_id
  depends_on     = [terraform_data.draft_gate]
}

resource "aws_appconfig_configuration_profile" "review" {
  application_id = aws_appconfig_application.review.id
  name           = "${var.run_id}-release"
  location_uri   = "hosted"
  type           = "AWS.Freeform"
  depends_on     = [terraform_data.draft_gate]
}

resource "aws_appconfig_hosted_configuration_version" "review" {
  application_id           = aws_appconfig_application.review.id
  configuration_profile_id = aws_appconfig_configuration_profile.review.configuration_profile_id
  content_type             = "application/json"
  content                  = local.envelope_text
  description              = "Reviewed CLI envelope; transport sha256 ${sha256(local.envelope_text)}"
  depends_on               = [terraform_data.draft_gate]

  lifecycle {
    create_before_destroy = true
  }
}

resource "aws_appconfig_deployment_strategy" "review" {
  name                           = "${var.run_id}-initial"
  deployment_duration_in_minutes = 0
  final_bake_time_in_minutes     = 0
  growth_factor                  = 100
  growth_type                    = "LINEAR"
  replicate_to                   = "NONE"
  depends_on                     = [terraform_data.draft_gate]
}

resource "aws_appconfig_deployment" "review" {
  application_id           = aws_appconfig_application.review.id
  environment_id           = aws_appconfig_environment.review.environment_id
  configuration_profile_id = aws_appconfig_configuration_profile.review.configuration_profile_id
  configuration_version    = aws_appconfig_hosted_configuration_version.review.version_number
  deployment_strategy_id   = aws_appconfig_deployment_strategy.review.id
  description              = "Initial immutable review release for ${var.run_id}"
  depends_on               = [terraform_data.draft_gate]
}

resource "aws_cloudwatch_log_group" "review" {
  name              = "/trusted-server/review/${var.run_id}"
  retention_in_days = var.log_retention_in_days
  skip_destroy      = var.retain_logs_on_destroy
  depends_on        = [terraform_data.draft_gate]
}
