use std::cell::Cell;
use std::sync::Arc;

use http::{HeaderMap, HeaderValue, Method, header};

use super::*;
use crate::ec::module::HmacModule;
use crate::evidence::OwnedRequestInfo;
use crate::permissions::{Permission, PermissionSet, PermissionState};
use crate::platform::test_support::{noop_services, noop_services_with_client_ip};
use crate::redacted::Redacted;

/// The permission the gated values of these tests need.
fn storage() -> PermissionSet {
    PermissionSet::none().with(Permission::StoreOnDevice)
}

fn geo() -> GeoInfo {
    GeoInfo {
        city: "Bristol".to_owned(),
        country: "GB".to_owned(),
        continent: "Europe".to_owned(),
        latitude: 51.45,
        longitude: -2.58,
        metro_code: 0,
        region: Some("BST".to_owned()),
        asn: None,
    }
}

fn device() -> DeviceSignals {
    DeviceSignals::derive_ua_only("Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/146.0")
}

fn evidence() -> OwnedRequestInfo {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::USER_AGENT,
        HeaderValue::from_static("ExampleAgent/1.0"),
    );
    OwnedRequestInfo::new("203.0.113.7".to_owned(), headers)
}

/// A module that is no more than the functions these tests hand to it.
struct Module;

#[test]
fn the_request_reaches_every_function_that_names_it() {
    let method = Method::POST;
    let request =
        ModuleRequest::new(&method, "publisher.example", "https", "/page").with_query("a=1");
    let context = ModuleContext::new(request);
    let call = context.call("module", PermissionSet::none());

    let seen = call
        .inject(&Module, |_: &Module, request: ModuleRequest<'_>| {
            (
                request.method().clone(),
                request.host().to_owned(),
                request.scheme().to_owned(),
                request.path().to_owned(),
                request.query().to_owned(),
            )
        })
        .expect("the request is in every context");

    assert_eq!(
        seen,
        (
            Method::POST,
            "publisher.example".to_owned(),
            "https".to_owned(),
            "/page".to_owned(),
            "a=1".to_owned(),
        )
    );
}

#[test]
fn each_value_the_context_carries_reaches_a_function_that_names_it() {
    let method = Method::GET;
    let evidence = evidence();
    let client = ClientInfo {
        tls_protocol: Some("TLSv1.3".to_owned()),
        ..ClientInfo::default()
    };
    let permissions = PermissionState::new(storage());
    let consent = ConsentContext::default();
    let mut settings = Settings::default();
    settings.publisher.domain = "publisher.example".to_owned();
    let services_ip: std::net::IpAddr = "203.0.113.9".parse().expect("should parse an IP");
    let services = noop_services_with_client_ip(services_ip);
    let context = ModuleContext::new(ModuleRequest::new(&method, "h", "https", "/"))
        .with_evidence(&evidence)
        .with_client(&client)
        .with_permissions(&permissions)
        .with_consent(&consent)
        .with_settings(&settings)
        .with_services(&services);
    let call = context.call("module", PermissionSet::none());

    let first = call
        .inject(
            &Module,
            |_: &Module,
             evidence: &dyn RequestInfo,
             client: &ClientInfo,
             permissions: &PermissionState,
             consent: &ConsentContext| {
                (
                    evidence.user_agent().to_owned(),
                    evidence.client_ip().to_owned(),
                    client.tls_protocol.clone(),
                    permissions.is_set(Permission::StoreOnDevice),
                    consent.jurisdiction.clone(),
                )
            },
        )
        .expect("every value named is carried");
    assert_eq!(
        first,
        (
            "ExampleAgent/1.0".to_owned(),
            "203.0.113.7".to_owned(),
            Some("TLSv1.3".to_owned()),
            true,
            ConsentContext::default().jurisdiction,
        )
    );

    let second = call
        .inject(
            &Module,
            |_: &Module, settings: &Settings, services: &RuntimeServices| {
                (
                    settings.publisher.domain.clone(),
                    services.client_info().client_ip,
                )
            },
        )
        .expect("every value named is carried");
    assert_eq!(second, ("publisher.example".to_owned(), Some(services_ip)));
}

#[test]
fn a_value_the_context_does_not_carry_is_none_or_skips_the_call() {
    let method = Method::GET;
    let context = ModuleContext::new(ModuleRequest::new(&method, "h", "https", "/"));
    let call = context.call("module", PermissionSet::none());

    let absent = call
        .inject(
            &Module,
            |_: &Module,
             evidence: Option<&dyn RequestInfo>,
             settings: Option<&Settings>,
             permissions: Option<&PermissionState>| {
                (
                    evidence.is_none(),
                    settings.is_none(),
                    permissions.is_none(),
                )
            },
        )
        .expect("an optional parameter never skips the call");
    assert_eq!(absent, (true, true, true));

    let called = Cell::new(false);
    let skipped = call
        .inject(&Module, |_: &Module, _settings: &Settings| called.set(true))
        .expect_err("a parameter the context does not carry skips the call");
    assert!(!called.get(), "the function is not called");
    assert_eq!(skipped.value(), "settings");
    assert_eq!(skipped.reason(), WithheldReason::Absent);
}

/// The three gated values, each set on a context that needs `requires` for
/// it, read back through `Option` parameters by a module declaring
/// `declared`.
fn gated_values(
    permissions: Option<&PermissionState>,
    requires: PermissionSet,
    declared: PermissionSet,
) -> (bool, bool, bool) {
    let method = Method::GET;
    let geo = geo();
    let device = device();
    let mut context = ModuleContext::new(ModuleRequest::new(&method, "h", "https", "/"))
        .with_geo(&geo, requires)
        .with_device(&device, requires)
        .with_edge_cookie("an-id", requires);
    if let Some(permissions) = permissions {
        context = context.with_permissions(permissions);
    }
    context
        .call("module", declared)
        .inject(
            &Module,
            |_: &Module,
             geo: Option<&GeoInfo>,
             device: Option<&DeviceSignals>,
             edge_cookie: Option<EdgeCookie<'_>>| {
                (geo.is_some(), device.is_some(), edge_cookie.is_some())
            },
        )
        .expect("an optional parameter never skips the call")
}

#[test]
fn a_gated_value_reaches_a_module_that_declares_its_permission_and_is_granted_it() {
    let granted = PermissionState::new(storage());

    assert_eq!(
        gated_values(Some(&granted), storage(), storage()),
        (true, true, true)
    );
}

#[test]
fn a_gated_value_is_withheld_from_a_module_that_does_not_declare_its_permission() {
    let granted = PermissionState::new(storage());

    assert_eq!(
        gated_values(Some(&granted), storage(), PermissionSet::none()),
        (false, false, false),
        "granted on the request is not enough without the declaration"
    );
}

#[test]
fn a_gated_value_is_withheld_when_its_permission_is_not_set() {
    let refused = PermissionState::default();

    assert_eq!(
        gated_values(Some(&refused), storage(), storage()),
        (false, false, false),
        "the declaration is not enough without the permission being set"
    );
    assert_eq!(
        gated_values(None, storage(), storage()),
        (false, false, false),
        "and nothing is granted where no permissions are resolved"
    );
}

#[test]
fn a_value_whose_producer_declares_nothing_reaches_every_module() {
    assert_eq!(
        gated_values(None, PermissionSet::none(), PermissionSet::none()),
        (true, true, true),
        "evidence is not rationed"
    );
}

#[test]
fn a_withheld_value_skips_a_call_that_names_it_and_says_why() {
    let method = Method::GET;
    let geo = geo();
    let refused = PermissionState::default();
    let context = ModuleContext::new(ModuleRequest::new(&method, "h", "https", "/"))
        .with_geo(&geo, storage())
        .with_permissions(&refused);
    let called = Cell::new(false);

    let undeclared = context
        .call("module", PermissionSet::none())
        .inject(&Module, |_: &Module, _geo: &GeoInfo| called.set(true))
        .expect_err("geo is withheld from a module that does not declare storage");
    let unset = context
        .call("module", storage())
        .inject(&Module, |_: &Module, _geo: &GeoInfo| called.set(true))
        .expect_err("geo is withheld while storage is not set");

    assert!(!called.get(), "a skipped call is never made");
    assert_eq!(undeclared.reason(), WithheldReason::NotDeclared(storage()));
    assert_eq!(unset.reason(), WithheldReason::NotGranted(storage()));
    assert_eq!(
        undeclared.to_string(),
        "geo is withheld because the module does not declare necessary.operations.storage, \
         which its use needs"
    );
}

#[test]
fn an_argument_of_the_callers_own_is_passed_after_the_module() {
    let method = Method::GET;
    let context = ModuleContext::new(ModuleRequest::new(&method, "h", "https", "/path"));

    let answer = context
        .call("module", PermissionSet::none())
        .inject_with(
            &Module,
            String::from("argument"),
            |_: &Module, argument: String, request: ModuleRequest<'_>| {
                format!("{argument} {}", request.path())
            },
        )
        .expect("nothing is withheld");

    assert_eq!(answer, "argument /path");
}

impl Module {
    async fn classify(&self, evidence: &dyn RequestInfo, services: &RuntimeServices) -> String {
        format!(
            "{} {}",
            evidence.user_agent(),
            services.client_info().client_ip.is_some()
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the most parameters a module function may name"
    )]
    fn eight(
        &self,
        _request: ModuleRequest<'_>,
        _evidence: &dyn RequestInfo,
        _permissions: &PermissionState,
        _settings: &Settings,
        _services: &RuntimeServices,
        _geo: Option<&GeoInfo>,
        _device: Option<&DeviceSignals>,
        _edge_cookie: Option<EdgeCookie<'_>>,
    ) -> usize {
        8
    }
}

#[test]
fn an_async_function_is_called_and_its_future_comes_back() {
    let method = Method::GET;
    let evidence = evidence();
    let services = noop_services_with_client_ip("203.0.113.9".parse().expect("should parse an IP"));
    let context = ModuleContext::new(ModuleRequest::new(&method, "h", "https", "/"))
        .with_evidence(&evidence)
        .with_services(&services);
    let call = context.call("module", PermissionSet::none());

    let future = call
        .inject(&Module, Module::classify)
        .expect("nothing is withheld");

    assert_eq!(futures::executor::block_on(future), "ExampleAgent/1.0 true");
}

#[test]
fn a_function_may_name_eight_values_and_a_tuple_groups_more() {
    let method = Method::GET;
    let evidence = evidence();
    let permissions = PermissionState::default();
    let deployment = Settings::default();
    let request_services = noop_services();
    let context = ModuleContext::new(ModuleRequest::new(&method, "h", "https", "/"))
        .with_evidence(&evidence)
        .with_permissions(&permissions)
        .with_settings(&deployment)
        .with_services(&request_services);
    let call = context.call("module", PermissionSet::none());

    assert_eq!(call.inject(&Module, Module::eight).expect("all carried"), 8);
    let grouped = call
        .inject(
            &Module,
            |_: &Module, (settings, services): (&Settings, &RuntimeServices)| {
                (
                    std::ptr::eq(settings, &deployment),
                    std::ptr::eq(services, &request_services),
                )
            },
        )
        .expect("a tuple is passed when every value in it is");
    assert_eq!(
        grouped,
        (true, true),
        "the tuple holds the context's own values"
    );
}

#[test]
fn the_request_state_carries_each_value_gated_by_what_its_producer_declares() {
    let method = Method::GET;
    let mut state = EcContext::new_for_test(Some("an-id".to_owned()), ConsentContext::default())
        .with_module_for_test(Arc::new(HmacModule::new(Redacted::new(
            "test-secret-key-32-bytes-minimum".to_owned(),
        ))))
        .with_geo_for_test(geo());
    state.set_device_signals(device());
    let services =
        noop_services().with_device_module(Arc::new(crate::ec::device::BuiltinDeviceModule));
    let context = ModuleContext::new(ModuleRequest::new(&method, "h", "https", "/"))
        .with_request_state(&state, &services);

    let undeclared = context
        .call("module", PermissionSet::none())
        .inject(
            &Module,
            |_: &Module,
             geo: Option<&GeoInfo>,
             device: Option<&DeviceSignals>,
             edge_cookie: Option<EdgeCookie<'_>>,
             permissions: &PermissionState| {
                (
                    geo.map(|geo| geo.country.clone()),
                    device.is_some(),
                    edge_cookie.map(|id| id.as_str().to_owned()),
                    permissions.is_set(Permission::StoreOnDevice),
                )
            },
        )
        .expect("nothing named is required");
    assert_eq!(
        undeclared,
        (Some("GB".to_owned()), true, None, true),
        "geo and the device signals come from producers declaring nothing, and the \
         identifier needs what the HMAC module declares"
    );

    let declared = context
        .call("module", storage())
        .inject(&Module, |_: &Module, edge_cookie: EdgeCookie<'_>| {
            edge_cookie.as_str().to_owned()
        })
        .expect("a module declaring storage is passed the identifier");
    assert_eq!(declared, "an-id");
}

#[test]
fn a_route_answers_forbidden_only_for_a_value_its_permissions_withhold() {
    use crate::error::IntoHttpResponse as _;

    let status = |reason| {
        error_stack::Report::<TrustedServerError>::from(Withheld::new("geo", reason))
            .current_context()
            .status_code()
    };

    assert_eq!(
        status(WithheldReason::NotDeclared(storage())),
        http::StatusCode::FORBIDDEN
    );
    assert_eq!(
        status(WithheldReason::NotGranted(storage())),
        http::StatusCode::FORBIDDEN
    );
    assert_eq!(
        status(WithheldReason::Absent),
        http::StatusCode::INTERNAL_SERVER_ERROR,
        "a value absent where the route runs is the route's fault, not the reader's"
    );
    assert_eq!(
        status(WithheldReason::Unresolved),
        http::StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[test]
fn a_module_request_carries_the_prefix_request_addressing_removed() {
    let mut request = http::Request::builder()
        .method(Method::GET)
        .uri("https://yoursite.example/news/")
        .body(EdgeBody::empty())
        .expect("should build the request");
    let client = ClientInfo::default();
    assert_eq!(ModuleRequest::of(&request, &client).removed_prefix(), None);

    request
        .extensions_mut()
        .insert(crate::addressing::RemovedPathPrefix::new(
            "/www.example.com".to_owned(),
        ));
    let view = ModuleRequest::of(&request, &client);
    assert_eq!(view.removed_prefix(), Some("/www.example.com"));
    assert_eq!(view.path(), "/news/");

    let held = ResolvedRequest::of(&request, &client);
    assert_eq!(
        held.view().removed_prefix(),
        Some("/www.example.com"),
        "a held request should keep the removed prefix"
    );
}
