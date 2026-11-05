//! Mock mediator's request preparation and bound response interpretation.
use super::{AdServerMockConfig, AdServerMockProvider, BidIndex, build_bid_index};
use crate::auction::mediator::{
    BoundMediatorResponseParser, CompiledMediator, MediatorDefinition, MediatorPreparation,
    PreparedMediatorExchange,
};
use crate::auction::openrtb::{BidDimensionIndex, build_bid_dimension_index_from_slots};
use crate::auction::types::AuctionResponse;
use crate::error::TrustedServerError;
use crate::platform::PlatformResponse;
use async_trait::async_trait;
#[cfg(test)]
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt as _};
#[cfg(test)]
use http::header;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use validator::Validate as _;

pub(crate) const DEFINITION: MediatorDefinition = MediatorDefinition {
    id: "adserver_mock",
    compile,
};
fn compile(value: &Value) -> Result<Arc<dyn CompiledMediator>, Report<TrustedServerError>> {
    let config: AdServerMockConfig = serde_json::from_value(value.clone()).change_context(
        TrustedServerError::Configuration {
            message: "invalid adserver_mock mediator configuration".to_string(),
        },
    )?;
    config
        .validate()
        .change_context(TrustedServerError::Configuration {
            message: "invalid adserver_mock mediator configuration".to_string(),
        })?;
    Ok(Arc::new(AdServerMockProvider::new(config)))
}

impl CompiledMediator for AdServerMockProvider {
    fn id(&self) -> &str {
        "adserver_mock"
    }
    fn timeout_ms(&self) -> u32 {
        self.config.timeout_ms
    }
    fn prepare(
        &self,
        input: MediatorPreparation<'_>,
    ) -> Result<PreparedMediatorExchange, Report<TrustedServerError>> {
        log::info!(
            "AdServer Mock: mediating {} slots with {} bidder responses",
            input.request.slots.len(),
            input.provider_responses.len()
        );

        let request = self.build_mediation_http_request(input.request, input.provider_responses)?;

        let backend = crate::integration::integration_backend_spec(
            &self.config.endpoint,
            "adserver_mock",
            true,
            Duration::from_millis(u64::from(input.transport_budget_ms)),
        )?;
        Ok(PreparedMediatorExchange {
            request,
            backend,
            parser: Box::new(MockMediatorParser {
                provider: AdServerMockProvider::new(self.config.clone()),
                bid_index: build_bid_index(input.provider_responses),
                dimensions: build_bid_dimension_index_from_slots(&input.request.slots),
            }),
        })
    }
}

struct MockMediatorParser {
    provider: AdServerMockProvider,
    bid_index: BidIndex,
    dimensions: BidDimensionIndex,
}
#[async_trait(?Send)]
impl BoundMediatorResponseParser for MockMediatorParser {
    async fn parse(
        self: Box<Self>,
        response: PlatformResponse,
        response_time_ms: u64,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        self.provider
            .parse_response_inner(
                response,
                response_time_ms,
                &self.bid_index,
                Some(&self.dimensions),
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::test_support::canonical_parity_auction_request;

    #[tokio::test]
    async fn shared_http_builder_preserves_wire_and_host_ports() {
        for (endpoint, host) in [
            (
                "https://mediator.example.com/mediate",
                "mediator.example.com",
            ),
            (
                "http://mediator.example.com:8080/mediate",
                "mediator.example.com:8080",
            ),
        ] {
            let provider = AdServerMockProvider::new(AdServerMockConfig {
                enabled: true,
                endpoint: endpoint.to_string(),
                ..AdServerMockConfig::default()
            });
            let request = canonical_parity_auction_request();
            let responses = vec![
                AuctionResponse::success("z-provider", Vec::new(), 2),
                AuctionResponse::success("a-provider", Vec::new(), 1),
            ];
            let expected = serde_json::to_vec(
                &provider
                    .build_mediation_request(&request, &responses)
                    .expect("should build expected mediation payload"),
            )
            .expect("should serialize expected mediation payload");
            let outbound = provider
                .build_mediation_http_request(&request, &responses)
                .expect("should build shared HTTP request");
            assert_eq!(outbound.method(), http::Method::POST);
            assert_eq!(outbound.uri(), endpoint);
            assert_eq!(outbound.headers()[header::CONTENT_TYPE], "application/json");
            assert_eq!(outbound.headers()[header::HOST], host);
            let body = outbound
                .into_body()
                .into_bytes_bounded(1024 * 1024)
                .await
                .expect("should collect shared request bytes");
            assert_eq!(
                body.as_ref(),
                expected.as_slice(),
                "should preserve exact payload bytes and supplied provider order"
            );
        }
    }

    #[tokio::test]
    async fn prepared_mediator_preserves_wire_backend_and_non_success_metadata() {
        let provider = AdServerMockProvider::new(AdServerMockConfig {
            enabled: true,
            endpoint: "http://mediator.example.com:8080/mediate".to_string(),
            ..AdServerMockConfig::default()
        });
        let request = canonical_parity_auction_request();
        let responses = vec![
            AuctionResponse::no_bid("z-provider", 2),
            AuctionResponse::no_bid("a-provider", 1),
        ];
        let expected = serde_json::to_vec(
            &provider
                .build_mediation_request(&request, &responses)
                .expect("should build legacy wire value"),
        )
        .expect("should serialize legacy wire value");
        let exchange = provider
            .prepare(MediatorPreparation {
                request: &request,
                provider_responses: &responses,
                logical_budget_ms: 17,
                transport_budget_ms: 20,
            })
            .expect("should prepare neutral mediator");
        assert_eq!(exchange.backend.scheme, "http");
        assert_eq!(
            exchange.backend.discriminator.as_deref(),
            Some("adserver_mock")
        );
        assert_eq!(
            exchange.backend.first_byte_timeout,
            Duration::from_millis(20)
        );
        assert_eq!(
            exchange.backend.between_bytes_timeout,
            Duration::from_millis(20)
        );
        assert_eq!(
            exchange.request.headers()[header::HOST],
            "mediator.example.com:8080"
        );
        let body = exchange
            .request
            .into_body()
            .into_bytes_bounded(1024 * 1024)
            .await
            .expect("should collect mediation wire");
        assert_eq!(
            body.as_ref(),
            expected.as_slice(),
            "should retain exact legacy request bytes and supplied outcome order"
        );
        let response = PlatformResponse::new(
            http::Response::builder()
                .status(503)
                .body(EdgeBody::from("not JSON"))
                .expect("should build failed mediator response"),
        );
        let parsed = exchange
            .parser
            .parse(response, 9)
            .await
            .expect("should normalize HTTP failure without parsing body");
        assert_eq!(parsed.provider, "adserver_mock");
        assert!(parsed.bids.is_empty());
        assert_eq!(
            parsed.metadata["error_type"],
            serde_json::json!("http_status")
        );
        assert_eq!(parsed.metadata["http_status"], serde_json::json!(503));
    }
}
