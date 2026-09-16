use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use error_stack::Report;
use serde::Deserialize;
use serde_json::json;
use serde_yaml_ng::Value;
use url::Url;

use super::{Output, PbsError, Result, identifier, read_text};

const MAX_CONFIG_BYTES: usize = 2 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Descriptor {
    schema_version: u32,
    environment: String,
    runtime: Runtime,
    aws: AwsTarget,
    pbs: PbsInputs,
    regions: BTreeMap<String, Region>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Runtime {
    Ec2Compose,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AwsTarget {
    pub account_id: String,
    pub profile: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PbsInputs {
    config: PathBuf,
    image: String,
    bindings: Option<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Region {
    #[serde(default)]
    pub instance_ids: Vec<String>,
    overrides: Option<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Binding {
    pub verified_image: String,
    pub source: String,
    pub secrets: BTreeMap<String, String>,
    pub keys: BTreeMap<String, KeyBinding>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct KeyBinding {
    pub env: String,
    pub pbs_path: Vec<String>,
    #[serde(default = "required")]
    pub required: bool,
}

fn required() -> bool {
    true
}

pub(super) struct Deployment {
    pub environment: String,
    pub aws: AwsTarget,
    pub regions: BTreeMap<String, Region>,
    pub bindings: BTreeMap<String, Binding>,
    pub rendered: BTreeMap<String, Value>,
    image: String,
}

impl Deployment {
    /// Load and validate every local deployment input without calling AWS or writing files.
    ///
    /// # Errors
    /// Rejects malformed schemas, unsupported runtimes, invalid targets, and binding conflicts.
    pub fn load(path: &Path) -> Result<Self> {
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        let descriptor: Descriptor = decode_yaml(path)?;
        let Descriptor {
            schema_version,
            environment,
            runtime: Runtime::Ec2Compose,
            aws,
            pbs,
            regions,
        } = descriptor;
        if schema_version != 1
            || !identifier(&environment)
            || !valid_account(&aws.account_id)
            || !identifier(&aws.profile)
            || regions.is_empty()
            || !pinned_image(&pbs.image)
        {
            return Err(invalid(
                "descriptor requires version 1, explicit account/profile, regions, and digest-pinned PBS image",
            ));
        }
        let baseline = read_yaml(&resolve(base, &pbs.config)?)?;
        if !baseline.is_mapping() {
            return Err(invalid("PBS configuration must be a YAML mapping"));
        }
        validate_yaml(&baseline)?;
        let bindings: BTreeMap<String, Binding> = match &pbs.bindings {
            Some(path) => decode_yaml(&resolve(base, path)?)?,
            None => BTreeMap::new(),
        };
        let mut rendered = BTreeMap::new();
        for (name, region) in &regions {
            if !valid_region(name)
                || region.instance_ids.len() > 100
                || region.instance_ids.iter().any(|id| !valid_instance(id))
                || region.instance_ids.iter().collect::<BTreeSet<_>>().len()
                    != region.instance_ids.len()
            {
                return Err(invalid("invalid region or EC2 instance identifiers"));
            }
            let mut config = baseline.clone();
            if let Some(path) = &region.overrides {
                let overrides = read_yaml(&resolve(base, path)?)?;
                if !overrides.is_mapping() {
                    return Err(invalid("regional overrides must be a YAML mapping"));
                }
                validate_yaml(&overrides)?;
                merge(&mut config, overrides);
            }
            rendered.insert(name.clone(), config);
        }
        let deployment = Self {
            environment,
            aws,
            regions,
            bindings,
            rendered,
            image: pbs.image,
        };
        deployment.validate_bindings()?;
        Ok(deployment)
    }

    /// Validate declared binding evidence and reject conflicting destinations in every region.
    ///
    /// # Errors
    /// Rejects incomplete metadata, wrong-account ARNs, duplicate destinations and YAML conflicts.
    fn validate_bindings(&self) -> Result<()> {
        let mut envs = BTreeSet::new();
        let mut paths = Vec::<Vec<String>>::new();
        let mut secret_arns = BTreeSet::new();
        for (bidder, binding) in &self.bindings {
            let source = Url::parse(&binding.source)
                .map_err(|_| invalid("binding source must be an HTTPS documentation URL"))?;
            if !identifier(bidder)
                || binding.verified_image != self.image
                || binding.keys.is_empty()
                || binding.secrets.is_empty()
                || source.scheme() != "https"
                || source.host_str().is_none()
                || !source.username().is_empty()
                || source.password().is_some()
            {
                return Err(invalid(
                    "binding requires matching image, documentation source, keys, and regional secret ARNs",
                ));
            }
            for (region, arn) in &binding.secrets {
                if !secret_arns.insert(arn) {
                    return Err(invalid(
                        "each secret ARN must belong to only one bidder binding",
                    ));
                }
                if !self.regions.contains_key(region)
                    || !secret_arn(arn, region, &self.aws.account_id)
                {
                    return Err(invalid(
                        "secret ARN must match a declared region and AWS account",
                    ));
                }
            }
            // Version 1 uses one binding set across regions; require explicit coverage.
            if binding.secrets.len() != self.regions.len() {
                return Err(invalid(
                    "every binding must declare a secret ARN for every deployment region",
                ));
            }
            for (key, target) in &binding.keys {
                if !identifier(key)
                    || !target.env.starts_with("PBS_")
                    || target.env.len() > 256
                    || !target.env.bytes().all(|byte| {
                        byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'
                    })
                    || target.pbs_path.is_empty()
                    || target.pbs_path.iter().any(|part| !identifier(part))
                {
                    return Err(invalid("invalid secret key or PBS binding destination"));
                }
                if !envs.insert(&target.env)
                    || paths.iter().any(|path| {
                        path.starts_with(&target.pbs_path) || target.pbs_path.starts_with(path)
                    })
                {
                    return Err(invalid(
                        "secret bindings must have distinct non-overlapping PBS destinations",
                    ));
                }
                paths.push(target.pbs_path.clone());
                for config in self.rendered.values() {
                    if occupied(config, &target.pbs_path) {
                        return Err(invalid(
                            "secret destination conflicts with a YAML value or non-mapping parent",
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

pub(super) fn valid_account(value: &str) -> bool {
    value.len() == 12 && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn valid_region(value: &str) -> bool {
    let parts: Vec<_> = value.split('-').collect();
    parts.len() >= 3
        && value.len() < 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && parts
            .last()
            .is_some_and(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn valid_instance(value: &str) -> bool {
    value.strip_prefix("i-").is_some_and(|id| {
        matches!(id.len(), 8 | 17) && id.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn pinned_image(value: &str) -> bool {
    value
        .rsplit_once("@sha256:")
        .is_some_and(|(image, digest)| {
            !image.is_empty()
                && !image.contains('@')
                && !image.chars().any(char::is_whitespace)
                && digest.len() == 64
                && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

fn secret_arn(value: &str, region: &str, account: &str) -> bool {
    let parts: Vec<_> = value.splitn(7, ':').collect();
    parts.len() == 7
        && parts[0] == "arn"
        && matches!(parts[1], "aws" | "aws-us-gov" | "aws-cn")
        && parts[2] == "secretsmanager"
        && parts[3] == region
        && parts[4] == account
        && parts[5] == "secret"
        && !parts[6].is_empty()
        && parts[6]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/_+=.@-".contains(&byte))
}

/// Resolve descriptor-relative paths, retaining support for explicit operator-owned absolute files.
///
/// # Errors
/// Rejects empty paths.
fn resolve(base: &Path, path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(invalid("configuration path cannot be empty"));
    }
    Ok(base.join(path))
}

/// Parse YAML into a value first to detect duplicate mapping keys before typed conversion.
///
/// # Errors
/// Rejects invalid YAML without exposing parser snippets.
fn read_yaml(path: &Path) -> Result<Value> {
    serde_yaml_ng::from_str(&read_text(path, MAX_CONFIG_BYTES)?)
        .map_err(|_| invalid("cannot parse YAML/JSON input; source details withheld"))
}

/// Decode a schema with unknown fields rejected by each descriptor type.
///
/// # Errors
/// Rejects unsupported schema fields and types without echoing values.
fn decode_yaml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let value = read_yaml(path)?;
    validate_yaml(&value)?;
    serde_yaml_ng::from_value(value).map_err(|_| {
        invalid("unsupported deployment/binding schema; see ts prebid server documentation")
    })
}

/// Require plain string-keyed data: YAML tags and merge keys obscure the effective configuration.
///
/// # Errors
/// Rejects tagged values, merge keys, and non-string mapping keys.
fn validate_yaml(value: &Value) -> Result<()> {
    match value {
        Value::Mapping(map) => {
            for (key, value) in map {
                if key.as_str().is_none_or(|key| key == "<<") {
                    return Err(invalid(
                        "YAML mappings require string keys and explicit overrides, not merge keys",
                    ));
                }
                validate_yaml(value)?;
            }
        }
        Value::Sequence(items) => {
            for item in items {
                validate_yaml(item)?;
            }
        }
        Value::Tagged(_) => return Err(invalid("YAML tags are not supported")),
        _ => {}
    }
    Ok(())
}

/// Recursively merge maps; replace arrays and scalar values as whole values.
fn merge(base: &mut Value, overrides: Value) {
    match (base, overrides) {
        (Value::Mapping(base), Value::Mapping(overrides)) => {
            for (key, value) in overrides {
                if let Some(previous) = base.get_mut(&key) {
                    merge(previous, value);
                } else {
                    base.insert(key, value);
                }
            }
        }
        (base, overrides) => *base = overrides,
    }
}

fn occupied(value: &Value, path: &[String]) -> bool {
    if path.is_empty() {
        return true;
    }
    match value.as_mapping() {
        Some(map) => map
            .get(Value::String(path[0].clone()))
            .is_some_and(|value| occupied(value, &path[1..])),
        None => true,
    }
}

pub(super) fn invalid(message: &'static str) -> Report<PbsError> {
    Report::new(PbsError::Input(message))
}

/// Report structural validation only, withholding resolved YAML values.
///
/// # Errors
/// Returns an error if deterministic rendering fails.
pub(super) fn check(deployment: &Deployment) -> Result<Output> {
    for value in deployment.rendered.values() {
        let rendered = serde_yaml_ng::to_string(value)
            .map_err(|_| invalid("cannot serialize resolved YAML"))?;
        let second = serde_yaml_ng::to_string(value)
            .map_err(|_| invalid("cannot serialize resolved YAML"))?;
        if rendered != second {
            return Err(invalid("regional rendering was not deterministic"));
        }
    }
    Ok(Output {
        failure: None,
        summary: format!("PBS local checks passed for {}", deployment.environment),
        details: vec![
            format!("Regions: {}", deployment.regions.keys().cloned().collect::<Vec<_>>().join(", ")),
            format!("Declared bidder secret bindings: {}", deployment.bindings.len()),
            "Structural checks only: PBS startup, adapter mappings, credential availability and AWS behavior remain unverified.".to_owned(),
            "No configuration files were written or AWS calls made.".to_owned(),
        ],
        data: json!({"environment": deployment.environment, "regions": deployment.regions.keys().collect::<Vec<_>>(),
            "binding_count": deployment.bindings.len(), "local_checks": "passed", "pbs_runtime_validation": "not_run",
            "aws_validation": "not_run", "binding_evidence": "operator_declared_not_independently_verified"}),
    })
}

#[cfg(test)]
pub(super) mod tests {
    use std::fs;

    use super::*;

    pub(crate) fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("should create temp directory");
        fs::write(
            dir.path().join("pbs.yaml"),
            "port: 8000\nauction_timeouts_ms:\n  default: 1000\n  max: 1200\nlist: [a, b]\n",
        )
        .expect("should write PBS config");
        fs::write(
            dir.path().join("east.yaml"),
            "auction_timeouts_ms:\n  default: 900\nlist: [c]\n",
        )
        .expect("should write overrides");
        let image = format!("registry.example.com/pbs@sha256:{}", "a".repeat(64));
        let path = dir.path().join("deployment.yaml");
        fs::write(&path, format!("schema_version: 1\nenvironment: sandbox\nruntime: ec2-compose\naws:\n  account_id: '123456789012'\n  profile: pbs-sandbox\npbs:\n  config: pbs.yaml\n  image: {image}\n  bindings: bindings.json\nregions:\n  us-east-1:\n    instance_ids: [i-0123456789abcdef0]\n    overrides: east.yaml\n")).expect("should write descriptor");
        let bindings = json!({"examplebidder": {
            "verified_image": image, "source": "https://example.com/adapter-reference",
            "secrets": {"us-east-1": "arn:aws:secretsmanager:us-east-1:123456789012:secret:pbs/example-AbCdEf"},
            "keys": {"api_key": {"env": "PBS_ADAPTERS_EXAMPLEBIDDER_API_KEY", "pbs_path": ["adapters", "examplebidder", "api_key"]}}
        }});
        fs::write(dir.path().join("bindings.json"), bindings.to_string())
            .expect("should write bindings");
        (dir, path)
    }

    #[test]
    fn resolves_relative_paths_merges_maps_and_replaces_arrays() {
        let (_dir, path) = fixture();
        let deployment = Deployment::load(&path).expect("should load descriptor");
        let east = &deployment.rendered["us-east-1"];
        assert_eq!(east["auction_timeouts_ms"]["default"].as_u64(), Some(900));
        assert_eq!(east["auction_timeouts_ms"]["max"].as_u64(), Some(1200));
        assert_eq!(
            east["list"].as_sequence().expect("should have list").len(),
            1
        );
        let again = Deployment::load(&path).expect("should load again");
        assert_eq!(deployment.rendered, again.rendered);
        assert_eq!(
            check(&deployment).expect("should validate").data["aws_validation"],
            "not_run"
        );
    }

    #[test]
    fn yaml_and_secret_destinations_cannot_conflict() {
        let (dir, path) = fixture();
        fs::write(
            dir.path().join("east.yaml"),
            "adapters:\n  examplebidder:\n    api_key: NEVER_PRINT_ME\n",
        )
        .expect("should write conflicting override");
        let error = Deployment::load(&path)
            .err()
            .expect("should reject conflicting source");
        assert!(error.to_string().contains("conflicts"));
        assert!(!format!("{error:?}").contains("NEVER_PRINT_ME"));
    }

    #[test]
    fn duplicate_yaml_keys_and_unsupported_runtime_fail_closed() {
        let (dir, path) = fixture();
        fs::write(dir.path().join("pbs.yaml"), "port: 8000\nport: 9000\n")
            .expect("should write duplicates");
        assert!(Deployment::load(&path).is_err());
        fs::write(dir.path().join("pbs.yaml"), "port: 8000\n").expect("should restore config");
        let text = fs::read_to_string(&path).expect("should read descriptor");
        fs::write(&path, text.replace("ec2-compose", "ecs")).expect("should change runtime");
        assert!(Deployment::load(&path).is_err());
    }

    #[test]
    fn bidders_cannot_overwrite_each_others_secret_object() {
        let (dir, path) = fixture();
        let file = dir.path().join("bindings.json");
        let mut bindings: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&file).expect("should read bindings"))
                .expect("should parse bindings");
        let mut other = bindings["examplebidder"].clone();
        other["keys"]["api_key"]["env"] = json!("PBS_ADAPTERS_OTHERBIDDER_API_KEY");
        other["keys"]["api_key"]["pbs_path"] = json!(["adapters", "otherbidder", "api_key"]);
        bindings["otherbidder"] = other;
        fs::write(file, bindings.to_string()).expect("should write bindings");
        assert!(
            Deployment::load(&path).is_err(),
            "complete-value writes require distinct secret ownership per bidder"
        );
    }

    #[test]
    fn unknown_schema_fields_and_wrong_account_secret_arns_are_rejected() {
        let (dir, path) = fixture();
        let original = fs::read_to_string(&path).expect("should read descriptor");
        fs::write(&path, format!("{original}typo: value\n")).expect("should append unknown field");
        assert!(Deployment::load(&path).is_err());
        fs::write(&path, original).expect("should restore descriptor");
        let bindings = dir.path().join("bindings.json");
        let original = fs::read_to_string(&bindings).expect("should read bindings");
        fs::write(bindings, original.replace("123456789012", "999999999999"))
            .expect("should change ARN");
        assert!(Deployment::load(&path).is_err());
    }
}
