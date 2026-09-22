# PBS runtime example

This directory owns nonsecret PBS configuration and the Compose shape. The image is pinned to Prebid Server Go v4.7.0 by digest. Compose uses bounded local logs rather than implying that a CloudWatch log shipper is installed.

The `/run/pbs/secrets/examplebidder.env` file is a runtime contract, not a checked-in credential file. A separately approved host-side loader must retrieve the selected regional Secrets Manager version, validate the complete JSON payload, render this restricted environment file atomically, and start or replace Compose. The current `ts prebid server` CLI does not implement that release or injection workflow.

`secret-bindings.example.json` and `examples/pbs-secrets.env` contain fictional values for local checks only. An authorized Terraform apply can render the ignored `secret-bindings.generated.json` with actual regional secret ARNs, but never secret values.

Deployment Compose binds `0.0.0.0:8000` by default so the ALB can reach PBS through the host security group. `scripts/smoke-runtime.sh` forces `PBS_BIND_ADDRESS=127.0.0.1` and checked-in dummy inputs for local startup. `scripts/test-smoke-runtime.py` verifies both bindings and the smoke selectors with Compose rendering and fake commands only. Run both scripts from the parent example directory or use their repository-relative paths.
