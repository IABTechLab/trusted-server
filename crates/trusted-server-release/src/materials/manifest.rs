//! Reviews declarations only; this module never observes or executes hooks.

use error_stack::Report;
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};

use super::{ExtractedOriginal, material_error};
use crate::documents::{Role, parse_json};
use crate::error::ReleaseError;
use crate::source::ApprovedSource;

const ROOT_MANIFEST: &str = "crates/trusted-server-js/lib/package.json";
const ROOT_BINDING: &str = "crates/trusted-server-js/lib/binding.gyp";
const ROOT_EVENTS: [&str; 10] = [
    "preinstall",
    "install",
    "postinstall",
    "prepublish",
    "preprepare",
    "prepare",
    "postprepare",
    "predependencies",
    "dependencies",
    "postdependencies",
];

// These exact declarations are source-owned policy, separate from observations.
const REVIEWED_HOOKS: [(&str, &str, &str, &str); 2] = [
    (
        "core-js",
        "3.48.0",
        "postinstall",
        "node -e \"try{require('./postinstall')}catch(e){}\"",
    ),
    ("esbuild", "0.27.3", "postinstall", "node install.js"),
];

/// Approved root manifest declarations, without lifecycle execution authority.
pub(crate) struct RootManifestReview {
    source_commit: String,
    manifest_sha256: String,
}

impl RootManifestReview {
    /// Returns the approved source identity supplying the reviewed declarations.
    pub(crate) fn source_commit(&self) -> &str {
        &self.source_commit
    }
}

/// A declared install hook; this is not an observed lifecycle event.
pub(crate) struct DeclaredHook {
    event: String,
    script_sha256: String,
}

impl DeclaredHook {
    /// Returns the reviewed npm event name.
    pub(crate) fn event(&self) -> &str {
        &self.event
    }

    /// Returns the hash of exact UTF-8 command bytes from the verified manifest.
    pub(crate) fn script_sha256(&self) -> &str {
        &self.script_sha256
    }
}

/// Reviewed declarations from one retained, authenticated npm original.
///
/// This type cannot construct a lifecycle observation or completed build.
pub(crate) struct ReviewedNpmManifest {
    extracted: ExtractedOriginal,
    manifest_sha256: String,
    hooks: Vec<DeclaredHook>,
}

impl ReviewedNpmManifest {
    /// Returns exact original member facts after rechecking retained extraction.
    ///
    /// # Errors
    ///
    /// Returns an error for a stale extraction or absent member.
    pub(super) fn member_facts(
        &self,
        relative: &str,
    ) -> Result<(String, u64, u32), Report<ReleaseError>> {
        self.extracted.member_facts(relative)
    }
    /// Rechecks all archive members before preparation consumes this review.
    ///
    /// # Errors
    ///
    /// Returns an error if the retained extraction changed or expired.
    pub(crate) fn verify_unchanged(&self) -> Result<(), Report<ReleaseError>> {
        self.extracted.verify_unchanged()
    }

    /// Copies the authenticated compressed original for a new private cache.
    ///
    /// # Errors
    ///
    /// Returns an error if retained original bytes changed or copying fails.
    pub(super) fn copy_original(
        &mut self,
        destination: &mut impl std::io::Write,
    ) -> Result<(), Report<ReleaseError>> {
        self.extracted.copy_original(destination)
    }
    /// Returns declarations only, without claiming any hook was executed.
    pub(crate) fn hooks(&self) -> &[DeclaredHook] {
        &self.hooks
    }
}

/// Reviews only approved tracked root declarations.
///
/// # Errors
///
/// Returns an error for changed source, malformed manifests or root lifecycle hooks.
pub(super) fn review_root(
    source: &ApprovedSource,
) -> Result<RootManifestReview, Report<ReleaseError>> {
    source.snapshot().verify_unchanged()?;
    let bytes = source.read_tracked(ROOT_MANIFEST, Role::Metadata)?;
    let document = parse_json(&bytes, Role::Metadata)?;
    review_root_document(&document, source.tracked_sha256(ROOT_BINDING).is_some())?;
    source.snapshot().verify_unchanged()?;
    Ok(RootManifestReview {
        source_commit: source.commit().as_str().to_owned(),
        manifest_sha256: hex::encode(Sha256::digest(&bytes)),
    })
}

/// Reviews exact dependency declarations from authenticated archive members.
///
/// # Errors
///
/// Returns an error for changed members, identity substitutions or unreviewed hooks.
pub(super) fn review_npm(
    extracted: ExtractedOriginal,
    name: &str,
    version: &str,
) -> Result<ReviewedNpmManifest, Report<ReleaseError>> {
    let bytes = extracted.read_member_checked("package.json")?;
    let document = parse_json(&bytes, Role::Metadata)?;
    let binding = extracted.contains_member_checked("binding.gyp")?;
    let hooks = declared_hooks(&document, name, version, binding)?;
    Ok(ReviewedNpmManifest {
        extracted,
        manifest_sha256: hex::encode(Sha256::digest(&bytes)),
        hooks,
    })
}

fn scripts(document: &Value) -> Result<Option<&Map<String, Value>>, Report<ReleaseError>> {
    let object = document
        .as_object()
        .ok_or_else(|| material_error("package manifest must be an object"))?;
    match object.get("scripts") {
        None => Ok(None),
        Some(Value::Object(scripts)) if scripts.values().all(Value::is_string) => Ok(Some(scripts)),
        Some(_) => Err(material_error(
            "manifest scripts must contain command strings",
        )),
    }
}

fn review_root_document(document: &Value, binding: bool) -> Result<(), Report<ReleaseError>> {
    let scripts = scripts(document)?;
    if binding
        || ROOT_EVENTS
            .iter()
            .any(|event| scripts.is_some_and(|scripts| scripts.contains_key(*event)))
    {
        return Err(material_error(
            "root npm ci lifecycle hooks and implicit installs are not allowed",
        ));
    }
    Ok(())
}

fn declared_hooks(
    document: &Value,
    name: &str,
    version: &str,
    binding: bool,
) -> Result<Vec<DeclaredHook>, Report<ReleaseError>> {
    if document.get("name").and_then(Value::as_str) != Some(name)
        || document.get("version").and_then(Value::as_str) != Some(version)
    {
        return Err(material_error(
            "verified package manifest differs from locked package identity",
        ));
    }
    let scripts = scripts(document)?;
    if binding
        && !scripts.is_some_and(|scripts| {
            scripts.contains_key("preinstall") || scripts.contains_key("install")
        })
    {
        return Err(material_error(
            "implicit binding.gyp installs are not reviewed",
        ));
    }
    let mut result = Vec::new();
    for event in ["preinstall", "install", "postinstall"] {
        let Some(command) = scripts
            .and_then(|scripts| scripts.get(event))
            .and_then(Value::as_str)
        else {
            continue;
        };
        if !REVIEWED_HOOKS.contains(&(name, version, event, command)) {
            return Err(material_error(
                "dependency install hook has no exact source-owned review",
            ));
        }
        result.push(DeclaredHook {
            event: event.to_owned(),
            script_sha256: hex::encode(Sha256::digest(command.as_bytes())),
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn empty_development_script_does_not_authorize_a_lifecycle_hook() {
        let mut manifest =
            json!({"name":"fictional-package","version":"1.0.0","scripts":{"test":""}});
        assert!(
            declared_hooks(&manifest, "fictional-package", "1.0.0", false)
                .expect("should accept an unused empty development script")
                .is_empty(),
            "should declare no install execution from a development script"
        );
        assert!(
            review_root_document(&manifest, false).is_ok(),
            "should allow a root development script without lifecycle declarations"
        );
        manifest["scripts"]["postinstall"] = json!("");
        assert!(
            declared_hooks(&manifest, "fictional-package", "1.0.0", false).is_err(),
            "should still reject an unreviewed empty lifecycle declaration"
        );
        assert!(
            review_root_document(&manifest, false).is_err(),
            "should still reject root lifecycle declarations"
        );
    }

    #[test]
    fn root_review_allows_build_scripts_but_rejects_all_ci_hooks() {
        let mut manifest = json!({"name":"fictional-project", "scripts":{"build":"node build.mjs", "test":"vitest run"}});
        assert!(
            review_root_document(&manifest, false).is_ok(),
            "should allow ordinary explicit build and test scripts"
        );
        assert!(
            review_root_document(&manifest, true).is_err(),
            "should reject implicit root binding install"
        );
        for event in [
            "preinstall",
            "install",
            "postinstall",
            "prepublish",
            "preprepare",
            "prepare",
            "postprepare",
            "predependencies",
            "dependencies",
            "postdependencies",
        ] {
            manifest["scripts"][event] = json!("node fictional-hook.mjs");
            assert!(
                review_root_document(&manifest, false).is_err(),
                "should reject root lifecycle hook"
            );
            manifest["scripts"]
                .as_object_mut()
                .expect("should have scripts object")
                .remove(event);
        }
    }

    #[test]
    fn dependency_review_requires_exact_source_owned_hook_tuples() {
        let manifest = json!({"name":"fictional-package", "version":"1.0.0"});
        assert!(
            declared_hooks(&manifest, "fictional-package", "1.0.0", false)
                .expect("should accept package without hooks")
                .is_empty(),
            "should declare no hook execution"
        );
        assert!(
            declared_hooks(&manifest, "other-package", "1.0.0", false).is_err(),
            "should reject manifest identity substitution"
        );
        assert!(
            declared_hooks(&manifest, "fictional-package", "1.0.0", true).is_err(),
            "should reject implicit binding install"
        );
        for (name, version, command) in [
            (
                "core-js",
                "3.48.0",
                "node -e \"try{require('./postinstall')}catch(e){}\"",
            ),
            ("esbuild", "0.27.3", "node install.js"),
        ] {
            let mut manifest =
                json!({"name":name,"version":version,"scripts":{"postinstall":command}});
            assert_eq!(
                declared_hooks(&manifest, name, version, false)
                    .expect("should match exact reviewed declaration")
                    .len(),
                1,
                "should return one declaration"
            );
            manifest["scripts"]["postinstall"] = json!(format!("{command} "));
            assert!(
                declared_hooks(&manifest, name, version, false).is_err(),
                "should reject changed command bytes"
            );
            manifest["scripts"]["postinstall"] = json!(command);
            manifest["scripts"]["install"] = json!("node other.mjs");
            assert!(
                declared_hooks(&manifest, name, version, false).is_err(),
                "should reject added lifecycle command"
            );
        }
    }

    #[test]
    fn rejects_malformed_scripts_and_hashes_the_exact_declared_command() {
        for scripts in [
            Value::Null,
            json!([]),
            json!({"postinstall":42}),
            json!({"postinstall":""}),
        ] {
            let manifest =
                json!({"name":"fictional-package", "version":"1.0.0", "scripts":scripts});
            assert!(
                declared_hooks(&manifest, "fictional-package", "1.0.0", false).is_err(),
                "should reject malformed script declarations"
            );
        }
        let manifest = json!({"name":"esbuild", "version":"0.27.3", "scripts":{"postinstall":"node install.js"}});
        let declarations = declared_hooks(&manifest, "esbuild", "0.27.3", false)
            .expect("should match reviewed command");
        assert_eq!(
            declarations[0].event(),
            "postinstall",
            "should preserve declared event"
        );
        assert_eq!(
            declarations[0].script_sha256(),
            "912d4d8f507b7b392ae422a459f98da94669a742e4cc43cfe061e630ee2846fe",
            "should hash exact UTF-8 command bytes"
        );
        let mut changed = manifest;
        changed["version"] = json!("0.27.4");
        assert!(
            declared_hooks(&changed, "esbuild", "0.27.4", false).is_err(),
            "should reject unreviewed locked version"
        );
        let prepare = json!({"name":"fictional-package", "version":"1.0.0", "scripts":{"prepare":"node source-only.mjs"}});
        assert!(
            declared_hooks(&prepare, "fictional-package", "1.0.0", false)
                .expect("should parse registry manifest")
                .is_empty(),
            "should not infer registry prepare execution"
        );
        assert!(
            parse_json(
                br#"{"name":"fictional-package","name":"substitute","version":"1.0.0"}"#,
                Role::Metadata
            )
            .is_err(),
            "should reject duplicate manifest keys before selection"
        );
    }
}
