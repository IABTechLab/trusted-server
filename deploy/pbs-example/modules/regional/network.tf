resource "aws_vpc" "main" {
  cidr_block           = var.vpc_cidr
  enable_dns_hostnames = true
  enable_dns_support   = true

  tags = merge(var.tags, {
    Name = "${var.name}-vpc"
  })
}

resource "aws_internet_gateway" "main" {
  vpc_id = aws_vpc.main.id

  tags = merge(var.tags, {
    Name = "${var.name}-igw"
  })
}

resource "aws_subnet" "public" {
  for_each = local.availability_zone_index

  vpc_id                  = aws_vpc.main.id
  availability_zone       = each.key
  cidr_block              = cidrsubnet(var.vpc_cidr, 4, each.value)
  map_public_ip_on_launch = false

  tags = merge(var.tags, {
    Name = "${var.name}-${each.key}-public"
    Tier = "public"
  })
}

resource "aws_subnet" "private" {
  for_each = local.availability_zone_index

  vpc_id            = aws_vpc.main.id
  availability_zone = each.key
  cidr_block        = cidrsubnet(var.vpc_cidr, 4, each.value + 8)

  tags = merge(var.tags, {
    Name = "${var.name}-${each.key}-private"
    Tier = "private"
  })
}

resource "aws_route_table" "public" {
  vpc_id = aws_vpc.main.id

  route {
    cidr_block = "0.0.0.0/0"
    gateway_id = aws_internet_gateway.main.id
  }

  tags = merge(var.tags, {
    Name = "${var.name}-public"
  })
}

resource "aws_route_table_association" "public" {
  for_each = local.availability_zone_index

  route_table_id = aws_route_table.public.id
  subnet_id      = aws_subnet.public[each.key].id
}

resource "aws_eip" "nat" {
  for_each = local.availability_zone_index

  domain = "vpc"

  tags = merge(var.tags, {
    Name = "${var.name}-${each.key}-nat"
  })
}

resource "aws_nat_gateway" "main" {
  for_each = local.availability_zone_index

  allocation_id = aws_eip.nat[each.key].id
  subnet_id     = aws_subnet.public[each.key].id

  depends_on = [aws_internet_gateway.main]

  tags = merge(var.tags, {
    Name = "${var.name}-${each.key}-nat"
  })
}

resource "aws_route_table" "private" {
  for_each = local.availability_zone_index

  vpc_id = aws_vpc.main.id

  route {
    cidr_block     = "0.0.0.0/0"
    nat_gateway_id = aws_nat_gateway.main[each.key].id
  }

  tags = merge(var.tags, {
    Name = "${var.name}-${each.key}-private"
  })
}

resource "aws_route_table_association" "private" {
  for_each = local.availability_zone_index

  route_table_id = aws_route_table.private[each.key].id
  subnet_id      = aws_subnet.private[each.key].id
}
