//! The plain `OpenRTB` 2.6 demand implementation, `openrtb`.
//!
//! A standards-compliant exchange needs no code of its own. It takes the
//! shared request baseline, adds the static extensions its table sets, and has
//! its response read the ordinary `OpenRTB` way.

use core::any::Any;
use std::sync::Arc;

use async_trait::async_trait;
use error_stack::{Report, ResultExt as _};
use http::StatusCode;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::auction::demand::{
    CONSERVATIVE_LANGUAGE_MAX_BYTES, CompiledDemand, DemandFieldPolicy, DemandImplementation,
    DemandResponse, DemandTimeoutDefault, ProviderAuctionInput, RegsPolicy, accept_endpoint,
};
use crate::auction::openrtb::extract_standard_response;
use crate::auction::types::AuctionResponse;
use crate::error::TrustedServerError;
use crate::openrtb::OpenRtbRequest;
use crate::platform::PlatformResponse;

/// The implementation id `[demand]` names.
pub const OPENRTB_ID: &str = "openrtb";

/// The plain `OpenRTB` demand implementation.
pub static DEMAND: DemandImplementation = DemandImplementation {
    id: OPENRTB_ID,
    default_timeout: DemandTimeoutDefault::Auction,
    allows_all_eligible: true,
    serves_stored_requests: false,
    canonicalize_endpoint: accept_endpoint,
    compile,
};

const STATIC_EXTENSION_MAX_BYTES: usize = 16 * 1024;
const STATIC_EXTENSION_MAX_DEPTH: usize = 8;
const STATIC_EXTENSION_MAX_KEYS: usize = 256;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// A validated static extension object.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StaticExtension(Map<String, Value>);

impl StaticExtension {
    /// Borrow the validated extension object.
    #[must_use]
    pub fn as_object(&self) -> &Map<String, Value> {
        &self.0
    }
}

/// One compiled plain `OpenRTB` demand source.
#[derive(Debug, Clone, Default)]
pub struct OpenRtbDemand {
    /// Static request-level extension fields.
    pub request_ext: StaticExtension,
    /// Static impression-level extension fields.
    pub imp_ext: StaticExtension,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct OpenRtbSettings {
    #[serde(default)]
    request_ext: Option<Value>,
    #[serde(default)]
    imp_ext: Option<Value>,
}

fn compile(
    settings: &Map<String, Value>,
) -> Result<Arc<dyn CompiledDemand>, Report<TrustedServerError>> {
    let settings = OpenRtbSettings::deserialize(Value::Object(settings.clone())).map_err(|error| {
        configuration_error(format!("invalid `{OPENRTB_ID}` settings: {error}"))
    })?;
    Ok(Arc::new(OpenRtbDemand {
        request_ext: validate_static_extension("request_ext", settings.request_ext)?,
        imp_ext: validate_static_extension("imp_ext", settings.imp_ext)?,
    }))
}

#[async_trait(?Send)]
impl CompiledDemand for OpenRtbDemand {
    fn field_policy(&self) -> DemandFieldPolicy {
        DemandFieldPolicy {
            language_max_bytes: Some(CONSERVATIVE_LANGUAGE_MAX_BYTES),
            regs: RegsPolicy::Jurisdiction,
            accept_json: true,
            ..DemandFieldPolicy::default()
        }
    }

    fn augment_request(
        &self,
        request: &mut OpenRtbRequest,
        _input: &ProviderAuctionInput,
    ) -> Result<(), Report<TrustedServerError>> {
        request.ext = nonempty_map(self.request_ext.as_object().clone());
        for imp in &mut request.imp {
            imp.ext = nonempty_map(self.imp_ext.as_object().clone());
        }
        Ok(())
    }

    async fn parse_response(
        &self,
        context: DemandResponse<'_>,
        response: PlatformResponse,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        parse_openrtb_response(context, response).await
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Reads an ordinary `OpenRTB` response. A `204` is a no-bid, any other
/// non-success status or unreadable JSON is an error response, and a body over
/// the size limit is an `Err`.
///
/// # Errors
///
/// Returns an error when the response body cannot be read.
pub async fn parse_openrtb_response(
    context: DemandResponse<'_>,
    response: PlatformResponse,
) -> Result<AuctionResponse, Report<TrustedServerError>> {
    let provider_id = context.provider_id;
    let response_time_ms = context.response_time_ms;
    let response = response.response;
    let status = response.status();
    if status == StatusCode::NO_CONTENT {
        return Ok(AuctionResponse::no_bid(provider_id, response_time_ms));
    }
    if !status.is_success() {
        if status.is_redirection() {
            log::warn!(
                "Provider '{provider_id}' returned a redirect; generic OpenRTB redirects are refused"
            );
        }
        return Ok(AuctionResponse::error(provider_id, response_time_ms)
            .with_metadata("error_type", json!("http_status"))
            .with_metadata("http_status", json!(status.as_u16())));
    }

    let body = response
        .into_body()
        .into_bytes_bounded(MAX_RESPONSE_BYTES)
        .await
        .change_context(TrustedServerError::Auction {
            message: format!("Provider {provider_id} response body failed"),
        })?;
    let value: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => {
            log::warn!("Provider '{provider_id}' response JSON was invalid: {error}");
            return Ok(AuctionResponse::error(provider_id, response_time_ms)
                .with_metadata("error_type", json!("parse_response")));
        }
    };

    Ok(extract_standard_response(
        provider_id,
        context.input,
        &value,
        response_time_ms,
    ))
}

fn nonempty_map(value: Map<String, Value>) -> Option<Map<String, Value>> {
    (!value.is_empty()).then_some(value)
}

fn configuration_error(message: impl Into<String>) -> Report<TrustedServerError> {
    Report::new(TrustedServerError::Configuration {
        message: message.into(),
    })
}

fn validate_static_extension(
    field: &str,
    value: Option<Value>,
) -> Result<StaticExtension, Report<TrustedServerError>> {
    let Some(value) = value else {
        return Ok(StaticExtension::default());
    };
    let object = value
        .as_object()
        .ok_or_else(|| configuration_error(format!("`{OPENRTB_ID}` {field} must be an object")))?;
    let size = serde_json::to_vec(&value)
        .map_err(|error| configuration_error(format!("cannot serialize {field}: {error}")))?
        .len();
    if size > STATIC_EXTENSION_MAX_BYTES {
        return Err(configuration_error(format!(
            "`{OPENRTB_ID}` {field} exceeds {STATIC_EXTENSION_MAX_BYTES} bytes"
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
                    "`{OPENRTB_ID}` {field} exceeds nesting depth {STATIC_EXTENSION_MAX_DEPTH}"
                )));
            }
            if object.len() > STATIC_EXTENSION_MAX_KEYS {
                return Err(configuration_error(format!(
                    "`{OPENRTB_ID}` {field} object exceeds {STATIC_EXTENSION_MAX_KEYS} keys"
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
                    "`{OPENRTB_ID}` {field} exceeds nesting depth {STATIC_EXTENSION_MAX_DEPTH}"
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
            "`{OPENRTB_ID}` {field} cannot claim reserved field `{key}`"
        )));
    }
    Ok(())
}
