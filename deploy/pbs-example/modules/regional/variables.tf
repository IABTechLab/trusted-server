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

variable "name" {
  description = "Stable name prefix for regional resources, leaving room for the ALB and target-group suffixes."
  type        = string

  validation {
    condition     = can(regex("^[a-zA-Z0-9]([a-zA-Z0-9-]{0,26}[a-zA-Z0-9])?$", var.name)) && !startswith(var.name, "internal-")
    error_message = "name must be 1-28 alphanumeric/hyphen characters, must not start or end with a hyphen, and must not use the reserved ALB prefix internal-."
  }
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
