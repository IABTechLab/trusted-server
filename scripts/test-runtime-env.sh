#!/usr/bin/env bash
# Compiler fixtures for lint-runtime-env.sh. No workspace dependencies needed.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
export CLIPPY_CONF_DIR="$root/scripts/clippy-runtime-env"
driver=$(rustup which clippy-driver)
rustc=$(rustup which rustc)
count=0

check() {
    local name=$1 expected=$2 diagnostic=${3:-disallowed_methods}
    local source="$scratch/$name.rs" output="$scratch/$name.log" fixture_exit=0
    if [ "$#" -gt 3 ]; then
        set -- --cfg "$4"
    else
        set --
    fi
    cat > "$source"
    TRUSTED_SERVER_DEFAULT_CONFIG_STORE_ID=fixture "$driver" --edition=2024 --crate-type=lib --emit=metadata \
        -A warnings -D clippy::disallowed_methods -D clippy::disallowed_types \
        --extern "helper=$scratch/libhelper.rlib" \
        --extern "config=$scratch/libconfig.rlib" \
        --extern "libc=$scratch/liblibc.rlib" \
        "$@" "$source" -o "$scratch/$name.rmeta" > "$output" 2>&1 || fixture_exit=$?
    if [ "$expected" = pass ]; then
        if [ "$fixture_exit" -ne 0 ]; then
            cat "$output" >&2
            echo "FAIL: $name should compile" >&2
            exit 1
        fi
    elif [ "$fixture_exit" -eq 0 ] || ! command grep -q "$diagnostic" "$output"; then
        cat "$output" >&2
        echo "FAIL: $name should fail with $diagnostic" >&2
        exit 1
    fi
    count=$((count + 1))
}

# An external macro exercises compiler expansion across crate boundaries.
cat > "$scratch/helper.rs" <<'RS'
#[macro_export]
macro_rules! foreign_read { () => { std::env::var("TS_EXTERNAL_MACRO") }; }
RS
"$rustc" --edition=2024 --crate-type=rlib --crate-name helper \
    "$scratch/helper.rs" -o "$scratch/libhelper.rlib"
# A stand-in isolates the equivalent loader's type rule without fetching crates.
cat > "$scratch/config.rs" <<'RS'
mod env {
    #[derive(Default)]
    pub struct Environment;
}
pub use env::Environment;
RS
"$rustc" --edition=2024 --crate-type=rlib --crate-name config \
    "$scratch/config.rs" -o "$scratch/libconfig.rlib"

cat > "$scratch/libc.rs" <<'RS'
unsafe extern "C" {
    pub fn getenv(name: *const i8) -> *mut i8;
    pub fn secure_getenv(name: *const i8) -> *mut i8;
}
RS
"$rustc" --edition=2024 --crate-type=rlib --crate-name libc \
    "$scratch/libc.rs" -o "$scratch/liblibc.rlib"

check valid_rust pass <<'RS'
pub fn control() -> u8 { 1 }
RS
check legacy_config fail <<'RS'
pub fn read() { let _ = std::env::var("TRUSTED_SERVER_CONFIG_EXAMPLE"); }
RS
check legacy_secret fail <<'RS'
pub fn read() { let _ = std::env::var("TRUSTED_SERVER_SECRET_EXAMPLE"); }
RS
check ts_name fail <<'RS'
pub fn read() { let _ = std::env::var("TS_EXAMPLE"); }
RS
check foreign_name fail <<'RS'
pub fn read() { let _ = std::env::var("OTHER_EXAMPLE"); }
RS
check module_alias fail <<'RS'
use std::env;
pub fn read() { let _ = env::var("TS_EXAMPLE"); }
RS
check renamed_module fail <<'RS'
use std::env as environment;
pub fn read() { let _ = environment::var("TS_EXAMPLE"); }
RS
check direct_approved_name fail <<'RS'
pub fn read() { let _ = std::env::var("TRUSTED_SERVER__EXAMPLE"); }
RS
check function_alias fail <<'RS'
use std::env::var as lookup;
pub fn read() { let _ = lookup("TS_EXAMPLE"); }
RS
check reexport fail <<'RS'
mod alias { pub use std::env::var as lookup; }
pub fn read() { let _ = alias::lookup("TS_EXAMPLE"); }
RS
check function_value fail <<'RS'
pub fn read() { let lookup = std::env::var::<&str>; let _ = lookup("TS_EXAMPLE"); }
RS
check local_macro fail <<'RS'
macro_rules! read { () => { std::env::var("TS_EXAMPLE") }; }
pub fn read() { let _ = read!(); }
RS
check external_macro fail <<'RS'
pub fn read() { let _ = helper::foreign_read!(); }
RS
check var_os fail <<'RS'
pub fn read() { let _ = ::std::env::var_os("TS_EXAMPLE"); }
RS
check vars fail <<'RS'
pub fn read() { let _ = std::env::vars(); }
RS
check vars_os fail <<'RS'
use std::env;
pub fn read() { let _ = env::vars_os(); }
RS
check fastly_default_branch fail <<'RS'
#[cfg(not(feature = "reusable-sandbox"))]
pub fn read() { let _ = std::env::var("TS_DEFAULT_ONLY_POISON"); }
RS
check fastly_reusable_branch fail disallowed_methods 'feature="reusable-sandbox"' <<'RS'
#[cfg(feature = "reusable-sandbox")]
pub fn read() { let _ = std::env::var("TS_REUSABLE_ONLY_POISON"); }
RS
check computed_name fail <<'RS'
pub fn read(name: &str) { let _ = std::env::var(name); }
RS
check getenv fail <<'RS'
pub fn read() { let _ = unsafe { libc::getenv(c"TS_EXAMPLE".as_ptr()) }; }
RS
check secure_getenv fail <<'RS'
pub fn read() { let _ = unsafe { libc::secure_getenv(c"TS_EXAMPLE".as_ptr()) }; }
RS
check equivalent_loader fail disallowed_types <<'RS'
pub fn read() { let _ = config::Environment::default(); }
RS
check test_only pass <<'RS'
#[cfg(test)] mod tests {
    fn poison() { let _ = std::env::var("TRUSTED_SERVER_CONFIG_POISON"); }
}
struct Settings;
impl Settings {
    #[cfg(test)] fn from_env() { let _ = std::env::vars_os(); }
}
RS
check poison_inputs pass <<'RS'
pub const POISON: &[&str] = &["TS_EXAMPLE", "TRUSTED_SERVER_SECRET_EXAMPLE"];
#[cfg(test)] fn poison() {
    unsafe { std::env::set_var("TS_EXAMPLE", "ignored"); }
    let _ = std::env::var("TS_EXAMPLE");
}
RS
check compile_time pass <<'RS'
pub const TS_GIT_VERSION: Option<&str> = option_env!("TRUSTED_SERVER__GIT_VERSION");
pub const BUILD: &str = env!("TRUSTED_SERVER_DEFAULT_CONFIG_STORE_ID");
pub const BUILD_ONLY: Option<&str> = option_env!("TS_BUILD_INPUT");
RS

# Test the actual exported macro as a dependency, as Fastly consumes it.
"$rustc" --edition=2024 --crate-type=rlib --crate-name checked \
    "$root/crates/trusted-server-core/src/runtime_env.rs" -o "$scratch/libchecked.rlib"
# Make the checked macro available through the fixture helper's existing extern.
cp "$scratch/libchecked.rlib" "$scratch/libhelper.rlib"
for key in TRUSTED_SERVER__EXAMPLE FASTLY_IS_STAGING FASTLY_SERVICE_VERSION FASTLY_HOSTNAME FASTLY_REGION TRUSTED_SERVER__NATIVE_GEO_PROFILE_FILE; do
    check "checked_$key" pass <<RS
const KEY: &str = "$key";
pub fn read() { let _ = helper::runtime_env!(KEY); }
RS
    check "checked_os_$key" pass <<RS
const KEY: &str = "$key";
pub fn read() { let _ = helper::runtime_env!(KEY, os); }
RS
done
for key in TS_EXAMPLE TS_NATIVE_GEO_PROFILE_FILE TRUSTED_SERVER_CONFIG_EXAMPLE TRUSTED_SERVER_SECRET_EXAMPLE OTHER_EXAMPLE TRUSTED_SERVER_ TRUSTED_SERVER__; do
    check "checked_$key" fail 'runtime environment name is not allowed' <<RS
const KEY: &str = "$key";
pub fn read() { let _ = helper::runtime_env!(KEY); }
RS
    check "checked_os_$key" fail 'runtime environment name is not allowed' <<RS
const KEY: &str = "$key";
pub fn read() { let _ = helper::runtime_env!(KEY, os); }
RS
done
check checked_computed fail 'non-constant' <<'RS'
pub fn read(name: &str) { let _ = helper::runtime_env!(name); }
RS
check checked_os_computed fail 'non-constant' <<'RS'
pub fn read(name: &str) { let _ = helper::runtime_env!(name, os); }
RS
check checked_runtime_built fail 'non-constant' <<'RS'
pub fn read() { let name = format!("TS_{}", "EXAMPLE"); let _ = helper::runtime_env!(&name); }
RS

echo "Runtime environment fixtures: $count passed"
