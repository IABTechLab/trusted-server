terraform {
  required_version = ">= 1.16.2, < 1.17.0"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "= 6.64.0"
    }
  }

  # Supply a retained bucket, installation-specific key and region at init.
  # Never reuse the workload state key. The bucket and its access policy are
  # owned separately from this example and the ECS workload.
  backend "s3" {
    encrypt      = true
    use_lockfile = true
  }
}

provider "aws" {
  region              = var.aws_region
  allowed_account_ids = [var.aws_account_id]

  default_tags {
    tags = {
      Application = "trusted-server"
      Owner       = var.owner
      Namespace   = var.namespace
      Lifecycle   = "retained-ec-state"
    }
  }
}
