//! Default-off diagnostic for the actual native RuntimeServices KV path.

use std::collections::BTreeSet;

use edgezero_adapter_axum::context::AxumRequestContext;
use edgezero_adapter_axum::dev_server::AxumRunOptions;
use edgezero_adapter_axum::diagnostics::NativeIngressMetadata;
use edgezero_adapter_axum::native_bindings::{
    NativeBindingsLimits, NativeConfigBinding, NativeKvBinding, NativeStoreBindings,
};
use edgezero_core::body::Body;
use edgezero_core::env_config::EnvConfig;
use edgezero_core::http::{HeaderValue, Method, Request, Response, StatusCode, header};
use edgezero_core::ingress::CheckedHostSource;
use error_stack::Report;
use serde::Deserialize;
use serde_json::json;
use trusted_server_core::config_payload::DEFAULT_CONFIG_STORE_ID;
use trusted_server_core::ec::kv::KvIdentityGraph;
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::http_util::RequestInfo;
use trusted_server_core::platform::RuntimeServices;

/// Reserved proof key. No caller-supplied keys or store aliases are accepted.
pub const PROOF_KEY: &str = "ts-lab/store-check";
/// Existing admin namespace, authenticated by the ordinary handler middleware.
pub const PATH: &str = "/_ts/admin/lab/store-check";

/// Capture a complete celld selection, never implicit native local defaults.
///
/// The pinned framework has no accessor for its captured binding document.
/// Capture the document once here, validate it, and supply that exact document
/// and its named token inputs through the explicit native options boundary.
///
/// # Errors
/// Rejects absent, malformed or incomplete celld bindings and invalid options.
pub fn native_options_from_env() -> Result<AxumRunOptions, String> {
    let variables = std::env::vars_os()
        .filter_map(|(key, value)| {
            let key = key.into_string().ok()?;
            key.starts_with("EDGEZERO__").then_some((key, value))
        })
        .map(|(key, value)| {
            value.into_string().map(|value| (key, value)).map_err(|_| {
                "native lab hosting environment contains a non-Unicode value".to_owned()
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let locator_count = variables
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case("EDGEZERO__STORE_BINDINGS_FILE"))
        .count();
    if locator_count != 1 {
        return Err(
            "lab-store-check requires one explicit celld bindings file; local fallback is disabled"
                .to_owned(),
        );
    }
    let env = EnvConfig::from_vars(variables.iter().map(|(key, value)| (key, value.clone())));
    let path = env.get(&["store_bindings_file"]).ok_or_else(|| {
        "lab-store-check requires explicit celld config and KV bindings; local fallback is disabled".to_owned()
    })?;
    let bindings =
        NativeStoreBindings::read(std::path::Path::new(path), NativeBindingsLimits::default())
            .map_err(|_| "lab celld bindings are invalid or unreadable".to_owned())?;
    validate_bindings(&bindings)?;
    let mut token_names = BTreeSet::new();
    for binding in bindings.config().values() {
        match binding {
            NativeConfigBinding::Celld(settings) => {
                token_names.extend(settings.access.token_env().map(str::to_owned));
            }
            NativeConfigBinding::AppConfigAgent(settings) => {
                token_names.extend(settings.access_token_env.clone());
            }
        }
    }
    for binding in bindings.kv().values() {
        if let NativeKvBinding::Celld(settings) = binding {
            token_names.extend(settings.access.token_env().map(str::to_owned));
        }
    }
    let tokens = token_names
        .into_iter()
        .map(|name| {
            let value = std::env::var(&name)
                .map_err(|_| "required native lab token is missing or non-Unicode".to_owned())?;
            Ok((name, value))
        })
        .collect::<Result<Vec<_>, String>>()?;
    AxumRunOptions::from_vars(
        variables
            .into_iter()
            .filter(|(key, _)| !key.eq_ignore_ascii_case("EDGEZERO__STORE_BINDINGS_FILE")),
    )
    .and_then(|options| options.with_store_bindings(bindings))
    .and_then(|options| options.with_bootstrap_values(tokens))
    .map_err(|_| "native lab hosting environment is invalid".to_owned())
}

fn validate_bindings(bindings: &NativeStoreBindings) -> Result<(), String> {
    if !matches!(
        bindings.config().get(DEFAULT_CONFIG_STORE_ID),
        Some(NativeConfigBinding::Celld(_))
    ) || !matches!(
        bindings.kv().get(crate::kv::STATE_STORE_ID),
        Some(NativeKvBinding::Celld(_))
    ) {
        return Err(
            "lab-store-check requires explicit celld config and KV bindings; local fallback is disabled".to_owned()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BINDINGS: &str = r#"
version = 1
[config.trusted_server_config]
provider = "celld-config"
endpoint = "http://127.0.0.1:7780"
default_key = "trusted_server_config"
[config.trusted_server_config.access]
mode = "trusted-network"
[config.trusted_server_config.documents.trusted_server_config]
store = "config"
key = "revision"
[kv.trusted_server_state]
provider = "celld-kv"
endpoint = "http://127.0.0.1:7780"
store = "state"
[kv.trusted_server_state.access]
mode = "trusted-network"
"#;

    #[test]
    fn lab_requires_both_selected_celld_bindings() {
        let parse = |input: &str| {
            NativeStoreBindings::parse(input, NativeBindingsLimits::default())
                .expect("should parse synthetic bindings")
        };
        validate_bindings(&parse(BINDINGS)).expect("should accept complete celld selection");
        let missing_kv = BINDINGS
            .split("[kv.trusted_server_state]")
            .next()
            .expect("should split fixture");
        assert!(validate_bindings(&parse(missing_kv)).is_err());
        assert!(validate_bindings(&parse("version = 1")).is_err());
        assert!(
            validate_bindings(&parse(
                &BINDINGS.replace("config.trusted_server_config", "config.other_config")
            ))
            .is_err()
        );
        assert!(
            validate_bindings(&parse(
                &BINDINGS.replace("kv.trusted_server_state", "kv.other_state")
            ))
            .is_err()
        );
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProofWrite {
    value: String,
}

fn response(status: StatusCode, payload: &serde_json::Value) -> Response {
    let mut response = Response::new(Body::from(
        serde_json::to_vec(payload).expect("should serialize diagnostic JSON"),
    ));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn failure(status: StatusCode, message: &str) -> Response {
    response(status, &json!({ "error": message }))
}

/// Write/read/delete the fixed proof key using production request services.
///
/// PUT accepts `{"value":"proof"}` and verifies the read-back. GET never
/// writes, allowing retention checks after restart. DELETE verifies absence.
/// Checked reader IP/source and immutable request geography are returned only
/// through this admin-protected route. The socket peer is IP-only. Fly's raw
/// client header is labeled separately and is never used as the checked reader.
/// Raw forwarding headers are unavailable after SDK sanitization, not empty.
/// The checksum covers the verified, pre-secret payload. Provider errors are redacted.
///
/// # Errors
/// Uses ordinary handler error typing; expected diagnostic failures are explicit
/// non-success JSON responses without provider bodies, tokens or secret values.
pub async fn handle(
    services: &RuntimeServices,
    config_sha256: Option<&str>,
    request: Request,
    ec_graph: Option<KvIdentityGraph>,
) -> Result<Response, Report<TrustedServerError>> {
    let Some(checksum) = config_sha256 else {
        return Ok(failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "adopted config checksum is unavailable",
        ));
    };
    let Some(info) = request.extensions().get::<RequestInfo>().cloned() else {
        return Ok(failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "checked native ingress is unavailable",
        ));
    };
    let socket_peer = AxumRequestContext::get(&request)
        .and_then(|context| context.remote_addr)
        .map(|address| address.ip());
    let ingress = request.extensions().get::<NativeIngressMetadata>();
    let reader_ip = ingress.and_then(NativeIngressMetadata::effective_client);
    let reader_source = ingress.and_then(|metadata| match metadata.client_source() {
        CheckedHostSource::Direct => Some("direct_peer"),
        CheckedHostSource::TrustedForwarded => Some("trusted_forwarded"),
        CheckedHostSource::TrustedXForwarded => Some("trusted_x_forwarded"),
        _ => None,
    });
    let fly_client_ip = request
        .headers()
        .get("fly-client-ip")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    // This service projects the immutable GeoOutcome captured before into_request.
    // It never looks up a raw header or resolves the provider a second time.
    let geo = services.geo().lookup(reader_ip).ok().flatten();
    let ingress = json!({
        "socket_peer": socket_peer,
        "reader_ip": reader_ip,
        "reader_source": reader_source,
        "forwarded_for": null,
        "forwarding_headers_stripped": true,
        "fly_client_ip": fly_client_ip,
    });
    let method = request.method().clone();
    if !matches!(method, Method::GET | Method::PUT | Method::DELETE) {
        let mut result = failure(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
        result
            .headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("GET, PUT, DELETE"));
        return Ok(result);
    }
    // A fixed non-identity key proves the selected EC provider is reachable.
    // No caller-supplied identity, consent snapshot or write enters this probe.
    let ec_configured = ec_graph.is_some();
    if let Some(graph) = &ec_graph
        && graph
            .key_exists_confirmed("ts-lab/ec-store-check")
            .await
            .is_err()
    {
        return Ok(failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "selected EC store probe failed",
        ));
    }
    let kv = services.kv_handle();
    let written = if method == Method::PUT {
        let bytes = match request.into_body().into_bytes_bounded(1_048_576).await {
            Ok(bytes) => bytes,
            Err(_) => {
                return Ok(failure(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "proof body could not be read within the store limit",
                ));
            }
        };
        let proof: ProofWrite = match serde_json::from_slice(&bytes) {
            Ok(proof) => proof,
            Err(_) => {
                return Ok(failure(
                    StatusCode::BAD_REQUEST,
                    "expected a JSON value string",
                ));
            }
        };
        if kv
            .put_bytes(PROOF_KEY, proof.value.clone().into())
            .await
            .is_err()
        {
            return Ok(failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "selected KV write failed; outcome may be uncertain",
            ));
        }
        Some(proof.value)
    } else {
        if method == Method::DELETE && kv.delete(PROOF_KEY).await.is_err() {
            return Ok(failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "selected KV delete failed; outcome may be uncertain",
            ));
        }
        None
    };
    let value = match kv.get_bytes(PROOF_KEY).await {
        Ok(Some(bytes)) => match String::from_utf8(bytes.to_vec()) {
            Ok(value) => Some(value),
            Err(_) => return Ok(failure(StatusCode::BAD_GATEWAY, "proof value is not UTF-8")),
        },
        Ok(None) => None,
        Err(_) => {
            return Ok(failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "selected KV read failed",
            ));
        }
    };
    if (method == Method::PUT && written != value) || (method == Method::DELETE && value.is_some())
    {
        return Ok(failure(
            StatusCode::BAD_GATEWAY,
            "selected KV read-back mismatch",
        ));
    }
    Ok(response(
        StatusCode::OK,
        &json!({
            "config_sha256": checksum,
            "scheme": info.scheme,
            "host": info.host,
            "key": PROOF_KEY,
            "value": value,
            "ingress": ingress,
            "ec": {
                "configured": ec_configured,
                "available": ec_configured,
            },
            "geo": {
                "available": geo.is_some(),
                "country": geo.as_ref().map(|location| location.country.as_str()),
                "region": geo.as_ref().and_then(|location| location.region.as_deref()),
            },
        }),
    ))
}
