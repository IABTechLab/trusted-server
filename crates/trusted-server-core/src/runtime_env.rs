//! Compile-time policy for serving code's process-environment reads.

/// Reads a process-environment variable with a statically approved name.
///
/// Accepts a constant `&str` beginning with `TRUSTED_SERVER__`, or one of the
/// explicitly justified Fastly platform names. Runtime-built names and scans
/// are intentionally unsupported. Build scripts and tests are outside this
/// runtime policy; platform bindings and `EdgeZero` configuration are separate.
///
/// # Errors
///
/// Returns [`std::env::VarError`] when the variable is absent or is not Unicode.
///
/// # Examples
///
/// ```no_run
/// let value = trusted_server_core::runtime_env!("TRUSTED_SERVER__EXAMPLE");
/// ```
///
/// Invalid names fail compilation rather than reading the environment:
///
/// ```compile_fail
/// let value = trusted_server_core::runtime_env!("TS_EXAMPLE");
/// ```
#[macro_export]
macro_rules! runtime_env {
    ($name:expr) => {{
        const NAME: &str = {
            let name: &str = $name;
            let allowed = match name.as_bytes() {
                // Fastly staging metadata preserves response tagging and
                // DataDome's existing staging protection-test behavior.
                b"FASTLY_IS_STAGING" => true,
                // Fastly deployment metadata supplies the service-version header.
                b"FASTLY_SERVICE_VERSION" => true,
                // Fastly server metadata supplies the platform's hostname.
                b"FASTLY_HOSTNAME" => true,
                // Fastly server metadata supplies the platform's region.
                b"FASTLY_REGION" => true,
                _ => {
                    let prefix = b"TRUSTED_SERVER__";
                    let bytes = name.as_bytes();
                    let mut allowed = bytes.len() > prefix.len();
                    let mut index = 0;
                    while allowed && index < prefix.len() {
                        allowed = bytes[index] == prefix[index];
                        index += 1;
                    }
                    allowed
                }
            };
            assert!(allowed, "runtime environment name is not allowed");
            name
        };
        // Only this read is exempt: the const assertion checks its exact key.
        #[allow(clippy::disallowed_methods)]
        let value = ::std::env::var(NAME);
        value
    }};
}
