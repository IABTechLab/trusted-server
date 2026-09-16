module "east" {
  source = "./modules/regional"

  providers = {
    aws = aws
  }

  alarm_actions              = var.alarm_actions
  availability_zones         = var.east_availability_zones
  ami_id                     = var.east_ami_id
  certificate_arn            = var.east_certificate_arn
  instance_type              = var.instance_type
  log_retention_days         = var.log_retention_days
  name                       = "pbs-example-us-east-1"
  region                     = "us-east-1"
  secrets_kms_key_arn        = var.secrets_kms_key_arn
  tags                       = local.common_tags
  trusted_server_cidr_blocks = var.trusted_server_cidr_blocks
  vpc_cidr                   = var.east_vpc_cidr
}

module "west" {
  source = "./modules/regional"

  providers = {
    aws = aws.west
  }

  alarm_actions              = var.alarm_actions
  availability_zones         = var.west_availability_zones
  ami_id                     = var.west_ami_id
  certificate_arn            = var.west_certificate_arn
  instance_type              = var.instance_type
  log_retention_days         = var.log_retention_days
  name                       = "pbs-example-us-west-2"
  region                     = "us-west-2"
  secrets_kms_key_arn        = var.secrets_kms_key_arn
  tags                       = local.common_tags
  trusted_server_cidr_blocks = var.trusted_server_cidr_blocks
  vpc_cidr                   = var.west_vpc_cidr
}
