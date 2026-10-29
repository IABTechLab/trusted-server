//! Pure tests of producer-owned manifest normalization over fictional workspaces.

use super::super::normalize::{
    AncestorManifest, INHERITED_PACKAGE_FIELDS, PackageManifest, check_target_paths,
    normalize_manifest, relocate,
};

const ROOT: &str = r#"[workspace]
members = ["crates/example-core", "crates/example-sibling"]
exclude = ["examples/example-demo"]

[workspace.package]
authors = ["Example Team <team@example.com>"]
categories = ["example-category"]
description = "Fictional workspace description"
documentation = "https://docs.example.com/example-core"
edition = "2024"
exclude = ["fixtures/"]
homepage = "https://example.com"
include = ["src/**", "Cargo.toml"]
keywords = ["example"]
license = "Apache-2.0"
publish = false
repository = "https://example.com/org/workspace"
rust-version = "1.85"
version = "0.4.0"

[workspace.dependencies]
example-plain = "1"
example-featured = { version = "2", features = ["base"] }
example-quiet = { version = "3", default-features = false, features = ["core"] }
example-sibling = { path = "crates/example-sibling", default-features = false }
example-renamed = { version = "4", package = "example-original" }

[workspace.lints.rust]
unsafe_code = "deny"

[workspace.lints.clippy]
pedantic = { level = "warn", priority = -1 }
"#;

const MEMBER: &str = r#"[package]
name = "example-core"
authors.workspace = true
categories.workspace = true
description.workspace = true
documentation.workspace = true
edition.workspace = true
exclude.workspace = true
homepage.workspace = true
include.workspace = true
keywords.workspace = true
license.workspace = true
publish.workspace = true
repository.workspace = true
rust-version.workspace = true
version = { workspace = true }

[features]
default = ["example-featured/extra"]

[dependencies]
example-plain = { workspace = true }
example-featured = { workspace = true, features = ["extra", "base"], optional = true }
example-quiet = { workspace = true, default-features = true }
example-sibling = { workspace = true, features = ["sibling-extra"] }
example-own = { path = "../example-own", version = "5" }

[target.'cfg(unix)'.dependencies]
example-renamed = { workspace = true }

[dev-dependencies]
example-plain = { workspace = true, features = ["testing"] }

[build-dependencies]
example-plain.workspace = true

[lints]
workspace = true
"#;

const EXPECTED: &str = r#"[package]
name = "example-core"
authors = ["Example Team <team@example.com>"]
categories = ["example-category"]
description = "Fictional workspace description"
documentation = "https://docs.example.com/example-core"
edition = "2024"
exclude = ["fixtures/"]
homepage = "https://example.com"
include = ["src/**", "Cargo.toml"]
keywords = ["example"]
license = "Apache-2.0"
publish = false
repository = "https://example.com/org/workspace"
rust-version = "1.85"
version = "0.4.0"

[features]
default = ["example-featured/extra"]

[dependencies]
example-plain = { version = "1" }
example-featured = { version = "2", features = ["base", "extra"], optional = true }
example-quiet = { version = "3", default-features = true, features = ["core"] }
example-sibling = { path = "../example-sibling", default-features = false, features = ["sibling-extra"] }
example-own = { path = "../example-own", version = "5" }

[target.'cfg(unix)'.dependencies]
example-renamed = { version = "4", package = "example-original" }

[dev-dependencies]
example-plain = { version = "1", features = ["testing"] }

[build-dependencies]
example-plain = { version = "1" }

[lints.rust]
unsafe_code = "deny"

[lints.clippy]
pedantic = { level = "warn", priority = -1 }
"#;

fn root_ancestors(root: &str) -> Vec<AncestorManifest> {
    vec![AncestorManifest {
        directory: String::new(),
        bytes: root.as_bytes().to_vec(),
    }]
}

fn normalize(
    member: &str,
    version: &str,
    ancestors: &[AncestorManifest],
) -> Result<Vec<u8>, String> {
    normalize_manifest(
        &PackageManifest {
            name: "example-core",
            version,
            directory: "crates/example-core",
            bytes: member.as_bytes(),
        },
        ancestors,
    )
    .map_err(|error| format!("{error:?}"))
}

fn table(bytes: &[u8]) -> toml::Table {
    toml::from_str(std::str::from_utf8(bytes).expect("should be UTF-8")).expect("should be TOML")
}

#[test]
fn every_supported_inheritance_kind_resolves_against_the_workspace_root() {
    let normalized =
        normalize(MEMBER, "0.4.0", &root_ancestors(ROOT)).expect("should normalize the member");
    assert_eq!(
        table(&normalized),
        table(EXPECTED.as_bytes()),
        "should resolve package fields, dependencies, features, defaults, paths and lints"
    );
    let text = String::from_utf8(normalized).expect("should be UTF-8");
    assert!(
        !text.contains("workspace = true") && !text.contains("[workspace"),
        "should leave no inheritance marker and no workspace table"
    );
    assert!(
        text.starts_with("# Normalized by trusted-server-release"),
        "should say who normalized the manifest"
    );
}

#[test]
fn every_listed_package_field_is_exercised() {
    let member = table(MEMBER.as_bytes());
    let package = member["package"]
        .as_table()
        .expect("should hold a package table");
    for field in INHERITED_PACKAGE_FIELDS {
        assert!(
            package.get(field).and_then(toml::Value::as_table).is_some(),
            "should inherit package.{field} in the fixture"
        );
    }
}

#[test]
fn normalization_is_deterministic_and_independent_of_input_key_order() {
    let ancestors = root_ancestors(ROOT);
    let first = normalize(MEMBER, "0.4.0", &ancestors).expect("should normalize");
    let second = normalize(MEMBER, "0.4.0", &ancestors).expect("should normalize again");
    assert_eq!(
        first, second,
        "should produce identical bytes for equal input"
    );
    let reordered = MEMBER.replace(
        "authors.workspace = true\ncategories.workspace = true\n",
        "categories.workspace = true\nauthors.workspace = true\n",
    );
    assert_ne!(reordered, MEMBER, "should actually reorder the fixture");
    assert_eq!(
        normalize(&reordered, "0.4.0", &ancestors).expect("should normalize the reordered input"),
        first,
        "should sort keys so input order cannot change the bytes"
    );
}

#[test]
fn nearest_workspace_root_is_found_past_plain_ancestor_manifests() {
    let ancestors = vec![
        AncestorManifest {
            directory: "crates".to_owned(),
            bytes: b"[package]\nname = \"example-umbrella\"\nversion = \"9.9.9\"\n".to_vec(),
        },
        AncestorManifest {
            directory: String::new(),
            bytes: ROOT.as_bytes().to_vec(),
        },
    ];
    let normalized = normalize(MEMBER, "0.4.0", &ancestors).expect("should skip plain manifests");
    assert_eq!(
        table(&normalized),
        table(EXPECTED.as_bytes()),
        "should resolve against the enclosing workspace root"
    );
}

#[test]
fn a_package_that_is_its_own_workspace_root_drops_the_workspace_table() {
    let member = "[package]\nname = \"example-core\"\nversion.workspace = true\n\n[workspace]\n\n[workspace.package]\nversion = \"0.4.0\"\n\n[workspace.dependencies]\nexample-plain = \"1\"\n\n[dependencies]\nexample-plain.workspace = true\n";
    let normalized = normalize_manifest(
        &PackageManifest {
            name: "example-core",
            version: "0.4.0",
            directory: "",
            bytes: member.as_bytes(),
        },
        &[],
    )
    .expect("should resolve against its own workspace table");
    assert_eq!(
        table(&normalized),
        table(b"[package]\nname = \"example-core\"\nversion = \"0.4.0\"\n\n[dependencies]\nexample-plain = { version = \"1\" }\n"),
        "should drop [workspace] and resolve inheritance"
    );
}

#[test]
fn workspace_paths_are_relocated_relative_to_the_package() {
    let cases = [
        (
            "",
            "crates/example-sibling",
            "crates/example-core",
            "../example-sibling",
        ),
        (
            "",
            "./crates/deep/example-leaf",
            "crates/example-core",
            "../deep/example-leaf",
        ),
        (
            "tools",
            "../crates/example-sibling",
            "crates/example-core",
            "../example-sibling",
        ),
        (
            "",
            "crates/example-core/inner",
            "crates/example-core",
            "inner",
        ),
        ("", "example-top", "example-top", "."),
    ];
    for (workspace, path, package, expected) in cases {
        assert_eq!(
            relocate(workspace, path, package).expect("should relocate the path"),
            expected,
            "should rewrite {path} from {workspace:?} for {package}"
        );
    }
    for (path, label) in [
        ("/example/absolute", "absolute"),
        ("../outside", "escaping"),
        ("", "empty"),
        ("crates\\example", "backslash"),
    ] {
        assert!(
            relocate("", path, "crates/example-core").is_err(),
            "should refuse a {label} workspace path"
        );
    }
}

#[test]
fn unsupported_or_unresolvable_inheritance_fails_closed() {
    let member_with = |extra: &str| format!("{MEMBER}{extra}");
    let replace = |from: &str, to: &str| MEMBER.replacen(from, to, 1);
    let cases: Vec<(&str, String, String, &str)> = vec![
        (
            "inherited readme",
            replace("authors.workspace = true", "readme.workspace = true"),
            ROOT.to_owned(),
            "workspace-relative path",
        ),
        (
            "inherited license-file",
            replace("authors.workspace = true", "license-file.workspace = true"),
            ROOT.to_owned(),
            "workspace-relative path",
        ),
        (
            "unknown inheritable key",
            replace("authors.workspace = true", "links.workspace = true"),
            ROOT.to_owned(),
            "not an inheritable field",
        ),
        (
            "workspace = false",
            replace("authors.workspace = true", "authors.workspace = false"),
            ROOT.to_owned(),
            "exactly { workspace = true }",
        ),
        (
            "missing workspace.package field",
            MEMBER.to_owned(),
            ROOT.replace("homepage = \"https://example.com\"\n", ""),
            "does not define",
        ),
        (
            "member key beside workspace",
            replace(
                "example-plain = { workspace = true }",
                "example-plain = { workspace = true, version = \"1\" }",
            ),
            ROOT.to_owned(),
            "sets version beside workspace = true",
        ),
        (
            "ignored default-features = false",
            replace(
                "example-plain = { workspace = true }",
                "example-plain = { workspace = true, default-features = false }",
            ),
            ROOT.to_owned(),
            "default-features = false",
        ),
        (
            "workspace git dependency",
            MEMBER.to_owned(),
            ROOT.replace(
                "example-plain = \"1\"",
                "example-plain = { git = \"https://example.com/org/plain\" }",
            ),
            "sets git; unsupported",
        ),
        (
            "workspace optional dependency",
            MEMBER.to_owned(),
            ROOT.replace(
                "example-plain = \"1\"",
                "example-plain = { version = \"1\", optional = true }",
            ),
            "sets optional; unsupported",
        ),
        (
            "missing workspace dependency",
            MEMBER.to_owned(),
            ROOT.replace("example-plain = \"1\"\n", ""),
            "which is not defined",
        ),
        (
            "missing workspace lints",
            MEMBER.to_owned(),
            ROOT.split("[workspace.lints.rust]")
                .next()
                .unwrap_or_default()
                .to_owned(),
            "[workspace.lints], which is not defined",
        ),
        (
            "replace section",
            member_with("\n[replace]\n\"example-plain:1.0.0\" = { path = \"x\" }\n"),
            ROOT.to_owned(),
            "declares [replace]",
        ),
        (
            "package workspace pointer",
            replace(
                "name = \"example-core\"",
                "name = \"example-core\"\nworkspace = \"../..\"",
            ),
            ROOT.to_owned(),
            "package.workspace pointers",
        ),
        (
            "legacy dependency spelling",
            member_with("\n[dev_dependencies]\nexample-plain = \"1\"\n"),
            ROOT.to_owned(),
            "legacy spelling",
        ),
        (
            "excluded package",
            MEMBER.to_owned(),
            ROOT.replace("examples/example-demo", "crates/example-core"),
            "excluded from its nearest workspace root",
        ),
        (
            "inherited badges",
            member_with("\n[badges]\nworkspace = true\n"),
            ROOT.to_owned(),
            "still inherits",
        ),
        (
            "other name",
            replace("name = \"example-core\"", "name = \"example-other\""),
            ROOT.to_owned(),
            "does not name package example-core",
        ),
    ];
    for (label, member, root, expected) in cases {
        let message = normalize(&member, "0.4.0", &root_ancestors(&root))
            .expect_err("should refuse the manifest");
        assert!(
            message.contains(expected),
            "should reject {label} with {expected:?}: {message}"
        );
    }
}

#[test]
fn missing_root_and_version_mismatch_fail_closed() {
    let missing = normalize(MEMBER, "0.4.0", &[]).expect_err("should need a workspace root");
    assert!(
        missing.contains("no workspace root encloses it"),
        "should report the missing workspace root: {missing}"
    );
    let plain_only = vec![AncestorManifest {
        directory: String::new(),
        bytes: b"[package]\nname = \"example-top\"\nversion = \"1.0.0\"\n".to_vec(),
    }];
    assert!(
        normalize(MEMBER, "0.4.0", &plain_only)
            .expect_err("should need a [workspace] table")
            .contains("no workspace root encloses it"),
        "should not treat a plain ancestor manifest as a workspace root"
    );
    let mismatch =
        normalize(MEMBER, "0.5.0", &root_ancestors(ROOT)).expect_err("should check the version");
    assert!(
        mismatch.contains("does not declare package example-core 0.5.0"),
        "should refuse an inherited version other than the locked one: {mismatch}"
    );
    let broken = normalize(MEMBER, "0.4.0", &root_ancestors("[workspace"))
        .expect_err("should refuse an unparsable root");
    assert!(
        broken.contains("TOML parse error"),
        "should report an unparsable workspace root: {broken}"
    );
}

#[test]
fn ancestor_pointers_and_non_manifests_are_refused() {
    let nearer = |bytes: &[u8]| {
        vec![
            AncestorManifest {
                directory: "crates".to_owned(),
                bytes: bytes.to_vec(),
            },
            AncestorManifest {
                directory: String::new(),
                bytes: ROOT.as_bytes().to_vec(),
            },
        ]
    };
    let pointer = normalize(
        MEMBER,
        "0.4.0",
        &nearer(
            b"[package]
name = \"example-umbrella\"\nversion = \"9.9.9\"\nworkspace = \"../elsewhere\"\n",
        ),
    )
    .expect_err("should refuse an ancestor that points to another root");
    assert!(
        pointer.contains("points to another workspace root"),
        "should report the workspace pointer instead of skipping it: {pointer}"
    );
    let neither = normalize(MEMBER, "0.4.0", &nearer(b"[profile.release]\nlto = true\n"))
        .expect_err("should refuse an ancestor that is neither package nor workspace");
    assert!(
        neither.contains("neither [package] nor [workspace]"),
        "should report the unrecognized ancestor manifest: {neither}"
    );
}

#[test]
fn matching_disabled_default_features_stay_disabled() {
    let member = MEMBER.replace(
        "example-quiet = { workspace = true, default-features = true }",
        "example-quiet = { workspace = true, default-features = false }",
    );
    let normalized =
        normalize(&member, "0.4.0", &root_ancestors(ROOT)).expect("should accept matching false");
    assert_eq!(
        table(&normalized)["dependencies"]["example-quiet"],
        toml::Value::Table(table(
            b"version = \"3\"\ndefault-features = false\nfeatures = [\"core\"]\n"
        )),
        "should keep default features off when both sides turn them off"
    );
}

#[test]
fn workspace_side_feature_repeats_are_dropped() {
    let root = ROOT.replace(
        "example-featured = { version = \"2\", features = [\"base\"] }",
        "example-featured = { version = \"2\", features = [\"base\", \"extra\", \"base\"] }",
    );
    let normalized =
        normalize(MEMBER, "0.4.0", &root_ancestors(&root)).expect("should normalize the member");
    assert_eq!(
        table(&normalized)["dependencies"]["example-featured"]["features"],
        toml::Value::Array(vec![
            toml::Value::String("base".to_owned()),
            toml::Value::String("extra".to_owned()),
        ]),
        "should keep each feature once, in first-seen order"
    );
}

#[test]
fn target_paths_must_stay_inside_the_package() {
    let manifest = |extra: &str| -> toml::Table {
        table(
            format!("[package]\nname = \"example-core\"\nversion = \"0.4.0\"\n{extra}").as_bytes(),
        )
    };
    for accepted in [
        "",
        "build = false\n",
        "build = \"tools/build.rs\"\n",
        "\n[lib]\npath = \"src/lib.rs\"\n",
        "\n[[bin]]\nname = \"example\"\npath = \"src/bin/example.rs\"\n",
    ] {
        check_target_paths(&manifest(accepted))
            .unwrap_or_else(|error| panic!("should accept {accepted:?}: {error:?}"));
    }
    for refused in [
        "build = \"../build.rs\"\n",
        "\n[lib]\npath = \"../shared/lib.rs\"\n",
        "\n[[bin]]\nname = \"example\"\npath = \"/example/main.rs\"\n",
        "\n[[example]]\nname = \"demo\"\npath = \"examples/../../demo.rs\"\n",
        "\n[[test]]\nname = \"suite\"\npath = \"..\\\\suite.rs\"\n",
        "\n[[bench]]\nname = \"speed\"\npath = \"../benches/speed.rs\"\n",
        "\n[lib]\npath = 7\n",
    ] {
        assert!(
            check_target_paths(&manifest(refused)).is_err(),
            "should refuse {refused:?}"
        );
    }
}
