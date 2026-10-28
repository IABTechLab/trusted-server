use std::collections::BTreeSet;

use edgezero_core::blob_envelope::BlobEnvelope;

use super::*;
use crate::config_payload::settings_from_config_blob_with;
use crate::integrations::{IntegrationBuilder, ModuleSecretSetting};
use crate::origin_auth::fixture;
use crate::permissions::{SAMPLE_POLICY_NAME, SAMPLE_POLICY_TEXT, policy_sha256};
use crate::platform::{PlatformError, PlatformSecretStore, PolicyOrigin, StoreId, StoreName};
use crate::secret_resolution::ResolvedSecrets;
use crate::settings::ProxyAssetRoute;
use crate::test_support::tests::{crate_test_settings_str, sample_permissions};

/// Every value this store hands back starts with this, so a view can be
/// searched for all of them at once.
const CANARY: &str = "canary-";

/// Resolves every key name to a value of its own, long enough for any
/// minimum length a secret has.
struct CanaryStore;

impl PlatformSecretStore for CanaryStore {
    fn get_bytes(
        &self,
        _store_name: &StoreName,
        key: &str,
    ) -> Result<Vec<u8>, Report<PlatformError>> {
        Ok(format!("{CANARY}{key}-value-0123456789abcdef").into_bytes())
    }

    fn create(
        &self,
        _store_id: &StoreId,
        _name: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        Ok(())
    }

    fn delete(&self, _store_id: &StoreId, _name: &str) -> Result<(), Report<PlatformError>> {
        Ok(())
    }
}

fn settings() -> Settings {
    Settings::from_toml(&crate_test_settings_str()).expect("should parse test settings")
}

/// The test settings with `extra` appended, as a stored document holds
/// them, so every secret in them is the name of one.
fn document(extra: &str) -> Value {
    document_from(&format!("{}\n{extra}\n", crate_test_settings_str()))
}

/// `toml` as a stored document holds it, with the allowed domains a load
/// needs for the test settings' asset hosts and the permission policy the
/// push writes.
fn document_from(toml: &str) -> Value {
    let mut settings = Settings::from_toml(toml).expect("should parse the document");
    settings.proxy.allowed_domains = vec!["*.example".to_owned(), "*.example.com".to_owned()];
    settings.permissions = sample_permissions();
    serde_json::to_value(&settings).expect("should serialize the document")
}

fn load_with(
    data: Value,
    builders: &[IntegrationBuilder],
) -> Result<Settings, Report<TrustedServerError>> {
    let envelope = BlobEnvelope::new(data, "2026-01-01T00:00:00Z".to_owned());
    let envelope = serde_json::to_string(&envelope).expect("should serialize the envelope");
    settings_from_config_blob_with(
        &envelope,
        &CanaryStore,
        &StoreName::from("ts_secrets"),
        PolicyOrigin::Blob,
        builders,
    )
}

fn load(data: Value) -> Result<Settings, Report<TrustedServerError>> {
    load_with(data, &[])
}

fn view(settings: &Settings) -> ConfigView {
    build_view(settings).expect("should build the view")
}

fn request(uri: &str) -> Request<EdgeBody> {
    Request::builder()
        .uri(uri)
        .body(EdgeBody::empty())
        .expect("should build a request")
}

fn body_text(response: Response<EdgeBody>) -> String {
    let bytes = response
        .into_body()
        .into_bytes()
        .expect("a buffered body")
        .to_vec();
    String::from_utf8(bytes).expect("a text body")
}

/// The JSON and the page the endpoint serves for `settings`.
fn served(settings: &Settings) -> (String, String) {
    (
        body_text(handle_config(settings, &request(CONFIG_JSON_PATH))),
        body_text(handle_config(settings, &request(CONFIG_PAGE_PATH))),
    )
}

/// The value at a concrete path, such as `proxy.asset_routes[0].prefix`.
fn at<'v>(value: &'v Value, path: &str) -> Option<&'v Value> {
    let pattern = PathPattern::parse(path).expect("a concrete path");
    let mut node = value;
    for step in &pattern.steps {
        node = match (step, node) {
            (PatternStep::Key(key), Value::Object(map)) => map.get(key)?,
            (PatternStep::Index(index), Value::Array(items)) => items.get(*index)?,
            _ => return None,
        };
    }
    Some(node)
}

fn is_masked(view: &ConfigView, path: &str) -> bool {
    view.masked.iter().any(|masked| masked == path)
}

/// `value` with every sensitive marker replaced by what it wraps.
fn unmarked(value: &Value) -> Value {
    if let Some(inner) = unmark(value) {
        return unmarked(inner);
    }
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), unmarked(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(unmarked).collect()),
        other => other.clone(),
    }
}

#[test]
fn a_pattern_names_keys_every_element_or_one_element() {
    let every = PathPattern::parse("proxy.asset_routes[].prefix").expect("well formed");
    let one = PathPattern::parse("proxy.asset_routes[1].prefix").expect("well formed");
    let route = |index| {
        vec![
            PathStep::Key("proxy".to_owned()),
            PathStep::Key("asset_routes".to_owned()),
            PathStep::Index(index),
            PathStep::Key("prefix".to_owned()),
        ]
    };

    assert!(every.matches(&route(0)) && every.matches(&route(7)));
    assert!(one.matches(&route(1)));
    assert!(!one.matches(&route(0)), "should name only its own index");
    assert!(
        !every.matches(&route(0)[..3]),
        "should name only values at its own depth"
    );
    assert_eq!(render_path(&route(1)), "proxy.asset_routes[1].prefix");
}

#[test]
fn a_malformed_pattern_is_refused_when_the_document_is_read() {
    for pattern in [
        "",
        "a..b",
        ".a",
        "a.",
        "a[x]",
        "a[",
        "a]",
        "[0]",
        "a b",
        "a[0]b",
        "a[99999999999999999999999]",
    ] {
        let toml = format!(
            "{}\n[inspect]\nhide = [{pattern:?}]\n",
            crate_test_settings_str()
        );

        let error = Settings::from_toml(&toml).expect_err("a malformed pattern is refused");

        assert!(
            format!("{error:?}").contains(&format!("`{pattern}` is not a path pattern")),
            "should name the pattern {pattern:?}: {error:?}"
        );
    }
}

#[test]
fn a_key_the_inspect_section_does_not_have_is_refused_naming_it() {
    let toml = format!("{}\n[inspect]\nshows = []\n", crate_test_settings_str());

    let error = Settings::from_toml(&toml).expect_err("a misspelt key is refused");

    assert!(
        format!("{error:?}").contains("unknown field `shows`"),
        "should name the key: {error:?}"
    );
}

/// Documents that between them hold a secret at every path core declares
/// one, and at both paths the fixture signer declares its own, each a key
/// name the canary store resolves to a value of its own.
fn documents_holding_every_declared_secret() -> Vec<(&'static str, Value)> {
    let mut main = document("");
    main["publisher"]["proxy_secret"] = json!("proxy");
    main["source"]["origin"]["auth"] = json!({
        "type": fixture::SIGNER,
        "realm": "site",
        "key_name": "origin-key",
        "token_name": "origin-token",
    });
    main["ec"]["hmac"]["passphrase"] = json!("hmac");
    main["ec"]["partners"] = json!([{
        "name": "Example Partner",
        "source_domain": "partner.example.com",
        "api_token": "partner-api",
        "pull_sync_enabled": true,
        "pull_sync_url": "https://partner.example.com/sync",
        "pull_sync_allowed_domains": ["partner.example.com"],
        "ts_pull_token": "partner-pull",
    }]);
    main["trusted_client_ip"] = json!({
        "ip_header": "x-ts-client-ip",
        "auth_header": "x-ts-client-ip-auth",
        "shared_secret": "client-ip",
    });
    main["proxy"]["asset_routes"] = json!([{
        "prefix": "/assets/",
        "source": {
            "module": "origin",
            "origin": {
                "url": "https://assets.example.com",
                "auth": {
                    "type": fixture::SIGNER,
                    "realm": "assets",
                    "key_name": "route-key",
                    "token_name": "route-token",
                },
            },
        },
    }]);
    let mut host_signals = document("");
    let ec = host_signals["ec"].as_object_mut().expect("an ec table");
    ec.remove("hmac");
    ec.insert("module".to_owned(), json!("host_signals"));
    ec.insert("host_signals".to_owned(), json!({ "passphrase": "host" }));

    let mut legacy = document("");
    let ec = legacy["ec"].as_object_mut().expect("an ec table");
    ec.remove("hmac");
    ec.remove("module");
    ec.insert("passphrase".to_owned(), json!("legacy"));

    let mut labeled = document("");
    let ec = labeled["ec"].as_object_mut().expect("an ec table");
    ec.remove("hmac");
    ec.insert("module".to_owned(), json!("primary"));
    ec.insert(
        "primary".to_owned(),
        json!({ "implementation": "hmac", "passphrase": "labeled" }),
    );

    vec![
        ("main", main),
        ("host signals", host_signals),
        ("legacy", legacy),
        ("labeled", labeled),
    ]
}

/// A concrete path with every index written as `[]`, as a declaration
/// writes it.
fn generalized(path: &[PathStep]) -> String {
    let mut out = String::new();
    for step in path {
        match step {
            PathStep::Key(key) => {
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(key);
            }
            PathStep::Index(_) => out.push_str("[]"),
        }
    }
    out
}

/// Every secret core declares, and every one the fixture signer declares at
/// the `auth` paths, loaded the way a deployment loads it, is live in the
/// settings and absent from both forms the endpoint serves, and wherever the
/// view still has its path, that path is masked.
#[test]
fn every_declared_core_secret_is_masked_and_never_served() {
    let mut covered = BTreeSet::new();
    for (name, data) in documents_holding_every_declared_secret() {
        let settings = load_with(data, &[fixture::builder()])
            .unwrap_or_else(|error| panic!("{name}: {error:?}"));
        let record = settings.resolved_secrets();
        let normal = serde_json::to_string(&settings).expect("should serialize");
        assert!(
            !record.values().is_empty(),
            "{name}: the load should record the secrets it wrote"
        );
        for value in record.values() {
            assert!(
                normal.contains(value.as_str()),
                "{name}: each resolved secret should be live in the settings"
            );
        }
        let view = view(&settings);
        // A failure lists the paths that are not masked, never the value
        // the view holds at them.
        let mut unmasked = Vec::new();
        let mut unlisted = Vec::new();
        for path in record.paths() {
            let rendered = render_path(path);
            if let Some(value) = at(&view.settings, &rendered) {
                if value != MASK {
                    unmasked.push(rendered.clone());
                }
                if !is_masked(&view, &rendered) {
                    unlisted.push(rendered);
                }
            }
            covered.insert(generalized(path));
        }
        assert_eq!(
            unmasked,
            Vec::<String>::new(),
            "{name}: the view should hold the mask at every secret's path"
        );
        assert_eq!(
            unlisted,
            Vec::<String>::new(),
            "{name}: the view should list every secret's path as masked"
        );
        let (json, page) = served(&settings);
        assert!(!json.contains(CANARY), "{name}: the JSON carries a secret");
        assert!(!page.contains(CANARY), "{name}: the page carries a secret");
    }

    let declared: BTreeSet<String> = TrustedServerAppConfig::secret_fields()
        .iter()
        .chain(crate::module_secrets::secret_fields(&[fixture::builder()]).iter())
        .filter_map(|field| PathPattern::from_segments(&field.path))
        .map(|pattern| pattern.as_str().to_owned())
        .collect();
    let missing: Vec<&String> = declared.difference(&covered).collect();
    assert!(
        missing.is_empty(),
        "every declared secret should be exercised, missing {missing:?}"
    );
    for path in [
        "source.origin.auth.key_name",
        "source.origin.auth.token_name",
        "proxy.asset_routes[].source.origin.auth.key_name",
        "proxy.asset_routes[].source.origin.auth.token_name",
    ] {
        assert!(
            covered.contains(path),
            "the signer's secret at {path} should be declared and masked"
        );
    }
}

/// Test settings holding a value of its own in every field marked
/// sensitive, with those paths and values.
fn settings_with_every_sensitive_field() -> (Settings, Vec<(&'static str, &'static str)>) {
    let expected = vec![
        ("source.origin.url", "https://origin-canary.example.com"),
        ("source.origin.host_header", "host-canary.example.com"),
        (
            "proxy.asset_routes[0].source.origin.url",
            "https://bucket-canary.example.com",
        ),
        ("ec.ec_store", "identity-store-canary"),
        ("auction.creative_store", "creative-store-canary"),
    ];
    let value = |path: &str| {
        expected
            .iter()
            .find(|(at, _)| *at == path)
            .map(|(_, value)| (*value).to_owned())
            .expect("an expected path")
    };
    let mut settings = settings();
    settings.set_origin_source(&crate::source::OriginSettings {
        host_header: Some(value("source.origin.host_header")),
        ..crate::source::OriginSettings::new(value("source.origin.url"))
    });
    settings.proxy.asset_routes = vec![ProxyAssetRoute::new(
        "/assets/",
        value("proxy.asset_routes[0].source.origin.url"),
    )];
    settings.ec.ec_store = Some(value("ec.ec_store"));
    settings.auction.creative_store = value("auction.creative_store");
    (settings, expected)
}

#[test]
fn every_value_sensitive_by_default_is_masked() {
    let (settings, expected) = settings_with_every_sensitive_field();

    let view = view(&settings);
    let (json, page) = served(&settings);

    for (path, value) in expected {
        assert_eq!(at(&view.settings, path), Some(&json!(MASK)), "{path}");
        assert!(is_masked(&view, path), "{path}");
        assert!(!json.contains(value), "the JSON carries {path}");
        assert!(!page.contains(value), "the page carries {path}");
    }
    assert!(
        is_masked(&view, "publisher.proxy_secret"),
        "a Redacted value is masked too: {:?}",
        view.masked
    );
}

#[test]
fn an_ordinary_serialization_is_unchanged() {
    let (settings, expected) = settings_with_every_sensitive_field();

    let normal = serde_json::to_value(&settings).expect("should serialize");
    let marked = to_marked_value(&settings).expect("should serialize");

    assert_eq!(
        normal,
        unmarked(&marked),
        "should differ from a marked serialization only by the markers"
    );
    assert!(!normal.to_string().contains(SENSITIVE_MARKER));
    for (path, value) in expected {
        assert_eq!(at(&normal, path), Some(&json!(value)), "{path}");
    }
    assert_eq!(
        at(&normal, "publisher.proxy_secret"),
        Some(&json!("unit-test-proxy-secret"))
    );
}

#[test]
fn a_stored_document_carries_the_section_only_when_the_publisher_wrote_one() {
    let unwritten = serde_json::to_value(settings()).expect("should serialize");
    let written = document("[inspect]\nhide = [\"publisher.domain\"]");

    assert_eq!(
        unwritten.get("inspect"),
        None,
        "should leave an unwritten section out, so the document is the bytes it was"
    );
    assert_eq!(
        written.get("inspect"),
        Some(&json!({ "config": true, "hide": ["publisher.domain"] })),
        "should store a section the publisher wrote"
    );
    assert!(
        !unwritten.to_string().contains("resolved_secrets"),
        "should never store where the loader wrote secrets"
    );
}

#[test]
fn a_recorded_secret_path_is_masked_without_the_value_scan() {
    let mut settings = settings();
    settings.set_resolved_secrets(ResolvedSecrets::recorded(
        vec![vec![
            PathStep::Key("publisher".to_owned()),
            PathStep::Key("domain".to_owned()),
        ]],
        Vec::new(),
    ));

    let view = view(&settings);

    assert_eq!(at(&view.settings, "publisher.domain"), Some(&json!(MASK)));
    assert!(is_masked(&view, "publisher.domain"));
}

#[test]
fn a_secret_copied_into_an_undeclared_field_is_masked_by_the_value_scan() {
    let secret = "canary-copied-secret-value";
    let mut settings = settings();
    settings
        .response_headers
        .insert("x-debug".to_owned(), format!("prefix {secret} suffix"));
    settings
        .response_headers
        .insert(format!("x-{secret}"), "open".to_owned());
    settings.set_resolved_secrets(ResolvedSecrets::recorded(
        Vec::new(),
        vec![secret.to_owned()],
    ));

    let view = view(&settings);
    let (json, page) = served(&settings);

    assert_eq!(
        at(&view.settings, "response_headers.x-debug"),
        Some(&json!(MASK))
    );
    assert!(is_masked(&view, "response_headers.x-debug"));
    assert!(
        is_masked(&view, "response_headers.XXXX"),
        "a key holding a secret is masked too: {:?}",
        view.masked
    );
    assert!(!json.contains(secret) && !page.contains(secret));
}

#[test]
fn a_path_masked_beneath_a_key_holding_a_secret_lists_the_renamed_key() {
    struct Case {
        name: &'static str,
        real_mask_key: bool,
        renamed: [&'static str; 2],
    }
    let secret = "canary-key-secret-value";
    // Upper case names sort ahead of a real `XXXX` key, so they are
    // renamed before it is reached.
    let keys = [format!("A-{secret}"), format!("B-{secret}")];
    let path_to = |key: &str| {
        vec![
            PathStep::Key("response_headers".to_owned()),
            PathStep::Key(key.to_owned()),
        ]
    };
    for case in [
        Case {
            name: "two keys holding the secret",
            real_mask_key: false,
            renamed: ["response_headers.XXXX", "response_headers.XXXX-2"],
        },
        Case {
            name: "beside a real key named XXXX",
            real_mask_key: true,
            renamed: ["response_headers.XXXX-2", "response_headers.XXXX-3"],
        },
    ] {
        let mut settings = settings();
        for key in &keys {
            settings
                .response_headers
                .insert(key.clone(), "open".to_owned());
        }
        if case.real_mask_key {
            settings
                .response_headers
                .insert(MASK.to_owned(), "real".to_owned());
        }
        settings.set_resolved_secrets(ResolvedSecrets::recorded(
            keys.iter().map(|key| path_to(key)).collect(),
            vec![secret.to_owned()],
        ));

        let view = view(&settings);
        let (json, page) = served(&settings);

        assert!(
            !view.masked.iter().any(|path| path.contains(secret)),
            "{}: no masked path should carry the secret: {:?}",
            case.name,
            view.masked
        );
        for renamed in case.renamed {
            assert!(
                is_masked(&view, renamed),
                "{}: should list {renamed}: {:?}",
                case.name,
                view.masked
            );
            assert_eq!(
                at(&view.settings, renamed),
                Some(&json!(MASK)),
                "{}",
                case.name
            );
        }
        if case.real_mask_key {
            assert_eq!(
                at(&view.settings, "response_headers.XXXX"),
                Some(&json!("real")),
                "{}: should keep the real key's value",
                case.name
            );
            assert!(
                !is_masked(&view, "response_headers.XXXX"),
                "{}: should not list the real key as masked: {:?}",
                case.name,
                view.masked
            );
        }
        assert!(
            !json.contains(secret) && !page.contains(secret),
            "{}: should publish no secret",
            case.name
        );
    }
}

/// Two asset routes, for a list holding values masked by default and a
/// secret.
const ASSET_ROUTES: &str = r#"
    [[proxy.asset_routes]]
    prefix = "/assets/"
    source.module = "origin"
    source.origin.url = "https://assets.example.com"

    [proxy.asset_routes.source.origin.auth]
    type = "fixture_scheme"
    realm = "assets"
    key_name = "route-key"

    [[proxy.asset_routes]]
    prefix = "/media/"
    source.module = "origin"
    source.origin.url = "https://media.example.com"
"#;

#[test]
fn show_reveals_a_value_masked_by_default() {
    let settings = load(document(&format!(
        "{ASSET_ROUTES}\n[inspect]\nshow = [\"source.origin.url\", \
         \"proxy.asset_routes[].source.origin.url\"]"
    )))
    .expect("should load");

    let view = view(&settings);

    assert_eq!(
        at(&view.settings, "source.origin.url"),
        Some(&json!("https://origin.test-publisher.com"))
    );
    assert_eq!(
        at(&view.settings, "proxy.asset_routes[1].source.origin.url"),
        Some(&json!("https://media.example.com"))
    );
    assert!(!view.masked.iter().any(|path| path.ends_with(".origin.url")));
    assert!(!is_masked(&view, "source.origin.url"));
}

#[test]
fn hide_masks_a_value_shown_by_default() {
    let settings = load(document("[inspect]\nhide = [\"publisher.domain\"]")).expect("should load");

    let view = view(&settings);

    assert_eq!(at(&view.settings, "publisher.domain"), Some(&json!(MASK)));
    assert!(is_masked(&view, "publisher.domain"));
}

/// A pattern that would not be honored as written is refused when the
/// document loads. Each row's fragment carries the whole reason, so a
/// pattern refused for another reason fails its row.
#[test]
fn a_pattern_not_honored_as_written_is_refused_saying_why() {
    for (section, reason) in [
        (
            r#"show = ["publisher.proxy_secret"]"#,
            "show names `publisher.proxy_secret`, which is a secret",
        ),
        (
            r#"show = ["proxy.asset_routes[].source.origin.auth.key_name"]"#,
            "show names `proxy.asset_routes[].source.origin.auth.key_name`, which is a secret",
        ),
        (
            r#"hide = ["publisher.no_such_setting"]"#,
            "hide names `publisher.no_such_setting`, which matches no value",
        ),
        (
            r#"show = ["proxy.asset_routes[9].source.origin.url"]"#,
            "show names `proxy.asset_routes[9].source.origin.url`, which matches no value",
        ),
        (
            r#"show = ["publisher.domain"]"#,
            "show names `publisher.domain`, which changes nothing, because nothing \
             it names is masked by default",
        ),
        (
            r#"hide = ["source.origin.url"]"#,
            "hide names `source.origin.url`, which changes nothing, because \
             everything it names is masked or empty already",
        ),
        (
            r#"hide = ["publisher.proxy_secret"]"#,
            "hide names `publisher.proxy_secret`, which changes nothing, because \
             everything it names is masked or empty already",
        ),
        (
            r#"show = ["source.origin.url", "source.origin.url"]"#,
            "show names `source.origin.url`, which changes nothing, because \
             `source.origin.url` in the same list already names everything it does",
        ),
        (
            r#"show = ["proxy.asset_routes[].source.origin.url", "proxy.asset_routes[0].source.origin.url"]"#,
            "show names `proxy.asset_routes[0].source.origin.url`, which changes nothing, because \
             `proxy.asset_routes[].source.origin.url` in the same list already names everything \
             it does",
        ),
        (
            "show = [\"source.origin.url\"]\nhide = [\"source.origin.url\"]",
            "show names `source.origin.url`, which changes nothing, because hide \
             names it as `source.origin.url` and hide wins over show",
        ),
        (
            "config = false\nhide = [\"publisher.domain\"]",
            "config = false publishes no page, so they change nothing",
        ),
    ] {
        // The route's signer is on offer, so its key name is a declared secret.
        let message = format!(
            "{:?}",
            load_with(
                document(&format!("{ASSET_ROUTES}\n[inspect]\n{section}")),
                &[fixture::builder()],
            )
            .expect_err("the document should be refused")
        );

        assert!(message.contains(reason), "{section}: {message}");
    }
}

/// The check a deployment's settings pass before they are pushed refuses
/// the same patterns, so a bad one is caught before it is live.
#[test]
fn the_deploy_check_refuses_a_pattern_that_matches_nothing() {
    let toml = format!(
        "{}\n[inspect]\nhide = [\"publisher.no_such_setting\"]\n",
        crate_test_settings_str()
    );
    let settings = Settings::from_toml(&toml).expect("should parse the document");

    let message = format!(
        "{:?}",
        validate_patterns(&settings).expect_err("the pattern should be refused")
    );

    assert!(
        message.contains("hide names `publisher.no_such_setting`, which matches no value"),
        "{message}"
    );
}

/// A secret only the configuration or a deployment's own module names is
/// one the load resolves, so the deploy check refuses showing it as the
/// load does.
#[test]
fn the_deploy_check_refuses_showing_a_secret_only_the_load_resolves() {
    let builder = IntegrationBuilder::new(
        "probe",
        "example-crate",
        crate::integrations::registry_test_support::probe_registration,
        crate::integrations::registry_test_support::validate_nothing,
    )
    .with_module_name("testing.probe")
    .with_secret_settings(MODULE_SECRETS);
    let labeled = crate::test_support::tests::crate_test_settings_str_with_ec_section(
        "[ec]
module = \"primary\"

[ec.primary]
implementation = \"hmac\"
         passphrase = \"labeled-passphrase-key-0123456789\"",
    );
    let mut labeled = Settings::from_toml(&format!(
        "{labeled}
[inspect]
show = [\"ec.primary.passphrase\"]
"
    ))
    .expect("should parse the document");
    labeled.proxy.allowed_domains = vec!["*.example".to_owned(), "*.example.com".to_owned()];
    let mut module = settings();
    module.proxy.allowed_domains = vec!["*.example".to_owned(), "*.example.com".to_owned()];
    module
        .insert_module_config(
            "testing",
            "testing.probe",
            &json!({ "lock": true, "key_name": "module-key" }),
        )
        .expect("should insert the module's table");
    module.inspect.show = vec![PathPattern::parse("testing.probe.key_name").expect("a pattern")];

    for (settings, pattern) in [
        (labeled, "ec.primary.passphrase"),
        (module, "testing.probe.key_name"),
    ] {
        let reason = format!("show names `{pattern}`, which is a secret");
        let data = serde_json::to_value(&settings).expect("should serialize the document");
        let loaded = format!(
            "{:?}",
            load_with(data, &[builder]).expect_err("the load should refuse the pattern")
        );
        let deployed = format!(
            "{:?}",
            crate::config::validate_settings_for_deploy_with(&settings, &[builder])
                .expect_err("the deploy check should refuse the pattern")
        );

        assert!(loaded.contains(&reason), "load: {loaded}");
        assert!(deployed.contains(&reason), "deploy: {deployed}");
    }
}

/// A loaded domain shows its policy file's name and SHA-256, which nothing
/// masks, so a publisher can check which exact file runs. The file itself is
/// never shown, because the loader takes it out of the settings.
#[test]
fn the_view_shows_the_policy_by_name_and_digest_and_never_its_file() {
    let loaded = load(document("")).expect("should load the document");
    let shown = view(&loaded);

    assert_eq!(
        at(&shown.settings, "permissions.policy"),
        Some(&json!(SAMPLE_POLICY_NAME))
    );
    assert_eq!(
        at(&shown.settings, "permissions.sha256"),
        Some(&json!(policy_sha256(SAMPLE_POLICY_TEXT)))
    );
    assert_eq!(at(&shown.settings, "permissions.text"), None);
    assert!(
        !shown
            .masked
            .iter()
            .any(|path| path.starts_with("permissions")),
        "neither the name nor the digest should be masked: {:?}",
        shown.masked
    );
    let (json, page) = served(&loaded);
    for served in [json, page] {
        assert!(
            !served.contains("groups:"),
            "the policy file should never be served"
        );
    }
}

/// The view never shows `permissions.text`, so a pattern naming it, or
/// anything beneath it, is refused by the deploy check and by the load alike.
#[test]
fn a_pattern_under_the_policy_file_is_refused() {
    for (section, reason) in [
        (
            r#"show = ["permissions.text"]"#,
            "show names `permissions.text`, the policy file the push writes",
        ),
        (
            r#"hide = ["permissions.text"]"#,
            "hide names `permissions.text`, the policy file the push writes",
        ),
        (
            r#"hide = ["permissions.text[]"]"#,
            "hide names `permissions.text[]`, which is under `permissions.text`, the policy \
             file the push writes",
        ),
    ] {
        let toml = format!("{}\n[inspect]\n{section}\n", crate_test_settings_str());
        let mut settings = Settings::from_toml(&toml).expect("should parse the document");
        settings.proxy.allowed_domains = vec!["*.example".to_owned(), "*.example.com".to_owned()];
        settings.permissions = sample_permissions();

        let deployed = format!(
            "{:?}",
            crate::config::validate_settings_for_deploy_with(&settings, &[])
                .expect_err("the deploy check should refuse the pattern")
        );
        let loaded = format!(
            "{:?}",
            load(document(&format!("[inspect]\n{section}")))
                .expect_err("the load should refuse the pattern")
        );

        assert!(deployed.contains(reason), "deploy: {deployed}");
        assert!(loaded.contains(reason), "load: {loaded}");
    }
}

/// The push always writes `permissions.sha256`, so a pattern naming it is
/// judged before the push, which has not written it yet, as it is when the
/// pushed settings load.
#[test]
fn a_pattern_naming_the_policy_digest_is_judged_alike_before_and_after_the_push() {
    let before_the_push = |section: &str| {
        let toml = format!(
            "{}\n[permissions]\npolicy = \"{SAMPLE_POLICY_NAME}\"\n\n[inspect]\n{section}\n",
            crate_test_settings_str()
        );
        let mut settings = Settings::from_toml(&toml).expect("should parse the document");
        settings.proxy.allowed_domains = vec!["*.example".to_owned(), "*.example.com".to_owned()];
        crate::config::validate_settings_for_deploy_with(&settings, &[])
    };

    let hide = r#"hide = ["permissions.sha256"]"#;
    before_the_push(hide).expect("hiding the digest should be honored before the push");
    let loaded = load(document(&format!("[inspect]\n{hide}")))
        .expect("hiding the digest should be honored when the settings load");
    assert!(is_masked(&view(&loaded), "permissions.sha256"));

    let show = r#"show = ["permissions.sha256"]"#;
    let reason = "show names `permissions.sha256`, which changes nothing, because nothing it \
                  names is masked by default";
    let before = format!(
        "{:?}",
        before_the_push(show).expect_err("showing the digest changes nothing before the push")
    );
    let after = format!(
        "{:?}",
        load(document(&format!("[inspect]\n{show}")))
            .expect_err("showing the digest changes nothing when the settings load")
    );
    assert!(before.contains(reason), "before the push: {before}");
    assert!(after.contains(reason), "at the load: {after}");
}

#[test]
fn the_page_answers_not_found_when_the_publisher_does_not_publish() {
    let settings = load(document("[inspect]\nconfig = false")).expect("should load");

    for (path, content_type) in [
        (CONFIG_PAGE_PATH, "text/html; charset=utf-8"),
        (CONFIG_JSON_PATH, "application/json"),
    ] {
        let response = handle_config(&settings, &request(path));

        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            content_type,
            "{path}"
        );
        assert!(body_text(response).contains(NOT_PUBLISHED), "{path}");
    }
}

#[test]
fn the_page_and_the_json_carry_the_view() {
    let settings = load(document("")).expect("should load");

    for (path, content_type) in [
        (CONFIG_PAGE_PATH, "text/html; charset=utf-8"),
        (CONFIG_JSON_PATH, "application/json"),
    ] {
        let response = handle_config(&settings, &request(path));

        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(response.headers()[header::CONTENT_TYPE], content_type);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
    }
    let json: Value = serde_json::from_str(&body_text(handle_config(
        &settings,
        &request(CONFIG_JSON_PATH),
    )))
    .expect("a JSON body");
    let keys: Vec<&String> = json.as_object().expect("an object").keys().collect();
    assert_eq!(keys, ["masked", "settings", "version"]);
    assert_eq!(json["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        json["settings"]["publisher"]["domain"],
        "test-publisher.com"
    );
    assert!(
        json["masked"]
            .as_array()
            .expect("a list")
            .contains(&json!("publisher.proxy_secret")),
        "{json}"
    );
}

fn assert_keys_sorted(value: &Value) {
    match value {
        Value::Object(map) => {
            let keys: Vec<&String> = map.keys().collect();
            let mut sorted = keys.clone();
            sorted.sort_unstable();
            assert_eq!(keys, sorted, "keys should be sorted");
            map.values().for_each(assert_keys_sorted);
        }
        Value::Array(items) => items.iter().for_each(assert_keys_sorted),
        _ => {}
    }
}

/// Two loads of one document build hash maps of their own, each iterating
/// in its own order, and the view has to come out the same.
#[test]
fn the_same_settings_give_the_same_view() {
    let headers: String = (0..12)
        .map(|index| format!("x-header-{index} = \"value-{index}\"\n"))
        .collect();
    let data = document_from(&format!(
        "{}\n[response_headers]\n{headers}",
        crate_test_settings_str()
    ));

    let first = load(data.clone()).expect("should load");
    let second = load(data).expect("should load");
    let first_json = view_payload(&view(&first)).to_string();
    let second_json = view_payload(&view(&second)).to_string();

    assert_eq!(first_json, second_json);
    let parsed: Value = serde_json::from_str(&first_json).expect("JSON");
    assert_keys_sorted(&parsed);
    assert_eq!(
        parsed["settings"]["response_headers"]
            .as_object()
            .expect("the headers")
            .len(),
        12,
        "should show every header"
    );
    let masked = view(&first).masked;
    let mut sorted_masked = masked.clone();
    sorted_masked.sort_unstable();
    assert_eq!(masked, sorted_masked);
}

fn module_lock_is_on(table: &serde_json::Map<String, Value>) -> bool {
    table.get("lock").and_then(Value::as_bool) == Some(true)
}

const MODULE_SECRETS: &[ModuleSecretSetting] = &[ModuleSecretSetting {
    path: &["key_name"],
    in_use: module_lock_is_on,
}];

/// A secret a module a deployment added declares in its own table is one
/// the loader wrote, so it is masked like any of core's.
#[test]
fn a_secret_a_module_declares_is_masked() {
    let builder = IntegrationBuilder::new(
        "probe",
        "example-crate",
        crate::integrations::registry_test_support::probe_registration,
        crate::integrations::registry_test_support::validate_nothing,
    )
    .with_module_name("testing.probe")
    .with_secret_settings(MODULE_SECRETS);
    let mut settings = settings();
    settings.proxy.allowed_domains = vec!["*.example".to_owned(), "*.example.com".to_owned()];
    settings.permissions = sample_permissions();
    settings
        .insert_module_config(
            "testing",
            "testing.probe",
            &json!({ "lock": true, "key_name": "module-key", "label": "open" }),
        )
        .expect("should insert the module's table");
    let data = serde_json::to_value(&settings).expect("should serialize the document");

    let loaded = load_with(data, &[builder]).expect("should load");
    let view = view(&loaded);
    let (json, page) = served(&loaded);

    assert!(
        is_masked(&view, "testing.probe.key_name"),
        "should mask the module's secret: {:?}",
        view.masked
    );
    assert_eq!(
        at(&view.settings, "testing.probe.label"),
        Some(&json!("open")),
        "should show the module's other settings"
    );
    assert!(!json.contains(CANARY) && !page.contains(CANARY));
}
