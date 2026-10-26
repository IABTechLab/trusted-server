//! TS-owned, label-free credential contexts and validated runtime capabilities.
//!
//! Serialized plans explain lookups. They cannot be deserialized into authority.
//! Only a successful native read and representation validation creates a resolved use.

use std::collections::BTreeMap;
use std::fmt;

use error_stack::Report;
use http::HeaderValue;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::TrustedServerError;
use crate::platform::{PlatformSecretStore, StoreName};
use crate::settings::{
    AssetOriginAuth, Ec, EcPartner, Handler, OriginQueryPolicy, Publisher, Settings,
    TrustedClientIpConfig,
};

pub const BINDING_ABI_VERSION: &str = "trusted-server-secret-binding-v1";
pub const SECRET_STORE_ID: &str = "trusted_server_secrets";

/// Fixed executable-owned purposes, also used for legacy-leaf validation metadata.
pub struct BindingDescriptor {
    pub id: &'static str,
    pub scope: &'static str,
    pub schema_path: &'static str,
    pub optional: bool,
}
pub const DESCRIPTORS: &[BindingDescriptor] = &[
    BindingDescriptor {
        id: "publisher.proxy",
        scope: "proxy",
        schema_path: "publisher.proxy_secret",
        optional: false,
    },
    BindingDescriptor {
        id: "ec.passphrase",
        scope: "ecpass",
        schema_path: "ec.passphrase",
        optional: false,
    },
    BindingDescriptor {
        id: "ec.inbound",
        scope: "ecin",
        schema_path: "ec.partners[].api_token",
        optional: true,
    },
    BindingDescriptor {
        id: "ec.pull",
        scope: "ecpull",
        schema_path: "ec.partners[].ts_pull_token",
        optional: true,
    },
    BindingDescriptor {
        id: "handler.password",
        scope: "handler",
        schema_path: "handlers[].password",
        optional: false,
    },
    BindingDescriptor {
        id: "clientip.shared",
        scope: "clientip",
        schema_path: "trusted_client_ip.shared_secret",
        optional: false,
    },
    BindingDescriptor {
        id: "tinybird.auction",
        scope: "tinybird",
        schema_path: "tinybird.auction_token_secret",
        optional: true,
    },
    BindingDescriptor {
        id: "datadome.server",
        scope: "ddserver",
        schema_path: "integrations.datadome.server_side_key_secret_name",
        optional: true,
    },
    BindingDescriptor {
        id: "datadome.bypass",
        scope: "ddbypass",
        schema_path: "integrations.datadome.protection_test_bypass.credential_secret_name",
        optional: true,
    },
    BindingDescriptor {
        id: "s3.access-id",
        scope: "s3id",
        schema_path: "proxy.asset_routes[].auth.access_key_id",
        optional: true,
    },
    BindingDescriptor {
        id: "s3.secret",
        scope: "s3secret",
        schema_path: "proxy.asset_routes[].auth.secret_access_key",
        optional: true,
    },
    BindingDescriptor {
        id: "s3.session",
        scope: "s3sess",
        schema_path: "proxy.asset_routes[].auth.session_token",
        optional: true,
    },
];

/// Metadata identifies protected validation leaves, not operator-configurable references.
#[must_use]
pub fn secret_fields() -> Vec<edgezero_core::app_config::SecretField> {
    use edgezero_core::app_config::{SecretField, SecretKind, SecretPathSegment};
    DESCRIPTORS
        .iter()
        .map(|descriptor| {
            let mut path = Vec::new();
            for component in descriptor.schema_path.split('.') {
                let name = component.strip_suffix("[]").unwrap_or(component);
                if matches!(
                    name,
                    "partners"
                        | "trusted_client_ip"
                        | "tinybird"
                        | "integrations"
                        | "datadome"
                        | "protection_test_bypass"
                        | "proxy"
                        | "asset_routes"
                        | "auth"
                ) {
                    path.push(SecretPathSegment::OptionalField(name.into()));
                } else {
                    path.push(SecretPathSegment::Field(name.into()));
                }
                if component.ends_with("[]") {
                    path.push(SecretPathSegment::ArrayEach);
                }
            }
            SecretField {
                kind: SecretKind::KeyInDefault,
                optional: descriptor.optional,
                path,
            }
        })
        .collect()
}

type BindingResult<T> = Result<T, Report<TrustedServerError>>;

fn invalid(path: &str, category: &str) -> Report<TrustedServerError> {
    Report::new(TrustedServerError::Configuration {
        message: format!("{path}: {category}"),
    })
}

/// Fixed send target. Endpoints never come from resolved secret values.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompiledTarget {
    None,
    Pull { endpoint: String },
    Tinybird { endpoint: String, dataset: String },
    DataDome { endpoint: String },
    S3 { origin: String },
}

/// Public, secret-free explanation of an executable-owned binding.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CompiledBinding {
    descriptor_id: &'static str,
    scope: &'static str,
    field_path: String,
    subject: Vec<String>,
    export: Vec<String>,
    key: String,
    target: CompiledTarget,
}

impl CompiledBinding {
    #[must_use]
    pub fn descriptor_id(&self) -> &str {
        self.descriptor_id
    }
    #[must_use]
    pub fn scope(&self) -> &str {
        self.scope
    }
    #[must_use]
    pub fn field_path(&self) -> &str {
        &self.field_path
    }
    #[must_use]
    pub fn subject(&self) -> &[String] {
        &self.subject
    }
    #[must_use]
    pub fn export(&self) -> &[String] {
        &self.export
    }
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }
    #[must_use]
    pub fn target(&self) -> &CompiledTarget {
        &self.target
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BindingPlan {
    abi_version: &'static str,
    entries: Vec<CompiledBinding>,
}

impl BindingPlan {
    #[must_use]
    pub fn entries(&self) -> &[CompiledBinding] {
        &self.entries
    }
}

/// No deserialization, serialization, or public arbitrary-use constructor.
#[derive(Clone, Default)]
pub struct ResolvedBindings {
    entries: Vec<ResolvedBinding>,
}

impl fmt::Debug for ResolvedBindings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedBindings")
            .field("count", &self.entries.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct ResolvedBinding {
    compiled: CompiledBinding,
    credential: String,
    bearer_header: Option<HeaderValue>,
}

impl fmt::Debug for ResolvedBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedBinding")
            .field("scope", &self.compiled.scope)
            .field("field_path", &self.compiled.field_path)
            .finish_non_exhaustive()
    }
}

impl ResolvedBinding {
    #[must_use]
    pub fn compiled(&self) -> &CompiledBinding {
        &self.compiled
    }
    #[must_use]
    pub fn credential(&self) -> &str {
        &self.credential
    }
    #[must_use]
    pub fn bearer_header(&self) -> Option<&HeaderValue> {
        self.bearer_header.as_ref()
    }
    #[must_use]
    pub fn subject(&self) -> &[String] {
        self.compiled.subject()
    }
    #[must_use]
    pub fn export(&self) -> &[String] {
        self.compiled.export()
    }
    #[must_use]
    pub fn endpoint(&self) -> Option<&str> {
        match &self.compiled.target {
            CompiledTarget::None => None,
            CompiledTarget::Pull { endpoint }
            | CompiledTarget::Tinybird { endpoint, .. }
            | CompiledTarget::DataDome { endpoint } => Some(endpoint),
            CompiledTarget::S3 { origin } => Some(origin),
        }
    }
}

impl ResolvedBindings {
    #[must_use]
    pub fn entries(&self) -> &[ResolvedBinding] {
        &self.entries
    }
    #[must_use]
    pub fn get(&self, descriptor_id: &str, field_path: &str) -> Option<&ResolvedBinding> {
        self.entries.iter().find(|entry| {
            entry.compiled.descriptor_id == descriptor_id && entry.compiled.field_path == field_path
        })
    }
    /// Reject changed identity, activation, route policy, or destination before export.
    ///
    /// # Errors
    /// Returns a schema-only error for invalid settings or a changed/missing capability.
    pub fn validated_for(
        &self,
        settings: &Settings,
        descriptor_id: &str,
        field_path: &str,
    ) -> BindingResult<Option<&ResolvedBinding>> {
        let current = compile_bindings(settings)?;
        let expected = current
            .entries
            .iter()
            .find(|entry| entry.descriptor_id == descriptor_id && entry.field_path == field_path);
        match (expected, self.get(descriptor_id, field_path)) {
            (None, None) => Ok(None),
            (Some(expected), Some(actual)) if expected == &actual.compiled => Ok(Some(actual)),
            _ => Err(invalid(field_path, "resolved binding context mismatch")),
        }
    }
    /// Require an active capability matching the current compiled context.
    ///
    /// # Errors
    /// Returns a schema-only error for invalid settings or a changed/missing capability.
    pub fn require_for_settings(
        &self,
        settings: &Settings,
        descriptor_id: &str,
        field_path: &str,
    ) -> BindingResult<&ResolvedBinding> {
        self.validated_for(settings, descriptor_id, field_path)?
            .ok_or_else(|| invalid(field_path, "required resolved binding unavailable"))
    }
}

fn checked_u32(value: usize, path: &str) -> BindingResult<u32> {
    u32::try_from(value).map_err(|_| invalid(path, "binding context too large"))
}
fn lp(out: &mut Vec<u8>, value: &str) -> BindingResult<()> {
    out.extend_from_slice(&checked_u32(value.len(), "secret_bindings")?.to_be_bytes());
    out.extend_from_slice(value.as_bytes());
    Ok(())
}
fn vector(out: &mut Vec<u8>, values: &[String]) -> BindingResult<()> {
    out.extend_from_slice(&checked_u32(values.len(), "secret_bindings")?.to_be_bytes());
    for value in values {
        lp(out, value)?;
    }
    Ok(())
}
fn derive_key(scope: &str, subject: &[String], export: &[String]) -> BindingResult<String> {
    let mut payload = b"trusted-server-secret-binding-v1\0".to_vec();
    lp(&mut payload, scope)?;
    vector(&mut payload, subject)?;
    vector(&mut payload, export)?;
    let digest = Sha256::digest(payload);
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut encoded = String::from("ts1_");
    let mut bits = 0_u32;
    let mut available = 0_u32;
    for byte in digest {
        bits = (bits << 8) | u32::from(byte);
        available += 8;
        while available >= 5 {
            available -= 5;
            encoded.push(ALPHABET[((bits >> available) & 31) as usize] as char);
        }
    }
    if available > 0 {
        encoded.push(ALPHABET[((bits << (5 - available)) & 31) as usize] as char);
    }
    Ok(encoded)
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

/// Validate original syntax before URL parsing can erase forbidden distinctions.
/// Errors contain the supplied schema path, never the input endpoint.
///
/// # Errors
/// Rejects syntax outside the v1 HTTPS DNS endpoint grammar.
pub fn normalize_endpoint(input: &str, field_path: &str) -> BindingResult<String> {
    let fail = || invalid(field_path, "invalid credential endpoint");
    if !input.is_ascii()
        || input
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
        || input.contains(['\\', '?', '#', '%'])
    {
        return Err(fail());
    }
    let (scheme, rest) = input.split_once("://").ok_or_else(fail)?;
    if !scheme.eq_ignore_ascii_case("https") {
        return Err(fail());
    }
    let (authority, path) = rest
        .find('/')
        .map_or((rest, "/"), |index| (&rest[..index], &rest[index..]));
    if authority.is_empty() || authority.contains(['@', '[', ']']) {
        return Err(fail());
    }
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => {
            if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
                return Err(fail());
            }
            let value = port.parse::<u16>().map_err(|_| fail())?;
            if value == 0 {
                return Err(fail());
            }
            (host, (value != 443).then_some(value))
        }
        None => (authority, None),
    };
    if host.ends_with('.')
        || host.len() > 253
        || host.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || !label.as_bytes()[0].is_ascii_alphanumeric()
                || !label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                || !label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    {
        return Err(fail());
    }
    // URL's host classification catches nonstandard numeric IPv4 spellings too.
    let parsed = url::Url::parse(input).map_err(|_| fail())?;
    if !matches!(parsed.host(), Some(url::Host::Domain(_))) {
        return Err(fail());
    }
    if !path
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"/-._~".contains(&b))
        || path.split('/').any(|part| part == "." || part == "..")
    {
        return Err(fail());
    }
    let host = host.to_ascii_lowercase();
    Ok(match port {
        Some(port) => format!("https://{host}:{port}{path}"),
        None => format!("https://{host}{path}"),
    })
}

/// Compile a pull destination and its optional hostname assertion.
///
/// # Errors
/// Rejects missing/invalid endpoints or mismatched allowlist assertions.
pub fn normalize_pull_endpoint(partner: &EcPartner, field_path: &str) -> BindingResult<String> {
    let raw = partner
        .pull_sync_url
        .as_deref()
        .ok_or_else(|| invalid(field_path, "required credential endpoint unavailable"))?;
    let endpoint = normalize_endpoint(raw, field_path)?;
    let host = url::Url::parse(&endpoint)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .ok_or_else(|| invalid(field_path, "invalid credential endpoint"))?;
    for allowed in &partner.pull_sync_allowed_domains {
        if allowed.trim() != allowed || !allowed.eq_ignore_ascii_case(&host) {
            return Err(invalid(field_path, "pull endpoint assertion mismatch"));
        }
    }
    Ok(endpoint)
}

/// Compile active roles from effective ordinary settings without reading secrets.
///
/// # Errors
/// Rejects malformed active identities, destinations, activation, and route policy.
pub fn compile_bindings(settings: &Settings) -> BindingResult<BindingPlan> {
    let mut entries = Vec::new();
    let mut add = |descriptor_id: &'static str,
                   scope: &'static str,
                   field_path: String,
                   subject: Vec<String>,
                   export: Vec<String>,
                   target: CompiledTarget|
     -> BindingResult<()> {
        if !DESCRIPTORS
            .iter()
            .any(|descriptor| descriptor.id == descriptor_id && descriptor.scope == scope)
        {
            return Err(invalid("secret_bindings", "unsupported binding descriptor"));
        }
        let key = derive_key(scope, &subject, &export)?;
        entries.push(CompiledBinding {
            descriptor_id,
            scope,
            field_path,
            subject,
            export,
            key,
            target,
        });
        Ok(())
    };
    add(
        "publisher.proxy",
        "proxy",
        "publisher.proxy_secret".into(),
        strings(&["app"]),
        strings(&["none"]),
        CompiledTarget::None,
    )?;
    add(
        "ec.passphrase",
        "ecpass",
        "ec.passphrase".into(),
        strings(&["app"]),
        strings(&["none"]),
        CompiledTarget::None,
    )?;
    let mut domains = std::collections::BTreeSet::new();
    for (index, partner) in settings.ec.partners.iter().enumerate() {
        let path = format!("ec.partners[{index}]");
        EcPartner::validate_source_domain(&partner.source_domain)
            .map_err(|_| invalid(&format!("{path}.source_domain"), "invalid partner identity"))?;
        let domain = partner
            .source_domain
            .trim_end_matches('.')
            .to_ascii_lowercase();
        if !domains.insert(domain.clone()) {
            return Err(invalid("ec.partners", "duplicate partner identity"));
        }
        let subject = strings(&["partner", &domain]);
        if partner.api_token_enabled {
            add(
                "ec.inbound",
                "ecin",
                format!("{path}.api_token"),
                subject.clone(),
                strings(&["none"]),
                CompiledTarget::None,
            )?;
        }
        if partner.pull_sync_enabled {
            let endpoint = normalize_pull_endpoint(partner, &format!("{path}.pull_sync_url"))?;
            add(
                "ec.pull",
                "ecpull",
                format!("{path}.ts_pull_token"),
                subject,
                strings(&["pull", &endpoint]),
                CompiledTarget::Pull { endpoint },
            )?;
        }
    }
    for (index, handler) in settings.handlers.iter().enumerate() {
        let path = format!("handlers[{index}].password");
        if handler.path.is_empty()
            || regex::Regex::new(&handler.path).is_err()
            || handler.username.expose().is_empty()
        {
            return Err(invalid(&path, "invalid handler identity"));
        }
        add(
            "handler.password",
            "handler",
            path,
            strings(&["handler", &handler.path, handler.username.expose()]),
            strings(&["none"]),
            CompiledTarget::None,
        )?;
    }
    if settings.trusted_client_ip.is_some() {
        add(
            "clientip.shared",
            "clientip",
            "trusted_client_ip.shared_secret".into(),
            strings(&["app"]),
            strings(&["none"]),
            CompiledTarget::None,
        )?;
    }
    if settings.tinybird.access_enabled {
        return Err(invalid(
            "tinybird.access_enabled",
            "unsupported telemetry activation",
        ));
    }
    if settings.tinybird.enabled {
        let host = &settings.tinybird.api_host;
        if host.contains('/') {
            return Err(invalid("tinybird.api_host", "invalid credential endpoint"));
        }
        let endpoint =
            normalize_endpoint(&format!("https://{host}/v0/events"), "tinybird.api_host")?;
        let dataset = &settings.tinybird.auction_dataset;
        if dataset.is_empty()
            || dataset.len() > 128
            || !dataset
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(invalid(
                "tinybird.auction_dataset",
                "invalid telemetry dataset",
            ));
        }
        add(
            "tinybird.auction",
            "tinybird",
            "tinybird.auction_token_secret".into(),
            strings(&["app"]),
            strings(&["tinybird", &endpoint, dataset]),
            CompiledTarget::Tinybird {
                endpoint,
                dataset: dataset.clone(),
            },
        )?;
    }
    if let Some(raw) = settings.integrations.get("datadome") {
        if !raw.is_object() {
            return Err(invalid(
                "integrations.datadome",
                "expected configuration object",
            ));
        }
        // Validate the complete present dynamic schema before any native read.
        let datadome_config =
            serde_json::from_value::<crate::integrations::datadome::DataDomeConfig>(raw.clone())
                .map_err(|_| invalid("integrations.datadome", "invalid integration structure"))?;
        let enabled = bool_value(raw, "enabled", false, "integrations.datadome.enabled")?;
        let protection = bool_value(
            raw,
            "enable_protection",
            false,
            "integrations.datadome.enable_protection",
        )?;
        let bypass = match raw.get("protection_test_bypass") {
            None | Some(Value::Null) => false,
            Some(value) if value.is_object() => bool_value(
                value,
                "enabled",
                false,
                "integrations.datadome.protection_test_bypass.enabled",
            )?,
            _ => {
                return Err(invalid(
                    "integrations.datadome.protection_test_bypass",
                    "expected configuration object",
                ));
            }
        };
        if enabled && protection {
            let normalized = normalize_endpoint(
                &datadome_config.protection_api_origin,
                "integrations.datadome.protection_api_origin",
            )?;
            if url::Url::parse(&normalized).map_or(true, |url| url.path() != "/") {
                return Err(invalid(
                    "integrations.datadome.protection_api_origin",
                    "protection endpoint must use origin only",
                ));
            }
            let endpoint = format!("{}validate-request", normalized);
            add(
                "datadome.server",
                "ddserver",
                "integrations.datadome.server_side_key_secret_name".into(),
                strings(&["app"]),
                strings(&["datadome", &endpoint]),
                CompiledTarget::DataDome { endpoint },
            )?;
            if bypass {
                add(
                    "datadome.bypass",
                    "ddbypass",
                    "integrations.datadome.protection_test_bypass.credential_secret_name".into(),
                    strings(&["app"]),
                    strings(&["none"]),
                    CompiledTarget::None,
                )?;
            }
        }
    }
    for (index, route) in settings.proxy.asset_routes.iter().enumerate() {
        let Some(AssetOriginAuth::S3SigV4(auth)) = &route.auth else {
            continue;
        };
        let path = format!("proxy.asset_routes[{index}]");
        let endpoint = normalize_endpoint(&route.origin_url, &format!("{path}.origin_url"))?;
        let url = url::Url::parse(&endpoint)
            .map_err(|_| invalid(&path, "invalid credential endpoint"))?;
        if url.path() != "/" {
            return Err(invalid(
                &format!("{path}.origin_url"),
                "authenticated origin must not contain path",
            ));
        }
        let origin = endpoint.trim_end_matches('/').to_owned();
        if route.prefix.is_empty()
            || !route.prefix.starts_with('/')
            || auth.region.is_empty()
            || !auth
                .region
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(invalid(&path, "invalid authenticated route policy"));
        }
        if route.path_pattern.is_some() != route.target_path.is_some()
            || route
                .path_pattern
                .as_ref()
                .is_some_and(|pattern| regex::Regex::new(pattern).is_err())
        {
            return Err(invalid(&path, "invalid authenticated route policy"));
        }
        let (pattern_presence, pattern) = route
            .path_pattern
            .as_deref()
            .map_or(("absent", ""), |value| ("present", value));
        let (target_presence, target) = route
            .target_path
            .as_deref()
            .map_or(("absent", ""), |value| ("present", value));
        let query = match route.origin_query_policy() {
            OriginQueryPolicy::Preserve => "preserve",
            OriginQueryPolicy::Strip => "strip",
        };
        if route.image_optimizer_enabled() && query == "preserve" {
            return Err(invalid(&path, "invalid authenticated route query policy"));
        }
        let subject = strings(&[
            "route",
            &route.prefix,
            &origin,
            pattern_presence,
            pattern,
            target_presence,
            target,
            &auth.region,
            query,
        ]);
        add(
            "s3.access-id",
            "s3id",
            format!("{path}.auth.access_key_id"),
            subject.clone(),
            strings(&["s3"]),
            CompiledTarget::S3 {
                origin: origin.clone(),
            },
        )?;
        add(
            "s3.secret",
            "s3secret",
            format!("{path}.auth.secret_access_key"),
            subject.clone(),
            strings(&["s3"]),
            CompiledTarget::S3 {
                origin: origin.clone(),
            },
        )?;
        if auth.session_token_enabled {
            add(
                "s3.session",
                "s3sess",
                format!("{path}.auth.session_token"),
                subject,
                strings(&["s3"]),
                CompiledTarget::S3 { origin },
            )?;
        }
    }
    // Stable across hash-map iteration and ordinary config property ordering.
    entries.sort_by(|a, b| {
        a.descriptor_id
            .cmp(b.descriptor_id)
            .then(a.subject.cmp(&b.subject))
            .then(a.export.cmp(&b.export))
            .then(a.field_path.cmp(&b.field_path))
    });
    Ok(BindingPlan {
        abi_version: BINDING_ABI_VERSION,
        entries,
    })
}

fn bool_value(value: &Value, key: &str, default: bool, path: &str) -> BindingResult<bool> {
    match value.get(key) {
        None => Ok(default),
        Some(Value::Bool(value)) => Ok(*value),
        _ => Err(invalid(path, "expected boolean")),
    }
}

/// Shared runtime/provisioning credential representation validator. Never trims.
///
/// # Errors
/// Rejects unknown scopes, invalid UTF-8, empty/placeholder values, and invalid wire representation.
pub fn validate_credential(scope: &str, bytes: &[u8]) -> BindingResult<()> {
    validate_value(scope, bytes, "secret_bindings")
}
fn validate_value(scope: &str, bytes: &[u8], path: &str) -> BindingResult<()> {
    let value = std::str::from_utf8(bytes)
        .map_err(|_| invalid(path, "invalid credential representation"))?;
    if value.is_empty() {
        return Err(invalid(path, "empty credential"));
    }
    let placeholder = match scope {
        "proxy" => Publisher::is_placeholder_proxy_secret(value),
        "ecpass" => Ec::is_placeholder_passphrase(value),
        "ecin" | "ecpull" => EcPartner::is_placeholder_api_token(value),
        "handler" => Handler::is_placeholder_password(value),
        "clientip" => TrustedClientIpConfig::is_placeholder_shared_secret(value),
        "tinybird" | "ddserver" | "ddbypass" | "s3id" | "s3secret" | "s3sess" => false,
        _ => return Err(invalid(path, "unsupported credential scope")),
    };
    if placeholder {
        return Err(invalid(path, "placeholder credential"));
    }
    if matches!(
        scope,
        "ecpass" | "ecin" | "ecpull" | "clientip" | "ddbypass"
    ) && bytes.len() < 32
    {
        return Err(invalid(path, "credential below minimum length"));
    }
    if scope == "clientip" && !bytes.iter().all(|byte| matches!(*byte, b'!'..=b'~')) {
        return Err(invalid(path, "invalid client IP credential"));
    }
    if scope == "ecin" && value.trim().is_empty() {
        return Err(invalid(path, "empty credential"));
    }
    if matches!(scope, "ecpull" | "tinybird") {
        let token = value.trim_end_matches('=');
        if token.is_empty()
            || !token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._~+/".contains(&b))
            || !value[token.len()..].bytes().all(|b| b == b'=')
        {
            return Err(invalid(path, "invalid bearer credential"));
        }
        HeaderValue::from_str(&format!("Bearer {value}"))
            .map_err(|_| invalid(path, "invalid bearer credential"))?;
    }
    if matches!(scope, "s3id" | "s3sess") {
        let header = HeaderValue::from_str(value)
            .map_err(|_| invalid(path, "invalid signing credential"))?;
        if value.bytes().any(|byte| byte.is_ascii_control()) || header.to_str().is_err() {
            return Err(invalid(path, "invalid signing credential"));
        }
    }
    Ok(())
}

/// Atomic resolution. Identical contexts are read once per load, with no fallback.
///
/// # Errors
/// Rejects invalid contexts, missing native values, and invalid credentials.
/// The store is selected by trusted loader/runtime mappings, never app config.
pub fn resolve_bindings(
    settings: &Settings,
    store: &dyn PlatformSecretStore,
    store_name: &StoreName,
) -> BindingResult<ResolvedBindings> {
    let plan = compile_bindings(settings)?;
    let mut reads: BTreeMap<String, (CompiledBinding, String)> = BTreeMap::new();
    let mut entries = Vec::with_capacity(plan.entries.len());
    for compiled in plan.entries {
        let credential = if let Some((previous, value)) = reads.get(&compiled.key) {
            if previous.scope != compiled.scope
                || previous.subject != compiled.subject
                || previous.export != compiled.export
            {
                return Err(invalid(&compiled.field_path, "binding identity collision"));
            }
            value.clone()
        } else {
            let bytes = store
                .get_bytes(store_name, &compiled.key)
                .map_err(|_| invalid(&compiled.field_path, "native credential unavailable"))?;
            validate_value(compiled.scope, &bytes, &compiled.field_path)?;
            let value = String::from_utf8(bytes)
                .map_err(|_| invalid(&compiled.field_path, "invalid credential representation"))?;
            reads.insert(compiled.key.clone(), (compiled.clone(), value.clone()));
            value
        };
        let bearer_header = if matches!(compiled.scope, "ecpull" | "tinybird") {
            let mut value = HeaderValue::from_str(&format!("Bearer {credential}"))
                .map_err(|_| invalid(&compiled.field_path, "invalid bearer credential"))?;
            value.set_sensitive(true);
            Some(value)
        } else {
            None
        };
        entries.push(ResolvedBinding {
            compiled,
            credential,
            bearer_header,
        });
    }
    Ok(ResolvedBindings { entries })
}

/// Reject old config inputs even when a feature is inactive or value is null.
///
/// # Errors
/// Returns only a schema path and fixed migration category for removed inputs.
pub fn reject_removed_references(value: &Value) -> BindingResult<()> {
    for (path, field) in [
        ("publisher", "proxy_secret"),
        ("ec", "passphrase"),
        ("trusted_client_ip", "shared_secret"),
        ("tinybird", "auction_token_secret"),
        ("integrations.datadome", "server_side_key_secret_name"),
        (
            "integrations.datadome.protection_test_bypass",
            "credential_secret_name",
        ),
    ] {
        let node = path.split('.').try_fold(value, |node, part| node.get(part));
        if node.is_some_and(|node| node.get(field).is_some()) {
            return Err(invalid(
                &format!("{path}.{field}"),
                "removed credential reference; migrate configuration",
            ));
        }
    }
    for (path, fields) in [
        ("ec.partners", &["api_token", "ts_pull_token"][..]),
        ("handlers", &["password"][..]),
        (
            "proxy.asset_routes",
            &["access_key_id", "secret_access_key", "session_token"][..],
        ),
    ] {
        let node = path.split('.').try_fold(value, |node, part| node.get(part));
        if let Some(node) = node {
            let items = collection_items(node, path)?;
            for (index, item) in items.iter().enumerate() {
                let (item, suffix) = if path == "proxy.asset_routes" {
                    (item.get("auth"), ".auth")
                } else {
                    (Some(item), "")
                };
                for field in fields {
                    if item.is_some_and(|item| item.get(*field).is_some()) {
                        return Err(invalid(
                            &format!("{path}[{index}]{suffix}.{field}"),
                            "removed credential reference; migrate configuration",
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Preserve original endpoint syntax checks before ordinary normalization.
///
/// # Errors
/// Rejects malformed endpoint syntax or collections, including inactive features.
pub fn validate_original_endpoints(value: &Value) -> BindingResult<()> {
    if let Some(partners) = value.pointer("/ec/partners") {
        let items = collection_items(partners, "ec.partners")?;
        for (index, partner) in items.iter().enumerate() {
            if let Some(endpoint) = partner.get("pull_sync_url")
                && !endpoint.is_null()
            {
                normalize_endpoint(
                    endpoint
                        .as_str()
                        .ok_or_else(|| invalid("ec.partners", "invalid credential endpoint"))?,
                    &format!("ec.partners[{index}].pull_sync_url"),
                )?;
            }
        }
    }
    if let Some(endpoint) = value.pointer("/integrations/datadome/protection_api_origin") {
        normalize_endpoint(
            endpoint.as_str().ok_or_else(|| {
                invalid(
                    "integrations.datadome.protection_api_origin",
                    "invalid credential endpoint",
                )
            })?,
            "integrations.datadome.protection_api_origin",
        )?;
    }
    if let Some(host) = value.pointer("/tinybird/api_host") {
        let host = host
            .as_str()
            .ok_or_else(|| invalid("tinybird.api_host", "invalid credential endpoint"))?;
        if !host.is_empty() {
            if host.contains('/') {
                return Err(invalid("tinybird.api_host", "invalid credential endpoint"));
            }
            normalize_endpoint(&format!("https://{host}/v0/events"), "tinybird.api_host")?;
        }
    }
    if let Some(routes) = value.pointer("/proxy/asset_routes") {
        let items = collection_items(routes, "proxy.asset_routes")?;
        for (index, route) in items.iter().enumerate() {
            if route.get("auth").is_some_and(|auth| !auth.is_null()) {
                let path = format!("proxy.asset_routes[{index}].origin_url");
                let endpoint = route
                    .get("origin_url")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid(&path, "invalid credential endpoint"))?;
                let normalized = normalize_endpoint(endpoint, &path)?;
                if url::Url::parse(&normalized).map_or(true, |url| url.path() != "/") {
                    return Err(invalid(&path, "authenticated origin must not contain path"));
                }
            }
        }
    }
    Ok(())
}

fn collection_items(node: &Value, path: &str) -> BindingResult<Vec<Value>> {
    match node {
        Value::Array(items) => Ok(items.clone()),
        Value::Object(items) => Ok(items.values().cloned().collect()),
        Value::String(raw) => {
            let parsed: Value = serde_json::from_str(raw)
                .map_err(|_| invalid(path, "invalid configuration collection"))?;
            if !parsed.is_array() {
                return Err(invalid(path, "invalid configuration collection"));
            }
            collection_items(&parsed, path)
        }
        _ => Err(invalid(path, "invalid configuration collection")),
    }
}

/// Exact adapter-native identity for the fixed TS logical store.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NativeBinding {
    pub adapter: String,
    pub store_id: String,
    pub key: String,
    pub native_name: String,
    pub declarations: Vec<String>,
}

/// Render the exact TS-native lookup identity and required declarations.
///
/// # Errors
/// Rejects unknown adapters, non-TS logical stores, and non-generated keys.
pub fn render_native(
    adapter: &str,
    logical_store: &str,
    key: &str,
) -> BindingResult<NativeBinding> {
    render_native_with_mapping(adapter, logical_store, key, logical_store)
}

/// Render using the trusted deployment/runtime store-name mapping.
///
/// Fastly uses logical resource links and Cloudflare ignores the store argument.
/// Axum and Spin encode the selected mapped store in the native identity.
///
/// # Errors
/// Rejects invalid role identities or a mapping unsupported by the native bridge.
pub fn render_native_with_mapping(
    adapter: &str,
    logical_store: &str,
    key: &str,
    mapped_store: &str,
) -> BindingResult<NativeBinding> {
    if logical_store != SECRET_STORE_ID {
        return Err(invalid(
            "secret_bindings",
            "unsupported logical secret store",
        ));
    }
    if key.len() != 56
        || !key.starts_with("ts1_")
        || !key[4..]
            .bytes()
            .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
    {
        return Err(invalid("secret_bindings", "invalid generated binding key"));
    }
    let (native_name, declarations) = match adapter {
        "fastly" | "cloudflare" => (key.to_owned(), Vec::new()),
        "axum" => (axum_secret_name(mapped_store, key), Vec::new()),
        "spin" => {
            let name = spin_secret_name(mapped_store, key)?;
            let declarations = vec![
                format!("[variables.{name}]\nrequired = true\nsecret = true"),
                format!("[component.trusted-server.variables]\n{name} = \"{{{{ {name} }}}}\""),
            ];
            (name, declarations)
        }
        _ => return Err(invalid("secret_bindings", "unsupported native adapter")),
    };
    Ok(NativeBinding {
        adapter: adapter.to_owned(),
        store_id: SECRET_STORE_ID.to_owned(),
        key: key.to_owned(),
        native_name,
        declarations,
    })
}

/// Native Axum environment mapping, shared with the runtime bridge.
#[must_use]
pub fn axum_secret_name(store: &str, key: &str) -> String {
    let normalize = |value: &str| value.to_uppercase().replace(['-', '.', ' '], "_");
    format!(
        "TRUSTED_SERVER_SECRET_{}_{}",
        normalize(store),
        normalize(key)
    )
}

/// Native Spin application/component mapping, shared with the runtime bridge.
///
/// # Errors
/// Rejects identity components outside the existing Spin encoder grammar.
pub fn spin_secret_name(store: &str, key: &str) -> BindingResult<String> {
    Ok(format!(
        "{}_{}",
        spin_component(store)?,
        spin_component(key)?
    ))
}

fn spin_component(value: &str) -> BindingResult<String> {
    if !value.starts_with(|ch: char| ch.is_ascii_lowercase())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
    {
        return Err(invalid(
            "secret_bindings",
            "invalid native variable identity",
        ));
    }
    let mut result = String::from("v_");
    for byte in value.bytes() {
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            result.push(char::from(byte));
        } else {
            result.push_str(&format!("_x{byte:02x}"));
        }
    }
    Ok(result)
}

#[cfg(test)]
#[path = "secret_bindings_tests.rs"]
mod tests;
