terraform {
  required_version = ">= 1.16.2, < 1.17.0"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "= 6.64.0"
    }
  }

  # Begin with a private path supplied at init. Migrate to S3 only after this
  # root creates the bucket. See README.md for both migration directions.
  backend "local" {}
}

provider "aws" {
  region              = var.aws_region
  allowed_account_ids = [var.aws_account_id]

  default_tags {
    tags = local.tags
  }
}

provider "aws" {
  alias               = "us_east_1"
  region              = "us-east-1"
  allowed_account_ids = [var.aws_account_id]

  default_tags {
    tags = local.tags
  }
}
