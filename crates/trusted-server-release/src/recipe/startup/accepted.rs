//! Owning native startup acceptance, without qualification or signing authority.

use std::collections::BTreeSet;
use std::path::Path;

use error_stack::Report;
use serde_json::{Value, json};

#[cfg(target_os = "macos")]
use super::native::NativeStartupLayout;
use super::native::NativeStartupObserved;
use super::{
    StartupClient, StartupCopy, StartupDistribution, StartupImage, StartupObserved, lifecycle,
};
use crate::composition::origin::Original;
use crate::documents::{Role as DocumentRole, canonical_json};
use crate::error::ReleaseError;
#[cfg(target_os = "macos")]
use crate::inspect::{MachoCpu, MachoPolicy, inspect_macho};
use crate::policy::ReleaseTarget;
use crate::recipe::{AssetId, RecipeCompleted, Role, recipe_error};
use crate::source::CommitId;

#[cfg(test)]
mod tests;

#[derive(Debug, PartialEq, Eq)]
struct Owner {
    source_commit: CommitId,
    role: Role,
    asset: AssetId,
    artifact_sha256: String,
    artifact_size: u64,
    package_version: String,
}

struct CliVersionInputs<'a> {
    name: &'a str,
    version: &'a str,
    origin_name: &'a str,
    origin_version: &'a str,
    original: &'a Original,
    commit: &'a CommitId,
}

fn checked_cli_version(inputs: &CliVersionInputs<'_>) -> Result<String, Report<ReleaseError>> {
    if inputs.name != "trusted-server-cli"
        || inputs.origin_name != inputs.name
        || inputs.origin_version != inputs.version
        || inputs.version.is_empty()
        || inputs.version.len() > 128
        || inputs
            .version
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(recipe_error(
            "startup CLI version is not the verified selected package version",
        ));
    }
    match inputs.original {
        Original::Workspace {
            source_commit,
            path,
        } if source_commit == inputs.commit.as_str()
            && path == Path::new("crates/trusted-server-cli/Cargo.toml") =>
        {
            Ok(inputs.version.to_owned())
        }
        _ => Err(recipe_error(
            "startup selected CLI has no matching approved workspace origin",
        )),
    }
}

impl Owner {
    fn from_completed(completed: &RecipeCompleted) -> Result<Self, Report<ReleaseError>> {
        completed.recipe_definition()?;
        if completed.asset().artifact().package != "trusted-server-cli"
            || completed.asset().artifact().target == ReleaseTarget::Wasm32Wasip1
        {
            return Err(recipe_error(
                "native startup requires an owning native CLI build",
            ));
        }
        let view = completed.composition_view()?;
        let origins = view.package_origins();
        if origins.source_commit() != completed.commit().as_str()
            || origins.materials_sha256() != completed.materials_digest()
        {
            return Err(recipe_error(
                "startup CLI origins differ from the completed owner",
            ));
        }
        let id = view.graph().selected_package_id();
        let mut packages = view
            .observations()
            .packages()
            .iter()
            .filter(|package| package.id() == id);
        let package = packages
            .next()
            .ok_or_else(|| recipe_error("startup has no selected CLI package"))?;
        if packages.next().is_some() {
            return Err(recipe_error("startup selected CLI package is ambiguous"));
        }
        let origin = origins
            .package(id)
            .ok_or_else(|| recipe_error("startup selected CLI has no verified origin"))?;
        let package_version = checked_cli_version(&CliVersionInputs {
            name: package.name(),
            version: package.version(),
            origin_name: origin.name(),
            origin_version: origin.version(),
            original: origin.origin(),
            commit: completed.commit(),
        })?;
        Ok(Self {
            source_commit: completed.commit().clone(),
            role: completed.role(),
            asset: completed.asset(),
            artifact_sha256: completed.artifact().sha256().to_owned(),
            artifact_size: completed.artifact().size(),
            package_version,
        })
    }

    fn expected_stdout(&self) -> String {
        format!(
            "ts package={} source={} target={} variant=default\n",
            self.package_version,
            self.source_commit.as_str(),
            self.asset.artifact().target.triple()
        )
    }

    fn check_same(&self, other: &Self) -> Result<(), Report<ReleaseError>> {
        if self != other {
            return Err(recipe_error(
                "native startup acceptance belongs to another source, role, asset, artifact or CLI version",
            ));
        }
        Ok(())
    }

    fn check_copy(&self, copy: &StartupCopy) -> Result<(), Report<ReleaseError>> {
        if copy.sha256 != self.artifact_sha256 || copy.size != self.artifact_size {
            return Err(recipe_error(
                "startup copy differs from the owning final bytes",
            ));
        }
        copy.verify()
    }
}

fn check_linux_images(images: &[StartupImage]) -> Result<(), Report<ReleaseError>> {
    if images.len() != StartupDistribution::ALL.len() {
        return Err(recipe_error(
            "Linux startup requires all three reviewed distributions",
        ));
    }
    let mut identities = BTreeSet::new();
    for (image, distribution) in images.iter().zip(StartupDistribution::ALL) {
        if image.distribution() != distribution || !identities.insert(image.id()) {
            return Err(recipe_error(
                "Linux startup selection repeats or substitutes a reviewed distribution",
            ));
        }
    }
    Ok(())
}

#[derive(Debug)]
enum Observed {
    Linux(Vec<StartupObserved>),
    Macos {
        startup: NativeStartupObserved,
        minimum_os: String,
        sdk: String,
    },
}

/// Successful actual native startup retained independently of its working copy.
///
/// Only actual owning startup methods construct this type. Public low-level
/// observations and serialized diagnostics cannot reconstruct acceptance.
#[derive(Debug)]
pub(crate) struct NativeStartupAccepted {
    owner: Owner,
    observed: Observed,
}

impl NativeStartupAccepted {
    /// Starts the owning Linux CLI bytes on all three reviewed clean selections.
    ///
    /// The supplied copy is consumed and removed on success or failure. Expected
    /// output comes from the sealed selected CLI package and approved source.
    ///
    /// # Errors
    ///
    /// Rejects mismatched owner/copy, incomplete or duplicate image selections,
    /// failed startup, changed bytes or failed cleanup. Local image IDs are not
    /// authenticated distribution provenance.
    pub(crate) fn accept_linux(
        completed: &RecipeCompleted,
        copy: StartupCopy,
        client: &StartupClient,
        images: &[StartupImage],
    ) -> Result<Self, Report<ReleaseError>> {
        let run = (|| {
            let owner = Owner::from_completed(completed)?;
            if owner.asset.artifact().target != ReleaseTarget::X86_64UnknownLinuxGnu {
                return Err(recipe_error("Linux startup requires the Linux CLI target"));
            }
            owner.check_copy(&copy)?;
            check_linux_images(images)?;
            let expected = owner.expected_stdout();
            let runs = images
                .iter()
                .map(|image| copy.start(client, image, &expected))
                .collect::<Result<Vec<_>, _>>()?;
            owner.check_copy(&copy)?;
            completed.rehash_for_handoff()?;
            Ok((owner, Observed::Linux(runs)))
        })();
        let (owner, observed) = lifecycle::combine(run, copy.remove())?;
        completed.rehash_for_handoff()?;
        Ok(Self { owner, observed })
    }

    /// Starts the owning Darwin CLI on its actual native macOS host.
    ///
    /// The same measured host that passes the execution gate is retained. The
    /// accepted Mach-O CPU, minimum OS and SDK come from the owning final bytes.
    ///
    /// # Errors
    ///
    /// Rejects another owner/target/copy, nonnative host, incompatible minimum OS,
    /// failed startup, changed bytes or failed cleanup.
    #[cfg(target_os = "macos")]
    pub(crate) fn accept_macos(
        completed: &RecipeCompleted,
        copy: StartupCopy,
        layout: &NativeStartupLayout<'_>,
    ) -> Result<Self, Report<ReleaseError>> {
        let run = (|| {
            let owner = Owner::from_completed(completed)?;
            let target = owner.asset.artifact().target;
            let cpu = match target {
                ReleaseTarget::Aarch64AppleDarwin => MachoCpu::Arm64,
                ReleaseTarget::X86_64AppleDarwin => MachoCpu::X86_64,
                _ => {
                    return Err(recipe_error(
                        "native macOS startup requires a Darwin CLI target",
                    ));
                }
            };
            owner.check_copy(&copy)?;
            let bytes = crate::recipe::compare::read_artifact(completed)?;
            let facts = inspect_macho(&bytes, &MachoPolicy::macos(cpu))?;
            let view = completed.composition_view()?;
            let requirements = view.platform().external_requirements();
            if requirements.target() != target
                || requirements.artifact_sha256() != owner.artifact_sha256
                || requirements.macho_sdk() != Some(facts.sdk().to_string().as_str())
            {
                return Err(recipe_error(
                    "native startup Mach-O facts differ from the sealed platform requirements",
                ));
            }
            let startup =
                copy.start_native(layout, target, &owner.expected_stdout(), facts.minos())?;
            owner.check_copy(&copy)?;
            completed.rehash_for_handoff()?;
            Ok((
                owner,
                Observed::Macos {
                    startup,
                    minimum_os: facts.minos().to_string(),
                    sdk: facts.sdk().to_string(),
                },
            ))
        })();
        let (owner, observed) = lifecycle::combine(run, copy.remove())?;
        completed.rehash_for_handoff()?;
        Ok(Self { owner, observed })
    }

    /// Borrows actual Linux run observations when this acceptance is Linux.
    pub(crate) fn linux_runs(&self) -> Option<&[StartupObserved]> {
        match &self.observed {
            Observed::Linux(runs) => Some(runs),
            _ => None,
        }
    }
    /// Borrows actual native macOS stdout when this acceptance is Darwin.
    pub(crate) fn macos_stdout(&self) -> Option<&str> {
        match &self.observed {
            Observed::Macos { startup, .. } => Some(&startup.stdout),
            _ => None,
        }
    }

    /// Exports successful startup facts for this exact completed build.
    ///
    /// # Errors
    ///
    /// Rejects changed retained outputs or another source, role, asset, artifact
    /// or selected CLI version. JSON provides diagnostics without authority.
    pub(crate) fn diagnostic_for(
        &self,
        completed: &RecipeCompleted,
    ) -> Result<Value, Report<ReleaseError>> {
        let actual = Owner::from_completed(completed)?;
        self.owner.check_same(&actual)?;
        let record = self.diagnostic_record()?;
        completed.rehash_for_handoff()?;
        Ok(record)
    }

    fn diagnostic_record(&self) -> Result<Value, Report<ReleaseError>> {
        let observation = match &self.observed {
            Observed::Linux(runs) => {
                json!({"kind":"linux-clean-containers", "runs":runs.iter().map(|run|Ok(json!({"distribution":run.image.distribution().label(),"reference":run.image.distribution().reference(),"image_id":run.image.id(),"exit_code":0,"stdout":run.stdout,"stderr_sha256":run.stderr_sha256,"elapsed_milliseconds":milliseconds(run.elapsed)?}))).collect::<Result<Vec<Value>,Report<ReleaseError>>>()?,"environment_scope":"selected_image_defaults_no_host_environment_injection","image_authentication":"unqualified-local-image-selection"})
            }
            Observed::Macos {
                startup,
                minimum_os,
                sdk,
            } => {
                json!({"kind":"native-macos", "host":startup.host,"minimum_os":minimum_os,"sdk":sdk,"exit_code":0,"stdout":startup.stdout,"stderr_sha256":startup.stderr_sha256,"elapsed_milliseconds":milliseconds(startup.elapsed)?,"environment":{"PATH":"/usr/bin:/bin","LC_ALL":"C","HOME":"private","TMPDIR":"private"}})
            }
        };
        let record = json!({"domain":"trusted-server.release.native-startup-diagnostic","schema_version":1,"release_qualification":false,"source_commit":self.owner.source_commit.as_str(),"role":self.owner.role.spec_label(),"asset":self.owner.asset.artifact().name,"target":self.owner.asset.artifact().target.triple(),"package_version":self.owner.package_version,"artifact":{"sha256":self.owner.artifact_sha256,"size":self.owner.artifact_size},"startup":observation});
        canonical_json(&record, DocumentRole::Metadata)
            .map_err(|error| error.change_context(ReleaseError::Recipe))?;
        Ok(record)
    }
}

fn milliseconds(duration: std::time::Duration) -> Result<u64, Report<ReleaseError>> {
    u64::try_from(duration.as_millis()).map_err(|error| {
        recipe_error(format!(
            "startup duration exceeds diagnostic bound: {error}"
        ))
    })
}
