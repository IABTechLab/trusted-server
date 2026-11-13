terraform {
  required_version = ">= 1.16.2, < 1.17.0"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "= 6.64.0"
    }
  }

  # Supply a separately provisioned bucket, workload-specific key and region
  # at init. Never reuse retained/ state or another workload's state.
  backend "s3" {
    encrypt      = true
    use_lockfile = true
  }
}

provider "aws" {
  region              = var.aws_region
  allowed_account_ids = [var.aws_account_id]

  default_tags {
    tags = local.tags
  }
}

# This is a draft, not an operator-overridable assurance checkbox. Replace this
# implementation gate only when all listed qualifications have reviewed proof,
# including the scoped operator role and persistent S3 state with lockfile locking.
resource "terraform_data" "draft_gate" {
  input = {
    run_id = var.run_id
  }

  lifecycle {
    precondition {
      condition     = length(local.deployment_blockers) == 0 && var.run_id != ""
      error_message = "DRAFT ONLY. Unqualified: ${join(", ", sort(tolist(local.deployment_blockers)))}. No workload resource may be applied. ECS desired count zero is not teardown."
    }
    precondition {
      condition     = local.envelope_valid
      error_message = "Supply a CLI BlobEnvelope with version 1, a SHA-256 checksum field, the selected publisher domain and ec.storage exactly matching ec_storage. Trusted Server validates application policy and checksum."
    }
  }
}
