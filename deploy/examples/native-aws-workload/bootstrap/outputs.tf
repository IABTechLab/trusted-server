output "backend_configs" {
  description = "Explicit S3 backend configuration for three separate stable state keys. Bootstrap starts local."
  value = {
    for root, key in local.backend_keys : root => {
      bucket       = aws_s3_bucket.state.id
      key          = key
      region       = var.aws_region
      encrypt      = true
      use_lockfile = true
    }
  }
}

output "ecr_repository_url" {
  description = "Push the final runtime wrapper here, then pass its immutable digest to workload."
  value       = aws_ecr_repository.application.repository_url
}

output "ecr_repository_name" {
  value = aws_ecr_repository.application.name
}

output "route53_zone_id" {
  value = local.zone_id
}

output "route53_name_servers" {
  description = "For a new evaluation zone, publish these NS values at demo_hostname in its external parent zone before waiting for ACM."
  value       = var.existing_route53_zone_id == null ? aws_route53_zone.demo[0].name_servers : data.aws_route53_zone.existing[0].name_servers
}

output "regional_certificate_arn" {
  description = "Regional ALB certificate request. Workload Terraform waits for issuance after public DNS delegation."
  value       = aws_acm_certificate.regional.arn
}

output "viewer_certificate_arn" {
  description = "us-east-1 CloudFront certificate request. Shares the regional request's single DNS validation CNAME."
  value       = aws_acm_certificate.viewer.arn
}

output "application_secret_arns" {
  description = "Four empty application secret containers. Seed outside Terraform and supply immutable version IDs to workload."
  value       = { for key, secret in aws_secretsmanager_secret.application : key => secret.arn }
}

output "agent_secret_arn" {
  description = "Separate empty AppConfig Agent token container. Its value never enters app config or Terraform."
  value       = aws_secretsmanager_secret.agent.arn
}
