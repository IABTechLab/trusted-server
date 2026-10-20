//! The plain `OpenRTB` 2.6 demand implementation, `auction-protocol.openrtb`.
//!
//! A standards-compliant exchange needs no code of its own. It takes the
//! shared request baseline, adds the static extensions its table sets, and has
//! its response read the ordinary `OpenRTB` way.

#![cfg_attr(
    test,
    allow(
        clippy::print_stdout,
        clippy::print_stderr,
        clippy::panic,
        clippy::dbg_macro,
        clippy::unwrap_used,
        reason = "tests use direct diagnostics and panic-on-failure helpers"
    )
)]

use core::any::Any;
use std::sync::Arc;

use async_trait::async_trait;
use error_stack::Report;
use serde::Deserialize;
use serde_json::{Map, Value};

use trusted_server_core::auction::demand::{
    CONSERVATIVE_LANGUAGE_MAX_BYTES, CompiledDemand, DemandFieldPolicy, DemandImplementation,
    DemandResponse, DemandTimeoutDefault, ProviderAuctionInput, RegsPolicy, RequestExtensions,
    accept_endpoint,
};
use trusted_server_core::auction::openrtb::parse_openrtb_response;
use trusted_server_core::auction::types::AuctionResponse;
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::platform::PlatformResponse;

/// The name an `implementation` line gives this implementation, its module
/// path, which also serves as its builder's id.
pub const MODULE: &str = "auction-protocol.openrtb";

/// The plain `OpenRTB` demand implementation.
pub static DEMAND: DemandImplementation = DemandImplementation {
    id: MODULE,
    default_timeout: DemandTimeoutDefault::Auction,
    allows_all_eligible: true,
    serves_stored_requests: false,
    canonicalize_endpoint: accept_endpoint,
    compile,
};

/// The builder a deployment hands to an adapter, which offers the plain
/// `OpenRTB` implementation to `[demand]`.
#[must_use]
pub fn builder() -> trusted_server_core::integrations::IntegrationBuilder {
    trusted_server_core::integrations::IntegrationBuilder::implementations(
        MODULE,
        env!("CARGO_PKG_NAME"),
    )
    .with_demand(&DEMAND)
}

const STATIC_EXTENSION_MAX_BYTES: usize = 16 * 1024;
const STATIC_EXTENSION_MAX_DEPTH: usize = 8;
const STATIC_EXTENSION_MAX_KEYS: usize = 256;

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
    let settings = OpenRtbSettings::deserialize(Value::Object(settings.clone()))
        .map_err(|error| configuration_error(format!("invalid `{MODULE}` settings: {error}")))?;
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
        extensions: &mut RequestExtensions<'_>,
        _input: &ProviderAuctionInput,
    ) -> Result<(), Report<TrustedServerError>> {
        *extensions.request = nonempty_map(self.request_ext.as_object().clone());
        for impression in &mut extensions.impressions {
            *impression.ext = nonempty_map(self.imp_ext.as_object().clone());
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
        .ok_or_else(|| configuration_error(format!("`{MODULE}` {field} must be an object")))?;
    let size = serde_json::to_vec(&value)
        .map_err(|error| configuration_error(format!("cannot serialize {field}: {error}")))?
        .len();
    if size > STATIC_EXTENSION_MAX_BYTES {
        return Err(configuration_error(format!(
            "`{MODULE}` {field} exceeds {STATIC_EXTENSION_MAX_BYTES} bytes"
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
                    "`{MODULE}` {field} exceeds nesting depth {STATIC_EXTENSION_MAX_DEPTH}"
                )));
            }
            if object.len() > STATIC_EXTENSION_MAX_KEYS {
                return Err(configuration_error(format!(
                    "`{MODULE}` {field} object exceeds {STATIC_EXTENSION_MAX_KEYS} keys"
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
                    "`{MODULE}` {field} exceeds nesting depth {STATIC_EXTENSION_MAX_DEPTH}"
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
            "`{MODULE}` {field} cannot claim reserved field `{key}`"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value, json};
    use trusted_server_core::auction::plan::AuctionPlan;
    use trusted_server_core::auction::test_support::{
        build_for_first_source, canonical_parity_auction_request, demand_table,
        deterministic_signer, golden_inbound_request, golden_plan_config, plan_config_with,
    };
    use trusted_server_core::error::TrustedServerError;
    use trusted_server_core::openrtb::OpenRtbRequest;
    use trusted_server_core::request_signing::RequestSigner;

    use super::{MODULE, OpenRtbDemand, builder};

    /// One `[demand.<name>]` table running this implementation.
    fn table() -> Map<String, Value> {
        demand_table(MODULE, "https://bid.example/openrtb2/auction")
    }

    /// The plan a `[demand]` selecting one source with `table` compiles to.
    fn compile(
        table: Map<String, Value>,
    ) -> Result<AuctionPlan, error_stack::Report<TrustedServerError>> {
        AuctionPlan::compile(plan_config_with(vec![("one", table)], &[builder()]))
    }

    /// The request core's driver builds for one source running this
    /// implementation with `settings`, signed when `signer` is given.
    fn build(settings: Value, signer: Option<&RequestSigner>) -> OpenRtbRequest {
        let plan = AuctionPlan::compile(golden_plan_config(MODULE, settings, true, &[builder()]))
            .expect("should compile plan");
        build_for_first_source(
            &plan,
            canonical_parity_auction_request(),
            &golden_inbound_request(),
            321,
            signer,
        )
        .expect("should build request")
        .expect("should keep the impression")
    }

    fn nested_object(levels: usize) -> Value {
        let mut value = Value::String("leaf".to_string());
        for level in 0..levels {
            value = Value::Object(Map::from_iter([(format!("level_{level}"), value)]));
        }
        value
    }

    fn nested_array(levels: usize) -> Value {
        let mut value = Value::String("leaf".to_string());
        for _ in 0..levels {
            value = Value::Array(vec![value]);
        }
        value
    }

    #[test]
    fn static_extensions_are_bounded_and_cannot_claim_reserved_fields() {
        let mut valid = table();
        valid.insert(
            "request_ext".to_string(),
            json!({"fictional_account": "example"}),
        );
        valid.insert("imp_ext".to_string(), json!({"placement_group": "display"}));
        let plan = compile(valid).expect("should compile static extensions");
        let openrtb = plan.providers()[0]
            .demand
            .as_any()
            .downcast_ref::<OpenRtbDemand>()
            .expect("should compile the OpenRTB implementation");
        assert_eq!(
            openrtb.request_ext.as_object()["fictional_account"],
            "example"
        );

        let mut reserved = table();
        reserved.insert(
            "request_ext".to_string(),
            json!({"trusted_server": {"signature": "forged"}}),
        );
        assert!(
            compile(reserved).is_err(),
            "should refuse an extension claiming a reserved field"
        );

        let mut too_large = table();
        too_large.insert(
            "request_ext".to_string(),
            json!({"padding": "x".repeat(17 * 1024)}),
        );
        assert!(
            compile(too_large).is_err(),
            "should refuse an extension over the size bound"
        );

        let mut too_deep = table();
        too_deep.insert("request_ext".to_string(), nested_object(9));
        assert!(
            compile(too_deep).is_err(),
            "should refuse an extension over the depth bound"
        );

        let mut deep_array = table();
        deep_array.insert(
            "request_ext".to_string(),
            json!({"levels": nested_array(8)}),
        );
        assert!(
            compile(deep_array).is_err(),
            "should count array levels toward the depth bound"
        );

        let mut not_an_object = table();
        not_an_object.insert("request_ext".to_string(), json!("string"));
        assert!(
            compile(not_an_object).is_err(),
            "should refuse an extension that is not an object"
        );
    }

    #[test]
    fn static_extensions_have_no_invented_bidder_param_location() {
        let request = build(
            json!({
                "request_ext": {"fictional_request": {"enabled": true}},
                "imp_ext": {"fictional_imp": "value"}
            }),
            None,
        );
        let value = serde_json::to_value(request).expect("should serialize request");
        assert_eq!(value["ext"]["fictional_request"]["enabled"], true);
        assert_eq!(value["imp"][0]["ext"]["fictional_imp"], "value");
        assert!(
            !value.to_string().contains("exampleBidder"),
            "the plain implementation must not invent bidder params placement"
        );
    }

    #[test]
    fn signed_and_unsigned_requests_have_exact_full_goldens() {
        let settings = json!({"request_ext": {"fictional": true}});
        let signer = deterministic_signer();
        assert_eq!(
            serde_json::to_string(&build(settings.clone(), Some(&signer)))
                .expect("should serialize signed request"),
            r#"{"id":"fictional-auction","imp":[{"id":"fictional-slot","banner":{"format":[{"w":300,"h":250},{"w":728,"h":90}]},"bidfloor":1.0,"bidfloorcur":"USD","secure":1}],"site":{"domain":"publisher.example","page":"https://publisher.example/article","publisher":{"domain":"publisher.example"}},"device":{"geo":{"type":2,"country":"US","region":"CA","metro":"501","city":"Example City"},"dnt":1,"ua":"Fictional Browser","ip":"192.0.2.10","language":"en"},"user":{"id":"fictional-user","consent":"fictional-tcf","ext":{"consent":"fictional-tcf","eids":[{"source":"identity.example","uids":[{"atype":1,"id":"fictional-uid"}]}]}},"tmax":321,"cur":["USD"],"regs":{"gdpr":1,"us_privacy":"1YNN","gpp":"fictional-gpp","gpp_sid":[2,6],"ext":{"gdpr":1,"gpp":"fictional-gpp","gpp_sid":[2,6],"us_privacy":"1YNN"}},"ext":{"fictional":true,"trusted_server":{"kid":"fictional-kid","request_host":"publisher.example","request_scheme":"https","signature":"LU_JUIA1BT80ShZNjSa4PIF5T-uMjEeodwKrV_6bXgh0hi1SYVtCKn9g_DTW62krmjCOFgoFYPHsu6L0nAcuDg","ts":1706900000,"version":"1.1"}}}"#,
            "signed wire fixture should stay exact"
        );
        assert_eq!(
            serde_json::to_string(&build(settings, None))
                .expect("should serialize unsigned request"),
            r#"{"id":"fictional-auction","imp":[{"id":"fictional-slot","banner":{"format":[{"w":300,"h":250},{"w":728,"h":90}]},"bidfloor":1.0,"bidfloorcur":"USD","secure":1}],"site":{"domain":"publisher.example","page":"https://publisher.example/article","publisher":{"domain":"publisher.example"}},"device":{"geo":{"type":2,"country":"US","region":"CA","metro":"501","city":"Example City"},"dnt":1,"ua":"Fictional Browser","ip":"192.0.2.10","language":"en"},"user":{"id":"fictional-user","consent":"fictional-tcf","ext":{"consent":"fictional-tcf","eids":[{"source":"identity.example","uids":[{"atype":1,"id":"fictional-uid"}]}]}},"tmax":321,"cur":["USD"],"regs":{"gdpr":1,"us_privacy":"1YNN","gpp":"fictional-gpp","gpp_sid":[2,6],"ext":{"gdpr":1,"gpp":"fictional-gpp","gpp_sid":[2,6],"us_privacy":"1YNN"}},"ext":{"fictional":true}}"#,
            "unsigned wire fixture should stay exact"
        );
    }

    #[test]
    fn an_endpoint_is_left_as_written() {
        let mut root = table();
        root.insert("endpoint".to_string(), json!("https://bid.example/"));
        let plan = compile(root).expect("should compile a root endpoint unchanged");
        assert_eq!(
            plan.providers()[0].endpoint.as_str(),
            "https://bid.example/"
        );
    }

    #[test]
    fn module_constant_is_the_crate_folder() {
        assert_eq!(
            MODULE,
            trusted_server_core::module_name!(),
            "should be named by the folder this crate lives in"
        );
    }
}
