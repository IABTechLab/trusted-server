output "east_alb_dns_name" {
  description = "DNS name of the us-east-1 regional ALB."
  value       = module.east.alb_dns_name
}

output "east_instance_ids" {
  description = "PBS EC2 instance IDs in us-east-1."
  value       = module.east.instance_ids
}

output "east_nat_eip_addresses" {
  description = "Stable us-east-1 NAT egress addresses for bidder allowlists."
  value       = module.east.nat_eip_addresses
}

output "east_secret_arns" {
  description = "Bidder Secrets Manager ARNs in us-east-1. Values are never managed by Terraform."
  value       = module.east.secret_arns
}

output "deployment_descriptor_yaml" {
  description = "Rendered nonsecret CLI descriptor. Write it to the ignored deployment.generated.yaml after apply."
  value       = yamlencode(local.deployment_descriptor)
}

output "secret_bindings_json" {
  description = "Rendered nonsecret binding metadata. Write it to the ignored runtime/secret-bindings.generated.json after apply."
  value       = jsonencode(local.secret_bindings)
}

output "west_alb_dns_name" {
  description = "DNS name of the us-west-2 regional ALB."
  value       = module.west.alb_dns_name
}

output "west_instance_ids" {
  description = "PBS EC2 instance IDs in us-west-2."
  value       = module.west.instance_ids
}

output "west_nat_eip_addresses" {
  description = "Stable us-west-2 NAT egress addresses for bidder allowlists."
  value       = module.west.nat_eip_addresses
}

output "west_secret_arns" {
  description = "Bidder Secrets Manager ARNs in us-west-2. Values are never managed by Terraform."
  value       = module.west.secret_arns
}
