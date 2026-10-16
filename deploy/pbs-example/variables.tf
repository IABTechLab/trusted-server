variable "alarm_actions" {
  description = "SNS topic ARNs that should receive CloudWatch alarm notifications. Leave empty for this local example."
  type        = set(string)
  default     = []
}

variable "aws_account_id" {
  description = "The 12-digit AWS account expected by Terraform. Replace the fictional example value before any authorized plan."
  type        = string

  validation {
    condition     = can(regex("^[0-9]{12}$", var.aws_account_id))
    error_message = "aws_account_id must be a 12-digit AWS account ID."
  }
}

variable "aws_profile" {
  description = "The short-lived AWS CLI profile used by an authorized operator."
  type        = string
}

variable "east_ami_id" {
  description = "Pre-baked us-east-1 AMI containing the approved host runtime, SSM agent, Docker, and Compose."
  type        = string

  validation {
    condition     = can(regex("^ami-[0-9a-f]+$", var.east_ami_id))
    error_message = "east_ami_id must be an AMI ID."
  }
}

variable "east_certificate_arn" {
  description = "Existing ACM certificate ARN for the us-east-1 regional ALB."
  type        = string

  validation {
    condition     = can(regex("^arn:aws[a-z-]*:acm:us-east-1:[0-9]{12}:certificate/.+$", var.east_certificate_arn))
    error_message = "east_certificate_arn must be an ACM certificate ARN in us-east-1."
  }
}

variable "east_secrets_kms_key_arn" {
  description = "Optional us-east-1 customer-managed KMS key ARN for bidder secrets. Null uses the regional Secrets Manager service key."
  type        = string
  default     = null

  validation {
    condition     = var.east_secrets_kms_key_arn == null || can(regex("^arn:aws[a-z-]*:kms:us-east-1:${var.aws_account_id}:key/.+$", var.east_secrets_kms_key_arn))
    error_message = "east_secrets_kms_key_arn must be a KMS key ARN in us-east-1 and the declared AWS account."
  }
}

variable "east_vpc_cidr" {
  description = "CIDR block for the us-east-1 VPC."
  type        = string
  default     = "10.80.0.0/16"
}

variable "east_availability_zones" {
  description = "Two us-east-1 Availability Zones used by the ALB and PBS host."
  type        = list(string)
  default     = ["us-east-1a", "us-east-1b"]

  validation {
    condition     = length(var.east_availability_zones) == 2 && length(distinct(var.east_availability_zones)) == 2
    error_message = "east_availability_zones must contain two distinct Availability Zones."
  }
}

variable "instance_type" {
  description = "EC2 instance type for each PBS host. This is a starting assumption, not a capacity guarantee."
  type        = string
  default     = "c7i.large"
}

variable "pbs_hostname" {
  description = "Shared HTTPS hostname returned by Route 53 latency records."
  type        = string
  default     = "pbs.example.com"

  validation {
    condition     = !can(regex("[/\\s]", var.pbs_hostname))
    error_message = "pbs_hostname must be a hostname without a path or whitespace."
  }
}

variable "route53_zone_id" {
  description = "Existing Route 53 public hosted zone ID that owns pbs_hostname."
  type        = string
}

variable "tags" {
  description = "Additional tags applied to supported AWS resources."
  type        = map(string)
  default     = {}
}

variable "trusted_server_cidr_blocks" {
  description = "Approved Trusted Server egress CIDR blocks allowed to reach the public ALBs. The default is documentation-only."
  type        = set(string)
  default     = ["203.0.113.0/24"]

  validation {
    condition     = length(var.trusted_server_cidr_blocks) > 0 && alltrue([for cidr in var.trusted_server_cidr_blocks : can(cidrhost(cidr, 0))])
    error_message = "trusted_server_cidr_blocks must contain at least one valid CIDR block."
  }
}

variable "west_ami_id" {
  description = "Pre-baked us-west-2 AMI containing the approved host runtime, SSM agent, Docker, and Compose."
  type        = string

  validation {
    condition     = can(regex("^ami-[0-9a-f]+$", var.west_ami_id))
    error_message = "west_ami_id must be an AMI ID."
  }
}

variable "west_certificate_arn" {
  description = "Existing ACM certificate ARN for the us-west-2 regional ALB."
  type        = string

  validation {
    condition     = can(regex("^arn:aws[a-z-]*:acm:us-west-2:[0-9]{12}:certificate/.+$", var.west_certificate_arn))
    error_message = "west_certificate_arn must be an ACM certificate ARN in us-west-2."
  }
}

variable "west_secrets_kms_key_arn" {
  description = "Optional us-west-2 customer-managed KMS key ARN for bidder secrets. Null uses the regional Secrets Manager service key."
  type        = string
  default     = null

  validation {
    condition     = var.west_secrets_kms_key_arn == null || can(regex("^arn:aws[a-z-]*:kms:us-west-2:${var.aws_account_id}:key/.+$", var.west_secrets_kms_key_arn))
    error_message = "west_secrets_kms_key_arn must be a KMS key ARN in us-west-2 and the declared AWS account."
  }
}

variable "west_vpc_cidr" {
  description = "CIDR block for the us-west-2 VPC."
  type        = string
  default     = "10.81.0.0/16"
}

variable "west_availability_zones" {
  description = "Two us-west-2 Availability Zones used by the ALB and PBS host."
  type        = list(string)
  default     = ["us-west-2a", "us-west-2b"]

  validation {
    condition     = length(var.west_availability_zones) == 2 && length(distinct(var.west_availability_zones)) == 2
    error_message = "west_availability_zones must contain two distinct Availability Zones."
  }
}
