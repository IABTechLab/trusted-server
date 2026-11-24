# Policy identity remains plan-known even when AppConfig makes task JSON unknown.
# This never projects the private credential value.
output "deployment_policy" {
  sensitive = true
  value = {
    run_id                        = var.run_id
    aws_account_id                = var.aws_account_id
    aws_region                    = var.aws_region
    geo_enabled                   = local.geo_enabled
    origin_credential_header_name = local.geo_enabled ? local.geo_credential_header_name : null
  }
}

output "ec_task_policy_json" {
  description = "Plan-known scoped DynamoDB allow document for the separately owned task permissions boundary. Explicit denies in boundaries, SCPs or endpoint policies still apply."
  value       = jsonencode(local.ec_task_policy)
}

output "deployment_metadata" {
  description = "Deployment identity and retained-resource lifetimes, not an assertion of runtime qualification."
  value = {
    run_id                              = var.run_id
    geo                                 = local.geo_enabled ? { phase = local.geo_phase } : null
    manual_shutdown_required            = true
    config_envelope_transport_sha256    = sha256(local.envelope_text)
    external_inputs_retained_on_destroy = ["Retained DynamoDB table and allocator rows", "Supplied Secrets Manager secret versions", "ACM certificates", "Route53 hosted zone", "Published images", "Separately bootstrapped remote state/operator authority"]
  }
}

output "planned_review_access" {
  description = "Planned address, not evidence of a deployed or authenticated endpoint."
  value = {
    tls_endpoint   = "https://${var.demo_hostname}"
    publisher_host = var.publisher_hostname
    proxy_mapping  = "${var.publisher_hostname}=${var.demo_hostname}"
  }
}
