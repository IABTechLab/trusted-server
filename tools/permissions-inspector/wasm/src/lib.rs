//! The permissions bit of Trusted Server, compiled to WebAssembly for the
//! inspector page. Inputs in, resulting permissions out, through the same
//! functions the server runs: `build_context_from_signals` decodes the raw
//! consent signals and `assemble_permissions` resolves the policy.

use serde::Deserialize;
use serde_json::json;
use trusted_server_core::consent::build_context_from_signals;
use trusted_server_core::consent::types::RawConsentSignals;
use trusted_server_core::ec::consent::{GeoStatus, assemble_permissions};
use trusted_server_core::permissions::{Permission, PermissionMaps};
use trusted_server_core::platform::GeoInfo;

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

fn eval_json(input: &str) -> String {
    let input: EvalInput = match serde_json::from_str(input) {
        Ok(input) => input,
        Err(e) => return json!({"ok": false, "error": e.to_string()}).to_string(),
    };
    let signals = RawConsentSignals {
        raw_tc_string: input.tc.filter(|s| !s.is_empty()),
        raw_gpp_string: input.gpp.filter(|s| !s.is_empty()),
        raw_gpp_sid: None,
        raw_us_privacy: input.us_privacy.filter(|s| !s.is_empty()),
        gpc: input.gpc,
    };
    let ctx = build_context_from_signals(&signals);
    let maps = PermissionMaps::standard();
    let (state, jurisdiction) = match input.geo.as_str() {
        "failed" => {
            let state = assemble_permissions(&ctx, GeoStatus::Failed);
            (state, "unknown".to_string())
        }
        "none" => {
            let state = assemble_permissions(&ctx, GeoStatus::NoLocation);
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
            let state = assemble_permissions(&ctx, GeoStatus::Located(&info));
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
        "tcf_decoded": ctx.tcf.is_some(),
        "malformed_record": ctx.has_malformed_record(),
    })
    .to_string()
}

fn jurisdiction_name(j: trusted_server_core::consent::jurisdiction::Jurisdiction) -> String {
    let name = format!("{j:?}").to_lowercase();
    let name = name.split('(').next().unwrap_or(&name).to_string();
    name.replace("usstate", "us-state").replace("nonregulated", "non-regulated")
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
