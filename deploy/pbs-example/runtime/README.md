# PBS runtime example

This directory owns nonsecret PBS configuration and the Compose shape. The image is pinned to Prebid Server Go v4.7.0 by digest.

The `/run/pbs/secrets/examplebidder.env` file is a runtime contract, not a checked-in credential file. A separately approved host-side loader must retrieve the selected regional Secrets Manager version, validate the complete JSON payload, render this restricted environment file atomically, and start or replace Compose. The current `ts prebid server` CLI does not implement that release or injection workflow.

`examples/pbs-secrets.env` contains dummy values for local Compose parsing only.
