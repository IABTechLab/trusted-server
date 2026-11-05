//! Observed schema-one outputs before the integrations extraction.
//!
//! Capture writes to an explicit scratch directory, never to the reviewed goldens.
//! Asset version hashes are recorded separately from behavior observations.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use edgezero_core::body::Body;
use edgezero_core::canonical_form::canonical_data_sha256;
use http::{HeaderMap, HeaderValue, Request, Response, header};
use serde_json::{Value, json};
use trusted_server_core::auction::compile_auction_plan;
use trusted_server_core::html_processor::{HtmlProcessorConfig, create_html_processor};
use trusted_server_core::integration::{
    AttributeRewriteOutcome, IntegrationAttributeContext, IntegrationRegistry,
};
use trusted_server_core::integrations::gpt_diagnostics;
use trusted_server_core::settings::Settings;
use trusted_server_core::streaming_processor::StreamProcessor as _;
use trusted_server_core::tsjs;

const SOURCE: &str = include_str!("../fixtures/integration_split_baseline/source.toml");
const DOCUMENT: &str = include_str!("../fixtures/integration_split_baseline/document.html");
const URL_CASES: &str =
    include_str!("../../trusted-server-js/lib/test/fixtures/script-url-ownership.json");

#[path = "integration_split_baseline/adapters.rs"]
mod adapters;
#[path = "integration_split_baseline/auction.rs"]
mod auction;
#[path = "integration_split_baseline/cli.rs"]
mod cli;

struct Observation {
    name: &'static str,
    bytes: Vec<u8>,
}

fn sorted_json(value: Value) -> Value {
    match value {
        Value::Object(entries) => Value::Object(
            entries
                .into_iter()
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .map(|(key, value)| (key, sorted_json(value)))
                .collect(),
        ),
        Value::Array(entries) => Value::Array(entries.into_iter().map(sorted_json).collect()),
        value => value,
    }
}

fn json_bytes(value: Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(&sorted_json(value))
        .expect("should serialize a baseline observation");
    bytes.push(b'\n');
    bytes
}

fn normalize_asset_versions(mut html: String, registry: &IntegrationRegistry) -> String {
    let immediate = tsjs::tsjs_script_src(&registry.js_module_ids_immediate());
    let (path, _) = immediate
        .rsplit_once("?v=")
        .expect("should identify the emitted bundle version query");
    html = html.replace(&immediate, &format!("{path}?v=@immediate-artifact@"));
    for id in trusted_server_js::all_module_ids() {
        let source = tsjs::tsjs_single_module_script_src(id);
        let (path, _) = source
            .rsplit_once("?v=")
            .expect("should identify the emitted module version query");
        html = html.replace(&source, &format!("{path}?v=@artifact:{id}@"));
    }
    html
}

fn attribute_action(outcome: AttributeRewriteOutcome) -> Value {
    match outcome {
        AttributeRewriteOutcome::Unchanged => json!({"action": "keep"}),
        AttributeRewriteOutcome::Replaced(value) => json!({"action": "replace", "value": value}),
        AttributeRewriteOutcome::RemoveElement => json!({"action": "remove"}),
    }
}

fn headers_json(headers: &HeaderMap) -> Value {
    let fields: BTreeMap<&str, Vec<&str>> = headers
        .keys()
        .map(|name| {
            let values = headers
                .get_all(name)
                .iter()
                .map(|value| value.to_str().expect("should emit sanitized ASCII headers"))
                .collect();
            (name.as_str(), values)
        })
        .collect();
    json!(fields)
}

fn request_decisions(registry: &IntegrationRegistry) -> Value {
    let cases: [(&str, &str, &[u8], &str); 7] = [
        ("inactive", "?section=example", b"keep=one", "text/html"),
        (
            "activate",
            "?ts_console=1&section=example",
            b"keep=one",
            "text/html",
        ),
        (
            "deactivate",
            "?ts_console=0",
            b"__Host-ts-console=1; keep=one",
            "text/html",
        ),
        (
            "ambiguous-query",
            "?ts_console=1&ts_console=0",
            b"__Host-ts-console=1",
            "text/html",
        ),
        (
            "duplicate-cookie",
            "",
            b"__Host-ts-console=1; keep=one; __Host-ts-console=1",
            "text/html",
        ),
        (
            "malformed-neighbor",
            "",
            b"bad=\xff; keep=one; __Host-ts-console=1; token=abc==",
            "text/html",
        ),
        (
            "non-navigation",
            "?ts_console=1",
            b"keep=one",
            "application/json",
        ),
    ];
    let mut rows = Vec::new();
    for enabled in [false, true] {
        let source = SOURCE.replace(
            "[integrations.gpt_diagnostics]\nenabled = true",
            &format!("[integrations.gpt_diagnostics]\nenabled = {enabled}"),
        );
        let settings =
            Settings::from_toml(&source).expect("should parse each diagnostics enablement case");
        for (id, query, cookie, accept) in cases {
            let mut request = Request::builder()
                .uri(format!("https://edge.example.com/article{query}"))
                .header(header::ACCEPT, accept)
                .header(
                    header::COOKIE,
                    HeaderValue::from_bytes(cookie).expect("should construct cookie field bytes"),
                )
                .body(Body::empty())
                .expect("should construct a request-preparation input");
            let decision = gpt_diagnostics::prepare_request(&settings, &mut request)
                .expect("should prepare diagnostics and sanitize cookies");
            let mut response = Response::builder()
                .header(header::CACHE_CONTROL, "public, max-age=3600")
                .body(Body::empty())
                .expect("should construct an origin response");
            gpt_diagnostics::finalize_response(&decision, &mut response);
            rows.push(json!({
                "id": id,
                "enabled": enabled,
                "active": decision.active(),
                "private": decision.requires_private_no_store(),
                "uri": request.uri().to_string(),
                "request_headers": headers_json(request.headers()),
                "response_headers": headers_json(response.headers()),
                "bootstrap": decision.bootstrap_script(),
                "module_script": decision.module_script_tag().map(|tag| normalize_asset_versions(tag, registry)),
            }));
        }
    }
    json!(rows)
}

fn trusted_script_attributes() -> Value {
    let rows: Vec<Value> = [(true, false), (false, true), (true, true)]
        .into_iter()
        .map(|(gam_attribution, aps_native)| {
            let mut source = SOURCE.to_owned();
            if gam_attribution {
                source = source.replace(
                    "[integrations.gpt]\nenabled = true",
                    "[integrations.gpt]\nenabled = true\ngam_attribution_enabled = true",
                );
            }
            if aps_native {
                source = source.replace(
                    "[integrations.aps]\nenabled = true",
                    "[integrations.aps]\nenabled = true\nrendering_mode = \"publisher_native\"",
                );
            }
            let settings = Settings::from_toml(&source)
                .expect("should parse each positive trusted-attribute variant");
            let plan = Arc::new(
                compile_auction_plan(&settings)
                    .expect("should compile each trusted-attribute plan"),
            );
            let registry = IntegrationRegistry::with_plan(&settings, plan)
                .expect("should construct each trusted-attribute variant");
            let attributes = registry.tsjs_script_tag_attributes();
            assert_eq!(
                attributes.len(),
                usize::from(gam_attribution) + usize::from(aps_native),
                "should activate every selected positive attribute case"
            );
            let tag = tsjs::tsjs_script_tag_with_attributes(
                &registry.js_module_ids_immediate(),
                &attributes,
            );
            let config = HtmlProcessorConfig::from_settings(
                &settings,
                &registry,
                "origin.example.com",
                "edge.example.com",
                "https",
            );
            let html = create_html_processor(config)
                .process_chunk(DOCUMENT.as_bytes(), true)
                .expect("should render actual trusted script attributes");
            let html = String::from_utf8(html).expect("should emit UTF-8 trusted-attribute HTML");
            assert!(
                html.contains(&tag),
                "should emit the complete publisher tag with its trusted attributes"
            );
            json!({
                "gam_attribution": gam_attribution,
                "aps_native": aps_native,
                "attributes": attributes,
                "publisher_tag": normalize_asset_versions(tag, &registry),
            })
        })
        .collect();
    json!(rows)
}

fn observations() -> (Vec<Observation>, Vec<u8>) {
    let settings = Settings::from_toml(SOURCE).expect("should parse the fixed baseline source");
    let plan = Arc::new(compile_auction_plan(&settings).expect("should compile the baseline plan"));
    let registry = IntegrationRegistry::with_plan(&settings, Arc::clone(&plan))
        .expect("should construct every configured baseline integration");
    let metadata: Vec<Value> = registry
        .registered_integrations()
        .into_iter()
        .map(|entry| {
            let routes: Vec<Value> = entry
                .routes
                .iter()
                .map(|route| json!({"method": route.method.as_str(), "path": route.path}))
                .collect();
            json!({
                "id": entry.id,
                "routes": routes,
                "attribute_rewriters": entry.attribute_rewriters,
                "script_selectors": entry.script_selectors,
                "head_injectors": entry.head_injectors,
                "request_filters": entry.request_filters,
            })
        })
        .collect();
    let providers: Vec<Value> = plan
        .providers()
        .iter()
        .map(|provider| {
            json!({
                "id": provider.id.as_str(),
                "profile": provider.profile.id(),
                "endpoint": provider.endpoint.as_str(),
                "timeout_ms": provider.timeout_ms,
            })
        })
        .collect();
    let settings_value = serde_json::to_value(&settings).expect("should serialize settings");
    let metadata = json!({
        "envelope_data_sha256": canonical_data_sha256(&settings_value),
        "integrations": metadata,
        "providers": providers,
        "mediator": plan.mediator(),
        "browser_modules": trusted_server_js::all_module_ids(),
        "immediate_modules": registry.js_module_ids_immediate(),
        "deferred_modules": registry.js_module_ids_deferred(),
        "trusted_script_attributes": registry.tsjs_script_tag_attributes(),
    });
    let cases: Vec<Value> =
        serde_json::from_str(URL_CASES).expect("should parse the shared source-reference inputs");
    let attributes: Vec<Value> = cases
        .iter()
        .map(|case| {
            let source = case["url"]
                .as_str()
                .expect("should have a source reference");
            let actions: Vec<Value> = [("script", "src"), ("link", "href")]
                .into_iter()
                .map(|(element_name, attribute_name)| {
                    let context = IntegrationAttributeContext {
                        attribute_name,
                        element_name,
                        request_host: case["requestHost"].as_str().unwrap_or("edge.example.com"),
                        request_scheme: case["requestScheme"].as_str().unwrap_or("https"),
                        origin_host: "origin.example.com",
                    };
                    attribute_action(registry.rewrite_attribute(attribute_name, source, &context))
                })
                .collect();
            json!({"id": case["id"], "actions": actions})
        })
        .collect();
    let mut results = vec![
        Observation {
            name: "registry.json",
            bytes: json_bytes(metadata),
        },
        Observation {
            name: "settings.json",
            bytes: json_bytes(settings_value),
        },
        Observation {
            name: "attributes.json",
            bytes: json_bytes(json!(attributes)),
        },
        Observation {
            name: "request-decisions.json",
            bytes: json_bytes(request_decisions(&registry)),
        },
        Observation {
            name: "trusted-attributes.json",
            bytes: json_bytes(trusted_script_attributes()),
        },
    ];
    for (name, diagnostics, suppress_datadome) in [
        ("document.html", false, false),
        ("diagnostics.html", true, false),
        ("suppressed-datadome.html", false, true),
    ] {
        let mut decision = diagnostics.then(|| {
            let mut request = Request::builder()
                .uri("https://edge.example.com/article?ts_console=1&section=example")
                .header("accept", "text/html")
                .body(Body::empty())
                .expect("should build a diagnostics navigation");
            let decision = gpt_diagnostics::prepare_request(&settings, &mut request)
                .expect("should prepare diagnostics activation");
            assert!(
                decision.active(),
                "should activate diagnostics in its baseline case"
            );
            trusted_server_core::integration::processing::RequestProcessingState::from(decision)
        });
        if suppress_datadome {
            let mut request = Request::new(Body::empty());
            trusted_server_core::integrations::datadome::suppress_client_tag(&mut request);
            decision = Some(
                trusted_server_core::integration::processing::RequestProcessingState::from_request(
                    &request,
                ),
            );
        }
        let config = HtmlProcessorConfig::from_settings(
            &settings,
            &registry,
            "origin.example.com",
            "edge.example.com",
            "https",
        )
        .with_request_processing(decision);
        let html = create_html_processor(config)
            .process_chunk(DOCUMENT.as_bytes(), true)
            .expect("should finish the baseline document");
        let html = String::from_utf8(html).expect("should produce UTF-8 baseline HTML");
        results.push(Observation {
            name,
            bytes: normalize_asset_versions(html, &registry).into_bytes(),
        });
    }
    let artifacts: BTreeMap<&str, Value> = trusted_server_js::all_module_ids()
        .iter()
        .map(|id| {
            let bundle = trusted_server_js::module_bundle(id)
                .expect("should embed each inventoried production browser module");
            (
                *id,
                json!({
                    "sha256": trusted_server_js::single_module_hash(id),
                    "bytes": bundle.len(),
                }),
            )
        })
        .collect();
    (results, json_bytes(json!(artifacts)))
}

async fn runtime_observations() -> (Vec<Observation>, Vec<u8>) {
    let (mut observations, artifacts) = observations();
    observations.extend(adapters::observations().await);
    observations.extend(auction::observations().await);
    (observations, artifacts)
}

fn golden_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/integration_split_baseline/observed")
}

fn assert_observation(expected: &[u8], observation: &Observation) {
    assert!(
        expected == observation.bytes,
        "should preserve corrected baseline observation {}",
        observation.name,
    );
}

#[tokio::test]
async fn preserves_corrected_schema_one_outputs() {
    for observation in runtime_observations().await.0 {
        let expected = fs::read(golden_directory().join(observation.name))
            .expect("should read a captured and reviewed baseline observation");
        assert_observation(&expected, &observation);
    }
}

#[tokio::test]
async fn every_golden_rejects_a_mutated_observation() {
    for mut observation in runtime_observations().await.0 {
        let expected = fs::read(golden_directory().join(observation.name))
            .expect("should read a captured and reviewed baseline observation");
        assert_observation(&expected, &observation);
        observation.bytes.push(0);
        let result = std::panic::catch_unwind(|| assert_observation(&expected, &observation));
        assert!(
            result.is_err(),
            "should detect a mutation to {}",
            observation.name
        );
    }
}

#[test]
fn repeated_source_parses_have_the_same_envelope_identity() {
    let expected = Settings::from_toml(SOURCE).expect("should parse baseline settings");
    let expected = canonical_data_sha256(
        &serde_json::to_value(expected).expect("should serialize baseline settings"),
    );
    for _ in 0..32 {
        let reparsed = Settings::from_toml(SOURCE).expect("should reparse baseline settings");
        let actual = canonical_data_sha256(
            &serde_json::to_value(reparsed).expect("should serialize reparsed settings"),
        );
        assert_eq!(
            actual, expected,
            "should retain envelope identity across fresh map seeds"
        );
    }
}

#[test]
#[ignore = "run with a freshly built CLI via scripts/check-integration-split-baseline.sh"]
fn preserves_cli_read_models() {
    for mut observation in cli::observations() {
        let expected = fs::read(golden_directory().join(observation.name))
            .expect("should read captured CLI output");
        assert_observation(&expected, &observation);
        observation.bytes.push(0);
        let result = std::panic::catch_unwind(|| assert_observation(&expected, &observation));
        assert!(result.is_err(), "should detect a mutation to CLI output");
    }
}

#[tokio::test]
#[ignore = "capture actual production outputs to INTEGRATION_SPLIT_CAPTURE_DIR for review"]
async fn capture_corrected_baseline() {
    let directory = PathBuf::from(
        std::env::var_os("INTEGRATION_SPLIT_CAPTURE_DIR")
            .expect("should explicitly select a scratch capture directory"),
    );
    fs::create_dir_all(&directory).expect("should create the scratch capture directory");
    let (mut observations, artifacts) = runtime_observations().await;
    observations.extend(cli::observations());
    for observation in observations {
        fs::write(directory.join(observation.name), observation.bytes)
            .expect("should write an observed production output");
    }
    fs::write(directory.join("artifacts.json"), artifacts)
        .expect("should record artifact hashes separately from behavior goldens");
}
