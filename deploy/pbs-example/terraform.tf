terraform {
  required_version = ">= 1.16.2, < 1.17.0"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "= 6.64.0"
    }
  }

  backend "local" {
    path = "terraform.tfstate"
  }
}
