//! Fixed gh argument vectors.

use super::*;

// Fixed argument vectors.

#[test]
fn attestation_argv_is_fixed_for_both_predicates() {
    let subject = Path::new("/verify/trusted-server-fastly.wasm");
    let signer = "https://github.com/IABTechLab/trusted-server/.github/workflows/release-build.yml@refs/tags/v1.2.0";

    for (predicate, uri, bundle) in [
        (
            PredicateKind::Provenance,
            "https://slsa.dev/provenance/v1",
            "/verify/trusted-server-fastly.wasm.provenance.sigstore.json",
        ),
        (
            PredicateKind::Spdx,
            "https://spdx.dev/Document/v2.3",
            "/verify/trusted-server-fastly.wasm.sbom.sigstore.json",
        ),
    ] {
        let argv = attestation_verify_args(subject, Path::new(bundle), &release(), predicate)
            .expect("should build fixed argv");

        assert_eq!(
            argv,
            args(&[
                "attestation",
                "verify",
                "/verify/trusted-server-fastly.wasm",
                "--bundle",
                bundle,
                "-R",
                "IABTechLab/trusted-server",
                "--hostname",
                "github.com",
                "--cert-identity",
                signer,
                "--cert-oidc-issuer",
                "https://token.actions.githubusercontent.com",
                "--source-ref",
                "refs/tags/v1.2.0",
                "--source-digest",
                COMMIT,
                "--signer-digest",
                COMMIT,
                "--deny-self-hosted-runners",
                "--predicate-type",
                uri,
                "--format",
                "json",
            ]),
            "should match the reviewed command for {uri}"
        );
    }
}

#[test]
fn attestation_argv_rejects_unsafe_subject_and_bundle() {
    let subject = Path::new("/verify/trusted-server-fastly.wasm");
    let provenance = Path::new("/verify/trusted-server-fastly.wasm.provenance.sigstore.json");

    for (subject, bundle, predicate, reason) in [
        (
            Path::new("--help"),
            provenance,
            PredicateKind::Provenance,
            "flag-like subject",
        ),
        (
            subject,
            Path::new("relative.provenance.sigstore.json"),
            PredicateKind::Provenance,
            "relative bundle",
        ),
        (
            subject,
            provenance,
            PredicateKind::Spdx,
            "provenance bundle for SPDX predicate",
        ),
        (
            subject,
            Path::new("/verify/trusted-server-fastly.wasm.sbom.sigstore.json"),
            PredicateKind::Provenance,
            "SBOM bundle for provenance predicate",
        ),
    ] {
        assert!(
            attestation_verify_args(subject, bundle, &release(), predicate).is_err(),
            "should reject {reason}"
        );
    }
}

#[test]
fn release_argv_is_fixed() {
    let tag = ReleaseTag::parse("v1.2.0").expect("should parse tag");

    assert_eq!(
        release_verify_args(&tag),
        args(&[
            "release",
            "verify",
            "v1.2.0",
            "-R",
            "github.com/IABTechLab/trusted-server",
            "--format",
            "json"
        ]),
        "should match reviewed release verify"
    );
    assert_eq!(
        release_verify_asset_args(&tag, Path::new("/verify/release-index.json"))
            .expect("should build verify-asset argv"),
        args(&[
            "release",
            "verify-asset",
            "v1.2.0",
            "/verify/release-index.json",
            "-R",
            "github.com/IABTechLab/trusted-server",
        ]),
        "should match reviewed release verify-asset"
    );
    assert!(
        release_verify_asset_args(&tag, Path::new("relative.json")).is_err(),
        "should require absolute asset path"
    );
}
