mock_provider "aws" {
  override_during = plan

  mock_resource "aws_s3_bucket" {
    defaults = {
      id  = "example-staging-state"
      arn = "arn:aws:s3:::example-staging-state"
    }
  }
  mock_resource "aws_route53_zone" {
    defaults = {
      zone_id      = "Z0000000000001"
      name_servers = ["ns-1.example.net", "ns-2.example.net", "ns-3.example.org", "ns-4.example.org"]
    }
  }
  mock_data "aws_route53_zone" {
    defaults = {
      zone_id      = "Z0000000000002"
      name         = "example.com."
      private_zone = false
      name_servers = ["ns-5.example.net", "ns-6.example.net", "ns-7.example.org", "ns-8.example.org"]
    }
  }
  mock_resource "aws_acm_certificate" {
    defaults = {
      arn = "arn:aws:acm:eu-west-2:000000000000:certificate/00000000-0000-0000-0000-000000000001"
      domain_validation_options = [{
        domain_name           = "ts-demo.example.com"
        resource_record_name  = "_fictional.ts-demo.example.com."
        resource_record_type  = "CNAME"
        resource_record_value = "_fictional.acm-validations.aws."
      }]
    }
  }
}

mock_provider "aws" {
  alias           = "us_east_1"
  override_during = plan

  mock_resource "aws_acm_certificate" {
    defaults = {
      arn = "arn:aws:acm:us-east-1:000000000000:certificate/00000000-0000-0000-0000-000000000002"
      domain_validation_options = [{
        domain_name           = "ts-demo.example.com"
        resource_record_name  = "_fictional.ts-demo.example.com."
        resource_record_type  = "CNAME"
        resource_record_value = "_fictional.acm-validations.aws."
      }]
    }
  }
}

variables {
  aws_account_id    = "000000000000"
  aws_region        = "eu-west-2"
  installation_name = "example-staging"
  owner             = "example-operator"
  state_bucket_name = "example-staging-state"
  demo_hostname     = "ts-demo.example.com"
}

run "new_evaluation_zone" {
  command = plan

  assert {
    condition     = length(aws_route53_zone.demo) == 1 && length(data.aws_route53_zone.existing) == 0 && aws_route53_zone.demo[0].name == var.demo_hostname && output.route53_zone_id == aws_route53_zone.demo[0].zone_id && output.route53_name_servers == aws_route53_zone.demo[0].name_servers
    error_message = "A new installation must create only its evaluation zone and output the nameservers before any external delegation."
  }
  assert {
    condition     = !aws_s3_bucket.state.force_destroy && one(aws_s3_bucket_versioning.state.versioning_configuration).status == "Enabled" && one(one(aws_s3_bucket_server_side_encryption_configuration.state.rule).apply_server_side_encryption_by_default).sse_algorithm == "AES256" && one(aws_s3_bucket_ownership_controls.state.rule).object_ownership == "BucketOwnerEnforced"
    error_message = "State must be versioned, encrypted and bucket-owned; nonempty bucket removal remains explicit."
  }
  assert {
    condition     = aws_s3_bucket_public_access_block.state.block_public_acls && aws_s3_bucket_public_access_block.state.block_public_policy && aws_s3_bucket_public_access_block.state.ignore_public_acls && aws_s3_bucket_public_access_block.state.restrict_public_buckets
    error_message = "State must block all public ACL and bucket-policy access."
  }
  assert {
    condition = jsondecode(aws_s3_bucket_policy.state.policy) == {
      Version = "2012-10-17"
      Statement = [{
        Sid       = "DenyInsecureTransport"
        Effect    = "Deny"
        Principal = "*"
        Action    = "s3:*"
        Resource  = [aws_s3_bucket.state.arn, "${aws_s3_bucket.state.arn}/*"]
        Condition = { Bool = { "aws:SecureTransport" = "false" } }
      }]
    }
    error_message = "The bucket and every object must deny non-HTTPS access, without adding a public allow."
  }
  assert {
    condition     = aws_ecr_repository.application.image_tag_mutability == "IMMUTABLE" && !aws_ecr_repository.application.force_delete && one(aws_ecr_repository.application.encryption_configuration).encryption_type == "AES256" && one(aws_ecr_repository.application.image_scanning_configuration).scan_on_push && output.ecr_repository_name == "example-staging/application"
    error_message = "The installation repository must be encrypted, immutable and scanned, with explicit image cleanup."
  }
  assert {
    condition     = toset(keys(output.backend_configs)) == toset(["bootstrap", "retained", "workload"]) && length(toset([for config in values(output.backend_configs) : config.key])) == 3 && alltrue([for root, config in output.backend_configs : config.key == "${var.installation_name}/${root}.tfstate" && config.bucket == aws_s3_bucket.state.id && config.region == var.aws_region && config.encrypt && config.use_lockfile])
    error_message = "All three roots need separate installation-scoped, encrypted and locked S3 state keys."
  }
  assert {
    condition     = startswith(output.regional_certificate_arn, "arn:aws:acm:eu-west-2:") && startswith(output.viewer_certificate_arn, "arn:aws:acm:us-east-1:") && aws_acm_certificate.regional.domain_name == var.demo_hostname && aws_acm_certificate.viewer.domain_name == var.demo_hostname && aws_acm_certificate.regional.validation_method == "DNS" && aws_acm_certificate.viewer.validation_method == "DNS"
    error_message = "Regional ALB and us-east-1 viewer certificate requests must cover the same evaluation name."
  }
  assert {
    condition     = aws_route53_record.certificate_validation.zone_id == output.route53_zone_id && aws_route53_record.certificate_validation.type == "CNAME" && aws_route53_record.certificate_validation.name == one(aws_acm_certificate.regional.domain_validation_options).resource_record_name && toset(aws_route53_record.certificate_validation.records) == toset([one(aws_acm_certificate.regional.domain_validation_options).resource_record_value]) && one(aws_acm_certificate.regional.domain_validation_options).resource_record_name == one(aws_acm_certificate.viewer.domain_validation_options).resource_record_name
    error_message = "Both certificate requests share one Terraform-owned validation CNAME in the selected zone."
  }
  assert {
    condition     = toset(keys(output.application_secret_arns)) == toset(["handler_password", "publisher_proxy_secret", "ec_passphrase", "browser_forwarder_secret"]) && length(aws_secretsmanager_secret.application) == 4 && alltrue([for key, secret in aws_secretsmanager_secret.application : secret.name == "${var.installation_name}/${key}"]) && aws_secretsmanager_secret.agent.name == "${var.installation_name}/agent_token"
    error_message = "Bootstrap owns four application secret containers and one separate Agent token container, never values."
  }
}

run "existing_public_zone" {
  command = plan

  variables {
    existing_route53_zone_id = "Z0000000000002"
  }

  assert {
    condition     = length(aws_route53_zone.demo) == 0 && length(data.aws_route53_zone.existing) == 1 && data.aws_route53_zone.existing[0].zone_id == var.existing_route53_zone_id && output.route53_zone_id == var.existing_route53_zone_id && output.route53_name_servers == data.aws_route53_zone.existing[0].name_servers && aws_route53_record.certificate_validation.zone_id == var.existing_route53_zone_id
    error_message = "An existing-zone installation must read the zone and create only its validation record, never take zone ownership."
  }
}
