# Regional PBS infrastructure module

This local module creates one regional ALB, two public and two private subnets, one NAT gateway per AZ, one private EC2 Compose host per AZ, Secrets Manager metadata, IAM access, and CloudWatch alarms.

The ALB verifies client certificates when `client_ca_bundle` is set. The optional `connection_logs_bucket` enables connection logging independently. The `alb_tls_settings` output reports the listener mode and configured bucket locations. See the [PBS runbook](../../RUNBOOK.md#optional-alb-mutual-tls) for certificate and bucket requirements.

It does not install software, write secret values, deploy PBS, or perform runtime replacement.
