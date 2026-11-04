//! Full routed HTML observations, not reconstructed expected documents.
//!
//! These cases request identity coding and exercise buffered and chunked origin
//! bodies. Only the parent's exact browser-asset version normalization is used.
//! Selected headers intentionally exclude minted cookies and build-version IDs.

use std::collections::BTreeMap;
use std::sync::Arc;

use edgezero_core::body::Body;
use edgezero_core::http::request_builder;
use http::HeaderMap;
use serde_json::{Value, json};
use tower::ServiceExt as _;
use trusted_server_adapter_axum::app::TrustedServerApp as AxumApp;
use trusted_server_adapter_cloudflare::app::TrustedServerApp as CloudflareApp;
use trusted_server_adapter_spin::app::TrustedServerApp as SpinApp;
use trusted_server_core::auction::compile_auction_plan;
use trusted_server_core::integrations::IntegrationRegistry;
use trusted_server_core::test_support::nextjs_auction;

use super::{Observation, json_bytes, normalize_asset_versions};

fn stable_headers(headers: &HeaderMap) -> Value {
    let values: BTreeMap<&str, Vec<&str>> = [
        "content-type",
        "content-encoding",
        "cache-control",
        "vary",
        "x-content-type-options",
    ]
    .into_iter()
    .map(|name| {
        let values = headers
            .get_all(name)
            .iter()
            .map(|value| {
                value
                    .to_str()
                    .expect("should retain ASCII response headers")
            })
            .collect();
        (name, values)
    })
    .collect();
    json!(values)
}

pub(super) async fn observations() -> Vec<Observation> {
    let mut rows = Vec::new();
    for composition in [false, true] {
        for streaming in [false, true] {
            let (case, settings, origin_html) = if composition {
                (
                    "nextjs-gtm-script-composition",
                    nextjs_auction::script_composition_settings(),
                    nextjs_auction::script_composition_html(),
                )
            } else {
                (
                    "1135-split-rsc-auction",
                    nextjs_auction::settings(),
                    nextjs_auction::origin_html(),
                )
            };
            let plan = Arc::new(
                compile_auction_plan(&settings).expect("should compile the routed fixture plan"),
            );
            let registry = IntegrationRegistry::with_plan(&settings, plan)
                .expect("should construct the routed fixture integrations");
            let client = Arc::new(nextjs_auction::NextJsAuctionOrigin::with_html_response(
                &origin_html,
                false,
                streaming,
            ));
            let services = nextjs_auction::services(Arc::clone(&client));
            let routers = [
                (
                    "axum",
                    AxumApp::routes_with_settings_and_services(settings.clone(), services.clone()),
                ),
                (
                    "cloudflare",
                    CloudflareApp::routes_with_settings_and_services(
                        settings.clone(),
                        services.clone(),
                    ),
                ),
                (
                    "spin",
                    SpinApp::routes_with_settings_and_services(settings, services),
                ),
            ];
            for (adapter, router) in routers {
                client.reset_auction_requests();
                let request = request_builder()
                    .method("GET")
                    .uri(format!(
                        "https://{}/article",
                        nextjs_auction::PUBLISHER_HOST
                    ))
                    .header("host", nextjs_auction::PUBLISHER_HOST)
                    .header("accept", "text/html")
                    .header("accept-encoding", "identity")
                    .body(Body::empty())
                    .expect("should build the baseline routed navigation");
                let response = router
                    .expect("should construct each actual adapter router")
                    .oneshot(request)
                    .await
                    .expect("should route the baseline navigation");
                let status = response.status().as_u16();
                let headers = stable_headers(response.headers());
                assert!(
                    response
                        .headers()
                        .get("content-encoding")
                        .is_none_or(|value| value == "identity"),
                    "should receive decoded identity-coded fixture HTML"
                );
                let bytes = response
                    .into_body()
                    .into_bytes_bounded(1024 * 1024)
                    .await
                    .expect("should consume the complete small fixture document");
                let html = String::from_utf8(bytes.to_vec())
                    .expect("should preserve full decoded UTF-8 HTML");
                assert_eq!(status, 200, "should route a successful fixture response");
                assert_eq!(
                    client.auction_requests(),
                    usize::from(!composition),
                    "should observe auction dispatch only in the auction fixture"
                );
                if composition {
                    nextjs_auction::assert_script_composition_response(&bytes, false);
                } else {
                    assert!(
                        html.contains("fixture-creative"),
                        "should deliver the actual auction creative"
                    );
                    assert!(
                        html.contains(&nextjs_auction::expected_rewritten_flight_header()),
                        "should retain the rewritten split Flight length"
                    );
                    assert!(
                        html.ends_with("</script></body></html>"),
                        "should deliver bids at the structural body close"
                    );
                }
                rows.push(json!({
                    "adapter": adapter,
                    "case": case,
                    "streaming_origin": streaming,
                    "status": status,
                    "headers": headers,
                    "auction_requests": client.auction_requests(),
                    "html": normalize_asset_versions(html, &registry),
                }));
            }
        }
    }
    vec![Observation {
        name: "adapter-runtime.json",
        bytes: json_bytes(json!(rows)),
    }]
}
