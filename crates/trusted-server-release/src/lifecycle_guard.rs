//! Linux-only N-API guard for the trusted npm lifecycle observer.
//!
//! Compile this approved source with the selected native Rust toolchain using
//! `--crate-type cdylib --edition 2024`. The resulting `.node` file is a measured
//! tool input, loaded before the observer receives its private authentication
//! key. It is not a standalone executable or a qualification receipt.

#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(not(target_os = "linux"))]
compile_error!("the npm lifecycle guard requires Linux");

use core::ffi::{c_char, c_int, c_ulong, c_void};
use core::ptr;

const PR_SET_DUMPABLE: c_int = 4;
const PR_GET_DUMPABLE: c_int = 3;

unsafe extern "C" {
    fn prctl(option: c_int, ...) -> c_int;
    fn umask(mask: u32) -> u32;
    fn napi_throw_error(env: *mut c_void, code: *const c_char, message: *const c_char) -> c_int;
    fn napi_get_boolean(env: *mut c_void, value: bool, result: *mut *mut c_void) -> c_int;
    fn napi_set_named_property(
        env: *mut c_void,
        object: *mut c_void,
        name: *const c_char,
        value: *mut c_void,
    ) -> c_int;
}

/// Protects the observer from same-user process-memory and `/proc` descriptor
/// access before it accepts sensitive input.
///
/// # Safety
///
/// Node must call this N-API registration entry with a live environment and
/// exports object. The library must come from the selected read-only tool root.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_register_module_v1(
    env: *mut c_void,
    exports: *mut c_void,
) -> *mut c_void {
    // SAFETY: umask accepts any mode mask. This dedicated observer process
    // selects npm's fixed extraction mode before spawning any install hooks.
    unsafe {
        umask(0o022);
    }
    // SAFETY: Linux prctl accepts these fixed options and zero arguments.
    let protected = unsafe {
        prctl(
            PR_SET_DUMPABLE,
            0 as c_ulong,
            0 as c_ulong,
            0 as c_ulong,
            0 as c_ulong,
        ) == 0
            && prctl(
                PR_GET_DUMPABLE,
                0 as c_ulong,
                0 as c_ulong,
                0 as c_ulong,
                0 as c_ulong,
            ) == 0
    };
    let mut value = ptr::null_mut();
    // SAFETY: Node supplies valid N-API handles; all strings are static C strings.
    let exported = protected
        && unsafe {
            napi_get_boolean(env, true, &mut value) == 0
                && napi_set_named_property(env, exports, c"protected".as_ptr(), value) == 0
        };
    if !exported {
        // SAFETY: The live Node environment and static NUL-terminated message
        // meet N-API's preconditions. A pending exception rejects require().
        unsafe {
            napi_throw_error(
                env,
                ptr::null(),
                c"Cannot protect lifecycle observer".as_ptr(),
            );
        }
        return ptr::null_mut();
    }
    exports
}
