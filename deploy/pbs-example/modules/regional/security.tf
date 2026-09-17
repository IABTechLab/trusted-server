resource "aws_security_group" "alb" {
  name        = "${var.name}-alb"
  description = "HTTPS ingress for the regional PBS ALB"
  vpc_id      = aws_vpc.main.id

  dynamic "ingress" {
    for_each = var.trusted_server_cidr_blocks

    content {
      description = "Trusted Server HTTPS"
      from_port   = 443
      to_port     = 443
      protocol    = "tcp"
      cidr_blocks = [ingress.value]
    }
  }

  egress {
    description = "Forward requests to private PBS hosts"
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }

  tags = merge(var.tags, {
    Name = "${var.name}-alb"
  })
}

resource "aws_security_group" "pbs" {
  name        = "${var.name}-pbs"
  description = "PBS host traffic from the regional ALB and outbound bidder access"
  vpc_id      = aws_vpc.main.id

  ingress {
    description     = "PBS HTTP from the regional ALB"
    from_port       = var.pbs_port
    to_port         = var.pbs_port
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
