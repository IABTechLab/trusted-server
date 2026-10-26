#![cfg(any(target_os = "linux", target_os = "macos"))]
#![allow(
    dead_code,
    reason = "actual build script is included for isolated process fixtures"
)]

#[path = "../build.rs"]
mod build_script;

use std::env;
use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tempfile::{TempDir, tempdir};

const SOURCE: &str = "0123456789abcdef0123456789abcdef01234567";
const BYTES: &[u8] = b"console.log('example');";

struct Fixture {
    root: TempDir,
    inventory: Value,
}

impl Fixture {
    fn new() -> Self {
        let root = tempdir().expect("should create fixture root");
        for name in ["lib/src/integrations", "bundles", "out", "bin"] {
            fs::create_dir_all(root.path().join(name)).expect("should create fixture directory");
        }
        fs::write(root.path().join("bundles/tsjs-core.js"), BYTES)
            .expect("should write core bundle");
        let npm = root.path().join("bin/npm");
        fs::write(
            &npm,
            "#!/bin/sh\n: > \"$CARGO_MANIFEST_DIR/npm-called\"\nexit 97\n",
        )
        .expect("should write failing npm");
        fs::set_permissions(npm, fs::Permissions::from_mode(0o755))
            .expect("should make npm executable");
        Self {
            root,
            inventory: json!({
                "schema_version": 1, "source_commit": SOURCE,
                "modules": [{ "id": "core", "filename": "tsjs-core.js", "size": BYTES.len(),
                    "sha256": hex::encode(Sha256::digest(BYTES)), "contributions": [] }],
                "tools": [], "materials": []
            }),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env::current_exe().expect("should locate test executable"));
        command
            .args(["--exact", "fixture_entry", "--ignored", "--nocapture"])
            .env_clear()
            .env("CARGO_MANIFEST_DIR", self.root.path())
            .env("OUT_DIR", self.path("out"))
            .env("PATH", self.path("bin"))
            .env("TRUSTED_SERVER__TSJS_TEST_SUBPROCESS", "1")
            .env("TRUSTED_SERVER__RELEASE_BUILD", "1")
            .env("TRUSTED_SERVER__GIT_VERSION", SOURCE)
            .env("TSJS_PREBUILT_DIR", self.path("bundles"))
            .env(
                "TRUSTED_SERVER__TSJS_PREBUILT_INVENTORY",
                self.path("inventory.json"),
            );
        command
    }

    fn write_inventory(&self) {
        fs::write(
            self.path("inventory.json"),
            serde_json::to_vec(&self.inventory).expect("should serialize inventory"),
        )
        .expect("should write inventory");
    }

    fn rejected(&self, output: &Output, diagnostic: &str) {
        let output_text = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "should reject fixture: {output_text}"
        );
        assert!(
            output_text.contains(diagnostic),
            "should report {diagnostic}: {output_text}"
        );
        assert!(
            !self.path("npm-called").exists(),
            "should reject before invoking npm"
        );
        assert!(
            !self.path("out/tsjs_modules.rs").exists(),
            "should not generate a rejected module table"
        );
    }

    fn add_integration(&mut self, id: &str) {
        fs::create_dir(self.path(&format!("lib/src/integrations/{id}")))
            .expect("should create integration");
        fs::write(
            self.path(&format!("lib/src/integrations/{id}/index.ts")),
            b"export {};",
        )
        .expect("should write integration entry");
        fs::write(self.path(&format!("bundles/tsjs-{id}.js")), BYTES)
            .expect("should write integration bundle");
        let mut module = self.inventory["modules"][0].clone();
        module["id"] = json!(id);
        module["filename"] = json!(format!("tsjs-{id}.js"));
        self.inventory["modules"]
            .as_array_mut()
            .expect("should access modules")
            .push(module);
        self.inventory["modules"]
            .as_array_mut()
            .expect("should access modules")
            .sort_by(|left, right| left["id"].as_str().cmp(&right["id"].as_str()));
    }
}

#[test]
fn rejects_reversed_complete_two_module_inventory() {
    let mut fixture = Fixture::new();
    fixture.add_integration("aaa");
    fixture.inventory["modules"]
        .as_array_mut()
        .expect("should access modules")
        .reverse();
    fixture.write_inventory();
    fixture.rejected(
        &fixture.command().output().expect("should execute fixture"),
        "inventory module identity, order, size or digest is invalid",
    );
}

#[test]
fn rejects_unsafe_and_duplicate_core_source_integrations() {
    for id in ["unsafe\"name", "core"] {
        let mut fixture = Fixture::new();
        fixture.add_integration(id);
        fixture.write_inventory();
        fixture.rejected(
            &fixture.command().output().expect("should execute fixture"),
            if id == "core" {
                "duplicate core module ID"
            } else {
                "module ID unsafe\"name must be a safe ASCII identifier"
            },
        );
    }
}

#[test]
fn accepts_safe_opaque_floats_and_negative_integers() {
    let mut fixture = Fixture::new();
    fixture.inventory["tools"] = json!([
        0.5,
        1.5e-10,
        -1,
        -9_007_199_254_740_991_i64,
        9_007_199_254_740_991_u64
    ]);
    fixture.inventory["materials"] = json!([{"nested": [1.0, -0.0]}]);
    fixture.write_inventory();
    let output = fixture.command().output().expect("should execute fixture");
    assert!(
        output.status.success(),
        "should accept safe opaque numeric metadata: {output:?}"
    );
}

#[test]
#[ignore = "entry point invoked only by fixture subprocesses"]
fn fixture_entry() {
    if env::var_os("TRUSTED_SERVER__TSJS_TEST_SUBPROCESS").as_deref()
        != Some(std::ffi::OsStr::new("1"))
    {
        return;
    }
    build_script::run_with_hook(|source, copied| {
        match env::var("TRUSTED_SERVER__TSJS_TEST_MUTATION").as_deref() {
            Ok("source") => fs::write(source.join("tsjs-core.js"), b"changed bytes")
                .expect("should mutate source"),
            Ok("swap") => {
                fs::remove_file(source.join("tsjs-core.js")).expect("should remove source");
                symlink(copied.join("tsjs-core.js"), source.join("tsjs-core.js"))
                    .expect("should swap source to link");
            }
            Ok("private") => fs::write(copied.join("tsjs-core.js"), b"changed bytes")
                .expect("should mutate copied bytes"),
            Ok("extra") => {
                fs::write(source.join("notes.txt"), b"extra").expect("should add source name")
            }
            Ok("identity") => {
                fs::write(source.join("replacement.js"), BYTES)
                    .expect("should write replacement bytes");
                fs::rename(source.join("replacement.js"), source.join("tsjs-core.js"))
                    .expect("should replace source identity");
            }
            Ok("directory") => {
                fs::rename(source, source.with_extension("original"))
                    .expect("should move source directory");
                fs::create_dir(source).expect("should replace source directory");
                fs::write(source.join("tsjs-core.js"), BYTES)
                    .expect("should replace identical source bytes");
            }
            Ok("parent") => {
                let parent = source.parent().expect("should have source parent");
                let root = parent.parent().expect("should have fixture root");
                fs::remove_file(parent).expect("should remove parent link");
                symlink(root.join("other"), parent).expect("should redirect parent link");
            }
            _ => {}
        }
    });
}

#[test]
fn release_accepts_checked_bundles_without_npm() {
    let fixture = Fixture::new();
    fixture.write_inventory();
    let output = fixture.command().output().expect("should execute fixture");
    assert!(
        output.status.success(),
        "should accept checked bundles: {output:?}"
    );
    assert_eq!(
        fs::read(fixture.path("out/tsjs-dist/tsjs-core.js")).expect("should read copied bundle"),
        BYTES,
        "should copy authenticated bytes"
    );
    assert!(
        !fixture.path("npm-called").exists(),
        "should never invoke npm in checked-prebuilt release mode"
    );
}

#[test]
fn ordinary_release_without_prebuilt_builds_private_bundles() {
    let fixture = Fixture::new();
    fs::write(fixture.path("lib/package.json"), b"{}").expect("should write package manifest");
    fs::write(fixture.path("lib/package-lock.json"), b"{}").expect("should write package lock");
    fs::create_dir(fixture.path("lib/node_modules")).expect("should create installed dependencies");
    fs::write(fixture.path("lib/node_modules/.package-lock.json"), b"{}")
        .expect("should write fresh installation marker");
    fs::write(
        fixture.path("bin/npm"),
        r#"#!/bin/sh
[ "$1" = run ] && [ "$2" = build ] && [ "$3" = -- ] && [ "$4" = --out-dir ] || exit 96
/bin/mkdir -p "$5"
printf '%s' 'console.log("ordinary");' > "$5/tsjs-core.js"
: > "$CARGO_MANIFEST_DIR/npm-called"
"#,
    )
    .expect("should write the isolated npm build fixture");
    let output = fixture
        .command()
        .env_remove("TSJS_PREBUILT_DIR")
        .env_remove("TRUSTED_SERVER__TSJS_PREBUILT_INVENTORY")
        .output()
        .expect("should execute the ordinary release fixture");
    assert!(
        output.status.success(),
        "should accept a fresh ordinary release build: {output:?}"
    );
    assert!(
        fixture.path("npm-called").exists(),
        "should build instead of copying a prebuilt folder"
    );
    assert_eq!(
        fs::read(fixture.path("out/tsjs-dist/tsjs-core.js")).expect("should read private output"),
        b"console.log(\"ordinary\");",
        "should retain fresh npm bytes in the private output"
    );
    assert!(
        fixture.path("out/tsjs_modules.rs").is_file(),
        "should generate the module table from private output"
    );
}

#[test]
fn ignored_entry_without_explicit_subprocess_marker_does_not_write() {
    let fixture = Fixture::new();
    fixture.write_inventory();
    let output = fixture
        .command()
        .env_remove("TRUSTED_SERVER__TSJS_TEST_SUBPROCESS")
        .output()
        .expect("should invoke ignored entry directly");
    assert!(
        output.status.success(),
        "should leave the direct ignored entry inert: {output:?}"
    );
    assert!(
        !fixture.path("out/tsjs_modules.rs").exists(),
        "should never write without explicit marker"
    );
}

#[test]
fn rejects_any_skip_presence_before_prebuilt_selection() {
    for value in [
        OsString::from(""),
        OsString::from("0"),
        OsString::from("1"),
        OsString::from_vec(vec![0xff]),
    ] {
        let fixture = Fixture::new();
        fixture.write_inventory();
        let output = fixture
            .command()
            .env("TSJS_SKIP_BUILD", value)
            .output()
            .expect("should execute fixture");
        fixture.rejected(&output, "TSJS_SKIP_BUILD");
    }
}

#[test]
fn release_requires_both_prebuilt_inputs() {
    for missing in [
        "TSJS_PREBUILT_DIR",
        "TRUSTED_SERVER__TSJS_PREBUILT_INVENTORY",
    ] {
        let fixture = Fixture::new();
        fixture.write_inventory();
        let output = fixture
            .command()
            .env_remove(missing)
            .output()
            .expect("should execute fixture");
        fixture.rejected(&output, missing);
    }
}

#[test]
fn rejects_inventory_identity_and_module_tampering() {
    for case in [
        "source",
        "digest",
        "size",
        "duplicate",
        "filename",
        "id",
        "missing",
        "empty",
        "extra",
        "unknown",
        "version",
        "tools",
        "bool_size",
        "float_size",
        "negative_size",
        "module_unknown",
        "missing_array",
        "size_bound",
        "contributions",
    ] {
        let mut fixture = Fixture::new();
        match case {
            "source" => fixture.inventory["source_commit"] = json!("a".repeat(40)),
            "digest" => fixture.inventory["modules"][0]["sha256"] = json!("a".repeat(64)),
            "size" => fixture.inventory["modules"][0]["size"] = json!(BYTES.len() + 1),
            "duplicate" => {
                let module = fixture.inventory["modules"][0].clone();
                fixture.inventory["modules"]
                    .as_array_mut()
                    .expect("should access modules")
                    .push(module);
            }
            "filename" => fixture.inventory["modules"][0]["filename"] = json!("../tsjs-core.js"),
            "id" => fixture.inventory["modules"][0]["id"] = json!("../core"),
            "missing" => fixture.inventory["modules"] = json!([]),
            "empty" => fixture.inventory["modules"][0]["size"] = json!(0),
            "extra" => {
                let mut module = fixture.inventory["modules"][0].clone();
                module["id"] = json!("aaa");
                module["filename"] = json!("tsjs-aaa.js");
                fixture.inventory["modules"]
                    .as_array_mut()
                    .expect("should access modules")
                    .push(module);
            }
            "unknown" => fixture.inventory["unknown"] = json!(1),
            "version" => fixture.inventory["schema_version"] = json!(true),
            "tools" => fixture.inventory["tools"] = json!({}),
            "bool_size" => fixture.inventory["modules"][0]["size"] = json!(true),
            "float_size" => fixture.inventory["modules"][0]["size"] = json!(1.0),
            "negative_size" => fixture.inventory["modules"][0]["size"] = json!(-1),
            "module_unknown" => fixture.inventory["modules"][0]["unknown"] = json!(1),
            "missing_array" => {
                fixture
                    .inventory
                    .as_object_mut()
                    .expect("should access inventory")
                    .remove("materials");
            }
            "size_bound" => fixture.inventory["modules"][0]["size"] = json!(32 * 1024 * 1024 + 1),
            "contributions" => fixture.inventory["modules"][0]["contributions"] = json!({}),
            _ => unreachable!("should use listed case"),
        }
        fixture.write_inventory();
        fixture.rejected(
            &fixture.command().output().expect("should execute fixture"),
            match case {
                "source" => "inventory schema version or source does not match",
                "digest" => "inventory bundle digest mismatch",
                "size" => "inventory bundle size mismatch",
                "missing" => "missing bundles",
                "unknown" | "module_unknown" => "unknown field",
                "missing_array" => "missing field",
                "version" | "tools" | "bool_size" | "float_size" | "contributions" => {
                    "invalid type"
                }
                "negative_size" => "invalid value",
                _ => "inventory module identity, order, size or digest is invalid",
            },
        );
    }
}

#[test]
fn rejects_inexact_source_directories() {
    for case in [
        "missing",
        "empty",
        "extra",
        "unrelated",
        "directory",
        "link",
        "hardlink",
    ] {
        let fixture = Fixture::new();
        fixture.write_inventory();
        match case {
            "missing" => {
                fs::remove_file(fixture.path("bundles/tsjs-core.js")).expect("should remove bundle")
            }
            "empty" => {
                fs::write(fixture.path("bundles/tsjs-core.js"), []).expect("should empty bundle")
            }
            "extra" | "unrelated" => fs::write(
                fixture.path(if case == "extra" {
                    "bundles/tsjs-extra.js"
                } else {
                    "bundles/notes.txt"
                }),
                BYTES,
            )
            .expect("should add file"),
            "directory" => {
                fs::create_dir(fixture.path("bundles/extra")).expect("should add directory")
            }
            "link" => {
                fs::remove_file(fixture.path("bundles/tsjs-core.js"))
                    .expect("should remove bundle");
                symlink(
                    fixture.path("inventory.json"),
                    fixture.path("bundles/tsjs-core.js"),
                )
                .expect("should link bundle");
            }
            "hardlink" => fs::hard_link(
                fixture.path("bundles/tsjs-core.js"),
                fixture.path("alias.js"),
            )
            .expect("should hardlink bundle"),
            _ => unreachable!("should use listed case"),
        }
        fixture.rejected(
            &fixture.command().output().expect("should execute fixture"),
            match case {
                "missing" => "missing bundles",
                "empty" => "empty bundles",
                "extra" | "unrelated" | "directory" => "unexpected extra bundle directory entry",
                "link" => "cannot open bundle",
                "hardlink" => "exactly one hard link",
                _ => unreachable!("should use listed case"),
            },
        );
    }
}

#[test]
fn rejects_ambiguous_or_unbounded_json() {
    for case in [
        "duplicate",
        "nested",
        "nonfinite",
        "overflow",
        "integer_overflow",
        "depth",
        "oversize",
        "bom",
        "utf8",
    ] {
        let fixture = Fixture::new();
        let valid = serde_json::to_string(&fixture.inventory).expect("should serialize fixture");
        let raw = match case {
            "duplicate" => valid
                .replacen("{", "{\"schema_version\":1,", 1)
                .into_bytes(),
            "nested" => valid
                .replace("\"tools\":[]", "\"tools\":[{\"a\":1,\"a\":2}]")
                .into_bytes(),
            "nonfinite" => valid
                .replace("\"tools\":[]", "\"tools\":[NaN]")
                .into_bytes(),
            "overflow" => valid
                .replace("\"tools\":[]", "\"tools\":[1e9999]")
                .into_bytes(),
            "integer_overflow" => valid
                .replace("\"tools\":[]", "\"tools\":[18446744073709551616]")
                .into_bytes(),
            "depth" => valid
                .replace(
                    "\"tools\":[]",
                    &format!("\"tools\":[{}0{}]", "[".repeat(65), "]".repeat(65)),
                )
                .into_bytes(),
            "oversize" => vec![b' '; 1024 * 1024 + 1],
            "bom" => [vec![0xef, 0xbb, 0xbf], valid.into_bytes()].concat(),
            "utf8" => vec![0xff],
            _ => unreachable!("should use listed case"),
        };
        fs::write(fixture.path("inventory.json"), raw).expect("should write malformed inventory");
        fixture.rejected(
            &fixture.command().output().expect("should execute fixture"),
            match case {
                "duplicate" | "nested" => "duplicate JSON key",
                "nonfinite" | "bom" => "expected value",
                "overflow" => "inventory number overflows",
                "integer_overflow" => "unsafe integer",
                "depth" => "JSON depth exceeds 64",
                "oversize" => "inventory exceeds 1 MiB",
                "utf8" => "inventory must be UTF-8",
                _ => unreachable!("should use listed case"),
            },
        );
    }
}

#[test]
fn development_prebuilt_without_inventory_remains_supported() {
    let fixture = Fixture::new();
    fs::write(fixture.path("bundles/manifest.json"), b"{}").expect("should write legacy manifest");
    fs::write(fixture.path("bundles/.DS_Store"), b"metadata")
        .expect("should write legacy metadata");
    fs::create_dir(fixture.path("bundles/prebid")).expect("should create legacy tree");
    fs::write(fixture.path("bundles/prebid/package.json"), b"{}")
        .expect("should write legacy package output");
    fs::write(fixture.path("bundles/prebid/prebid.js"), BYTES)
        .expect("should write legacy external bundle output");
    symlink(
        fixture.path("bundles/prebid"),
        fixture.path("bundles/legacy-link"),
    )
    .expect("should create legacy directory alias");
    fs::hard_link(
        fixture.path("bundles/tsjs-core.js"),
        fixture.path("alias.js"),
    )
    .expect("should retain legacy bundle hard links");
    let output = fixture
        .command()
        .env_remove("TRUSTED_SERVER__RELEASE_BUILD")
        .env_remove("TRUSTED_SERVER__TSJS_PREBUILT_INVENTORY")
        .output()
        .expect("should execute fixture");
    assert!(
        output.status.success(),
        "should support development prebuilt bundles: {output:?}"
    );
}

#[test]
fn rejects_source_or_copied_bytes_changed_during_copy() {
    for mutation in [
        "source",
        "swap",
        "private",
        "extra",
        "identity",
        "directory",
    ] {
        let fixture = Fixture::new();
        fixture.write_inventory();
        let output = fixture
            .command()
            .env("TRUSTED_SERVER__TSJS_TEST_MUTATION", mutation)
            .output()
            .expect("should execute fixture");
        fixture.rejected(
            &output,
            match mutation {
                "source" => "changed during copy",
                "swap" => "cannot open bundle",
                "private" => "private copied bundle does not match",
                "extra" => "unexpected extra bundle directory entry",
                "identity" | "directory" => "identity changed during copy",
                _ => unreachable!("should use listed mutation"),
            },
        );
    }
}

#[test]
fn rejects_release_controls_and_relative_paths_before_npm() {
    for (variable, value) in [
        ("TRUSTED_SERVER__RELEASE_BUILD", ""),
        ("TRUSTED_SERVER__RELEASE_BUILD", "0"),
        ("TRUSTED_SERVER__RELEASE_BUILD", "true"),
        ("TRUSTED_SERVER__GIT_VERSION", ""),
        ("TRUSTED_SERVER__GIT_VERSION", "abcdef"),
        ("TSJS_PREBUILT_DIR", "bundles"),
        ("TRUSTED_SERVER__TSJS_PREBUILT_INVENTORY", "inventory.json"),
    ] {
        let fixture = Fixture::new();
        fixture.write_inventory();
        fixture.rejected(
            &fixture
                .command()
                .env(variable, value)
                .output()
                .expect("should execute fixture"),
            match variable {
                "TRUSTED_SERVER__RELEASE_BUILD" => {
                    "TRUSTED_SERVER__RELEASE_BUILD must be exactly 1"
                }
                "TRUSTED_SERVER__GIT_VERSION" if value.is_empty() => {
                    "release source must be exactly 40"
                }
                "TRUSTED_SERVER__GIT_VERSION" => "release source must be exactly 40",
                _ => "must be an absolute path",
            },
        );
    }
    let fixture = Fixture::new();
    fixture.write_inventory();
    fixture.rejected(
        &fixture
            .command()
            .env_remove("TRUSTED_SERVER__GIT_VERSION")
            .output()
            .expect("should execute fixture"),
        "TRUSTED_SERVER__GIT_VERSION",
    );
}

#[test]
fn development_inventory_checks_bundle_bytes_without_comparing_display_source() {
    for identity in ["main", "v1.2.3", "a".repeat(40).as_str()] {
        let fixture = Fixture::new();
        fixture.write_inventory();
        let output = fixture
            .command()
            .env_remove("TRUSTED_SERVER__RELEASE_BUILD")
            .env("TRUSTED_SERVER__GIT_VERSION", identity)
            .output()
            .expect("should execute development fixture");
        assert!(
            output.status.success(),
            "should permit development display identity: {output:?}"
        );
    }
    {
        let fixture = Fixture::new();
        fixture.write_inventory();
        let mut command = fixture.command();
        command.env_remove("TRUSTED_SERVER__RELEASE_BUILD");
        fs::write(fixture.path("bundles/tsjs-core.js"), b"tampered bytes")
            .expect("should change bundle");
        fixture.rejected(
            &command.output().expect("should execute fixture"),
            "inventory bundle size mismatch",
        );
    }
    let fixture = Fixture::new();
    fixture.write_inventory();
    let output = fixture
        .command()
        .env_remove("TRUSTED_SERVER__RELEASE_BUILD")
        .env_remove("TRUSTED_SERVER__GIT_VERSION")
        .output()
        .expect("should execute fixture");
    assert!(
        output.status.success(),
        "should validate development inventory without claiming approved source: {output:?}"
    );
}

#[test]
fn rejects_skip_presence_in_development_source_and_prebuilt_modes() {
    for prebuilt in [true, false] {
        for value in ["", "0", "1"] {
            let fixture = Fixture::new();
            let mut command = fixture.command();
            command
                .env_remove("TRUSTED_SERVER__RELEASE_BUILD")
                .env_remove("TRUSTED_SERVER__TSJS_PREBUILT_INVENTORY")
                .env("TSJS_SKIP_BUILD", value);
            if !prebuilt {
                command.env_remove("TSJS_PREBUILT_DIR");
            }
            fixture.rejected(
                &command.output().expect("should execute fixture"),
                "TSJS_SKIP_BUILD",
            );
        }
    }
}

#[test]
fn development_source_build_uses_fresh_npm_output() {
    let fixture = Fixture::new();
    fs::write(fixture.path("lib/package.json"), json!({}).to_string())
        .expect("should write package fixture");
    fs::create_dir(fixture.path("lib/node_modules"))
        .expect("should create installed dependencies fixture");
    fs::create_dir(fixture.path("dist")).expect("should create stale old dist");
    fs::write(fixture.path("dist/tsjs-core.js"), b"stale bytes")
        .expect("should write stale bundle");
    fs::write(fixture.path("bin/npm"), "#!/bin/sh\n: > \"$CARGO_MANIFEST_DIR/npm-called\"\nfor argument in \"$@\"; do output=\"$argument\"; done\n/bin/mkdir -p \"$output\"\nprintf 'fresh source bytes' > \"$output/tsjs-core.js\"\n").expect("should write source builder fixture");
    let output = fixture
        .command()
        .env_remove("TRUSTED_SERVER__RELEASE_BUILD")
        .env_remove("TSJS_PREBUILT_DIR")
        .env_remove("TRUSTED_SERVER__TSJS_PREBUILT_INVENTORY")
        .output()
        .expect("should execute fixture");
    assert!(
        output.status.success(),
        "should support ordinary fresh development build: {output:?}"
    );
    assert_eq!(
        fs::read(fixture.path("out/tsjs-dist/tsjs-core.js")).expect("should read fresh bundle"),
        b"fresh source bytes",
        "should embed current npm output"
    );
    assert!(
        fixture.path("npm-called").exists(),
        "should invoke npm for development source build"
    );
}

#[test]
fn inventory_sorting_preserves_core_first_runtime_order() {
    let mut fixture = Fixture::new();
    fs::create_dir(fixture.path("lib/src/integrations/aaa")).expect("should create integration");
    fs::write(
        fixture.path("lib/src/integrations/aaa/index.ts"),
        b"export {};",
    )
    .expect("should write source entry");
    fs::write(fixture.path("bundles/tsjs-aaa.js"), BYTES).expect("should write integration bundle");
    let mut module = fixture.inventory["modules"][0].clone();
    module["id"] = json!("aaa");
    module["filename"] = json!("tsjs-aaa.js");
    fixture.inventory["modules"]
        .as_array_mut()
        .expect("should access modules")
        .insert(0, module);
    fixture.write_inventory();
    let output = fixture.command().output().expect("should execute fixture");
    assert!(
        output.status.success(),
        "should accept sorted inventory independently of runtime order: {output:?}"
    );
    let generated = fs::read_to_string(fixture.path("out/tsjs_modules.rs"))
        .expect("should read generated code");
    assert!(
        generated.find("id: \"core\"") < generated.find("id: \"aaa\""),
        "should retain core first runtime order"
    );
}

#[test]
fn rejects_inventory_inside_source_and_private_destination_links() {
    for case in ["inside", "link", "file"] {
        let fixture = Fixture::new();
        fixture.write_inventory();
        let mut command = fixture.command();
        match case {
            "inside" => {
                fs::copy(
                    fixture.path("inventory.json"),
                    fixture.path("bundles/inventory.json"),
                )
                .expect("should place inventory inside source");
                command.env(
                    "TRUSTED_SERVER__TSJS_PREBUILT_INVENTORY",
                    fixture.path("bundles/inventory.json"),
                );
            }
            "link" => symlink(fixture.path("bundles"), fixture.path("out/tsjs-dist"))
                .expect("should link private destination"),
            "file" => fs::write(fixture.path("out/tsjs-dist"), b"existing file")
                .expect("should write conflicting destination"),
            _ => unreachable!("should use listed case"),
        }
        fixture.rejected(
            &command.output().expect("should execute fixture"),
            match case {
                "inside" => "inventory must be outside",
                "link" | "file" => "private bundle destination must be a directory",
                _ => unreachable!("should use listed case"),
            },
        );
        assert_eq!(
            fs::read(fixture.path("bundles/tsjs-core.js")).expect("should preserve input bundle"),
            BYTES,
            "should preserve source when rejecting destination"
        );
    }
}

#[test]
fn emits_rerun_directives_for_inventory_and_release_inputs() {
    let fixture = Fixture::new();
    fixture.write_inventory();
    let output = fixture.command().output().expect("should execute fixture");
    assert!(output.status.success(), "should accept checked fixture");
    let stdout = String::from_utf8(output.stdout).expect("should capture UTF-8 directives");
    for variable in [
        "TSJS_SKIP_BUILD",
        "TSJS_PREBUILT_DIR",
        "TRUSTED_SERVER__TSJS_PREBUILT_INVENTORY",
        "TRUSTED_SERVER__RELEASE_BUILD",
        "TRUSTED_SERVER__GIT_VERSION",
    ] {
        assert!(
            stdout.contains(&format!("cargo:rerun-if-env-changed={variable}")),
            "should watch {variable}"
        );
    }
    for path in [
        "lib/build-module-options.mjs",
        "lib/release-composition.mjs",
        "lib/vitest.config.ts",
    ] {
        assert!(
            stdout.contains(&format!("cargo:rerun-if-changed={path}")),
            "should watch JS build and test configuration input {path}"
        );
    }
    for path in [fixture.path("inventory.json"), fixture.path("bundles")] {
        assert!(
            stdout.contains(&format!("cargo:rerun-if-changed={}", path.display())),
            "should watch prebuilt input path"
        );
    }
}

#[test]
fn exposes_raw_inventory_digest_for_later_cargo_build_evidence() {
    let fixture = Fixture::new();
    let mut raw = serde_json::to_vec(&fixture.inventory).expect("should serialize inventory");
    raw.extend_from_slice(b"\n \n");
    fs::write(fixture.path("inventory.json"), &raw).expect("should write raw inventory");

    let output = fixture.command().output().expect("should execute fixture");

    assert!(
        output.status.success(),
        "should accept whitespace-preserving inventory"
    );
    let stdout = String::from_utf8(output.stdout).expect("should capture UTF-8 directives");
    assert!(
        stdout.contains(&format!(
            "cargo:rustc-env=TRUSTED_SERVER__TSJS_INVENTORY_SHA256={}",
            hex::encode(Sha256::digest(&raw))
        )),
        "should report digest of the exact held inventory bytes: {stdout}"
    );
}

#[test]
fn development_inventory_does_not_compare_non_unicode_display_source() {
    let fixture = Fixture::new();
    fixture.write_inventory();

    let output = fixture
        .command()
        .env_remove("TRUSTED_SERVER__RELEASE_BUILD")
        .env(
            "TRUSTED_SERVER__GIT_VERSION",
            OsString::from_vec(vec![0xff]),
        )
        .output()
        .expect("should execute fixture");

    assert!(
        output.status.success(),
        "should leave development identity resolution to core: {output:?}"
    );
}

#[test]
fn rejects_source_leaf_links_with_lexical_directory_suffixes() {
    for suffix in ["", "/", "//", "/.", "/./"] {
        let fixture = Fixture::new();
        fixture.write_inventory();
        symlink(fixture.path("bundles"), fixture.path("bundles-link"))
            .expect("should link source directory");
        let output = fixture
            .command()
            .env(
                "TSJS_PREBUILT_DIR",
                format!("{}{suffix}", fixture.path("bundles-link").display()),
            )
            .output()
            .expect("should execute linked source fixture");
        fixture.rejected(&output, "bundles-link");
    }
}

#[test]
fn rejects_unsafe_integral_opaque_numbers() {
    for literal in ["9007199254740993", "-9007199254740993", "1e20", "1.5e300"] {
        for field in ["tools", "materials", "contributions"] {
            let fixture = Fixture::new();
            let raw = serde_json::to_string(&fixture.inventory)
                .expect("should serialize inventory")
                .replace(
                    &format!("\"{field}\":[]"),
                    &format!("\"{field}\":[{{\"nested\":{literal}}}]"),
                );
            fs::write(fixture.path("inventory.json"), raw).expect("should write opaque number");
            fixture.rejected(
                &fixture.command().output().expect("should execute fixture"),
                "unsafe integer",
            );
        }
    }
}

#[test]
fn permits_exact_inventory_size_and_depth_boundaries() {
    let mut fixture = Fixture::new();
    let mut value = json!(0);
    for _ in 0..62 {
        value = json!([value]);
    }
    fixture.inventory["tools"] = json!([value]);
    let mut raw =
        serde_json::to_vec(&fixture.inventory).expect("should serialize boundary inventory");
    raw.resize(1024 * 1024, b' ');
    fs::write(fixture.path("inventory.json"), raw).expect("should write boundary inventory");

    let output = fixture.command().output().expect("should execute fixture");

    assert!(
        output.status.success(),
        "should accept inventory at byte and container-depth limits: {output:?}"
    );
}

#[test]
fn permits_parent_links_when_the_held_source_identity_stays_fixed() {
    let fixture = Fixture::new();
    fixture.write_inventory();
    symlink(fixture.root.path(), fixture.path("parent-alias")).expect("should create parent alias");

    let output = fixture
        .command()
        .env("TSJS_PREBUILT_DIR", fixture.path("parent-alias/bundles"))
        .output()
        .expect("should execute fixture");

    assert!(
        output.status.success(),
        "should anchor copying to unchanged directory identity through a parent alias: {output:?}"
    );
}

#[test]
fn rejects_parent_link_redirection_to_byte_identical_directory() {
    let fixture = Fixture::new();
    fixture.write_inventory();
    symlink(fixture.root.path(), fixture.path("parent-alias")).expect("should create parent alias");
    fs::create_dir_all(fixture.path("other/bundles"))
        .expect("should create other source directory");
    fs::write(fixture.path("other/bundles/tsjs-core.js"), BYTES)
        .expect("should write identical other bytes");

    let output = fixture
        .command()
        .env("TSJS_PREBUILT_DIR", fixture.path("parent-alias/bundles"))
        .env("TRUSTED_SERVER__TSJS_TEST_MUTATION", "parent")
        .output()
        .expect("should execute fixture");

    fixture.rejected(&output, "identity changed");
}
