locals {
  task_assume_role = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Action    = "sts:AssumeRole"
      Principal = { Service = "ecs-tasks.amazonaws.com" }
      Condition = {
        StringEquals = { "aws:SourceAccount" = var.aws_account_id }
        ArnLike      = { "aws:SourceArn" = "arn:aws:ecs:${var.aws_region}:${var.aws_account_id}:*" }
      }
    }]
  })
  ecr_matches = [for image in [var.application_image, var.agent_image] : try(
    regex("^([0-9]{12})\\.dkr\\.ecr\\.([a-z0-9-]+)\\.amazonaws\\.com/(.+)@sha256:[0-9a-f]{64}$", image),
    null
  )]
  private_ecr_arns = [for match in local.ecr_matches : "arn:aws:ecr:${match[1]}:${match[0]}:repository/${match[2]}" if match != null]
  kms_read_statement = length(var.secrets_kms_key_arns) == 0 ? [] : [{
    Effect   = "Allow"
    Action   = ["kms:Decrypt"]
    Resource = sort(tolist(var.secrets_kms_key_arns))
    Condition = {
      StringEquals = { "kms:ViaService" = "secretsmanager.${var.aws_region}.amazonaws.com" }
    }
  }]
}

resource "aws_iam_role" "execution" {
  name                 = "${var.run_id}-execution"
  permissions_boundary = var.runtime_boundaries.execution
  assume_role_policy   = local.task_assume_role
  depends_on           = [terraform_data.draft_gate]
}

resource "aws_iam_role_policy" "execution" {
  name = "${var.run_id}-execution"
  role = aws_iam_role.execution.id
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = concat([
      {
        Effect   = "Allow"
        Action   = ["logs:CreateLogStream", "logs:PutLogEvents"]
        Resource = ["${aws_cloudwatch_log_group.review.arn}:*"]
      },
      {
        Effect   = "Allow"
        Action   = ["secretsmanager:GetSecretValue"]
        Resource = [var.agent_token.arn]
      }
      ], [for statement in [
        {
          Effect   = "Allow"
          Action   = ["ecr:GetAuthorizationToken"]
          Resource = "*"
        },
        {
          Effect   = "Allow"
          Action   = ["ecr:BatchCheckLayerAvailability", "ecr:GetDownloadUrlForLayer", "ecr:BatchGetImage"]
          Resource = local.private_ecr_arns
        }
    ] : statement if length(local.private_ecr_arns) > 0], local.kms_read_statement)
  })
  depends_on = [terraform_data.draft_gate]
}

resource "aws_iam_role" "task" {
  name                 = "${var.run_id}-task"
  permissions_boundary = var.runtime_boundaries.task
  assume_role_policy   = local.task_assume_role
  depends_on           = [terraform_data.draft_gate]
}

resource "aws_iam_role_policy" "task" {
  name = "${var.run_id}-runtime-read"
  role = aws_iam_role.task.id
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = concat([
      {
        Effect   = "Allow"
        Action   = ["appconfig:StartConfigurationSession", "appconfig:GetLatestConfiguration"]
        Resource = ["arn:aws:appconfig:${var.aws_region}:${var.aws_account_id}:application/${aws_appconfig_application.review.id}/environment/${aws_appconfig_environment.review.environment_id}/configuration/${aws_appconfig_configuration_profile.review.configuration_profile_id}"]
      },
      {
        Effect   = "Allow"
        Action   = ["secretsmanager:GetSecretValue"]
        Resource = sort(distinct([for ref in values(var.application_secrets) : ref.arn]))
      }
    ], local.kms_read_statement)
  })
  depends_on = [terraform_data.draft_gate]
}
