output "east_alb_dns_name" {
  description = "DNS name of the us-east-1 regional ALB."
  value       = module.east.alb_dns_name
}

output "east_instance_ids" {
  description = "PBS EC2 instance IDs in us-east-1."
  value       = module.east.instance_ids
}

output "east_secret_arns" {
  description = "Bidder Secrets Manager ARNs in us-east-1. Values are never managed by Terraform."
  value       = module.east.secret_arns
}

output "west_alb_dns_name" {
  description = "DNS name of the us-west-2 regional ALB."
  value       = module.west.alb_dns_name
}

output "west_instance_ids" {
  description = "PBS EC2 instance IDs in us-west-2."
  value       = module.west.instance_ids
}

output "west_secret_arns" {
  description = "Bidder Secrets Manager ARNs in us-west-2. Values are never managed by Terraform."
  value       = module.west.secret_arns
}
