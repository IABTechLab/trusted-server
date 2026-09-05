//! Serde-aware configuration schema extraction and checked companion records.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path as FsPath;

use error_stack::{Report, ResultExt as _};
use serde::Deserialize;
use syn::{Attribute, Expr, ExprLit, Fields, Item, Lit, Path, Stmt, Type};

use crate::repository::{NormalizedRelativePath, Repository};

const MAX_SETTINGS_INPUT_BYTES: usize = 4 * 1024 * 1024;
const COMPANION_MANIFEST_PATH: &str = "tools/docs-parity/manifests/settings-companions.toml";
const SOURCE_TYPES: &[(&str, &str)] = &[
    ("crates/trusted-server-core/src/settings.rs", "Settings"),
    (
        "crates/trusted-server-core/src/auction/profile.rs",
        "StandardProfileConfig",
    ),
    (
        "crates/trusted-server-core/src/integrations/aps.rs",
        "ApsProfileConfig",
    ),
    (
        "crates/trusted-server-core/src/integrations/prebid.rs",
        "PrebidIntegrationConfig",
    ),
];
const COMPANION_SOURCES: &[&str] = &[
    "crates/trusted-server-core/src/integrations/aps.rs",
    "crates/trusted-server-core/src/integrations/prebid.rs",
    "crates/trusted-server-core/src/settings.rs",
];

/// Failure while extracting checked settings semantics.
#[derive(Debug, derive_more::Display)]
pub enum SettingsError {
    /// Rust syntax could not be parsed.
    #[display("cannot parse Rust settings source")]
    RustSyntax,
    /// The companion TOML record could not be parsed.
    #[display("cannot parse settings companion manifest")]
    CompanionSyntax,
    /// The companion record is not explicitly reviewed.
    #[display("settings companion manifest must be version 1 and reviewed")]
    CompanionReview,
    /// A companion entry is malformed or duplicated.
    #[display("invalid settings companion for {symbol}: {reason}")]
    InvalidCompanion {
        /// Companion symbol.
        symbol: String,
        /// Stable explanation.
        reason: &'static str,
    },
    /// Custom semantics do not have a checked companion.
    #[display("missing {kind} companion for {symbol} in {source}")]
    MissingCompanion {
        /// Source path.
        source: String,
        /// Function or method path.
        symbol: String,
        /// Companion kind.
        kind: &'static str,
    },
    /// A shape-changing Serde attribute is outside the closed grammar.
    #[display("unsupported serde attribute {attribute} on {item}")]
    UnsupportedSerdeAttribute {
        /// Attribute name.
        attribute: String,
        /// Affected item.
        item: String,
    },
    /// A supported attribute has an invalid form.
    #[display("invalid {attribute} attribute on {item}")]
    InvalidAttribute {
        /// Attribute family.
        attribute: &'static str,
        /// Affected item.
        item: String,
    },
    /// A template-harness phase did not match its checked contract.
    #[display("settings template harness failed {phase}")]
    HarnessMismatch {
        /// Stable phase name.
        phase: &'static str,
    },
    /// Repository input could not be read safely.
    #[display("cannot read checked settings repository input")]
    Repository,
    /// A checked settings contract is incomplete or stale.
    #[display("invalid checked settings contract: {reason}")]
    InvalidContract {
        /// Stable explanation of the mismatch.
        reason: String,
    },
}

impl core::error::Error for SettingsError {}

/// Resolved source of a field default.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DefaultValue {
    /// Uses the field type's Default implementation.
    Trait,
    /// A companion-free literal returned by the named default function.
    Literal(String),
    /// A checked companion value for a nonliteral default function.
    Companion(String),
}

/// Field lifecycle independent from its other dispositions.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    /// Current supported field.
    Canonical,
    /// Accepted for compatibility but discouraged.
    Deprecated,
    /// Removed field retained as a rejection record.
    Rejected,
}

/// Serialized-key identity independent from lifecycle.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum KeyIdentity {
    /// Canonical key.
    Canonical,
    /// Deserialization alias of another key.
    Alias,
}

/// Output serialization behavior.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SerializationDisposition {
    /// Field is serialized.
    Serialized,
    /// Field is accepted but skipped during serialization.
    Skipped,
}

/// Runtime behavior after deserialization.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeDisposition {
    /// Field remains active at runtime.
    Active,
    /// Field is removed by normalization.
    NormalizedAway,
    /// Field exists only in the input schema.
    DeserializationOnly,
}

/// Secret-value handling for a field.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SecretDisposition {
    /// A key name is resolved through the configured store.
    StoreResolved,
    /// The field deliberately contains an inline secret.
    DeliberatelyInline,
    /// An accepted compatibility field is discarded before runtime.
    AcceptedDiscarded,
    /// The field is not secret-bearing.
    None,
}

/// Independent directional dispositions for one setting path.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FieldDisposition {
    /// Exact type-qualified field path.
    pub path: String,
    /// Lifecycle axis.
    pub lifecycle: Lifecycle,
    /// Canonical or alias key axis.
    pub key_identity: KeyIdentity,
    /// Canonical path for aliases.
    pub alias_of: Option<String>,
    /// Serialization axis.
    pub serialization: SerializationDisposition,
    /// Runtime axis.
    pub runtime: RuntimeDisposition,
    /// Secret-handling axis.
    pub secret: SecretDisposition,
}

/// Exact checked sets that define the template harness.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TemplateContract {
    /// Placeholder paths rejected by the unmodified source template.
    pub placeholder_paths: BTreeSet<String>,
    /// Integration IDs exercised in isolation.
    pub integration_ids: BTreeSet<String>,
    /// Provider profile IDs compiled in isolation.
    pub profile_ids: BTreeSet<String>,
    /// Load-bearing exact-string substitutions.
    pub consumer_literals: BTreeSet<String>,
    /// Stable diagnostic required from the unmodified template.
    pub expected_failure_diagnostic: String,
}

/// Observed results from the eight-phase production template harness.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HarnessObservation {
    /// Placeholder paths reported for the source template.
    pub placeholder_paths: BTreeSet<String>,
    /// Integration IDs present in the checked production registry.
    pub known_integration_ids: BTreeSet<String>,
    /// Integration IDs forced enabled and validated in isolation.
    pub enabled_integration_probes: BTreeSet<String>,
    /// Provider profiles compiled by positive and negative probes.
    pub profile_compiler_probes: BTreeSet<String>,
    /// Exact source-template literals observed at every checked consumer.
    pub consumed_literals: BTreeSet<String>,
    /// Unknown keys accepted by a disabled block, which must remain empty.
    pub unknown_keys: BTreeSet<String>,
    /// Profile IDs whose invalid-config negative probe passed unexpectedly.
    pub invalid_profiles: BTreeSet<String>,
    /// Secret paths that were not resolved through the fake store.
    pub unresolved_secrets: BTreeSet<String>,
    /// Optional blocks that were validated only while inactive.
    pub inactive_shortcuts: BTreeSet<String>,
    /// Stable diagnostic observed for the unmodified template.
    pub failure_diagnostic: String,
}

/// Validate exact sets from a completed production template-harness run.
///
/// # Errors
///
/// Returns an error when any phase is missing, contains extras, or reports a
/// different stable diagnostic.
pub fn validate_harness_observation(
    contract: &TemplateContract,
    observation: &HarnessObservation,
) -> Result<(), Report<SettingsError>> {
    if observation.placeholder_paths != contract.placeholder_paths {
        return Err(Report::new(SettingsError::HarnessMismatch {
            phase: "placeholder set",
        }));
    }
    if observation.known_integration_ids != contract.integration_ids
        || !observation.unknown_keys.is_empty()
    {
        return Err(Report::new(SettingsError::HarnessMismatch {
            phase: "unknown integration keys",
        }));
    }
    if !observation.invalid_profiles.is_empty() {
        return Err(Report::new(SettingsError::HarnessMismatch {
            phase: "profile rejection",
        }));
    }
    if !observation.unresolved_secrets.is_empty() {
        return Err(Report::new(SettingsError::HarnessMismatch {
            phase: "secret resolution",
        }));
    }
    if observation.consumed_literals != contract.consumer_literals {
        return Err(Report::new(SettingsError::HarnessMismatch {
            phase: "literal consumer equality",
        }));
    }
    if observation.enabled_integration_probes != contract.integration_ids
        || !observation.inactive_shortcuts.is_empty()
    {
        return Err(Report::new(SettingsError::HarnessMismatch {
            phase: "enabled integration probes",
        }));
    }
    if observation.profile_compiler_probes != contract.profile_ids {
        return Err(Report::new(SettingsError::HarnessMismatch {
            phase: "profile compiler probes",
        }));
    }
    if observation.failure_diagnostic != contract.expected_failure_diagnostic {
        return Err(Report::new(SettingsError::HarnessMismatch {
            phase: "diagnostic equality",
        }));
    }
    Ok(())
}

/// Inclusive numeric validation bounds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationRange {
    /// Inclusive lower bound when declared.
    pub min: Option<i128>,
    /// Inclusive upper bound when declared.
    pub max: Option<i128>,
}

/// One deserializable field and its serialization semantics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtractedField {
    /// Rust identifier before Serde renaming.
    pub rust_name: String,
    /// Canonical serialized key.
    pub name: String,
    /// Accepted deserialization-only aliases.
    pub aliases: Vec<String>,
    /// Whether the Rust type is Option.
    pub optional: bool,
    /// Whether the field merges into its container.
    pub flatten: bool,
    /// Whether the field is accepted but omitted from serialization.
    pub skip_serializing: bool,
    /// Resolved default semantics.
    pub default: Option<DefaultValue>,
    /// Custom deserializer path, when present.
    pub deserializer: Option<String>,
    /// Checked custom validator paths.
    pub validators: Vec<String>,
    /// Inclusive numeric range, when present.
    pub range: Option<ValidationRange>,
}

/// One Serde enum variant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtractedVariant {
    /// Rust variant identifier.
    pub rust_name: String,
    /// Canonical serialized name.
    pub name: String,
    /// Accepted deserialization-only aliases.
    pub aliases: Vec<String>,
}

/// Extracted struct or enum semantics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtractedType {
    /// Rust type name.
    pub name: String,
    /// Container rename rule.
    pub rename_all: Option<String>,
    /// Whether unknown object keys are rejected.
    pub deny_unknown_fields: bool,
    /// Adjacent or internal tag field.
    pub tag: Option<String>,
    /// Adjacent tag content field.
    pub content: Option<String>,
    /// Whether an enum is untagged.
    pub untagged: bool,
    /// Extracted deserializable fields.
    pub fields: Vec<ExtractedField>,
    /// Extracted enum variants.
    pub variants: Vec<ExtractedVariant>,
}

impl ExtractedType {
    /// Find a canonical field.
    #[must_use]
    pub fn field_named(&self, name: &str) -> Option<&ExtractedField> {
        self.fields.iter().find(|field| field.name == name)
    }

    /// Find a canonical enum variant.
    #[must_use]
    pub fn variant_named(&self, name: &str) -> Option<&ExtractedVariant> {
        self.variants.iter().find(|variant| variant.name == name)
    }
}

/// Extracted settings types from one Rust source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtractedSchema {
    /// Structs and enums in source order.
    pub types: Vec<ExtractedType>,
}

impl ExtractedSchema {
    /// Find an extracted Rust type by its source identifier.
    #[must_use]
    pub fn type_named(&self, name: &str) -> Option<&ExtractedType> {
        self.types.iter().find(|item| item.name == name)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "snake_case")]
enum CompanionKind {
    Default,
    Deserializer,
    Validator,
}

impl CompanionKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Deserializer => "deserializer",
            Self::Validator => "validator",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCompanionManifest {
    version: u32,
    reviewed: bool,
    #[serde(default)]
    companions: Vec<Companion>,
    #[serde(default)]
    fields: Vec<FieldDisposition>,
    template: Option<TemplateRecord>,
    #[serde(default)]
    consumers: Vec<ConsumerRecord>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TemplateRecord {
    source: String,
    placeholder_paths: BTreeSet<String>,
    integration_ids: BTreeSet<String>,
    profile_ids: BTreeSet<String>,
    consumer_literals: BTreeSet<String>,
    expected_failure_diagnostic: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "snake_case")]
enum ConsumerMode {
    LiteralSubstitution,
    IncludeOnly,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsumerRecord {
    path: String,
    mode: ConsumerMode,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Companion {
    source: String,
    symbol: String,
    kind: CompanionKind,
    value: Option<String>,
    positive_probe: String,
    negative_probe: String,
}

/// Reviewed custom-semantics record used by the extractor.
#[derive(Clone, Debug, Default)]
pub struct CompanionManifest {
    entries: BTreeMap<(String, String, CompanionKind), Companion>,
    fields: BTreeMap<String, FieldDisposition>,
    template: Option<TemplateRecord>,
    consumers: BTreeMap<String, ConsumerMode>,
}

impl CompanionManifest {
    /// Parse and validate a versioned companion manifest.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid TOML, missing review attestation, duplicate
    /// keys, blank probes, or a default companion without a resolved value.
    pub fn parse(source: &str) -> Result<Self, Report<SettingsError>> {
        let raw: RawCompanionManifest =
            toml::from_str(source).change_context(SettingsError::CompanionSyntax)?;
        if raw.version != 1 || !raw.reviewed {
            return Err(Report::new(SettingsError::CompanionReview));
        }

        let RawCompanionManifest {
            version: _,
            reviewed: _,
            companions,
            fields: raw_fields,
            template,
            consumers: raw_consumers,
        } = raw;

        let mut entries = BTreeMap::new();
        for companion in companions {
            if companion.source.trim().is_empty()
                || companion.symbol.trim().is_empty()
                || companion.positive_probe.trim().is_empty()
                || companion.negative_probe.trim().is_empty()
            {
                return Err(Report::new(SettingsError::InvalidCompanion {
                    symbol: companion.symbol,
                    reason: "source, symbol, and both probes must be nonblank",
                }));
            }
            if companion.kind == CompanionKind::Default
                && companion.value.as_deref().is_none_or(str::is_empty)
            {
                return Err(Report::new(SettingsError::InvalidCompanion {
                    symbol: companion.symbol,
                    reason: "default companion requires a value",
                }));
            }
            let key = (
                companion.source.clone(),
                companion.symbol.clone(),
                companion.kind,
            );
            if entries.insert(key, companion).is_some() {
                return Err(Report::new(SettingsError::InvalidCompanion {
                    symbol: "duplicate".to_owned(),
                    reason: "duplicate source/symbol/kind",
                }));
            }
        }
        let mut fields = BTreeMap::new();
        for field in raw_fields {
            let alias_shape = match field.key_identity {
                KeyIdentity::Alias => field
                    .alias_of
                    .as_deref()
                    .is_some_and(|value| !value.is_empty()),
                KeyIdentity::Canonical => field.alias_of.is_none(),
            };
            if field.path.trim().is_empty() || !alias_shape {
                return Err(Report::new(SettingsError::InvalidCompanion {
                    symbol: field.path,
                    reason: "alias fields require alias_of and canonical fields prohibit alias_of",
                }));
            }
            if fields.insert(field.path.clone(), field).is_some() {
                return Err(Report::new(SettingsError::InvalidCompanion {
                    symbol: "duplicate field".to_owned(),
                    reason: "duplicate field disposition path",
                }));
            }
        }
        let mut consumers = BTreeMap::new();
        for consumer in raw_consumers {
            if consumer.path.trim().is_empty()
                || consumers
                    .insert(consumer.path.clone(), consumer.mode)
                    .is_some()
            {
                return Err(Report::new(SettingsError::InvalidCompanion {
                    symbol: consumer.path,
                    reason: "consumer paths must be nonblank and unique",
                }));
            }
        }
        Ok(Self {
            entries,
            fields,
            template,
            consumers,
        })
    }

    fn require(
        &self,
        source: &str,
        symbol: &str,
        kind: CompanionKind,
    ) -> Result<&Companion, Report<SettingsError>> {
        self.entries
            .get(&(source.to_owned(), symbol.to_owned(), kind))
            .ok_or_else(|| {
                Report::new(SettingsError::MissingCompanion {
                    source: source.to_owned(),
                    symbol: symbol.to_owned(),
                    kind: kind.label(),
                })
            })
    }

    /// Find the exact independent dispositions for a setting path.
    #[must_use]
    pub fn field_disposition(&self, path: &str) -> Option<&FieldDisposition> {
        self.fields.get(path)
    }

    fn verify_probe_declarations(
        &self,
        source_path: &str,
        source: &str,
    ) -> Result<(), Report<SettingsError>> {
        for companion in self
            .entries
            .values()
            .filter(|companion| companion.source == source_path)
        {
            for probe in [&companion.positive_probe, &companion.negative_probe] {
                if !source.contains(&format!("fn {probe}()")) {
                    return Err(Report::new(SettingsError::InvalidContract {
                        reason: format!(
                            "{} companion {} has no compiled probe {}",
                            companion.kind.label(),
                            companion.symbol,
                            probe
                        ),
                    }));
                }
            }
        }
        Ok(())
    }
}

/// Check the real settings sources, compiled companions, template sets, and
/// every exact-string consumer without modifying repository bytes.
pub(crate) fn check_repository(repository: &Repository) -> Result<(), Report<SettingsError>> {
    let manifest_source = read_repository_text(repository, COMPANION_MANIFEST_PATH, false)?;
    let companions = CompanionManifest::parse(&manifest_source)?;

    let actual_sources = companions
        .entries
        .keys()
        .map(|(source, _, _)| source.clone())
        .collect::<BTreeSet<_>>();
    let expected_sources = COMPANION_SOURCES
        .iter()
        .map(|source| (*source).to_owned())
        .collect::<BTreeSet<_>>();
    require_equal("companion source set", &actual_sources, &expected_sources)?;

    for (source_path, required_type) in SOURCE_TYPES {
        let source = read_repository_text(repository, source_path, true)?;
        companions.verify_probe_declarations(source_path, &source)?;
        let schema = extract_schema(source_path, &source, &companions)?;
        if schema.type_named(required_type).is_none() {
            return invalid_contract(format!(
                "{source_path} does not extract required type {required_type}"
            ));
        }
        if *required_type == "Settings" {
            let actual = schema
                .type_named("Settings")
                .expect("required Settings type was checked")
                .fields
                .iter()
                .map(|field| field.name.clone())
                .collect::<BTreeSet<_>>();
            let expected = [
                "publisher",
                "tester_cookie",
                "trusted_client_ip",
                "ec",
                "integrations",
                "handlers",
                "response_headers",
                "request_signing",
                "rewrite",
                "auction",
                "consent",
                "cache",
                "proxy",
                "creative_opportunities",
                "image_optimizer",
                "tinybird",
                "debug",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
            require_equal("Settings root field set", &actual, &expected)?;
        }
    }

    check_field_dispositions(&companions)?;
    check_template_contract(repository, &companions)
}

fn check_field_dispositions(companions: &CompanionManifest) -> Result<(), Report<SettingsError>> {
    let store_resolved = string_set(&[
        "DataDomeConfig.server_side_key_secret_name",
        "DataDomeProtectionTestBypassConfig.credential_secret_name",
        "Ec.passphrase",
        "EcPartner.api_token",
        "EcPartner.ts_pull_token",
        "Handler.password",
        "Publisher.proxy_secret",
        "S3SigV4AuthConfig.access_key_id",
        "S3SigV4AuthConfig.secret_access_key",
        "S3SigV4AuthConfig.session_token",
        "TinybirdSettings.auction_token_secret",
    ]);
    let deliberately_inline = string_set(&["TrustedClientIpConfig.shared_secret"]);
    let accepted_discarded = string_set(&["TinybirdSettings.access_token_secret"]);
    let legacy_selectors = string_set(&[
        "DataDomeConfig.server_side_key_secret_store",
        "DataDomeProtectionTestBypassConfig.credential_secret_store",
        "S3SigV4AuthConfig.secret_store",
        "TinybirdSettings.secret_store",
    ]);
    let aliases = BTreeMap::from([
        (
            "AssetOriginAuth.s3_sig_v4".to_owned(),
            "AssetOriginAuth.s3".to_owned(),
        ),
        (
            "LegacyApsProviderConfig.pub_id".to_owned(),
            "LegacyApsProviderConfig.account_id".to_owned(),
        ),
    ]);
    let expected_paths = store_resolved
        .iter()
        .chain(&deliberately_inline)
        .chain(&accepted_discarded)
        .chain(&legacy_selectors)
        .chain(aliases.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let actual_paths = companions.fields.keys().cloned().collect::<BTreeSet<_>>();
    require_equal(
        "directional field disposition path set",
        &actual_paths,
        &expected_paths,
    )?;

    for (path, disposition) in &companions.fields {
        let expected = if store_resolved.contains(path) {
            (
                Lifecycle::Canonical,
                KeyIdentity::Canonical,
                None,
                SerializationDisposition::Serialized,
                RuntimeDisposition::Active,
                SecretDisposition::StoreResolved,
            )
        } else if deliberately_inline.contains(path) {
            (
                Lifecycle::Canonical,
                KeyIdentity::Canonical,
                None,
                SerializationDisposition::Serialized,
                RuntimeDisposition::Active,
                SecretDisposition::DeliberatelyInline,
            )
        } else if accepted_discarded.contains(path) {
            (
                Lifecycle::Deprecated,
                KeyIdentity::Canonical,
                None,
                SerializationDisposition::Skipped,
                RuntimeDisposition::NormalizedAway,
                SecretDisposition::AcceptedDiscarded,
            )
        } else if legacy_selectors.contains(path) {
            (
                Lifecycle::Deprecated,
                KeyIdentity::Canonical,
                None,
                SerializationDisposition::Skipped,
                RuntimeDisposition::NormalizedAway,
                SecretDisposition::None,
            )
        } else {
            (
                Lifecycle::Deprecated,
                KeyIdentity::Alias,
                aliases.get(path).map(String::as_str),
                SerializationDisposition::Skipped,
                RuntimeDisposition::DeserializationOnly,
                SecretDisposition::None,
            )
        };
        let actual = (
            disposition.lifecycle,
            disposition.key_identity,
            disposition.alias_of.as_deref(),
            disposition.serialization,
            disposition.runtime,
            disposition.secret,
        );
        require_equal(&format!("field disposition {path}"), &actual, &expected)?;
    }
    Ok(())
}

fn check_template_contract(
    repository: &Repository,
    companions: &CompanionManifest,
) -> Result<(), Report<SettingsError>> {
    let template = companions.template.as_ref().ok_or_else(|| {
        Report::new(SettingsError::InvalidContract {
            reason: "missing [template] record".to_owned(),
        })
    })?;
    if template.source != "trusted-server.example.toml" {
        return invalid_contract("template source must be trusted-server.example.toml".to_owned());
    }
    let source_template = read_repository_text(repository, &template.source, true)?;
    toml::from_str::<toml::Value>(&source_template).map_err(|error| {
        Report::new(SettingsError::InvalidContract {
            reason: format!("source template does not parse as TOML: {error}"),
        })
    })?;

    require_equal(
        "placeholder path set",
        &template.placeholder_paths,
        &string_set(&[
            "publisher.cookie_domain",
            "publisher.domain",
            "publisher.origin_url",
        ]),
    )?;
    require_equal(
        "integration ID set",
        &template.integration_ids,
        &string_set(&[
            "adserver_mock",
            "aps",
            "datadome",
            "didomi",
            "google_tag_manager",
            "gpt",
            "gpt_diagnostics",
            "lockr",
            "nextjs",
            "osano",
            "permutive",
            "prebid",
            "sourcepoint",
            "testlight",
        ]),
    )?;
    require_equal(
        "profile ID set",
        &template.profile_ids,
        &string_set(&["aps", "prebid-server", "standard"]),
    )?;
    require_equal(
        "consumer literal set",
        &template.consumer_literals,
        &string_set(&[
            "ec_passphrase",
            "handler_password",
            "publisher_proxy_secret",
        ]),
    )?;
    if template.expected_failure_diagnostic
        != "unmodified template rejects publisher.cookie_domain, publisher.domain, publisher.origin_url"
    {
        return invalid_contract("unexpected template failure diagnostic contract".to_owned());
    }

    let expected_consumers = BTreeMap::from([
        (
            "crates/trusted-server-cli/src/commands/audit/generate/mod.rs".to_owned(),
            ConsumerMode::LiteralSubstitution,
        ),
        (
            "crates/trusted-server-cli/src/commands/audit/generate/validate.rs".to_owned(),
            ConsumerMode::LiteralSubstitution,
        ),
        (
            "crates/trusted-server-cli/src/commands/config/ad_templates.rs".to_owned(),
            ConsumerMode::LiteralSubstitution,
        ),
        (
            "crates/trusted-server-cli/src/commands/config/init.rs".to_owned(),
            ConsumerMode::IncludeOnly,
        ),
        (
            "crates/trusted-server-core/src/config.rs".to_owned(),
            ConsumerMode::LiteralSubstitution,
        ),
        (
            "scripts/template-cache-local-test.sh".to_owned(),
            ConsumerMode::LiteralSubstitution,
        ),
    ]);
    require_equal(
        "template consumer path/mode set",
        &companions.consumers,
        &expected_consumers,
    )?;

    for (path, mode) in &companions.consumers {
        let consumer = read_repository_text(repository, path, true)?;
        let required = match mode {
            ConsumerMode::LiteralSubstitution => template
                .consumer_literals
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ConsumerMode::IncludeOnly => vec![template.source.as_str()],
        };
        for literal in required {
            if !consumer.contains(literal) {
                return invalid_contract(format!(
                    "template consumer {path} is missing exact literal {literal}"
                ));
            }
        }
    }
    Ok(())
}

fn read_repository_text(
    repository: &Repository,
    path: &str,
    tracked: bool,
) -> Result<String, Report<SettingsError>> {
    let normalized =
        NormalizedRelativePath::new(FsPath::new(path)).change_context(SettingsError::Repository)?;
    let bytes = if tracked {
        repository
            .read_tracked(&normalized)
            .change_context(SettingsError::Repository)?
    } else {
        repository
            .read_optional(&normalized)
            .change_context(SettingsError::Repository)?
            .ok_or_else(|| {
                Report::new(SettingsError::Repository).attach(format!("missing {path}"))
            })?
    };
    if bytes.len() > MAX_SETTINGS_INPUT_BYTES {
        return invalid_contract(format!("{path} exceeds {MAX_SETTINGS_INPUT_BYTES} bytes"));
    }
    String::from_utf8(bytes).map_err(|error| {
        Report::new(SettingsError::InvalidContract {
            reason: format!("{path} is not UTF-8: {error}"),
        })
    })
}

fn string_set(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn require_equal<T: core::fmt::Debug + Eq>(
    label: &str,
    actual: &T,
    expected: &T,
) -> Result<(), Report<SettingsError>> {
    if actual == expected {
        Ok(())
    } else {
        invalid_contract(format!(
            "{label} mismatch: expected {expected:?}, observed {actual:?}"
        ))
    }
}

fn invalid_contract<T>(reason: String) -> Result<T, Report<SettingsError>> {
    Err(Report::new(SettingsError::InvalidContract { reason }))
}

#[derive(Default)]
struct SerdeAttributes {
    rename: Option<String>,
    rename_all: Option<String>,
    aliases: Vec<String>,
    deny_unknown_fields: bool,
    tag: Option<String>,
    content: Option<String>,
    untagged: bool,
    flatten: bool,
    skip: bool,
    skip_serializing: bool,
    default: Option<Option<String>>,
    deserialize_with: Option<String>,
}

#[derive(Default)]
struct ValidationAttributes {
    validators: BTreeSet<String>,
    range: Option<ValidationRange>,
}

/// Parse one Rust source and extract its closed Serde/settings grammar.
///
/// # Errors
///
/// Returns an error for invalid Rust syntax, unsupported shape-changing Serde
/// attributes, malformed supported attributes, or missing custom-semantics
/// companions.
pub fn extract_schema(
    source_path: &str,
    source: &str,
    companions: &CompanionManifest,
) -> Result<ExtractedSchema, Report<SettingsError>> {
    let file = syn::parse_file(source).change_context(SettingsError::RustSyntax)?;
    let literal_defaults = literal_default_functions(&file.items);
    let mut types = Vec::new();

    for item in &file.items {
        match item {
            Item::Struct(item) => {
                let name = item.ident.to_string();
                let serde = parse_serde_attributes(&item.attrs, &name, AttributeSite::Container)?;
                let validation = parse_validation_attributes(&item.attrs, &name)?;
                require_validators(source_path, companions, &validation.validators)?;
                let fields = extract_fields(
                    source_path,
                    &name,
                    &item.fields,
                    serde.rename_all.as_deref(),
                    &literal_defaults,
                    companions,
                )?;
                types.push(ExtractedType {
                    name,
                    rename_all: serde.rename_all,
                    deny_unknown_fields: serde.deny_unknown_fields,
                    tag: serde.tag,
                    content: serde.content,
                    untagged: serde.untagged,
                    fields,
                    variants: Vec::new(),
                });
            }
            Item::Enum(item) => {
                let name = item.ident.to_string();
                let serde = parse_serde_attributes(&item.attrs, &name, AttributeSite::Container)?;
                let mut variants = Vec::new();
                for variant in &item.variants {
                    let rust_name = variant.ident.to_string();
                    let context = format!("{name}::{rust_name}");
                    let attributes =
                        parse_serde_attributes(&variant.attrs, &context, AttributeSite::Variant)?;
                    if attributes.skip {
                        continue;
                    }
                    let variant_name = attributes.rename.unwrap_or_else(|| {
                        apply_rename_rule(&rust_name, serde.rename_all.as_deref())
                    });
                    variants.push(ExtractedVariant {
                        rust_name,
                        name: variant_name,
                        aliases: attributes.aliases,
                    });
                }
                types.push(ExtractedType {
                    name,
                    rename_all: serde.rename_all,
                    deny_unknown_fields: serde.deny_unknown_fields,
                    tag: serde.tag,
                    content: serde.content,
                    untagged: serde.untagged,
                    fields: Vec::new(),
                    variants,
                });
            }
            _ => {}
        }
    }

    Ok(ExtractedSchema { types })
}

fn extract_fields(
    source_path: &str,
    container: &str,
    fields: &Fields,
    rename_all: Option<&str>,
    literal_defaults: &BTreeMap<String, String>,
    companions: &CompanionManifest,
) -> Result<Vec<ExtractedField>, Report<SettingsError>> {
    let mut extracted = Vec::new();
    for field in fields {
        let Some(identifier) = &field.ident else {
            continue;
        };
        let rust_name = identifier.to_string();
        let context = format!("{container}.{rust_name}");
        let serde = parse_serde_attributes(&field.attrs, &context, AttributeSite::Field)?;
        if serde.skip {
            continue;
        }
        let validation = parse_validation_attributes(&field.attrs, &context)?;
        require_validators(source_path, companions, &validation.validators)?;
        if let Some(symbol) = &serde.deserialize_with {
            companions.require(source_path, symbol, CompanionKind::Deserializer)?;
        }
        let default = match serde.default {
            None => None,
            Some(None) => Some(DefaultValue::Trait),
            Some(Some(symbol)) => match literal_defaults.get(&symbol) {
                Some(value) => Some(DefaultValue::Literal(value.clone())),
                None => {
                    let companion =
                        companions.require(source_path, &symbol, CompanionKind::Default)?;
                    Some(DefaultValue::Companion(
                        companion.value.clone().expect("validated default value"),
                    ))
                }
            },
        };
        extracted.push(ExtractedField {
            rust_name: rust_name.clone(),
            name: serde
                .rename
                .unwrap_or_else(|| apply_rename_rule(&rust_name, rename_all)),
            aliases: serde.aliases,
            optional: option_inner(&field.ty),
            flatten: serde.flatten,
            skip_serializing: serde.skip_serializing,
            default,
            deserializer: serde.deserialize_with,
            validators: validation.validators.into_iter().collect(),
            range: validation.range,
        });
    }
    Ok(extracted)
}

fn require_validators(
    source_path: &str,
    companions: &CompanionManifest,
    validators: &BTreeSet<String>,
) -> Result<(), Report<SettingsError>> {
    for validator in validators {
        companions.require(source_path, validator, CompanionKind::Validator)?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum AttributeSite {
    Container,
    Field,
    Variant,
}

fn parse_serde_attributes(
    attributes: &[Attribute],
    item: &str,
    site: AttributeSite,
) -> Result<SerdeAttributes, Report<SettingsError>> {
    let mut parsed = SerdeAttributes::default();
    for attribute in attributes
        .iter()
        .filter(|value| value.path().is_ident("serde"))
    {
        attribute
            .parse_nested_meta(|meta| {
                let Some(name) = meta.path.get_ident().map(ToString::to_string) else {
                    return Err(meta.error("unsupported serde attribute path"));
                };
                match name.as_str() {
                    "rename" => parsed.rename = Some(parse_string_value(&meta)?),
                    "rename_all" => parsed.rename_all = Some(parse_string_value(&meta)?),
                    "alias" => parsed.aliases.push(parse_string_value(&meta)?),
                    "deny_unknown_fields" => parsed.deny_unknown_fields = true,
                    "tag" => parsed.tag = Some(parse_string_value(&meta)?),
                    "content" => parsed.content = Some(parse_string_value(&meta)?),
                    "untagged" => parsed.untagged = true,
                    "flatten" => parsed.flatten = true,
                    "skip" => parsed.skip = true,
                    "skip_serializing" => parsed.skip_serializing = true,
                    "default" if meta.input.peek(syn::Token![=]) => {
                        parsed.default = Some(Some(parse_path_string_value(&meta)?));
                    }
                    "default" => parsed.default = Some(None),
                    "deserialize_with" => {
                        parsed.deserialize_with = Some(parse_path_string_value(&meta)?);
                    }
                    "skip_serializing_if"
                    | "serialize_with"
                    | "bound"
                    | "borrow"
                    | "crate"
                    | "expecting"
                    | "other"
                    | "skip_deserializing"
                    | "rename_all_fields" => {
                        consume_optional_value(&meta)?;
                    }
                    "from" | "try_from" | "into" | "remote" | "transparent" | "with" | "getter" => {
                        return Err(meta.error(format!("unsupported serde attribute {name}")));
                    }
                    _ => return Err(meta.error(format!("unsupported serde attribute {name}"))),
                }
                Ok(())
            })
            .map_err(|error| {
                let attribute = unsupported_attribute_name(&error.to_string());
                Report::new(SettingsError::UnsupportedSerdeAttribute {
                    attribute,
                    item: item.to_owned(),
                })
            })?;
    }

    if matches!(site, AttributeSite::Field) && (parsed.tag.is_some() || parsed.untagged) {
        return Err(Report::new(SettingsError::InvalidAttribute {
            attribute: "serde",
            item: item.to_owned(),
        }));
    }
    Ok(parsed)
}

fn unsupported_attribute_name(message: &str) -> String {
    message
        .split("unsupported serde attribute ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or("syntax")
        .to_owned()
}

fn consume_optional_value(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<()> {
    if meta.input.peek(syn::Token![=]) {
        let _ = meta.value()?.parse::<Expr>()?;
    }
    Ok(())
}

fn parse_string_value(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<String> {
    Ok(meta.value()?.parse::<syn::LitStr>()?.value())
}

fn parse_path_string_value(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<String> {
    let expression = meta.value()?.parse::<Expr>()?;
    match expression {
        Expr::Lit(ExprLit {
            lit: Lit::Str(value),
            ..
        }) => Ok(value.value()),
        Expr::Path(value) => Ok(path_text(&value.path)),
        _ => Err(meta.error("expected a string or path")),
    }
}

fn parse_validation_attributes(
    attributes: &[Attribute],
    item: &str,
) -> Result<ValidationAttributes, Report<SettingsError>> {
    let mut parsed = ValidationAttributes::default();
    for attribute in attributes
        .iter()
        .filter(|value| value.path().is_ident("validate"))
    {
        attribute
            .parse_nested_meta(|meta| {
                if meta.path.is_ident("custom") || meta.path.is_ident("schema") {
                    meta.parse_nested_meta(|nested| {
                        if nested.path.is_ident("function") {
                            parsed.validators.insert(parse_path_string_value(&nested)?);
                        } else {
                            consume_optional_value(&nested)?;
                        }
                        Ok(())
                    })?;
                } else if meta.path.is_ident("range") {
                    let mut range = ValidationRange {
                        min: None,
                        max: None,
                    };
                    meta.parse_nested_meta(|nested| {
                        if nested.path.is_ident("min") {
                            range.min = Some(parse_integer_value(&nested)?);
                        } else if nested.path.is_ident("max") {
                            range.max = Some(parse_integer_value(&nested)?);
                        } else {
                            consume_optional_value(&nested)?;
                        }
                        Ok(())
                    })?;
                    parsed.range = Some(range);
                } else if meta.input.peek(syn::token::Paren) {
                    meta.parse_nested_meta(|nested| consume_optional_value(&nested))?;
                } else {
                    consume_optional_value(&meta)?;
                }
                Ok(())
            })
            .map_err(|_| {
                Report::new(SettingsError::InvalidAttribute {
                    attribute: "validate",
                    item: item.to_owned(),
                })
            })?;
    }
    Ok(parsed)
}

fn parse_integer_value(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<i128> {
    let expression = meta.value()?.parse::<Expr>()?;
    match expression {
        Expr::Lit(ExprLit {
            lit: Lit::Int(value),
            ..
        }) => value.base10_parse(),
        _ => Err(meta.error("expected integer literal")),
    }
}

fn literal_default_functions(items: &[Item]) -> BTreeMap<String, String> {
    items
        .iter()
        .filter_map(|item| {
            let Item::Fn(function) = item else {
                return None;
            };
            let [Stmt::Expr(expression, None)] = function.block.stmts.as_slice() else {
                return None;
            };
            literal_expression(expression).map(|value| (function.sig.ident.to_string(), value))
        })
        .collect()
}

fn literal_expression(expression: &Expr) -> Option<String> {
    match expression {
        Expr::Lit(ExprLit { lit, .. }) => match lit {
            Lit::Str(value) => Some(value.value()),
            Lit::ByteStr(value) => Some(String::from_utf8_lossy(&value.value()).into_owned()),
            Lit::Byte(value) => Some(value.value().to_string()),
            Lit::Char(value) => Some(value.value().to_string()),
            Lit::Int(value) => Some(value.base10_digits().to_owned()),
            Lit::Float(value) => Some(value.base10_digits().to_owned()),
            Lit::Bool(value) => Some(value.value.to_string()),
            _ => None,
        },
        Expr::Unary(unary) => literal_expression(&unary.expr).map(|value| format!("-{value}")),
        _ => None,
    }
}

fn option_inner(field_type: &Type) -> bool {
    let Type::Path(path) = field_type else {
        return false;
    };
    path.path
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "Option")
}

fn path_text(path: &Path) -> String {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

fn apply_rename_rule(name: &str, rule: Option<&str>) -> String {
    match rule {
        None => name.to_owned(),
        Some("lowercase") => name.to_ascii_lowercase(),
        Some("UPPERCASE") => name.to_ascii_uppercase(),
        Some("snake_case") => words(name).join("_"),
        Some("SCREAMING_SNAKE_CASE") => words(name).join("_").to_ascii_uppercase(),
        Some("kebab-case") => words(name).join("-"),
        Some("SCREAMING-KEBAB-CASE") => words(name).join("-").to_ascii_uppercase(),
        Some("camelCase") => {
            let pieces = words(name);
            let Some((first, rest)) = pieces.split_first() else {
                return String::new();
            };
            let mut result = first.clone();
            for piece in rest {
                result.push_str(&capitalize(piece));
            }
            result
        }
        Some("PascalCase") => words(name).iter().map(|word| capitalize(word)).collect(),
        Some(_) => name.to_owned(),
    }
}

fn words(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let characters = name.chars().collect::<Vec<_>>();
    for (index, character) in characters.iter().copied().enumerate() {
        if character == '_' || character == '-' {
            if !current.is_empty() {
                words.push(current.to_ascii_lowercase());
                current.clear();
            }
            continue;
        }
        let prior_lower = index > 0 && characters[index - 1].is_ascii_lowercase();
        let next_lower = characters
            .get(index + 1)
            .is_some_and(char::is_ascii_lowercase);
        if character.is_ascii_uppercase() && !current.is_empty() && (prior_lower || next_lower) {
            words.push(current.to_ascii_lowercase());
            current.clear();
        }
        current.push(character);
    }
    if !current.is_empty() {
        words.push(current.to_ascii_lowercase());
    }
    words
}

fn capitalize(value: &str) -> String {
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return String::new();
    };
    let mut result = first.to_ascii_uppercase().to_string();
    result.extend(characters);
    result
}
