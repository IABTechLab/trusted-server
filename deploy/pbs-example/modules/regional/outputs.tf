output "alb_dns_name" {
  description = "Regional ALB DNS name."
  value       = aws_lb.main.dns_name
}

output "alb_zone_id" {
  description = "Route 53 hosted-zone ID for the regional ALB alias."
  value       = aws_lb.main.zone_id
}

output "instance_ids" {
  description = "PBS EC2 instance IDs keyed by Availability Zone."
  value       = { for availability_zone, instance in aws_instance.pbs : availability_zone => instance.id }
}

output "private_subnet_ids" {
  description = "Private subnet IDs keyed by Availability Zone."
  value       = { for availability_zone, subnet in aws_subnet.private : availability_zone => subnet.id }
}

output "secret_arns" {
  description = "Bidder Secrets Manager ARNs keyed by bidder identifier."
  value       = { for bidder, secret in aws_secretsmanager_secret.bidder : bidder => secret.arn }
}

output "vpc_id" {
  description = "Regional VPC ID."
  value       = aws_vpc.main.id
}
