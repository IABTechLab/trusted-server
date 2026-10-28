//! Explicit ordinary command assembly, without authenticated or qualified authority.

use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use error_stack::Report;
use serde_json::{Value, json};

use super::{diagnostic, inputs, normalization, output, require, root, run, transfer};
use crate::documents::{self, Role};
use crate::error::ReleaseError;

const ARCHIVES: [&str; 4] = [
    "canonical.zip",
    "canonical-observation.zip",
    "reproduction.zip",
    "reproduction-observation.zip",
];

/// Checks fresh runner layout and seals the exact Cargo arguments before build.
///
/// # Errors
///
/// Rejects reused/aliased paths, competing flags or a failed private atomic write.
pub(crate) fn prepare_rustflags(
    checkout: &Path,
    target: &Path,
    role: &str,
    output: &Path,
    inherited: &[(OsString, OsString)],
) -> Result<String, Report<ReleaseError>> {
    let flags = normalization::prepare(checkout, target, role, inherited)?;
    output::rustflags(output, &flags.arguments)?;
    Ok(flags.arguments[1].clone())
}

fn directory(path: &Path) -> Result<(), Report<ReleaseError>> {
    require(
        path.to_str().is_some_and(root)
            && std::fs::canonicalize(path).is_ok_and(|resolved| resolved == path)
            && std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir()),
        "ordinary command input root must be a canonical directory",
    )
}

fn bind(input: &Path) -> Result<run::Bindings, Report<ReleaseError>> {
    transfer_root(input)?;
    let context = transfer_read(&input.join("context.json"), Role::Metadata)?;
    let jobs = transfer_read(&input.join("jobs.json"), Role::Metadata)?;
    let artifacts = transfer_read(&input.join("artifacts.json"), Role::Metadata)?;
    run::bind(&context.bytes, &jobs.bytes, &artifacts.bytes)
        .map_err(|report| diagnostic::tag(report, diagnostic::Code::IdentityMismatch))
}

fn available(path: &Path) -> Result<(), Report<ReleaseError>> {
    std::fs::symlink_metadata(path)
        .map(|_| ())
        .map_err(|error| {
            let code = if error.kind() == ErrorKind::NotFound {
                diagnostic::Code::TransferUnavailable
            } else {
                diagnostic::Code::TransferRejected
            };
            Report::new(ReleaseError::Document)
                .attach(error)
                .attach(diagnostic::Failure::new(code))
        })
}

fn transfer_root(path: &Path) -> Result<(), Report<ReleaseError>> {
    available(path)?;
    directory(path).map_err(|report| diagnostic::tag(report, diagnostic::Code::TransferRejected))
}

fn transfer_read(path: &Path, role: Role) -> Result<documents::RawBytes, Report<ReleaseError>> {
    available(path)?;
    documents::read_bounded(path, role)
        .map_err(|report| diagnostic::tag(report, diagnostic::Code::TransferRejected))
}

/// Plans exact-ID downloads only after joining direct outputs and service records.
///
/// # Errors
///
/// Rejects aliased roots, unsafe/oversized metadata and any invalid current-run
/// identity. The caller must obtain service bytes over authenticated transport.
pub(crate) fn transfer_plan(input: &Path) -> Result<Value, Report<ReleaseError>> {
    let bindings = bind(input)?;
    let [canonical, reproduction] = bindings.builds();
    let archives: Vec<_> = [
        &canonical.artifact,
        &canonical.observation,
        &reproduction.artifact,
        &reproduction.observation,
    ]
    .into_iter()
    .zip(ARCHIVES)
    .map(|(archive, file)| {
        json!({"id":archive.id,"size":archive.size,"sha256":archive.sha256,"file":file})
    })
    .collect();
    Ok(json!({"version":1,"archives":archives}))
}

/// Compares retained transfer archives and writes only an actual matched companion.
///
/// # Errors
///
/// Rejects invalid bindings, unsafe/changed archives, any byte/input mismatch or
/// an output that cannot be freshly and atomically sealed. No failed receipt is published.
pub(crate) fn compare_transfers(
    input: &Path,
    archives: &Path,
    source: &Path,
    output: &Path,
) -> Result<PathBuf, Report<ReleaseError>> {
    let mut artifact_ids = None;
    let result = compare_retained(input, archives, source, output, &mut artifact_ids);
    result.map_err(|report| {
        let report = diagnostic::tag(report, diagnostic::Code::TransferUnavailable);
        if diagnostic::retain(&report, output, artifact_ids).is_err() {
            return report.attach(
                "outcome=failed: ordinary diagnostic writer failed; no successful comparison",
            );
        }
        report
    })
}

fn compare_retained(
    input: &Path,
    archives: &Path,
    source: &Path,
    output: &Path,
    artifact_ids: &mut Option<[u64; 4]>,
) -> Result<PathBuf, Report<ReleaseError>> {
    let bindings = bind(input)?;
    let [canonical, reproduction] = bindings.builds();
    *artifact_ids = Some([
        canonical.artifact.id,
        canonical.observation.id,
        reproduction.artifact.id,
        reproduction.observation.id,
    ]);
    transfer_root(archives)?;
    let retained = ARCHIVES
        .into_iter()
        .enumerate()
        .map(|(index, name)| {
            let role = if index % 2 == 0 {
                Role::ArtifactArchive
            } else {
                Role::ObservationArchive
            };
            transfer_read(&archives.join(name), role)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let checked = transfer::check(
        bindings,
        &transfer::Archives([
            &retained[0].bytes,
            &retained[1].bytes,
            &retained[2].bytes,
            &retained[3].bytes,
        ]),
    )
    .map_err(|report| diagnostic::tag(report, diagnostic::Code::InputsRejected))?;
    let receipt = checked.receipt();
    let source_commit = checked_source(&receipt).map_err(|report| {
        diagnostic::tag_equal(report, diagnostic::Code::InputsRejected, &checked)
    })?;
    let identity = inputs::source_identity(source, &source_commit).map_err(|report| {
        diagnostic::tag_equal(report, diagnostic::Code::InputsRejected, &checked)
    })?;
    require(
        receipt["recipe_sha256"] == identity.recipe_sha256
            && receipt["cargo_lock_sha256"] == identity.cargo_lock_sha256
            && receipt["npm_lock_sha256"] == identity.npm_lock_sha256,
        "ordinary observations disagree with the independent comparison source",
    )
    .map_err(|report| diagnostic::tag_equal(report, diagnostic::Code::InputsRejected, &checked))?;
    inputs::verify_versions(source, &receipt["tools"]).map_err(|report| {
        diagnostic::tag_equal(report, diagnostic::Code::InputsRejected, &checked)
    })?;
    output::companion(&checked, output)
        .map_err(|report| diagnostic::tag_equal(report, diagnostic::Code::OutputRejected, &checked))
}

fn checked_source(receipt: &Value) -> Result<String, Report<ReleaseError>> {
    receipt["source_commit"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| {
            Report::new(ReleaseError::Document).attach("ordinary receipt source is missing")
        })
}

/// Measures one source-pinned ordinary build without granting recipe qualification.
///
/// # Errors
///
/// Rejects invalid captures, failed bounded version queries, changed input bytes
/// or unsafe output paths. Only the fixed Rust/Node version queries execute.
pub(crate) fn measure_build(
    request: &Path,
    output: &Path,
) -> Result<PathBuf, Report<ReleaseError>> {
    require(
        request.to_str().is_some_and(root)
            && std::fs::canonicalize(request).is_ok_and(|resolved| resolved == request),
        "ordinary measurement request must use its canonical path",
    )?;
    let request = documents::read_bounded(request, Role::Metadata)?;
    let observed = inputs::measure(&request.bytes)?;
    output::observation(&observed, output)
}

#[cfg(test)]
mod tests;
