# Regional PBS infrastructure module

This local module creates one regional ALB, two public and two private subnets, one NAT gateway per AZ, one private EC2 Compose host per AZ, Secrets Manager metadata, IAM access, and CloudWatch alarms.

It does not install software, write secret values, deploy PBS, or perform runtime replacement.
