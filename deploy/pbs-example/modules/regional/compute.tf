resource "aws_instance" "pbs" {
  for_each = local.availability_zone_index

  ami                         = var.ami_id
  instance_type               = var.instance_type
  iam_instance_profile        = aws_iam_instance_profile.pbs.name
  subnet_id                   = aws_subnet.private[each.key].id
  vpc_security_group_ids      = [aws_security_group.pbs.id]
  associate_public_ip_address = false

  metadata_options {
    http_endpoint               = "enabled"
    http_protocol_ipv6          = "disabled"
    http_put_response_hop_limit = 1
    http_tokens                 = "required"
    instance_metadata_tags      = "disabled"
  }

  root_block_device {
    delete_on_termination = true
    encrypted             = true
    volume_size           = 30
    volume_type           = "gp3"
  }

  tags = merge(var.tags, {
    Name = "${var.name}-${each.key}-pbs"
  })

  depends_on = [aws_iam_role_policy.runtime]
}
