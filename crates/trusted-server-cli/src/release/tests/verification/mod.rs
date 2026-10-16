//! Evidence-graph verification: a fully synthetic fictional graph passes and
//! each independently altered edge fails closed.
//!
//! These semantic tests do not qualify a cryptographic verifier or action.
//! The gh runner is exercised separately with real OS fixture children.

use error_stack::Report;
use serde_json::{Value, json};

use crate::release::error::ReleaseError;
use crate::release::evidence::ComparisonOutcome;

use self::fixture::{
    ASSET, Graph, OTHER_COMMIT, OTHER_RUN, PROVENANCE_SUFFIXES, RUN, Signed, consistent,
};

pub(super) mod fixture;
mod publication;

#[test]
fn synthetic_graph_rejects_malformed_or_duplicate_key_recipe() {
    for recipe in [b"not JSON".as_slice(), br#"{"version":1,"version":1}"#] {
        let mut built = Graph::new().build();
        built.documents.build_recipe = recipe.to_vec();
        built.reindex();
        assert_eq!(
            context(built.sign().verify()),
            ReleaseError::Document,
            "should strictly parse authenticated recipe bytes"
        );
    }
}

#[test]
fn synthetic_graph_rejects_unsupported_informational_versions() {
    for field in ["recipe", "comparison"] {
        let mut graph = Graph::new();
        if field == "recipe" {
            graph.build_recipe["version"] = json!(2);
        } else {
            graph.rebuild_check["version"] = json!(2);
        }
        assert!(
            graph.build().sign().verify().is_err(),
            "should reject unsupported {field} version"
        );
    }
}

const ZERO_DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// A named in-place edit of one proof section.
type Mutation = (&'static str, fn(&mut Value));

fn context<T: core::fmt::Debug>(result: Result<T, Report<ReleaseError>>) -> ReleaseError {
    *result
        .expect_err("should reject the altered edge")
        .current_context()
}

fn rejects_evidence(signed: &Signed, why: &str) {
    assert_eq!(
        context(signed.verify()),
        ReleaseError::Evidence,
        "should reject: {why}"
    );
}

fn certificate_mut(proof: &mut Value) -> &mut Value {
    &mut proof["verificationResult"]["signature"]["certificate"]
}

fn statement_mut(proof: &mut Value) -> &mut Value {
    &mut proof["verificationResult"]["statement"]
}

#[test]
fn consistent_graph_verifies() {
    let build = consistent()
        .verify()
        .expect("should verify the consistent fictional graph");

    assert_eq!(build.run_invocation(), RUN, "should keep the shared run");
    assert_eq!(build.consumed().len(), 1, "should verify one artifact");
    let consumed = &build.consumed()[0];
    assert_eq!(consumed.name(), ASSET, "should name the consumed asset");
    assert_eq!(
        consumed.assurance_profile(),
        "provenance-sbom-repeatable",
        "should carry the index profile"
    );
    assert_eq!(
        consumed.comparison(),
        ComparisonOutcome::Matched,
        "should report the rebuild outcome"
    );
}

#[test]
fn rejects_index_subject_digest() {
    let mut signed = consistent();
    statement_mut(&mut signed.proofs.index)["subject"][0]["digest"]["sha256"] = json!(ZERO_DIGEST);

    rejects_evidence(&signed, "index provenance subject digest differs");
}

#[test]
fn rejects_index_bytes_altered_after_signing() {
    let mut signed = consistent();
    signed.index_bytes.push(b'\n');

    rejects_evidence(&signed, "index bytes no longer match the signed subject");
}

#[test]
fn rejects_each_altered_provenance_subject_digest() {
    for position in 0..=PROVENANCE_SUFFIXES.len() {
        let mut signed = consistent();
        statement_mut(&mut signed.proofs.provenance)["subject"][position]["digest"]["sha256"] =
            json!(ZERO_DIGEST);

        rejects_evidence(&signed, &format!("provenance subject {position} digest"));
    }
}

#[test]
fn rejects_missing_and_extra_provenance_subjects() {
    let mut missing = consistent();
    statement_mut(&mut missing.proofs.provenance)["subject"]
        .as_array_mut()
        .expect("should hold subjects")
        .pop();
    rejects_evidence(&missing, "provenance lacks the rebuild-check subject");

    let mut extra = consistent();
    statement_mut(&mut extra.proofs.provenance)["subject"]
        .as_array_mut()
        .expect("should hold subjects")
        .push(json!({"name": "unapproved.bin", "digest": {"sha256": ZERO_DIGEST}}));
    rejects_evidence(&extra, "provenance carries an unapproved subject");
}

#[test]
fn rejects_renamed_signed_subject() {
    let mut signed = consistent();
    statement_mut(&mut signed.proofs.provenance)["subject"][3]["name"] =
        json!("ts-x86_64-unknown-linux-gnu.build-recipe.json");

    rejects_evidence(&signed, "signed subject names another asset");
}

#[test]
fn rejects_spdx_subject() {
    let mut signed = consistent();
    statement_mut(&mut signed.proofs.spdx)["subject"][0]["digest"]["sha256"] = json!(ZERO_DIGEST);

    rejects_evidence(&signed, "SPDX attestation subject is not the executable");
}

#[test]
fn rejects_spdx_root_checksum() {
    let mut graph = Graph::new();
    graph.spdx["packages"][0]["checksums"][0]["checksumValue"] = json!(ZERO_DIGEST);

    rejects_evidence(
        &graph.build().sign(),
        "SPDX root checksum is not the executable digest",
    );
}

#[test]
fn rejects_ambiguous_spdx_root() {
    let mut graph = Graph::new();
    graph.spdx["documentDescribes"] = json!(["SPDXRef-Package-dependency"]);

    rejects_evidence(&graph.build().sign(), "SPDX describes two roots");
}

#[test]
fn rejects_spdx_predicate_differing_from_document() {
    let mut signed = consistent();
    statement_mut(&mut signed.proofs.spdx)["predicate"]["packages"][1]["name"] =
        json!("substituted-crate");

    rejects_evidence(
        &signed,
        "SPDX predicate differs from the published document",
    );
}

#[test]
fn spdx_comparison_ignores_key_order_but_not_array_order() {
    let mut reordered = consistent();
    let predicate = &mut statement_mut(&mut reordered.proofs.spdx)["predicate"];
    let packages = predicate["packages"].clone();
    predicate
        .as_object_mut()
        .expect("should hold predicate object")
        .remove("packages");
    predicate["packages"] = packages;
    reordered
        .verify()
        .expect("should ignore object key order in the SPDX comparison");

    let mut swapped = consistent();
    statement_mut(&mut swapped.proofs.spdx)["predicate"]["packages"]
        .as_array_mut()
        .expect("should hold packages")
        .reverse();
    rejects_evidence(&swapped, "SPDX array order is significant");
}

#[test]
fn rejects_duplicate_keys_in_published_spdx() {
    let mut built = Graph::new().build();
    let text = String::from_utf8(built.documents.spdx.clone()).expect("should be UTF-8");
    built.documents.spdx = text
        .replacen("\"name\":", "\"name\": \"shadow\",\n  \"name\":", 1)
        .into_bytes();
    built.reindex();

    assert_eq!(
        context(built.sign().verify()),
        ReleaseError::Document,
        "should reject a signed but duplicate-key SPDX document"
    );
}

#[test]
fn rejects_js_inventory_digest() {
    let mut graph = Graph::new();
    graph.build_evidence["js_inventory_sha256"] = json!(ZERO_DIGEST);

    rejects_evidence(
        &graph.build().sign(),
        "build evidence names another inventory",
    );
}

#[test]
fn rejects_index_inventory_record() {
    let mut built = Graph::new().build();
    built.index["tsjs_inventory"]["sha256"] = json!(ZERO_DIGEST);
    for artifact in built.index["artifacts"]
        .as_array_mut()
        .expect("should hold artifacts")
    {
        artifact["js_inventory_sha256"] = json!(ZERO_DIGEST);
    }

    rejects_evidence(
        &built.sign(),
        "index inventory digest differs from its bytes",
    );
}

#[test]
fn rejects_prepared_module_table() {
    let mut graph = Graph::new();
    graph.build_evidence["prepared_modules"][1]["size"] = json!(1);

    rejects_evidence(
        &graph.build().sign(),
        "prepared modules differ from inventory",
    );
}

#[test]
fn rejects_retained_module_outside_inventory() {
    let mut graph = Graph::new();
    graph.build_evidence["embedded_modules"][0]["sha256"] = json!(ZERO_DIGEST);

    rejects_evidence(
        &graph.build().sign(),
        "retained module is not an inventory module",
    );
}

#[test]
fn rejects_build_evidence_for_another_executable() {
    let mut graph = Graph::new();
    graph.build_evidence["artifact_sha256"] = json!(ZERO_DIGEST);

    rejects_evidence(
        &graph.build().sign(),
        "build evidence names another executable",
    );
}

#[test]
fn rejects_unknown_rebuild_outcome() {
    let mut graph = Graph::new();
    graph.rebuild_check["outcome"] = json!("assumed");

    rejects_evidence(
        &graph.build().sign(),
        "rebuild outcome outside the closed enum",
    );
}

#[test]
fn rejects_altered_companion_bytes() {
    let mut signed = consistent();
    signed.documents.build_recipe.push(b' ');

    rejects_evidence(&signed, "companion bytes differ from the signed index");
}

#[test]
fn rejects_policy_version_and_digest() {
    let mut digest = Graph::new().build();
    digest.index["policy_sha256"] = json!(ZERO_DIGEST);
    assert_eq!(
        context(digest.sign().verify()),
        ReleaseError::ProducerPolicy,
        "should reject a producer policy digest not matching approved-source bytes"
    );

    let mut version = Graph::new().build();
    version.index["policy_version"] = json!(2);
    assert_eq!(
        context(version.sign().verify()),
        ReleaseError::ProducerPolicy,
        "should reject a producer policy version not matching approved-source bytes"
    );
}

#[test]
fn rejects_another_approved_source_commit() {
    let mut signed = consistent();
    signed.commit = OTHER_COMMIT.to_owned();

    rejects_evidence(&signed, "proofs are not for the approved commit");
}

#[test]
fn rejects_certificate_source_digest_and_resolved_commit() {
    let mut certificate = consistent();
    certificate_mut(&mut certificate.proofs.provenance)["sourceRepositoryDigest"] =
        json!(OTHER_COMMIT);
    rejects_evidence(&certificate, "certificate source digest differs");

    let mut resolved = consistent();
    statement_mut(&mut resolved.proofs.index)["predicate"]["buildDefinition"]["resolvedDependencies"]
        [0]["digest"]["gitCommit"] = json!(OTHER_COMMIT);
    rejects_evidence(&resolved, "resolved source commit differs");
}

#[test]
fn rejects_run_uri_mismatch_between_proofs() {
    let mut spdx = consistent();
    certificate_mut(&mut spdx.proofs.spdx)["runInvocationURI"] = json!(OTHER_RUN);
    rejects_evidence(&spdx, "SPDX proof comes from another run attempt");

    let mut provenance = consistent();
    certificate_mut(&mut provenance.proofs.provenance)["runInvocationURI"] = json!(OTHER_RUN);
    statement_mut(&mut provenance.proofs.provenance)["predicate"]["runDetails"]["metadata"]["invocationId"] =
        json!(OTHER_RUN);
    rejects_evidence(&provenance, "artifact provenance comes from another run");
}

#[test]
fn rejects_invocation_id_differing_from_certificate_run() {
    let mut signed = consistent();
    statement_mut(&mut signed.proofs.index)["predicate"]["runDetails"]["metadata"]["invocationId"] =
        json!(OTHER_RUN);

    rejects_evidence(&signed, "invocationId differs from the certificate run URI");
}

#[test]
fn rejects_foreign_run_uri() {
    let mut signed = consistent();
    let foreign = "https://github.com/IABTechLab/trusted-server-fork/actions/runs/1/attempts/1";
    for proof in [
        &mut signed.proofs.index,
        &mut signed.proofs.provenance,
        &mut signed.proofs.spdx,
    ] {
        certificate_mut(proof)["runInvocationURI"] = json!(foreign);
    }

    rejects_evidence(&signed, "run URI is outside the official repository");
}

#[test]
fn rejects_caller_build_config_uri() {
    let mut certificate = consistent();
    certificate_mut(&mut certificate.proofs.spdx)["buildConfigURI"] = json!(
        "https://github.com/IABTechLab/trusted-server/.github/workflows/other.yml@refs/tags/v1.2.0"
    );
    rejects_evidence(&certificate, "certificate caller is another workflow");

    let mut external = consistent();
    statement_mut(&mut external.proofs.provenance)["predicate"]["buildDefinition"]["externalParameters"]
        ["workflow"]["path"] = json!(".github/workflows/other.yml");
    rejects_evidence(&external, "provenance caller path differs");

    let mut inputs = consistent();
    statement_mut(&mut inputs.proofs.provenance)["predicate"]["buildDefinition"]["externalParameters"]
        ["inputs"] = json!({"command": "fictional"});
    rejects_evidence(&inputs, "external parameters carry arbitrary inputs");
}

#[test]
fn rejects_signer_san_workflow() {
    let mut signed = consistent();
    certificate_mut(&mut signed.proofs.index)["subjectAlternativeName"] = json!(
        "https://github.com/IABTechLab/trusted-server/.github/workflows/release.yml@refs/tags/v1.2.0"
    );

    rejects_evidence(&signed, "SAN names the caller instead of release-build.yml");
}

#[test]
fn rejects_certificate_identity_fields() {
    let fields = [
        ("issuer", json!("https://issuer.example.com")),
        ("sourceRepositoryIdentifier", json!("1")),
        ("sourceRepositoryOwnerIdentifier", json!("2")),
        ("buildTrigger", json!("workflow_dispatch")),
        ("runnerEnvironment", json!("self-hosted")),
        ("buildConfigDigest", json!(OTHER_COMMIT)),
    ];
    for (field, value) in fields {
        let mut signed = consistent();
        certificate_mut(&mut signed.proofs.provenance)[field] = value;

        rejects_evidence(&signed, &format!("certificate `{field}` differs"));
    }
}

#[test]
fn rejects_provenance_predicate_structure() {
    let mutations: [Mutation; 4] = [
        ("build type", |predicate| {
            predicate["buildDefinition"]["buildType"] = json!("https://example.com/build/v1");
        }),
        ("runner", |predicate| {
            predicate["buildDefinition"]["internalParameters"]["github"]["runner_environment"] =
                json!("self-hosted");
        }),
        ("builder id", |predicate| {
            predicate["runDetails"]["builder"]["id"] = json!("https://example.com/builder");
        }),
        ("unexpected structure", |predicate| {
            predicate["extra"] = json!(true);
        }),
    ];
    for (why, mutate) in mutations {
        let mut signed = consistent();
        mutate(&mut statement_mut(&mut signed.proofs.provenance)["predicate"]);

        rejects_evidence(&signed, why);
    }
}

#[test]
fn rejects_wrong_predicate_type() {
    let mut signed = consistent();
    statement_mut(&mut signed.proofs.spdx)["predicateType"] =
        json!("https://slsa.dev/provenance/v1");

    rejects_evidence(&signed, "SPDX proof carries a provenance predicate type");
}
