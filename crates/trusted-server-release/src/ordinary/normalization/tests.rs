use std::ffi::OsString;
use std::fs;
use std::path::Path;

use serde_json::json;

use super::{plan, validate_configuration};

#[test]
fn both_roles_have_identical_four_map_arguments_and_retained_canonical_bytes() {
    let a = plan(
        Path::new("/example/workspace/ordinary-canonical-source"),
        Path::new("/example/temp/ordinary-fastly-canonical-target"),
        "canonical",
    )
    .expect("should plan the closed canonical layout");
    let b = plan(
        Path::new("/example/workspace/ordinary-reproduction-source"),
        Path::new("/example/temp/ordinary-fastly-reproduction-target"),
        "reproduction",
    )
    .expect("should plan the closed reproduction layout");
    assert_eq!(
        a.arguments, b.arguments,
        "should pass identical raw configuration arguments to both builds"
    );
    assert_eq!(
        a.arguments[0], "--config",
        "should merge target flags through Cargo configuration"
    );
    let document: toml::Value =
        toml::from_str(&a.arguments[1]).expect("should encode valid quoted TOML");
    assert_eq!(
        document["target"]["wasm32-wasip1"]["rustflags"]
            .as_array()
            .expect("should list the closed remaps")
            .len(),
        4,
        "should include both source and both target mappings"
    );
    assert_eq!(
        a.bytes,
        crate::documents::canonical_json(&json!(a.arguments), crate::documents::Role::Metadata)
            .expect("should encode the actual argument pair"),
        "should hash exact canonical UTF-8 arguments with LF"
    );
    assert_eq!(
        a.sha256, b.sha256,
        "should retain one shared raw-argument digest within the run"
    );
    assert!(
        a.arguments[1].contains("=/trusted-server"),
        "should reuse the reviewed source prefix"
    );
    assert!(
        a.arguments[1].contains("=/cargo-target"),
        "should reuse the reviewed target prefix"
    );
}

#[test]
fn rejects_wrong_roles_unsafe_prefixes_and_overlapping_layouts() {
    for (source, target, role) in [
        (
            "/example/workspace/source",
            "/example/temp/ordinary-fastly-canonical-target",
            "canonical",
        ),
        (
            "/example/workspace/ordinary-canonical-source",
            "/example/temp/target",
            "canonical",
        ),
        (
            "/example/workspace/ordinary-canonical-source",
            "/example/temp/ordinary-fastly-canonical-target",
            "other",
        ),
        (
            "/example/bad=prefix/ordinary-canonical-source",
            "/example/temp/ordinary-fastly-canonical-target",
            "canonical",
        ),
        (
            "/example/workspace/ordinary-canonical-source",
            "/example/workspace/ordinary-canonical-source/ordinary-fastly-canonical-target",
            "canonical",
        ),
    ] {
        assert!(
            plan(Path::new(source), Path::new(target), role).is_err(),
            "should reject unsupported role/layout or ambiguous remapping text"
        );
    }
}

#[test]
fn preserves_supported_array_flags_and_rejects_competing_or_unhandled_configuration() {
    let temporary = tempfile::tempdir().expect("should create configuration fixture");
    let root = fs::canonicalize(temporary.path()).expect("should resolve configuration fixture");
    fs::create_dir(root.join(".cargo"))
        .expect("should create source Cargo configuration directory");
    let path = root.join(".cargo/config.toml");
    let valid = "[target.wasm32-wasip1]\nrustflags = [\"-Copt-level=3\"]\n";
    fs::write(&path, valid).expect("should retain reviewed array flags");
    validate_configuration(&root, &[]).expect("should preserve configured target arrays");
    assert_eq!(
        fs::read_to_string(&path).expect("should retain source configuration"),
        valid,
        "should never rewrite configured flags"
    );
    for invalid in [
        "include = [\"other.toml\"]\n",
        "[build]\nrustflags=[]\n",
        "[target.wasm32-wasip1]\nrustflags=\"-Copt-level=3\"\n",
        "[target.wasm32-wasip1]\nrustflags=[\"--remap-path-prefix=/a=/b\"]\n",
        "[target.'cfg(unix)']\nrustflags=[\"--remap-path-prefix=/a=/b\"]\n",
        "[env]\nRUSTFLAGS=\"-Copt-level=0\"\n",
    ] {
        fs::write(&path, invalid).expect("should write rejected configuration");
        assert!(
            validate_configuration(&root, &[]).is_err(),
            "should refuse unhandled or competing configured flags"
        );
    }
    fs::write(&path, valid).expect("should restore valid source configuration");
    for name in [
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_BUILD_RUSTFLAGS",
        "CARGO_TARGET_WASM32_WASIP1_RUSTFLAGS",
    ] {
        assert!(
            validate_configuration(&root, &[(OsString::from(name), OsString::new())]).is_err(),
            "should reject any ambient flag presence"
        );
    }
    fs::write(root.join(".cargo/config"), b"")
        .expect("should add an unreviewed extensionless layer");
    assert!(
        validate_configuration(&root, &[]).is_err(),
        "should reject the extensionless source configuration"
    );
}

#[test]
fn rejects_unreviewed_ancestor_and_cargo_home_config_layers() {
    let temporary = tempfile::tempdir().expect("should create configuration parent");
    let parent = fs::canonicalize(temporary.path()).expect("should resolve configuration parent");
    let source = parent.join("project");
    fs::create_dir_all(source.join(".cargo")).expect("should create source configuration root");
    fs::write(source.join(".cargo/config.toml"), b"[alias]\n")
        .expect("should write source configuration");
    let home = parent.join("cargo-home");
    fs::create_dir(&home).expect("should create explicit Cargo home");
    fs::write(home.join("config.toml"), b"[build]\nrustflags=[]\n")
        .expect("should retain an unreviewed layer");
    assert!(
        validate_configuration(
            &source,
            &[(OsString::from("CARGO_HOME"), home.into_os_string())]
        )
        .is_err(),
        "should not silently supersede global flags"
    );
    fs::create_dir(parent.join(".cargo"))
        .expect("should create an ancestor configuration directory");
    fs::write(parent.join(".cargo/config.toml"), b"[alias]\n")
        .expect("should create an inherited layer");
    assert!(
        validate_configuration(&source, &[]).is_err(),
        "should refuse unknown ancestor layers"
    );
}

fn wasm(sections: &[(&str, &[u8])]) -> Vec<u8> {
    let mut bytes = b"\0asm\x01\0\0\0".to_vec();
    for (name, data) in sections {
        assert!(
            name.len() + data.len() + 1 < 128,
            "should keep fixture lengths single-byte"
        );
        bytes.extend([0, (name.len() + data.len() + 1) as u8, name.len() as u8]);
        bytes.extend(name.as_bytes());
        bytes.extend(*data);
    }
    bytes
}

#[test]
fn scan_requires_debug_sections_and_rejects_raw_roots_without_rewriting_bytes() {
    let flags = plan(
        Path::new("/example/workspace/ordinary-canonical-source"),
        Path::new("/example/temp/ordinary-fastly-canonical-target"),
        "canonical",
    )
    .expect("should plan fixed maps");
    let sections: [(&str, &[u8]); 4] = [
        (".debug_info", b"fixture"),
        (".debug_str", b"/trusted-server\0"),
        (".debug_line", b"fixture"),
        ("name", b"fixture"),
    ];
    let valid = wasm(&sections);
    super::scan(&valid, &flags).expect("should retain debug sections and logical source identity");
    let false_prefix = wasm(&[
        sections[0],
        (".debug_str", b"x/trusted-server\0"),
        sections[2],
        sections[3],
    ]);
    assert!(
        super::scan(&false_prefix, &flags).is_err(),
        "should require a complete logical source string rather than a matching substring"
    );
    for invalid in [
        b"\0asm\x01\0\0\0".to_vec(),
        wasm(&sections[..3]),
        wasm(&[
            sections[0],
            sections[0],
            sections[1],
            sections[2],
            sections[3],
        ]),
        wasm(&[(".debug_info", b""), sections[1], sections[2], sections[3]]),
        wasm(&[
            sections[0],
            (".debug_str", b"/unmapped-source\0"),
            sections[2],
            sections[3],
        ]),
    ] {
        assert!(
            super::scan(&invalid, &flags).is_err(),
            "should reject missing, duplicate, empty or unmapped debug identity"
        );
    }
    for prefix in &flags.roots {
        let contaminated = wasm(&[
            sections[0],
            sections[1],
            sections[2],
            sections[3],
            ("raw", format!("{prefix}/src.rs").as_bytes()),
        ]);
        assert!(
            super::scan(&contaminated, &flags).is_err(),
            "should scan all bytes including unrelated custom sections"
        );
    }
    let mut truncated = valid.clone();
    truncated.pop();
    assert!(
        super::scan(&truncated, &flags).is_err(),
        "should reject truncated section data"
    );
}

#[test]
fn preparation_requires_canonical_owned_roots_and_a_fresh_target() {
    let temporary = tempfile::tempdir().expect("should create layout fixture");
    let parent = fs::canonicalize(temporary.path()).expect("should resolve fixture parent");
    let source = parent.join("workspace/ordinary-canonical-source");
    let target = parent.join("temp/ordinary-fastly-canonical-target");
    fs::create_dir_all(source.join(".cargo")).expect("should create source configuration");
    fs::create_dir_all(&target).expect("should create fresh target");
    fs::write(source.join(".cargo/config.toml"), b"[alias]\n")
        .expect("should retain valid source configuration");
    super::prepare(&source, &target, "canonical", &[])
        .expect("should accept fresh canonical layout");
    let output = parent.join("arguments");
    let argument =
        crate::ordinary::commands::prepare_rustflags(&source, &target, "canonical", &output, &[])
            .expect("should seal arguments before the build");
    let expected =
        plan(&source, &target, "canonical").expect("should derive fixed expected arguments");
    assert_eq!(
        argument, expected.arguments[1],
        "should return precisely the retained Cargo config argument"
    );
    assert_eq!(
        fs::read(output.join("rustflags.json")).expect("should retain argument bytes"),
        expected.bytes,
        "should seal canonical argument bytes before Cargo runs"
    );
    assert!(
        crate::ordinary::commands::prepare_rustflags(&source, &target, "canonical", &output, &[])
            .is_err(),
        "should refuse to reuse argument output"
    );
    fs::write(target.join("stale"), b"stale").expect("should contaminate target");
    assert!(
        super::prepare(&source, &target, "canonical", &[]).is_err(),
        "should refuse reused target roots"
    );
    fs::remove_file(target.join("stale")).expect("should restore empty target");
    let peer = parent.join("workspace/ordinary-reproduction-source");
    std::os::unix::fs::symlink(&source, peer).expect("should create forbidden peer alias");
    assert!(
        super::prepare(&source, &target, "canonical", &[]).is_err(),
        "should refuse an existing aliased peer mapping"
    );
}
