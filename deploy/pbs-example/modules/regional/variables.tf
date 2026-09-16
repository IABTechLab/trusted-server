variable "alarm_actions" {
  description = "CloudWatch alarm action ARNs."
  type        = set(string)
}

variable "ami_id" {
  description = "Pre-baked regional AMI containing the approved host runtime."
  type        = string
}

variable "availability_zones" {
  description = "Exactly two Availability Zones for the regional ALB and PBS hosts."
  type        = list(string)

  validation {
    condition     = length(var.availability_zones) == 2 && length(distinct(var.availability_zones)) == 2
    error_message = "availability_zones must contain exactly two distinct Availability Zones."
  }
}

variable "certificate_arn" {
  description = "Existing ACM certificate ARN for the regional ALB."
  type        = string
}

variable "instance_type" {
  description = "EC2 instance type for the regional PBS hosts."
  type        = string
}

variable "log_retention_days" {
  description = "CloudWatch Logs retention period."
  type        = number
}

variable "name" {
  description = "Stable name prefix for regional resources."
  type        = string
}

variable "pbs_port" {
  description = "Host port used by the PBS Compose service."
  type        = number
  default     = 8000

  validation {
    condition     = var.pbs_port >= 1 && var.pbs_port <= 65535
    error_message = "pbs_port must be a valid TCP port."
  }
}

variable "region" {
  description = "AWS region represented by this module instance."
  type        = string
}

variable "secrets_kms_key_arn" {
  description = "Optional customer-managed KMS key ARN for regional bidder secrets."
  type        = string
  default     = null
}

variable "tags" {
  description = "Common resource tags."
  type        = map(string)
}

variable "trusted_server_cidr_blocks" {
  description = "CIDR blocks permitted to reach the regional ALB."
  type        = set(string)
}

variable "vpc_cidr" {
  description = "Regional VPC CIDR block."
  type        = string
}
