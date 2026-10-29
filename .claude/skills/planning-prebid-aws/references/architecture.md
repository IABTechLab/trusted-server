# Architecture decisions

Select the smallest design that meets the approved requirements. Reuse an established AWS platform when it meets them; avoid introducing a second operations model just for PBS.

## Runtime and availability

| Requirement                                                        | Candidate                                              | Decision to resolve                                                                               |
| ------------------------------------------------------------------ | ------------------------------------------------------ | ------------------------------------------------------------------------------------------------- |
| Bounded pilot accepting host outages and maintenance interruptions | One EC2 host per selected region, Compose, Caddy, SSM  | Explicit outage acceptance, replacement procedure, and measured fixed headroom                    |
| Automatic host replacement with an existing EC2 operating model    | Launch template, Auto Scaling group, regional ALB      | Multi-AZ placement, minimum capacity, health checks, draining, and instance refresh policy        |
| Managed container lifecycle and rolling deployments                | ECS service with Fargate or EC2 capacity, regional ALB | Existing platform, resource fit, sustained cost, capacity provider, and rollout headroom          |
| Variable demand requiring elastic capacity                         | Scaling policy on the chosen runtime                   | Minimum/maximum capacity, signal, target, startup time, cooldown, quotas, and behavior at the cap |

A region containing one host is still a regional single point of failure. Host replacement, multi-AZ availability, regional failover, and zero-downtime releases each need their own evidence. ECS alone establishes none of these without the corresponding service configuration and capacity.

Use EKS only when the user's existing platform or an explicit requirement justifies Kubernetes ownership. Do not infer a need for orchestration from the word "production" alone.

## Network, ingress, and egress

| Need                                    | Candidate services or mechanism                                          | Required evidence                                                                                                                 |
| --------------------------------------- | ------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------- |
| Public HTTPS on standalone hosts        | Public subnet, internet gateway, Elastic IP, Caddy                       | Accepted exposure, durable certificate storage, renewal and replacement path                                                      |
| Managed HTTPS with multiple backends    | ALB and ACM                                                              | Certificate ownership/validation, target health, draining, timeout budget, private management endpoints                           |
| Private compute reaching public bidders | NAT-based egress or an approved existing egress service                  | AZ failure behavior, routing, hourly/data charges, and outbound address stability                                                 |
| Private PBS-to-bidder connectivity      | AWS RTB Fabric requester gateway and standard or outbound external links | Partner participation, gateway and link ownership, regional support, quotas, bidder endpoint mapping, timeout, fallback, and cost |
| Bidder source-IP allowlists             | Stable egress IPs                                                        | Every normal, scaling, and failover path uses partner-approved addresses                                                          |
| Existing caller selects regions         | Regional hostnames                                                       | Caller routing and failure policy, TLS, identity behavior                                                                         |
| DNS-based regional selection            | Route 53 latency records with per-region health checks                   | Independent health targets, cached-answer behavior, all-unhealthy behavior, and failover capacity                                 |
| Public auctions from untrusted callers  | Existing controls or WAF with compatible ingress                         | Auction payload compatibility, rate limits, false-positive testing, cost, and owner                                               |

Private subnets do not provide internet egress by themselves. VPC endpoints may serve supported AWS APIs but do not replace bidder internet access. Public addressing on replaceable compute does not by itself provide stable egress for allowlists.

For a shared hostname on standalone Caddy hosts, specify a workable multi-host certificate issuance method. Route 53 DNS challenges require the matching Caddy module, a pinned custom image, and scoped DNS permissions. Choose one routing/TLS design rather than generating all alternatives.

Define trusted proxy hops and client-IP handling, ingress ports, metrics isolation, outbound needs, and management access. Specify publisher/account admission and request limits for public auctions; CORS alone is not authorization. For EC2, use SSM without inbound SSH, require IMDSv2, encrypt disks, and separate host AWS access from application access. For ECS, distinguish the execution role from the application task role.

## Supporting services

For each item, record reuse, create, or not needed, with the reason:

- State storage and locking, independently owned from the deployment it records.
- Secrets Manager or an approved existing secret service, with regional access, rotation, and least-privilege permissions.
- Artifact storage, typically versioned S3 releases and an approved image registry such as ECR. Include checksums, retention, and regional startup independence where required.
- Logs, metrics, alarms, notification delivery, and budget alerts through CloudWatch or existing systems. Resolve metrics collection and dashboards, not just a log group.
- Stored-request/account storage and Prebid Cache only where the selected auction workload needs them. A generic Redis or database deployment is not automatically a PBS integration.

Alert on caller failures and latency, bidder behavior, capacity, resource exhaustion, and cost. Budget alerts are notifications, not a hard spending cap. Define data retention and redaction before recording auction diagnostics.

## Capacity and cost

Calculate from measured workload:

```text
pilot_peak_qps = eligible_peak_qps * allocation_fraction
regional_peak_qps = pilot_peak_qps * measured_regional_share
outbound_qps = pilot_peak_qps * measured_bidder_calls_per_auction
```

Use representative payloads and controlled bidder responses to establish throughput, tail latency, resource limits, and slow-bidder behavior. Include deployment overlap and the approved failure scenario in required headroom. If failover concentrates traffic, either prove surviving capacity or define how allocation is reduced.

Scaling needs a load-tested signal that predicts auction saturation, plus minimum capacity for bursts during startup. Consider CPU, memory, concurrency, connection limits, and outbound bandwidth. Never equate an instance count or CPU percentage with a measured QPS guarantee.

Estimate compute, storage, public IPv4, load balancing, NAT processing, internet/inter-AZ/inter-region transfer, DNS/health checks, secret retrieval, artifacts, and telemetry. State region, prices checked on, traffic assumptions, and uncertainty. Unknown volume means a conditional estimate, not a quoted total.

## Sources to verify for the selected branch

Consult current AWS documentation when recommending concrete settings:

- [ECS service autoscaling](https://docs.aws.amazon.com/AmazonECS/latest/developerguide/service-auto-scaling.html)
- [EC2 Auto Scaling](https://docs.aws.amazon.com/autoscaling/ec2/userguide/what-is-amazon-ec2-auto-scaling.html)
- [NAT gateway basics](https://docs.aws.amazon.com/vpc/latest/userguide/nat-gateway-basics.html)
- [Route 53 latency routing](https://docs.aws.amazon.com/Route53/latest/DeveloperGuide/routing-policy-latency.html)
- [Caddy Route 53 module](https://github.com/caddy-dns/route53)
- [AWS Pricing Calculator](https://calculator.aws/)
