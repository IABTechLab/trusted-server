locals {
  availability_zone_index = {
    for index, availability_zone in var.availability_zones : availability_zone => index
  }

  bidder_secret_names = {
    examplebidder = "${var.name}/examplebidder"
  }
}
