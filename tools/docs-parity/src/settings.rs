//! Serde-aware configuration schema extraction and checked companion records.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path as FsPath;

use error_stack::{Report, ResultExt as _};
use serde::Deserialize;
use syn::{Attribute, Expr, ExprLit, Fields, Item, Lit, Path, Stmt, Type, UnOp};

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
    /// Uses the enclosing struct's container-level default.
    Container,
}

/// Default applied when a struct is absent during deserialization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContainerDefault {
    /// Uses the struct type's `Default` implementation.
    Trait,
    /// Uses the exact named function.
    Function(String),
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
    /// Rust identifier before Serde renaming, or the zero-based tuple index.
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
    /// Whether the field is output-only and ignored during deserialization.
    pub skip_deserializing: bool,
    /// Conditional serialization predicate, when declared.
    pub skip_serializing_if: Option<String>,
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
    /// Rename rule for fields of this struct variant.
    pub rename_all: Option<String>,
    /// Whether this variant is omitted from serialization.
    pub skip_serializing: bool,
    /// Whether this variant is ignored during deserialization.
    pub skip_deserializing: bool,
    /// Whether this variant is untagged inside an otherwise tagged enum.
    pub untagged: bool,
    /// Fields carried by this variant.
    pub fields: Vec<ExtractedField>,
}

impl ExtractedVariant {
    /// Find a canonical field carried by this variant.
    #[must_use]
    pub fn field_named(&self, name: &str) -> Option<&ExtractedField> {
        self.fields.iter().find(|field| field.name == name)
    }
}

/// Extracted struct or enum semantics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtractedType {
    /// Rust type name.
    pub name: String,
    /// Serialized container name after an explicit rename.
    pub serialized_name: String,
    /// Container rename rule.
    pub rename_all: Option<String>,
    /// Default rename rule for fields of enum struct variants.
    pub rename_all_fields: Option<String>,
    /// Struct-level default applied to missing fields.
    pub container_default: Option<ContainerDefault>,
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

fn companion_probe_name(source: &str, symbol: &str, kind: CompanionKind, polarity: &str) -> String {
    let mut identity = String::new();
    for character in format!("{source}_{symbol}").chars() {
        if character.is_ascii_alphanumeric() {
            identity.push(character.to_ascii_lowercase());
        } else if !identity.ends_with('_') {
            identity.push('_');
        }
    }
    let identity = identity.trim_matches('_');
    format!("task7_{identity}_{}_{}", kind.label(), polarity)
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

/// Compiled evidence for one exact companion record.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CompanionReceipt {
    /// Exact source-local symbol.
    pub symbol: String,
    /// Companion kind (`default`, `deserializer`, or `validator`).
    pub kind: String,
    /// Runtime-observed default value, when applicable.
    pub value: Option<String>,
    /// Unique positive probe bound to source, symbol, and kind.
    pub positive_probe: String,
    /// Unique negative probe bound to source, symbol, and kind.
    pub negative_probe: String,
    /// Whether the compiled positive behavior passed.
    pub positive_passed: bool,
    /// Whether the compiled negative behavior rejected its input.
    pub negative_passed: bool,
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
            let expected_positive = companion_probe_name(
                &companion.source,
                &companion.symbol,
                companion.kind,
                "positive",
            );
            let expected_negative = companion_probe_name(
                &companion.source,
                &companion.symbol,
                companion.kind,
                "negative",
            );
            if companion.positive_probe != expected_positive
                || companion.negative_probe != expected_negative
            {
                return Err(Report::new(SettingsError::InvalidCompanion {
                    symbol: companion.symbol,
                    reason: "probe names must bind the exact source, symbol, kind, and polarity",
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

    /// Compare compiled companion evidence with one source's reviewed records.
    ///
    /// # Errors
    ///
    /// Returns an error for missing, stale, duplicated, failed, or mismatched
    /// source/symbol/kind/value/probe evidence.
    pub fn verify_compiled_receipts(
        &self,
        source: &str,
        receipts: &[CompanionReceipt],
    ) -> Result<(), Report<SettingsError>> {
        if receipts
            .iter()
            .any(|receipt| !receipt.positive_passed || !receipt.negative_passed)
        {
            return invalid_contract(format!(
                "compiled companion receipt failed positive/negative behavior for {source}"
            ));
        }
        let actual = receipts.iter().cloned().collect::<BTreeSet<_>>();
        if actual.len() != receipts.len() {
            return invalid_contract(format!(
                "compiled companion receipt set contains duplicates for {source}"
            ));
        }
        let expected = self
            .entries
            .values()
            .filter(|companion| companion.source == source)
            .map(|companion| CompanionReceipt {
                symbol: companion.symbol.clone(),
                kind: companion.kind.label().to_owned(),
                value: companion.value.clone(),
                positive_probe: companion.positive_probe.clone(),
                negative_probe: companion.negative_probe.clone(),
                positive_passed: true,
                negative_passed: true,
            })
            .collect::<BTreeSet<_>>();
        require_equal("compiled companion receipt set", &actual, &expected)
    }

    fn verify_discovered_companions(
        &self,
        source: &str,
        discovered: &BTreeSet<(String, CompanionKind)>,
    ) -> Result<(), Report<SettingsError>> {
        let expected = self
            .entries
            .keys()
            .filter(|(entry_source, _symbol, _kind)| entry_source == source)
            .map(|(_source, symbol, kind)| (symbol.clone(), *kind))
            .collect::<BTreeSet<_>>();
        require_equal("AST companion set", discovered, &expected)
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
    rename_all_fields: Option<String>,
    aliases: Vec<String>,
    deny_unknown_fields: bool,
    tag: Option<String>,
    content: Option<String>,
    untagged: bool,
    flatten: bool,
    skip: bool,
    skip_serializing: bool,
    skip_deserializing: bool,
    skip_serializing_if: Option<String>,
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
    let mut discovered_companions = BTreeSet::new();

    for item in &file.items {
        match item {
            Item::Struct(item) => {
                let name = item.ident.to_string();
                let serde = parse_serde_attributes(
                    &item.attrs,
                    &name,
                    AttributeSite::StructContainer {
                        has_fields: !matches!(&item.fields, Fields::Unit),
                        named_fields: matches!(&item.fields, Fields::Named(_)),
                    },
                )?;
                let validation = parse_validation_attributes(&item.attrs, &name)?;
                require_validators(
                    source_path,
                    companions,
                    &validation.validators,
                    &mut discovered_companions,
                )?;
                let container_default = serde.default.as_ref().map(|symbol| match symbol {
                    None => ContainerDefault::Trait,
                    Some(symbol) => ContainerDefault::Function(symbol.clone()),
                });
                if let Some(ContainerDefault::Function(symbol)) = &container_default {
                    discovered_companions.insert((symbol.clone(), CompanionKind::Default));
                    companions.require(source_path, symbol, CompanionKind::Default)?;
                }
                let fields = extract_fields(
                    &mut FieldExtractionContext {
                        source_path,
                        literal_defaults: &literal_defaults,
                        companions,
                        discovered_companions: &mut discovered_companions,
                    },
                    &name,
                    &item.fields,
                    serde.rename_all.as_deref(),
                    container_default.as_ref(),
                )?;
                types.push(ExtractedType {
                    serialized_name: serde.rename.clone().unwrap_or_else(|| name.clone()),
                    name,
                    rename_all: serde.rename_all,
                    rename_all_fields: serde.rename_all_fields,
                    container_default,
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
                let serde =
                    parse_serde_attributes(&item.attrs, &name, AttributeSite::EnumContainer)?;
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
                        apply_rename_rule(
                            &rust_name,
                            serde.rename_all.as_deref(),
                            RenameTarget::Variant,
                        )
                    });
                    let field_rename = attributes
                        .rename_all
                        .as_deref()
                        .or(serde.rename_all_fields.as_deref());
                    let fields = extract_fields(
                        &mut FieldExtractionContext {
                            source_path,
                            literal_defaults: &literal_defaults,
                            companions,
                            discovered_companions: &mut discovered_companions,
                        },
                        &format!("{name}::{rust_name}"),
                        &variant.fields,
                        field_rename,
                        None,
                    )?;
                    variants.push(ExtractedVariant {
                        rust_name,
                        name: variant_name,
                        aliases: attributes.aliases,
                        rename_all: attributes.rename_all,
                        skip_serializing: attributes.skip_serializing,
                        skip_deserializing: attributes.skip_deserializing,
                        untagged: attributes.untagged,
                        fields,
                    });
                }
                types.push(ExtractedType {
                    serialized_name: serde.rename.clone().unwrap_or_else(|| name.clone()),
                    name,
                    rename_all: serde.rename_all,
                    rename_all_fields: serde.rename_all_fields,
                    container_default: None,
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

    companions.verify_discovered_companions(source_path, &discovered_companions)?;
    Ok(ExtractedSchema { types })
}

struct FieldExtractionContext<'a> {
    source_path: &'a str,
    literal_defaults: &'a BTreeMap<String, String>,
    companions: &'a CompanionManifest,
    discovered_companions: &'a mut BTreeSet<(String, CompanionKind)>,
}

fn extract_fields(
    context: &mut FieldExtractionContext<'_>,
    container: &str,
    fields: &Fields,
    rename_all: Option<&str>,
    container_default: Option<&ContainerDefault>,
) -> Result<Vec<ExtractedField>, Report<SettingsError>> {
    let mut extracted = Vec::new();
    for (index, field) in fields.iter().enumerate() {
        let rust_name = field
            .ident
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| index.to_string());
        let field_context = format!("{container}.{rust_name}");
        let serde = parse_serde_attributes(
            &field.attrs,
            &field_context,
            AttributeSite::Field {
                named: field.ident.is_some(),
            },
        )?;
        if serde.skip {
            continue;
        }
        let validation = parse_validation_attributes(&field.attrs, &field_context)?;
        require_validators(
            context.source_path,
            context.companions,
            &validation.validators,
            context.discovered_companions,
        )?;
        if let Some(symbol) = &serde.deserialize_with {
            context
                .discovered_companions
                .insert((symbol.clone(), CompanionKind::Deserializer));
            context
                .companions
                .require(context.source_path, symbol, CompanionKind::Deserializer)?;
        }
        let default = match serde.default {
            None => None,
            Some(None) => Some(DefaultValue::Trait),
            Some(Some(symbol)) => match context.literal_defaults.get(&symbol) {
                Some(value) => Some(DefaultValue::Literal(value.clone())),
                None => {
                    context
                        .discovered_companions
                        .insert((symbol.clone(), CompanionKind::Default));
                    let companion = context.companions.require(
                        context.source_path,
                        &symbol,
                        CompanionKind::Default,
                    )?;
                    Some(DefaultValue::Companion(
                        companion.value.clone().expect("validated default value"),
                    ))
                }
            },
        };
        let default = default
            .or_else(|| container_default.map(|_default| DefaultValue::Container))
            .or_else(|| serde.skip_deserializing.then_some(DefaultValue::Trait));
        extracted.push(ExtractedField {
            rust_name: rust_name.clone(),
            name: serde
                .rename
                .unwrap_or_else(|| apply_rename_rule(&rust_name, rename_all, RenameTarget::Field)),
            aliases: serde.aliases,
            optional: option_inner(&field.ty),
            flatten: serde.flatten,
            skip_serializing: serde.skip_serializing,
            skip_deserializing: serde.skip_deserializing,
            skip_serializing_if: serde.skip_serializing_if,
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
    discovered_companions: &mut BTreeSet<(String, CompanionKind)>,
) -> Result<(), Report<SettingsError>> {
    for validator in validators {
        discovered_companions.insert((validator.clone(), CompanionKind::Validator));
        companions.require(source_path, validator, CompanionKind::Validator)?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum AttributeSite {
    StructContainer {
        has_fields: bool,
        named_fields: bool,
    },
    EnumContainer,
    Field {
        named: bool,
    },
    Variant,
}

fn parse_serde_attributes(
    attributes: &[Attribute],
    item: &str,
    site: AttributeSite,
) -> Result<SerdeAttributes, Report<SettingsError>> {
    let mut parsed = SerdeAttributes::default();
    let mut invalid_site = false;
    let mut invalid_form = false;
    for attribute in attributes
        .iter()
        .filter(|value| value.path().is_ident("serde"))
    {
        attribute
            .parse_nested_meta(|meta| {
                let Some(name) = meta.path.get_ident().map(ToString::to_string) else {
                    return Err(meta.error("unsupported serde attribute path"));
                };
                if serde_attribute_intentionally_unsupported(&name) {
                    return Err(meta.error(format!("unsupported serde attribute {name}")));
                }
                if !serde_attribute_allowed(&name, site) {
                    invalid_site = true;
                    return Err(meta.error(format!("invalid serde attribute {name} for this site")));
                }
                invalid_form = false;
                match name.as_str() {
                    "rename" => {
                        invalid_form = true;
                        parsed.rename = Some(parse_string_value(&meta)?);
                    }
                    "rename_all" => {
                        invalid_form = true;
                        parsed.rename_all = Some(parse_string_value(&meta)?);
                    }
                    "rename_all_fields" => {
                        invalid_form = true;
                        parsed.rename_all_fields = Some(parse_string_value(&meta)?);
                    }
                    "alias" => {
                        invalid_form = true;
                        parsed.aliases.push(parse_string_value(&meta)?);
                    }
                    "deny_unknown_fields" => parsed.deny_unknown_fields = true,
                    "tag" => {
                        invalid_form = true;
                        parsed.tag = Some(parse_string_value(&meta)?);
                    }
                    "content" => {
                        invalid_form = true;
                        parsed.content = Some(parse_string_value(&meta)?);
                    }
                    "untagged" => parsed.untagged = true,
                    "flatten" => parsed.flatten = true,
                    "skip" => parsed.skip = true,
                    "skip_serializing" => parsed.skip_serializing = true,
                    "skip_deserializing" => parsed.skip_deserializing = true,
                    "skip_serializing_if" => {
                        invalid_form = true;
                        parsed.skip_serializing_if = Some(parse_serde_path_string_value(&meta)?);
                    }
                    "default" if meta.input.peek(syn::Token![=]) => {
                        invalid_form = true;
                        parsed.default = Some(Some(parse_serde_path_string_value(&meta)?));
                    }
                    "default" => parsed.default = Some(None),
                    "deserialize_with" => {
                        invalid_form = true;
                        parsed.deserialize_with = Some(parse_serde_path_string_value(&meta)?);
                    }
                    "serialize_with" | "bound" | "borrow" | "crate" | "expecting" | "other"
                    | "from" | "try_from" | "into" | "remote" | "transparent" | "with"
                    | "getter" => {
                        return Err(meta.error(format!("unsupported serde attribute {name}")));
                    }
                    _ => return Err(meta.error(format!("unsupported serde attribute {name}"))),
                }
                invalid_form = false;
                Ok(())
            })
            .map_err(|error| {
                if invalid_site || invalid_form {
                    Report::new(SettingsError::InvalidAttribute {
                        attribute: "serde",
                        item: item.to_owned(),
                    })
                } else {
                    let attribute = unsupported_attribute_name(&error.to_string());
                    Report::new(SettingsError::UnsupportedSerdeAttribute {
                        attribute,
                        item: item.to_owned(),
                    })
                }
            })?;
    }

    for (attribute, rule) in [
        ("rename_all", parsed.rename_all.as_deref()),
        ("rename_all_fields", parsed.rename_all_fields.as_deref()),
    ] {
        if let Some(rule) = rule
            && !valid_rename_rule(rule)
        {
            return Err(Report::new(SettingsError::InvalidAttribute {
                attribute,
                item: item.to_owned(),
            }));
        }
    }
    if parsed.content.is_some() && parsed.tag.is_none()
        || parsed.untagged && (parsed.tag.is_some() || parsed.content.is_some())
    {
        return Err(Report::new(SettingsError::InvalidAttribute {
            attribute: "serde",
            item: item.to_owned(),
        }));
    }
    Ok(parsed)
}

fn serde_attribute_allowed(name: &str, site: AttributeSite) -> bool {
    match site {
        AttributeSite::StructContainer {
            has_fields,
            named_fields,
        } => match name {
            "rename" | "rename_all" | "deny_unknown_fields" => true,
            "default" => has_fields,
            "tag" => named_fields,
            _ => false,
        },
        AttributeSite::EnumContainer => matches!(
            name,
            "rename"
                | "rename_all"
                | "rename_all_fields"
                | "deny_unknown_fields"
                | "tag"
                | "content"
                | "untagged"
        ),
        AttributeSite::Field { named } => {
            matches!(
                name,
                "rename"
                    | "alias"
                    | "default"
                    | "skip"
                    | "skip_serializing"
                    | "skip_deserializing"
                    | "skip_serializing_if"
                    | "deserialize_with"
            ) || name == "flatten" && named
        }
        AttributeSite::Variant => matches!(
            name,
            "rename"
                | "alias"
                | "rename_all"
                | "skip"
                | "skip_serializing"
                | "skip_deserializing"
                | "untagged"
        ),
    }
}

fn serde_attribute_intentionally_unsupported(name: &str) -> bool {
    matches!(
        name,
        "serialize_with"
            | "bound"
            | "borrow"
            | "crate"
            | "expecting"
            | "other"
            | "from"
            | "try_from"
            | "into"
            | "remote"
            | "transparent"
            | "with"
            | "getter"
    )
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

fn parse_serde_path_string_value(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<String> {
    let value = meta.value()?.parse::<syn::LitStr>()?.value();
    syn::parse_str::<syn::ExprPath>(&value)
        .map_err(|_| meta.error("expected a string containing a function path"))?;
    Ok(value)
}

fn parse_validation_path_value(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<String> {
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
                            parsed
                                .validators
                                .insert(parse_validation_path_value(&nested)?);
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
        Expr::Unary(unary) if matches!(unary.op, UnOp::Neg(_)) => match unary.expr.as_ref() {
            Expr::Lit(ExprLit {
                lit: Lit::Int(value),
                ..
            }) => Some(format!("-{}", value.base10_digits())),
            Expr::Lit(ExprLit {
                lit: Lit::Float(value),
                ..
            }) => Some(format!("-{}", value.base10_digits())),
            _ => None,
        },
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

#[derive(Clone, Copy)]
enum RenameTarget {
    Field,
    Variant,
}

fn valid_rename_rule(rule: &str) -> bool {
    matches!(
        rule,
        "lowercase"
            | "UPPERCASE"
            | "PascalCase"
            | "camelCase"
            | "snake_case"
            | "SCREAMING_SNAKE_CASE"
            | "kebab-case"
            | "SCREAMING-KEBAB-CASE"
    )
}

fn apply_rename_rule(name: &str, rule: Option<&str>, target: RenameTarget) -> String {
    let Some(rule) = rule else {
        return name.to_owned();
    };
    debug_assert!(valid_rename_rule(rule), "rename rules should be validated");
    match target {
        RenameTarget::Field => apply_field_rename_rule(name, rule),
        RenameTarget::Variant => apply_variant_rename_rule(name, rule),
    }
}

fn apply_field_rename_rule(name: &str, rule: &str) -> String {
    match rule {
        "lowercase" | "snake_case" => name.to_owned(),
        "UPPERCASE" => name.to_ascii_uppercase(),
        "PascalCase" => {
            let mut output = String::new();
            let mut capitalize = true;
            for character in name.chars() {
                if character == '_' {
                    capitalize = true;
                } else if capitalize {
                    output.push(character.to_ascii_uppercase());
                    capitalize = false;
                } else {
                    output.push(character);
                }
            }
            output
        }
        "camelCase" => {
            let pascal = apply_field_rename_rule(name, "PascalCase");
            let mut characters = pascal.chars();
            match characters.next() {
                Some(first) => first.to_ascii_lowercase().to_string() + characters.as_str(),
                None => String::new(),
            }
        }
        "SCREAMING_SNAKE_CASE" => name.to_ascii_uppercase(),
        "kebab-case" => name.replace('_', "-"),
        "SCREAMING-KEBAB-CASE" => name.to_ascii_uppercase().replace('_', "-"),
        _ => unreachable!("validated rename rule should be complete"),
    }
}

fn apply_variant_rename_rule(name: &str, rule: &str) -> String {
    match rule {
        "PascalCase" => name.to_owned(),
        "lowercase" => name.to_ascii_lowercase(),
        "UPPERCASE" => name.to_ascii_uppercase(),
        "camelCase" => {
            let mut characters = name.chars();
            match characters.next() {
                Some(first) => first.to_ascii_lowercase().to_string() + characters.as_str(),
                None => String::new(),
            }
        }
        "snake_case" => {
            let mut output = String::new();
            for (index, character) in name.char_indices() {
                if index > 0 && character.is_uppercase() {
                    output.push('_');
                }
                output.push(character.to_ascii_lowercase());
            }
            output
        }
        "SCREAMING_SNAKE_CASE" => {
            apply_variant_rename_rule(name, "snake_case").to_ascii_uppercase()
        }
        "kebab-case" => apply_variant_rename_rule(name, "snake_case").replace('_', "-"),
        "SCREAMING-KEBAB-CASE" => {
            apply_variant_rename_rule(name, "SCREAMING_SNAKE_CASE").replace('_', "-")
        }
        _ => unreachable!("validated rename rule should be complete"),
    }
}
