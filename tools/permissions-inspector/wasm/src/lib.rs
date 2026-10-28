//! The permissions bit of Trusted Server, compiled to WebAssembly for the
//! inspector page. Inputs in, resulting permissions out, through the same
//! functions the server runs: `read_consent` asks the signal modules to read
//! the strings from the cookies and header the page fills in, and
//! `assemble_permissions` resolves the policy.

use std::sync::{Arc, OnceLock};

use http::{HeaderMap, HeaderName, HeaderValue, header};
use serde::Deserialize;
use serde_json::json;
use trusted_server_core::consent::jurisdiction::Jurisdiction;
use trusted_server_core::consent::read_consent;
use trusted_server_core::ec::consent::{GeoStatus, assemble_permissions};
use trusted_server_core::evidence::OwnedRequestInfo;
use trusted_server_core::module_context::ModuleContext;
use trusted_server_core::permission_signal::PermissionSignalModule;
use trusted_server_core::permissions::{Permission, PermissionMaps};
use trusted_server_core::platform::GeoInfo;

/// The signal modules the inspector asks, in the order every adapter offers
/// them when `[permission-signal] modules` names none.
fn modules() -> Vec<Arc<dyn PermissionSignalModule>> {
    vec![
        Arc::new(trusted_server_permission_signal_gpc::GpcModule::new()),
        Arc::new(trusted_server_permission_signal_gpp::GppSaleOptOutModule::new()),
        Arc::new(trusted_server_permission_signal_us_privacy::UsPrivacyModule::new()),
        Arc::new(trusted_server_permission_signal_tcf::TcfModule::new()),
    ]
}

/// The inspector's evaluation request.
#[derive(Deserialize)]
struct EvalInput {
    /// `located`, `none`, or `failed`.
    geo: String,
    country: Option<String>,
    region: Option<String>,
    tc: Option<String>,
    gpp: Option<String>,
    us_privacy: Option<String>,
    #[serde(default)]
    gpc: bool,
}

/// The repository's sample policy, which the page's repository samples tab
/// evaluates.
const SAMPLE_POLICY: &str = include_str!("../../../../config/permissions/sample.yaml");

/// The sample's rules, parsed once.
fn sample_maps() -> &'static PermissionMaps {
    static MAPS: OnceLock<PermissionMaps> = OnceLock::new();
    MAPS.get_or_init(|| {
        PermissionMaps::from_yaml(SAMPLE_POLICY)
            .expect("should parse the repository's sample policy")
    })
}

fn eval_json(input: &str) -> String {
    let input: EvalInput = match serde_json::from_str(input) {
        Ok(input) => input,
        Err(e) => return json!({"ok": false, "error": e.to_string()}).to_string(),
    };
    let evidence = evidence_for(&input);
    let modules = modules();
    // The page's repository mode evaluates the repository's sample.
    let maps = sample_maps();
    // The jurisdiction the modules read the signals under, as the server
    // detects it from the location the page enters.
    let detected = match input.geo.as_str() {
        "failed" => Jurisdiction::Unknown,
        "none" => maps.default_jurisdiction(),
        _ => maps.jurisdiction_for(input.country.as_deref(), input.region.as_deref()),
    };
    // The page stands for no request, so the modules are asked in the empty
    // context with the evidence the entered signals make.
    let ctx = read_consent(
        &ModuleContext::empty().with_evidence(&evidence),
        &modules,
        detected,
    );
    let context = ModuleContext::empty()
        .with_consent(&ctx)
        .with_evidence(&evidence);
    let (state, jurisdiction) = match input.geo.as_str() {
        "failed" => {
            let state = assemble_permissions(maps, &context, GeoStatus::Failed, &modules);
            (state, "unknown".to_string())
        }
        "none" => {
            let state = assemble_permissions(maps, &context, GeoStatus::NoLocation, &modules);
            (state, jurisdiction_name(maps.default_jurisdiction()))
        }
        _ => {
            let info = GeoInfo {
                city: String::new(),
                country: input.country.clone().unwrap_or_default(),
                continent: String::new(),
                latitude: 0.0,
                longitude: 0.0,
                metro_code: 0,
                region: input.region.clone().filter(|r| !r.is_empty()),
                asn: None,
            };
            let state = assemble_permissions(maps, &context, GeoStatus::Located(&info), &modules);
            let jurisdiction = jurisdiction_name(
                maps.jurisdiction_for(input.country.as_deref(), input.region.as_deref()),
            );
            (state, jurisdiction)
        }
    };
    let set: Vec<&'static str> = Permission::all()
        .filter(|p| state.is_set(*p))
        .map(Permission::as_str)
        .collect();
    json!({
        "ok": true,
        "jurisdiction": jurisdiction,
        "set": set,
        "tcf_decoded": trusted_server_permission_signal_tcf::effective(&ctx).is_some(),
        // No `malformed_record`. Whether an absent, unreadable or expired
        // signal changes the permissions is each module's own decision,
        // taken in its answer, so nothing at this level can report one and
        // `false` would read as "none was found".
    })
    .to_string()
}

/// The evidence a request carrying the entered signals would give: the
/// strings in the cookies their modules read and the GPC header.
fn evidence_for(input: &EvalInput) -> OwnedRequestInfo {
    let entered = |value: &Option<String>| {
        value
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let cookies: Vec<String> = [
        (
            trusted_server_permission_signal_tcf::COOKIE_EUCONSENT_V2,
            entered(&input.tc),
        ),
        (
            trusted_server_permission_signal_gpp::COOKIE_GPP,
            entered(&input.gpp),
        ),
        (
            trusted_server_permission_signal_us_privacy::COOKIE_US_PRIVACY,
            entered(&input.us_privacy),
        ),
    ]
    .into_iter()
    .filter_map(|(name, value)| value.map(|value| format!("{name}={value}")))
    .collect();
    let mut headers = HeaderMap::new();
    if let Ok(value) = HeaderValue::from_str(&cookies.join("; "))
        && !cookies.is_empty()
    {
        headers.insert(header::COOKIE, value);
    }
    if input.gpc {
        headers.insert(
            HeaderName::from_static(trusted_server_permission_signal_gpc::HEADER_SEC_GPC),
            HeaderValue::from_static("1"),
        );
    }
    OwnedRequestInfo::new(String::new(), headers)
}

fn jurisdiction_name(j: trusted_server_core::consent::jurisdiction::Jurisdiction) -> String {
    let name = format!("{j:?}").to_lowercase();
    let name = name.split('(').next().unwrap_or(&name).to_string();
    name.replace("usstate", "us-state")
        .replace("nonregulated", "non-regulated")
}

fn validate_json(yaml: &str) -> String {
    match PermissionMaps::from_yaml(yaml) {
        Ok(_) => json!({"ok": true}).to_string(),
        Err(e) => json!({"ok": false, "error": e.to_string()}).to_string(),
    }
}

fn meta_json() -> String {
    json!({
        "version": env!("TS_CORE_VERSION"),
        "commit": env!("TS_CORE_COMMIT"),
        "date": env!("TS_CORE_DATE"),
        "branch": env!("TS_CORE_BRANCH"),
    })
    .to_string()
}

/// Leaks a length-prefixed buffer the host reads and then frees.
fn out(s: String) -> *mut u8 {
    let bytes = s.into_bytes();
    let mut buf = Vec::with_capacity(4 + bytes.len());
    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(&bytes);
    let ptr = buf.as_mut_ptr();
    core::mem::forget(buf);
    ptr
}

#[unsafe(no_mangle)]
pub extern "C" fn ts_alloc(len: usize) -> *mut u8 {
    let mut buf = vec![0u8; len];
    let ptr = buf.as_mut_ptr();
    core::mem::forget(buf);
    ptr
}

/// # Safety
/// `ptr` must come from `ts_alloc` or an `out` buffer with capacity `len`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ts_free(ptr: *mut u8, len: usize) {
    unsafe { drop(Vec::from_raw_parts(ptr, len, len)) };
}

/// # Safety
/// `ptr`/`len` must describe a valid UTF-8 JSON buffer from `ts_alloc`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ts_eval(ptr: *const u8, len: usize) -> *mut u8 {
    let input = unsafe { core::slice::from_raw_parts(ptr, len) };
    out(eval_json(core::str::from_utf8(input).unwrap_or("{}")))
}

/// # Safety
/// `ptr`/`len` must describe a valid UTF-8 YAML buffer from `ts_alloc`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ts_validate(ptr: *const u8, len: usize) -> *mut u8 {
    let input = unsafe { core::slice::from_raw_parts(ptr, len) };
    out(validate_json(core::str::from_utf8(input).unwrap_or("")))
}

#[unsafe(no_mangle)]
pub extern "C" fn ts_meta() -> *mut u8 {
    out(meta_json())
}
