locals {
  # Only presence is public. The credential name/value remain sensitive.
  geo_enabled                = nonsensitive(var.geo_front_door != null)
  geo_phase                  = local.geo_enabled ? nonsensitive(var.geo_front_door.rollout_phase) : null
  geo_credential_header_name = local.geo_enabled ? var.geo_front_door.origin_credential_header_name : null
  geo_credential_value       = local.geo_enabled ? random_password.origin_credential[0].result : null
  cloudfront_ipv4_cidrs      = local.geo_enabled ? sort([for entry in data.aws_ec2_managed_prefix_list.cloudfront[0].entries : entry.cidr]) : []
  trusted_proxy_cidrs        = concat(local.alb_subnet_cidrs, local.cloudfront_ipv4_cidrs)
  native_geo_profile = local.geo_enabled ? join("\n", [
    "version = 1",
    "",
    "[geo]",
    "provider = \"cloudfront\"",
    "alb_peer_cidrs = ${jsonencode(local.alb_subnet_cidrs)}",
    "origin_credential_header_name = ${jsonencode(local.geo_credential_header_name)}",
    ""
  ]) : null
}

# Use the same AWS-managed origin-facing list for network ingress and checked
# XFF trust. The prepared application adopts its current entries on task startup.
data "aws_ec2_managed_prefix_list" "cloudfront" {
  count = local.geo_enabled ? 1 : 0
  name  = "com.amazonaws.global.cloudfront.origin-facing"
}

# Persist one credential in workload state. Ordinary updates must not rotate it.
# CloudFront and ALB are its only consumers; never publish it to the task.
resource "random_password" "origin_credential" {
  count   = local.geo_enabled ? 1 : 0
  length  = 43
  special = false
}

resource "aws_vpc_security_group_ingress_rule" "cloudfront" {
  count             = local.geo_enabled ? 1 : 0
  security_group_id = aws_security_group.alb.id
  prefix_list_id    = data.aws_ec2_managed_prefix_list.cloudfront[0].id
  ip_protocol       = "tcp"
  from_port         = 443
  to_port           = 443
}

# Never share this target group with the old Off service or its rollback history.
resource "aws_lb_target_group" "geo" {
  count                = local.geo_enabled ? 1 : 0
  name                 = "${var.run_id}-geo"
  vpc_id               = aws_vpc.review.id
  target_type          = "ip"
  port                 = 8787
  protocol             = "HTTP"
  deregistration_delay = var.target_deregistration_delay_seconds
  health_check {
    enabled             = local.target_health.enabled
    path                = local.target_health.path
    protocol            = local.target_health.protocol
    matcher             = local.target_health.matcher
    interval            = local.target_health.interval
    timeout             = local.target_health.timeout
    healthy_threshold   = local.target_health.healthy_threshold
    unhealthy_threshold = local.target_health.unhealthy_threshold
  }
}

resource "aws_lb_listener_rule" "geo" {
  count        = local.geo_enabled ? 1 : 0
  listener_arn = aws_lb_listener.https.arn
  priority     = 90
  action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.geo[0].arn
  }
  condition {
    host_header { values = [var.demo_hostname] }
  }
  condition {
    http_header {
      http_header_name = local.geo_credential_header_name
      values           = [local.geo_credential_value]
    }
  }
}

# Explicitly request generated assertions. Viewer-header forwarding alone does
# not request CloudFront's geographic metadata. All viewer headers also preserve
# Host and admin Authorization. Cookies/query strings carry real consent state.
resource "aws_cloudfront_origin_request_policy" "geo" {
  count = local.geo_enabled ? 1 : 0
  name  = "${var.run_id}-geo"
  headers_config {
    header_behavior = "allViewerAndWhitelistCloudFront"
    headers { items = ["CloudFront-Viewer-Country", "CloudFront-Viewer-Country-Region"] }
  }
  cookies_config { cookie_behavior = "all" }
  query_strings_config { query_string_behavior = "all" }
}

resource "aws_cloudfront_cache_policy" "geo" {
  count       = local.geo_enabled ? 1 : 0
  name        = "${var.run_id}-no-cache"
  min_ttl     = 0
  default_ttl = 0
  max_ttl     = 0
  parameters_in_cache_key_and_forwarded_to_origin {
    enable_accept_encoding_gzip   = false
    enable_accept_encoding_brotli = false
    headers_config { header_behavior = "none" }
    cookies_config { cookie_behavior = "none" }
    query_strings_config { query_string_behavior = "none" }
  }
}

resource "aws_cloudfront_distribution" "geo" {
  count               = local.geo_enabled ? 1 : 0
  enabled             = local.geo_phase != "staged"
  is_ipv6_enabled     = false
  aliases             = [var.demo_hostname]
  comment             = "${var.run_id} protected native geo"
  wait_for_deployment = true
  retain_on_delete    = false
  tags                = local.tags
  origin {
    # Independent of the viewer DNS alias, including after cutover.
    domain_name = aws_lb.review.dns_name
    origin_id   = "protected-alb"
    custom_header {
      name  = local.geo_credential_header_name
      value = local.geo_credential_value
    }
    # ALB preserves this field, and trusted proxy normalization consumes it.
    # Fix browser identity at deployment time, never trust the viewer value.
    # Publisher identity is opt-in; TLS and ALB admission still use demo Host.
    custom_header {
      name  = "X-Forwarded-Host"
      value = var.preserve_publisher_hostname ? var.publisher_hostname : var.demo_hostname
    }
    custom_origin_config {
      http_port              = 80
      https_port             = 443
      origin_protocol_policy = "https-only"
      origin_ssl_protocols   = ["TLSv1.2"]
    }
  }
  default_cache_behavior {
    target_origin_id         = "protected-alb"
    viewer_protocol_policy   = "https-only"
    allowed_methods          = ["GET", "HEAD", "OPTIONS", "PUT", "PATCH", "POST", "DELETE"]
    cached_methods           = ["GET", "HEAD"]
    cache_policy_id          = aws_cloudfront_cache_policy.geo[0].id
    origin_request_policy_id = aws_cloudfront_origin_request_policy.geo[0].id
    compress                 = false
  }
  dynamic "custom_error_response" {
    for_each = toset([400, 403, 404, 405, 414, 416, 500, 501, 502, 503, 504])
    content {
      error_code            = custom_error_response.value
      error_caching_min_ttl = 0
    }
  }
  restrictions {
    geo_restriction { restriction_type = "none" }
  }
  viewer_certificate {
    acm_certificate_arn      = aws_acm_certificate_validation.viewer[0].certificate_arn
    ssl_support_method       = "sni-only"
    minimum_protocol_version = "TLSv1.2_2021"
  }
}
