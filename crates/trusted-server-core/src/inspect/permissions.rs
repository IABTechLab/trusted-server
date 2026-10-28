//! The permissions resolved for the request being served, at
//! [`PERMISSIONS_PAGE_PATH`] and [`PERMISSIONS_JSON_PATH`].
//!
//! The answer is the evaluation the page receives as
//! `window.tsjs.permissions`, offered at an address so client code can fetch
//! it, another front end can render it, and a person can read it. It is
//! resolved for the request that asks, exactly as a page's is, and nothing is
//! written for the reader, because the Edge Cookie lifecycle does not run
//! here.

use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::{HeaderValue, Request, Response, StatusCode, header};
use serde_json::{Value, json};

use crate::ec::EcContext;
use crate::error::TrustedServerError;
use crate::permissions::PermissionState;
use crate::platform::RuntimeServices;
use crate::settings::Settings;

use super::render_page;

/// The permissions page.
pub const PERMISSIONS_PAGE_PATH: &str = "/_ts/permissions";

/// The same resolution as data.
pub const PERMISSIONS_JSON_PATH: &str = "/_ts/permissions.json";

/// Both addresses, the page first.
pub const PERMISSIONS_PATHS: [&str; 2] = [PERMISSIONS_PAGE_PATH, PERMISSIONS_JSON_PATH];

/// The permission resolution for a request, as the endpoint shows it, being
/// the page shape of [`PermissionState::page_value`] with these fields added.
///
/// - `storageWithdrawn`: whether the request explicitly withdrew device storage.
/// - `modules.contributed`: the signal modules that produced a signal used here.
/// - `modules.configured`: the signal modules the publisher named, or `null`.
/// - `policy`: the permission policy file whose rules the answer resolved,
///   by the name `[permissions] policy` gives it, or `null` for settings that
///   name none, which no loaded domain has.
/// - `version`: the version of the core crate that answered.
#[must_use]
pub fn permissions_payload(state: &PermissionState, settings: &Settings) -> Value {
    let mut contributed: Vec<&str> = state.signals().iter().map(|signal| signal.module).collect();
    contributed.sort_unstable();
    contributed.dedup();
    let configured = settings
        .permission_signal
        .modules
        .as_ref()
        .map(|names| json!(names));

    let mut payload = state.page_value();
    if let Value::Object(fields) = &mut payload {
        fields.insert("version".to_owned(), json!(env!("CARGO_PKG_VERSION")));
        fields.insert(
            "storageWithdrawn".to_owned(),
            json!(state.storage_withdrawn()),
        );
        fields.insert(
            "modules".to_owned(),
            json!({
                "contributed": contributed,
                "configured": configured,
            }),
        );
        fields.insert("policy".to_owned(), json!(settings.permissions.policy));
    }
    payload
}

/// The permissions page, or its JSON when `path` is
/// [`PERMISSIONS_JSON_PATH`], for a request whose permissions have been
/// resolved.
///
/// Never stored, because each answer is the asking request's own, and
/// readable from any origin, so a page on another site can render it. A
/// request from another origin carries no cookies, so what that page reads is
/// the resolution of a request with no stored signal.
///
/// # Panics
///
/// Never in practice, because the status and every header are fixed, valid
/// values.
#[must_use]
pub fn permissions_response(
    state: &PermissionState,
    settings: &Settings,
    path: &str,
) -> Response<EdgeBody> {
    let payload = permissions_payload(state, settings);
    let (content_type, body) = if path == PERMISSIONS_JSON_PATH {
        ("application/json", payload.to_string())
    } else {
        (
            "text/html; charset=utf-8",
            render_page("Permissions", &payload),
        )
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, HeaderValue::from_static(content_type))
        .header(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))
        .header(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        )
        .body(EdgeBody::from(body.into_bytes()))
        .expect("should build permissions response")
}

/// Answers [`PERMISSIONS_PAGE_PATH`] or [`PERMISSIONS_JSON_PATH`] for `req`.
///
/// The permissions are resolved for this request as a page's would be, from
/// its signals and its location, and then shown. Nothing is created or
/// written for the reader.
///
/// # Errors
///
/// When the request's state cannot be read, which is when the selected Edge
/// Cookie module cannot be built or the `Cookie` header is not valid UTF-8,
/// the same as for a page.
pub async fn handle_permissions(
    settings: &Settings,
    services: &RuntimeServices,
    req: &Request<EdgeBody>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    let state = EcContext::read_from_request_resolving_geo(settings, req, services).await?;
    Ok(permissions_response(
        state.permissions(),
        settings,
        req.uri().path(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::{Permission, PermissionSet, ValidSignal};
    use crate::platform::test_support::noop_services;
    use crate::tdl::Tdl;
    use crate::test_support::tests::create_test_settings;

    fn state_with_signal() -> PermissionState {
        PermissionState::new(PermissionSet::none().with(Permission::StoreOnDevice))
            .with_signals(vec![ValidSignal::new("example", "example", "CPabc123")].into())
            .with_tdls(
                vec![
                    Tdl::new("https://terms.example.com/marketing/2.txt")
                        .expect("should accept the test locator"),
                ]
                .into(),
            )
    }

    fn body_of(response: Response<EdgeBody>) -> String {
        String::from_utf8(
            response
                .into_body()
                .into_bytes()
                .unwrap_or_default()
                .to_vec(),
        )
        .expect("the body should be text")
    }

    /// A reader comes to see which signal produced the answer, so the signal
    /// and the module that read it must both be there.
    #[test]
    fn the_payload_names_the_signal_and_the_module_that_read_it() {
        let payload = permissions_payload(&state_with_signal(), &create_test_settings());

        assert_eq!(payload["signals"][0]["module"], "example");
        assert_eq!(payload["signals"][0]["scheme"], "example");
        assert_eq!(payload["signals"][0]["value"], "CPabc123");
        assert_eq!(payload["modules"]["contributed"], json!(["example"]));
        assert_eq!(
            payload["tdls"],
            json!(["https://terms.example.com/marketing/2.txt"])
        );
        assert_eq!(payload["set"], json!(["necessary.operations.storage"]));
        assert_eq!(payload["storageWithdrawn"], json!(false));
        assert_eq!(payload["version"], json!(env!("CARGO_PKG_VERSION")));
    }

    /// The page shape is spelled once, on the state, so the endpoint cannot
    /// drift from what a page is given.
    #[test]
    fn the_payload_holds_everything_the_page_is_given() {
        let state = state_with_signal();
        let payload = permissions_payload(&state, &create_test_settings());

        let page = state.page_value();
        for (key, value) in page.as_object().expect("the page shape is an object") {
            assert_eq!(
                &payload[key], value,
                "`{key}` should be what the page is given"
            );
        }
    }

    /// A module that ran and found nothing and a module that was never
    /// configured are different facts, and somebody asking why a permission
    /// is not set needs to tell them apart.
    #[test]
    fn configured_and_contributed_modules_are_reported_separately() {
        let mut settings = create_test_settings();
        settings.permission_signal.modules = Some(vec!["example".to_owned(), "other".to_owned()]);

        let payload = permissions_payload(&state_with_signal(), &settings);

        assert_eq!(payload["modules"]["contributed"], json!(["example"]));
        assert_eq!(
            payload["modules"]["configured"],
            json!(["example", "other"])
        );
    }

    /// With no modules named the selection is the adapter's, which core
    /// cannot see, so it says nothing and does not invent a list.
    #[test]
    fn configured_is_null_when_the_publisher_named_no_modules() {
        let mut settings = create_test_settings();
        settings.permission_signal.modules = None;

        let payload = permissions_payload(&state_with_signal(), &settings);

        assert!(
            payload["modules"]["configured"].is_null(),
            "should not guess which modules the adapter offers"
        );
    }

    /// A name is a file's stem, which the configuration page shows with the
    /// file's SHA-256, so a publisher matches the answer to one exact file.
    #[test]
    fn the_payload_names_the_policy_whose_rules_it_resolved() {
        let mut settings = create_test_settings();
        settings.permissions.policy = Some("standard".to_owned());

        let payload = permissions_payload(&state_with_signal(), &settings);

        assert_eq!(payload["policy"], json!("standard"));
    }

    /// "No signal was found" is itself the answer somebody came for.
    #[test]
    fn a_resolution_with_no_signals_still_answers() {
        let payload = permissions_payload(&PermissionState::default(), &create_test_settings());

        assert_eq!(payload["signals"], json!([]));
        assert_eq!(payload["set"], json!([]));
        assert_eq!(payload["awaiting"], json!([]));
        assert_eq!(payload["modules"]["contributed"], json!([]));
    }

    #[test]
    fn a_withdrawal_of_storage_is_shown_as_one() {
        let state = PermissionState::default().with_storage_withdrawn(true);

        let payload = permissions_payload(&state, &create_test_settings());

        assert_eq!(payload["storageWithdrawn"], json!(true));
    }

    #[test]
    fn the_json_address_answers_data_and_the_page_address_a_page() {
        let settings = create_test_settings();
        let state = state_with_signal();

        let data = permissions_response(&state, &settings, PERMISSIONS_JSON_PATH);
        assert_eq!(data.status(), StatusCode::OK);
        assert_eq!(data.headers()[header::CONTENT_TYPE], "application/json");
        assert_eq!(data.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(data.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
        let parsed: Value =
            serde_json::from_str(&body_of(data)).expect("the data form should be JSON");
        assert_eq!(parsed, permissions_payload(&state, &settings));

        let page = permissions_response(&state, &settings, PERMISSIONS_PAGE_PATH);
        assert_eq!(
            page.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        assert_eq!(page.headers()[header::CACHE_CONTROL], "no-store");
        let html = body_of(page);
        assert!(html.contains("<h1>Permissions</h1>"), "should be the page");
        assert!(html.contains("CPabc123"), "should show the signal");
    }

    /// The page carries a signal value straight from the request, so
    /// anything that could close the surrounding tag is escaped.
    #[test]
    fn the_page_escapes_a_hostile_signal_value() {
        let state = PermissionState::new(PermissionSet::none())
            .with_signals(vec![ValidSignal::new("example", "example", "</pre><script>x()")].into());

        let html = body_of(permissions_response(
            &state,
            &create_test_settings(),
            PERMISSIONS_PAGE_PATH,
        ));

        assert!(
            !html.contains("<script>x()"),
            "should not let a signal value open a tag"
        );
        assert!(html.contains("&lt;/pre&gt;&lt;script&gt;x()"));
    }

    /// The handler resolves the asking request's own permissions, so its
    /// answer is the one a page request from the same reader gets.
    #[tokio::test]
    async fn the_handler_resolves_the_permissions_of_the_request_that_asks() {
        let settings = create_test_settings();
        let services = noop_services();
        let request = |path: &str| {
            Request::builder()
                .method("GET")
                .uri(format!("https://publisher.example{path}"))
                .body(EdgeBody::empty())
                .expect("should build the request")
        };

        let response = handle_permissions(&settings, &services, &request(PERMISSIONS_JSON_PATH))
            .await
            .expect("should answer");
        assert_eq!(response.status(), StatusCode::OK);
        let payload: Value =
            serde_json::from_str(&body_of(response)).expect("the data form should be JSON");
        let state = EcContext::read_from_request_resolving_geo(&settings, &request("/"), &services)
            .await
            .expect("should read the request's state");
        assert_eq!(
            payload,
            permissions_payload(state.permissions(), &settings),
            "should be the resolution a page request from the same reader gets"
        );

        let page = handle_permissions(&settings, &services, &request(PERMISSIONS_PAGE_PATH))
            .await
            .expect("should answer");
        assert_eq!(
            page.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8",
            "the page address should answer a page"
        );
    }
}
