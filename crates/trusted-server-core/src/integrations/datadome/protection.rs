use std::time::Duration;

use edgezero_core::body::Body as EdgeBody;
use edgezero_core::http::{request_builder, HeaderMap, HeaderName};
use error_stack::{Report, ResultExt};
use http::{header, Method, Request, Response, StatusCode};
use url::Url;

use crate::error::TrustedServerError;
use crate::integrations::{
    HeaderMutation, RequestFilterDecision, RequestFilterEffects, RequestFilterInput,
};
use crate::platform::{PlatformBackendSpec, PlatformHttpRequest, RuntimeServices, StoreName};
use crate::redacted::Redacted;

use super::protection_scope::{ProtectionRequestFacts, ProtectionScopeDecision};
use super::DataDomeIntegration;

const VALIDATE_REQUEST_PATH: &str = "/validate-request";
const REQUEST_MODULE_NAME: &str = "Trusted-Server-Rust";
const MODULE_VERSION: &str = env!("CARGO_PKG_VERSION");
const HEADER_DATADOME_RESPONSE: &str = "x-datadomeresponse";
const HEADER_DATADOME_REQUEST_HEADERS: &str = "x-datadome-request-headers";
const HEADER_DATADOME_HEADERS: &str = "x-datadome-headers";
const HEADER_DATADOME_CLIENT_ID: &str = "x-datadome-clientid";
const HEADER_DATADOME_X_SET_COOKIE: &str = "x-datadome-x-set-cookie";
const DATADOME_COOKIE_NAME: &str = "datadome";

enum ProtectionRequestError {
    Setup(Report<TrustedServerError>),
    Runtime(Report<TrustedServerError>),
}

impl DataDomeIntegration {
    pub(super) async fn filter_protection_request(
        &self,
        input: RequestFilterInput<'_>,
    ) -> RequestFilterDecision {
        if !self.config.enable_protection || !self.is_request_protected(&input) {
            return RequestFilterDecision::Continue(RequestFilterEffects::default());
        }

        match self.filter_protection_request_inner(input).await {
            Ok(decision) => decision,
            Err(ProtectionRequestError::Setup(err)) => {
                log::error!("[datadome] Protection setup failed open: {err:?}");
                RequestFilterDecision::Continue(RequestFilterEffects::default())
            }
            Err(ProtectionRequestError::Runtime(err)) => {
                log::warn!("[datadome] Protection API failed open: {err:?}");
                RequestFilterDecision::Continue(RequestFilterEffects::default())
            }
        }
    }

    async fn filter_protection_request_inner(
        &self,
        input: RequestFilterInput<'_>,
    ) -> Result<RequestFilterDecision, ProtectionRequestError> {
        let api_url = self.protection_validate_url();
        let backend_name = self
            .ensure_protection_backend(input.services, &api_url)
            .map_err(ProtectionRequestError::Setup)?;
        let server_side_key = self
            .load_server_side_key(input.services)
            .map_err(ProtectionRequestError::Setup)?;
        let payload = self.build_protection_payload(&input, &server_side_key);
        let encoded_body = form_encode(&payload.fields);

        let mut builder = request_builder()
            .method(Method::POST.as_str())
            .uri(api_url.as_str())
            .header(
                header::CONTENT_TYPE.as_str(),
                "application/x-www-form-urlencoded",
            )
            .header(
                header::CONTENT_LENGTH.as_str(),
                encoded_body.len().to_string(),
            );

        if payload.uses_header_client_id {
            builder = builder.header(HEADER_DATADOME_X_SET_COOKIE, "true");
        }

        let request = builder
            .body(EdgeBody::from(encoded_body))
            .change_context(Self::error(
                "Failed to build DataDome Protection API request",
            ))
            .map_err(ProtectionRequestError::Runtime)?;

        let platform_response = input
            .services
            .http_client()
            .send(PlatformHttpRequest::new(request, backend_name))
            .await
            .change_context(Self::error("Failed to call DataDome Protection API"))
            .map_err(ProtectionRequestError::Runtime)?;

        Ok(self.classify_protection_response(platform_response.response, input.request.method()))
    }

    fn is_request_protected(&self, input: &RequestFilterInput<'_>) -> bool {
        let req = input.request;
        if req.method() == Method::OPTIONS {
            return false;
        }

        if input.is_integration_route {
            return false;
        }

        let path = req.uri().path();
        if is_internal_path(path) {
            return false;
        }

        let facts = ProtectionRequestFacts {
            method: req.method().as_str(),
            path,
            query: req.uri().query(),
            client_ip: input.services.client_info().client_ip,
            asn: input.geo_info.and_then(|geo| geo.asn),
        };
        match self.protection_scope.evaluate(&facts, input.services) {
            ProtectionScopeDecision::Protect => {}
            ProtectionScopeDecision::Skip { rule_id, reason } => {
                log::debug!("[datadome] Skipping Protection API for rule {rule_id} ({reason})");
                return false;
            }
        }

        true
    }

    fn protection_validate_url(&self) -> String {
        format!(
            "{}{}",
            self.config.protection_api_origin.trim_end_matches('/'),
            VALIDATE_REQUEST_PATH
        )
    }

    fn ensure_protection_backend(
        &self,
        services: &RuntimeServices,
        api_url: &str,
    ) -> Result<String, Report<TrustedServerError>> {
        let parsed = Url::parse(api_url)
            .change_context(Self::error("Invalid DataDome Protection API URL"))?;
        let host = parsed
            .host_str()
            .ok_or_else(|| Report::new(Self::error("Missing DataDome Protection API host")))?;
        let spec = PlatformBackendSpec {
            scheme: parsed.scheme().to_string(),
            host: host.to_string(),
            port: parsed.port(),
            host_header_override: None,
            certificate_check: true,
            first_byte_timeout: Duration::from_millis(u64::from(self.config.timeout_ms)),
        };

        services.backend().ensure(&spec).change_context(Self::error(
            "Failed to register DataDome Protection API backend",
        ))
    }

    fn load_server_side_key(
        &self,
        services: &RuntimeServices,
    ) -> Result<Redacted<String>, Report<TrustedServerError>> {
        let store_name = StoreName::from(self.config.server_side_key_secret_store.as_str());
        let key = services
            .secret_store()
            .get_string(&store_name, &self.config.server_side_key_secret_name)
            .change_context(Self::error(
                "Failed to read DataDome server-side key from secret store",
            ))?;
        let key = key.trim().to_string();
        if key.is_empty() {
            return Err(Report::new(Self::error(
                "DataDome server-side key secret must not be empty",
            )));
        }

        Ok(Redacted::new(key))
    }

    fn build_protection_payload(
        &self,
        input: &RequestFilterInput<'_>,
        server_side_key: &Redacted<String>,
    ) -> ProtectionPayload {
        let req = input.request;
        let client_info = input.services.client_info();
        let mut fields = Vec::new();
        let header_client_id = header_value(req, HEADER_DATADOME_CLIENT_ID);
        let cookie_header = header_value(req, header::COOKIE.as_str());
        let cookie_client_id = parse_cookie_value(&cookie_header, DATADOME_COOKIE_NAME);
        let client_id = if header_client_id.is_empty() {
            cookie_client_id.unwrap_or_default()
        } else {
            header_client_id.clone()
        };

        push_field(&mut fields, "Key", server_side_key.expose());
        push_field(
            &mut fields,
            "IP",
            client_info
                .client_ip
                .map(|ip| ip.to_string())
                .unwrap_or_default(),
        );
        push_header_field(&mut fields, req, "Accept", header::ACCEPT.as_str());
        push_header_field(&mut fields, req, "AcceptCharset", "accept-charset");
        push_header_field(
            &mut fields,
            req,
            "AcceptEncoding",
            header::ACCEPT_ENCODING.as_str(),
        );
        push_header_field(
            &mut fields,
            req,
            "AcceptLanguage",
            header::ACCEPT_LANGUAGE.as_str(),
        );
        push_field(
            &mut fields,
            "AuthorizationLen",
            header_value(req, header::AUTHORIZATION.as_str())
                .len()
                .to_string(),
        );
        push_header_field(
            &mut fields,
            req,
            "CacheControl",
            header::CACHE_CONTROL.as_str(),
        );
        push_field(&mut fields, "ClientID", client_id);
        push_header_field(&mut fields, req, "Connection", header::CONNECTION.as_str());
        push_header_field(
            &mut fields,
            req,
            "ContentType",
            header::CONTENT_TYPE.as_str(),
        );
        push_field(&mut fields, "CookiesLen", cookie_header.len().to_string());
        push_header_field(&mut fields, req, "From", "from");
        push_field(&mut fields, "HeadersList", headers_list(req));
        push_field(&mut fields, "Host", request_host(req));
        push_field(&mut fields, "Method", req.method().as_str());
        push_field(&mut fields, "ModuleVersion", MODULE_VERSION);
        push_header_field(&mut fields, req, "Origin", header::ORIGIN.as_str());
        push_field(&mut fields, "Port", "0");
        push_header_field(
            &mut fields,
            req,
            "PostParamLen",
            header::CONTENT_LENGTH.as_str(),
        );
        push_header_field(&mut fields, req, "Pragma", header::PRAGMA.as_str());
        push_field(
            &mut fields,
            "Protocol",
            req.uri().scheme_str().unwrap_or_default(),
        );
        push_header_field(&mut fields, req, "Referer", header::REFERER.as_str());
        push_field(&mut fields, "Request", request_path_and_query(req));
        push_field(&mut fields, "RequestModuleName", REQUEST_MODULE_NAME);
        push_header_field(
            &mut fields,
            req,
            "SecCHDeviceMemory",
            "sec-ch-device-memory",
        );
        push_header_field(&mut fields, req, "SecCHUA", "sec-ch-ua");
        push_header_field(&mut fields, req, "SecCHUAArch", "sec-ch-ua-arch");
        push_header_field(
            &mut fields,
            req,
            "SecCHUAFullVersionList",
            "sec-ch-ua-full-version-list",
        );
        push_header_field(&mut fields, req, "SecCHUAMobile", "sec-ch-ua-mobile");
        push_header_field(&mut fields, req, "SecCHUAModel", "sec-ch-ua-model");
        push_header_field(&mut fields, req, "SecCHUAPlatform", "sec-ch-ua-platform");
        push_header_field(&mut fields, req, "SecFetchDest", "sec-fetch-dest");
        push_header_field(&mut fields, req, "SecFetchMode", "sec-fetch-mode");
        push_header_field(&mut fields, req, "SecFetchSite", "sec-fetch-site");
        push_header_field(
            &mut fields,
            req,
            "SecFetchStorageAccess",
            "sec-fetch-storage-access",
        );
        push_header_field(&mut fields, req, "SecFetchUser", "sec-fetch-user");
        push_field(&mut fields, "ServerHostname", request_host(req));
        push_field(
            &mut fields,
            "ServerName",
            client_info.server_hostname.as_deref().unwrap_or_default(),
        );
        push_field(
            &mut fields,
            "ServerRegion",
            client_info.server_region.as_deref().unwrap_or_default(),
        );
        push_field(
            &mut fields,
            "TimeRequest",
            chrono::Utc::now().timestamp_micros().to_string(),
        );
        push_header_field(&mut fields, req, "TrueClientIP", "true-client-ip");
        push_header_field(&mut fields, req, "UserAgent", header::USER_AGENT.as_str());
        push_header_field(&mut fields, req, "Via", header::VIA.as_str());
        push_header_field(&mut fields, req, "XForwardedForIP", "x-forwarded-for");
        push_header_field(&mut fields, req, "X-Real-IP", "x-real-ip");
        push_header_field(&mut fields, req, "X-Requested-With", "x-requested-with");
        push_field(
            &mut fields,
            "TlsProtocol",
            client_info.tls_protocol.as_deref().unwrap_or_default(),
        );
        push_field(
            &mut fields,
            "TlsCipher",
            client_info.tls_cipher.as_deref().unwrap_or_default(),
        );
        push_field(
            &mut fields,
            "JA4",
            client_info.tls_ja4.as_deref().unwrap_or_default(),
        );
        push_field(
            &mut fields,
            "H2Fingerprint",
            client_info.h2_fingerprint.as_deref().unwrap_or_default(),
        );

        ProtectionPayload {
            fields,
            uses_header_client_id: !header_client_id.is_empty(),
        }
    }

    fn classify_protection_response(
        &self,
        response: edgezero_core::http::Response,
        request_method: &Method,
    ) -> RequestFilterDecision {
        let (parts, body) = response.into_parts();
        let status = parts.status;
        let Some(datadome_status) = datadome_response_status(&parts.headers) else {
            log::warn!("[datadome] Protection API response missing X-DataDomeResponse");
            return RequestFilterDecision::Continue(RequestFilterEffects::default());
        };

        if datadome_status != status.as_u16() {
            log::warn!(
                "[datadome] Protection API status/header mismatch: status={} header={}",
                status.as_u16(),
                datadome_status
            );
            return RequestFilterDecision::Continue(RequestFilterEffects::default());
        }

        let effects = RequestFilterEffects {
            request_headers: extract_header_mutations(
                &parts.headers,
                HEADER_DATADOME_REQUEST_HEADERS,
            ),
            response_headers: extract_header_mutations(&parts.headers, HEADER_DATADOME_HEADERS),
        };

        if status == StatusCode::OK {
            return RequestFilterDecision::Continue(effects);
        }

        if matches!(status.as_u16(), 301 | 302 | 401 | 403 | 429) {
            let response_body = if request_method == Method::HEAD {
                EdgeBody::empty()
            } else {
                if body.is_stream() {
                    log::warn!(
                        "[datadome] Protection API challenge body was streaming; failing open"
                    );
                    return RequestFilterDecision::Continue(RequestFilterEffects::default());
                }
                let body_bytes = body.into_bytes();
                EdgeBody::from(body_bytes.as_ref().to_vec())
            };
            let challenge = Response::builder()
                .status(status)
                .body(response_body)
                .expect("should build DataDome challenge response");
            return RequestFilterDecision::Respond {
                response: Box::new(challenge),
                effects,
            };
        }

        log::warn!(
            "[datadome] Protection API returned fail-open status {}",
            status.as_u16()
        );
        RequestFilterDecision::Continue(RequestFilterEffects::default())
    }
}

struct ProtectionPayload {
    fields: Vec<(String, String)>,
    uses_header_client_id: bool,
}

fn is_internal_path(path: &str) -> bool {
    path.starts_with("/static/tsjs=")
        || path.starts_with("/integrations/")
        || path.starts_with("/first-party/")
        || path == "/.well-known/trusted-server.json"
        || path == "/verify-signature"
        || path.starts_with("/admin/")
        || path.starts_with("/_ts/admin/")
        || path == "/_ts/api/v1/identify"
        || path == "/_ts/api/v1/batch-sync"
}

fn request_host(req: &Request<EdgeBody>) -> String {
    req.headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .or_else(|| req.uri().host())
        .unwrap_or_default()
        .to_string()
}

fn request_path_and_query(req: &Request<EdgeBody>) -> String {
    req.uri()
        .path_and_query()
        .map(|path_and_query| path_and_query.as_str().to_string())
        .unwrap_or_else(|| req.uri().path().to_string())
}

fn header_value(req: &Request<EdgeBody>, name: &str) -> String {
    req.headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

fn headers_list(req: &Request<EdgeBody>) -> String {
    req.headers()
        .keys()
        .map(HeaderName::as_str)
        .collect::<Vec<_>>()
        .join(",")
}

fn push_header_field(
    fields: &mut Vec<(String, String)>,
    req: &Request<EdgeBody>,
    field_name: &'static str,
    header_name: &str,
) {
    push_field(fields, field_name, header_value(req, header_name));
}

fn push_field(fields: &mut Vec<(String, String)>, key: &'static str, value: impl AsRef<str>) {
    let value = value.as_ref();
    if value.is_empty() {
        return;
    }

    fields.push((key.to_string(), truncate_field(key, value)));
}

fn form_encode(fields: &[(String, String)]) -> String {
    fields
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                urlencoding::encode(key),
                urlencoding::encode(value)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn datadome_response_status(headers: &HeaderMap) -> Option<u16> {
    headers
        .get(HEADER_DATADOME_RESPONSE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u16>().ok())
}

fn extract_header_mutations(headers: &HeaderMap, pointer_header: &str) -> Vec<HeaderMutation> {
    let mut mutations = Vec::new();

    for pointer_value in headers.get_all(pointer_header) {
        let Ok(pointer_value) = pointer_value.to_str() else {
            continue;
        };

        for header_name in pointer_value.split_whitespace() {
            if header_name.eq_ignore_ascii_case(HEADER_DATADOME_HEADERS)
                || header_name.eq_ignore_ascii_case(HEADER_DATADOME_REQUEST_HEADERS)
                || header_name.eq_ignore_ascii_case(HEADER_DATADOME_RESPONSE)
            {
                continue;
            }

            let Ok(parsed_name) = HeaderName::from_bytes(header_name.as_bytes()) else {
                log::warn!("[datadome] Ignoring invalid pointer header name: {header_name}");
                continue;
            };

            for value in headers.get_all(&parsed_name) {
                let Ok(value) = value.to_str() else {
                    continue;
                };
                if parsed_name
                    .as_str()
                    .eq_ignore_ascii_case(header::SET_COOKIE.as_str())
                {
                    mutations.push(HeaderMutation::append(parsed_name.as_str(), value));
                } else {
                    mutations.push(HeaderMutation::set(parsed_name.as_str(), value));
                }
            }
        }
    }

    mutations
}

fn parse_cookie_value(cookie_header: &str, name: &str) -> Option<String> {
    for pair in cookie_header.split(';') {
        let trimmed = pair.trim();
        let Some((cookie_name, cookie_value)) = trimmed.split_once('=') else {
            continue;
        };
        if cookie_name == name {
            let unquoted = cookie_value.trim_matches('"');
            return Some(
                urlencoding::decode(unquoted)
                    .map(std::borrow::Cow::into_owned)
                    .unwrap_or_else(|_| unquoted.to_string()),
            );
        }
    }

    None
}

fn truncate_field(key: &str, value: &str) -> String {
    let limit = field_limit(key);
    if limit == 0 {
        return value.to_string();
    }

    truncate_utf8(value, limit)
}

fn field_limit(key: &str) -> i32 {
    match key.to_ascii_lowercase().as_str() {
        "jsonrpcversion"
        | "secchdevicememory"
        | "secchuamobile"
        | "secfetchstorageaccess"
        | "secfetchuser" => 8,
        "mcpparamsclientinfoversion" | "mcpprotocolversion" | "secchuaarch" => 16,
        "secchuaplatform" | "secfetchdest" | "secfetchmode" => 32,
        "contenttype"
        | "jsonrpcrequestid"
        | "mcpmethod"
        | "mcpparamsclientinfoname"
        | "mcpparamstoolname"
        | "mcpsessionid"
        | "secfetchsite"
        | "tlscipher" => 64,
        "acceptcharset"
        | "acceptencoding"
        | "cachecontrol"
        | "connection"
        | "from"
        | "graphqloperationname"
        | "pragma"
        | "secchua"
        | "secchuamodel"
        | "trueclientip"
        | "userid"
        | "x-real-ip"
        | "x-requested-with"
        | "productid" => 128,
        "acceptlanguage" | "secchuafullversionlist" | "via" => 256,
        "accept" | "clientid" | "headerslist" | "host" | "origin" | "serverhostname"
        | "servername" | "signature" | "signatureagent" => 512,
        "xforwardedforip" => -512,
        "useragent" => 768,
        "cookieslist" | "referer" => 1024,
        "request" | "signatureinput" => 2048,
        _ => 0,
    }
}

fn truncate_utf8(value: &str, limit: i32) -> String {
    let max = limit.unsigned_abs() as usize;
    if value.len() <= max {
        return value.to_string();
    }

    if limit > 0 {
        let mut end = 0;
        for (idx, ch) in value.char_indices() {
            let next = idx + ch.len_utf8();
            if next > max {
                break;
            }
            end = next;
        }
        value[..end].to_string()
    } else {
        let mut start = value.len();
        let mut used = 0;
        for (idx, ch) in value.char_indices().rev() {
            let next = used + ch.len_utf8();
            if next > max {
                break;
            }
            used = next;
            start = idx;
        }
        value[start..].to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use crate::integrations::datadome::DataDomeConfig;
    use crate::platform::test_support::{
        build_services_with_config_and_secret, HashMapSecretStore, NoopConfigStore, NoopSecretStore,
    };

    use super::*;

    fn protection_integration() -> Arc<DataDomeIntegration> {
        let config = DataDomeConfig {
            enabled: true,
            enable_protection: true,
            ..DataDomeConfig::default()
        };
        DataDomeIntegration::try_new(config).expect("should create integration")
    }

    #[test]
    fn load_server_side_key_reads_secret_store() {
        let mut secrets = HashMap::new();
        secrets.insert(
            "datadome_server_side_key".to_string(),
            b"secret-from-store".to_vec(),
        );
        let services = build_services_with_config_and_secret(
            NoopConfigStore,
            HashMapSecretStore::new(secrets),
        );
        let integration = protection_integration();

        let key = integration
            .load_server_side_key(&services)
            .expect("should load server-side key");

        assert_eq!(key.expose(), "secret-from-store");
    }

    #[test]
    fn load_server_side_key_errors_when_secret_missing() {
        let services = build_services_with_config_and_secret(NoopConfigStore, NoopSecretStore);
        let config = DataDomeConfig {
            enabled: true,
            enable_protection: true,
            server_side_key_secret_name: "missing_server_side_key".to_string(),
            ..DataDomeConfig::default()
        };
        let integration = DataDomeIntegration::try_new(config).expect("should create integration");

        let result = integration.load_server_side_key(&services);

        assert!(result.is_err(), "should error when secret is missing");
    }

    #[test]
    fn extract_header_mutations_appends_set_cookie_and_sets_other_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HEADER_DATADOME_HEADERS,
            edgezero_core::http::HeaderValue::from_static("Set-Cookie X-DD-B"),
        );
        headers.append(
            header::SET_COOKIE.as_str(),
            edgezero_core::http::HeaderValue::from_static("datadome=abc; Path=/"),
        );
        headers.insert("x-dd-b", edgezero_core::http::HeaderValue::from_static("1"));

        let mutations = extract_header_mutations(&headers, HEADER_DATADOME_HEADERS);

        assert_eq!(
            mutations,
            vec![
                HeaderMutation::append("set-cookie", "datadome=abc; Path=/"),
                HeaderMutation::set("x-dd-b", "1"),
            ],
            "should append Set-Cookie while replacing non-cookie headers"
        );
    }

    #[test]
    fn parse_cookie_value_decodes_datadome_cookie() {
        let value = parse_cookie_value("a=1; datadome=abc%20123; b=2", "datadome")
            .expect("should parse datadome cookie");
        assert_eq!(value, "abc 123");
    }

    #[test]
    fn truncate_utf8_preserves_char_boundaries() {
        assert_eq!(truncate_utf8("ééé", 4), "éé");
        assert_eq!(truncate_utf8("ééé", -4), "éé");
    }

    #[test]
    fn classify_head_challenge_omits_response_body() {
        let integration = protection_integration();
        let response = edgezero_core::http::response_builder()
            .status(StatusCode::FORBIDDEN)
            .header(HEADER_DATADOME_RESPONSE, "403")
            .body(EdgeBody::from("blocked"))
            .expect("should build DataDome response");

        let decision = integration.classify_protection_response(response, &Method::HEAD);

        let RequestFilterDecision::Respond { response, .. } = decision else {
            panic!("should return a challenge response for DataDome 403");
        };
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "should preserve challenge status"
        );
        assert_eq!(
            response.into_body().into_bytes().as_ref(),
            b"",
            "HEAD challenges should not include a response body"
        );
    }

    #[test]
    fn classify_redirect_challenge_preserves_location_as_response_effect() {
        let integration = protection_integration();
        let response = edgezero_core::http::response_builder()
            .status(StatusCode::FOUND)
            .header(HEADER_DATADOME_RESPONSE, "302")
            .header(HEADER_DATADOME_HEADERS, "Location")
            .header(header::LOCATION, "/challenge")
            .body(EdgeBody::empty())
            .expect("should build DataDome redirect response");

        let decision = integration.classify_protection_response(response, &Method::GET);

        let RequestFilterDecision::Respond { response, effects } = decision else {
            panic!("should return a redirect challenge response");
        };
        assert_eq!(
            response.status(),
            StatusCode::FOUND,
            "should preserve redirect status"
        );
        assert_eq!(
            effects.response_headers,
            vec![HeaderMutation::set("location", "/challenge")],
            "should carry Location through response effects"
        );
    }

    #[test]
    fn classify_ok_response_preserves_request_header_effects() {
        let integration = protection_integration();
        let response = edgezero_core::http::response_builder()
            .status(StatusCode::OK)
            .header(HEADER_DATADOME_RESPONSE, "200")
            .header(HEADER_DATADOME_REQUEST_HEADERS, "X-DataDome-ClientID")
            .header(HEADER_DATADOME_CLIENT_ID, "client-123")
            .body(EdgeBody::empty())
            .expect("should build DataDome allow response");

        let decision = integration.classify_protection_response(response, &Method::GET);

        let RequestFilterDecision::Continue(effects) = decision else {
            panic!("should continue with request header effects");
        };
        assert_eq!(
            effects.request_headers,
            vec![HeaderMutation::set(HEADER_DATADOME_CLIENT_ID, "client-123")],
            "should carry requested upstream headers through effects"
        );
    }

    #[test]
    fn form_encode_url_encodes_values() {
        let encoded = form_encode(&[("Key".to_string(), "a b+c".to_string())]);
        assert_eq!(encoded, "Key=a%20b%2Bc");
    }
}
