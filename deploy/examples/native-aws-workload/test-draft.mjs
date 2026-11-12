import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.dirname(fileURLToPath(import.meta.url));
const terraform = process.env.TERRAFORM_BIN || "terraform";
const data = path.resolve(root, process.env.TF_DATA_DIR || ".terraform");
const temporary = fs.mkdtempSync(
  path.join(os.tmpdir(), "ts-terraform-contract-"),
);
const sourceFiles = [
  "config.tf",
  "ecs.tf",
  "geo.tf",
  "iam.tf",
  "locals.tf",
  "network.tf",
  "outputs.tf",
  "terraform.tf",
  "variables.tf",
];
const env = {
  PATH: process.env.PATH,
  HOME: path.join(temporary, "home"),
  TMPDIR: temporary,
  LANG: "C.UTF-8",
  AWS_EC2_METADATA_DISABLED: "true",
  TF_IN_AUTOMATION: "1",
};
fs.mkdirSync(env.HOME);
function run(args, cwd, extra) {
  const result = spawnSync(terraform, args, {
    cwd,
    env: { ...env, ...extra },
    encoding: "utf8",
    timeout: 120_000,
    maxBuffer: 8 * 1024 * 1024,
  });
  process.stdout.write(result.stdout || "");
  process.stderr.write(result.stderr || "");
  assert.equal(
    result.error,
    undefined,
    "Terraform command must complete within the deadline",
  );
  assert.equal(result.status, 0, `Terraform ${args.join(" ")} failed`);
}
try {
  // Reject extra configuration without reading it. In particular, never pick up
  // operator overrides, auto tfvars or any state/backend files in this checkout.
  const configurations = fs
    .readdirSync(root)
    .filter((name) => name.endsWith(".tf") || name.endsWith(".tf.json"))
    .sort();
  assert.deepEqual(
    configurations,
    sourceFiles,
    "only reviewed public Terraform source belongs in the mock fixture",
  );
  const variables = fs.readFileSync(path.join(root, "variables.tf"), "utf8");
  const geoVariable = variables
    .split('variable "geo_front_door" {')[1]
    .split('variable "application_image" {')[0];
  assert.doesNotMatch(
    geoVariable,
    /\bdefault\s*=/,
    "Geo must require an explicit profile or null, never silently select Off",
  );
  assert.doesNotMatch(
    fs.readFileSync(path.join(root, "ecs.tf"), "utf8"),
    /^moved\s*\{/m,
    "The new public root has no historical service address to migrate",
  );
  let resources = 0;
  for (const file of sourceFiles) {
    const text = fs.readFileSync(path.join(root, file), "utf8");
    assert.doesNotMatch(
      text,
      /\bdeployment_blockers\b|\bdraft_gate\b/,
      `${file}: production source must not contain the unconditional draft gate`,
    );
    for (const match of text.matchAll(
      /^resource "(aws_[^"]+)" "[^"]+" \{([\s\S]*?)(?=^resource |$(?![\s\S]))/gm,
    )) {
      resources++;
      assert.notEqual(
        match[1],
        "aws_secretsmanager_secret_version",
        "secret values must not enter Terraform state",
      );
    }
  }
  assert.equal(
    resources,
    40,
    "all workload AWS resource blocks must retain the no-secret-values check",
  );
  assert.ok(
    fs.existsSync(path.join(data, "providers")),
    "initialize the public provider with terraform init -backend=false first",
  );
  const cliConfig = path.join(temporary, "mirror.tfrc");
  fs.writeFileSync(
    cliConfig,
    `provider_installation {\n  filesystem_mirror {\n    path = ${JSON.stringify(path.join(data, "providers"))}\n    include = ["registry.terraform.io/hashicorp/aws"]\n  }\n}\n`,
  );
  {
    // Plan unchanged public-source copies with fictional test variables, never
    // the checkout's backend, state, credentials or automatic var files.
    const fixture = path.join(temporary, "workload-contract");
    fs.mkdirSync(path.join(fixture, "tests"), { recursive: true });
    for (const file of [
      ...sourceFiles,
      ".terraform.lock.hcl",
      "example-envelope.json",
      "example-noindex-envelope.json",
    ])
      fs.copyFileSync(path.join(root, file), path.join(fixture, file));
    const testFile = "task-contract.tftest.hcl.fixture";
    let contract = fs.readFileSync(path.join(root, "tests", testFile), "utf8");
    assert.match(contract, /^mock_provider "aws"/);
    assert.doesNotMatch(
      contract,
      /command\s*=\s*apply/,
      "offline checks must not apply even mocked workload",
    );
    const geoInput = (phase = "staged", overrides = {}) => {
      const fields = {
        rollout_phase: phase,
        viewer_certificate_arn:
          "arn:aws:acm:us-east-1:000000000000:certificate/00000000-0000-0000-0000-000000000000",
        origin_credential_header_name: "x-fictional-origin-check",
        origin_credential_value:
          "fictional_only_00000000000000000000000000000000",
        cloudfront_ipv4_cidrs: ["192.0.2.0/24", "198.51.100.0/24"],
        ...overrides,
      };
      return `{${Object.entries(fields)
        .map(([key, value]) => `${key} = ${JSON.stringify(value)}`)
        .join("\n")}}`;
    };
    contract = contract.replace(
      'mock_provider "aws" {',
      `mock_provider "aws" {
      mock_data "aws_ec2_managed_prefix_list" {
        override_during = plan
        defaults = { id = "pl-0123456789abcdef0" }
      }
      mock_resource "aws_security_group" {
        override_during = plan
        defaults = { id = "sg-0123456789abcdef0" }
      }
      mock_resource "aws_lb" {
        override_during = plan
        defaults = { arn = "arn:aws:elasticloadbalancing:eu-west-2:000000000000:loadbalancer/app/review-example/0000000000000000", dns_name = "review-alb.example.com", zone_id = "Z0000000000001" }
      }
      mock_resource "aws_lb_target_group" {
        override_during = plan
        defaults = { arn = "arn:aws:elasticloadbalancing:eu-west-2:000000000000:targetgroup/review-example-geo/0000000000000000" }
      }
      mock_resource "aws_cloudfront_cache_policy" {
        override_during = plan
        defaults = { id = "00000000-0000-0000-0000-000000000001" }
      }
      mock_resource "aws_cloudfront_distribution" {
        override_during = plan
        defaults = { domain_name = "review-distribution.example.com", hosted_zone_id = "Z0000000000002" }
      }
    `,
    );
    const ecs = fs.readFileSync(path.join(root, "ecs.tf"), "utf8");
    assert.match(
      ecs,
      /resource "aws_ecs_service" "geo" \{[\s\S]*?depends_on\s*=\s*\[[^\]]*aws_vpc_security_group_egress_rule\.auction/,
      "Geo service must wait for the scoped auction egress rule",
    );
    const check = (condition, message) =>
      `assert {\ncondition = ${condition}\nerror_message = ${JSON.stringify(message)}\n}\n`;
    // Omit the new input for demo identity to exercise its actual default.
    const phases = ["staged", "origin-enabled", "dns-cutover"];
    for (const { preservePublisher, phase } of [false, true].flatMap(
      (preservePublisher) =>
        phases.map((phase) => ({ preservePublisher, phase })),
    )) {
      const forwardedHost = preservePublisher
        ? "var.publisher_hostname"
        : "var.demo_hostname";
      contract += `\nrun "geo_${phase.replaceAll("-", "_")}_${preservePublisher ? "publisher" : "demo_default"}" {
        command = plan
        variables {
          geo_front_door = ${geoInput(phase)}
          ${preservePublisher ? "preserve_publisher_hostname = true" : ""}
        }
        ${[
          check(
            `aws_cloudfront_distribution.geo[0].enabled == ${phase !== "staged"} && !aws_cloudfront_distribution.geo[0].is_ipv6_enabled && aws_cloudfront_distribution.geo[0].tags.RunId == var.run_id && output.deployment_metadata.geo.phase == "${phase}" && output.deployment_metadata.manual_shutdown_required`,
            "Traffic must be separate from opt-in; IPv6 is not enabled.",
          ),
          check(
            `length(aws_ecs_service.review) == 0 && aws_ecs_service.geo[0].desired_count == 1 && aws_ecs_service.geo[0].name == "review-example-geo" && aws_lb_target_group.geo[0].name == "review-example-geo" && one(aws_ecs_service.geo[0].load_balancer).target_group_arn == aws_lb_target_group.geo[0].arn`,
            "Old Off tasks must never join the credential-bearing target group.",
          ),
          check(
            `one(aws_lb_listener_rule.publisher.action).type == "fixed-response" && length(aws_vpc_security_group_ingress_rule.reviewers) == 0 && aws_vpc_security_group_ingress_rule.cloudfront[0].prefix_list_id == data.aws_ec2_managed_prefix_list.cloudfront[0].id`,
            "Geo opt-in must immediately remove the unprotected ALB bypass.",
          ),
          check(
            `length(aws_lb_listener_rule.geo[0].condition) == 2 && anytrue([for c in aws_lb_listener_rule.geo[0].condition : try(one(c.http_header).http_header_name == var.geo_front_door.origin_credential_header_name && toset(one(c.http_header).values) == toset([var.geo_front_door.origin_credential_value]), false)]) && anytrue([for c in aws_lb_listener_rule.geo[0].condition : try(toset(one(c.host_header).values) == toset([var.demo_hostname]), false)]) && one(aws_lb_listener_rule.geo[0].action).target_group_arn == aws_lb_target_group.geo[0].arn`,
            "Only exact credential plus demo Host may reach the isolated Geo targets.",
          ),
          check(
            `strcontains(local.native_geo_profile, "alb_peer_cidrs = ") && strcontains(local.native_geo_profile, var.geo_front_door.origin_credential_header_name) && !strcontains(local.native_geo_profile, var.geo_front_door.origin_credential_value) && !strcontains(aws_ecs_task_definition.review.container_definitions, var.geo_front_door.origin_credential_value) && !strcontains(local.native_geo_profile, "192.0.2.0/24") && strcontains(local.application_environment.EDGEZERO__ADAPTER__TRUSTED_PROXY_CIDRS, "192.0.2.0/24")`,
            "Geo ALB peers and ordinary proxy networks must stay distinct, and the credential value must never reach ECS.",
          ),
          check(
            `var.preserve_publisher_hostname == ${preservePublisher} && length(one(aws_cloudfront_distribution.geo[0].origin).custom_header) == 2 && anytrue([for h in one(aws_cloudfront_distribution.geo[0].origin).custom_header : h.name == "X-Forwarded-Host" && h.value == ${forwardedHost}]) && anytrue([for h in one(aws_cloudfront_distribution.geo[0].origin).custom_header : h.name == var.geo_front_door.origin_credential_header_name && h.value == var.geo_front_door.origin_credential_value])`,
            "Origin custom headers must be exactly the private credential and deployment-fixed forwarded Host override, never arbitrary viewer values or extra headers.",
          ),
          check(
            `toset(aws_cloudfront_distribution.geo[0].aliases) == toset([var.demo_hostname]) && one(aws_cloudfront_distribution.geo[0].viewer_certificate).acm_certificate_arn == var.geo_front_door.viewer_certificate_arn && aws_lb_listener.https.certificate_arn == var.certificate_arn && one(one(aws_lb_listener.https.default_action).fixed_response).status_code == "404"`,
            "Publisher browser identity must not change demo TLS/DNS identity or fail-closed ALB admission.",
          ),
          check(
            `one(aws_cloudfront_origin_request_policy.geo[0].headers_config).header_behavior == "allViewerAndWhitelistCloudFront" && toset(one(one(aws_cloudfront_origin_request_policy.geo[0].headers_config).headers).items) == toset(["CloudFront-Viewer-Country", "CloudFront-Viewer-Country-Region"]) && one(aws_cloudfront_origin_request_policy.geo[0].cookies_config).cookie_behavior == "all" && one(aws_cloudfront_origin_request_policy.geo[0].query_strings_config).query_string_behavior == "all"`,
            "Generated Geo, Host, Authorization, cookies and queries must be explicitly forwarded.",
          ),
          check(
            `aws_cloudfront_cache_policy.geo[0].min_ttl == 0 && aws_cloudfront_cache_policy.geo[0].default_ttl == 0 && aws_cloudfront_cache_policy.geo[0].max_ttl == 0 && length(aws_cloudfront_distribution.geo[0].custom_error_response) == 11 && alltrue([for e in aws_cloudfront_distribution.geo[0].custom_error_response : e.error_caching_min_ttl == 0])`,
            "No response or error may be cached between viewers.",
          ),
          check(
            `one(aws_cloudfront_distribution.geo[0].default_cache_behavior).viewer_protocol_policy == "https-only" && one(one(aws_cloudfront_distribution.geo[0].origin).custom_origin_config).origin_protocol_policy == "https-only" && one(aws_cloudfront_distribution.geo[0].origin).domain_name == aws_lb.review.dns_name && one(aws_route53_record.review.alias).evaluate_target_health == ${phase !== "dns-cutover"}`,
            "Both public hops require HTTPS; origin DNS must not recurse into the viewer alias.",
          ),
          check(
            `one(aws_route53_record.review.alias).name == ${phase === "dns-cutover" ? "aws_cloudfront_distribution.geo[0].domain_name" : "aws_lb.review.dns_name"}`,
            "DNS must cut over only after separate protected-origin acceptance.",
          ),
        ].join("\n")}
      }\n`;
    }
    contract += `\nrun "auction_egress_scoped" {
      command = plan
      variables { auction_egress = { cidr_ipv4 = "192.0.2.80/32", port = 8000 } }
      ${check('length(aws_vpc_security_group_egress_rule.auction) == 1 && aws_vpc_security_group_egress_rule.auction[0].security_group_id == aws_security_group.task.id && aws_vpc_security_group_egress_rule.auction[0].ip_protocol == "tcp" && aws_vpc_security_group_egress_rule.auction[0].from_port == 8000 && aws_vpc_security_group_egress_rule.auction[0].to_port == 8000 && aws_vpc_security_group_egress_rule.auction[0].cidr_ipv4 == "192.0.2.80/32"', "Auction egress must target only the owned task SG and exact TCP endpoint.")}
    }\n`;
    {
      // Keep the identity projection wired to the same plan-known local as all
      // credential-removal/admission consumers, never computed AppConfig data.
      const geo = fs.readFileSync(path.join(root, "geo.tf"), "utf8");
      assert.equal(
        [...geo.matchAll(/var\.geo_front_door\.origin_credential_header_name/g)]
          .length,
        1,
        "only the shared local may read the private removal name",
      );
      assert.match(
        geo,
        /http_header_name\s*=\s*local\.geo_credential_header_name/,
      );
      assert.match(geo, /name\s*=\s*local\.geo_credential_header_name/);
      assert.ok(geo.includes("jsonencode(local.geo_credential_header_name)"));
      for (const phase of [null, "staged", "origin-enabled", "dns-cutover"]) {
        const service = phase
          ? "aws_ecs_service.geo[0]"
          : "aws_ecs_service.review[0]";
        const targets = phase
          ? "[aws_lb_target_group.review, aws_lb_target_group.geo[0]]"
          : "[aws_lb_target_group.review]";
        contract += `\nrun "settings_${phase?.replaceAll("-", "_") || "off"}" {
          command = plan
          variables {
            ${phase ? `geo_front_door = ${geoInput(phase)}` : ""}
            task_cpu = 512
            task_memory = 3072
            log_retention_in_days = 30
            retain_logs_on_destroy = true
            app_shutdown_grace_seconds = 20
            container_stop_timeout_seconds = 45
            alb_idle_timeout_seconds = 120
            target_deregistration_delay_seconds = 25
            ecs_health_grace_seconds = 180
            health_interval_seconds = 20
            health_timeout_seconds = 6
            healthy_threshold = 3
            unhealthy_threshold = 4
            application_log_filter = "debug"
            agent_log_level = "info"
            extra_tags = { CostCenter = "fictional" }
          }
          ${check('aws_ecs_task_definition.review.cpu == "512" && aws_ecs_task_definition.review.memory == "3072" && alltrue([for c in local.containers : c.stopTimeout == 45]) && local.application_environment.EDGEZERO__ADAPTER__SHUTDOWN_GRACE_SECONDS == "20" && local.application_environment.RUST_LOG == "debug" && anytrue([for e in local.containers[1].environment : e.name == "LOG_LEVEL" && e.value == "info"])', "Task settings must be passed literally to both containers.")}
          ${check('aws_cloudwatch_log_group.review.retention_in_days == 30 && aws_cloudwatch_log_group.review.skip_destroy && aws_lb.review.idle_timeout == 120 && local.tags.CostCenter == "fictional" && local.tags.RunId == var.run_id', "Log lifecycle, idle timeout and nonreserved tags must be configurable.")}
          ${check(`${service}.health_check_grace_period_seconds == 180 && ${service}.desired_count == 1 && ${service}.deployment_minimum_healthy_percent == 0 && ${service}.deployment_maximum_percent == 100`, "Both service histories must use shared settings without changing stop-first semantics.")}
          ${check(`alltrue([for t in ${targets} : tonumber(t.deregistration_delay) == 25 && one(t.health_check).interval == 20 && one(t.health_check).timeout == 6 && one(t.health_check).healthy_threshold == 3 && one(t.health_check).unhealthy_threshold == 4 && one(t.health_check).path == "/health" && one(t.health_check).matcher == "200"])`, "Both target-group histories must use shared health settings.")}
          ${check(`issensitive(output.deployment_policy) && output.deployment_policy.run_id == var.run_id && output.deployment_policy.aws_account_id == var.aws_account_id && output.deployment_policy.aws_region == var.aws_region && output.deployment_policy.geo_enabled == ${Boolean(phase)} && output.deployment_policy.origin_credential_header_name == ${phase ? '"x-fictional-origin-check"' : "null"} && !contains(keys(output.deployment_policy), "origin_credential_value")`, "Sensitive policy must expose only known intended identity and never the credential value.")}
        }\n`;
      }
      contract += `\nrun "defaults_preserve_settings" {
        command = plan
        ${check('aws_ecs_task_definition.review.cpu == "1024" && aws_ecs_task_definition.review.memory == "2048" && aws_cloudwatch_log_group.review.retention_in_days == 7 && !aws_cloudwatch_log_group.review.skip_destroy && aws_lb.review.idle_timeout == 60 && tonumber(aws_lb_target_group.review.deregistration_delay) == 15 && one(aws_lb_target_group.review.health_check).interval == 15 && one(aws_lb_target_group.review.health_check).timeout == 5 && one(aws_lb_target_group.review.health_check).healthy_threshold == 2 && one(aws_lb_target_group.review.health_check).unhealthy_threshold == 2 && aws_ecs_service.review[0].health_check_grace_period_seconds == 90 && alltrue([for c in local.containers : c.stopTimeout == 30]) && local.application_environment.EDGEZERO__ADAPTER__SHUTDOWN_GRACE_SECONDS == "10" && local.application_environment.RUST_LOG == "warn" && anytrue([for e in local.containers[1].environment : e.name == "LOG_LEVEL" && e.value == "warn"])', "Extraction defaults must preserve the public workload settings.")}
      }\n`;
      for (const [name, variables, failures] of [
        ["cpu_pair", "task_cpu = 256\\ntask_memory = 3072", "var.task_memory"],
        ["cpu_unknown", "task_cpu = 1000", "var.task_cpu"],
        [
          "shutdown",
          "app_shutdown_grace_seconds = 30",
          "var.app_shutdown_grace_seconds",
        ],
        [
          "stop_timeout",
          "container_stop_timeout_seconds = 121",
          "var.container_stop_timeout_seconds",
        ],
        [
          "idle_timeout",
          "alb_idle_timeout_seconds = 0",
          "var.alb_idle_timeout_seconds",
        ],
        [
          "deregistration",
          "target_deregistration_delay_seconds = -1",
          "var.target_deregistration_delay_seconds",
        ],
        [
          "ecs_grace",
          "ecs_health_grace_seconds = -1",
          "var.ecs_health_grace_seconds",
        ],
        [
          "health_interval",
          "health_interval_seconds = 301",
          "var.health_interval_seconds",
        ],
        [
          "health_timeout",
          "health_timeout_seconds = 15",
          "var.health_timeout_seconds",
        ],
        ["health_threshold", "healthy_threshold = 11", "var.healthy_threshold"],
        [
          "unhealthy_threshold",
          "unhealthy_threshold = 1",
          "var.unhealthy_threshold",
        ],
        ["retention", "log_retention_in_days = 2", "var.log_retention_in_days"],
        ["tags", 'extra_tags = { RunId = "other" }', "var.extra_tags"],
        [
          "log_filter",
          'application_log_filter = ""',
          "var.application_log_filter",
        ],
        ["agent_level", 'agent_log_level = "verbose"', "var.agent_log_level"],
      ]) {
        contract += `\nrun "settings_reject_${name}" {
          command = plan
          variables {
            ${variables.replaceAll("\\n", "\n")}
          }
          expect_failures = [${failures}]
        }\n`;
      }
      contract += `\nrun "geo_header_maximum" {
        command = plan
        variables { geo_front_door = ${geoInput("staged", { origin_credential_header_name: `x-${"a".repeat(38)}` })} }
        ${check("length(output.deployment_policy.origin_credential_header_name) == 40", "ALB permits a 40-character credential header name.")}
      }\n`;
      contract += `\nrun "geo_reject_header_over_limit" {
        command = plan
        variables { geo_front_door = ${geoInput("staged", { origin_credential_header_name: `x-${"a".repeat(39)}` })} }
        expect_failures = [var.geo_front_door]
      }\n`;
      for (const [name, egress] of [
        ["broad_cidr", { cidr_ipv4: "0.0.0.0/0", port: 8000 }],
        ["subnet", { cidr_ipv4: "192.0.2.0/24", port: 8000 }],
        ["invalid_ipv4", { cidr_ipv4: "999.0.2.80/32", port: 8000 }],
        ["ipv6", { cidr_ipv4: "::1/128", port: 8000 }],
        ["noncanonical_ipv4", { cidr_ipv4: "192.000.2.80/32", port: 8000 }],
        ["fractional_port", { cidr_ipv4: "192.0.2.80/32", port: 8000.5 }],
        ["zero_port", { cidr_ipv4: "192.0.2.80/32", port: 0 }],
        ["large_port", { cidr_ipv4: "192.0.2.80/32", port: 65536 }],
      ]) {
        contract += `\nrun "auction_reject_${name}" {
          command = plan
          variables { auction_egress = { cidr_ipv4 = ${JSON.stringify(egress.cidr_ipv4)}, port = ${egress.port} } }
          expect_failures = [var.auction_egress]
        }\n`;
      }
      const defaultPhaseInput = geoInput().replace(
        'rollout_phase = "staged"\n',
        "",
      );
      contract += `\nrun "geo_phase_defaults_staged" {
        command = plan
        variables { geo_front_door = ${defaultPhaseInput} }
        ${check('local.geo_phase == "staged" && !aws_cloudfront_distribution.geo[0].enabled && issensitive(var.geo_front_door) && issensitive(local.native_geo_profile)', "Omitted phase must stage with no traffic, and private inputs/profile must stay sensitive.")}
      }\n`;
      for (const preservePublisher of [false, true]) {
        contract += `\nrun "explicit_off_unchanged_${preservePublisher ? "publisher" : "demo_default"}" {
          command = plan
          variables { ${preservePublisher ? "preserve_publisher_hostname = true" : ""} }
          ${check(`var.preserve_publisher_hostname == ${preservePublisher} && length(aws_cloudfront_distribution.geo) == 0 && length(aws_ecs_service.geo) == 0 && length(aws_ecs_service.review) == 1 && aws_ecs_service.review[0].desired_count == 1 && one(aws_lb_listener_rule.publisher.action).type == "forward" && !contains(keys(local.application_environment), "TS_NATIVE_GEO_PROFILE_TOML") && local.trusted_proxy_cidrs == local.alb_subnet_cidrs`, "Explicit null must preserve the old Off topology and runtime regardless of browser identity.")}
        }\n`;
      }
      for (const [name, table] of [
        ["too_short", "ab"],
        ["too_long", "a".repeat(256)],
        ["wildcard", "example-*"],
        ["question_mark", "example-?"],
        ["path", "example/table"],
        ["unicode", "example-\u00e9"],
      ]) {
        const envelope = JSON.parse(
          fs.readFileSync(path.join(root, "example-envelope.json"), "utf8"),
        );
        envelope.data.ec.storage.table = table;
        const file = `invalid-table-${name}.json`;
        fs.writeFileSync(path.join(fixture, file), JSON.stringify(envelope));
        contract += `\nrun "ec_storage_reject_${name}" {
          command = plan
          variables {
            ec_storage = {
              provider = "dynamodb"
              region = "eu-west-2"
              table = ${JSON.stringify(table)}
              namespace = "example-staging"
            }
            config_envelope_file = "${file}"
          }
          expect_failures = [var.ec_storage]
        }\n`;
      }
      for (const [name, overrides] of [
        ["invalid_phase", { rollout_phase: "invalid" }],
        ["unbounded_proxy_trust", { cloudfront_ipv4_cidrs: ["0.0.0.0/0"] }],
        [
          "wildcard_credential",
          { origin_credential_value: "fictional_credential_000000000000000*" },
        ],
        ["reserved_header", { origin_credential_header_name: "authorization" }],
        ...[
          "fictional-origin",
          "x-forwarded-fictional",
          "x-amz-fictional",
          "x-amzn-fictional",
          "x-edge-fictional",
          "x-real-ip",
          "x-request-id",
          "x-correlation-id",
          "X-Fictional-Origin",
          "cloudfront-fictional",
          "host",
        ].map((header, index) => [
          `dedicated_header_${index}`,
          { origin_credential_header_name: header },
        ]),
        [
          "wrong_viewer_region",
          {
            viewer_certificate_arn:
              "arn:aws:acm:eu-west-2:000000000000:certificate/00000000-0000-0000-0000-000000000000",
          },
        ],
      ])
        contract += `\nrun "geo_reject_${name}" {
        command = plan
        variables { geo_front_door = ${geoInput("staged", overrides)} }
        expect_failures = [var.geo_front_door]
      }\n`;
    }
    {
      // Synthetic envelopes exercise only Terraform's structural boundary. They
      // do not claim valid TS application policy or native feature support;
      // the matching real CLI remains mandatory before deployment.
      const baseEnvelope = JSON.parse(
        fs.readFileSync(path.join(root, "example-envelope.json"), "utf8"),
      );
      const applicationCases = [
        [
          "no_review_policy",
          (envelope) => {
            envelope.data.handlers = [];
            envelope.data.response_headers = {};
          },
        ],
        [
          "application_auth_regex",
          (envelope) => {
            envelope.data.handlers = [
              {
                path: "^/application-owned-route$",
                username: "fictional",
                password: "handler_password",
              },
            ];
          },
        ],
        [
          "native_signing",
          (envelope) => {
            envelope.data.request_signing = { enabled: true };
          },
        ],
        [
          "http_origin",
          (envelope) => {
            envelope.data.publisher.origin_url =
              "http://origin.publisher.example.com:8080";
          },
        ],
        [
          "application_owned_secret_consumers",
          (envelope) => {
            delete envelope.data.ec.passphrase;
            delete envelope.data.publisher.proxy_secret;
            envelope.data.handlers = [];
          },
        ],
      ];
      for (const [name, change] of applicationCases) {
        const envelope = structuredClone(baseEnvelope);
        change(envelope);
        const file = `application-${name}.json`;
        fs.writeFileSync(path.join(fixture, file), JSON.stringify(envelope));
        contract += `\nrun "application_boundary_${name}" {
          command = plan
          variables { config_envelope_file = "${file}" }
          ${check("local.envelope_valid && aws_appconfig_hosted_configuration_version.review.content == file(var.config_envelope_file)", "Terraform must preserve CLI bytes without imposing application policy.")}
        }\n`;
      }
      for (const [name, change] of [
        ...["provider", "region", "table", "namespace"].map((field) => [
          `storage_mismatch_${field}`,
          (envelope) => {
            envelope.data.ec.storage[field] = "different";
          },
        ]),
        [
          "storage_missing",
          (envelope) => {
            delete envelope.data.ec.storage;
          },
        ],
        [
          "storage_extra_field",
          (envelope) => {
            envelope.data.ec.storage.extra = "unexpected";
          },
        ],
        [
          "wrong_version",
          (envelope) => {
            envelope.version = 2;
          },
        ],
        [
          "invalid_checksum",
          (envelope) => {
            envelope.sha256 = "invalid";
          },
        ],
        [
          "wrong_publisher_binding",
          (envelope) => {
            envelope.data.publisher.domain = "other.example.com";
          },
        ],
      ]) {
        const envelope = structuredClone(baseEnvelope);
        change(envelope);
        const file = `invalid-${name}.json`;
        fs.writeFileSync(path.join(fixture, file), JSON.stringify(envelope));
        contract += `\nrun "envelope_reject_${name}" {
          command = plan
          variables { config_envelope_file = "${file}" }
          expect_failures = [aws_appconfig_hosted_configuration_version.review]
        }\n`;
      }
    }
    // Exercise the committed operator-facing defaults, not a parallel list of
    // fictional values that can drift away from terraform.tfvars.example.
    contract += `\nrun "committed_example_cloudfront" {
      command = plan
      variables {
        ${fs.readFileSync(path.join(root, "terraform.tfvars.example"), "utf8")}
      }
      ${check('local.geo_enabled && local.geo_phase == "dns-cutover" && aws_cloudfront_distribution.geo[0].enabled && length(aws_ecs_service.review) == 0 && length(aws_ecs_service.geo) == 1 && one(aws_route53_record.review.alias).name == aws_cloudfront_distribution.geo[0].domain_name && local.envelope_valid', "Committed example inputs must select protected CloudFront and its DNS alias, never silently default Off.")}
    }\n`;
    fs.writeFileSync(path.join(fixture, "tests/contract.tftest.hcl"), contract);
    const validation = fs.readFileSync(
      path.join(root, "tests/draft.tftest.hcl"),
      "utf8",
    );
    assert.match(validation, /^mock_provider "aws"/);
    assert.doesNotMatch(
      validation,
      /command\s*=\s*apply/,
      "offline validation checks must not apply even mocked workload",
    );
    fs.writeFileSync(
      path.join(fixture, "tests/validation.tftest.hcl"),
      validation,
    );
    for (const file of sourceFiles)
      assert.equal(
        fs.readFileSync(path.join(fixture, file), "utf8"),
        fs.readFileSync(path.join(root, file), "utf8"),
        `${file}: mock plans must exercise unchanged production source`,
      );
    const fixtureEnv = {
      TF_DATA_DIR: path.join(fixture, ".terraform"),
      TF_CLI_CONFIG_FILE: cliConfig,
    };
    run(
      [
        "init",
        "-backend=false",
        "-input=false",
        "-lockfile=readonly",
        "-no-color",
      ],
      fixture,
      fixtureEnv,
    );
    run(["validate", "-no-color"], fixture, fixtureEnv);
    run(["test", "-no-color"], fixture, fixtureEnv);
  }
  // Validate only allowlisted public retained sources in a fresh directory.
  // No retained state, credentials or operator tfvars are read.
  const retainedRoot = path.join(root, "retained");
  const retained = path.join(temporary, "retained");
  fs.mkdirSync(path.join(retained, "tests"), { recursive: true });
  const retainedSources = [
    "main.tf",
    "outputs.tf",
    "terraform.tf",
    "variables.tf",
  ];
  assert.deepEqual(
    fs
      .readdirSync(retainedRoot)
      .filter((name) => name.endsWith(".tf") || name.endsWith(".tf.json"))
      .sort(),
    retainedSources,
    "retained mock checks accept only reviewed public Terraform sources",
  );
  const retainedSource = retainedSources
    .map((file) => fs.readFileSync(path.join(retainedRoot, file), "utf8"))
    .join("\n");
  assert.deepEqual(
    [...retainedSource.matchAll(/^resource "([^"]+)" "([^"]+)"/gm)].map(
      (match) => [match[1], match[2]],
    ),
    [["aws_dynamodb_table", "ec"]],
    "retained state owns only the DynamoDB table, never IAM attachments or seed rows",
  );
  assert.doesNotMatch(
    retainedSource,
    /task_role|aws_iam|terraform_remote_state/,
  );
  assert.match(retainedSource, /prevent_destroy\s*=\s*true/);
  for (const file of [...retainedSources, ".terraform.lock.hcl"])
    fs.copyFileSync(path.join(retainedRoot, file), path.join(retained, file));
  fs.copyFileSync(
    path.join(retainedRoot, "tests/retained.tftest.hcl"),
    path.join(retained, "tests/retained.tftest.hcl"),
  );
  const retainedEnv = {
    TF_DATA_DIR: path.join(retained, ".terraform"),
    TF_CLI_CONFIG_FILE: cliConfig,
  };
  run(
    [
      "init",
      "-backend=false",
      "-input=false",
      "-lockfile=readonly",
      "-no-color",
    ],
    retained,
    retainedEnv,
  );
  run(["validate", "-no-color"], retained, retainedEnv);
  run(["test", "-no-color"], retained, retainedEnv);
  console.log(
    "PASS retained table-only layout, protected lifetime and storage outputs",
  );

  // Bootstrap uses the same credential-free mirror and unchanged source-copy
  // boundary. Its local backend and any operator migration override stay unused.
  const bootstrapRoot = path.join(root, "bootstrap");
  const bootstrap = path.join(temporary, "bootstrap");
  fs.mkdirSync(path.join(bootstrap, "tests"), { recursive: true });
  const bootstrapSources = [
    "main.tf",
    "outputs.tf",
    "terraform.tf",
    "variables.tf",
  ];
  assert.deepEqual(
    fs
      .readdirSync(bootstrapRoot)
      .filter(
        (name) =>
          (name.endsWith(".tf") || name.endsWith(".tf.json")) &&
          name !== "backend_override.tf",
      )
      .sort(),
    bootstrapSources,
    "bootstrap mock checks accept only reviewed public Terraform sources",
  );
  for (const file of [...bootstrapSources, ".terraform.lock.hcl"])
    fs.copyFileSync(path.join(bootstrapRoot, file), path.join(bootstrap, file));
  const bootstrapTest = fs.readFileSync(
    path.join(bootstrapRoot, "tests/bootstrap.tftest.hcl"),
    "utf8",
  );
  assert.doesNotMatch(
    bootstrapTest,
    /command\s*=\s*apply/,
    "bootstrap offline checks must not apply even mocked resources",
  );
  fs.writeFileSync(
    path.join(bootstrap, "tests/bootstrap.tftest.hcl"),
    bootstrapTest,
  );
  const bootstrapEnv = {
    TF_DATA_DIR: path.join(bootstrap, ".terraform"),
    TF_CLI_CONFIG_FILE: cliConfig,
  };
  run(
    [
      "init",
      "-backend=false",
      "-input=false",
      "-lockfile=readonly",
      "-no-color",
    ],
    bootstrap,
    bootstrapEnv,
  );
  run(["validate", "-no-color"], bootstrap, bootstrapEnv);
  run(["test", "-no-color"], bootstrap, bootstrapEnv);
  console.log(
    "PASS bootstrap ownership, state settings, single DNS validation record and empty secret containers",
  );

  console.log(
    `PASS ${resources} unchanged AWS resource blocks, valid mock plans and contract rejection cases without an unconditional draft gate`,
  );
} finally {
  fs.rmSync(temporary, { recursive: true, force: true });
}
