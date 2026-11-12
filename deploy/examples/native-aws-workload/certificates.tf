# Bootstrap requests certificates and publishes their validation CNAME without
# waiting, so external parent delegation can happen before this workload apply.
# These resources wait for issuance; they do not own or replace the certificates.
resource "aws_acm_certificate_validation" "regional" {
  certificate_arn = var.certificate_arn
}

resource "aws_acm_certificate_validation" "viewer" {
  count           = local.geo_enabled ? 1 : 0
  provider        = aws.us_east_1
  certificate_arn = var.geo_front_door.viewer_certificate_arn
}
