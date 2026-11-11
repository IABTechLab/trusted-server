locals {
  log_configuration = {
    logDriver = "awslogs"
    options = {
      awslogs-group         = aws_cloudwatch_log_group.review.name
      awslogs-region        = var.aws_region
      awslogs-stream-prefix = "review"
    }
  }
  application_environment = merge({
    RUST_LOG                                    = var.application_log_filter
    AWS_REGION                                  = var.aws_region
    AWS_DEFAULT_REGION                          = var.aws_region
    AWS_EC2_METADATA_DISABLED                   = "true"
    EDGEZERO__ADAPTER__HOST                     = "0.0.0.0"
    EDGEZERO__ADAPTER__PORT                     = "8787"
    EDGEZERO__ADAPTER__SHUTDOWN_GRACE_SECONDS   = tostring(var.app_shutdown_grace_seconds)
    EDGEZERO__ADAPTER__TRUSTED_PROXY_CIDRS      = join(",", local.trusted_proxy_cidrs)
    EDGEZERO__ADAPTER__FORWARDING_HEADER_FAMILY = "x-forwarded"
    EDGEZERO__STORE_BINDINGS_FILE               = "/run/trusted-server/native-bindings.toml"
    TS_NATIVE_BINDINGS_TOML                     = local.native_bindings
    # Release metadata forces a task-definition revision when config changes.
    # These fields do not make the Agent read a historical configuration.
    TS_REVIEW_CONFIG_TRANSPORT_SHA256 = sha256(local.envelope_text)
    TS_REVIEW_APPCONFIG_VERSION       = tostring(aws_appconfig_hosted_configuration_version.review.version_number)
    TS_REVIEW_APPCONFIG_DEPLOYMENT    = tostring(aws_appconfig_deployment.review.deployment_number)
    }, local.geo_enabled ? {
    TS_NATIVE_GEO_PROFILE_TOML = local.native_geo_profile
  } : {})
  containers = [
    {
      name                   = "app"
      image                  = var.application_image
      essential              = true
      user                   = "10001:10001"
      readonlyRootFilesystem = true
      stopTimeout            = var.container_stop_timeout_seconds
      portMappings           = [{ containerPort = 8787, hostPort = 8787, protocol = "tcp" }]
      # Match ECS's empty-array representation so a phase-only change does not
      # replace an otherwise identical task definition after an AWS refresh.
      linuxParameters = { capabilities = { add = [], drop = ["ALL"] } }
      systemControls  = []
      volumesFrom     = []
      mountPoints     = [{ sourceVolume = "runtime-bindings", containerPath = "/run/trusted-server", readOnly = false }]
      environment     = [for key in sort(keys(local.application_environment)) : { name = key, value = local.application_environment[key] }]
      secrets         = [{ name = "TS_APPCONFIG_TOKEN", valueFrom = local.token_value_from }]
      # START is ordering only, not a fabricated readiness probe. The accepted
      # provider/image must qualify cold Agent startup within bounded reads.
      dependsOn        = [{ containerName = "appconfig-agent", condition = "START" }]
      logConfiguration = local.log_configuration
    },
    {
      name                   = "appconfig-agent"
      image                  = var.agent_image
      essential              = true
      readonlyRootFilesystem = true
      stopTimeout            = var.container_stop_timeout_seconds
      linuxParameters        = { capabilities = { add = [], drop = ["ALL"] } }
      systemControls         = []
      volumesFrom            = []
      mountPoints            = []
      portMappings           = []
      environment = [
        { name = "HTTP_HOST", value = "127.0.0.1" },
        { name = "HTTP_PORT", value = "2772" },
        { name = "LOG_LEVEL", value = var.agent_log_level },
        { name = "REQUEST_TIMEOUT", value = "3000ms" },
        { name = "POLL_INTERVAL", value = "60s" },
        { name = "PRELOAD_BACKUPS", value = "false" },
        { name = "PREFETCH_LIST", value = "${aws_appconfig_application.review.id}:${aws_appconfig_environment.review.environment_id}:${aws_appconfig_configuration_profile.review.configuration_profile_id}" }
      ]
      # BACKUP_DIRECTORY and MANIFEST deliberately absent. PRELOAD_BACKUPS=false
      # alone would still permit disk-backup fallback if a directory were set.
      secrets          = [{ name = "ACCESS_TOKEN", valueFrom = local.token_value_from }]
      logConfiguration = local.log_configuration
    }
  ]
}

resource "aws_ecs_cluster" "review" {
  name = var.run_id
  setting {
    name  = "containerInsights"
    value = "disabled"
  }
  depends_on = [terraform_data.draft_gate]
}

resource "aws_ecs_task_definition" "review" {
  family                   = var.run_id
  requires_compatibilities = ["FARGATE"]
  network_mode             = "awsvpc"
  cpu                      = tostring(var.task_cpu)
  memory                   = tostring(var.task_memory)
  execution_role_arn       = aws_iam_role.execution.arn
  task_role_arn            = aws_iam_role.task.arn
  runtime_platform {
    operating_system_family = "LINUX"
    cpu_architecture        = "X86_64"
  }
  volume {
    name = "runtime-bindings"
  }
  container_definitions = jsonencode(local.containers)
  depends_on            = [terraform_data.draft_gate]
}

// Retire the old service before its final listener association is removed.
// Keeping a zero-desired service would still require that association for every
// ECS UpdateService, including the scale-down itself.
moved {
  from = aws_ecs_service.review
  to   = aws_ecs_service.review[0]
}

resource "aws_ecs_service" "review" {
  count                              = local.geo_enabled ? 0 : 1
  name                               = var.run_id
  cluster                            = aws_ecs_cluster.review.id
  task_definition                    = aws_ecs_task_definition.review.arn
  desired_count                      = local.service_settings.desired_count
  launch_type                        = "FARGATE"
  platform_version                   = "1.4.0"
  enable_execute_command             = false
  wait_for_steady_state              = true
  health_check_grace_period_seconds  = local.service_settings.health_check_grace_period_seconds
  deployment_minimum_healthy_percent = local.service_settings.deployment_minimum_healthy_percent
  deployment_maximum_percent         = local.service_settings.deployment_maximum_percent
  deployment_circuit_breaker {
    enable   = true
    rollback = true
  }
  network_configuration {
    subnets          = [aws_subnet.task.id]
    security_groups  = [aws_security_group.task.id]
    assign_public_ip = true
  }
  load_balancer {
    target_group_arn = aws_lb_target_group.review.arn
    container_name   = "app"
    container_port   = 8787
  }
  timeouts {
    create = "15m"
    update = "15m"
    delete = "15m"
  }
  depends_on = [
    terraform_data.draft_gate,
    aws_appconfig_deployment.review,
    aws_lb_listener_rule.publisher,
    aws_vpc_security_group_egress_rule.auction,
    aws_iam_role_policy.execution,
    aws_iam_role_policy.task,
    aws_route_table_association.public,
    aws_route.internet
  ]
}

// Separate service history prevents ECS rollback from selecting an old Off task.
resource "aws_ecs_service" "geo" {
  count                              = local.geo_enabled ? 1 : 0
  name                               = "${var.run_id}-geo"
  cluster                            = aws_ecs_cluster.review.id
  task_definition                    = aws_ecs_task_definition.review.arn
  desired_count                      = local.service_settings.desired_count
  launch_type                        = "FARGATE"
  platform_version                   = "1.4.0"
  enable_execute_command             = false
  wait_for_steady_state              = true
  health_check_grace_period_seconds  = local.service_settings.health_check_grace_period_seconds
  deployment_minimum_healthy_percent = local.service_settings.deployment_minimum_healthy_percent
  deployment_maximum_percent         = local.service_settings.deployment_maximum_percent
  deployment_circuit_breaker {
    enable   = true
    rollback = true
  }
  network_configuration {
    subnets          = [aws_subnet.task.id]
    security_groups  = [aws_security_group.task.id]
    assign_public_ip = true
  }
  load_balancer {
    target_group_arn = aws_lb_target_group.geo[0].arn
    container_name   = "app"
    container_port   = 8787
  }
  timeouts {
    create = "15m"
    update = "15m"
    delete = "15m"
  }
  depends_on = [
    terraform_data.draft_gate,
    aws_appconfig_deployment.review,
    aws_lb_listener_rule.geo,
    aws_lb_listener_rule.publisher,
    aws_vpc_security_group_ingress_rule.cloudfront,
    aws_vpc_security_group_ingress_rule.reviewers,
    aws_vpc_security_group_egress_rule.auction,
    aws_iam_role_policy.execution,
    aws_iam_role_policy.task,
    aws_route_table_association.public,
    aws_route.internet
  ]
}
