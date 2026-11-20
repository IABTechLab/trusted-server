variable "aws_account_id" {
  description = "Reviewed AWS account. The provider refuses another account."
  type        = string
  validation {
    condition     = can(regex("^[0-9]{12}$", var.aws_account_id))
    error_message = "Supply a 12-digit AWS account ID."
  }
}

variable "aws_region" {
  description = "Single approved commercial AWS region for both table and runtime."
  type        = string
}

variable "table_name" {
  description = "Stable retained table name, not an ephemeral workload/run name."
  type        = string
}

variable "namespace" {
  description = "Exclusive fresh app/environment record namespace. Retain its per-key allocators with the values."
  type        = string
  validation {
    condition     = can(regex("^[A-Za-z0-9_.-]+$", var.namespace))
    error_message = "Use an ASCII namespace containing only letters, digits, underscores, periods or hyphens so the IAM partition pattern stays literal."
  }
}

variable "owner" {
  description = "Named owner of data retention, allocator continuity, costs, state and approved decommissioning."
  type        = string
  validation {
    condition     = length(trimspace(var.owner)) > 0
    error_message = "An accountable retention owner is required."
  }
}
