//! The rustls crypto provider this CLI's HTTPS clients depend on.

/// Install the default rustls crypto provider, once per process.
///
/// **Call this before building any `reqwest` client.** Without it the first HTTPS request
/// panics with "No provider set" — at runtime, on the operator's machine, not at compile
/// time here.
///
/// This crate's `reqwest` is built with a `-no-provider` rustls feature on purpose: it
/// already links `aws-lc-rs` through `reqwest` 0.13, and letting `reqwest` 0.12 pull `ring`
/// as well would compile two providers, which makes rustls's default ambiguous and panics
/// the dev proxy. The cost of that choice is that somebody must install the default, and
/// this is the one place that does.
///
/// Idempotent: a second call returns `Err` because one is already installed, which is not
/// a failure.
pub(crate) fn install_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}
