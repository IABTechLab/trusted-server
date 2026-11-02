//! Native S3 transport contract fixture for adapters without shipping asset routing.
//! Available only with `test-utils`; uses the loaded snapshot and the real exporter.

use std::sync::Arc;

use async_trait::async_trait;
use edgezero_core::body::Body;
use error_stack::{Report, ResultExt as _};
use http::{Method, Request, Response};
use serde::Deserialize;
use validator::Validate;

use crate::error::TrustedServerError;
use crate::integrations::{IntegrationEndpoint, IntegrationProxy, IntegrationRegistration};
use crate::platform::RuntimeServices;
use crate::settings::{IntegrationConfig, Settings};

pub(crate) const ID: &str = "secret_binding_runtime_fixture";
const PREFIX: &str = "/integrations/secret-binding-runtime";

#[derive(Deserialize, Validate)]
pub(crate) struct FixtureConfig {
    enabled: bool,
}

impl IntegrationConfig for FixtureConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

pub(crate) fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    if settings.integration_config::<FixtureConfig>(ID)?.is_none() {
        return Ok(None);
    }
    Ok(Some(
        IntegrationRegistration::builder(ID)
            .without_js()
            .with_proxy(Arc::new(Fixture))
            .build(),
    ))
}

struct Fixture;

fn invalid() -> Report<TrustedServerError> {
    Report::new(TrustedServerError::BadRequest {
        message: "invalid native S3 fixture path".to_owned(),
    })
}

#[async_trait(?Send)]
impl IntegrationProxy for Fixture {
    fn integration_name(&self) -> &'static str {
        ID
    }

    fn routes(&self) -> Vec<IntegrationEndpoint> {
        [Method::GET, Method::HEAD]
            .into_iter()
            .map(|method| IntegrationEndpoint::new(method, format!("{PREFIX}/*")))
            .collect()
    }

    async fn handle(
        &self,
        settings: &Settings,
        services: &RuntimeServices,
        mut req: Request<Body>,
    ) -> Result<Response<Body>, Report<TrustedServerError>> {
        let tail = req
            .uri()
            .path_and_query()
            .and_then(|path| path.as_str().strip_prefix(PREFIX))
            .ok_or_else(invalid)?;
        *req.uri_mut() = tail
            .parse()
            .change_context(TrustedServerError::BadRequest {
                message: "invalid native S3 fixture URI".to_owned(),
            })?;
        let route = settings
            .asset_route_for_path(req.uri().path())
            .ok_or_else(invalid)?;
        let asset =
            crate::proxy::handle_asset_proxy_request(settings, services, req, route).await?;
        let (mut response, stream) = asset.into_response_and_body();
        if let Some(body) = stream {
            *response.body_mut() = body;
        }
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::IntegrationRegistry;
    use crate::test_support::tests::create_test_settings;

    #[test]
    fn fixture_registers_get_and_head_only_when_enabled() {
        for enabled in [false, true] {
            let mut settings = create_test_settings();
            settings
                .integrations
                .insert_config(ID, &serde_json::json!({"enabled": enabled}))
                .expect("should configure native S3 fixture");
            let registry = IntegrationRegistry::new(&settings).expect("should register fixture");
            for method in [Method::GET, Method::HEAD] {
                assert_eq!(
                    registry.has_route(&method, &format!("{PREFIX}/assets/example.svg")),
                    enabled
                );
            }
            assert!(!registry.has_route(&Method::POST, &format!("{PREFIX}/assets/example.svg")));
        }
    }
}
