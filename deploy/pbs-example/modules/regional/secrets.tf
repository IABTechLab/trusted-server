resource "aws_secretsmanager_secret" "bidder" {
  for_each = local.bidder_secret_names

  description             = "Credential payload for ${each.key}; values are written outside Terraform."
  name                    = each.value
  kms_key_id              = var.secrets_kms_key_arn
  recovery_window_in_days = 7

  tags = merge(var.tags, {
    Name   = each.value
    Secret = each.key
  })
}

resource "aws_iam_role" "pbs" {
  name = "${var.name}-runtime"
  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Action = "sts:AssumeRole"
      Effect = "Allow"
      Principal = {
        Service = "ec2.amazonaws.com"
      }
    }]
  })

  tags = merge(var.tags, {
    Name = "${var.name}-runtime"
  })
}

resource "aws_iam_instance_profile" "pbs" {
  name = "${var.name}-runtime"
  role = aws_iam_role.pbs.name

  tags = merge(var.tags, {
    Name = "${var.name}-runtime"
  })
}

resource "aws_iam_role_policy_attachment" "ssm" {
  role       = aws_iam_role.pbs.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"
}

resource "aws_iam_role_policy" "runtime" {
  name = "runtime"
  role = aws_iam_role.pbs.name

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = concat([
      {
        Sid    = "ReadBidderSecrets"
        Effect = "Allow"
        Action = [
          "secretsmanager:DescribeSecret",
          "secretsmanager:GetSecretValue",
        ]
        Resource = [for secret in aws_secretsmanager_secret.bidder : secret.arn]
      },
      {
        Sid    = "WriteRuntimeLogs"
        Effect = "Allow"
        Action = [
          "logs:CreateLogStream",
          "logs:DescribeLogStreams",
          "logs:PutLogEvents",
        ]
        Resource = "${aws_cloudwatch_log_group.runtime.arn}:*"
      },
      ], var.secrets_kms_key_arn == null ? [] : [{
        Sid      = "DecryptBidderSecrets"
        Effect   = "Allow"
        Action   = ["kms:Decrypt"]
        Resource = var.secrets_kms_key_arn
    }])
  })
}
