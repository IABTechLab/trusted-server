use std::path::Path;

use error_stack::Report;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use super::{Output, PbsError, Result, identifier, read_text};

/// Deliberately partial: inspecting PBS requirements must not require unrelated TS settings.
#[derive(Default, Deserialize)]
struct Source {
    #[serde(default)]
    integrations: Integrations,
}

#[derive(Default, Deserialize)]
struct Integrations {
    prebid: Option<Prebid>,
}

#[derive(Default, Deserialize)]
struct Prebid {
    enabled: Option<bool>,
    server_url: Option<String>,
    account_id: Option<String>,
    timeout_ms: Option<u32>,
    test_mode: Option<bool>,
    debug: Option<bool>,
    #[serde(default, deserialize_with = "bidder_list")]
    bidders: Vec<String>,
    #[serde(default, deserialize_with = "bidder_list")]
    client_side_bidders: Vec<String>,
    #[serde(default)]
    bundle: Bundle,
    #[serde(default)]
    bid_param_override_rules: Vec<toml::Value>,
}

#[derive(Default, Deserialize)]
struct Bundle {
    #[serde(default)]
    adapters: Vec<String>,
    #[serde(default)]
    user_id_modules: Vec<String>,
}

/// Mirror the private core list deserializer without expanding runtime defaults.
/// Parity tests compare these accepted representations with `PrebidIntegrationConfig`.
///
/// # Errors
/// Rejects malformed lists, invalid numeric indexes, and non-string items.
fn bidder_list<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Vec<String>, D::Error> {
    match Value::deserialize(deserializer)? {
        Value::Array(values) => {
            serde_json::from_value(Value::Array(values)).map_err(serde::de::Error::custom)
        }
        Value::Object(values) => {
            let mut indexed = Vec::with_capacity(values.len());
            for (index, value) in values {
                let index = index.parse::<usize>().map_err(serde::de::Error::custom)?;
                let value: String =
                    serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                indexed.push((index, value));
            }
            indexed.sort_by_key(|(index, _)| *index);
            Ok(indexed.into_iter().map(|(_, value)| value).collect())
        }
        Value::String(value) => {
            let text = value.trim();
            let bracketed = text.starts_with('[') && text.ends_with(']');
            if bracketed && let Ok(values) = serde_json::from_str::<Vec<String>>(text) {
                return Ok(values);
            }
            let parts = if bracketed {
                text[1..text.len() - 1]
                    .trim()
                    .split(',')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
            } else if text.contains(',') {
                text.split(',')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .collect()
            } else {
                vec![text]
            };
            parts
                .into_iter()
                .map(|part| {
                    serde_json::from_str(&format!("\"{}\"", part.replace('"', "\\\"")))
                        .map_err(serde::de::Error::custom)
                })
                .collect()
        }
        _ => Err(serde::de::Error::custom(
            "expected bidder list, indexed map, or list string",
        )),
    }
}

/// Inspect selected local fields, preserving the source and redacting account/parameter values.
///
/// # Errors
/// Rejects invalid TOML or malformed identifiers with sanitized messages.
pub(super) fn inspect(path: &Path) -> Result<Output> {
    let text = read_text(path, 2 * 1024 * 1024)?;
    let source: Source = toml::from_str(&text).map_err(|_| {
        Report::new(PbsError::Input(
            "cannot parse Trusted Server TOML; source details withheld",
        ))
    })?;
    let present = source.integrations.prebid.is_some();
    let prebid = source.integrations.prebid.unwrap_or_default();
    for name in prebid
        .bidders
        .iter()
        .chain(&prebid.client_side_bidders)
        .chain(&prebid.bundle.adapters)
        .chain(&prebid.bundle.user_id_modules)
    {
        if !identifier(name) {
            return Err(Report::new(PbsError::Input(
                "invalid bidder or identity-module identifier",
            )));
        }
    }
    let requirements: Vec<_> = prebid
        .bidders
        .iter()
        .map(|bidder| {
            json!({
                "bidder": bidder,
                "source_key": "integrations.prebid.bidders",
                "host_secret_requirement": "unresolved",
                "partner_authorization": "unresolved"
            })
        })
        .collect();
    let warnings = [
        "Local file only: confirm environment, remote configuration, and request-time overrides.",
        "Omitted fields/defaults are not expanded; empty candidate lists are not proof of no demand.",
        "Disabled integrations and browser bundle adapters do not authorize PBS activation.",
        "Host secret requirements need adapter metadata verified against the selected PBS release.",
    ];
    let mut details = vec![
        format!(
            "Prebid section present: {present}; enabled explicitly: {:?}",
            prebid.enabled
        ),
        format!("Server bidder candidates: {}", prebid.bidders.join(", ")),
        format!(
            "Client-side bidders: {}",
            prebid.client_side_bidders.join(", ")
        ),
        format!(
            "Browser bundle adapters: {}",
            prebid.bundle.adapters.join(", ")
        ),
        format!(
            "Bid-parameter rules: {}; values withheld",
            prebid.bid_param_override_rules.len()
        ),
    ];
    details.extend(warnings.iter().map(|warning| (*warning).to_owned()));
    Ok(Output {
        failure: None,
        summary: "Local PBS requirements discovery; no files changed or AWS calls made".to_owned(),
        details,
        data: json!({
            "source": path,
            "source_section": "integrations.prebid",
            "section_present": present,
            "enabled_explicit": prebid.enabled,
            "server_url_configured": prebid.server_url.is_some(),
            "account_id_configured": prebid.account_id.is_some(),
            "timeout_ms_explicit": prebid.timeout_ms,
            "test_mode_explicit": prebid.test_mode,
            "debug_explicit": prebid.debug,
            "server_bidder_candidates": requirements,
            "client_side_bidders": prebid.client_side_bidders,
            "bundle_adapters": prebid.bundle.adapters,
            "identity_modules": prebid.bundle.user_id_modules,
            "bid_param_override_rule_count": prebid.bid_param_override_rules.len(),
            "warnings": warnings
        }),
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn discovery_classifies_bidders_redacts_values_and_preserves_source() {
        let dir = tempfile::tempdir().expect("should create temp directory");
        let path = dir.path().join("trusted-server.toml");
        let source = r#"
[integrations.prebid]
enabled = false
server_url = "https://user:NEVER_PRINT_ME@pbs.example.com/path?token=NEVER_PRINT_ME"
account_id = "NEVER_PRINT_ME"
bidders = ["serverbidder"]
client_side_bidders = ["browserbidder"]
[[integrations.prebid.bid_param_override_rules]]
set = { placementId = "NEVER_PRINT_ME" }
[integrations.prebid.bundle]
adapters = ["bundlebidder"]
user_id_modules = ["sharedIdSystem"]
"#;
        fs::write(&path, source).expect("should write fixture");
        let report = inspect(&path).expect("should inspect config");
        assert_eq!(report.data["enabled_explicit"], false);
        assert_eq!(
            report.data["server_bidder_candidates"][0]["bidder"],
            "serverbidder"
        );
        assert_eq!(report.data["client_side_bidders"][0], "browserbidder");
        assert_eq!(report.data["bundle_adapters"][0], "bundlebidder");
        assert_eq!(
            report.data["server_bidder_candidates"][0]["host_secret_requirement"],
            "unresolved"
        );
        for json in [false, true] {
            let mut output = Vec::new();
            report
                .write(json, &mut output)
                .expect("should render report");
            assert!(!String::from_utf8_lossy(&output).contains("NEVER_PRINT_ME"));
        }
        assert_eq!(
            fs::read_to_string(path).expect("should read fixture"),
            source
        );
    }

    #[test]
    fn invalid_toml_never_exposes_parser_snippets() {
        let dir = tempfile::tempdir().expect("should create temp directory");
        let path = dir.path().join("invalid.toml");
        fs::write(&path, "secret = NEVER_PRINT_ME").expect("should write fixture");
        let error = inspect(&path).err().expect("should reject invalid TOML");
        assert!(!format!("{error:?}").contains("NEVER_PRINT_ME"));
    }

    #[test]
    fn accepts_the_runtime_bidder_list_encodings() {
        for input in [
            "['examplebidder', 'otherbidder']",
            "'examplebidder,otherbidder'",
            "'[examplebidder, otherbidder]'",
            "'[\"examplebidder\", \"otherbidder\"]'",
            "{ '10' = 'otherbidder', '2' = 'examplebidder' }",
        ] {
            let text = format!(
                "[integrations.prebid]\nserver_url='https://pbs.example.com'\nbidders={input}\nclient_side_bidders={input}\n"
            );
            let file = tempfile::NamedTempFile::new().expect("should create config");
            fs::write(file.path(), &text).expect("should write config");
            let runtime: trusted_server_core::integrations::prebid::PrebidIntegrationConfig =
                toml::from_str(&format!("client_side_bidders={input}"))
                    .expect("runtime should accept encoding");
            let output = inspect(file.path()).expect("inspect should accept runtime encoding");
            let candidates: Vec<_> = output.data["server_bidder_candidates"]
                .as_array()
                .expect("should report candidates")
                .iter()
                .map(|candidate| {
                    candidate["bidder"]
                        .as_str()
                        .expect("should identify bidder")
                })
                .collect();
            assert_eq!(candidates, ["examplebidder", "otherbidder"]);
            assert_eq!(
                output.data["client_side_bidders"],
                json!(runtime.client_side_bidders)
            );
        }
    }

    #[test]
    fn missing_section_stays_unresolved_instead_of_enabling_defaults() {
        let dir = tempfile::tempdir().expect("should create temp directory");
        let path = dir.path().join("empty.toml");
        fs::write(&path, "").expect("should write fixture");
        let output = inspect(&path).expect("should inspect empty file");
        assert_eq!(output.data["section_present"], false);
        assert!(output.data["enabled_explicit"].is_null());
    }
}
