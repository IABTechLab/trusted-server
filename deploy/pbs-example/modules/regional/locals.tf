locals {
  # Coupled to runtime/compose.yaml's host/container ports and runtime/pbs.yaml's
  # port. Changing this requires matching runtime edits and wiring checks.
  pbs_port = 8000

  availability_zone_index = {
    for index, availability_zone in var.availability_zones : availability_zone => index
  }

  bidder_secret_names = {
    examplebidder = "${var.name}/examplebidder"
  }
}
