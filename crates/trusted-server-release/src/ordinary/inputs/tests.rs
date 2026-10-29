use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::{javascript_identity, source_identity};

pub(in crate::ordinary) struct Fixture {
    _temporary: tempfile::TempDir,
    pub(in crate::ordinary) checkout: PathBuf,
    pub(super) target: PathBuf,
}

impl Fixture {
    pub(in crate::ordinary) fn new() -> Self {
        let temporary = tempfile::tempdir().expect("should create source fixture");
        let outer = fs::canonicalize(temporary.path()).expect("should resolve fixture root");
        let checkout = outer.join("workspace/ordinary-canonical-source");
        let target = outer.join("temp/ordinary-fastly-canonical-target");
        fs::create_dir_all(&checkout).expect("should create source root");
        fs::create_dir_all(&target).expect("should create target root");
        for file in [
            "scripts/release/phase1-build.sh",
            ".cargo/config.toml",
            "Cargo.toml",
            "rust-toolchain.toml",
            ".tool-versions",
            "Cargo.lock",
            "crates/trusted-server-js/lib/package-lock.json",
        ] {
            let path = checkout.join(file);
            fs::create_dir_all(path.parent().expect("should have a parent"))
                .expect("should create fixed input directory");
            fs::write(path, b"example source input\n").expect("should write fixed input");
        }
        fs::write(checkout.join(".cargo/config.toml"), b"[alias]\n")
            .expect("should retain valid Cargo configuration");
        Self {
            _temporary: temporary,
            checkout,
            target,
        }
    }

    pub(super) fn private_js(&self) -> (Value, Vec<Value>) {
        let js = self.checkout.join("crates/trusted-server-js");
        fs::write(js.join("Cargo.toml"), b"example package").expect("should write JS manifest");
        for module in ["core", "integrations/example"] {
            let path = js.join("lib/src").join(module);
            fs::create_dir_all(&path).expect("should create module directory");
            fs::write(path.join("index.ts"), b"export {};").expect("should write module entry");
        }
        fs::create_dir_all(self.output()).expect("should create private JS output");
        for file in ["tsjs-core.js", "tsjs-example.js"] {
            fs::write(self.output().join(file), file.as_bytes())
                .expect("should write distinct bundle bytes");
        }
        let id = format!("path+file://{}#0.1.0", js.display());
        let metadata = json!({"workspace_root": self.checkout, "workspace_members": [id], "packages":[{
            "id":id,"name":"trusted-server-js","manifest_path":js.join("Cargo.toml")
        }]});
        let messages = vec![
            json!({"reason":"build-script-executed","package_id":id,"out_dir":self.output().parent().expect("should have output parent"),"env":[]}),
            json!({"reason":"build-finished","success":true}),
        ];
        (metadata, messages)
    }

    fn output(&self) -> PathBuf {
        self.target
            .join("wasm32-wasip1/release/build/trusted-server-js-example/out/tsjs-dist")
    }

    fn js_identity(
        &self,
        metadata: &Value,
        messages: &[Value],
    ) -> Result<String, error_stack::Report<crate::error::ReleaseError>> {
        let metadata = serde_json::to_vec(metadata).expect("should encode Cargo metadata");
        let messages: Vec<u8> = messages
            .iter()
            .flat_map(|message| {
                let mut bytes = serde_json::to_vec(message).expect("should encode Cargo message");
                bytes.push(b'\n');
                bytes
            })
            .collect();
        javascript_identity(&self.checkout, &self.target, &metadata, &messages)
    }
}

#[test]
fn source_recipe_identity_is_canonical_and_independent_of_runner_roots() {
    let first = Fixture::new();
    let second = Fixture::new();
    let identity = source_identity(&first.checkout, &"a".repeat(40))
        .expect("should measure fixed source inputs");
    assert_eq!(
        identity,
        source_identity(&second.checkout, &"a".repeat(40)).expect("should measure the other root"),
        "should omit deliberate runner path differences from recipe identity"
    );
    assert_eq!(
        identity.recipe_sha256.len(),
        64,
        "should produce a SHA-256 recipe identity"
    );
}

#[test]
fn source_recipe_and_lock_identities_change_only_with_their_actual_inputs() {
    let fixture = Fixture::new();
    let source = "a".repeat(40);
    let first = source_identity(&fixture.checkout, &source).expect("should measure baseline");
    fs::write(fixture.checkout.join("Cargo.lock"), b"changed lock").expect("should change lock");
    let lock = source_identity(&fixture.checkout, &source).expect("should remeasure lock");
    assert_eq!(
        first.recipe_sha256, lock.recipe_sha256,
        "should keep lock identity separately visible"
    );
    assert_ne!(
        first.cargo_lock_sha256, lock.cargo_lock_sha256,
        "should hash actual lock bytes"
    );
    fs::write(
        fixture.checkout.join("scripts/release/phase1-build.sh"),
        b"changed recipe",
    )
    .expect("should change recipe");
    let recipe = source_identity(&fixture.checkout, &source).expect("should remeasure recipe");
    assert_ne!(
        lock.recipe_sha256, recipe.recipe_sha256,
        "should hash the actual source-owned producer"
    );
    assert_ne!(
        recipe.recipe_sha256,
        source_identity(&fixture.checkout, &"b".repeat(40))
            .expect("should bind another source")
            .recipe_sha256,
        "should bind the declared workflow source commit"
    );
}

#[test]
fn source_identity_rejects_invalid_commits_and_aliased_or_hardlinked_inputs() {
    let fixture = Fixture::new();
    assert!(
        source_identity(&fixture.checkout, "main").is_err(),
        "should reject symbolic source identity"
    );
    let path = fixture.checkout.join("Cargo.lock");
    fs::hard_link(&path, fixture.checkout.join("other-lock")).expect("should create test hardlink");
    assert!(
        source_identity(&fixture.checkout, &"a".repeat(40)).is_err(),
        "should reject shared source input identity"
    );
    fs::remove_file(&path).expect("should remove first link");
    symlink("other-lock", &path).expect("should create input alias");
    assert!(
        source_identity(&fixture.checkout, &"a".repeat(40)).is_err(),
        "should reject even an internal source alias"
    );
}

#[test]
fn measures_consumed_private_js_without_hashing_checkout_dependent_package_ids() {
    let first = Fixture::new();
    let second = Fixture::new();
    let (metadata, messages) = first.private_js();
    let (other_metadata, other_messages) = second.private_js();
    let identity = first
        .js_identity(&metadata, &messages)
        .expect("should measure actual private JS");
    assert_eq!(
        identity,
        second
            .js_identity(&other_metadata, &other_messages)
            .expect("should measure the other private root"),
        "should hash sorted bundle facts rather than local Cargo IDs"
    );
    fs::write(first.output().join("tsjs-example.js"), b"changed")
        .expect("should mutate actual consumed output");
    assert_ne!(
        identity,
        first
            .js_identity(&metadata, &messages)
            .expect("should remeasure private bytes"),
        "should identify actual consumed output changes"
    );
}

#[test]
fn rejects_missing_duplicate_foreign_and_host_side_js_build_records() {
    let fixture = Fixture::new();
    let (metadata, messages) = fixture.private_js();
    for changed in [
        vec![messages[1].clone()],
        vec![
            messages[0].clone(),
            messages[0].clone(),
            messages[1].clone(),
        ],
        vec![
            json!({"reason":"build-script-executed","package_id":"other","out_dir":fixture.output().parent(),"env":[]}),
            messages[1].clone(),
        ],
        vec![
            json!({"reason":"build-script-executed","package_id":messages[0]["package_id"],"out_dir":fixture.target.join("release/build/trusted-server-js-example/out"),"env":[]}),
            messages[1].clone(),
        ],
    ] {
        assert!(
            fixture.js_identity(&metadata, &changed).is_err(),
            "should reject an ambiguous or non-target JS result"
        );
    }
    let mut duplicate = metadata.clone();
    duplicate["packages"]
        .as_array_mut()
        .expect("should list packages")
        .push(metadata["packages"][0].clone());
    assert!(
        fixture.js_identity(&duplicate, &messages).is_err(),
        "should reject ambiguous package IDs"
    );
    for (field, value) in [
        ("/workspace_root", json!("/example/foreign")),
        (
            "/packages/0/manifest_path",
            json!("/example/foreign/Cargo.toml"),
        ),
        ("/workspace_members", json!([])),
    ] {
        let mut changed = metadata.clone();
        *changed
            .pointer_mut(field)
            .expect("should locate metadata fixture") = value;
        assert!(
            fixture.js_identity(&changed, &messages).is_err(),
            "should reject foreign or unowned metadata at {field}"
        );
    }
    let mut prebuilt = messages.clone();
    prebuilt[0]["env"] = json!([["TRUSTED_SERVER__TSJS_INVENTORY_SHA256", "a".repeat(64)]]);
    assert!(
        fixture.js_identity(&metadata, &prebuilt).is_err(),
        "should reject a checked-prebuilt result in the ordinary fresh-build profile"
    );
}

#[test]
fn rejects_private_output_directory_and_ancestor_aliases() {
    for ancestor in [false, true] {
        let fixture = Fixture::new();
        let (metadata, messages) = fixture.private_js();
        let original = if ancestor {
            fixture
                .output()
                .parent()
                .expect("should have out parent")
                .parent()
                .expect("should have package parent")
                .to_owned()
        } else {
            fixture.output()
        };
        let retained = original.with_extension("retained");
        fs::rename(&original, &retained)
            .expect("should retain actual output under a different identity");
        symlink(&retained, &original).expect("should alias the output directory");
        assert!(
            fixture.js_identity(&metadata, &messages).is_err(),
            "should reject an aliased private output directory or ancestor"
        );
    }
}

#[test]
fn rejects_casefold_collisions_with_the_source_core_module() {
    let fixture = Fixture::new();
    let (metadata, messages) = fixture.private_js();
    let path = fixture
        .checkout
        .join("crates/trusted-server-js/lib/src/integrations/Core");
    fs::create_dir(&path).expect("should create colliding integration directory");
    fs::write(path.join("index.ts"), b"export {};").expect("should write collision entry");
    assert!(
        fixture.js_identity(&metadata, &messages).is_err(),
        "should reject a casefold collision with the reserved core module on every filesystem"
    );
}

#[test]
fn rejects_failed_or_truncated_cargo_completion() {
    let fixture = Fixture::new();
    let (metadata, mut messages) = fixture.private_js();
    messages[1]["success"] = json!(false);
    assert!(
        fixture.js_identity(&metadata, &messages).is_err(),
        "should require the actual successful Cargo completion"
    );
    let bytes = serde_json::to_vec(&metadata).expect("should encode metadata");
    assert!(
        javascript_identity(&fixture.checkout, &fixture.target, &bytes, b"{}").is_err(),
        "should reject a truncated or malformed Cargo stream"
    );
}

#[test]
fn rejects_missing_extra_empty_aliased_and_hardlinked_private_bundles() {
    for mutation in 0..5 {
        let fixture = Fixture::new();
        let (metadata, messages) = fixture.private_js();
        let file = fixture.output().join("tsjs-example.js");
        match mutation {
            0 => fs::remove_file(&file).expect("should remove bundle"),
            1 => fs::write(fixture.output().join("extra.txt"), b"extra")
                .expect("should add unexpected output"),
            2 => fs::write(&file, b"").expect("should empty bundle"),
            3 => {
                fs::remove_file(&file).expect("should remove bundle");
                symlink("tsjs-core.js", &file).expect("should alias bundle");
            }
            _ => {
                fs::remove_file(&file).expect("should remove bundle");
                fs::hard_link(fixture.output().join("tsjs-core.js"), &file)
                    .expect("should link bundle");
            }
        }
        assert!(
            fixture.js_identity(&metadata, &messages).is_err(),
            "should reject private JS mutation {mutation}"
        );
    }
}
