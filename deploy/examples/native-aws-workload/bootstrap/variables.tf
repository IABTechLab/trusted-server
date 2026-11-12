variable "aws_account_id" {
  description = "Authorized AWS account. Both providers refuse another account."
  type        = string
  validation {
    condition     = can(regex("^[0-9]{12}$", var.aws_account_id))
    error_message = "Supply a 12-digit AWS account ID."
  }
}

variable "aws_region" {
  description = "Regional workload, state bucket, repository and secrets region."
  type        = string
}

variable "installation_name" {
  description = "Stable lowercase installation name used in resource names and all three state keys."
  type        = string
  validation {
    condition     = can(regex("^[a-z][a-z0-9-]*$", var.installation_name))
    error_message = "Use lowercase letters, digits and hyphens, starting with a letter."
  }
}

variable "owner" {
  description = "Named operator responsible for bootstrap resources and final retirement."
  type        = string
}

variable "state_bucket_name" {
  description = "Globally unique S3 bucket name for this installation's three Terraform roots."
  type        = string
}

variable "demo_hostname" {
  description = "Evaluation hostname, never the production publisher hostname. A new zone delegates exactly this name."
  type        = string
}

variable "existing_route53_zone_id" {
  description = "Optional existing public hosted zone covering demo_hostname. Null creates a dedicated evaluation child zone."
  type        = string
  default     = null
}
