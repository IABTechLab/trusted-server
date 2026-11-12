resource "aws_vpc" "review" {
  cidr_block           = var.vpc_cidr
  enable_dns_support   = true
  enable_dns_hostnames = true
  tags                 = { Name = var.run_id }
}

resource "aws_internet_gateway" "review" {
  vpc_id = aws_vpc.review.id
}

resource "aws_subnet" "alb" {
  count                   = 2
  vpc_id                  = aws_vpc.review.id
  availability_zone       = var.availability_zones[count.index]
  cidr_block              = local.alb_subnet_cidrs[count.index]
  map_public_ip_on_launch = false
  tags                    = { Name = "${var.run_id}-alb-${count.index}" }
}

resource "aws_subnet" "task" {
  vpc_id                  = aws_vpc.review.id
  availability_zone       = var.availability_zones[0]
  cidr_block              = local.task_subnet_cidr
  map_public_ip_on_launch = false
  tags                    = { Name = "${var.run_id}-task" }
}

resource "aws_route_table" "public" {
  vpc_id = aws_vpc.review.id
}

resource "aws_route" "internet" {
  route_table_id         = aws_route_table.public.id
  destination_cidr_block = "0.0.0.0/0"
  gateway_id             = aws_internet_gateway.review.id
}

resource "aws_route_table_association" "public" {
  for_each       = { alb0 = aws_subnet.alb[0].id, alb1 = aws_subnet.alb[1].id, task = aws_subnet.task.id }
  subnet_id      = each.value
  route_table_id = aws_route_table.public.id
}

resource "aws_security_group" "alb" {
  name_prefix = "${var.run_id}-alb-"
  description = "Reviewer HTTPS only; backend traffic limited to the task SG"
  vpc_id      = aws_vpc.review.id
}

resource "aws_security_group" "task" {
  name_prefix = "${var.run_id}-task-"
  description = "Only owned ALB ingress; no public application port"
  vpc_id      = aws_vpc.review.id
}

resource "aws_vpc_security_group_ingress_rule" "reviewers" {
  for_each          = local.geo_enabled ? toset([]) : var.reviewer_ipv4_cidrs
  security_group_id = aws_security_group.alb.id
  ip_protocol       = "tcp"
  from_port         = 443
  to_port           = 443
  cidr_ipv4         = each.value
}

resource "aws_vpc_security_group_egress_rule" "alb_to_task" {
  security_group_id            = aws_security_group.alb.id
  referenced_security_group_id = aws_security_group.task.id
  ip_protocol                  = "tcp"
  from_port                    = 8787
  to_port                      = 8787
}

resource "aws_vpc_security_group_ingress_rule" "alb_to_task" {
  security_group_id            = aws_security_group.task.id
  referenced_security_group_id = aws_security_group.alb.id
  ip_protocol                  = "tcp"
  from_port                    = 8787
  to_port                      = 8787
}

resource "aws_vpc_security_group_egress_rule" "task_https" {
  security_group_id = aws_security_group.task.id
  ip_protocol       = "tcp"
  from_port         = 443
  to_port           = 443
  cidr_ipv4         = "0.0.0.0/0"
  description       = "Reviewed public HTTPS origin/AWS/registry egress; no NAT or VPC endpoints"
}

resource "aws_vpc_security_group_egress_rule" "auction" {
  count             = var.auction_egress == null ? 0 : 1
  security_group_id = aws_security_group.task.id
  ip_protocol       = "tcp"
  from_port         = var.auction_egress.port
  to_port           = var.auction_egress.port
  cidr_ipv4         = var.auction_egress.cidr_ipv4
  description       = "Reviewed single-endpoint auction TCP egress"
}

resource "aws_lb" "review" {
  name                       = var.run_id
  internal                   = false
  load_balancer_type         = "application"
  ip_address_type            = "ipv4"
  subnets                    = aws_subnet.alb[*].id
  security_groups            = [aws_security_group.alb.id]
  preserve_host_header       = true
  drop_invalid_header_fields = true
  desync_mitigation_mode     = "strictest"
  idle_timeout               = var.alb_idle_timeout_seconds
  enable_deletion_protection = false
}

resource "aws_lb_target_group" "review" {
  name                 = "${var.run_id}-app"
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

resource "aws_lb_listener" "https" {
  load_balancer_arn = aws_lb.review.arn
  port              = 443
  protocol          = "HTTPS"
  ssl_policy        = "ELBSecurityPolicy-TLS13-1-2-2021-06"
  certificate_arn   = var.certificate_arn
  default_action {
    type = "fixed-response"
    fixed_response {
      content_type = "text/plain"
      message_body = "Not a configured review host"
      status_code  = "404"
    }
  }
}

# Accept both the publisher-preserving CLI flow and direct demo browsing.
# A normal browser does not supply X-Forwarded-Host. Keep the exact Host allowlist
# and the default 404; this rule is not a forwarding-header provenance check.
resource "aws_lb_listener_rule" "publisher" {
  listener_arn = aws_lb_listener.https.arn
  priority     = 100
  dynamic "action" {
    for_each = local.geo_enabled ? [] : [1]
    content {
      type             = "forward"
      target_group_arn = aws_lb_target_group.review.arn
    }
  }
  dynamic "action" {
    for_each = local.geo_enabled ? [1] : []
    content {
      type = "fixed-response"
      fixed_response {
        content_type = "text/plain"
        message_body = "Protected origin only"
        status_code  = "404"
      }
    }
  }
  condition {
    host_header { values = [var.publisher_hostname, var.demo_hostname] }
  }
}

resource "aws_route53_record" "review" {
  zone_id = var.route53_zone_id
  name    = var.demo_hostname
  type    = "A"
  alias {
    name                   = local.geo_phase == "dns-cutover" ? aws_cloudfront_distribution.geo[0].domain_name : aws_lb.review.dns_name
    zone_id                = local.geo_phase == "dns-cutover" ? aws_cloudfront_distribution.geo[0].hosted_zone_id : aws_lb.review.zone_id
    evaluate_target_health = local.geo_phase != "dns-cutover"
  }
}
