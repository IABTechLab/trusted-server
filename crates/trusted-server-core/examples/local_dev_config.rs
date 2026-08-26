//! Generate a ready-to-use local dev config envelope for the Axum adapter.
//!
//! Reads `trusted-server.example.toml`, replaces the placeholder secrets with
//! random values, flips the flags a local smoke test needs, validates the
//! result through [`trusted_server_core::settings::Settings::from_toml`], and
//! prints the blob envelope JSON that
//! `TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG` expects.
//!
//! The random values are time-and-pid seeded, not cryptographic. This tool
//! exists for throwaway local test instances only; never use its output for a
//! deployed service.
//!
//! Usage:
//!
//! ```text
//! cargo run -p trusted-server-core --example local_dev_config \
//!   --target <host-triple> -- [origin-url] [--realistic]
//! ```
//!
//! `origin-url` defaults to `https://www.example.com`. By default every
//! response is forced `Cache-Control: private, no-store` so the Server-Timing
//! header is visible on all routes; pass `--realistic` to keep the origin's
//! own cache policy instead.

use std::time::{SystemTime, UNIX_EPOCH};

/// Deliberately non-cryptographic generator for local placeholder secrets.
struct WeakRandom(u64);

impl WeakRandom {
    fn from_environment() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("should compute epoch time")
            .subsec_nanos() as u64;
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("should compute epoch time")
            .as_secs();
        let pid = std::process::id() as u64;
        Self(nanos ^ (secs << 20) ^ (pid << 40) ^ 0x9e37_79b9_7f4a_7c15)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn hex(&mut self, chars: usize) -> String {
        let mut out = String::with_capacity(chars);
        while out.len() < chars {
            out.push_str(&format!("{:016x}", self.next()));
        }
        out.truncate(chars);
        out
    }
}

#[allow(clippy::print_stdout, clippy::print_stderr)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let realistic = args.iter().any(|a| a == "--realistic");
    let origin = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .unwrap_or_else(|| "https://www.example.com".to_string());

    let template = std::fs::read_to_string("trusted-server.example.toml")
        .expect("should read trusted-server.example.toml from the repo root");

    let mut random = WeakRandom::from_environment();
    let mut config = template
        .replace(
            "password = \"replace-with-admin-password-32-bytes\"",
            &format!("password = \"{}\"", random.hex(48)),
        )
        .replace(
            "proxy_secret = \"change-me-proxy-secret\"",
            &format!("proxy_secret = \"{}\"", random.hex(48)),
        )
        .replace(
            "passphrase = \"trusted-server-placeholder-secret\"",
            &format!("passphrase = \"{}\"", random.hex(48)),
        )
        .replace(
            "server_timing_enabled = false",
            "server_timing_enabled = true",
        );

    let origin_line = config
        .lines()
        .find(|line| line.starts_with("origin_url = "))
        .expect("should find the origin_url line in the template")
        .to_string();
    config = config.replace(&origin_line, &format!("origin_url = \"{origin}\""));

    if !realistic {
        config = config.replace(
            "# [response_headers]",
            "[response_headers]\n\"Cache-Control\" = \"private, no-store\"",
        );
    }

    let settings = trusted_server_core::settings::Settings::from_toml(&config)
        .expect("should validate the generated local config");
    let data = serde_json::to_value(&settings).expect("should serialize settings");
    let generated_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("should compute epoch time")
        .as_secs()
        .to_string();
    let envelope = edgezero_core::blob_envelope::BlobEnvelope::new(data, generated_at);
    println!(
        "{}",
        serde_json::to_string(&envelope).expect("should serialize the envelope")
    );
    eprintln!(
        "local dev envelope generated: origin={origin} force_private={}",
        !realistic
    );
}
