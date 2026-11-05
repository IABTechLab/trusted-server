//! Ordinary CI candidate support: the Cargo fixture description, the explicit
//! container settings and the measurements the real Linux fixtures print.
//!
//! Nothing here is release qualification. The fixture is acquired without
//! authentication, the toolchain and image are unqualified, and every
//! measurement says so.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value, json};
use sha2::Sha256;

use super::super::ambient::Dimension;
use super::super::compare::FastlyEqualityChecked;
use super::*;
use crate::boundary::DockerSettings;
use crate::process::ProcessLimits;

/// Schema version of `fixture.json` and of every measurement.
pub(super) const SCHEMA_VERSION: u64 = 1;
/// Kind of the Linux Cargo fixture description.
const FIXTURE_KIND: &str = "ordinary-ci-cargo-fixture";
/// Kind of the macOS Cargo fixture description, which names no image.
const MACOS_FIXTURE_KIND: &str = "ordinary-ci-macos-cargo-fixture";

/// Which host a Cargo fixture was prepared for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FixtureHost {
    /// The Linux container job; the fixture names a local builder image ID.
    LinuxContainer,
    /// A macOS sandbox job; the fixture names no image (`image_id: null`).
    MacosSandbox,
}
/// How the fixture inputs were acquired.
const ACQUISITION: &str = "unauthenticated-ordinary-ci";

/// Every field `fixture.json` must hold, and nothing else.
const FIXTURE_FIELDS: [&str; 10] = [
    "acquisition",
    "cargo_lock_sha256",
    "comparison_manifests",
    "git",
    "image_id",
    "kind",
    "registry",
    "rustc_version",
    "schema_version",
    "source_commit",
];

/// Container process limit of an ordinary CI Cargo build.
const CARGO_PIDS_LIMIT: u32 = 4096;
/// Container memory of an ordinary CI Cargo build.
const CARGO_MEMORY_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Most CPUs an ordinary CI Cargo container receives (a 4-vCPU runner).
const CARGO_MAX_CPUS: u32 = 4;
/// Private `/tmp` of the Cargo container; Cargo's own `TMPDIR` is a root.
const CARGO_TMPFS_BYTES: u64 = 512 * 1024 * 1024;

/// One retained Git object directory for a locked Git source.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct GitObjects {
    pub(super) selector: String,
    pub(super) revision: String,
    pub(super) objects: PathBuf,
}

/// The manifest `cargo vendor` wrote for one locked Git package.
///
/// It is a differential comparison fixture only: the producer never vendors
/// it, and it names which locked Git packages to supply closures for.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ComparisonManifest {
    pub(super) name: String,
    pub(super) version: String,
    pub(super) selector: String,
    pub(super) cargo_vendor_manifest: PathBuf,
}

/// The parsed, unauthenticated ordinary CI Cargo fixture.
#[derive(Debug)]
pub(super) struct CargoFixture {
    pub(super) root: PathBuf,
    pub(super) source_commit: String,
    pub(super) cargo_lock_sha256: String,
    pub(super) host: FixtureHost,
    pub(super) image_id: Option<String>,
    pub(super) registry: BTreeMap<(String, String), PathBuf>,
    pub(super) git: Vec<GitObjects>,
    pub(super) comparison_manifests: Vec<ComparisonManifest>,
}

fn fixture_error(detail: impl std::fmt::Display) -> Report<ReleaseError> {
    recipe_error(format!("cargo fixture: {detail}"))
}

fn lower_hex(text: &str, length: usize) -> bool {
    text.len() == length
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Returns the string field `name` of `value`.
fn text<'a>(value: &'a Value, name: &str) -> Result<&'a str, Report<ReleaseError>> {
    value[name]
        .as_str()
        .filter(|text| !text.is_empty())
        .ok_or_else(|| fixture_error(format!("{name} is not a non-empty string")))
}

/// Joins a relative fixture path made only of normal components.
fn below(root: &Path, value: &Value, name: &str) -> Result<PathBuf, Report<ReleaseError>> {
    let relative = Path::new(text(value, name)?);
    let normal = relative
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
        && relative.components().next().is_some();
    if !normal {
        return Err(fixture_error(format!(
            "{name} must be a relative path of normal components"
        )));
    }
    Ok(root.join(relative))
}

/// Returns the array field `name` of `value`.
fn entries<'a>(value: &'a Value, name: &str) -> Result<&'a [Value], Report<ReleaseError>> {
    value[name]
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| fixture_error(format!("{name} is not an array")))
}

impl CargoFixture {
    /// Parses `fixture.json` of the fixture at `root`.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] for another schema, kind or
    /// acquisition, a missing or extra field, a malformed commit or digest,
    /// a Linux fixture without a local image ID or a macOS fixture with any
    /// image ID, a path that is absolute or dotted, a repeated registry
    /// identity, no Git source, or a comparison manifest whose selector names
    /// no retained Git source.
    pub(super) fn parse(root: &Path, document: &Value) -> Result<Self, Report<ReleaseError>> {
        let fields: BTreeSet<&str> = document
            .as_object()
            .ok_or_else(|| fixture_error("description is not an object"))?
            .keys()
            .map(String::as_str)
            .collect();
        if fields != BTreeSet::from(FIXTURE_FIELDS) {
            return Err(fixture_error("description fields differ from the schema"));
        }
        let host = match text(document, "kind")? {
            FIXTURE_KIND => FixtureHost::LinuxContainer,
            MACOS_FIXTURE_KIND => FixtureHost::MacosSandbox,
            _ => return Err(fixture_error("unsupported schema, kind or acquisition")),
        };
        if document["schema_version"].as_u64() != Some(SCHEMA_VERSION)
            || text(document, "acquisition")? != ACQUISITION
        {
            return Err(fixture_error("unsupported schema, kind or acquisition"));
        }
        let source_commit = text(document, "source_commit")?.to_owned();
        let cargo_lock_sha256 = text(document, "cargo_lock_sha256")?.to_owned();
        let image_id = match (host, &document["image_id"]) {
            (FixtureHost::LinuxContainer, Value::String(id))
                if id
                    .strip_prefix("sha256:")
                    .is_some_and(|digest| lower_hex(digest, 64)) =>
            {
                Some(id.clone())
            }
            (FixtureHost::MacosSandbox, Value::Null) => None,
            _ => {
                return Err(fixture_error(
                    "a Linux fixture needs a local image ID and a macOS fixture none",
                ));
            }
        };
        if !lower_hex(&source_commit, 40) || !lower_hex(&cargo_lock_sha256, 64) {
            return Err(fixture_error("malformed commit or lock digest"));
        }
        let git = entries(document, "git")?
            .iter()
            .map(|entry| {
                Ok(GitObjects {
                    selector: text(entry, "selector")?.to_owned(),
                    revision: text(entry, "revision")?.to_owned(),
                    objects: below(root, entry, "objects")?,
                })
            })
            .collect::<Result<Vec<_>, Report<ReleaseError>>>()?;
        if git.is_empty() || git.iter().any(|entry| !lower_hex(&entry.revision, 40)) {
            return Err(fixture_error("needs Git sources with full revisions"));
        }
        Ok(Self {
            registry: registry(root, document)?,
            comparison_manifests: comparison_manifests(root, document, &git)?,
            root: root.to_owned(),
            source_commit,
            cargo_lock_sha256,
            host,
            image_id,
            git,
        })
    }

    /// The local builder image ID of a Linux container fixture.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] for a macOS fixture, which names none.
    pub(super) fn container_image_id(&self) -> Result<&str, Report<ReleaseError>> {
        self.image_id
            .as_deref()
            .ok_or_else(|| fixture_error("a macOS fixture names no container image"))
    }

    /// The private single-link Rust tool root.
    pub(super) fn tools(&self) -> PathBuf {
        self.root.join("tools")
    }

    /// The staged producer probe inside the tool root.
    pub(super) fn probe(&self) -> PathBuf {
        self.root.join("tools/probe")
    }
}

/// Parses the registry originals, keyed by exact locked identity.
fn registry(
    root: &Path,
    document: &Value,
) -> Result<BTreeMap<(String, String), PathBuf>, Report<ReleaseError>> {
    let mut registry = BTreeMap::new();
    for entry in entries(document, "registry")? {
        let key = (
            text(entry, "name")?.to_owned(),
            text(entry, "version")?.to_owned(),
        );
        if registry
            .insert(key.clone(), below(root, entry, "path")?)
            .is_some()
        {
            return Err(fixture_error(format!(
                "registry repeats {} {}",
                key.0, key.1
            )));
        }
    }
    Ok(registry)
}

/// Parses the `cargo vendor` comparison manifests; each must name a retained
/// Git source.
fn comparison_manifests(
    root: &Path,
    document: &Value,
    git: &[GitObjects],
) -> Result<Vec<ComparisonManifest>, Report<ReleaseError>> {
    entries(document, "comparison_manifests")?
        .iter()
        .map(|entry| {
            let pair = ComparisonManifest {
                name: text(entry, "name")?.to_owned(),
                version: text(entry, "version")?.to_owned(),
                selector: text(entry, "selector")?.to_owned(),
                cargo_vendor_manifest: below(root, entry, "cargo_vendor_manifest")?,
            };
            if !git.iter().any(|source| source.selector == pair.selector) {
                return Err(fixture_error(format!(
                    "comparison manifest {} names no retained Git source",
                    pair.name
                )));
            }
            Ok(pair)
        })
        .collect()
}

/// Explicit container settings of an ordinary CI Cargo build.
///
/// These are fixture settings for the ordinary CI runner, not production
/// pins: a qualified builder environment selects its own.
pub(super) fn cargo_docker_settings(user: (u32, u32), parallelism: u32) -> DockerSettings {
    DockerSettings {
        client: PathBuf::from("/usr/bin/docker"),
        cpus: parallelism.clamp(1, CARGO_MAX_CPUS),
        memory_bytes: CARGO_MEMORY_BYTES,
        pids_limit: CARGO_PIDS_LIMIT,
        tmpfs_bytes: CARGO_TMPFS_BYTES,
        user,
        stop_timeout_seconds: 10,
        host_limits: ProcessLimits {
            wall: Duration::from_secs(300),
            stdout_max: 1024 * 1024,
            stderr_max: 1024 * 1024,
        },
    }
}

fn role_text(role: Role) -> &'static str {
    match role {
        Role::Canonical => "canonical",
        Role::Reproduction => "reproduction",
    }
}

/// The ordinary CI measurement of one confined Cargo build.
pub(super) fn cargo_measurement(completed: &RecipeCompleted) -> Value {
    let artifact = completed.artifact();
    let [js_inventory, js_settings, js_tools] = completed.js_digests();
    json!({
        "schema_version": SCHEMA_VERSION,
        "kind": "ordinary-ci-confined-cargo-build",
        "asset": completed.asset().artifact().name,
        "role": role_text(completed.role()),
        "source_commit": completed.commit().as_str(),
        "cargo_lock_sha256": completed.cargo_lock_sha256(),
        "artifact": {
            "name": artifact.name(),
            "sha256": artifact.sha256(),
            "size": artifact.size(),
        },
        "rust_tools_digest": completed.rust_tools_digest(),
        "materials_digest": completed.materials_digest(),
        "plan_digest": completed.plan_digest(),
        "js_inventory_sha256": js_inventory,
        "js_settings_sha256": js_settings,
        "js_tools_sha256": js_tools,
        "boundary_fingerprint": hex::encode(completed.boundary_fingerprint()),
        "descendant_cleanup": completed.descendant_cleanup().to_string(),
        "stdout_sha256": completed.stdout_sha256(),
        "stderr_sha256": completed.stderr_sha256(),
        "cargo_observations": completed.cargo_observations_summary(),
        "elapsed_seconds": completed.elapsed().as_secs(),
        "normalized_manifest_count": completed.normalized_manifest_digests().len(),
        "normalized_manifest_digests": completed.normalized_manifest_digests(),
        "derived_manifest_digests": completed.derived_manifest_digests().len(),
        "release_qualification": false,
    })
}

/// The ambient conditions one build ran under, as the measurement states them.
///
/// Umask and jobs are the profile values the plan enforced on the child, and
/// the host locale and timezone are profile labels: the child always ran
/// with the normalized `C` locale and `UTC` timezone.
fn ambient_measurement(completed: &RecipeCompleted) -> Value {
    let ambient = completed.ambient();
    let observed = ambient.observed();
    json!({
        "role": role_text(ambient.role()),
        "umask": format!("{:03o}", observed.umask),
        "jobs": observed.jobs,
        "checkout_root": observed.checkout_root.to_string_lossy(),
        "ambient_started_at_seconds": observed.started_at_seconds,
        "run_started_at_seconds": completed.started_at_seconds(),
        "execution_uid": observed.execution_uid,
        "host_locale_label": observed.host_locale,
        "host_timezone_label": observed.host_timezone,
        "child_locale": observed.child_locale,
        "child_timezone": observed.child_timezone,
        "observation": "profile-enforced-not-independently-observed",
    })
}

/// The ordinary CI measurement of the Fastly canonical/reproduction final-byte
/// comparison.
pub(super) fn equality_measurement(
    canonical: &RecipeCompleted,
    reproduction: &RecipeCompleted,
    checked: &FastlyEqualityChecked,
) -> Value {
    let claim = checked.ambient().claim();
    let names = |set: &BTreeSet<Dimension>| -> Vec<String> {
        set.iter().map(ToString::to_string).collect()
    };
    let build = |completed: &RecipeCompleted| {
        let mut value = cargo_measurement(completed);
        value["ambient"] = ambient_measurement(completed);
        value
    };
    json!({
        "schema_version": SCHEMA_VERSION,
        "kind": "ordinary-ci-fastly-canonical-reproduction-equality",
        "asset": checked.asset().artifact().name,
        "source_commit": canonical.commit().as_str(),
        "equality": true,
        "artifact": {
            "sha256": checked.sha256(),
            "size": checked.size(),
        },
        "builds": {
            "canonical": build(canonical),
            "reproduction": build(reproduction),
        },
        "ambient_claim": {
            "varied": names(&claim.varied),
            "normalized": names(&claim.normalized),
            "unsupported": names(&claim.unsupported),
        },
        "release_qualification": false,
    })
}

/// Names the dependency build cache the CI job restored, or `miss`.
const CI_BUILD_CACHE_VARIABLE: &str = "TS_RELEASE_CI_BUILD_CACHE";
/// What the provenance records when the job reported no cache outcome.
const UNREPORTED_BUILD_CACHE: &str = "unreported";

/// How this run's producer probe and test binary were built, as ordinary CI
/// evidence only.
///
/// CI jobs may restore third-party dependency artifacts written by an
/// earlier main-branch run, which then link into the staged probe and this
/// test binary. Recording both digests and the restored cache key makes that
/// trust root auditable. A cache never makes the run release qualification:
/// every measurement carrying this still states `release_qualification:
/// false`.
///
/// # Panics
///
/// Panics when the staged probe or the running test binary cannot be read,
/// or the cache variable is not UTF-8.
pub(super) fn ci_build_provenance(fixture: &CargoFixture) -> Value {
    let probe = std::fs::read(fixture.probe()).expect("should read the staged producer probe");
    let test_binary = std::env::current_exe()
        .and_then(std::fs::read)
        .expect("should read the running test binary");
    let cache = std::env::var_os(CI_BUILD_CACHE_VARIABLE).map(|value| {
        value
            .into_string()
            .expect("should name the CI build cache in UTF-8")
    });
    build_provenance(&probe, &test_binary, cache.as_deref())
}

/// The provenance of explicit probe and test binary bytes and cache outcome.
fn build_provenance(probe: &[u8], test_binary: &[u8], cache: Option<&str>) -> Value {
    json!({
        "producer_probe_sha256": hex::encode(Sha256::digest(probe)),
        "test_binary_sha256": hex::encode(Sha256::digest(test_binary)),
        "build_cache": cache
            .filter(|cache| !cache.is_empty())
            .unwrap_or(UNREPORTED_BUILD_CACHE),
    })
}

/// The ordinary CI measurement of the Cargo denial fixture.
pub(super) fn denial_measurement(
    proof_errno: i32,
    observed: &BTreeMap<String, i32>,
    control: &RecipeCompleted,
) -> Value {
    let cases: Map<String, Value> = observed
        .iter()
        .map(|(case, errno)| (case.clone(), json!({ "errno": errno })))
        .collect();
    json!({
        "schema_version": SCHEMA_VERSION,
        "kind": "ordinary-ci-cargo-denial",
        "proof_errno": proof_errno,
        "attempts": cases,
        "control": {
            "asset": control.asset().artifact().name,
            "sha256": control.artifact().sha256(),
            "size": control.artifact().size(),
        },
        "release_qualification": false,
    })
}

/// Returns the errno one denial fixture case reported in `text`.
///
/// The fixture prints `TS_DENIAL case=<case> errno=[<n>]`, or
/// `... child errno=[<n>]` from the re-executed grandchild. A connected
/// attempt, a child that never reported or any other text yields `None`.
pub(super) fn reported_errno(case: &str, text: &str) -> Option<i32> {
    let marker = format!("TS_DENIAL case={case} ");
    let start = text.find(&marker)? + marker.len();
    let rest = &text[start..];
    let rest = rest.strip_prefix("child ").unwrap_or(rest);
    let digits = rest.strip_prefix("errno=[")?;
    let end = digits.find(']')?;
    digits[..end].parse().ok()
}

#[test]
fn fixture_description_parses_relative_inputs() {
    let root = Path::new("/example/fixture");
    let parsed = CargoFixture::parse(root, &description()).expect("should parse the description");
    assert_eq!(
        parsed
            .registry
            .get(&("example".to_owned(), "1.0.0".to_owned())),
        Some(&root.join("crates/example-1.0.0.crate")),
        "should key registry originals by locked identity"
    );
    assert_eq!(
        parsed.git[0].objects,
        root.join("git/0/objects"),
        "should resolve Git objects below the root"
    );
    assert_eq!(
        parsed.comparison_manifests[0].cargo_vendor_manifest,
        root.join("cargo-vendor/example-git-0.1.0/Cargo.toml"),
        "should resolve the comparison manifest below the root"
    );
    assert_eq!(
        parsed.probe(),
        root.join("tools/probe"),
        "should fix the probe"
    );
}

/// One in-place change to a fixture description.
type Edit = fn(&mut Value);

#[test]
fn fixture_description_rejects_unsafe_or_foreign_inputs() {
    let root = Path::new("/example/fixture");
    let cases: [(&str, Edit); 8] = [
        ("absolute path", |value| {
            value["registry"][0]["path"] = json!("/example/elsewhere.crate");
        }),
        ("dotted path", |value| {
            value["git"][0]["objects"] = json!("git/../../objects");
        }),
        ("extra field", |value| value["extra"] = json!(true)),
        ("kind", |value| {
            value["kind"] = json!("release-qualification")
        }),
        ("acquisition", |value| {
            value["acquisition"] = json!("authenticated");
        }),
        ("image", |value| value["image_id"] = json!("example:latest")),
        ("duplicate", |value| {
            let first = value["registry"][0].clone();
            value["registry"]
                .as_array_mut()
                .expect("should hold registry entries")
                .push(first);
        }),
        ("orphan comparison manifest", |value| {
            value["comparison_manifests"][0]["selector"] =
                json!("git+https://example.com/other?tag=v2");
        }),
    ];
    for (label, edit) in cases {
        let mut value = description();
        edit(&mut value);
        assert!(
            CargoFixture::parse(root, &value).is_err(),
            "should reject a description with a bad {label}"
        );
    }
}

#[test]
fn fixture_kind_decides_whether_an_image_is_named() {
    let root = Path::new("/example/fixture");
    let linux = CargoFixture::parse(root, &description()).expect("should parse Linux");
    assert_eq!(linux.host, FixtureHost::LinuxContainer);
    assert_eq!(
        linux.container_image_id().ok(),
        Some(format!("sha256:{}", "b".repeat(64)).as_str()),
        "should keep the Linux builder image ID"
    );

    let mut macos = description();
    macos["kind"] = json!("ordinary-ci-macos-cargo-fixture");
    macos["image_id"] = Value::Null;
    let parsed = CargoFixture::parse(root, &macos).expect("should parse macOS");
    assert_eq!(parsed.host, FixtureHost::MacosSandbox);
    assert_eq!(parsed.image_id, None, "should name no image");
    assert!(
        parsed.container_image_id().is_err(),
        "should refuse a container image for a macOS fixture"
    );

    let cases: [(&str, &str, Value); 4] = [
        (
            "macOS with an image",
            "ordinary-ci-macos-cargo-fixture",
            json!(format!("sha256:{}", "b".repeat(64))),
        ),
        (
            "Linux without an image",
            "ordinary-ci-cargo-fixture",
            Value::Null,
        ),
        (
            "macOS with an empty image",
            "ordinary-ci-macos-cargo-fixture",
            json!(""),
        ),
        (
            "an unknown kind",
            "ordinary-ci-windows-cargo-fixture",
            Value::Null,
        ),
    ];
    for (label, kind, image) in cases {
        let mut value = description();
        value["kind"] = json!(kind);
        value["image_id"] = image;
        assert!(
            CargoFixture::parse(root, &value).is_err(),
            "should reject {label}"
        );
    }
}

#[test]
fn cargo_container_settings_are_explicit_and_fit_the_runner() {
    let settings = cargo_docker_settings((1001, 1001), 16);
    assert_eq!(settings.pids_limit, 4096, "should allow Cargo's job tree");
    assert_eq!(
        settings.memory_bytes,
        8 * 1024 * 1024 * 1024,
        "should give the Wasm link 8 GiB"
    );
    assert_eq!(settings.cpus, 4, "should cap CPUs at a 4-vCPU runner");
    assert_eq!(
        cargo_docker_settings((1001, 1001), 2).cpus,
        2,
        "should never exceed host parallelism"
    );
}

#[test]
fn build_provenance_records_digests_and_an_explicit_cache_outcome() {
    let probe = b"fictional probe bytes";
    let test_binary = b"fictional test binary bytes";
    let restored = build_provenance(probe, test_binary, Some("release-tool-example-key"));
    assert_eq!(
        restored["producer_probe_sha256"],
        hex::encode(Sha256::digest(probe)),
        "should hash the exact probe bytes"
    );
    assert_eq!(
        restored["test_binary_sha256"],
        hex::encode(Sha256::digest(test_binary)),
        "should hash the exact test binary bytes"
    );
    assert_eq!(
        restored["build_cache"], "release-tool-example-key",
        "should record the restored cache key"
    );
    assert_eq!(
        build_provenance(probe, test_binary, Some("miss"))["build_cache"],
        "miss",
        "should record a reported miss"
    );
    for absent in [None, Some("")] {
        assert_eq!(
            build_provenance(probe, test_binary, absent)["build_cache"],
            "unreported",
            "should record an unreported cache rather than omit it"
        );
    }
}

#[test]
fn reported_errno_reads_only_the_named_case() {
    let text = "--- stderr\n  TS_DENIAL case=build-direct errno=[1]\n";
    assert_eq!(
        reported_errno("build-direct", text),
        Some(1),
        "should read errno"
    );
    assert_eq!(
        reported_errno(
            "build-grandchild",
            "TS_DENIAL case=build-grandchild child errno=[13]"
        ),
        Some(13),
        "should read the grandchild report"
    );
    assert_eq!(
        reported_errno("macro-attempt", text),
        None,
        "should not read another case"
    );
    for other in [
        "TS_DENIAL case=build-direct connected",
        "TS_DENIAL case=build-direct child failed with exit status: 1",
        "TS_DENIAL case=build-direct errno=[1",
    ] {
        assert_eq!(
            reported_errno("build-direct", other),
            None,
            "should not invent an errno from {other:?}"
        );
    }
}

#[test]
fn denial_fixture_rows_stay_outside_the_fixed_table() {
    for package in [
        "build-direct",
        "build-grandchild",
        "macro-attempt",
        "control",
    ] {
        let asset = AssetId::denial_fixture(package).expect("should resolve the fixture member");
        let artifact = asset.artifact();
        assert_eq!(
            artifact.bin, package,
            "should name the binary after the package"
        );
        assert_eq!(
            artifact.target,
            ReleaseTarget::Wasm32Wasip1,
            "should build the fixture for Wasm"
        );
        assert!(
            AssetId::parse(artifact.name).is_err(),
            "should keep {} unreachable from request text",
            artifact.name
        );
        assert!(
            FIXED_ARTIFACTS
                .iter()
                .all(|fixed| fixed.name != artifact.name && fixed.package != package),
            "should not touch the fixed release table"
        );
    }
    assert!(
        AssetId::denial_fixture("trusted-server-adapter-fastly").is_err(),
        "should resolve only fixture members"
    );
}

#[test]
fn darwin_denial_rows_stay_outside_the_fixed_table() {
    for target in [
        ReleaseTarget::Aarch64AppleDarwin,
        ReleaseTarget::X86_64AppleDarwin,
    ] {
        for package in [
            "build-direct",
            "build-grandchild",
            "macro-attempt",
            "control",
        ] {
            let artifact = AssetId::darwin_denial_fixture(package, target)
                .expect("should resolve the Darwin fixture member")
                .artifact();
            assert_eq!(
                artifact.bin, package,
                "should name the binary after the package"
            );
            assert_eq!(
                artifact.target, target,
                "should build for the Darwin target"
            );
            assert!(
                artifact.name.ends_with(target.triple()),
                "should name the row after its target"
            );
            assert!(
                AssetId::parse(artifact.name).is_err(),
                "should keep {} unreachable from request text",
                artifact.name
            );
            assert!(
                FIXED_ARTIFACTS
                    .iter()
                    .all(|fixed| fixed.name != artifact.name && fixed.package != package),
                "should not touch the fixed release table"
            );
        }
    }
    for (package, target) in [
        ("trusted-server-cli", ReleaseTarget::Aarch64AppleDarwin),
        ("control", ReleaseTarget::Wasm32Wasip1),
        ("control", ReleaseTarget::X86_64UnknownLinuxGnu),
    ] {
        assert!(
            AssetId::darwin_denial_fixture(package, target).is_err(),
            "should resolve only Darwin fixture members, not {package} for {}",
            target.triple()
        );
    }
}

fn description() -> Value {
    let selector = "git+https://example.com/org/workspace?tag=v1";
    json!({
        "schema_version": 1,
        "kind": "ordinary-ci-cargo-fixture",
        "acquisition": "unauthenticated-ordinary-ci",
        "source_commit": "0123456789abcdef0123456789abcdef01234567",
        "cargo_lock_sha256": "a".repeat(64),
        "image_id": format!("sha256:{}", "b".repeat(64)),
        "rustc_version": "rustc 1.95.0 (example)",
        "registry": [
            {"name": "example", "version": "1.0.0", "path": "crates/example-1.0.0.crate"}
        ],
        "git": [
            {"selector": selector, "revision": "c".repeat(40), "objects": "git/0/objects"}
        ],
        "comparison_manifests": [{
            "name": "example-git",
            "version": "0.1.0",
            "selector": selector,
            "cargo_vendor_manifest": "cargo-vendor/example-git-0.1.0/Cargo.toml",
        }],
    })
}

/// SHA-256 of the exact original JS inventory this build consumed.
///
/// # Panics
///
/// Panics when the completed build retains no original JS inventory.
pub(super) fn js_inventory_sha256(completed: &RecipeCompleted) -> String {
    completed
        .javascript_contributions()
        .expect("should retain the original JS inventory")
        .inventory_sha256()
        .to_owned()
}
