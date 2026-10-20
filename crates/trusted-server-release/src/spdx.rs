//! Pure SPDX document rendering, with actual-build generation deliberately closed.
//!
//! Schema and graph validity do not establish complete composition coverage or
//! grant signing, publication or qualification authority. Complete owning-input
//! integration and full real-artifact inspection remain outstanding.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component as PathComponent, Path, PathBuf};

use error_stack::Report;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::documents::{Role, canonical_json, lower_hex, validate_spdx};
use crate::error::ReleaseError;
use crate::javascript::JsBuildCompleted;
use crate::materials::CargoOrigin;
use crate::policy::ReleaseTarget;
use crate::recipe::RecipeCompleted;

pub(crate) mod candidate;
pub(crate) mod cargo;
pub(crate) mod javascript;
pub(crate) mod native;
pub(crate) mod platform;

/// Validated bounded SPDX bytes, without a release-authorization capability.
#[derive(Debug)]
pub(crate) struct GeneratedSpdx {
    bytes: Vec<u8>,
    sha256: String,
}

impl GeneratedSpdx {
    /// The exact serialized SPDX document bytes.
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// SHA-256 of the exact document bytes, distinct from its executable root.
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }
}

/// Rejects actual builds until complete owning composition inputs are available.
///
/// No current candidate receipt establishes complete native/tool coverage. This
/// closed adapter performs no complete-input validation and emits no document.
///
/// # Errors
///
/// Always returns an error for the missing complete owning-input integration.
pub(crate) fn generate_from_completed(
    _completed: &RecipeCompleted,
    _javascript: &JsBuildCompleted,
) -> Result<GeneratedSpdx, Report<ReleaseError>> {
    Err(spdx_error(
        "complete owning composition input integration is unavailable; candidate native/tool coverage remains incomplete",
    ))
}

fn spdx_error(detail: impl std::fmt::Display) -> Report<ReleaseError> {
    Report::new(ReleaseError::Spdx).attach(detail.to_string())
}

// Only this module's tests construct positive inputs until the owning adapter
// can validate all required composition classes. No JSON constructor exists.
struct SpdxInputs<'a> {
    executable: &'a [u8],
    expected_artifact_sha256: String,
    name: String,
    version: String,
    source_commit: String,
    target: ReleaseTarget,
    created: String,
    // Rendered as `creationInfo.comment`; candidate documents state their gaps here.
    document_comment: Option<String>,
    components: Vec<Component>,
    dependencies: Vec<Dependency>,
}

struct Component {
    // The future owning adapter must derive this logical key from verified
    // original source/package identities, excluding temporary checkout/vendor
    // paths that can occur inside opaque Cargo IDs.
    identity: String,
    name: String,
    version: Option<String>,
    roles: Vec<ComponentRole>,
    origin: Option<Origin>,
    declared_license: Option<String>,
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
enum ComponentRole {
    ObservedRust,
    RetainedJavascript,
    ConservativeJavascript,
    ObservedNative,
    BuildMaterial,
    BuildTool,
    ExternalRuntime,
}

enum Origin {
    NativeVerified {
        original: native::NativeOriginal,
    },
    PlatformMeasuredFile {
        role: String,
        sha256: String,
    },
    JavascriptVerified {
        source_commit: String,
        original: javascript::JavascriptOriginal,
        producer: Option<String>,
    },
    CargoMaterial {
        source: String,
        original: CargoOrigin,
        prepared_manifest_sha256: String,
    },

    Source {
        commit: String,
        path: String,
    },
    Distribution {
        location: String,
        sha256: String,
    },
    StructuredTree {
        sha256: String,
    },
    Generated {
        producer: String,
        sha256: String,
    },
    RuntimeObservation {
        artifact_sha256: String,
    },
}

struct Dependency {
    from: String,
    to: String,
    build: bool,
}

const ROOT_ID: &str = "SPDXRef-Artifact";
const DOCUMENT_ID: &str = "SPDXRef-DOCUMENT";

fn plain(value: &str) -> Result<(), Report<ReleaseError>> {
    if value.is_empty() || value.len() > 8192 || value.chars().any(char::is_control) {
        return Err(spdx_error("SPDX input is not bounded plain text"));
    }
    Ok(())
}

fn relative_source(path: &str) -> Result<(), Report<ReleaseError>> {
    plain(path)?;
    if path.contains('\\')
        || Path::new(path)
            .components()
            .collect::<PathBuf>()
            .as_os_str()
            != Path::new(path).as_os_str()
        || Path::new(path)
            .components()
            .any(|component| !matches!(component, PathComponent::Normal(_)))
    {
        return Err(spdx_error("source origin path is not relative and normal"));
    }
    Ok(())
}

fn valid_created(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 20
        || [4, 7, 10, 13, 16, 19]
            .into_iter()
            .zip(b"--T::Z")
            .any(|(index, expected)| bytes[index] != *expected)
        || bytes
            .iter()
            .enumerate()
            .any(|(index, byte)| ![4, 7, 10, 13, 16, 19].contains(&index) && !byte.is_ascii_digit())
    {
        return false;
    }
    let number = |range| {
        value
            .get(range)
            .and_then(|part: &str| part.parse::<u32>().ok())
    };
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        number(0..4),
        number(5..7),
        number(8..10),
        number(11..13),
        number(14..16),
        number(17..19),
    ) else {
        return false;
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    };
    year != 0 && day != 0 && day <= days && hour < 24 && minute < 60 && second < 60
}

fn component_id(identity: &str) -> String {
    format!(
        "SPDXRef-Component-{}",
        hex::encode(Sha256::digest(identity.as_bytes()))
    )
}

fn license(declaration: Option<&str>) -> Result<(&'static str, String), Report<ReleaseError>> {
    let Some(declaration) = declaration else {
        return Ok((
            "NOASSERTION",
            "No declared license information or license conclusion is asserted.".to_owned(),
        ));
    };
    plain(declaration)?;
    let known = match declaration {
        "MIT" => "MIT",
        "Apache-2.0" => "Apache-2.0",
        "MIT OR Apache-2.0" => "MIT OR Apache-2.0",
        "Apache-2.0 OR MIT" => "Apache-2.0 OR MIT",
        _ => {
            return Ok((
                "NOASSERTION",
                format!(
                    "Unrecognized observed declaration: {declaration}. No license conclusion is asserted."
                ),
            ));
        }
    };
    Ok((
        known,
        "Exact reviewed declaration retained; no license conclusion is asserted.".to_owned(),
    ))
}

type OriginFields = (String, String, Option<(&'static str, String)>);

fn origin_fields(
    component: &Component,
    inputs: &SpdxInputs<'_>,
) -> Result<OriginFields, Report<ReleaseError>> {
    let origin = component
        .origin
        .as_ref()
        .ok_or_else(|| spdx_error("component has no contributing origin"))?;
    if component.roles.contains(&ComponentRole::ExternalRuntime)
        != matches!(origin, Origin::RuntimeObservation { .. })
        || component.roles.contains(&ComponentRole::ExternalRuntime) && component.roles.len() != 1
    {
        return Err(spdx_error(
            "runtime requirements and contributing origins have incompatible roles",
        ));
    }
    let unknown_location = "NOASSERTION".to_owned();
    match origin {
        Origin::NativeVerified { original } => native::origin_fields(component, original),
        Origin::PlatformMeasuredFile { role, sha256 } => {
            platform::file_origin_fields(component, role, sha256)
        }
        Origin::JavascriptVerified {
            source_commit,
            original,
            producer,
        } => javascript::origin_fields(
            component,
            inputs,
            source_commit,
            original,
            producer.as_deref(),
        ),
        Origin::CargoMaterial {
            source,
            original,
            prepared_manifest_sha256,
        } => {
            cargo::validate_material(source, original)?;
            if !lower_hex(prepared_manifest_sha256, 64) {
                return Err(spdx_error("prepared Cargo manifest SHA-256 is invalid"));
            }
            let facts = serde_json::to_string(&json!({"locked_source":source,"original":original,"prepared_manifest_sha256":prepared_manifest_sha256})).map_err(spdx_error)?;
            plain(&facts)?;
            let checksum = match original {
                CargoOrigin::Registry { archive_sha256 } => {
                    Some(("SHA256", archive_sha256.clone()))
                }
                CargoOrigin::Git { .. } => None,
            };
            let source_info = format!(
                "Verified original Cargo facts {facts}; prepared manifest and Git object identities are provenance evidence, not raw package checksums."
            );
            plain(&source_info)?;
            Ok((unknown_location, source_info, checksum))
        }

        Origin::Source { commit, path } => {
            if !lower_hex(commit, 40) || commit != &inputs.source_commit {
                return Err(spdx_error(
                    "workspace source origin differs from the selected source",
                ));
            }
            relative_source(path)?;
            Ok((
                unknown_location,
                format!("Approved source commit {commit}; source-relative path {path}."),
                None,
            ))
        }
        Origin::Distribution { location, sha256 } => {
            plain(location)?;
            if !location.starts_with("https://") || !lower_hex(sha256, 64) {
                return Err(spdx_error(
                    "distribution has no bounded origin and raw archive digest",
                ));
            }
            Ok((location.clone(),"Checksum identifies the original distribution archive, not the final executable or individual surviving functions.".to_owned(),Some(("SHA256", sha256.clone()))))
        }
        Origin::StructuredTree { sha256 } => {
            if !lower_hex(sha256, 64)
                || component.roles.iter().any(|role| {
                    !matches!(
                        role,
                        ComponentRole::BuildMaterial | ComponentRole::BuildTool
                    )
                })
            {
                return Err(spdx_error(
                    "structured tree evidence cannot replace a contributing original",
                ));
            }
            Ok((
                unknown_location,
                format!(
                    "Structured installed-tree evidence SHA-256 {sha256}; this is not a raw-file checksum or original acquisition provenance."
                ),
                None,
            ))
        }
        Origin::Generated { producer, sha256 } => {
            if !lower_hex(sha256, 64)
                || !inputs.components.iter().any(|candidate| {
                    candidate.identity == *producer
                        && candidate.identity != component.identity
                        && candidate.roles.contains(&ComponentRole::BuildTool)
                })
            {
                return Err(spdx_error(
                    "generated contributor has no known build-tool producer or byte digest",
                ));
            }
            Ok((
                unknown_location,
                format!(
                    "Observed generated module bytes before later transformations; producer identity {producer}."
                ),
                Some(("SHA256", sha256.clone())),
            ))
        }
        Origin::RuntimeObservation { artifact_sha256 } => {
            if artifact_sha256 != &inputs.expected_artifact_sha256 {
                return Err(spdx_error(
                    "runtime requirement belongs to another executable",
                ));
            }
            Ok((unknown_location,"Logical requirement observed in accepted executable bytes; installed runtime identity and bytes are not asserted.".to_owned(),None))
        }
    }
}

fn component_package(
    component: &Component,
    inputs: &SpdxInputs<'_>,
) -> Result<Value, Report<ReleaseError>> {
    plain(&component.name)?;
    let (declared, license_comment) = license(component.declared_license.as_deref())?;
    let (download, source_info, checksum) = origin_fields(component, inputs)?;
    let mut value = json!({"SPDXID":component_id(&component.identity),"name":component.name,"downloadLocation":download,"filesAnalyzed":false,"licenseDeclared":declared,"licenseConcluded":"NOASSERTION","licenseComments":license_comment,"copyrightText":"NOASSERTION","sourceInfo":source_info});
    if let Some(version) = &component.version {
        plain(version)?;
        value["versionInfo"] = json!(version);
    }
    if let Some((algorithm, digest)) = checksum {
        value["checksums"] = json!([{"algorithm":algorithm,"checksumValue":digest}]);
    }
    Ok(value)
}

fn render(inputs: &SpdxInputs<'_>) -> Result<GeneratedSpdx, Report<ReleaseError>> {
    plain(&inputs.name)?;
    plain(&inputs.version)?;
    if inputs.executable.is_empty()
        || inputs.executable.len() as u64 > Role::Executable.limit()
        || !lower_hex(&inputs.expected_artifact_sha256, 64)
        || hex::encode(Sha256::digest(inputs.executable)) != inputs.expected_artifact_sha256
        || !lower_hex(&inputs.source_commit, 40)
        || !valid_created(&inputs.created)
        || inputs.components.len() > 16_384
        || inputs.dependencies.len() > 65_536
    {
        return Err(spdx_error(
            "SPDX root identity, timestamp or input bounds are invalid",
        ));
    }
    let mut components = BTreeMap::new();
    let mut identifiers = BTreeSet::from([ROOT_ID.to_owned(), DOCUMENT_ID.to_owned()]);
    for component in &inputs.components {
        plain(&component.identity)?;
        if component.roles.is_empty()
            || component.roles.iter().collect::<BTreeSet<_>>().len() != component.roles.len()
        {
            return Err(spdx_error("component roles must be nonempty and unique"));
        }
        if components
            .insert(component.identity.as_str(), component)
            .is_some()
            || !identifiers.insert(component_id(&component.identity))
        {
            return Err(spdx_error(
                "duplicate component identity or SPDX identifier",
            ));
        }
    }
    let mut packages = vec![
        json!({"SPDXID":ROOT_ID,"name":inputs.name,"versionInfo":inputs.version,"downloadLocation":"NOASSERTION","filesAnalyzed":false,"licenseDeclared":"NOASSERTION","licenseConcluded":"NOASSERTION","copyrightText":"NOASSERTION","checksums":[{"algorithm":"SHA256","checksumValue":inputs.expected_artifact_sha256}],"sourceInfo":format!("Final executable for {}; approved source commit {}.",inputs.target.triple(),inputs.source_commit)}),
    ];
    let mut relationships = BTreeSet::new();
    relationships.insert((
        DOCUMENT_ID.to_owned(),
        "DESCRIBES",
        ROOT_ID.to_owned(),
        "Selected final executable.",
    ));
    for component in components.values() {
        packages.push(component_package(component, inputs)?);
        let id = component_id(&component.identity);
        for role in &component.roles {
            let (from, kind, to, comment) = match role {
                ComponentRole::ObservedRust | ComponentRole::ObservedNative => (
                    ROOT_ID.to_owned(),
                    "GENERATED_FROM",
                    id.clone(),
                    "Conservative observed compilation input; final function reachability is not asserted.",
                ),
                ComponentRole::RetainedJavascript => (
                    ROOT_ID.to_owned(),
                    "CONTAINS",
                    id.clone(),
                    "Complete module bytes observed in final executable; function reachability is not asserted.",
                ),
                ComponentRole::ConservativeJavascript => (
                    ROOT_ID.to_owned(),
                    "GENERATED_FROM",
                    id.clone(),
                    "Conservative post-Rollup rendered input before esbuild minification; final function reachability is not asserted.",
                ),
                ComponentRole::BuildMaterial => (
                    id.clone(),
                    "BUILD_DEPENDENCY_OF",
                    ROOT_ID.to_owned(),
                    "Prepared build input; final executable retention is not asserted.",
                ),
                ComponentRole::BuildTool => (
                    id.clone(),
                    "BUILD_TOOL_OF",
                    ROOT_ID.to_owned(),
                    "Build tool input; executable embedding and original acquisition are not asserted.",
                ),
                ComponentRole::ExternalRuntime => (
                    id.clone(),
                    "RUNTIME_DEPENDENCY_OF",
                    ROOT_ID.to_owned(),
                    "Logical external runtime requirement; runtime bytes are not embedded component evidence.",
                ),
            };
            relationships.insert((from, kind, to, comment));
        }
        if let Some(
            Origin::Generated { producer, .. }
            | Origin::JavascriptVerified {
                producer: Some(producer),
                ..
            },
        ) = &component.origin
        {
            relationships.insert((
                component_id(producer),
                "GENERATES",
                component_id(&component.identity),
                "Observed producer of generated module bytes before later transformations.",
            ));
        }
    }
    let mut dependency_keys = BTreeSet::new();
    for dependency in &inputs.dependencies {
        if !components.contains_key(dependency.from.as_str())
            || !components.contains_key(dependency.to.as_str())
            || dependency.from == dependency.to
            || !dependency_keys.insert((&dependency.from, &dependency.to, dependency.build))
        {
            return Err(spdx_error(
                "dependency is duplicated, self-referential or dangling",
            ));
        }
        let (from, kind, to) = if dependency.build {
            (
                component_id(&dependency.to),
                "BUILD_DEPENDENCY_OF",
                component_id(&dependency.from),
            )
        } else {
            (
                component_id(&dependency.from),
                "DEPENDS_ON",
                component_id(&dependency.to),
            )
        };
        relationships.insert((
            from,
            kind,
            to,
            "Potential Cargo package relation between observed endpoints; exact compiler unit dependency DAG is not asserted.",
        ));
    }
    let relationships:Vec<_> = relationships.into_iter().map(|(from,kind,to,comment)| json!({"spdxElementId":from,"relationshipType":kind,"relatedSpdxElement":to,"comment":comment})).collect();
    let mut document = json!({"SPDXID":DOCUMENT_ID,"spdxVersion":"SPDX-2.3","dataLicense":"CC0-1.0","name":format!("{} {} composition",inputs.name,inputs.version),"creationInfo":{"created":inputs.created,"creators":[format!("Tool: trusted-server-release-{}",env!("CARGO_PKG_VERSION"))]},"documentDescribes":[ROOT_ID],"packages":packages,"relationships":relationships});
    if let Some(comment) = &inputs.document_comment {
        plain(comment)?;
        document["creationInfo"]["comment"] = json!(comment);
    }
    let semantic_bytes = canonical_json(&document, Role::Spdx)
        .map_err(|error| error.change_context(ReleaseError::Spdx))?;
    document["documentNamespace"] = json!(format!(
        "https://spdx.org/spdxdocs/trusted-server-{}",
        hex::encode(Sha256::digest(&semantic_bytes))
    ));
    validate_spdx(&document).map_err(|error| error.change_context(ReleaseError::Spdx))?;
    let bytes = canonical_json(&document, Role::Spdx)
        .map_err(|error| error.change_context(ReleaseError::Spdx))?;
    let sha256 = hex::encode(Sha256::digest(&bytes));
    Ok(GeneratedSpdx { bytes, sha256 })
}

#[cfg(test)]
mod tests;
