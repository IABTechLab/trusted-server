use std::fs;
use std::os::unix::fs::symlink;
use std::time::Duration;

use serde_json::{Value, json};
use sha2::Sha256;

use super::complete_darwin::{darwin_executable, darwin_executable_with, rejects_embedded_path};
use super::fixture::*;
use super::*;
use crate::boundary::DescendantCleanup;
use crate::inspect::Violation;
use crate::inspect::tests::builders::{ElfSpec, MachoSpec, elf, macho};

const FINGERPRINT: [u8; 32] = [7; 32];

#[test]
fn a_completed_candidate_without_complete_composition_cannot_generate_spdx() {
    let bytes = wasm_with(&[(".debug_info", b"i"), (".debug_line", b"l")]);
    let mut build = built("trusted-server-fastly.wasm", Role::Canonical, &bytes);
    let stdout = build.stdout(|_| {});
    let completed = build
        .finish(stdout)
        .expect("should complete the accepted candidate artifact");
    build
        .fixture
        .completed
        .rehash_for_handoff()
        .expect("should retain an unchanged owned JS handoff");
    assert!(
        crate::spdx::generate_from_completed(&completed, &build.fixture.completed).is_err(),
        "should reject SPDX generation without complete owning composition inputs"
    );
}

#[test]
fn pure_completion_fixtures_cannot_export_measured_socket_receipts() {
    let bytes = wasm_with(&[(".debug_info", b"i"), (".debug_line", b"l")]);
    let mut build = built("trusted-server-fastly.wasm", Role::Canonical, &bytes);
    let stdout = build.stdout(|_| {});
    let completed = build
        .finish(stdout)
        .expect("should complete the accepted fixture artifact");
    assert!(
        completed.denial_receipt().is_err(),
        "should not export measured socket observations from a pure completion fixture"
    );
}

#[test]
fn pure_completion_fixtures_cannot_export_candidate_transfers() {
    let bytes = wasm_with(&[(".debug_info", b"i"), (".debug_line", b"l")]);
    let mut build = built("trusted-server-fastly.wasm", Role::Canonical, &bytes);
    let stdout = build.stdout(|_| {});
    let completed = build
        .finish(stdout)
        .expect("should complete the accepted fixture artifact");
    assert!(
        completed.composition_export().is_err(),
        "should reject a fixture with no actual retained composition receipts"
    );
    assert!(
        completed.composition_view().is_err(),
        "should refuse a typed owner view without actual retained composition receipts"
    );
    assert!(
        completed.javascript_contributions().is_err(),
        "should refuse original JS evidence absent from the owning completion"
    );
    assert!(
        crate::collection::producer::CandidateBuildExport::from_completed(&completed).is_err(),
        "should not manufacture a measured candidate transfer from a pure fixture"
    );
}

// Inject only a checked JS snapshot into an explicitly unmeasured completion
// fixture. This cannot create composition, socket or workflow authority.
fn completed_with_js_snapshot() -> (Built, RecipeCompleted, Vec<u8>) {
    let javascript = JsBuildCompleted::fixture_for_projection(COMMIT);
    let snapshot = javascript
        .retained_contributions()
        .expect("should validate original JS inventory before retaining it");
    let raw = snapshot.raw_inventory().to_vec();
    let bytes = wasm_with(&[(".debug_info", b"i"), (".debug_line", b"l")]);
    let mut build = built("trusted-server-fastly.wasm", Role::Canonical, &bytes);
    let prepared = build
        .prepared
        .as_mut()
        .expect("should hold prepared fixture");
    prepared.js.inventory_sha256 = snapshot.inventory_sha256().to_owned();
    prepared.js.contributions = Some(snapshot);
    drop(javascript);
    let stdout = build.stdout(|_| {});
    let completed = build
        .finish(stdout)
        .expect("should retain checked JS snapshot");
    (build, completed, raw)
}

#[test]
fn completion_retains_exact_js_inventory_after_original_owner_disappears() {
    let (_build, completed, raw) = completed_with_js_snapshot();
    let snapshot = completed
        .javascript_contributions()
        .expect("should retain checked evidence without its preparation owner or lease");
    assert_eq!(
        snapshot.raw_inventory(),
        raw,
        "should retain original bytes"
    );
    assert_eq!(
        fs::read(completed.retained_inventory_path()).expect("should retain original file"),
        raw,
        "should copy original inventory rather than serialize normalized facts"
    );
    assert!(
        completed.denial_receipt().is_err(),
        "should grant no measured socket authority to a checked JS snapshot"
    );
}

#[test]
fn completion_rejects_changed_retained_js_inventory() {
    let (_build, completed, _) = completed_with_js_snapshot();
    fs::write(completed.retained_inventory_path(), b"changed original")
        .expect("should change retained bytes");
    assert!(
        completed.javascript_contributions().is_err(),
        "should reject a changed original even with unchanged normalized facts"
    );
}

#[test]
fn completion_rejects_aliases_of_unchanged_retained_js_inventory() {
    for hard_link in [false, true] {
        let (_build, completed, raw) = completed_with_js_snapshot();
        let outside = tempfile::tempdir().expect("should create separate inventory storage");
        let replacement = outside.path().join("inventory.json");
        fs::write(&replacement, raw).expect("should copy identical original inventory");
        let retained = completed.retained_inventory_path();
        fs::remove_file(&retained).expect("should replace retained inventory");
        if hard_link {
            fs::hard_link(&replacement, &retained).expect("should install matching hardlink");
        } else {
            symlink(&replacement, &retained).expect("should install matching alias");
        }
        assert!(
            completed.javascript_contributions().is_err(),
            "should reject aliases even when original JS inventory bytes match"
        );
    }
}

#[test]
fn completion_rejects_another_js_snapshot_source_or_digest() {
    for change in [
        (|facts: &mut super::super::completed::SuccessFacts| {
            facts.commit = crate::source::CommitId::parse(&"b".repeat(40))
                .expect("should parse fictional commit")
        }) as fn(&mut super::super::completed::SuccessFacts),
        |facts| facts.js_inventory_sha256 = "c".repeat(64),
    ] {
        let (_build, mut completed, _) = completed_with_js_snapshot();
        change(completed.facts_mut_for_tests());
        assert!(
            completed.javascript_contributions().is_err(),
            "should reject snapshot binding to another completed build"
        );
    }
}

pub(super) fn wasm_with(customs: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = b"\0asm\x01\0\0\0".to_vec();
    for (name, data) in customs
        .iter()
        .copied()
        .chain([("remapped-source", b"/trusted-server/src/main.rs".as_slice())])
    {
        let mut body = vec![u8::try_from(name.len()).expect("should fit")];
        body.extend_from_slice(name.as_bytes());
        body.extend_from_slice(data);
        out.push(0);
        out.push(u8::try_from(body.len()).expect("should fit"));
        out.extend_from_slice(&body);
    }
    out
}

/// Cargo's executable file name for `asset`.
fn executable_name(asset: &str) -> String {
    let artifact = AssetId::parse(asset)
        .expect("should resolve asset")
        .artifact();
    if artifact.target == ReleaseTarget::Wasm32Wasip1 {
        format!("{}.wasm", artifact.bin)
    } else {
        artifact.bin.to_owned()
    }
}

pub(super) struct Built {
    fixture: Fixture,
    prepared: Option<PreparedRecipe>,
    asset: &'static str,
    role: Role,
    cleanup: DescendantCleanup,
}

/// Which Git manifest digests the fictional materials token records.
#[derive(Clone, Copy, Debug)]
pub(super) enum ManifestMarks {
    /// Only committed manifests: nothing normalized or derived.
    Committed,
    /// One producer-normalized manifest.
    Normalized,
    /// One caller-derived manifest.
    Derived,
}

pub(super) fn built(asset: &'static str, role: Role, bytes: &[u8]) -> Built {
    built_with(asset, role, bytes, ManifestMarks::Committed)
}

/// Builds a bound recipe over materials carrying the given manifest marks.
pub(super) fn built_with(
    asset: &'static str,
    role: Role,
    bytes: &[u8],
    marks: ManifestMarks,
) -> Built {
    let fixture = Fixture::new(COMMIT);
    let marked = |materials: PreparedCargoMaterials| match marks {
        ManifestMarks::Committed => materials,
        ManifestMarks::Normalized => {
            materials.with_normalized_for_tests("example-git", &"e".repeat(64))
        }
        ManifestMarks::Derived => {
            materials.with_derived_for_tests("example-1.0.0", &"d".repeat(64))
        }
    };
    let prepared = match marks {
        ManifestMarks::Committed => fixture.bind(asset, role),
        ManifestMarks::Normalized | ManifestMarks::Derived => {
            let request = fixture.request(asset, role.spec_label(), COMMIT);
            let tools = fixture.tools_for(request.asset.artifact().target);
            let materials = marked(fixture.materials(COMMIT));
            fixture.bind_parts(request, role, COMMIT, tools, materials)
        }
    }
    .expect("should bind the recipe");
    let artifact = AssetId::parse(asset)
        .expect("should resolve asset")
        .artifact();
    let release = fixture
        .base
        .join("target")
        .join(artifact.target.triple())
        .join("release");
    fs::create_dir_all(&release).expect("should create the release directory");
    fs::write(release.join(executable_name(asset)), bytes).expect("should write the executable");
    let cleanup = if artifact.target.is_darwin() {
        DescendantCleanup::ReapedAfterRun
    } else {
        DescendantCleanup::OwnedByContainer
    };
    Built {
        fixture,
        prepared: Some(prepared),
        asset,
        role,
        cleanup,
    }
}

impl Built {
    fn prepared(&self) -> &PreparedRecipe {
        self.prepared
            .as_ref()
            .expect("should hold the prepared recipe")
    }

    pub(super) fn stdout(&self, edit: impl FnOnce(&mut Value)) -> Vec<u8> {
        let artifact = AssetId::parse(self.asset)
            .expect("should resolve asset")
            .artifact();
        let release = format!("/boundary/writable/1/{}/release", artifact.target.triple());
        let mut message = json!({
            "reason": "compiler-artifact",
            "manifest_path": format!("/boundary/immutable/0/crates/{}/Cargo.toml", artifact.package),
            "target": {"kind": ["bin"], "crate_types": ["bin"], "name": artifact.bin},
            "profile": {"opt_level": "3", "debuginfo": 1},
            "executable": format!("{release}/{}", executable_name(self.asset)),
            "fresh": false
        });
        edit(&mut message);
        let mut out = Vec::new();
        for line in [
            json!({"reason": "build-script-executed", "env": []}),
            message,
            json!({"reason": "build-finished", "success": true}),
        ] {
            out.extend_from_slice(line.to_string().as_bytes());
            out.push(b'\n');
        }
        out
    }

    fn run(&self, stdout: Vec<u8>) -> RunOutput {
        RunOutput {
            gnu: None,
            stdout,
            stderr_sha256: hex::encode(Sha256::digest(b"fictional diagnostics")),
            started_at_seconds: 1_700_000_000,
            elapsed: Duration::from_secs(42),
            cleanup: self.cleanup,
            queries: super::super::execute::CargoQueryOutput::Fixture,
        }
    }

    pub(super) fn finish(
        &mut self,
        stdout: Vec<u8>,
    ) -> Result<RecipeCompleted, Report<ReleaseError>> {
        let paths = self.fixture.paths(self.role);
        let run = self.run(stdout);
        self.prepared
            .take()
            .expect("should hold the prepared recipe")
            .complete(run, FINGERPRINT, &paths)
    }
}

fn rejection(result: Result<RecipeCompleted, Report<ReleaseError>>) -> String {
    match result {
        Ok(_) => panic!("should reject the completion"),
        Err(error) => format!("{error:?}"),
    }
}

#[test]
fn successful_run_binds_every_input_and_copies_the_artifact() {
    let bytes = darwin_executable();
    let mut built = built("ts-aarch64-apple-darwin", Role::Canonical, &bytes);
    let stdout = built.stdout(|_| {});
    let expected_plan = built
        .prepared()
        .plan(&built.fixture.paths(Role::Canonical))
        .expect("should derive the plan")
        .digest()
        .expect("should digest the plan");
    let materials_digest = built.prepared().materials.digest().to_owned();
    let tools_digest = built.prepared().tools_digest.clone();
    let js = [
        built.fixture.completed.inventory_sha256().to_owned(),
        built.fixture.completed.settings_sha256().to_owned(),
        built.fixture.completed.tools_sha256().to_owned(),
    ];
    let output = built.fixture.base.join("output");
    let completed = built
        .finish(stdout.clone())
        .expect("should complete the run");

    assert_eq!(
        completed.asset().artifact().name,
        "ts-aarch64-apple-darwin",
        "should bind the asset"
    );
    assert_eq!(completed.role(), Role::Canonical, "should bind the role");
    assert_eq!(
        completed.commit().as_str(),
        COMMIT,
        "should bind the commit"
    );
    assert_eq!(
        completed.cargo_lock_sha256(),
        "b".repeat(64),
        "should bind the lock digest"
    );
    assert_eq!(
        completed.materials_digest(),
        materials_digest,
        "should bind the materials digest"
    );
    assert_eq!(
        completed.js_digests(),
        js.each_ref().map(String::as_str),
        "should bind the JS handoff"
    );
    assert_eq!(
        completed.rust_tools_digest(),
        tools_digest,
        "should bind the Rust tools digest"
    );
    assert_eq!(
        completed.boundary_fingerprint(),
        FINGERPRINT,
        "should bind the boundary instance"
    );
    assert_eq!(
        completed.plan_digest(),
        expected_plan,
        "should bind the exact plan"
    );
    assert_eq!(
        completed.ambient().role(),
        Role::Canonical,
        "should bind the ambient"
    );
    assert_eq!(
        completed.started_at_seconds(),
        1_700_000_000,
        "should bind the start time"
    );
    assert_eq!(
        completed.elapsed(),
        Duration::from_secs(42),
        "should bind the elapsed time"
    );
    assert_eq!(
        completed.stdout_sha256(),
        hex::encode(Sha256::digest(&stdout)),
        "should bind the Cargo stream"
    );
    assert_eq!(
        completed.stderr_sha256(),
        hex::encode(Sha256::digest(b"fictional diagnostics")),
        "should bind the diagnostics digest"
    );
    assert_eq!(
        completed.artifact().name(),
        "ts-aarch64-apple-darwin",
        "should name the artifact"
    );
    assert_eq!(
        completed.artifact().size(),
        bytes.len() as u64,
        "should record the size"
    );
    assert_eq!(
        completed.artifact().sha256(),
        hex::encode(Sha256::digest(&bytes)),
        "should record the digest"
    );
    assert_eq!(
        fs::read(output.join("ts-aarch64-apple-darwin")).expect("should read output"),
        bytes,
        "should copy the bytes"
    );
    assert_eq!(
        completed
            .rehash_for_handoff()
            .expect("should rehash the untouched output"),
        output,
        "should return the output directory"
    );
}

#[test]
fn rehash_rejects_changed_output_and_retained_stream() {
    type Tamper = fn(&std::path::Path, &RecipeCompleted);
    let tampers: [(&str, Tamper); 6] = [
        ("edit", |output, _| {
            fs::write(
                output.join("ts-aarch64-apple-darwin"),
                b"tampered executable",
            )
            .expect("should edit");
        }),
        ("extra entry", |output, _| {
            fs::write(output.join("extra"), b"x").expect("should add an entry");
        }),
        ("hard link", |output, _| {
            fs::hard_link(
                output.join("ts-aarch64-apple-darwin"),
                output.join("..").join("link"),
            )
            .expect("should link");
        }),
        ("removal", |output, _| {
            fs::remove_file(output.join("ts-aarch64-apple-darwin")).expect("should remove");
        }),
        ("symlink swap", |output, _| {
            let path = output.join("ts-aarch64-apple-darwin");
            fs::remove_file(&path).expect("should remove");
            fs::write(output.join("..").join("elsewhere"), darwin_executable())
                .expect("should write target");
            symlink(output.join("..").join("elsewhere"), path).expect("should swap in a symlink");
        }),
        ("retained stream", |_, completed| {
            fs::write(completed.retained_stdout_path(), b"tampered stream").expect("should edit");
        }),
    ];
    for (label, tamper) in tampers {
        let mut built = built(
            "ts-aarch64-apple-darwin",
            Role::Canonical,
            &darwin_executable(),
        );
        let stdout = built.stdout(|_| {});
        let output = built.fixture.base.join("output");
        let completed = built.finish(stdout).expect("should complete the run");
        completed
            .rehash_for_handoff()
            .expect("should rehash before tampering");
        tamper(&output, &completed);
        assert!(
            completed.rehash_for_handoff().is_err(),
            "should reject {label} before handoff"
        );
    }
}

#[test]
fn malformed_streams_complete_nothing_and_copy_nothing() {
    type Edit = fn(&mut Value);
    let edits: [(&str, Edit); 5] = [
        ("fresh", |m| m["fresh"] = json!(true)),
        ("debuginfo", |m| m["profile"]["debuginfo"] = json!(0)),
        ("name", |m| m["target"]["name"] = json!("other")),
        ("outside executable", |m| {
            m["executable"] = json!("/boundary/immutable/0/ts");
        }),
        ("executable in a read-only root", |m| {
            m["executable"] = json!("/boundary/immutable/0/target/release/ts");
        }),
    ];
    for (label, edit) in edits {
        let mut built = built(
            "ts-aarch64-apple-darwin",
            Role::Canonical,
            &darwin_executable(),
        );
        let stdout = built.stdout(edit);
        let output = built.fixture.base.join("output");
        assert!(
            built.finish(stdout).is_err(),
            "should reject a stream with {label}"
        );
        assert_eq!(
            fs::read_dir(output).expect("should list output").count(),
            0,
            "should copy nothing after a {label} rejection"
        );
    }
}

#[test]
fn changed_tools_or_materials_or_existing_output_reject_completion() {
    let mut built_tools = built(
        "ts-aarch64-apple-darwin",
        Role::Canonical,
        &darwin_executable(),
    );
    let stdout = built_tools.stdout(|_| {});
    fs::write(built_tools.fixture.base.join("tools/bin/cargo"), b"swapped")
        .expect("should swap cargo");
    assert!(
        rejection(built_tools.finish(stdout)).contains("Rust tools changed"),
        "should reject tools changed after the run"
    );

    let mut built_materials = built(
        "ts-aarch64-apple-darwin",
        Role::Canonical,
        &darwin_executable(),
    );
    let stdout = built_materials.stdout(|_| {});
    fs::write(
        built_materials
            .fixture
            .base
            .join("materials/vendor/example-1.0.0/Cargo.toml"),
        b"swapped",
    )
    .expect("should swap a dependency");
    assert!(
        rejection(built_materials.finish(stdout)).contains("changed after binding"),
        "should reject materials changed after the run"
    );

    let mut built_output = built(
        "ts-aarch64-apple-darwin",
        Role::Canonical,
        &darwin_executable(),
    );
    let stdout = built_output.stdout(|_| {});
    fs::write(
        built_output
            .fixture
            .base
            .join("output/ts-aarch64-apple-darwin"),
        b"planted",
    )
    .expect("should plant an output");
    assert!(
        rejection(built_output.finish(stdout)).contains("exclusively"),
        "should reject an output that exists already"
    );
}

#[test]
fn wasm_artifacts_must_keep_debug_sections() {
    let asset = "trusted-server-fastly.wasm";
    let stripped = wasm_with(&[("name", b"n")]);
    let mut built_stripped = built(asset, Role::Canonical, &stripped);
    let stdout = built_stripped.stdout(|_| {});
    let message = rejection(built_stripped.finish(stdout));
    assert!(
        message.contains("debug section") || message.contains("WasmDebugInfo"),
        "should reject a stripped module: {message}"
    );

    let kept = wasm_with(&[(".debug_info", b"i"), (".debug_line", b"l")]);
    let mut built_kept = built(asset, Role::Canonical, &kept);
    let stdout = built_kept.stdout(|_| {});
    let completed = built_kept
        .finish(stdout)
        .expect("should accept a module with debug sections");
    assert_eq!(
        completed.artifact().name(),
        asset,
        "should publish under the asset name"
    );

    let mut built_text = built(asset, Role::Canonical, b"not wasm at all");
    let stdout = built_text.stdout(|_| {});
    assert!(
        built_text.finish(stdout).is_err(),
        "should reject bytes that are not a module"
    );
}

/// Completes a Linux `ts` run whose executable holds `bytes`.
fn finish_linux(bytes: &[u8]) -> Result<RecipeCompleted, Report<ReleaseError>> {
    let mut built = built("ts-x86_64-unknown-linux-gnu", Role::Canonical, bytes);
    let stdout = built.stdout(|_| {});
    built.finish(stdout)
}

#[test]
fn linux_executables_must_pass_elf_inspection_and_keep_debug_info() {
    let completed = finish_linux(&elf(&ElfSpec {
        payload: b"/trusted-server/src/main.rs",
        ..ElfSpec::conforming()
    }))
    .expect("should accept a conforming executable with debug info");
    assert_eq!(
        completed.artifact().name(),
        "ts-x86_64-unknown-linux-gnu",
        "should publish under the asset name"
    );
    let cases: [(&str, ElfSpec, Violation); 3] = [
        (
            "stripped",
            ElfSpec {
                debug_sections: Vec::new(),
                ..ElfSpec::conforming()
            },
            Violation::ElfDebugInfo,
        ),
        (
            "above the glibc ceiling",
            ElfSpec {
                requirements: vec![("libc.so.6", vec!["GLIBC_2.35"])],
                ..ElfSpec::conforming()
            },
            Violation::SymbolVersion,
        ),
        (
            "runner path",
            ElfSpec {
                payload: b"panic at /home/runner/work/example/src/main.rs",
                ..ElfSpec::conforming()
            },
            Violation::EmbeddedPath,
        ),
    ];
    for (label, spec, expected) in cases {
        let error = finish_linux(&elf(&spec)).expect_err("should reject the executable");
        assert_eq!(
            error.downcast_ref::<Violation>(),
            Some(&expected),
            "should reject the {label} executable by its rule: {error:?}"
        );
    }
    assert!(
        finish_linux(b"fictional text, not an executable").is_err(),
        "should reject bytes that are not ELF"
    );
}

#[test]
fn execute_stays_closed_for_an_ordinary_ci_selection() {
    let built = built(
        "ts-aarch64-apple-darwin",
        Role::Canonical,
        &darwin_executable(),
    );
    assert_eq!(
        built.prepared().tools.authority(),
        Authority::Unqualified,
        "should carry no release authority into the execute gate"
    );
}

#[test]
fn manifest_digests_are_carried_into_the_completion() {
    let finish = |marks: ManifestMarks| {
        let mut built = built_with(
            "ts-aarch64-apple-darwin",
            Role::Canonical,
            &darwin_executable(),
            marks,
        );
        let stdout = built.stdout(|_| {});
        let completed = built.finish(stdout).expect("should complete the run");
        (built, completed)
    };
    let (_plain, completed) = finish(ManifestMarks::Committed);
    assert!(
        completed.derived_manifest_digests().is_empty()
            && completed.normalized_manifest_digests().is_empty(),
        "should carry no normalized or derived manifest when none was used"
    );

    let (_normalized, completed) = finish(ManifestMarks::Normalized);
    assert_eq!(
        completed.normalized_manifest_digests().get("example-git"),
        Some(&"e".repeat(64)),
        "should carry the producer-normalized manifest digest"
    );
    assert!(
        completed.derived_manifest_digests().is_empty(),
        "should not treat a producer-normalized manifest as derived"
    );

    let (_derived, completed) = finish(ManifestMarks::Derived);
    assert_eq!(
        completed.derived_manifest_digests().get("example-1.0.0"),
        Some(&"d".repeat(64)),
        "should carry the derived manifest digest so the build stays marked unqualified"
    );
}

#[test]
fn candidate_measurements_report_no_release_qualification() {
    let asset = "trusted-server-fastly.wasm";
    let kept = wasm_with(&[(".debug_info", b"i"), (".debug_line", b"l")]);
    let mut built = built_with(asset, Role::Canonical, &kept, ManifestMarks::Normalized);
    let stdout = built.stdout(|_| {});
    let completed = built.finish(stdout).expect("should complete the run");

    let measurement = super::candidate::cargo_measurement(&completed);
    assert_eq!(
        measurement["kind"], "ordinary-ci-confined-cargo-build",
        "should name the candidate kind"
    );
    assert_eq!(
        measurement["release_qualification"], false,
        "should never claim release qualification"
    );
    assert_eq!(measurement["asset"], asset, "should name the asset");
    assert_eq!(measurement["role"], "canonical", "should name the role");
    assert_eq!(
        measurement["source_commit"], COMMIT,
        "should bind the commit"
    );
    assert_eq!(
        measurement["artifact"]["sha256"],
        hex::encode(Sha256::digest(&kept)),
        "should report the copied artifact digest"
    );
    assert_eq!(
        measurement["artifact"]["size"],
        kept.len(),
        "should report the copied artifact size"
    );
    assert_eq!(
        measurement["normalized_manifest_count"], 1,
        "should count the producer-normalized manifests"
    );
    assert_eq!(
        measurement["normalized_manifest_digests"]["example-git"],
        "e".repeat(64),
        "should report each producer-normalized manifest digest"
    );
    assert_eq!(
        measurement["derived_manifest_digests"], 0,
        "should report that no caller-derived manifest was consumed"
    );
    for field in ["rust_tools_digest", "materials_digest", "plan_digest"] {
        assert_eq!(
            measurement[field].as_str().map(str::len),
            Some(64),
            "should report the {field}"
        );
    }

    let observed = std::collections::BTreeMap::from([("build-direct".to_owned(), libc::EPERM)]);
    let denial = super::candidate::denial_measurement(libc::EPERM, &observed, &completed);
    assert_eq!(
        denial["attempts"]["build-direct"]["errno"],
        libc::EPERM,
        "should report each observed errno"
    );
    assert_eq!(
        denial["release_qualification"], false,
        "should never claim release qualification"
    );
}

/// Maps the bound roots into an innocuous namespace without `/boundary/`.
fn innocuous_paths(built: &Built) -> super::super::paths::PrefixPaths {
    let prepared = built.prepared();
    super::super::paths::PrefixPaths::new()
        .with(prepared.ambient.checkout_root(), "/work/source", false)
        .with(&prepared.retained_root, "/work/scripts", false)
        .with(&prepared.request.materials, "/work/dependencies", false)
        .with(prepared.tools.root(), "/work/compiler", false)
        .with(&prepared.request.cargo_state, "/work/state", true)
        .with(&prepared.request.target_dir, "/work/products", true)
        .with(&built.fixture.developer_dir(), "/work/developer", false)
}

#[test]
fn completion_rejects_every_bound_host_and_execution_root() {
    for index in 0..15 {
        let mut build = built(
            "ts-aarch64-apple-darwin",
            Role::Canonical,
            &darwin_executable(),
        );
        let paths = innocuous_paths(&build);
        let prepared = build.prepared();
        let host = [
            prepared.ambient.checkout_root().to_owned(),
            prepared.retained_root.clone(),
            prepared.request.materials.clone(),
            prepared.tools.root().to_owned(),
            prepared.request.cargo_state.clone(),
            prepared.request.target_dir.clone(),
            build.fixture.developer_dir(),
        ];
        let mut denied = host.to_vec();
        denied.extend(host.iter().map(|root| {
            paths
                .execution_path(root)
                .expect("should translate a bound root")
        }));
        denied.push(prepared.request.output.clone());
        let leaked = denied[index].join("generated/source.rs");
        let payload = format!(
            "/trusted-server/src/main.rs env!(CARGO_MANIFEST_DIR)={}",
            leaked.display()
        );
        let target = prepared
            .request
            .target_dir
            .join("aarch64-apple-darwin/release/ts");
        fs::write(&target, darwin_executable_with(payload.as_bytes()))
            .expect("should plant the literal in the executable");
        let stdout = build.stdout(|message| {
            message["manifest_path"] = json!("/work/source/crates/trusted-server-cli/Cargo.toml");
            message["executable"] = json!("/work/products/aarch64-apple-darwin/release/ts");
        });
        let run = build.run(stdout);
        let result = build
            .prepared
            .take()
            .expect("should retain the prepared recipe")
            .complete(run, FINGERPRINT, &paths);
        let error = rejection(result);
        assert!(
            rejects_embedded_path(&error),
            "should reject bound root {} by final-path gate: {error}",
            denied[index].display()
        );
    }
}

#[test]
fn completion_rejects_cargo_state_home_and_temporary_literals() {
    for child in ["home", "tmp", "cargo-home"] {
        let mut build = built(
            "ts-aarch64-apple-darwin",
            Role::Canonical,
            &darwin_executable(),
        );
        let paths = innocuous_paths(&build);
        let payload = format!(" env!(OUT_DIR)=/work/state/{child}/generated.rs");
        fs::write(
            build
                .prepared()
                .request
                .target_dir
                .join("aarch64-apple-darwin/release/ts"),
            darwin_executable_with(payload.as_bytes()),
        )
        .expect("should plant cargo-state descendant");
        let stdout = build.stdout(|message| {
            message["manifest_path"] = json!("/work/source/crates/trusted-server-cli/Cargo.toml");
            message["executable"] = json!("/work/products/aarch64-apple-darwin/release/ts");
        });
        let run = build.run(stdout);
        let result = build
            .prepared
            .take()
            .expect("should retain the prepared recipe")
            .complete(run, FINGERPRINT, &paths);
        let error = rejection(result);
        assert!(
            rejects_embedded_path(&error),
            "should reject {child} outside remapped Cargo home too: {error}"
        );
    }
}

#[test]
fn completion_requires_a_fixed_remapped_prefix() {
    let mut build = built(
        "ts-aarch64-apple-darwin",
        Role::Canonical,
        &macho(&MachoSpec::conforming()),
    );
    let stdout = build.stdout(|_| {});
    assert!(
        rejection(build.finish(stdout)).contains("no remapped path prefix"),
        "should require proof of remapping before recording completion"
    );
}

#[test]
fn completion_accepts_fixed_prefixes_in_an_innocuous_execution_namespace() {
    let mut build = built(
        "ts-aarch64-apple-darwin",
        Role::Canonical,
        &darwin_executable_with(b" /cargo-target/generated/file.rs"),
    );
    let paths = innocuous_paths(&build);
    let stdout = build.stdout(|message| {
        message["manifest_path"] = json!("/work/source/crates/trusted-server-cli/Cargo.toml");
        message["executable"] = json!("/work/products/aarch64-apple-darwin/release/ts");
    });
    let run = build.run(stdout);
    build
        .prepared
        .take()
        .expect("should retain prepared recipe")
        .complete(run, FINGERPRINT, &paths)
        .expect("should complete only remapped final bytes without requiring Linux mount names");
}
