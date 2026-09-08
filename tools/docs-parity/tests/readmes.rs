use std::collections::BTreeMap;

use docs_parity::readmes::validate_inventory;

fn complete_inventory() -> (String, BTreeMap<String, Vec<u8>>) {
    let root = "[workspace]\nmembers = [\"crates/alpha\", \"crates/beta\"]\n".to_owned();
    let files = BTreeMap::from([
        (
            "crates/alpha/Cargo.toml".to_owned(),
            b"[package]\nname = \"alpha\"\nreadme = \"README.md\"\n".to_vec(),
        ),
        ("crates/alpha/README.md".to_owned(), b"# Alpha\n".to_vec()),
        (
            "crates/beta/Cargo.toml".to_owned(),
            b"[package]\nname = \"beta\"\nreadme = \"README.md\"\n".to_vec(),
        ),
        ("crates/beta/README.md".to_owned(), b"# Beta\n".to_vec()),
    ]);
    (root, files)
}

#[test]
fn workspace_packages_require_exact_readme_metadata_and_files() {
    let (root, files) = complete_inventory();
    validate_inventory(&root, &files).expect("complete README inventory should pass");

    let mut missing_metadata = files.clone();
    missing_metadata.insert(
        "crates/alpha/Cargo.toml".to_owned(),
        b"[package]\nname = \"alpha\"\n".to_vec(),
    );
    assert!(
        validate_inventory(&root, &missing_metadata).is_err(),
        "missing readme metadata should fail"
    );

    let mut missing_file = files;
    missing_file.remove("crates/beta/README.md");
    assert!(
        validate_inventory(&root, &missing_file).is_err(),
        "missing README file should fail"
    );
}

#[test]
fn extra_unlisted_crate_readme_fails_closed() {
    let (root, mut files) = complete_inventory();
    files.insert(
        "crates/not-a-member/README.md".to_owned(),
        b"# Unlisted\n".to_vec(),
    );

    assert!(
        validate_inventory(&root, &files).is_err(),
        "README for an unlisted crate should fail"
    );
}
