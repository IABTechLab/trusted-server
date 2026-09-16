resource "aws_cloudwatch_log_group" "runtime" {
  name              = "/pbs-example/${var.region}"
  retention_in_days = var.log_retention_days

  tags = merge(var.tags, {
    Name = "${var.name}-runtime"
  })
}

resource "aws_cloudwatch_metric_alarm" "instance_cpu" {
  for_each = aws_instance.pbs

  alarm_name          = "${var.name}-${each.key}-high-cpu"
  comparison_operator = "GreaterThanThreshold"
  evaluation_periods  = 3
  metric_name         = "CPUUtilization"
  namespace           = "AWS/EC2"
  period              = 60
  statistic           = "Average"
  threshold           = 80
  alarm_actions       = var.alarm_actions
  treat_missing_data  = "breaching"

  dimensions = {
    InstanceId = each.value.id
  }

  tags = merge(var.tags, {
    Name = "${var.name}-${each.key}-high-cpu"
  })
}

resource "aws_cloudwatch_metric_alarm" "unhealthy_hosts" {
  alarm_name          = "${var.name}-unhealthy-hosts"
  comparison_operator = "GreaterThanThreshold"
  evaluation_periods  = 2
  metric_name         = "UnHealthyHostCount"
  namespace           = "AWS/ApplicationELB"
  period              = 60
  statistic           = "Maximum"
  threshold           = 0
  alarm_actions       = var.alarm_actions
  treat_missing_data  = "breaching"

  dimensions = {
    LoadBalancer = aws_lb.main.arn_suffix
    TargetGroup  = aws_lb_target_group.pbs.arn_suffix
  }

  tags = merge(var.tags, {
    Name = "${var.name}-unhealthy-hosts"
  })
}

resource "aws_cloudwatch_metric_alarm" "elb_5xx" {
  alarm_name          = "${var.name}-elb-5xx"
  comparison_operator = "GreaterThanThreshold"
  evaluation_periods  = 3
  metric_name         = "HTTPCode_ELB_5XX_Count"
  namespace           = "AWS/ApplicationELB"
  period              = 60
  statistic           = "Sum"
  threshold           = 1
  alarm_actions       = var.alarm_actions
  treat_missing_data  = "notBreaching"

  dimensions = {
    LoadBalancer = aws_lb.main.arn_suffix
  }

  tags = merge(var.tags, {
    Name = "${var.name}-elb-5xx"
  })
}
