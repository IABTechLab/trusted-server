//! A fully synthetic fictional release evidence graph, built in three
//! mutable stages so each test alters exactly one edge:
//!
//! 1. [`Graph`]: companion documents and executable bytes;
//! 2. [`Built`]: the release index derived from those exact bytes;
//! 3. [`Signed`]: gh-shaped proofs and publication JSON derived from the
//!    exact index and companion bytes.

use error_stack::Report;
use serde_json::{Value, json};

use crate::release::artifact::AssetMeasurement;
use crate::release::documents::{ApprovedCommit, ApprovedRelease, ReleaseTag, sha256_hex};
use crate::release::error::ReleaseError;
use crate::release::evidence::{
    BuildEvidenceVerified, ConsumedArtifact, EvidenceInputs, verify_build_evidence,
};
use crate::release::gh::{PredicateKind, VerifiedAttestation, VerifiedReleaseStatement};
use crate::release::github::{PublicationInputs, PublicationVerified, verify_publication};
use crate::release::policy::{EMBEDDED_POLICY, embedded_policy};
use crate::release::source_policy::ApprovedSourcePolicy;

const INDEX_FIXTURE: &[u8] =
    include_bytes!("../../../../tests/fixtures/release/release-index.json");
pub(crate) const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
pub(crate) const OTHER_COMMIT: &str = "89abcdef0123456789abcdef0123456789abcdef";
pub(crate) const TAG: &str = "v1.2.0";
pub(crate) const ASSET: &str = "trusted-server-fastly.wasm";
pub(crate) const RUN: &str =
    "https://github.com/IABTechLab/trusted-server/actions/runs/1234567890/attempts/1";
pub(crate) const OTHER_RUN: &str =
    "https://github.com/IABTechLab/trusted-server/actions/runs/1234567890/attempts/2";
pub(crate) const SAN: &str = "https://github.com/IABTechLab/trusted-server/.github/workflows/release-build.yml@refs/tags/v1.2.0";
pub(crate) const CALLER: &str =
    "https://github.com/IABTechLab/trusted-server/.github/workflows/release.yml@refs/tags/v1.2.0";
const REPOSITORY_URI: &str = "https://github.com/IABTechLab/trusted-server";
const RELEASE_SIGNER: &str = "https://dotcom.releases.github.com"; // allow-domain: dotcom.releases.github.com

/// Suffixes of the five artifact provenance subjects besides the executable.
pub(crate) const PROVENANCE_SUFFIXES: [&str; 4] = [
    ".spdx.json",
    ".build-evidence.json",
    ".build-recipe.json",
    ".rebuild-check.json",
];

pub(crate) fn approved(commit: &str) -> ApprovedRelease {
    ApprovedRelease::new(
        ReleaseTag::parse(TAG).expect("should accept fictional tag"),
        ApprovedCommit::parse(commit).expect("should accept fictional commit"),
    )
}

fn module(id: &str, body: &str) -> Value {
    json!({
        "id": id,
        "filename": format!("tsjs-{id}.js"),
        "size": body.len(),
        "sha256": sha256_hex(body.as_bytes()),
    })
}

/// Stage 1: companion documents and executable bytes.
pub(crate) struct Graph {
    pub(crate) executable: Vec<u8>,
    pub(crate) inventory: Value,
    pub(crate) spdx: Value,
    pub(crate) build_evidence: Value,
    pub(crate) build_recipe: Value,
    pub(crate) rebuild_check: Value,
}

impl Graph {
    pub(crate) fn new() -> Self {
        let executable = b"fictional trusted-server-fastly wasm bytes".to_vec();
        let executable_sha256 = sha256_hex(&executable);
        let modules = json!([module("core", "core();"), module("prebid", "prebid();")]);
        let mut inventory_modules = modules.clone();
        inventory_modules[0]["contributions"] = json!([{"package": "fictional-js"}]);
        let inventory = json!({
            "schema_version": 1,
            "source_commit": COMMIT,
            "modules": inventory_modules,
            "tools": [],
        });
        Self {
            spdx: spdx_document(&executable_sha256),
            build_evidence: json!({
                "schema_version": 1,
                "source_commit": COMMIT,
                "target": "wasm32-wasip1",
                "variant": "default",
                "artifact_sha256": executable_sha256,
                "js_inventory_sha256": sha256_hex(&bytes_of(&inventory)),
                "prepared_modules": modules,
                "embedded_modules": [modules[0].clone()],
                "compatibility_tests": [],
                "inputs": [],
            }),
            build_recipe: json!({"version": 1, "source_commit": COMMIT}),
            rebuild_check: json!({"version": 1, "outcome": "matched"}),
            executable,
            inventory,
        }
    }

    /// Serializes documents and derives the index from their exact bytes.
    pub(crate) fn build(self) -> Built {
        let documents = Documents {
            inventory: bytes_of(&self.inventory),
            spdx: bytes_of(&self.spdx),
            build_evidence: bytes_of(&self.build_evidence),
            build_recipe: bytes_of(&self.build_recipe),
            rebuild_check: bytes_of(&self.rebuild_check),
            provenance_bundle: br#"{"fictional":"artifact provenance bundle"}"#.to_vec(),
            sbom_bundle: br#"{"fictional":"artifact sbom bundle"}"#.to_vec(),
            executable: self.executable,
        };
        let index = index_for(&documents);
        Built { documents, index }
    }
}

fn spdx_document(executable_sha256: &str) -> Value {
    json!({
        "spdxVersion": "SPDX-2.3",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": ASSET,
        "dataLicense": "CC0-1.0",
        "documentNamespace": "https://example.com/spdx/trusted-server-fastly",
        "creationInfo": {"creators": ["Tool: fictional"], "created": "2026-01-01T00:00:00Z"},
        "packages": [
            {
                "SPDXID": "SPDXRef-Package-root",
                "name": ASSET,
                "checksums": [{"algorithm": "SHA256", "checksumValue": executable_sha256}],
            },
            {"SPDXID": "SPDXRef-Package-dependency", "name": "fictional-crate"},
        ],
        "relationships": [
            {
                "spdxElementId": "SPDXRef-DOCUMENT",
                "relationshipType": "DESCRIBES",
                "relatedSpdxElement": "SPDXRef-Package-root",
            },
            {
                "spdxElementId": "SPDXRef-Package-root",
                "relationshipType": "CONTAINS",
                "relatedSpdxElement": "SPDXRef-Package-dependency",
            },
        ],
    })
}

pub(crate) fn bytes_of(value: &Value) -> Vec<u8> {
    serde_json::to_vec_pretty(value).expect("should serialize fixture JSON")
}

/// Exact serialized companion and executable bytes.
pub(crate) struct Documents {
    pub(crate) executable: Vec<u8>,
    pub(crate) inventory: Vec<u8>,
    pub(crate) spdx: Vec<u8>,
    pub(crate) build_evidence: Vec<u8>,
    pub(crate) build_recipe: Vec<u8>,
    pub(crate) rebuild_check: Vec<u8>,
    pub(crate) provenance_bundle: Vec<u8>,
    pub(crate) sbom_bundle: Vec<u8>,
}

fn record(name: &str, bytes: &[u8]) -> Value {
    json!({"name": name, "size": bytes.len(), "sha256": sha256_hex(bytes)})
}

fn index_for(documents: &Documents) -> Value {
    let mut index: Value =
        serde_json::from_slice(INDEX_FIXTURE).expect("should parse index fixture");
    index["policy_sha256"] = json!(sha256_hex(EMBEDDED_POLICY));
    index["tsjs_inventory"] = record("tsjs-inventory.json", &documents.inventory);
    let inventory_sha256 = sha256_hex(&documents.inventory);
    for artifact in index["artifacts"]
        .as_array_mut()
        .expect("should hold artifacts")
    {
        artifact["js_inventory_sha256"] = json!(inventory_sha256);
        if artifact["name"] != ASSET {
            continue;
        }
        artifact["size"] = json!(documents.executable.len());
        artifact["sha256"] = json!(sha256_hex(&documents.executable));
        let companions = &mut artifact["companions"];
        companions["sbom"] = record(&format!("{ASSET}.spdx.json"), &documents.spdx);
        companions["build_evidence"] = record(
            &format!("{ASSET}.build-evidence.json"),
            &documents.build_evidence,
        );
        companions["build_recipe"] = record(
            &format!("{ASSET}.build-recipe.json"),
            &documents.build_recipe,
        );
        companions["rebuild_check"] = record(
            &format!("{ASSET}.rebuild-check.json"),
            &documents.rebuild_check,
        );
        companions["provenance"] = record(
            &format!("{ASSET}.provenance.sigstore.json"),
            &documents.provenance_bundle,
        );
        companions["sbom_attestation"] = record(
            &format!("{ASSET}.sbom.sigstore.json"),
            &documents.sbom_bundle,
        );
    }
    index
}

/// Stage 2: documents plus the release index value.
pub(crate) struct Built {
    pub(crate) documents: Documents,
    pub(crate) index: Value,
}

/// gh-shaped verified proofs for every collected bundle.
pub(crate) struct Proofs {
    pub(crate) index: Value,
    pub(crate) provenance: Value,
    pub(crate) spdx: Value,
}

/// Official publication JSON for the release.
pub(crate) struct Publication {
    pub(crate) release: Value,
    pub(crate) tag_ref: Value,
    pub(crate) statement: Value,
    pub(crate) index_bundle: Vec<u8>,
    pub(crate) checksums: Vec<u8>,
}

impl Built {
    /// Re-derives the index after directly altering serialized documents.
    pub(crate) fn reindex(&mut self) {
        self.index = index_for(&self.documents);
    }

    /// Serializes the index and signs proofs/publication over exact bytes.
    pub(crate) fn sign(self) -> Signed {
        let index_bytes = bytes_of(&self.index);
        let documents = &self.documents;
        let mut provenance_subjects = vec![(ASSET.to_owned(), sha256_hex(&documents.executable))];
        let companions = [
            &documents.spdx,
            &documents.build_evidence,
            &documents.build_recipe,
            &documents.rebuild_check,
        ];
        for (suffix, bytes) in PROVENANCE_SUFFIXES.iter().zip(companions) {
            provenance_subjects.push((format!("{ASSET}{suffix}"), sha256_hex(bytes)));
        }
        let spdx_value: Value =
            serde_json::from_slice(&documents.spdx).expect("should parse fixture SPDX");
        let proofs = Proofs {
            index: proof(
                PROVENANCE,
                &[("release-index.json".to_owned(), sha256_hex(&index_bytes))],
                &provenance_predicate(RUN),
                RUN,
            ),
            provenance: proof(
                PROVENANCE,
                &provenance_subjects,
                &provenance_predicate(RUN),
                RUN,
            ),
            spdx: proof(
                SPDX,
                &[(ASSET.to_owned(), sha256_hex(&documents.executable))],
                &spdx_value,
                RUN,
            ),
        };
        let publication = publication_for(&self.index, &index_bytes);
        Signed {
            commit: COMMIT.to_owned(),
            documents: self.documents,
            index_bytes,
            proofs,
            publication,
        }
    }
}

const PROVENANCE: &str = "https://slsa.dev/provenance/v1";
const SPDX: &str = "https://spdx.dev/Document/v2.3";

pub(crate) fn certificate(run: &str) -> Value {
    json!({
        "certificateIssuer": "CN=fictional-intermediate,O=example.com",
        "subjectAlternativeName": SAN,
        "issuer": "https://token.actions.githubusercontent.com",
        "githubWorkflowTrigger": "push",
        "githubWorkflowSHA": COMMIT,
        "githubWorkflowName": "Release",
        "githubWorkflowRepository": "IABTechLab/trusted-server",
        "githubWorkflowRef": "refs/tags/v1.2.0",
        "buildSignerURI": SAN,
        "buildSignerDigest": COMMIT,
        "runnerEnvironment": "github-hosted",
        "sourceRepositoryURI": REPOSITORY_URI,
        "sourceRepositoryDigest": COMMIT,
        "sourceRepositoryRef": "refs/tags/v1.2.0",
        "sourceRepositoryIdentifier": "950860906",
        "sourceRepositoryOwnerURI": "https://github.com/IABTechLab",
        "sourceRepositoryOwnerIdentifier": "82903538",
        "buildConfigURI": CALLER,
        "buildConfigDigest": COMMIT,
        "buildTrigger": "push",
        "runInvocationURI": run,
        "sourceRepositoryVisibilityAtSigning": "public",
    })
}

pub(crate) fn provenance_predicate(run: &str) -> Value {
    json!({
        "buildDefinition": {
            "buildType": "https://actions.github.io/buildtypes/workflow/v1",
            "externalParameters": {
                "workflow": {
                    "ref": "refs/tags/v1.2.0",
                    "repository": REPOSITORY_URI,
                    "path": ".github/workflows/release.yml",
                }
            },
            "internalParameters": {
                "github": {
                    "event_name": "push",
                    "repository_id": "950860906",
                    "repository_owner_id": "82903538",
                    "runner_environment": "github-hosted",
                }
            },
            "resolvedDependencies": [{
                "uri": "git+https://github.com/IABTechLab/trusted-server@refs/tags/v1.2.0",
                "digest": {"gitCommit": COMMIT},
            }],
        },
        "runDetails": {
            "builder": {"id": SAN},
            "metadata": {"invocationId": run},
        },
    })
}

fn proof(
    predicate_type: &str,
    subjects: &[(String, String)],
    predicate: &Value,
    run: &str,
) -> Value {
    let subjects: Vec<Value> = subjects
        .iter()
        .map(|(name, sha256)| json!({"name": name, "digest": {"sha256": sha256}}))
        .collect();
    json!({
        "attestation": {
            "bundle": {"mediaType": "application/vnd.dev.sigstore.bundle.v0.3+json"},
            "bundle_url": "",
        },
        "verificationResult": {
            "mediaType": "application/vnd.dev.sigstore.verificationresult+json;version=0.1",
            "signature": {"certificate": certificate(run)},
            "statement": {
                "_type": "https://in-toto.io/Statement/v1",
                "subject": subjects,
                "predicateType": predicate_type,
                "predicate": predicate,
            },
            "verifiedTimestamps": [],
        },
    })
}

/// Every release asset (name, size, SHA-256) for a signed index.
fn release_assets(index: &Value, index_bytes: &[u8], extra: &[(&str, &[u8])]) -> Vec<Value> {
    let mut assets = vec![index["tsjs_inventory"].clone()];
    for artifact in index["artifacts"]
        .as_array()
        .expect("should hold artifacts")
    {
        assets.push(json!({
            "name": artifact["name"],
            "size": artifact["size"],
            "sha256": artifact["sha256"],
        }));
        for companion in artifact["companions"]
            .as_object()
            .expect("should hold companions")
            .values()
        {
            assets.push(companion.clone());
        }
    }
    assets.push(record("release-index.json", index_bytes));
    for (name, bytes) in extra {
        assets.push(record(name, bytes));
    }
    assets
}

fn publication_for(index: &Value, index_bytes: &[u8]) -> Publication {
    let index_bundle = br#"{"fictional":"index provenance bundle"}"#.to_vec();
    let checksums = b"fictional checksum list\n".to_vec();
    let assets = release_assets(
        index,
        index_bytes,
        &[
            ("release-index.provenance.sigstore.json", &index_bundle),
            ("SHA256SUMS", &checksums),
        ],
    );
    let rest_assets: Vec<Value> = assets
        .iter()
        .map(|asset| {
            json!({
                "name": asset["name"],
                "size": asset["size"],
                "state": "uploaded",
                "digest": format!("sha256:{}", asset["sha256"].as_str().unwrap_or_default()),
            })
        })
        .collect();
    let mut subjects = vec![
        json!({"uri": format!("pkg:github/IABTechLab/trusted-server@{TAG}"), "digest": {"sha1": COMMIT}}),
    ];
    subjects.extend(
        assets
            .iter()
            .map(|asset| json!({"name": asset["name"], "digest": {"sha256": asset["sha256"]}})),
    );
    Publication {
        release: json!({
            "tag_name": TAG,
            "draft": false,
            "prerelease": false,
            "immutable": true,
            "assets": rest_assets,
        }),
        tag_ref: json!({
            "ref": "refs/tags/v1.2.0",
            "object": {"sha": COMMIT, "type": "commit"},
        }),
        statement: json!({
            "attestation": {"bundle": {}, "bundle_url": ""},
            "verificationResult": {
                "mediaType": "application/vnd.dev.sigstore.verificationresult+json;version=0.1",
                "signature": {"certificate": {"subjectAlternativeName": RELEASE_SIGNER}},
                "statement": {
                    "_type": "https://in-toto.io/Statement/v1",
                    "predicateType": "https://in-toto.io/attestation/release/v0.1",
                    "subject": subjects,
                    "predicate": {"tag": TAG, "repository": "IABTechLab/trusted-server", "repositoryId": "950860906", "ownerId": "82903538", "purl": format!("pkg:github/IABTechLab/trusted-server@{TAG}")},
                },
            },
        }),
        index_bundle,
        checksums,
    }
}

/// Stage 3: exact bytes, proofs and publication data.
pub(crate) struct Signed {
    pub(crate) commit: String,
    pub(crate) documents: Documents,
    pub(crate) index_bytes: Vec<u8>,
    pub(crate) proofs: Proofs,
    pub(crate) publication: Publication,
}

impl Signed {
    /// Runs the evidence-graph verifier over this graph.
    pub(crate) fn verify(&self) -> Result<BuildEvidenceVerified, Report<ReleaseError>> {
        let policy = embedded_policy().expect("should accept embedded policy");
        let release = approved(&self.commit);
        let source_policy = ApprovedSourcePolicy::for_tests(
            ApprovedCommit::parse(COMMIT).expect("should accept fictional commit"),
            EMBEDDED_POLICY.to_vec(),
        );
        let documents = &self.documents;
        let executable = AssetMeasurement::of_bytes(&documents.executable);
        let provenance_bundle = AssetMeasurement::of_bytes(&documents.provenance_bundle);
        let sbom_bundle = AssetMeasurement::of_bytes(&documents.sbom_bundle);
        let index_bundle = AssetMeasurement::of_bytes(&self.publication.index_bundle);
        let index_proof = VerifiedAttestation::synthetic_for_tests(
            self.proofs.index.clone(),
            release.clone(),
            PredicateKind::Provenance,
            AssetMeasurement::of_bytes(&self.index_bytes),
            index_bundle.clone(),
        );
        let provenance_proof = VerifiedAttestation::synthetic_for_tests(
            self.proofs.provenance.clone(),
            release.clone(),
            PredicateKind::Provenance,
            executable.clone(),
            provenance_bundle.clone(),
        );
        let spdx_proof = VerifiedAttestation::synthetic_for_tests(
            self.proofs.spdx.clone(),
            release.clone(),
            PredicateKind::Spdx,
            executable.clone(),
            sbom_bundle.clone(),
        );
        let artifacts = [ConsumedArtifact {
            name: ASSET,
            executable: &executable,
            spdx: &documents.spdx,
            build_evidence: &documents.build_evidence,
            build_recipe: &documents.build_recipe,
            rebuild_check: &documents.rebuild_check,
            provenance_bundle: &provenance_bundle,
            sbom_bundle: &sbom_bundle,
            provenance_proof: &provenance_proof,
            spdx_proof: &spdx_proof,
        }];
        verify_build_evidence(&EvidenceInputs {
            release: &release,
            consumer_policy: &policy,
            source_policy: &source_policy,
            index_bytes: &self.index_bytes,
            index_proof: &index_proof,
            index_bundle: &index_bundle,
            inventory_bytes: &documents.inventory,
            artifacts: &artifacts,
        })
    }

    /// Runs evidence then publication verification.
    pub(crate) fn publish(&self) -> Result<PublicationVerified, Report<ReleaseError>> {
        let build = self.verify()?;
        let publication = &self.publication;
        let release_proof = VerifiedReleaseStatement::synthetic_for_tests(
            publication.statement.clone(),
            approved(&self.commit),
        );
        verify_publication(
            build,
            &PublicationInputs {
                release: &publication.release,
                tag_ref: &publication.tag_ref,
                release_statement: &release_proof,
                index_bundle: &AssetMeasurement::of_bytes(&publication.index_bundle),
                checksums: &AssetMeasurement::of_bytes(&publication.checksums),
            },
        )
    }
}

/// Builds and signs an unmodified, fully consistent graph.
pub(crate) fn consistent() -> Signed {
    Graph::new().build().sign()
}
