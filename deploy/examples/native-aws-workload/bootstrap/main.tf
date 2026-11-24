locals {
  tags = {
    Application  = "trusted-server"
    Installation = var.installation_name
    Owner        = var.owner
    Lifecycle    = "bootstrap"
  }
  zone_id = var.existing_route53_zone_id == null ? aws_route53_zone.demo[0].zone_id : data.aws_route53_zone.existing[0].zone_id
  application_secret_keys = toset([
    "handler_password",
    "publisher_proxy_secret",
    "ec_passphrase",
    "browser_forwarder_secret",
  ])
  backend_keys = {
    bootstrap = "${var.installation_name}/bootstrap.tfstate"
    retained  = "${var.installation_name}/retained.tfstate"
    workload  = "${var.installation_name}/workload.tfstate"
  }
}

resource "aws_s3_bucket" "state" {
  bucket        = var.state_bucket_name
  force_destroy = false
}

resource "aws_s3_bucket_versioning" "state" {
  bucket = aws_s3_bucket.state.id
  versioning_configuration {
    status = "Enabled"
  }
}

resource "aws_s3_bucket_server_side_encryption_configuration" "state" {
  bucket = aws_s3_bucket.state.id
  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm = "AES256"
    }
  }
}

resource "aws_s3_bucket_public_access_block" "state" {
  bucket                  = aws_s3_bucket.state.id
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_ownership_controls" "state" {
  bucket = aws_s3_bucket.state.id
  rule {
    object_ownership = "BucketOwnerEnforced"
  }
}

resource "aws_s3_bucket_policy" "state" {
  bucket = aws_s3_bucket.state.id
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Sid       = "DenyInsecureTransport"
      Effect    = "Deny"
      Principal = "*"
      Action    = "s3:*"
      Resource  = [aws_s3_bucket.state.arn, "${aws_s3_bucket.state.arn}/*"]
      Condition = {
        Bool = { "aws:SecureTransport" = "false" }
      }
    }]
  })
}

resource "aws_ecr_repository" "application" {
  name                 = "${var.installation_name}/application"
  image_tag_mutability = "IMMUTABLE"
  force_delete         = false

  encryption_configuration {
    encryption_type = "AES256"
  }
  image_scanning_configuration {
    scan_on_push = true
  }
}

resource "aws_route53_zone" "demo" {
  count   = var.existing_route53_zone_id == null ? 1 : 0
  name    = var.demo_hostname
  comment = "${var.installation_name} evaluation only; parent delegation is external"
}

data "aws_route53_zone" "existing" {
  count   = var.existing_route53_zone_id == null ? 0 : 1
  zone_id = var.existing_route53_zone_id
}

resource "aws_acm_certificate" "regional" {
  domain_name       = var.demo_hostname
  validation_method = "DNS"

  lifecycle {
    create_before_destroy = true
  }
}

resource "aws_acm_certificate" "viewer" {
  provider          = aws.us_east_1
  domain_name       = var.demo_hostname
  validation_method = "DNS"

  lifecycle {
    create_before_destroy = true
  }
}

# ACM reuses a hostname's DNS token across regions in the same AWS account.
# Only this record owns the shared CNAME. Do not create a second viewer record.
# Do not wait for issuance here: a new zone must first output its nameservers
# so the operator can delegate it from the externally owned parent zone.
resource "aws_route53_record" "certificate_validation" {
  zone_id = local.zone_id
  name    = one(aws_acm_certificate.regional.domain_validation_options).resource_record_name
  type    = one(aws_acm_certificate.regional.domain_validation_options).resource_record_type
  ttl     = 300
  records = [one(aws_acm_certificate.regional.domain_validation_options).resource_record_value]
}

# Terraform owns containers, never secret values or versions. Seed values with
# Secrets Manager outside Terraform, then hand immutable version IDs to workload.
resource "aws_secretsmanager_secret" "application" {
  for_each    = local.application_secret_keys
  name        = "${var.installation_name}/${each.key}"
  description = "${var.installation_name} application config reference ${each.key}"
}

resource "aws_secretsmanager_secret" "agent" {
  name        = "${var.installation_name}/agent_token"
  description = "${var.installation_name} AppConfig Agent bearer token, separate from application config"
}
