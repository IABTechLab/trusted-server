//! Effective Docker seccomp profile derived from the source-owned baseline.
//!
//! The baseline is the reviewed `moby/profiles` `seccomp/v0.2.1` default
//! profile retained in `scripts/release/qualification/`. Its SHA-256 is
//! checked before use. The effective profile preserves the baseline's default
//! `SCMP_ACT_ERRNO`/`EPERM` action, native architectures and every other
//! syscall rule except process-memory and descriptor-transfer allowances.
//! It empties every `subArchitectures` list, so non-native ABIs
//! (x86 and x32 on `x86_64`, where sockets go through `socketcall`; 32-bit
//! ARM on `aarch64`) are not admitted by the filter at all and fail with the
//! filter's bad-architecture action. It removes every `socket`, `socketpair`
//! and `socketcall` allowance and adds exactly one rule allowing
//! `socket`/`socketpair` with argument zero equal to `AF_UNIX`, so INET,
//! INET6, netlink and packet sockets fail with `EPERM`. `--network none` alone still permits loopback INET
//! sockets, so this layer is what turns them into `EPERM`.

use error_stack::Report;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::error::ReleaseError;

/// The reviewed baseline bytes, compiled in from the source tree.
const BASELINE: &[u8] =
    include_bytes!("../../../../scripts/release/qualification/seccomp-default.json");
/// SHA-256 of the reviewed baseline recorded in the qualification README.
pub(crate) const BASELINE_SHA256: &str =
    "536529b665dd0972c37bfb569f5d4ac8a53592e7b00752bc39ff063ca9864c74";
/// Socket-creating syscalls whose baseline allowances are removed.
const SOCKET_SYSCALLS: [&str; 3] = ["socket", "socketpair", "socketcall"];
/// An install hook must not inspect or modify its trusted observer's memory.
const PROCESS_ACCESS_SYSCALLS: [&str; 6] = [
    "ptrace",
    "process_vm_readv",
    "process_vm_writev",
    "pidfd_getfd",
    "kcmp",
    "process_madvise",
];
/// `AF_UNIX` on every Linux architecture.
const AF_UNIX: u64 = 1;

fn seccomp_error(message: impl std::fmt::Display) -> Report<ReleaseError> {
    Report::new(ReleaseError::Boundary).attach(message.to_string())
}

/// Returns the hex SHA-256 of `bytes`.
#[must_use]
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Returns the baseline after checking its reviewed digest.
///
/// # Errors
///
/// Returns [`ReleaseError::Boundary`] when the compiled-in bytes do not match
/// [`BASELINE_SHA256`].
pub(crate) fn baseline() -> Result<&'static [u8], Report<ReleaseError>> {
    if sha256_hex(BASELINE) != BASELINE_SHA256 {
        return Err(seccomp_error(
            "seccomp baseline digest does not match review",
        ));
    }
    Ok(BASELINE)
}

fn strip_socket_names(rule: &mut Value) -> Result<bool, Report<ReleaseError>> {
    let names = rule
        .get_mut("names")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| seccomp_error("seccomp rule has no names array"))?;
    names.retain(|name| {
        name.as_str().is_none_or(|name| {
            !SOCKET_SYSCALLS.contains(&name) && !PROCESS_ACCESS_SYSCALLS.contains(&name)
        })
    });
    Ok(!names.is_empty())
}

/// Derives the effective profile from `baseline`.
///
/// # Errors
///
/// Returns [`ReleaseError::Boundary`] when the baseline is not JSON, does not
/// default to `SCMP_ACT_ERRNO` with errno `1`, or has a malformed rule.
pub(crate) fn effective(baseline: &[u8]) -> Result<Vec<u8>, Report<ReleaseError>> {
    let mut profile: Value = serde_json::from_slice(baseline)
        .map_err(|error| seccomp_error(format!("seccomp baseline is not JSON: {error}")))?;
    let default_denies = profile.get("defaultAction") == Some(&json!("SCMP_ACT_ERRNO"))
        && profile.get("defaultErrnoRet") == Some(&json!(1));
    if !default_denies {
        return Err(seccomp_error("seccomp baseline does not default to EPERM"));
    }
    let architectures = profile
        .get_mut("archMap")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| seccomp_error("seccomp baseline has no archMap"))?;
    for architecture in architectures {
        let entry = architecture
            .as_object_mut()
            .ok_or_else(|| seccomp_error("seccomp archMap entry is not an object"))?;
        entry.insert(String::from("subArchitectures"), json!([]));
    }
    let rules = profile
        .get_mut("syscalls")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| seccomp_error("seccomp baseline has no syscalls array"))?;
    let mut retained = Vec::with_capacity(rules.len() + 1);
    for mut rule in rules.drain(..) {
        if strip_socket_names(&mut rule)? {
            retained.push(rule);
        }
    }
    retained.push(json!({
        "names": ["socket", "socketpair"],
        "action": "SCMP_ACT_ALLOW",
        "args": [{"index": 0, "value": AF_UNIX, "op": "SCMP_CMP_EQ"}],
    }));
    *rules = retained;
    serde_json::to_vec_pretty(&profile)
        .map_err(|error| seccomp_error(format!("cannot serialize seccomp profile: {error}")))
}
