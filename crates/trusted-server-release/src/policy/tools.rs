//! Strict parsing and compiled-pin comparison of `release-tools.json`.
//!
//! Qualification states and digests parsed here are diagnostics only; they
//! never construct production authority.

use std::collections::{BTreeMap, BTreeSet};

use error_stack::Report;
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value, json};

use super::{
    ATTEST_REVISION, ATTEST_VERSION, FIXED_ARTIFACTS, GH_VERSION, SCHEMA_VERSION, rejected,
};
use crate::documents::lower_hex;
use crate::error::ReleaseError;
use crate::recipe::AmbientProfile;

const RUST_VERSION: &str = "1.98.1";
const NODE_VERSION: &str = "24.20.0";
const NPM_VERSION: &str = "11.19.0";
const ATTEST_NAME: &str = "actions/attest:github-action";
const ARTIFACT_SCRIPT: &str = "scripts/release/build.sh";
const JS_RECIPE: &str = "tsjs";
const JS_SCRIPT: &str = "scripts/release/build-js.sh";
/// Builder environment shared by the Fastly, Linux CLI and JS recipes.
pub(super) const BUILDER_ENVIRONMENT: &str = "gnu-builder-x86_64";
const TOOL_HOSTS: [&str; 3] = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
];
const PER_HOST_TOOLS: [(&str, &str); 7] = [
    ("fastly", "16.0.0"),
    ("gh", GH_VERSION),
    ("node", NODE_VERSION),
    ("npm", NPM_VERSION),
    ("rust", RUST_VERSION),
    ("viceroy", "0.21.0"),
    ("wasmtime", "48.0.1"),
];

/// Fixed environment identity: id, kind, role and kind-specific constant.
const FIXED_ENVIRONMENTS: [(&str, &str, &str, &str); 6] = [
    (
        "debian12-x86_64",
        "oci-runtime",
        "compatibility-runtime",
        "",
    ),
    (BUILDER_ENVIRONMENT, "oci-builder", "compilation", "2.34"),
    (
        "macos15-aarch64",
        "github-hosted-macos",
        "compilation",
        "macos-15",
    ),
    (
        "macos15-x86_64",
        "github-hosted-macos",
        "compilation",
        "macos-15-intel",
    ),
    ("ubi9-x86_64", "oci-runtime", "compatibility-runtime", ""),
    (
        "ubuntu22.04-x86_64",
        "oci-runtime",
        "compatibility-runtime",
        "",
    ),
];

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Qualification {
    Pending,
    Qualified,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
/// Parsed `release-tools.json` with closed per-kind field sets.
pub(super) struct ToolsDocument {
    environments: Vec<Environment>,
    recipes: Vec<Recipe>,
    schema_version: u64,
    tools: Vec<ToolPin>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolPin {
    #[serde(deserialize_with = "nullable")]
    acquisition: Option<Value>,
    name: String,
    qualification: Qualification,
    #[serde(deserialize_with = "nullable")]
    revision: Option<String>,
    #[serde(deserialize_with = "nullable")]
    sha256: Option<String>,
    version: String,
}

#[derive(Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum Environment {
    #[serde(rename = "oci-runtime")]
    OciRuntime {
        #[serde(deserialize_with = "nullable")]
        acquisition: Option<Value>,
        id: String,
        #[serde(deserialize_with = "nullable")]
        image_reference: Option<String>,
        #[serde(deserialize_with = "nullable")]
        oci_index_sha256: Option<String>,
        #[serde(deserialize_with = "nullable")]
        oci_manifest_sha256: Option<String>,
        qualification: Qualification,
        role: String,
    },
    #[serde(rename = "oci-builder")]
    OciBuilder {
        #[serde(deserialize_with = "nullable")]
        acquisition: Option<Value>,
        glibc_maximum: String,
        id: String,
        #[serde(deserialize_with = "nullable")]
        image_reference: Option<String>,
        #[serde(deserialize_with = "nullable")]
        oci_index_sha256: Option<String>,
        #[serde(deserialize_with = "nullable")]
        oci_manifest_sha256: Option<String>,
        qualification: Qualification,
        role: String,
        #[serde(deserialize_with = "nullable")]
        sdk_sha256: Option<String>,
    },
    #[serde(rename = "github-hosted-macos")]
    HostedMacos {
        #[serde(deserialize_with = "nullable")]
        acquisition: Option<Value>,
        id: String,
        qualification: Qualification,
        role: String,
        #[serde(deserialize_with = "nullable")]
        runner_image_version: Option<String>,
        runner_label: String,
        #[serde(deserialize_with = "nullable")]
        sdk_sha256: Option<String>,
    },
}

/// A nullable pin field: present-or-null, never absent.
struct PinField<'a> {
    name: &'static str,
    value: Option<&'a str>,
    digest: bool,
}

impl<'a> PinField<'a> {
    fn new(name: &'static str, value: &'a Option<String>, digest: bool) -> Self {
        Self {
            name,
            value: value.as_deref(),
            digest,
        }
    }
}

impl Environment {
    fn identity(&self) -> (&str, &'static str, &str, &str) {
        match self {
            Self::OciRuntime { id, role, .. } => (id, "oci-runtime", role, ""),
            Self::OciBuilder {
                id,
                role,
                glibc_maximum,
                ..
            } => (id, "oci-builder", role, glibc_maximum),
            Self::HostedMacos {
                id,
                role,
                runner_label,
                ..
            } => (id, "github-hosted-macos", role, runner_label),
        }
    }

    fn qualification(&self) -> Qualification {
        match self {
            Self::OciRuntime { qualification, .. }
            | Self::OciBuilder { qualification, .. }
            | Self::HostedMacos { qualification, .. } => *qualification,
        }
    }

    fn acquisition(&self) -> Option<&Value> {
        match self {
            Self::OciRuntime { acquisition, .. }
            | Self::OciBuilder { acquisition, .. }
            | Self::HostedMacos { acquisition, .. } => acquisition.as_ref(),
        }
    }

    fn pins(&self) -> Vec<PinField<'_>> {
        let field = PinField::new;
        match self {
            Self::OciRuntime {
                image_reference,
                oci_index_sha256,
                oci_manifest_sha256,
                ..
            } => vec![
                field("image_reference", image_reference, false),
                field("oci_index_sha256", oci_index_sha256, true),
                field("oci_manifest_sha256", oci_manifest_sha256, true),
            ],
            Self::OciBuilder {
                image_reference,
                oci_index_sha256,
                oci_manifest_sha256,
                sdk_sha256,
                ..
            } => vec![
                field("image_reference", image_reference, false),
                field("oci_index_sha256", oci_index_sha256, true),
                field("oci_manifest_sha256", oci_manifest_sha256, true),
                field("sdk_sha256", sdk_sha256, true),
            ],
            Self::HostedMacos {
                runner_image_version,
                sdk_sha256,
                ..
            } => vec![
                field("runner_image_version", runner_image_version, false),
                field("sdk_sha256", sdk_sha256, true),
            ],
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Recipe {
    environment_id: String,
    id: String,
    qualification: Qualification,
    script: String,
    settings: Map<String, Value>,
    #[serde(deserialize_with = "nullable")]
    sha256: Option<String>,
}

fn nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

impl ToolsDocument {
    /// Lists pending qualifications and null pins in document order.
    pub(super) fn pending_items(&self) -> Vec<String> {
        let mut items = Vec::new();
        for environment in &self.environments {
            note_pending(
                &mut items,
                &format!("environment {}", environment.identity().0),
                environment.qualification(),
                Some(environment.acquisition()),
                &environment.pins(),
            );
        }
        for recipe in &self.recipes {
            let digest = PinField::new("sha256", &recipe.sha256, true);
            note_pending(
                &mut items,
                &format!("recipe {}", recipe.id),
                recipe.qualification,
                None,
                &[digest],
            );
        }
        for tool in &self.tools {
            let digest = PinField::new("sha256", &tool.sha256, true);
            note_pending(
                &mut items,
                &format!("tool {}", tool.name),
                tool.qualification,
                Some(tool.acquisition.as_ref()),
                &[digest],
            );
        }
        items
    }
}

/// Appends pending diagnostics; `acquisition` is `None` when the record has no such field.
fn note_pending(
    items: &mut Vec<String>,
    subject: &str,
    qualification: Qualification,
    acquisition: Option<Option<&Value>>,
    pins: &[PinField<'_>],
) {
    if qualification != Qualification::Qualified {
        items.push(format!("{subject}: qualification pending"));
    }
    if acquisition.is_some_and(|value| value.is_none()) {
        items.push(format!("{subject}: acquisition is null"));
    }
    for pin in pins.iter().filter(|pin| pin.value.is_none()) {
        items.push(format!("{subject}: {} is null", pin.name));
    }
}

fn expected_tools() -> BTreeMap<String, (&'static str, Option<&'static str>)> {
    let mut expected = BTreeMap::new();
    expected.insert(
        ATTEST_NAME.to_owned(),
        (ATTEST_VERSION, Some(ATTEST_REVISION)),
    );
    for (tool, version) in PER_HOST_TOOLS {
        for host in TOOL_HOSTS {
            expected.insert(format!("{tool}:{host}"), (version, None));
        }
    }
    expected
}

/// Compares parsed pins, environments and recipes with the compiled contract.
pub(super) fn check_tools(tools: &ToolsDocument) -> Result<(), Report<ReleaseError>> {
    if tools.schema_version != SCHEMA_VERSION {
        return Err(rejected("tools schema_version must be 1"));
    }
    let fixed = expected_tools();
    let mut seen = BTreeSet::new();
    for tool in &tools.tools {
        if !seen.insert(tool.name.as_str()) {
            return Err(rejected(format!("duplicate tool {}", tool.name)));
        }
        let (version, revision) = *fixed
            .get(&tool.name)
            .ok_or_else(|| rejected(format!("unknown tool {}", tool.name)))?;
        if tool.version != version || tool.revision.as_deref() != revision {
            return Err(rejected(format!(
                "tool {} must pin version {version} and revision {revision:?}",
                tool.name
            )));
        }
        check_digest(
            &format!("tool {}", tool.name),
            "sha256",
            tool.sha256.as_deref(),
        )?;
    }
    if let Some(missing) = fixed.keys().find(|name| !seen.contains(name.as_str())) {
        return Err(rejected(format!("missing tool {missing}")));
    }
    check_environments(&tools.environments)?;
    check_recipes(&tools.recipes)
}

fn check_digest(
    subject: &str,
    field: &str,
    value: Option<&str>,
) -> Result<(), Report<ReleaseError>> {
    match value {
        Some(digest) if !lower_hex(digest, 64) => Err(rejected(format!(
            "{subject}: {field} must be 64 lowercase hex digits or null"
        ))),
        _ => Ok(()),
    }
}

fn check_environments(environments: &[Environment]) -> Result<(), Report<ReleaseError>> {
    let mut seen = BTreeSet::new();
    for environment in environments {
        let identity = environment.identity();
        let id = identity.0;
        if !seen.insert(id) {
            return Err(rejected(format!("duplicate environment {id}")));
        }
        let fixed = FIXED_ENVIRONMENTS
            .iter()
            .find(|fixed| fixed.0 == id)
            .ok_or_else(|| rejected(format!("unknown environment {id}")))?;
        if (fixed.1, fixed.2, fixed.3) != (identity.1, identity.2, identity.3) {
            return Err(rejected(format!(
                "environment {id} changes its fixed kind, role or platform constant"
            )));
        }
        for pin in environment.pins().iter().filter(|pin| pin.digest) {
            check_digest(&format!("environment {id}"), pin.name, pin.value)?;
        }
    }
    if let Some(missing) = FIXED_ENVIRONMENTS
        .iter()
        .find(|fixed| !seen.contains(fixed.0))
    {
        return Err(rejected(format!("missing environment {}", missing.0)));
    }
    Ok(())
}

/// One source-owned recipe identity and the settings it must restate.
struct FixedRecipe {
    id: &'static str,
    environment: &'static str,
    script: &'static str,
    settings: Value,
}

/// Complete effective settings shared by every executable recipe.
fn artifact_settings() -> Value {
    json!({
        "ambient_profiles": AmbientProfile::tracked_json(),
        "aws_lc_target_selector": true,
        "aws_lc_use_system": false,
        "cargo_config": ".cargo/config.toml",
        "cargo_frozen": true,
        "cargo_home": "private-prepared",
        "cargo_locked": true,
        "cargo_message_format": "json-render-diagnostics",
        "download_control": "actual-cargo-build-script-required",
        "fresh_target": true,
        "network_boundary": "source-owned-os-denial-v1",
        "normalized_inputs": [
            "compiler-locale-C",
            "compiler-timezone-UTC",
            "source-commit-epoch",
            "remapped-paths",
        ],
        "npm_lifecycle": "reviewed-preparation-only",
        "release_debug": 1,
        "release_strip": "none",
        "rust_toolchain": RUST_VERSION,
        "sqlite_features": ["bundled"],
        "sqlite_use_pkg_config": false,
        "sysroot": "authenticated-sdk-required",
        "tool_paths": "authenticated-absolute-paths-required",
        "vite_config_loading": false,
        "vite_env_loading": false,
        "vite_mode": "production",
    })
}

/// Complete effective settings of the shared JS recipe.
fn js_settings() -> Value {
    json!({
        "compare": "all-tracked-final-module-bytes",
        "fresh_installations": 2,
        "fresh_private_npm_caches": true,
        "fresh_source": "tracked-objects-only",
        "mandatory_tests": true,
        "network_boundary": "source-owned-os-denial-v1",
        "node_version": NODE_VERSION,
        "npm_audit": false,
        "npm_fund": false,
        "npm_include_dev": true,
        "npm_lifecycle": "reviewed-observed-foreground",
        "npm_offline": true,
        "npm_version": NPM_VERSION,
        "publish": "A-only-after-equality",
        "tool_paths": "authenticated-absolute-paths-required",
        "vite_config_loading": false,
        "vite_env_loading": false,
        "vite_mode": "production",
    })
}

fn expected_recipes() -> Vec<FixedRecipe> {
    let mut recipes: Vec<_> = FIXED_ARTIFACTS
        .iter()
        .map(|fixed| FixedRecipe {
            id: fixed.name,
            environment: fixed.recipe_environment,
            script: ARTIFACT_SCRIPT,
            settings: artifact_settings(),
        })
        .collect();
    recipes.push(FixedRecipe {
        id: JS_RECIPE,
        environment: BUILDER_ENVIRONMENT,
        script: JS_SCRIPT,
        settings: js_settings(),
    });
    recipes
}

/// Names the first settings key whose presence or value differs.
fn first_settings_difference(expected: &Value, actual: &Map<String, Value>) -> Option<String> {
    let expected = expected.as_object()?;
    expected
        .keys()
        .chain(actual.keys())
        .find(|key| expected.get(*key) != actual.get(*key))
        .cloned()
}

fn check_recipes(recipes: &[Recipe]) -> Result<(), Report<ReleaseError>> {
    let expected = expected_recipes();
    let mut seen = BTreeSet::new();
    for recipe in recipes {
        let id = recipe.id.as_str();
        if !seen.insert(id) {
            return Err(rejected(format!("duplicate recipe {id}")));
        }
        let FixedRecipe {
            environment,
            script,
            settings,
            ..
        } = expected
            .iter()
            .find(|fixed| fixed.id == id)
            .ok_or_else(|| rejected(format!("unknown recipe {id}")))?;
        if recipe.environment_id != *environment {
            return Err(rejected(format!(
                "recipe {id} environment_id must be {environment}"
            )));
        }
        if recipe.script != *script {
            return Err(rejected(format!("recipe {id} script must be {script}")));
        }
        if let Some(key) = first_settings_difference(settings, &recipe.settings) {
            return Err(rejected(format!(
                "recipe {id} settings.{key} differs from the compiled recipe settings"
            )));
        }
        check_digest(&format!("recipe {id}"), "sha256", recipe.sha256.as_deref())?;
    }
    if let Some(missing) = expected.iter().find(|fixed| !seen.contains(fixed.id)) {
        return Err(rejected(format!("missing recipe {}", missing.id)));
    }
    Ok(())
}
