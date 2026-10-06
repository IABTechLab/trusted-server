provider "aws" {
  profile             = var.aws_profile
  region              = "us-east-1"
  allowed_account_ids = [var.aws_account_id]

  default_tags {
    tags = local.common_tags
  }
}

provider "aws" {
  alias               = "west"
  profile             = var.aws_profile
  region              = "us-west-2"
  allowed_account_ids = [var.aws_account_id]

  default_tags {
    tags = local.common_tags
  }
}
