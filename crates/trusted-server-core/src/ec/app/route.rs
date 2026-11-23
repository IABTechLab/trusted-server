//! Same-origin EC HTTP contract, independent of the serving adapter.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{Method, Request, Response, StatusCode, header};
use edgezero_core::router::RouterBuilder;
use serde::Deserialize;
use serde_json::json;

use super::IdentityApp;
use super::lifecycle::EcRouteResponse;
use crate::ec::eid_types::UnixSeconds;
use crate::ec::mirror::{MirrorCookie, build_mirror};
use crate::ec::page_load::NavigationPermit;
use crate::ec::sharedid::apply_sharedid_write_back;
use crate::ec::{EidSyncSource, current_timestamp, refresh_marker_values};
use crate::error::{IntoHttpResponse as _, http_error};
use crate::http_util::read_request_body;
use crate::platform::RuntimeServices;
use crate::response_privacy::enforce_terminal_private_cache_privacy;

/// The first-party identity endpoint.
pub const EC_PATH: &str = "/_ts/api/v1/ec";
const MAX_BODY: usize = 16 * 1024;
const MAX_SIGNAL: usize = 8 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ConsentSnapshot {
    #[serde(default, rename = "tcString", deserialize_with = "posted_field")]
    tcf_string: Option<String>,
    #[serde(default, deserialize_with = "posted_field")]
    gpp_string: Option<String>,
    #[serde(default, deserialize_with = "posted_field")]
    applicable_sections: Option<Vec<i32>>,
}

// Keep omission distinct from a posted empty array; posted fields must have
// their declared type, so null cannot silently stand in for a CMP reading.
fn posted_field<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

impl IdentityApp {
    /// Mounts the EC HTTP contract using an adapter's platform-service factory.
    ///
    /// Services supply trusted ingress and outbound capabilities, never identity
    /// policy. Every method is claimed locally, including unsupported ones.
    #[must_use]
    pub fn mount_ec_route<F>(
        self: &Arc<Self>,
        mut router: RouterBuilder,
        services: F,
    ) -> RouterBuilder
    where
        F: Fn(&RequestContext) -> Result<RuntimeServices, EdgeError> + Send + Sync + 'static,
    {
        let services = Arc::new(services);
        for method in [
            Method::GET,
            Method::POST,
            Method::HEAD,
            Method::OPTIONS,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::TRACE,
            Method::CONNECT,
        ] {
            let app = Arc::clone(self);
            let services = Arc::clone(&services);
            router = router.route(EC_PATH, method, move |context: RequestContext| {
                let app = Arc::clone(&app);
                let services = services(&context);
                async move {
                    let services = services?;
                    let request = context.into_request()?;
                    Ok::<_, EdgeError>(app.ec_route(&services, request).await)
                }
            });
        }
        router
    }

    /// Processes an explicit EC request without forwarding it to the publisher.
    pub async fn ec_route(
        self: &Arc<Self>,
        services: &RuntimeServices,
        request: Request,
    ) -> Response {
        let scheduler = request
            .extensions()
            .get::<edgezero_core::after_response::AfterResponseHandle>()
            .cloned();
        let request = match self.prepare(request) {
            Ok(request) => request,
            Err(error) => return private_error(http_error(&error)),
        };
        if request.method() != Method::GET && request.method() != Method::POST {
            let mut response = response(StatusCode::METHOD_NOT_ALLOWED);
            response
                .headers_mut()
                .insert(header::ALLOW, header::HeaderValue::from_static("GET, POST"));
            return response;
        }
        if self.identity_graph().is_none() {
            let mut result = response(StatusCode::NOT_IMPLEMENTED);
            if self.settings.ec.refresh_after_days.is_none()
                && refresh_marker_values(&request).next().is_none()
            {
                return result;
            }
            // Preserve the unsupported response while still honoring accepted withdrawal.
            // Validate posted consent before cleanup; rejected events keep the original 501.
            let request = if request.method() == Method::POST {
                match prepare_snapshot(services, request).await {
                    Ok(request) => request,
                    Err(_) => return result,
                }
            } else {
                request
            };
            if let Ok(identity) = self.read_request(services, &request) {
                self.finalize_refresh_withdrawal(&identity.context, &mut result);
            }
            return result;
        }
        if request.method() == Method::GET {
            let mut response = response(StatusCode::NO_CONTENT);
            if let Ok(mut identity) = self.read_request(services, &request) {
                self.finalize_refresh_withdrawal(&identity.context, &mut response);
                self.apply_cookie_opt_outs(&request, &mut identity.context);
                if identity.context.ec_allowed()
                    && let Some(graph) = identity.graph.as_ref()
                {
                    apply_sharedid_write_back(
                        &self.settings,
                        &mut identity.context,
                        graph,
                        &self.partners,
                        &identity.sharedid,
                        &mut response,
                    )
                    .await;
                }
            }
            return response;
        }
        let request = match prepare_snapshot(services, request).await {
            Ok(request) => request,
            Err(response) => return *response,
        };
        let mut identity = match self.read_request(services, &request) {
            Ok(identity) => identity,
            Err(error) => return private_error(http_error(&error)),
        };
        self.apply_cookie_opt_outs(&request, &mut identity.context);
        identity
            .context
            .set_eid_sync_source(EidSyncSource::Navigation);
        // An EC POST has its own write authority; it is a same-origin document
        // consent event, not an auction or partner API masquerading as one.
        identity
            .context
            .set_navigation_permit(NavigationPermit::for_ec_route());
        self.prepare_creation(&mut identity.context, identity.creation_allowed)
            .await;
        let after_response = self.prepare_after_response_lookups(
            services,
            &request,
            &identity.context,
            identity.graph.as_ref(),
            identity.creation_allowed,
        );
        let lookups = if identity.creation_allowed {
            self.dispatch_identity_lookups(
                services,
                &request,
                &mut identity.context,
                identity.graph.as_ref(),
            )
            .await
        } else {
            None
        };
        if let Some(lookups) = lookups {
            self.collect_identity_lookups(lookups, &mut identity.context)
                .await;
        }
        let mut result = response(StatusCode::OK);
        result.extensions_mut().insert(EcRouteResponse);
        crate::ec::consent_update::require_reconciliation(&mut result);
        self.finalize(
            &mut identity.context,
            identity.eids_cookie.as_deref(),
            &identity.sharedid,
            true,
            &mut result,
        )
        .await;
        let mirror = match build_mirror(
            identity
                .context
                .ec_value()
                .and_then(|id| identity.context.kv_snapshot().entry_for(id)),
            &identity.context,
            &self.partners,
            UnixSeconds::new(current_timestamp()),
        ) {
            MirrorCookie::Set(value) => URL_SAFE_NO_PAD
                .decode(value.as_str())
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .unwrap_or_else(|| json!({"v":1,"eids":[]})),
            MirrorCookie::Expire => json!({"v":1,"eids":[]}),
        };
        *result.body_mut() = edgezero_core::body::Body::from(mirror.to_string());
        result.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        self.schedule_after_response(
            identity.context,
            after_response,
            scheduler,
            services.outbound_client().clone(),
        );
        result
    }
}

async fn prepare_snapshot(
    services: &RuntimeServices,
    request: Request,
) -> Result<Request, Box<Response>> {
    if !crate::ec::consent_update::same_origin(services, &request) {
        return Err(Box::new(response(StatusCode::FORBIDDEN)));
    }
    let (mut parts, body) = request.into_parts();
    let bytes = read_request_body(&parts.headers, body, MAX_BODY)
        .await
        .map_err(|error| Box::new(response(error.current_context().status_code())))?;
    let snapshot: ConsentSnapshot =
        serde_json::from_slice(&bytes).map_err(|_| Box::new(response(StatusCode::BAD_REQUEST)))?;
    if snapshot.tcf_string.as_ref().is_some_and(|value| {
        value.len() > MAX_SIGNAL
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
            || crate::consent::tcf::decode_tc_string(value).is_err()
    }) || snapshot.gpp_string.as_ref().is_some_and(|value| {
        value.len() > MAX_SIGNAL
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._~=-".contains(&byte))
            || crate::consent::gpp::decode_gpp_string(value).is_err()
    }) || snapshot
        .applicable_sections
        .as_ref()
        .is_some_and(|sections| {
            sections.len() > 32 || sections.iter().any(|id| !(1..=65535).contains(id))
        })
    {
        return Err(Box::new(response(StatusCode::BAD_REQUEST)));
    }
    let mut cookies = Vec::new();
    for value in parts.headers.get_all(header::COOKIE) {
        let value = value
            .to_str()
            .map_err(|_| Box::new(response(StatusCode::BAD_REQUEST)))?;
        for pair in value.split(';').map(str::trim) {
            let name = pair.split_once('=').map_or(pair, |(name, _)| name.trim());
            let replaced = match name {
                "euconsent-v2" => snapshot.tcf_string.is_some(),
                "__gpp" => snapshot.gpp_string.is_some(),
                "__gpp_sid" => snapshot.applicable_sections.is_some(),
                _ => false,
            };
            if !replaced {
                cookies.push(pair.to_owned());
            }
        }
    }
    if let Some(tcf) = snapshot.tcf_string {
        cookies.push(format!("euconsent-v2={tcf}"));
    }
    if let Some(gpp) = snapshot.gpp_string {
        cookies.push(format!("__gpp={gpp}"));
    }
    if let Some(sections) = snapshot.applicable_sections
        && !sections.is_empty()
    {
        cookies.push(format!(
            "__gpp_sid={}",
            sections
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    parts.headers.remove(header::COOKIE);
    if !cookies.is_empty() {
        parts.headers.insert(
            header::COOKIE,
            header::HeaderValue::from_str(&cookies.join("; "))
                .map_err(|_| Box::new(response(StatusCode::BAD_REQUEST)))?,
        );
    }
    Ok(Request::from_parts(
        parts,
        edgezero_core::body::Body::empty(),
    ))
}

fn response(status: StatusCode) -> Response {
    let mut response = Response::new(edgezero_core::body::Body::empty());
    *response.status_mut() = status;
    private_error(response)
}
fn private_error(mut response: Response) -> Response {
    enforce_terminal_private_cache_privacy(&mut response);
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_util::RuntimeRequestOrigin;
    use crate::platform::test_support::noop_services;
    use edgezero_core::body::Body;
    use edgezero_core::ingress::{InboundOrigin, OriginSource};

    async fn snapshot(body: serde_json::Value) -> Result<Request, Box<Response>> {
        let mut request = edgezero_core::http::request_builder()
            .method(Method::POST)
            .uri("https://publisher.example.com/_ts/api/v1/ec")
            .header(header::ORIGIN, "https://publisher.example.com")
            .header(
                header::COOKIE,
                "euconsent-v2=old-tcf; __gpp=old-gpp; __gpp_sid=7; unrelated=keep",
            )
            .body(Body::from(body.to_string()))
            .expect("should build snapshot request");
        request.headers_mut().append(
            header::COOKIE,
            header::HeaderValue::from_static("other=keep-too"),
        );
        request.extensions_mut().insert(RuntimeRequestOrigin::new(
            InboundOrigin::parse("https", "publisher.example.com", OriginSource::RuntimeUri)
                .expect("should parse public origin"),
        ));
        prepare_snapshot(&noop_services(), request).await
    }

    #[tokio::test]
    async fn absent_snapshot_fields_keep_each_request_cookie() {
        for (body, expected) in [
            (
                json!({}),
                "euconsent-v2=old-tcf; __gpp=old-gpp; __gpp_sid=7; unrelated=keep; other=keep-too",
            ),
            (
                json!({"tcString": "CAAAAAAAAAAAAAAAAAAAAAAAAJAAAAAAAAAAAAAAAAA"}),
                "__gpp=old-gpp; __gpp_sid=7; unrelated=keep; other=keep-too; euconsent-v2=CAAAAAAAAAAAAAAAAAAAAAAAAJAAAAAAAAAAAAAAAAA",
            ),
            (
                json!({"applicableSections": []}),
                "euconsent-v2=old-tcf; __gpp=old-gpp; unrelated=keep; other=keep-too",
            ),
            (
                json!({"applicableSections": [1, 65535]}),
                "euconsent-v2=old-tcf; __gpp=old-gpp; unrelated=keep; other=keep-too; __gpp_sid=1,65535",
            ),
            (
                json!({"gppString": "DBABLA~BVQqAAAAAgA.QA"}),
                "euconsent-v2=old-tcf; __gpp_sid=7; unrelated=keep; other=keep-too; __gpp=DBABLA~BVQqAAAAAgA.QA",
            ),
        ] {
            let request = snapshot(body).await.expect("should accept optional fields");
            assert_eq!(
                request.headers()[header::COOKIE],
                expected,
                "should replace only posted consent cookies"
            );
        }
    }

    #[tokio::test]
    async fn snapshot_rejects_cookie_delimiters_in_posted_strings() {
        let tcf = "CAAAAAAAAAAAAAAAAAAAAAAAAJAAAAAAAAAAAAAAAAA";
        let body = json!({"tcString": format!("{tcf}.; ts-ec=injected")});
        assert_eq!(
            snapshot(body)
                .await
                .expect_err("should reject cookie injection")
                .status(),
            StatusCode::BAD_REQUEST,
            "should validate the complete posted string before cookie serialization"
        );
    }

    #[tokio::test]
    async fn snapshot_rejects_unknown_fields_and_invalid_sections() {
        for body in [
            json!({"tcfString": "CAAAAAAAAAAAAAAAAAAAAAAAAJAAAAAAAAAAAAAAAAA"}),
            json!({"tcString": "invalid"}),
            json!({"unknown": true}),
            json!({"tcString": null}),
            json!({"gppString": null}),
            json!({"applicableSections": null}),
            json!({"applicableSections": [0]}),
            json!({"applicableSections": [65536]}),
            json!({"applicableSections": [-1]}),
            json!({"applicableSections": [1.5]}),
            json!({"applicableSections": vec![7; 33]}),
        ] {
            assert_eq!(
                snapshot(body)
                    .await
                    .expect_err("should reject invalid contract")
                    .status(),
                StatusCode::BAD_REQUEST,
                "should validate every posted section and reject unknown fields"
            );
        }
        assert!(
            snapshot(json!({"applicableSections": vec![7; 32]}))
                .await
                .is_ok(),
            "should accept exactly 32 section IDs"
        );
    }
}
