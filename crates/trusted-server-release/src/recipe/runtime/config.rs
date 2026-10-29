//! Fixed Viceroy local configuration for the final-byte runtime gate.
//!
//! The gate reuses the integration harness fixtures verbatim: its Viceroy
//! template (KV, secret and JWKS stores with generated test-only values) and
//! its readable Trusted Server app config. Like the harness generator, it
//! overrides only `publisher.origin_url`, wraps the app config in a version 1
//! blob envelope and injects it into the template's single generated-store
//! marker under the config store ID the tracked `edgezero.toml` declares.
//!
//! The producer has no dependency on the core crate, so the app config is
//! converted from TOML to JSON here and its envelope digest is computed with
//! the envelope's canonical form: compact JSON with object keys sorted by
//! UTF-8 bytes. Floats and datetimes are rejected rather than canonicalized,
//! so the fixture can never depend on number formatting.
//!
//! # Embedded fixtures
//!
//! The three fixture files are compiled into the producer from the producer's
//! own source tree, not read from the source that was built. The producer may
//! be built from a different revision than the approved source, so
//! [`require_built_source_fixtures`] fails closed unless the built source
//! tracks each file at the exact embedded content.

use std::fmt::Write as _;

use error_stack::Report;
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};

use super::{RuntimeRejection, runtime_error};
use crate::error::ReleaseError;

/// The integration harness Viceroy template.
const VICEROY_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../trusted-server-integration-tests/fixtures/configs/viceroy-template.toml"
));
/// The integration harness readable app config.
const APP_CONFIG: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml"
));
/// The tracked `EdgeZero` manifest naming the default config store.
const EDGEZERO_MANIFEST: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../edgezero.toml"));
/// The single line the generated config store replaces.
const GENERATED_STORES_MARKER: &str = "        # GENERATED_TRUSTED_SERVER_CONFIG_STORES";
/// Fixed envelope timestamp, the same one the harness generator stamps.
const GENERATED_AT: &str = "2026-06-23T00:00:00Z";
/// The only blob envelope version the runtime accepts.
const ENVELOPE_VERSION: u64 = 1;

/// Each embedded fixture with its repository-relative tracked path.
const EMBEDDED_FIXTURES: [(&str, &str); 3] = [
    (
        "crates/trusted-server-integration-tests/fixtures/configs/viceroy-template.toml",
        VICEROY_TEMPLATE,
    ),
    (
        "crates/trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml",
        APP_CONFIG,
    ),
    ("edgezero.toml", EDGEZERO_MANIFEST),
];

/// Requires the built source to track every embedded fixture unchanged.
///
/// `tracked_sha256` returns the SHA-256 of a tracked path's content in the
/// built source, or `None` when the path is not tracked.
///
/// # Errors
///
/// Returns [`RuntimeRejection::Fixture`] naming the first fixture that is
/// untracked or differs from the built source.
pub(in crate::recipe) fn require_built_source_fixtures(
    tracked_sha256: &dyn Fn(&str) -> Option<String>,
) -> Result<(), Report<ReleaseError>> {
    for (path, embedded) in EMBEDDED_FIXTURES {
        let expected = sha256_hex(embedded.as_bytes());
        match tracked_sha256(path) {
            Some(built) if built == expected => {}
            Some(built) => {
                return Err(runtime_error(
                    RuntimeRejection::Fixture,
                    format!(
                        "embedded fixture {path} differs from built source \
                         (embedded {expected}, built {built})"
                    ),
                ));
            }
            None => {
                return Err(runtime_error(
                    RuntimeRejection::Fixture,
                    format!("embedded fixture {path} is not tracked by the built source"),
                ));
            }
        }
    }
    Ok(())
}

/// One generated Viceroy configuration and the digests that identify it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::recipe) struct GeneratedConfig {
    /// The complete Viceroy `local_server` configuration.
    pub(in crate::recipe) text: String,
    /// SHA-256 of [`Self::text`].
    pub(in crate::recipe) sha256: String,
    /// SHA-256 binding the fixed template, app config and store ID.
    pub(in crate::recipe) fixture_sha256: String,
}

/// Returns the default config store ID declared by `edgezero.toml`.
///
/// # Errors
///
/// Returns [`RuntimeRejection::Fixture`] when the manifest does not parse or
/// the ID is missing or not a plain lowercase identifier.
pub(in crate::recipe) fn config_store_id() -> Result<String, Report<ReleaseError>> {
    let fixture = |message: &str| runtime_error(RuntimeRejection::Fixture, message);
    let manifest: toml::Table = toml::from_str(EDGEZERO_MANIFEST)
        .map_err(|error| fixture(&format!("edgezero.toml does not parse: {error}")))?;
    let id = manifest
        .get("stores")
        .and_then(|stores| stores.get("config"))
        .and_then(|config| config.get("default"))
        .and_then(toml::Value::as_str)
        .ok_or_else(|| fixture("edgezero.toml declares no default config store"))?;
    let plain = !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    if !plain {
        return Err(fixture("default config store ID is not a plain identifier"));
    }
    Ok(id.to_owned())
}

/// Converts a TOML value into JSON, rejecting floats and datetimes.
fn toml_to_json(value: &toml::Value) -> Result<Value, Report<ReleaseError>> {
    Ok(match value {
        toml::Value::String(text) => Value::String(text.clone()),
        toml::Value::Integer(number) => Value::from(*number),
        toml::Value::Boolean(flag) => Value::Bool(*flag),
        toml::Value::Array(items) => {
            Value::Array(items.iter().map(toml_to_json).collect::<Result<_, _>>()?)
        }
        toml::Value::Table(table) => table_to_json(table)?,
        toml::Value::Float(_) | toml::Value::Datetime(_) => {
            return Err(runtime_error(
                RuntimeRejection::Fixture,
                "app config fixture holds a float or datetime",
            ));
        }
    })
}

fn table_to_json(table: &toml::Table) -> Result<Value, Report<ReleaseError>> {
    let mut map = Map::new();
    for (key, value) in table {
        map.insert(key.clone(), toml_to_json(value)?);
    }
    Ok(Value::Object(map))
}

/// Returns the integration app config as JSON with `origin_url` replaced.
///
/// # Errors
///
/// Returns [`RuntimeRejection::Fixture`] when the fixture does not parse,
/// holds a float or datetime, or lacks a string `publisher.origin_url`.
pub(in crate::recipe) fn app_config_data(origin_url: &str) -> Result<Value, Report<ReleaseError>> {
    let table: toml::Table = toml::from_str(APP_CONFIG).map_err(|error| {
        runtime_error(
            RuntimeRejection::Fixture,
            format!("app config fixture does not parse: {error}"),
        )
    })?;
    let mut data = table_to_json(&table)?;
    let origin = data
        .pointer_mut("/publisher/origin_url")
        .filter(|value| value.is_string())
        .ok_or_else(|| {
            runtime_error(
                RuntimeRejection::Fixture,
                "app config fixture lacks publisher.origin_url",
            )
        })?;
    *origin = Value::String(origin_url.to_owned());
    Ok(data)
}

/// Writes `value` in the envelope's canonical form.
///
/// # Errors
///
/// Returns [`RuntimeRejection::Fixture`] for a non-integer number.
pub(in crate::recipe) fn write_canonical(
    out: &mut String,
    value: &Value,
) -> Result<(), Report<ReleaseError>> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                let _ = write!(out, "{integer}");
            } else if let Some(integer) = number.as_u64() {
                let _ = write!(out, "{integer}");
            } else {
                return Err(runtime_error(
                    RuntimeRejection::Fixture,
                    "canonical form rejects non-integer numbers",
                ));
            }
        }
        Value::String(text) => write_string(out, text),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(out, item)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_string(out, key);
                out.push(':');
                write_canonical(out, &map[key])?;
            }
            out.push('}');
        }
    }
    Ok(())
}

/// Writes a JSON string with the envelope's fixed escape table.
fn write_string(out: &mut String, text: &str) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000C}' => out.push_str("\\f"),
            control if u32::from(control) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(control));
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

/// Returns the canonical form of `value`.
///
/// # Errors
///
/// Returns [`RuntimeRejection::Fixture`] for a non-integer number.
pub(in crate::recipe) fn canonical(value: &Value) -> Result<String, Report<ReleaseError>> {
    let mut out = String::new();
    write_canonical(&mut out, value)?;
    Ok(out)
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Returns the blob envelope JSON wrapping `data`.
///
/// # Errors
///
/// Returns [`RuntimeRejection::Fixture`] for a non-integer number.
pub(in crate::recipe) fn envelope_json(data: &Value) -> Result<String, Report<ReleaseError>> {
    let sha256 = sha256_hex(canonical(data)?.as_bytes());
    canonical(&serde_json::json!({
        "data": data,
        "generated_at": GENERATED_AT,
        "sha256": sha256,
        "version": ENVELOPE_VERSION,
    }))
}

/// Builds the complete Viceroy configuration for `origin_url`.
///
/// # Errors
///
/// Returns [`RuntimeRejection::Fixture`] when the template does not hold
/// exactly one generated-store marker, the store ID or app config is
/// unusable, or the envelope cannot be embedded as a TOML literal string.
pub(in crate::recipe) fn viceroy_config(
    origin_url: &str,
) -> Result<GeneratedConfig, Report<ReleaseError>> {
    let markers = VICEROY_TEMPLATE.matches(GENERATED_STORES_MARKER).count();
    if markers != 1 {
        return Err(runtime_error(
            RuntimeRejection::Fixture,
            format!("Viceroy template holds {markers} generated-store markers, not one"),
        ));
    }
    let store = config_store_id()?;
    let envelope = envelope_json(&app_config_data(origin_url)?)?;
    if envelope.contains("'''") {
        return Err(runtime_error(
            RuntimeRejection::Fixture,
            "envelope cannot be embedded as a TOML literal string",
        ));
    }
    let block = format!(
        "        [local_server.config_stores.{store}]\n            format = \"inline-toml\"\n        \
         [local_server.config_stores.{store}.contents]\n            {store} = '''{envelope}'''"
    );
    let text = VICEROY_TEMPLATE.replace(GENERATED_STORES_MARKER, &block);
    let fixture_sha256 = sha256_hex(
        format!(
            "viceroy-template.toml {}\ntrusted-server.integration.toml {}\nconfig-store {store}\n",
            sha256_hex(VICEROY_TEMPLATE.as_bytes()),
            sha256_hex(APP_CONFIG.as_bytes()),
        )
        .as_bytes(),
    );
    Ok(GeneratedConfig {
        sha256: sha256_hex(text.as_bytes()),
        text,
        fixture_sha256,
    })
}
