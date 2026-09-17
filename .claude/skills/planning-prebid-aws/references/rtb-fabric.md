# AWS RTB Fabric connectivity

Load this reference when a bidder partner offers AWS RTB Fabric connectivity or the deployment is considering it as an alternative to public/NAT egress.

## Decision boundary

For this PBS workflow, RTB Fabric is an optional outbound path from PBS to selected bidders. PBS is the requester. A partner's bidder is the responder.

- A requester gateway is colocated with the PBS VPC.
- A standard Fabric link connects that gateway to a partner responder gateway. The partner must provide its gateway ID and accept the request before traffic can use the link.
- An outbound external link targets a partner's public HTTP or HTTPS endpoint. AWS may use its Global Network or public Internet routing, so treat it as a different security, latency, and cost profile from a standard link.
- PBS still fans out auctions to its configured bidders. Fabric does not multiplex traffic to multiple partners. Plan one endpoint path per participating bidder.
- Keep the regional ALB or other approved PBS ingress path separate. Replacing it with Fabric requires a different responder or inbound design and separate approval.

Route only partner-approved bidder endpoints through Fabric. Keep the approved NAT or external path for bidders without Fabric participation until a reviewed migration removes that dependency.

## Interview record

For each bidder and PBS region, record:

| Decision | Required evidence |
| --- | --- |
| Participation | Partner name, responder gateway ID or public endpoint, AWS account owner, and acceptance owner |
| Link type | Standard internal link or outbound external link, with the selected region |
| PBS binding | Pinned PBS Go release, adapter endpoint field, config source, rollout owner, and link URL update procedure |
| Capacity | Peak transactions per second, bidder timeout, payload sizes, burst duration, and regional failover load |
| Fallback | Behavior while a link is requested, inactive, over quota, timing out, or unavailable |
| Operations | Link creation, partner acceptance, status checks, endpoint changes, deletion order, alerts, and recovery owner |
| Cost | Region, monthly sent transactions, payload distribution, no-bid volume, link count, external traffic, and estimate date |
| Privacy and security | OpenRTB fields, TLS mode, source-IP requirements, data residency, logging, and retention |

Gateway IDs, link state, quota increases, and partner acceptance are external facts. Ask before authenticated AWS inspection, naming the account, role, regions, and read scope. Do not treat a partner's willingness to participate as proof that its gateway, endpoint, adapter, or regional capacity is ready.

## Constraints to verify

AWS currently lists RTB Fabric in US East (N. Virginia), US West (Oregon), Singapore, Tokyo, Frankfurt, and Ireland. Verify regional availability again during design. Gateways are regional, so a multi-region PBS deployment needs a regional connectivity decision for every participating bidder.

The current default quotas include two gateways per account and region, two standard links per gateway, two outbound external links per gateway, one Availability Zone per gateway, 1,000 transactions per second per link, and a 1.5-second HTTP request timeout. Gateway and throughput quotas may be adjustable. The control-plane quota is 10 API requests per second per account and region and is not adjustable. Use the selected bidder deadline and measured traffic, not the defaults, as the capacity requirement.

RTB Fabric supports OpenRTB versions including 2.6 and does not support OpenRTB 3.0. Verify the selected PBS Go release, bidder adapter, headers, custom extensions, and TLS behavior against the partner contract.

AWS charges for sent RTB requests and responses according to region, traffic type, volume, and payload. No-bid traffic has its own pricing dimension, received traffic is not charged, and there is no Free Tier. Use the current pricing page or calculator. Do not copy rates from an example or assume that Fabric eliminates all NAT cost.

CloudWatch exposes RTB Fabric request count, success and failure count, HTTP status, forwarding and total latency, target counts, filtered transactions, and internal or external no-bid metrics. Use Sum for volume metrics and P90, P95, or P99 for latency metrics where the metric supports those statistics. Define alarms and retention with the rest of the PBS observability plan.

## Terraform and lifecycle

The AWS Labs Terraform module creates requester and responder gateways, links, and inbound external links through the AWS Cloud Control provider. Pin a reviewed module release. Its documentation currently shows `v0.3.0` as the pinned example.

Treat a standard partner link as a two-party lifecycle, not a normal single-account Terraform resource. The requester can create the request, but the partner must accept it. Keep acceptance, activation, and endpoint publication as separate operator-owned steps unless a separately approved automation owns both accounts.

The module documents these limitations:

- `http_responder_allowed` is immutable and may be absent from Cloud Control responses, which can cause Terraform state drift. The documented recovery is a one-time state cleanup and re-import. Changing the value requires link replacement.
- Link module configuration is currently supported from the requester side, not the responder side.
- Managed responder endpoints use EKS or EC2 Auto Scaling groups. The current Trusted Server descriptor supports `ec2-compose` with explicit instance IDs, so it does not establish RTB Fabric responder support.

Keep secret values out of link configuration, Terraform variables, state, and reports. Link IDs, gateway IDs, and endpoint URLs are deployment metadata, but still require the selected access controls and change owner.

## Sources

- [AWS RTB Fabric FAQ](https://aws.amazon.com/rtb-fabric/faqs/)
- [RTB Fabric concepts](https://docs.aws.amazon.com/rtb-fabric/latest/userguide/what-is-rtb-fabric.html)
- [Create links](https://docs.aws.amazon.com/rtb-fabric/latest/userguide/creating-rtb-links.html)
- [Create outbound external links](https://docs.aws.amazon.com/rtb-fabric/latest/userguide/creating-outbound-external-links.html)
- [RTB Fabric quotas](https://docs.aws.amazon.com/rtb-fabric/latest/userguide/rtb-fabric-quotas.html)
- [RTB Fabric metrics](https://docs.aws.amazon.com/rtb-fabric/latest/userguide/monitoring-cloudwatch-metrics.html)
- [AWS Prebid Server partner onboarding](https://docs.aws.amazon.com/solutions/latest/prebid-server-deployment-on-aws/use-the-solution.html)
- [AWS Labs Terraform module](https://github.com/awslabs/rtb-fabric-terraform-module)
