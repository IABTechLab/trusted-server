locals {
  common_tags = merge({
    Environment = "example"
    ManagedBy   = "Terraform"
    Project     = "prebid-server-example"
    Owner       = "example-team"
  }, var.tags)
}
