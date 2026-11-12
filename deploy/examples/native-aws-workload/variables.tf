variable "run_id" {
  description = "Stable workload name used by the persistent Terraform state."
  type        = string
  validation {
    condition     = can(regex("^[a-z][a-z0-9-]{2,23}$", var.run_id))
    error_message = "run_id must be 3-24 lowercase letters/digits/hyphens, starting with a letter."
  }
}

variable "owner" {
  description = "Named owner responsible for cost, manual shutdown and retained resources."
  type        = string
  validation {
    condition     = length(trimspace(var.owner)) > 0
    error_message = "owner must not be empty."
  }
}

variable "aws_account_id" {
  description = "Explicit authorized AWS account; provider refuses another account."
  type        = string
  validation {
    condition     = can(regex("^[0-9]{12}$", var.aws_account_id))
    error_message = "aws_account_id must contain 12 digits."
  }
}

variable "aws_region" {
  description = "One approved commercial AWS region. No region is inferred from credentials."
  type        = string
  validation {
    condition     = can(regex("^[a-z]{2}-[a-z]+-[0-9]$", var.aws_region))
    error_message = "Supply an explicit commercial AWS region."
  }
}

variable "availability_zones" {
  description = "Two approved AZs for the regional ALB; the initial task runs in the first."
  type        = list(string)
  validation {
    condition     = length(var.availability_zones) == 2 && length(distinct(var.availability_zones)) == 2 && alltrue([for zone in var.availability_zones : startswith(zone, var.aws_region)])
    error_message = "Supply two distinct Availability Zones in aws_region."
  }
}

variable "demo_hostname" {
  description = "Owned demo HTTPS hostname whose certificate is used for proxy-to-ALB TLS."
  type        = string
  validation {
    condition     = can(regex("^[a-z0-9][a-z0-9.-]*\\.[a-z]{2,}$", var.demo_hostname)) && var.demo_hostname != var.publisher_hostname
    error_message = "demo_hostname must be a lowercase DNS hostname distinct from the publisher hostname, not a URL."
  }
}

variable "publisher_hostname" {
  description = "Exact approved publisher Host retained by ts dev proxy. No production DNS change."
  type        = string
  validation {
    condition     = can(regex("^[a-z0-9][a-z0-9.-]*\\.[a-z]{2,}$", var.publisher_hostname))
    error_message = "publisher_hostname must be a lowercase DNS hostname, not a URL."
  }
}

variable "preserve_publisher_hostname" {
  description = "CloudFront-only opt-in for a publisher-origin browser proxy: fix X-Forwarded-Host to publisher_hostname instead of demo_hostname. Does not change demo TLS, ALB Host admission or production DNS."
  type        = bool
  default     = false
}

variable "certificate_arn" {
  description = "Existing reviewed ACM certificate covering demo_hostname; retained outside this workload."
  type        = string
  validation {
    condition     = can(regex("^arn:aws:acm:${var.aws_region}:${var.aws_account_id}:certificate/[a-zA-Z0-9-]+$", var.certificate_arn))
    error_message = "certificate_arn must belong to the approved account and region."
  }
}

variable "route53_zone_id" {
  description = "Existing approved public hosted zone for the demo alias, not the publisher zone."
  type        = string
  validation {
    condition     = can(regex("^Z[A-Z0-9]+$", var.route53_zone_id))
    error_message = "route53_zone_id must be a hosted-zone ID."
  }
}

variable "reviewer_ipv4_cidrs" {
  description = "HTTPS access CIDRs for direct ALB ingress. Application authentication belongs to TS; application and Agent ports remain ALB-only and loopback-only respectively."
  type        = set(string)
  validation {
    condition     = (length(var.reviewer_ipv4_cidrs) > 0 || var.geo_front_door != null) && alltrue([for cidr in var.reviewer_ipv4_cidrs : can(cidrnetmask(cidr))])
    error_message = "Direct ALB ingress needs at least one valid IPv4 CIDR. CloudFront may use an empty set because these CIDRs do not restrict viewers."
  }
}

variable "auction_egress" {
  description = "Optional reviewed auction endpoint. Adds only TCP egress to one IPv4 /32 and one port."
  type = object({
    cidr_ipv4 = string
    port      = number
  })
  default = null
  validation {
    condition = var.auction_egress == null ? true : try(
      can(cidrnetmask(var.auction_egress.cidr_ipv4)) &&
      endswith(var.auction_egress.cidr_ipv4, "/32") &&
      cidrhost(var.auction_egress.cidr_ipv4, 0) == trimsuffix(var.auction_egress.cidr_ipv4, "/32") &&
      var.auction_egress.port >= 1 && var.auction_egress.port <= 65535 &&
      floor(var.auction_egress.port) == var.auction_egress.port,
      false
    )
    error_message = "auction_egress must be null or one canonical IPv4 /32 and an integer TCP port from 1 to 65535."
  }
}

variable "geo_front_door" {
  description = "Explicit protected CloudFront opt-in. Null preserves Geo Off. Terraform generates the origin credential and derives forwarding CIDRs from the managed origin-facing prefix list. The credential enters restricted Terraform state/plans, never ECS. Viewer certificate covers demo_hostname in us-east-1; regional certificate_arn covers the forwarded demo Host."
  type = object({
    rollout_phase                 = optional(string, "staged")
    viewer_certificate_arn        = string
    origin_credential_header_name = string
  })
  sensitive = true
  validation {
    condition = var.geo_front_door == null ? true : (
      contains(["staged", "origin-enabled", "dns-cutover"], var.geo_front_door.rollout_phase) &&
      can(regex("^arn:aws:acm:us-east-1:${var.aws_account_id}:certificate/[a-zA-Z0-9-]+$", var.geo_front_door.viewer_certificate_arn)) &&
      # Lowercase deployment subset of ProtectedCloudFrontAlb's dedicated
      # removal policy, also excluding CloudFront-prohibited origin headers.
      can(regex("^x-[a-z0-9-]{1,38}$", var.geo_front_door.origin_credential_header_name)) &&
      !can(regex("^(x-amz-|x-amzn-|x-edge-|x-forwarded-)", var.geo_front_door.origin_credential_header_name)) &&
      !contains(["x-real-ip", "x-request-id", "x-correlation-id", "host", "authorization", "cookie", "connection", "content-length", "expect", "if-match", "if-modified-since", "if-none-match", "if-range", "if-unmodified-since", "max-forwards", "proxy-authenticate", "proxy-authorization", "proxy-connection", "range", "request-range", "te", "trailer", "transfer-encoding", "upgrade", "via", "cache-control", "pragma"], var.geo_front_door.origin_credential_header_name)
    )
    error_message = "Supply a same-account us-east-1 viewer certificate and a dedicated lowercase x- origin header name excluding forwarding/AWS/real-IP/correlation fields."
  }
}

variable "application_image" {
  description = "Accepted linux/amd64 deployment image with both AWS providers and the reviewed runtime bootstrap wrapper. No tag-only references or deploy-time build."
  type        = string
  validation {
    condition     = can(regex("^[a-zA-Z0-9./:_-]+@sha256:[0-9a-f]{64}$", var.application_image))
    error_message = "application_image must be an approved digest-pinned image reference."
  }
}

variable "agent_image" {
  description = "Reviewed linux/amd64 AWS AppConfig Agent image, pinned by manifest digest."
  type        = string
  validation {
    condition     = can(regex("^[a-zA-Z0-9./:_-]+@sha256:[0-9a-f]{64}$", var.agent_image))
    error_message = "agent_image must be a digest-pinned image reference."
  }
}

variable "config_envelope_file" {
  description = "Reviewed reference-only TS CLI publication-envelope JSON file. Never the outer local map or a config containing resolved secret values. Content enters Terraform state."
  type        = string
}

variable "ec_storage" {
  description = "Exact retained ec_storage output, also published as ec.storage in the CLI envelope. The workload owns only scoped task access, never the table."
  type = object({
    provider  = string
    region    = string
    table     = string
    namespace = string
  })
  nullable = false
  validation {
    condition     = var.ec_storage.provider == "dynamodb" && var.ec_storage.region == var.aws_region && can(regex("^[A-Za-z0-9_.-]{3,255}$", var.ec_storage.table))
    error_message = "ec_storage must select DynamoDB in the workload region and a 3-255-character table name containing only ASCII letters, digits, underscores, periods or hyphens."
  }
}

variable "application_secrets" {
  description = "Existing separately seeded whole-value Secrets Manager references keyed by the actual TS config secret names. Values never enter Terraform. These supplied secrets are retained outside this workload."
  type = map(object({
    arn        = string
    version_id = string
  }))
  validation {
    condition = length(var.application_secrets) > 0 && alltrue([for key, ref in var.application_secrets :
      can(regex("^[A-Za-z0-9_-]+$", key)) && can(regex("^arn:aws:secretsmanager:${var.aws_region}:${var.aws_account_id}:secret:[A-Za-z0-9/_+=.@-]+$", ref.arn)) && can(regex("^[A-Za-z0-9-]{32,64}$", ref.version_id))
    ])
    error_message = "Each application secret requires a config-reference key, same-account/region secret ARN and immutable 32-64-character version ID."
  }
}

variable "agent_token" {
  description = "Existing separately seeded Agent bearer token reference. ECS injects this same version into Agent ACCESS_TOKEN and app TS_APPCONFIG_TOKEN. No token value is accepted by Terraform."
  type = object({
    arn        = string
    version_id = string
  })
  validation {
    condition     = can(regex("^arn:aws:secretsmanager:${var.aws_region}:${var.aws_account_id}:secret:[A-Za-z0-9/_+=.@-]+$", var.agent_token.arn)) && can(regex("^[A-Za-z0-9-]{32,64}$", var.agent_token.version_id))
    error_message = "agent_token requires a same-account/region secret ARN and immutable 32-64-character version ID."
  }
}

variable "runtime_boundaries" {
  description = "Optional organisation-required task/execution permission ceilings. Null creates roles without boundaries; supplied boundaries remain independently owned and must permit the scoped runtime calls."
  type = object({
    execution = string
    task      = string
  })
  default = null
  validation {
    condition = var.runtime_boundaries == null ? true : (
      alltrue([for arn in values(var.runtime_boundaries) : can(regex("^arn:aws:iam::${var.aws_account_id}:policy/[A-Za-z0-9/+=,.@_-]+$", arn))])
    )
    error_message = "Omit runtime_boundaries or supply same-account managed-policy ARNs required by your organisation. Both roles may use the same boundary."
  }
}

variable "secrets_kms_key_arns" {
  description = "Optional approved customer-managed KMS keys for supplied secrets; no key is provisioned."
  type        = set(string)
  default     = []
  validation {
    condition     = alltrue([for arn in var.secrets_kms_key_arns : can(regex("^arn:aws:kms:${var.aws_region}:${var.aws_account_id}:key/[a-zA-Z0-9-]+$", arn))])
    error_message = "KMS keys must belong to the approved account and region."
  }
}

variable "vpc_cidr" {
  description = "Dedicated demo VPC; /16 supplies separate ALB and task subnets."
  type        = string
  default     = "10.84.0.0/16"
  validation {
    condition     = can(cidrnetmask(var.vpc_cidr)) && endswith(var.vpc_cidr, "/16")
    error_message = "vpc_cidr must be an IPv4 /16."
  }
}

variable "task_cpu" {
  description = "Initial task CPU units. A starting assumption, not a measured capacity claim."
  type        = number
  default     = 1024
  validation {
    condition     = contains([256, 512, 1024, 2048, 4096, 8192, 16384], var.task_cpu)
    error_message = "task_cpu must be a supported x86_64 Fargate CPU size."
  }
}

variable "task_memory" {
  description = "Initial task memory MiB for TS plus Agent; qualify actual resource use."
  type        = number
  default     = 2048
  validation {
    condition = contains(lookup({
      "256"   = [512, 1024, 2048]
      "512"   = range(1024, 4097, 1024)
      "1024"  = range(2048, 8193, 1024)
      "2048"  = range(4096, 16385, 1024)
      "4096"  = range(8192, 30721, 1024)
      "8192"  = range(16384, 61441, 4096)
      "16384" = range(32768, 122881, 8192)
    }, tostring(var.task_cpu), []), var.task_memory)
    error_message = "task_memory must form an AWS-supported Fargate pair with task_cpu."
  }
}

variable "log_retention_in_days" {
  description = "CloudWatch workload log retention, independent of state and external audit retention."
  type        = number
  default     = 7
  validation {
    condition     = contains([0, 1, 3, 5, 7, 14, 30, 60, 90, 120, 150, 180, 365, 400, 545, 731, 1096, 1827, 2192, 2557, 2922, 3288, 3653], var.log_retention_in_days)
    error_message = "Supply a supported CloudWatch log retention value."
  }
}

variable "retain_logs_on_destroy" {
  description = "Retain the workload log group when Terraform destroys the workload."
  type        = bool
  default     = false
}

variable "app_shutdown_grace_seconds" {
  description = "Application request drain grace. Leave time before ECS terminates the containers."
  type        = number
  default     = 10
  validation {
    condition     = floor(var.app_shutdown_grace_seconds) == var.app_shutdown_grace_seconds && var.app_shutdown_grace_seconds >= 1 && var.app_shutdown_grace_seconds < var.container_stop_timeout_seconds
    error_message = "Application shutdown grace must be a positive integer below the container stop timeout."
  }
}

variable "container_stop_timeout_seconds" {
  description = "ECS stop timeout for both application and Agent containers."
  type        = number
  default     = 30
  validation {
    condition     = floor(var.container_stop_timeout_seconds) == var.container_stop_timeout_seconds && var.container_stop_timeout_seconds >= 2 && var.container_stop_timeout_seconds <= 120
    error_message = "Fargate container stop timeout must be an integer from 2 to 120 seconds."
  }
}

variable "alb_idle_timeout_seconds" {
  description = "ALB connection idle timeout."
  type        = number
  default     = 60
  validation {
    condition     = floor(var.alb_idle_timeout_seconds) == var.alb_idle_timeout_seconds && var.alb_idle_timeout_seconds >= 1 && var.alb_idle_timeout_seconds <= 4000
    error_message = "ALB idle timeout must be an integer from 1 to 4000 seconds."
  }
}

variable "target_deregistration_delay_seconds" {
  description = "Target deregistration delay, shared by Off and Geo target groups."
  type        = number
  default     = 15
  validation {
    condition     = floor(var.target_deregistration_delay_seconds) == var.target_deregistration_delay_seconds && var.target_deregistration_delay_seconds >= 0 && var.target_deregistration_delay_seconds <= 3600
    error_message = "Target deregistration delay must be an integer from 0 to 3600 seconds."
  }
}

variable "ecs_health_grace_seconds" {
  description = "ECS load-balancer health grace period, shared by Off and Geo services."
  type        = number
  default     = 90
  validation {
    condition     = floor(var.ecs_health_grace_seconds) == var.ecs_health_grace_seconds && var.ecs_health_grace_seconds >= 0 && var.ecs_health_grace_seconds <= 2147483647
    error_message = "ECS health grace must be an integer from 0 to 2147483647 seconds."
  }
}

variable "health_interval_seconds" {
  description = "ALB target health check interval."
  type        = number
  default     = 15
  validation {
    condition     = floor(var.health_interval_seconds) == var.health_interval_seconds && var.health_interval_seconds >= 5 && var.health_interval_seconds <= 300
    error_message = "Health interval must be an integer from 5 to 300 seconds."
  }
}

variable "health_timeout_seconds" {
  description = "ALB target health check timeout, shorter than its interval."
  type        = number
  default     = 5
  validation {
    condition     = floor(var.health_timeout_seconds) == var.health_timeout_seconds && var.health_timeout_seconds >= 2 && var.health_timeout_seconds <= 120 && var.health_timeout_seconds < var.health_interval_seconds
    error_message = "Health timeout must be an integer from 2 to 120 seconds below the health interval."
  }
}

variable "healthy_threshold" {
  description = "Consecutive successful ALB health checks required for readiness."
  type        = number
  default     = 2
  validation {
    condition     = floor(var.healthy_threshold) == var.healthy_threshold && var.healthy_threshold >= 2 && var.healthy_threshold <= 10
    error_message = "Healthy threshold must be an integer from 2 to 10."
  }
}

variable "unhealthy_threshold" {
  description = "Consecutive failed ALB health checks required for removal."
  type        = number
  default     = 2
  validation {
    condition     = floor(var.unhealthy_threshold) == var.unhealthy_threshold && var.unhealthy_threshold >= 2 && var.unhealthy_threshold <= 10
    error_message = "Unhealthy threshold must be an integer from 2 to 10."
  }
}

variable "application_log_filter" {
  description = "Literal application log level accepted by the qualified Axum producer. No environment interpolation."
  type        = string
  default     = "warn"
  validation {
    condition     = contains(["off", "error", "warn", "info", "debug", "trace"], lower(trimspace(var.application_log_filter)))
    error_message = "Application log filter must be one supported level name, not a module filter."
  }
}

variable "agent_log_level" {
  description = "AWS AppConfig Agent log level."
  type        = string
  default     = "warn"
  validation {
    condition     = contains(["trace", "debug", "info", "warn", "error", "none"], var.agent_log_level)
    error_message = "Supply a supported AppConfig Agent log level."
  }
}

variable "extra_tags" {
  description = "Additional tags. Ownership and identity keys remain reserved."
  type        = map(string)
  default     = {}
  validation {
    condition     = !anytrue([for key in keys(var.extra_tags) : contains(["Application", "Purpose", "RunId", "Owner", "Lifecycle"], key) || startswith(lower(key), "aws:")])
    error_message = "Extra tags cannot override reserved ownership and identity keys or use the AWS-reserved prefix."
  }
}
