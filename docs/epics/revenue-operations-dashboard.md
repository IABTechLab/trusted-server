# Epic: Trusted Server Revenue & Operations Dashboard

## Overview

A real-time publisher transparency dashboard providing visibility into ad monetization, third-party activity, and compliance health. Publishers gain unprecedented insight into what's happening on their pages—who's bidding, what's being called, and whether it's authorized.

## Business Value

- **Revenue Optimization**: Identify underperforming exchanges, timeout issues, and bid density gaps
- **Compliance Assurance**: Detect unauthorized calls, consent violations, and policy breaches
- **Operational Visibility**: Debug issues faster with real-time data on all ad-related activity
- **Vendor Accountability**: Hold partners accountable with data on their behavior

---

## Stories

### Story 1: Prebid Bid Event Instrumentation

**As a** publisher
**I want** to capture all Prebid bid events flowing through Trusted Server
**So that** I can analyze bidding behavior and revenue patterns

#### Acceptance Criteria

- [ ] Capture bid events with: timestamp, exchange, ad-slot, bid price, currency, creative URL, timeout status
- [ ] Tag winning bid vs losing bids per auction
- [ ] Emit structured JSON log events via Fastly Real-time Logging
- [ ] Support high-cardinality dimensions without performance degradation

#### Technical Tasks

- [ ] Define `BidEvent` struct in Rust with all required fields
- [ ] Instrument Prebid response parsing to extract bid data
- [ ] Configure Fastly log endpoint (BigQuery, S3, or Datadog)
- [ ] Add auction_id correlation to group bids per auction
- [ ] Benchmark logging overhead (<1ms per request)

#### Data Schema

```json
{
  "event_type": "prebid_bid",
  "timestamp": "2026-01-24T09:45:00Z",
  "auction_id": "uuid",
  "ad_slot": "top-banner",
  "exchange": "rubicon",
  "bid_price": 2.45,
  "currency": "USD",
  "creative_url": "https://...",
  "is_winner": true,
  "latency_ms": 120,
  "timed_out": false
}
```

---

### Story 2: Third-Party Domain Call Tracking

**As a** publisher
**I want** to see all external domains called during page rendering
**So that** I can audit vendor activity and detect unauthorized calls

#### Acceptance Criteria

- [ ] Track all outbound requests proxied through Trusted Server
- [ ] Capture: domain, full URL path, request type, initiator (script/pixel/etc)
- [ ] Aggregate call counts per page load (session/page correlation)
- [ ] Flag domains not on approved allowlist

#### Technical Tasks

- [ ] Instrument proxy request handlers to emit domain call events
- [ ] Add session/page_id correlation (via synthetic ID or request header)
- [ ] Create domain allowlist configuration in settings.toml
- [ ] Emit `unauthorized_domain` alert event when unknown domain called
- [ ] Track request timing (DNS, connect, TTFB, total)

#### Data Schema

```json
{
  "event_type": "domain_call",
  "timestamp": "2026-01-24T09:45:00Z",
  "page_id": "uuid",
  "synthetic_id": "abc123",
  "domain": "ads.example.com",
  "full_url": "https://ads.example.com/bid?id=123",
  "request_type": "xhr",
  "initiator": "prebid.js",
  "authorized": true,
  "latency_ms": 85
}
```

---

### Story 3: JavaScript/SDK Endpoint Tracking

**As a** publisher
**I want** to see what JavaScript endpoints SDKs are calling
**So that** I can understand vendor behavior and script activity

#### Acceptance Criteria

- [ ] Track JS file requests and API endpoints called by ad SDKs
- [ ] Identify which SDK initiated each call (Prebid, GAM, identity partners)
- [ ] Capture endpoint patterns (e.g., `/v1/auction`, `/sync`, `/pixel`)
- [ ] Count calls per SDK per page load

#### Technical Tasks

- [ ] Parse request paths to categorize endpoint types
- [ ] Build SDK fingerprinting logic (match known SDK URL patterns)
- [ ] Emit `sdk_call` events with SDK attribution
- [ ] Create SDK registry configuration for known vendors

#### Data Schema

```json
{
  "event_type": "sdk_call",
  "timestamp": "2026-01-24T09:45:00Z",
  "page_id": "uuid",
  "sdk_name": "prebid",
  "sdk_version": "8.51.0",
  "endpoint": "/v1/auction",
  "endpoint_category": "bidding",
  "domain": "prebid.adnxs.com",
  "latency_ms": 145
}
```

---

### Story 4: Consent & Compliance Monitoring

**As a** publisher
**I want** to track consent state and detect compliance violations
**So that** I can ensure GDPR/CCPA compliance and avoid regulatory risk

#### Acceptance Criteria

- [ ] Log consent state (TCF string, USP string) per request
- [ ] Flag calls made to tracking domains when consent not granted
- [ ] Track consent rate by geography
- [ ] Alert on potential violations (call to tracking domain without consent)

#### Technical Tasks

- [ ] Parse TCF/GPP consent strings in request flow
- [ ] Build tracking domain classification (analytics, advertising, functional)
- [ ] Cross-reference consent state with domain calls
- [ ] Emit `compliance_violation` events when unauthorized tracking detected
- [ ] Add geo detection for consent rate reporting

#### Data Schema

```json
{
  "event_type": "compliance_check",
  "timestamp": "2026-01-24T09:45:00Z",
  "page_id": "uuid",
  "consent_tcf": "CO...",
  "consent_usp": "1YNN",
  "geo_country": "DE",
  "tracking_authorized": true,
  "violation_detected": false,
  "violation_domain": null
}
```

---

### Story 5: Log Streaming Infrastructure

**As a** platform operator
**I want** to configure real-time log streaming to a data warehouse
**So that** events can be aggregated and visualized

#### Acceptance Criteria

- [ ] Support multiple log destinations (BigQuery, S3, Datadog, Kafka)
- [ ] Configure via settings.toml or environment variables
- [ ] Structured JSON output with consistent schema
- [ ] Batching and compression for efficiency
- [ ] <5 second latency from event to availability

#### Technical Tasks

- [ ] Implement Fastly Real-time Log Streaming configuration
- [ ] Add log destination settings to configuration schema
- [ ] Create log formatter for consistent JSON output
- [ ] Test with BigQuery and S3 endpoints
- [ ] Document setup for each supported destination

#### Configuration Example

```toml
[observability.logging]
enabled = true
destination = "bigquery"
dataset = "trusted_server_events"
batch_size = 100
flush_interval_ms = 1000

[observability.logging.bigquery]
project_id = "my-gcp-project"
service_account_key_secret = "BIGQUERY_SA_KEY"
```

---

### Story 6: Dashboard MVP (Grafana)

**As a** publisher
**I want** a visual dashboard showing key metrics
**So that** I can monitor revenue and operations at a glance

#### Acceptance Criteria

- [ ] Real-time bid activity panel (bids/min by exchange)
- [ ] Win rate by exchange (pie/bar chart)
- [ ] Top domains called (table with counts)
- [ ] Consent rate by geo (map or bar chart)
- [ ] Compliance violation alerts (list)
- [ ] Filterable by time range, ad-slot, exchange

#### Technical Tasks

- [ ] Set up Grafana instance (or use Grafana Cloud)
- [ ] Configure Prometheus/BigQuery as data source
- [ ] Build dashboard JSON template
- [ ] Create saved queries for each panel
- [ ] Document dashboard import process

#### Dashboard Panels

1. **Bid Activity** - Time series of bids/minute by exchange
2. **Revenue Metrics** - Avg bid price, win rate, timeout rate
3. **Domain Calls** - Top 20 domains, authorized vs unauthorized
4. **SDK Activity** - Calls by SDK, endpoint category breakdown
5. **Compliance** - Consent rate gauge, violation count, geo map
6. **Alerts** - Recent compliance violations, unauthorized domains

---

### Story 7: Alerting Rules

**As a** publisher
**I want** automated alerts for anomalies and violations
**So that** I can respond quickly to issues

#### Acceptance Criteria

- [ ] Alert when exchange win rate drops >20% from baseline
- [ ] Alert on compliance violation detected
- [ ] Alert when unauthorized domain called
- [ ] Alert when bid timeout rate exceeds threshold
- [ ] Configurable notification channels (Slack, email, PagerDuty)

#### Technical Tasks

- [ ] Define alerting rules in Prometheus/Grafana
- [ ] Configure notification channels
- [ ] Set baseline thresholds (configurable per publisher)
- [ ] Create alert runbook documentation
- [ ] Test alert delivery end-to-end

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                        Browser / Page                           │
└─────────────────────────────────────────────────────────────────┘
                                │
                                ▼
┌─────────────────────────────────────────────────────────────────┐
│                    Trusted Server (Fastly)                      │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────────┐  │
│  │ Bid Parser  │  │ Domain      │  │ Consent                 │  │
│  │ & Logger    │  │ Tracker     │  │ Validator               │  │
│  └──────┬──────┘  └──────┬──────┘  └───────────┬─────────────┘  │
│         │                │                     │                │
│         └────────────────┼─────────────────────┘                │
│                          ▼                                      │
│              ┌───────────────────────┐                          │
│              │  Structured Event     │                          │
│              │  Emitter (JSON logs)  │                          │
│              └───────────┬───────────┘                          │
└──────────────────────────┼──────────────────────────────────────┘
                           │
                           ▼
              ┌───────────────────────┐
              │  Fastly Real-time     │
              │  Log Streaming        │
              └───────────┬───────────┘
                          │
         ┌────────────────┼────────────────┐
         ▼                ▼                ▼
   ┌──────────┐    ┌──────────┐    ┌──────────┐
   │ BigQuery │    │ S3/Athena│    │ Datadog  │
   └────┬─────┘    └────┬─────┘    └────┬─────┘
        │               │               │
        └───────────────┼───────────────┘
                        ▼
              ┌───────────────────────┐
              │  Grafana Dashboard    │
              │  + Alerting           │
              └───────────────────────┘
```

---

## Success Metrics

| Metric                  | Target                               |
| ----------------------- | ------------------------------------ |
| Event capture latency   | <5 seconds from request to dashboard |
| Log completeness        | >99.9% of events captured            |
| Dashboard query latency | <2 seconds for common queries        |
| Alert delivery time     | <1 minute from trigger               |

---

## Dependencies

- Fastly Real-time Log Streaming enabled on account
- GCP BigQuery or AWS S3 for log storage
- Grafana instance (self-hosted or Cloud)
- Prometheus (optional, for Fastly Exporter metrics)

---

## Open Questions

1. **Data retention**: How long should bid-level data be retained? (Cost vs utility)
2. **Sampling**: Should we sample high-volume events or capture 100%?
3. **Multi-tenant**: Should dashboard support multiple publisher views?
4. **PII handling**: Do any captured fields require anonymization?

---

## Timeline Estimate

| Phase                    | Stories     | Estimate      |
| ------------------------ | ----------- | ------------- |
| Phase 1: Instrumentation | Stories 1-4 | 3-4 weeks     |
| Phase 2: Infrastructure  | Story 5     | 1-2 weeks     |
| Phase 3: Visualization   | Stories 6-7 | 2-3 weeks     |
| **Total**                |             | **6-9 weeks** |

---

## References

- [Fastly Real-time Log Streaming](https://www.fastly.com/documentation/guides/integrations/logging/)
- [Fastly Exporter for Prometheus](https://github.com/fastly/fastly-exporter)
- [Grafana Dashboard for Fastly](https://grafana.com/grafana/dashboards/8951-fastly-dashboard/)
- [Prebid Analytics Adapter](https://docs.prebid.org/dev-docs/integrate-with-the-prebid-analytics-api.html)
