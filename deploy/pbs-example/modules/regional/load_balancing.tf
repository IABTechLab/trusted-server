resource "aws_lb" "main" {
  name                       = "${var.name}-alb"
  internal                   = false
  load_balancer_type         = "application"
  security_groups            = [aws_security_group.alb.id]
  subnets                    = [for availability_zone in var.availability_zones : aws_subnet.public[availability_zone].id]
  drop_invalid_header_fields = true
  idle_timeout               = 30

  tags = merge(var.tags, {
    Name = "${var.name}-alb"
  })
}

resource "aws_lb_target_group" "pbs" {
  name                 = "${var.name}-pbs"
  port                 = local.pbs_port
  protocol             = "HTTP"
  target_type          = "instance"
  vpc_id               = aws_vpc.main.id
  deregistration_delay = 30

  health_check {
    enabled             = true
    path                = "/status"
    port                = "traffic-port"
    protocol            = "HTTP"
    matcher             = "200-399"
    interval            = 15
    timeout             = 5
    healthy_threshold   = 2
    unhealthy_threshold = 3
  }

  tags = merge(var.tags, {
    Name = "${var.name}-pbs"
  })
}

resource "aws_lb_target_group_attachment" "pbs" {
  for_each = aws_instance.pbs

  target_group_arn = aws_lb_target_group.pbs.arn
  target_id        = each.value.id
  port             = local.pbs_port
}

resource "aws_lb_listener" "https" {
  load_balancer_arn = aws_lb.main.arn
  port              = 443
  protocol          = "HTTPS"
  ssl_policy        = "ELBSecurityPolicy-TLS13-1-2-2021-06"
  certificate_arn   = var.certificate_arn

  default_action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.pbs.arn
  }
}
