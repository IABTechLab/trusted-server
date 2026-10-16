resource "aws_security_group" "alb" {
  name        = "${var.name}-alb"
  description = "HTTPS ingress for the regional PBS ALB"
  vpc_id      = aws_vpc.main.id

  tags = merge(var.tags, {
    Name = "${var.name}-alb"
  })
}

resource "aws_vpc_security_group_ingress_rule" "trusted_server_https" {
  for_each = var.trusted_server_cidr_blocks

  description       = "Trusted Server HTTPS"
  security_group_id = aws_security_group.alb.id
  cidr_ipv4         = each.value
  from_port         = 443
  to_port           = 443
  ip_protocol       = "tcp"
}

resource "aws_vpc_security_group_egress_rule" "alb_to_pbs" {
  description                  = "Forward requests to private PBS hosts"
  security_group_id            = aws_security_group.alb.id
  referenced_security_group_id = aws_security_group.pbs.id
  from_port                    = local.pbs_port
  to_port                      = local.pbs_port
  ip_protocol                  = "tcp"
}

resource "aws_security_group" "pbs" {
  name        = "${var.name}-pbs"
  description = "PBS host traffic from the regional ALB and outbound bidder access"
  vpc_id      = aws_vpc.main.id

  ingress {
    description     = "PBS HTTP from the regional ALB"
    from_port       = local.pbs_port
    to_port         = local.pbs_port
    protocol        = "tcp"
    security_groups = [aws_security_group.alb.id]
  }

  egress {
    description = "Bidder and AWS API access through the per-AZ NAT gateway"
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }

  tags = merge(var.tags, {
    Name = "${var.name}-pbs"
  })
}
