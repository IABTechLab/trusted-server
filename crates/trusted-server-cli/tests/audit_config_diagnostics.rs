//! Command-path coverage for safe generation config diagnostics.

use std::fs;
use std::process::Command;

#[test]
fn generate_rejects_invalid_config_without_echoing_source() {
    for (source, guidance) in [
        (
            "passphrase = FICTIONAL_SECRET_SENTINEL\n",
            "fix the TOML syntax and re-run",
        ),
        (
            "[creative_opportunities]\ngam_network_id = \"123\"\nslot = \"FICTIONAL_SECRET_SENTINEL\"\n",
            "Fix the section (or delete it) and re-run",
        ),
    ] {
        for dry_run in [false, true] {
            let directory = tempfile::tempdir().expect("should create temporary config directory");
            let path = directory.path().join("trusted-server.toml");
            fs::write(&path, source).expect("should write malformed config");
            let mut command = Command::new(env!("CARGO_BIN_EXE_ts"));
            command
                .args([
                    "audit",
                    "ad-templates",
                    "generate",
                    "https://publisher.example.com",
                    "--app-config",
                ])
                .arg(&path)
                .current_dir(directory.path());
            if dry_run {
                command.arg("--dry-run");
            }

            let output = command.output().expect("should run generation command");
            let stderr = String::from_utf8(output.stderr).expect("should decode stderr");

            assert_eq!(
                output.status.code(),
                Some(2),
                "should reject config before browser launch"
            );
            assert!(
                output.stdout.is_empty(),
                "should not print config to stdout"
            );
            assert!(
                !stderr.contains("FICTIONAL_SECRET_SENTINEL"),
                "should not echo source in errors"
            );
            assert!(
                stderr.contains(guidance),
                "should explain how to repair the config: {stderr}"
            );
            assert_eq!(
                fs::read_to_string(&path).expect("should read config"),
                source,
                "should preserve invalid config"
            );
        }
    }
}
