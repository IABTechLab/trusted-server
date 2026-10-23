use std::path::PathBuf;

use serde_json::{Value, json};
use sha2::Sha256;

use super::*;
use crate::spdx::{ComponentRole, Origin, valid_created};

const EXECUTABLE: &[u8] = b"fictional candidate executable bytes";
const COMMIT: &str = "1111111111111111111111111111111111111111";
const ROOT_PACKAGE: &str = "example-adapter";

// ---- Step 1: SPDX creation timestamps from SOURCE_DATE_EPOCH ----

#[test]
fn spdx_created_renders_the_unix_epoch_start() {
    let created = spdx_created(0).expect("should render the Unix epoch start");
    assert_eq!(
        created, "1970-01-01T00:00:00Z",
        "should render epoch zero as midnight on 1970-01-01"
    );
    assert!(
        valid_created(&created),
        "should satisfy the renderer timestamp check"
    );
}

#[test]
fn spdx_created_skips_february_29_in_a_non_leap_century() {
    assert_eq!(
        spdx_created(4_107_456_000).expect("should render the last February day of 2100"),
        "2100-02-28T00:00:00Z",
        "should keep 2100-02-28"
    );
    assert_eq!(
        spdx_created(4_107_542_400).expect("should render the following day"),
        "2100-03-01T00:00:00Z",
        "should honour the 100-year non-leap rule"
    );
}

#[test]
fn spdx_created_renders_a_leap_day() {
    let created = spdx_created(951_782_400).expect("should render a leap day");
    assert_eq!(
        created, "2000-02-29T00:00:00Z",
        "should honour the 400-year leap rule"
    );
    assert!(
        valid_created(&created),
        "should satisfy the renderer timestamp check"
    );
}

#[test]
fn spdx_created_renders_time_of_day_and_the_last_four_digit_second() {
    let afternoon = spdx_created(1_700_000_000).expect("should render a time of day");
    assert_eq!(
        afternoon, "2023-11-14T22:13:20Z",
        "should render hours, minutes and seconds"
    );
    let last = spdx_created(253_402_300_799).expect("should render the last four-digit second");
    assert_eq!(
        last, "9999-12-31T23:59:59Z",
        "should render the final representable second"
    );
    for created in [afternoon, last] {
        assert!(
            valid_created(&created),
            "should satisfy the renderer timestamp check"
        );
    }
}

#[test]
fn spdx_created_rejects_years_beyond_four_digits() {
    for epoch in [253_402_300_800, u64::MAX] {
        assert!(
            spdx_created(epoch).is_err(),
            "should reject epoch {epoch} past year 9999"
        );
    }
}

// ---- Step 2: root identity from the policy asset package ----

fn workspace_original(path: &str) -> Original {
    Original::Workspace {
        source_commit: COMMIT.to_owned(),
        path: PathBuf::from(path),
    }
}

fn root_inputs(original: &Original) -> RootIdentityInputs<'_> {
    RootIdentityInputs {
        asset_package: ROOT_PACKAGE,
        observed_name: ROOT_PACKAGE,
        observed_version: "1.2.3",
        origin_name: ROOT_PACKAGE,
        origin_version: "1.2.3",
        original,
        source_commit: COMMIT,
    }
}

#[test]
fn root_identity_accepts_the_asset_package_workspace_origin() {
    let original = workspace_original("crates/example-adapter/Cargo.toml");
    let version = checked_root_version(&root_inputs(&original))
        .expect("should accept the asset package's workspace origin");
    assert_eq!(version, "1.2.3", "should return the verified version");
}

#[test]
fn root_identity_rejects_a_registry_origin() {
    let original = Original::Material {
        source: "registry+https://example.com/index".to_owned(),
        original: crate::materials::CargoOrigin::Registry {
            archive_sha256: "a".repeat(64),
        },
    };
    assert!(
        checked_root_version(&root_inputs(&original)).is_err(),
        "should reject a root package that resolves to a registry original"
    );
}

#[test]
fn root_identity_rejects_another_manifest_path_or_commit() {
    for path in [
        "crates/another-adapter/Cargo.toml",
        "crates/example-adapter/sub/Cargo.toml",
        "Cargo.toml",
    ] {
        let original = workspace_original(path);
        assert!(
            checked_root_version(&root_inputs(&original)).is_err(),
            "should reject workspace manifest {path}"
        );
    }
    let original = Original::Workspace {
        source_commit: "2".repeat(40),
        path: PathBuf::from("crates/example-adapter/Cargo.toml"),
    };
    assert!(
        checked_root_version(&root_inputs(&original)).is_err(),
        "should reject a workspace origin at another commit"
    );
}

#[test]
fn root_identity_rejects_name_or_version_mismatches() {
    let original = workspace_original("crates/example-adapter/Cargo.toml");
    let mut version_mismatch = root_inputs(&original);
    version_mismatch.origin_version = "1.2.4";
    let mut name_mismatch = root_inputs(&original);
    name_mismatch.observed_name = "another-adapter";
    let mut origin_name_mismatch = root_inputs(&original);
    origin_name_mismatch.origin_name = "another-adapter";
    let mut empty_version = root_inputs(&original);
    empty_version.observed_version = "";
    empty_version.origin_version = "";
    for (label, inputs) in [
        ("version", version_mismatch),
        ("observed name", name_mismatch),
        ("origin name", origin_name_mismatch),
        ("empty version", empty_version),
    ] {
        assert!(
            checked_root_version(&inputs).is_err(),
            "should reject a root {label} mismatch"
        );
    }
}

// ---- Step 3: merging the four projected parts ----

fn source_component(identity: &str, role: ComponentRole) -> Component {
    Component {
        identity: identity.to_owned(),
        name: identity.to_owned(),
        version: Some("1.0.0".to_owned()),
        roles: vec![role],
        origin: Some(Origin::Source {
            commit: COMMIT.to_owned(),
            path: "crates/example-adapter/Cargo.toml".to_owned(),
        }),
        declared_license: None,
    }
}

fn tool_component(identity: &str) -> Component {
    Component {
        identity: identity.to_owned(),
        name: identity.to_owned(),
        version: None,
        roles: vec![ComponentRole::BuildTool],
        origin: Some(Origin::StructuredTree {
            sha256: "d".repeat(64),
        }),
        declared_license: None,
    }
}

fn edge(from: &str, to: &str) -> Dependency {
    Dependency {
        from: from.to_owned(),
        to: to.to_owned(),
        build: false,
    }
}

fn parts() -> CandidateParts {
    CandidateParts {
        rust: (
            vec![
                source_component("rust:root", ComponentRole::ObservedRust),
                source_component("rust:dependency", ComponentRole::ObservedRust),
            ],
            vec![edge("rust:root", "rust:dependency")],
        ),
        native: (Vec::new(), Vec::new()),
        javascript: (
            vec![source_component(
                "js:core",
                ComponentRole::RetainedJavascript,
            )],
            Vec::new(),
        ),
        platform: (vec![tool_component("platform:rust-tree")], Vec::new()),
    }
}

#[test]
fn merge_counts_equal_the_sum_of_the_parts() {
    let (components, dependencies) = merge_parts(parts()).expect("should merge disjoint parts");
    assert_eq!(components.len(), 4, "should keep every projected component");
    assert_eq!(dependencies.len(), 1, "should keep every projected edge");
}

#[test]
fn merge_rejects_a_cross_adapter_duplicate_identity() {
    let mut duplicated = parts();
    duplicated
        .platform
        .0
        .push(source_component("js:core", ComponentRole::BuildMaterial));
    assert!(
        merge_parts(duplicated).is_err(),
        "should reject one identity projected by two adapters"
    );
}

#[test]
fn merge_rejects_edges_that_dangle_or_cross_parts() {
    let mut dangling = parts();
    dangling.rust.1.push(edge("rust:root", "rust:absent"));
    assert!(
        merge_parts(dangling).is_err(),
        "should reject an edge to an absent component"
    );
    let mut crossing = parts();
    crossing.rust.1.push(edge("rust:root", "js:core"));
    assert!(
        merge_parts(crossing).is_err(),
        "should reject an edge whose endpoint belongs to another part"
    );
}

#[test]
fn merge_rejects_an_empty_required_part() {
    for label in ["rust", "javascript", "platform"] {
        let mut missing = parts();
        match label {
            "rust" => missing.rust = (Vec::new(), Vec::new()),
            "javascript" => missing.javascript = (Vec::new(), Vec::new()),
            "platform" => missing.platform = (Vec::new(), Vec::new()),
            _ => unreachable!("should select a required part"),
        }
        assert!(
            merge_parts(missing).is_err(),
            "should reject an empty {label} part"
        );
    }
}

// ---- Step 4: gap-bearing candidate rendering ----

fn candidate_inputs() -> SpdxInputs<'static> {
    let (components, dependencies) = merge_parts(parts()).expect("should merge fixture parts");
    SpdxInputs {
        executable: EXECUTABLE,
        expected_artifact_sha256: hex::encode(Sha256::digest(EXECUTABLE)),
        name: "example-adapter.wasm".to_owned(),
        version: "1.2.3".to_owned(),
        source_commit: COMMIT.to_owned(),
        target: crate::policy::ReleaseTarget::Wasm32Wasip1,
        created: "2026-01-01T00:00:00Z".to_owned(),
        document_comment: None,
        components,
        dependencies,
    }
}

fn gaps(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

#[test]
fn candidate_rendering_rejects_an_empty_gap_list() {
    assert!(
        render_candidate(candidate_inputs(), Vec::new()).is_err(),
        "should never present a gap-free candidate as complete"
    );
}

#[test]
fn candidate_rendering_rejects_an_unplain_gap() {
    for gap in ["", "line\nbreak"] {
        assert!(
            render_candidate(candidate_inputs(), gaps(&[gap])).is_err(),
            "should reject gap {gap:?}"
        );
    }
}

#[test]
fn candidate_comment_names_the_candidate_and_lists_sorted_unique_gaps() {
    let candidate = render_candidate(
        candidate_inputs(),
        gaps(&["zeta-gap", "alpha-gap", "zeta-gap"]),
    )
    .expect("should render a gap-bearing candidate");
    assert_eq!(
        candidate.gaps(),
        ["alpha-gap", "zeta-gap"],
        "should retain sorted unique gaps"
    );
    let document: Value =
        serde_json::from_slice(candidate.bytes()).expect("should parse candidate bytes");
    let comment = document["creationInfo"]["comment"]
        .as_str()
        .expect("should carry a creation comment");
    assert!(
        comment.contains("candidate"),
        "should name the document a candidate"
    );
    let alpha = comment
        .find("alpha-gap")
        .expect("should list the first gap");
    let zeta = comment
        .find("zeta-gap")
        .expect("should list the second gap");
    assert!(alpha < zeta, "should list gaps in sorted order");
    assert_eq!(
        comment.matches("zeta-gap").count(),
        1,
        "should list each gap once"
    );
}

#[test]
fn candidate_document_validates_and_binds_root_and_document_digests() {
    let inputs = candidate_inputs();
    let artifact_sha256 = inputs.expected_artifact_sha256.clone();
    let candidate = render_candidate(inputs, gaps(&["native-material-coverage-incomplete"]))
        .expect("should render a gap-bearing candidate");
    let document: Value =
        serde_json::from_slice(candidate.bytes()).expect("should parse candidate bytes");
    crate::documents::validate_spdx(&document).expect("should satisfy the pinned SPDX schema");
    let root = document["packages"]
        .as_array()
        .expect("should contain packages")
        .iter()
        .find(|package| package["SPDXID"] == "SPDXRef-Artifact")
        .expect("should contain the executable root");
    assert_eq!(
        root["checksums"],
        json!([{"algorithm":"SHA256","checksumValue":artifact_sha256}]),
        "should bind the root to the artifact SHA-256"
    );
    assert_eq!(
        candidate.sha256(),
        hex::encode(Sha256::digest(candidate.bytes())),
        "should hash the exact document bytes"
    );
}

#[test]
fn candidate_bounds_reject_documents_over_sixteen_mebibytes() {
    let limit = usize::try_from(Role::Spdx.limit()).expect("should fit the SPDX limit in usize");
    assert!(
        bounded_candidate(vec![b' '; limit + 1], gaps(&["example-gap"])).is_err(),
        "should reject a document past the SPDX limit"
    );
    let bounded = bounded_candidate(b"{}\n".to_vec(), gaps(&["example-gap"]))
        .expect("should accept a small document");
    assert_eq!(
        bounded.sha256(),
        hex::encode(Sha256::digest(b"{}\n")),
        "should hash accepted bytes"
    );
}
