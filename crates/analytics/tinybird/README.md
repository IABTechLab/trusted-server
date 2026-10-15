# trusted-server-analytics-tinybird

Tinybird as an analytics module. It sends each auction's telemetry rows to
the Tinybird Events API as one NDJSON request, started and not waited for,
so a slow or failing Events API never delays a reader's page.

Select it with `[analytics] module = "tinybird"` and give it the regional
API host, the datasource and the name of the secret holding the
datasource's APPEND token in `[analytics.tinybird]`:

```toml
[analytics]
module = "tinybird"

[analytics.tinybird]
api_host = "api.us-east.aws.tinybird.co"
auction_dataset = "auction_events_raw"
auction_token_secret = "tinybird_auction_append_token"
```

Each adapter, and the `ts` tool, lists the module's builder among the
modules it links. The settings, what each one accepts and what is refused are
in the configuration guide under "Analytics Configuration", and the data path
in the telemetry guide.

Build and test from the repository root:

```bash
cargo test -p trusted-server-analytics-tinybird
cargo clippy -p trusted-server-analytics-tinybird --all-targets -- -D warnings
```
