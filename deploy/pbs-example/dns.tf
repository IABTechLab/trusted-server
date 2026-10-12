resource "aws_route53_record" "east" {
  zone_id = var.route53_zone_id
  name    = var.pbs_hostname
  type    = "A"

  set_identifier = "us-east-1"

  alias {
    name                   = module.east.alb_dns_name
    zone_id                = module.east.alb_zone_id
    evaluate_target_health = true
  }

  latency_routing_policy {
    region = "us-east-1"
  }
}

resource "aws_route53_record" "west" {
  zone_id = var.route53_zone_id
  name    = var.pbs_hostname
  type    = "A"

  set_identifier = "us-west-2"

  alias {
    name                   = module.west.alb_dns_name
    zone_id                = module.west.alb_zone_id
    evaluate_target_health = true
  }

  latency_routing_policy {
    region = "us-west-2"
  }
}
