//! Helpers shared by the explicit real ordinary CI fixtures on both the
//! Linux container and the macOS sandbox hosts; never selected by a normal
//! focused unit run.
//!
//! Every run that uses them is ordinary CI candidate evidence
//! (`release_qualification: false`): the toolchain and fixture acquisition
//! are unqualified and unauthenticated, so nothing here qualifies a release.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::candidate::CargoFixture;
use super::cli_candidate::OPERATOR_SENTINELS;
use super::*;
use crate::documents::{parse_json, read_bounded};
use crate::materials::{GitVendorInput, LockedMaterials, VendorInputs};
use crate::preflight::child::build_child_for_tests;
use crate::preflight::{AuditedEnvironment, audit_environment};
use crate::process::Cancellation;

/// The committed denial fixture workspace.
const DENIAL_FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cargo-denial");
/// Fictional committer epoch of the denial fixture child environment.
const DENIAL_EPOCH: u64 = 1_700_000_000;
/// Members that attempt a connection while Cargo builds them.
pub(super) const ATTEMPTS: [&str; 3] = ["build-direct", "build-grandchild", "macro-attempt"];

/// Reads the ordinary CI Cargo fixture named by `TS_RELEASE_CARGO_FIXTURE_ROOT`.
pub(super) fn cargo_fixture() -> CargoFixture {
    let root = std::env::var_os("TS_RELEASE_CARGO_FIXTURE_ROOT")
        .map(PathBuf::from)
        .expect("should explicitly supply the ordinary CI Cargo fixture root");
    let root = fs::canonicalize(root).expect("should resolve the Cargo fixture root");
    let raw = read_bounded(&root.join("fixture.json"), DocumentRole::Metadata)
        .expect("should read the Cargo fixture description");
    let document =
        parse_json(&raw.bytes, DocumentRole::Metadata).expect("should parse the description");
    CargoFixture::parse(&root, &document).expect("should accept the Cargo fixture description")
}

/// Audits the minimal source environment.
pub(super) fn minimal_environment() -> AuditedEnvironment {
    audit_environment(&[(OsString::from("PATH"), OsString::from("/usr/bin:/bin"))])
        .expect("should audit the minimal environment")
}

/// Audits an inherited environment carrying the planted operator values and
/// requires preflight to remove every one of them.
pub(super) fn environment_with_operator_values() -> (AuditedEnvironment, Vec<String>) {
    let mut snapshot = vec![(OsString::from("PATH"), OsString::from("/usr/bin:/bin"))];
    snapshot.extend(
        OPERATOR_SENTINELS
            .iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value))),
    );
    let audited = audit_environment(&snapshot).expect("should remove, not reject, operator values");
    let removed = audited.removed_names().to_vec();
    assert_eq!(
        removed.len(),
        OPERATOR_SENTINELS.len(),
        "should remove every planted operator variable"
    );
    (audited, removed)
}

/// Conditions of an ordinary CI build of `role`.
///
/// The umask and jobs are the profile values the plan enforces on the child.
/// The reproduction build uses the test-only alternate profile: its host locale and timezone
/// are profile labels, while the child runs with the normalized `C` locale
/// and `UTC` timezone either way. This is not an independent qualified
/// observation, and the production reproduction build stays unqualified.
pub(super) fn ordinary_ci_ambient(role: Role, checkout: &Path) -> CheckedAmbient {
    try_ordinary_ci_ambient(role, checkout).expect("should match the role profile")
}

pub(super) fn try_ordinary_ci_ambient(
    role: Role,
    checkout: &Path,
) -> Result<CheckedAmbient, Report<ReleaseError>> {
    let (profile, base) = match role {
        Role::Canonical => (AmbientProfile::CANONICAL, observed_canonical()),
        Role::Reproduction => (
            AmbientProfile::alternate_for_tests(),
            observed_reproduction(),
        ),
    };
    // Reading the real user ID has no preconditions.
    let execution_uid = unsafe { libc::getuid() };
    let started_at_seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| recipe_error(format!("cannot read ambient clock: {error}")))?
        .as_secs();
    CheckedAmbient::check(
        role,
        &profile,
        ObservedAmbient {
            execution_uid,
            checkout_root: checkout.to_owned(),
            started_at_seconds,
            ..base
        },
    )
}

/// Copies a tree of directories and regular files into a new directory.
fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir(destination).expect("should create the copied directory");
    for entry in fs::read_dir(source).expect("should list the fixture directory") {
        let entry = entry.expect("should read the fixture entry");
        let kind = entry.file_type().expect("should inspect the fixture entry");
        let target = destination.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target);
        } else if kind.is_file() {
            fs::copy(entry.path(), &target).expect("should copy the fixture file");
        } else {
            panic!("should hold only directories and regular files");
        }
    }
}

/// One bound denial fixture member with the roots it owns.
pub(super) struct DenialCase {
    _work: tempfile::TempDir,
    _completed: JsBuildCompleted,
    pub(super) prepared: PreparedRecipe,
}

/// What one host builds a denial fixture member as.
pub(super) struct DenialSelection {
    /// The test-only fixture row of the member for the host's target.
    pub(super) asset: AssetId,
    /// The ordinary CI Rust tools, with Apple developer tools on macOS.
    pub(super) tools: SelectedRustTools,
    /// A fixed target directory, or `None` for one inside the case roots.
    pub(super) target_dir: Option<PathBuf>,
}

/// Binds one denial fixture member over fresh private roots.
pub(super) fn bind_denial_case(
    fixture: &CargoFixture,
    member: &str,
    selection: DenialSelection,
) -> DenialCase {
    let work = tempfile::tempdir().expect("should create the case roots");
    let base = fs::canonicalize(work.path()).expect("should resolve the case roots");
    let checkout = base.join("checkout");
    copy_tree(Path::new(DENIAL_FIXTURE), &checkout);
    for directory in ["materials/vendor", "cargo-state", "output"] {
        fs::create_dir_all(base.join(directory)).expect("should create a case root");
    }
    let completed = JsBuildCompleted::fixture_for_tests(&fixture.source_commit);
    let retained = fs::canonicalize(
        completed
            .rehash_for_handoff()
            .expect("should rehash the fixture handoff"),
    )
    .expect("should resolve the retained root");
    let target_dir = selection.target_dir.unwrap_or_else(|| base.join("target"));
    let request = RecipeRequest::parse_for_asset(
        selection.asset,
        &RecipeInput {
            asset: member,
            variant: "default",
            role: Role::Canonical.spec_label(),
            commit: &fixture.source_commit,
            js_dir: &retained.join("bundles"),
            js_inventory: &retained.join("tsjs-inventory.json"),
            materials: &base.join("materials"),
            cargo_state: &base.join("cargo-state"),
            output: &base.join("output"),
            target_dir: &target_dir,
        },
    )
    .expect("should accept the case request");
    let commit = CommitId::parse(&fixture.source_commit).expect("should parse the commit");
    let child = build_child_for_tests(
        &minimal_environment(),
        &commit,
        DENIAL_EPOCH,
        &child_request(&request),
    )
    .expect("should build the child environment");
    let prepared = PreparedRecipe::bind(BindInputs {
        workspace: WorkspaceSelection::Fixture,
        commit: &commit,
        child,
        completed: &completed,
        tools: selection.tools,
        materials: PreparedCargoMaterials::for_tests(&base.join("materials"), commit.as_str(), &[])
            .expect("should describe the empty vendor directory"),
        ambient: ordinary_ci_ambient(Role::Canonical, &checkout),
        request,
    })
    .expect("should bind the denial case");
    DenialCase {
        _work: work,
        _completed: completed,
        prepared,
    }
}

/// Prepares the vendor tree from the fixture's retained originals.
///
/// The fixture's `cargo vendor` manifests only name which locked Git packages
/// to supply closures for; the producer normalizes every inheriting manifest
/// itself from the verified tree.
pub(super) fn try_prepare_vendor(
    fixture: &CargoFixture,
    locked: &LockedMaterials,
    root: &Path,
    deadline: Instant,
    cancellation: &Cancellation,
) -> Result<PreparedCargoMaterials, Report<ReleaseError>> {
    let collection = locked.git_collection(Path::new("/usr/bin/git"), deadline, cancellation)?;
    let verified: Vec<_> = fixture
        .git
        .iter()
        .map(|source| collection.verify(&source.selector, &source.revision, &source.objects))
        .collect::<Result<_, _>>()?;
    let inputs: Vec<GitVendorInput<'_>> = fixture
        .comparison_manifests
        .iter()
        .map(|package| {
            let index = fixture
                .git
                .iter()
                .position(|source| source.selector == package.selector)
                .ok_or_else(|| recipe_error("comparison manifest names no retained Git source"))?;
            Ok(GitVendorInput {
                material: &verified[index],
                name: &package.name,
                version: &package.version,
            })
        })
        .collect::<Result<_, Report<ReleaseError>>>()?;
    locked.prepare_cargo_vendor(
        &VendorInputs {
            registry_archives: &fixture.registry,
            git: &inputs,
        },
        root,
        deadline,
        cancellation,
    )
}

pub(super) fn path_text(path: &Path) -> String {
    path.to_str().expect("should be a UTF-8 path").to_owned()
}
