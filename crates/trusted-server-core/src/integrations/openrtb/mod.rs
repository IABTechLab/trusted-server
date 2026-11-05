//! Standard `OpenRTB` profile owned by the built-in integration.

use crate::auction::openrtb::{OpenRtbBuildOutcome, extract_standard_response};
use crate::auction::profile::{
    BoundOpenRtbResponseParser, CompiledOpenRtbProfile, OpenRtbPreparation,
    OpenRtbProfileDefinition, PreparedOpenRtbExchange, PreparedOpenRtbOutcome,
    ProfileRoutingPolicy, ProfileTimeoutDefault, build_profile_request, no_impressions,
    outbound_request,
};
use crate::auction::routing::{ProviderAuctionInput, RoutedAuction};
use crate::auction::types::AuctionResponse;
use crate::error::TrustedServerError;
use crate::openrtb::OpenRtbRequest;
use crate::platform::PlatformResponse;
use async_trait::async_trait;
use error_stack::{Report, ResultExt as _};
use http::StatusCode;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::sync::Arc;

const STANDARD_PROFILE_ID: &str = "standard";
const STATIC_EXTENSION_MAX_BYTES: usize = 16 * 1024;
const STATIC_EXTENSION_MAX_DEPTH: usize = 8;
const STATIC_EXTENSION_MAX_KEYS: usize = 256;
const MAX_PLANNED_RESPONSE_BYTES: usize = 1024 * 1024;

pub(crate) const DEFINITION: OpenRtbProfileDefinition = OpenRtbProfileDefinition {
    id: STANDARD_PROFILE_ID,
    default_timeout: ProfileTimeoutDefault::Auction,
    routing_policy: ProfileRoutingPolicy {
        allows_all_eligible: true,
        recognizes_stored_requests: false,
        consumes_bidder_params: false,
    },
    compile: compile_standard,
    canonicalize_endpoint: |_| Ok(()),
};

/// Validated static extension object.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StaticExtension(Map<String, Value>);

impl StaticExtension {
    /// Borrow the validated extension object.
    #[must_use]
    pub fn as_object(&self) -> &Map<String, Value> {
        &self.0
    }
}

/// Compiled generic `OpenRTB` profile configuration.
#[derive(Debug, Clone, Default)]
pub struct StandardProfilePlan {
    /// Static request-level extension fields.
    pub request_ext: StaticExtension,
    /// Static impression-level extension fields.
    pub imp_ext: StaticExtension,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct StandardProfileConfig {
    #[serde(default)]
    request_ext: Option<Value>,
    #[serde(default)]
    imp_ext: Option<Value>,
}

fn validate_static_extension(
    field: &str,
    value: Option<Value>,
) -> Result<StaticExtension, Report<TrustedServerError>> {
    let Some(value) = value else {
        return Ok(StaticExtension::default());
    };
    let object = value.as_object().ok_or_else(|| {
        configuration_error(format!("standard profile {field} must be an object"))
    })?;
    let size = serde_json::to_vec(&value)
        .map_err(|error| configuration_error(format!("cannot serialize {field}: {error}")))?
        .len();
    if size > STATIC_EXTENSION_MAX_BYTES {
        return Err(configuration_error(format!(
            "standard profile {field} exceeds {STATIC_EXTENSION_MAX_BYTES} bytes"
        )));
    }
    validate_extension_value(field, &value, 0)?;
    reject_reserved_fields(field, object)?;
    Ok(StaticExtension(object.clone()))
}

fn validate_extension_value(
    field: &str,
    value: &Value,
    container_depth: usize,
) -> Result<(), Report<TrustedServerError>> {
    match value {
        Value::Object(object) => {
            let container_depth = container_depth + 1;
            if container_depth > STATIC_EXTENSION_MAX_DEPTH {
                return Err(configuration_error(format!(
                    "standard profile {field} exceeds nesting depth {STATIC_EXTENSION_MAX_DEPTH}"
                )));
            }
            if object.len() > STATIC_EXTENSION_MAX_KEYS {
                return Err(configuration_error(format!(
                    "standard profile {field} object exceeds {STATIC_EXTENSION_MAX_KEYS} keys"
                )));
            }
            for nested in object.values() {
                validate_extension_value(field, nested, container_depth)?;
            }
        }
        Value::Array(array) => {
            let container_depth = container_depth + 1;
            if container_depth > STATIC_EXTENSION_MAX_DEPTH {
                return Err(configuration_error(format!(
                    "standard profile {field} exceeds nesting depth {STATIC_EXTENSION_MAX_DEPTH}"
                )));
            }
            for nested in array {
                validate_extension_value(field, nested, container_depth)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn reject_reserved_fields(
    field: &str,
    object: &Map<String, Value>,
) -> Result<(), Report<TrustedServerError>> {
    let reserved: &[&str] = match field {
        "request_ext" => &["trusted_server"],
        _ => &[],
    };
    if let Some(key) = reserved.iter().find(|key| object.contains_key(**key)) {
        return Err(configuration_error(format!(
            "standard profile {field} cannot claim reserved field `{key}`"
        )));
    }
    Ok(())
}

fn configuration_error(message: impl Into<String>) -> Report<TrustedServerError> {
    Report::new(TrustedServerError::Configuration {
        message: message.into(),
    })
}

fn deserialize_profile<T>(id: &str, value: &Value) -> Result<T, Report<TrustedServerError>>
where
    T: for<'de> Deserialize<'de>,
{
    T::deserialize(value)
        .map_err(|error| configuration_error(format!("invalid `{id}` profile_config: {error}")))
}

fn compile_standard(
    value: &Value,
) -> Result<Arc<dyn CompiledOpenRtbProfile>, Report<TrustedServerError>> {
    let config: StandardProfileConfig = deserialize_profile(STANDARD_PROFILE_ID, value)?;
    Ok(Arc::new(StandardProfilePlan {
        request_ext: validate_static_extension("request_ext", config.request_ext)?,
        imp_ext: validate_static_extension("imp_ext", config.imp_ext)?,
    }))
}

impl CompiledOpenRtbProfile for StandardProfilePlan {
    fn id(&self) -> &'static str {
        STANDARD_PROFILE_ID
    }
    fn routing_policy(&self) -> ProfileRoutingPolicy {
        DEFINITION.routing_policy
    }

    fn specialize_request(
        &self,
        request: &mut OpenRtbRequest,
        _: &ProviderAuctionInput,
        _: &RoutedAuction,
    ) -> Result<(), Report<TrustedServerError>> {
        request.ext = (!self.request_ext.as_object().is_empty())
            .then(|| self.request_ext.as_object().clone());
        for imp in &mut request.imp {
            imp.ext =
                (!self.imp_ext.as_object().is_empty()).then(|| self.imp_ext.as_object().clone());
        }
        Ok(())
    }
    fn prepare(
        &self,
        context: OpenRtbPreparation<'_>,
    ) -> Result<PreparedOpenRtbOutcome, Report<TrustedServerError>> {
        let request = match build_profile_request(
            self,
            context.input,
            context.routed,
            context.logical_budget_ms,
            &context.finalization,
        )? {
            OpenRtbBuildOutcome::NoImpressions => return Ok(no_impressions()),
            OpenRtbBuildOutcome::Ready(request) => request,
        };
        let request = outbound_request(&request, context.provider.endpoint.as_str(), true)?;
        Ok(PreparedOpenRtbOutcome::Exchange(PreparedOpenRtbExchange {
            request,
            parser: Box::new(StandardResponseParser {
                provider_id: context.input.provider_id().as_str().to_string(),
                input: context.input.clone(),
            }),
        }))
    }
}

struct StandardResponseParser {
    provider_id: String,
    input: ProviderAuctionInput,
}

#[async_trait(?Send)]
impl BoundOpenRtbResponseParser for StandardResponseParser {
    async fn parse(
        self: Box<Self>,
        response: PlatformResponse,
        response_time_ms: u64,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        let response = response.response;
        let status = response.status();
        let input = &self.input;
        if status == StatusCode::NO_CONTENT {
            let parsed = AuctionResponse::no_bid(&self.provider_id, response_time_ms);
            return Ok(parsed);
        }
        if !status.is_success() {
            if status.is_redirection() {
                log::warn!(
                    "Provider '{}' returned a redirect; generic OpenRTB redirects are refused",
                    self.provider_id
                );
            }
            let parsed = AuctionResponse::error(&self.provider_id, response_time_ms)
                .with_metadata("error_type", json!("http_status"))
                .with_metadata("http_status", json!(status.as_u16()));
            return Ok(parsed);
        }

        let body = response
            .into_body()
            .into_bytes_bounded(MAX_PLANNED_RESPONSE_BYTES)
            .await
            .change_context(TrustedServerError::Auction {
                message: format!("Provider {} response body failed", self.provider_id),
            })?;
        let value: Value = match serde_json::from_slice(&body) {
            Ok(value) => value,
            Err(error) => {
                log::warn!(
                    "Provider '{}' response JSON was invalid: {}",
                    self.provider_id,
                    error
                );
                let parsed = AuctionResponse::error(&self.provider_id, response_time_ms)
                    .with_metadata("error_type", json!("parse_response"));
                return Ok(parsed);
            }
        };

        let parsed = extract_standard_response(&self.provider_id, input, &value, response_time_ms);
        Ok(parsed)
    }
}
