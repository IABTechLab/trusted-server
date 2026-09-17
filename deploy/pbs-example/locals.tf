locals {
  common_tags = merge({
    Environment = "example"
    ManagedBy   = "Terraform"
    Project     = "prebid-server-example"
    Owner       = "example-team"
  }, var.tags)

  pbs_image = "prebid/prebid-server@sha256:f0fee9caab93e14e9988b376c2c5371412628b7de1eca7a663bf203c4dd530a7"

  deployment_descriptor = {
    schema_version = 1
    environment    = "example"
    runtime        = "ec2-compose"
    aws = {
      account_id = var.aws_account_id
      profile    = var.aws_profile
    }
    pbs = {
      config   = "runtime/pbs.yaml"
      image    = local.pbs_image
      bindings = "runtime/secret-bindings.generated.json"
    }
    regions = {
      us-east-1 = {
        instance_ids = values(module.east.instance_ids)
        overrides    = "runtime/regions/east.yaml"
      }
      us-west-2 = {
        instance_ids = values(module.west.instance_ids)
        overrides    = "runtime/regions/west.yaml"
      }
    }
  }

  secret_bindings = {
    examplebidder = {
      verified_image = local.pbs_image
      source         = "https://example.com/pbs-adapter-reference"
      secrets = {
        us-east-1 = module.east.secret_arns["examplebidder"]
        us-west-2 = module.west.secret_arns["examplebidder"]
      }
      keys = {
        api_key = {
          env      = "PBS_ADAPTERS_EXAMPLEBIDDER_API_KEY"
          pbs_path = ["adapters", "examplebidder", "api_key"]
          required = true
        }
        optional_token = {
          env      = "PBS_ADAPTERS_EXAMPLEBIDDER_OPTIONAL_TOKEN"
          pbs_path = ["adapters", "examplebidder", "optional_token"]
          required = false
        }
      }
    }
  }
}
