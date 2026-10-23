//! Fictionalized format observations from separately verified upstream proofs.
//!
//! These JSON values contain no signatures and supply no verification authority.
//! Actual cryptographic qualification is recorded separately under
//! `scripts/release/qualification/`; these tests protect parser compatibility.

use std::collections::BTreeMap;

use serde_json::Value;

use super::{Expected, check_certificate, check_subjects, statement};
use crate::release::documents::{ApprovedCommit, ApprovedRelease, ReleaseTag, Role, parse_json};
use crate::release::policy::{PROVENANCE_PREDICATE, SBOM_PREDICATE};

const FORMATS: &[u8] = include_bytes!("../../../tests/fixtures/release/upstream-formats.json");

fn observed(kind: &str) -> Value {
    let formats = parse_json(FORMATS, Role::GhOutput)
        .expect("should parse fictionalized upstream format observations");
    formats[kind][0].clone()
}

#[test]
fn pinned_upstream_provenance_and_spdx_shapes_reach_subject_checks() {
    let digest = "a".repeat(64);
    let expected = BTreeMap::from([("example-asset", digest.as_str())]);
    for (kind, predicate) in [
        ("provenance", PROVENANCE_PREDICATE),
        ("spdx", SBOM_PREDICATE),
    ] {
        let proof = observed(kind);
        let statement = statement(&proof, predicate)
            .expect("should accept the observed pinned verification result shape");
        check_subjects(statement, &expected)
            .expect("should bind the observed subject shape to exact bytes");
        let changed = BTreeMap::from([("another-asset", digest.as_str())]);
        assert!(
            check_subjects(statement, &changed).is_err(),
            "should reject a subject name outside the expected set"
        );
    }
}

#[test]
fn upstream_format_never_confers_official_repository_identity() {
    let release = ApprovedRelease::new(
        ReleaseTag::parse("v1.2.0").expect("should parse the fictional tag"),
        ApprovedCommit::parse("0123456789abcdef0123456789abcdef01234567")
            .expect("should parse the fictional commit"),
    );
    let expected = Expected::new(&release);
    for kind in ["provenance", "spdx"] {
        assert!(
            check_certificate(&observed(kind), &expected).is_err(),
            "should reject an unrelated signer despite compatible JSON"
        );
    }
}

#[test]
fn observed_spdx_predicate_cannot_substitute_for_provenance() {
    assert!(
        statement(&observed("spdx"), PROVENANCE_PREDICATE).is_err(),
        "should keep the observed predicate roles distinct"
    );
}
