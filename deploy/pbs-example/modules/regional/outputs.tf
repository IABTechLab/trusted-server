output "alb_dns_name" {
  description = "Regional ALB DNS name."
  value       = aws_lb.main.dns_name
}

output "alb_tls_settings" {
  description = "Client certificate verification mode, CA bundle location, and connection log bucket configured on the regional ALB."
  value = {
    mode = aws_lb_listener.https.mutual_authentication[0].mode
    client_ca_bundle = one([
      for store in aws_lb_trust_store.clients : {
        bucket = store.ca_certificates_bundle_s3_bucket
        key    = store.ca_certificates_bundle_s3_key
      }
    ])
    connection_logs_bucket = aws_lb.main.connection_logs[0].enabled ? aws_lb.main.connection_logs[0].bucket : null
  }
}

output "alb_zone_id" {
  description = "Route 53 hosted-zone ID for the regional ALB alias."
  value       = aws_lb.main.zone_id
}

output "instance_ids" {
  description = "PBS EC2 instance IDs keyed by Availability Zone."
  value       = { for availability_zone, instance in aws_instance.pbs : availability_zone => instance.id }
}

output "nat_eip_addresses" {
  description = "Stable NAT gateway egress addresses keyed by Availability Zone for bidder allowlists."
  value       = { for availability_zone, eip in aws_eip.nat : availability_zone => eip.public_ip }
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
