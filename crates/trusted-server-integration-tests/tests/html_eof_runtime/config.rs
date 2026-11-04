use std::path::Path;

use error_stack::{Report, ResultExt as _};
use toml::Value;

use super::{HarnessError, HarnessResult};

pub(super) const SECRETS: &[(&str, &str)] = &[
    ("handler_password", "example-handler-password-32-bytes-ok"),
    (
        "publisher_proxy_secret",
        "example-publisher-secret-32-bytes-ok",
    ),
    ("ec_passphrase", "example-cookie-passphrase-32-bytes-ok"),
];

fn invalid(field: &str) -> Report<HarnessError> {
    Report::new(HarnessError::Configuration)
        .attach(format!("missing or invalid template field: {field}"))
}

pub(super) fn parse(template: &str) -> HarnessResult<Value> {
    toml::from_str(template).change_context(HarnessError::Configuration)
}

pub(super) fn serialize(value: &Value) -> HarnessResult<String> {
    toml::to_string(value).change_context(HarnessError::Configuration)
}

pub(super) fn replace(value: &mut Value, path: &[&str], replacement: Value) -> HarnessResult<()> {
    let mut field = value;
    for key in path {
        field = field
            .get_mut(*key)
            .ok_or_else(|| invalid(&path.join(".")))?;
    }
    if field.type_str() != replacement.type_str() {
        return Err(invalid(&path.join(".")));
    }
    *field = replacement;
    Ok(())
}

fn table<'a>(
    value: &'a mut Value,
    path: &[&str],
) -> HarnessResult<&'a mut toml::map::Map<String, Value>> {
    let mut field = value;
    for key in path {
        field = field
            .get_mut(*key)
            .ok_or_else(|| invalid(&path.join(".")))?;
    }
    field.as_table_mut().ok_or_else(|| invalid(&path.join(".")))
}

pub(super) fn fixture_config(
    template: &str,
    case: &str,
    origin: u16,
    bid: u16,
) -> HarnessResult<String> {
    let mut value = parse(template)?;
    for (key, replacement) in [
        ("domain", "127.0.0.1".to_owned()),
        ("cookie_domain", "127.0.0.1".to_owned()),
        ("origin_url", format!("http://127.0.0.1:{origin}")),
    ] {
        replace(&mut value, &["publisher", key], Value::String(replacement))?;
    }
    let publisher = table(&mut value, &["publisher"])?;
    publisher.insert(
        "allow_async_html_finish".to_owned(),
        Value::Boolean(case != "disabled"),
    );
    publisher.insert(
        "html_finalization_timeout_ms".to_owned(),
        Value::Integer(750),
    );
    replace(
        &mut value,
        &["auction", "enabled"],
        Value::Boolean(case == "auction"),
    )?;
    if case == "auction" {
        let providers = table(&mut value, &["auction", "providers"])?;
        if providers.is_empty() {
            return Err(invalid("auction.providers"));
        }
        // No listener runs on this port: this controls empty-bid seam assembly,
        // rather than successful or delayed bidder collection.
        for (_, provider) in providers.iter_mut() {
            replace(
                provider,
                &["endpoint"],
                Value::String(format!("https://127.0.0.1:{bid}/bid")),
            )?;
        }
        let slot = parse(
            "id = 'example-slot'\npage_patterns = ['/article']\nformats = [{ width = 300, height = 250 }]\n",
        )?;
        table(&mut value, &["creative_opportunities"])?
            .insert("slot".to_owned(), Value::Array(vec![slot]));
    }
    let lookup = if matches!(case, "auction" | "streaming") {
        "success"
    } else {
        case
    };
    let fixture = parse(&format!(
        "enabled = {}\nlookup_url = 'http://127.0.0.1:{origin}/{lookup}'\ntransport_timeout_ms = 30000\nstreaming = {}\n",
        case != "disabled",
        case == "streaming"
    ))?;
    table(&mut value, &["integrations"])?.insert("html_finalization_fixture".to_owned(), fixture);
    validate_fixture(&value, case, origin, bid)?;
    serialize(&value)
}

pub(super) fn validate_fixture(
    value: &Value,
    case: &str,
    origin: u16,
    bid: u16,
) -> HarnessResult<()> {
    let lookup = if matches!(case, "auction" | "streaming") {
        "success"
    } else {
        case
    };
    let required = [
        (
            vec!["publisher", "domain"],
            Value::String("127.0.0.1".to_owned()),
        ),
        (
            vec!["publisher", "cookie_domain"],
            Value::String("127.0.0.1".to_owned()),
        ),
        (
            vec!["publisher", "origin_url"],
            Value::String(format!("http://127.0.0.1:{origin}")),
        ),
        (
            vec!["publisher", "allow_async_html_finish"],
            Value::Boolean(case != "disabled"),
        ),
        (
            vec!["publisher", "html_finalization_timeout_ms"],
            Value::Integer(750),
        ),
        (
            vec!["integrations", "html_finalization_fixture", "enabled"],
            Value::Boolean(case != "disabled"),
        ),
        (
            vec!["integrations", "html_finalization_fixture", "lookup_url"],
            Value::String(format!("http://127.0.0.1:{origin}/{lookup}")),
        ),
        (
            vec![
                "integrations",
                "html_finalization_fixture",
                "transport_timeout_ms",
            ],
            Value::Integer(30000),
        ),
        (
            vec!["integrations", "html_finalization_fixture", "streaming"],
            Value::Boolean(case == "streaming"),
        ),
        (
            vec!["auction", "enabled"],
            Value::Boolean(case == "auction"),
        ),
    ];
    for (path, expected) in required {
        let actual = path.iter().try_fold(value, |field, key| field.get(*key));
        if actual != Some(&expected) {
            return Err(invalid(&path.join(".")));
        }
    }
    if case == "auction" {
        let providers = value
            .get("auction")
            .and_then(|v| v.get("providers"))
            .and_then(Value::as_table)
            .ok_or_else(|| invalid("auction.providers"))?;
        if providers.is_empty()
            || providers.values().any(|provider| {
                provider.get("endpoint").and_then(Value::as_str)
                    != Some(format!("https://127.0.0.1:{bid}/bid").as_str())
            })
        {
            return Err(invalid("auction endpoint"));
        }
    }
    Ok(())
}

pub(super) fn fixture_manifest(
    template: &str,
    root: &Path,
    workspace: &Path,
) -> HarnessResult<String> {
    let mut value = parse(template)?;
    let entry = value
        .get("app")
        .and_then(|v| v.get("entry"))
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("app.entry"))?;
    let entry = root.join(entry);
    replace(
        &mut value,
        &["app", "entry"],
        Value::String(entry.to_string_lossy().into_owned()),
    )?;
    for (name, adapter) in table(&mut value, &["adapters"])? {
        let adapter = table(adapter, &["adapter"])?;
        for key in ["crate", "manifest"] {
            let old = adapter
                .get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| invalid(key))?;
            let path = if key == "manifest" && matches!(name.as_str(), "fastly" | "spin") {
                workspace.join(format!("{name}.toml"))
            } else {
                root.join(old)
            };
            adapter.insert(
                key.to_owned(),
                Value::String(path.to_string_lossy().into_owned()),
            );
        }
    }
    serialize(&value)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = include_str!("../../../../trusted-server.example.toml");
    const MANIFEST: &str = include_str!("../../../../edgezero.toml");

    #[test]
    fn fixture_config_is_loopback_only_with_explicit_permission() {
        for case in ["disabled", "success", "hung", "auction", "streaming"] {
            let config = fixture_config(CONFIG, case, 12345, 12346)
                .expect("should generate isolated configuration");
            let parsed: toml::Value = toml::from_str(&config).expect("should parse configuration");
            assert_eq!(
                parsed["publisher"]["domain"].as_str(),
                Some("127.0.0.1"),
                "should isolate the publisher"
            );
            assert_eq!(
                parsed["publisher"]["origin_url"].as_str(),
                Some("http://127.0.0.1:12345"),
                "should isolate the origin"
            );
            assert_eq!(
                parsed["publisher"]["allow_async_html_finish"].as_bool(),
                Some(case != "disabled"),
                "should set explicit permission"
            );
            assert_eq!(
                parsed["publisher"]["html_finalization_timeout_ms"].as_integer(),
                Some(750),
                "should configure the fixture budget"
            );
        }
    }

    #[test]
    fn fixture_config_rejects_missing_required_template_fields() {
        let result = fixture_config(
            "[publisher]\ndomain = 'example.com'",
            "success",
            12345,
            12346,
        );
        assert!(result.is_err(), "should reject incomplete templates");
    }

    #[test]
    fn fixture_validation_rejects_external_endpoints() {
        let config = fixture_config(CONFIG, "auction", 12345, 12346)
            .expect("should generate auction configuration");
        let mut value = parse(&config).expect("should parse generated configuration");
        let provider = table(&mut value, &["auction", "providers"])
            .expect("should contain a provider")
            .iter_mut()
            .next()
            .expect("should have a provider")
            .1;
        replace(
            provider,
            &["endpoint"],
            Value::String("https://auction.example.com/bid".to_owned()),
        )
        .expect("should change the controlled endpoint");
        assert!(
            validate_fixture(&value, "auction", 12345, 12346).is_err(),
            "should reject non-loopback auction endpoints"
        );
    }

    #[test]
    fn fixture_manifest_round_trips_paths() {
        let root = Path::new("/tmp/example 🚀 \"quoted\"\\path\nnext");
        let workspace = root.join("workspace");
        let manifest =
            fixture_manifest(MANIFEST, root, &workspace).expect("should isolate manifests");
        let parsed: toml::Value = toml::from_str(&manifest).expect("should parse quoted paths");
        assert_eq!(
            parsed["app"]["entry"].as_str(),
            root.join("crates/trusted-server-core").to_str(),
            "should preserve all path characters"
        );
        assert_eq!(
            parsed["adapters"]["spin"]["adapter"]["manifest"].as_str(),
            workspace.join("spin.toml").to_str(),
            "should use an isolated runtime manifest"
        );
    }
}
