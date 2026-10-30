use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest as _, Sha256};

pub(crate) fn trace_source_digest(ts_dir: &Path) -> String {
    let definition = "trace-build-inputs.json";
    let inputs = read_json(&ts_dir.join(definition));
    let mut sources = vec![definition.to_owned()];
    sources.extend(strings(&inputs["files"]));
    let mut directories: Vec<PathBuf> = strings(&inputs["source_roots"])
        .into_iter()
        .map(PathBuf::from)
        .collect();
    while let Some(directory) = directories.pop() {
        for entry in
            fs::read_dir(ts_dir.join(&directory)).expect("should enumerate trace build inputs")
        {
            let entry = entry.expect("should read a trace build input entry");
            let relative = directory.join(entry.file_name());
            let kind = entry.file_type().expect("should inspect trace build input");
            if kind.is_dir() {
                directories.push(relative);
            } else if kind.is_file() {
                sources.push(
                    relative
                        .to_str()
                        .expect("should use UTF-8 input paths")
                        .replace('\\', "/"),
                );
            }
        }
    }
    sources.sort();
    let mut hash = Sha256::new();
    for source in sources {
        add(
            &mut hash,
            &source,
            &fs::read(ts_dir.join(&source)).expect("should read trace build input"),
        );
    }
    let lock = read_json(&ts_dir.join("package-lock.json"));
    let packages = lock["packages"]
        .as_object()
        .expect("should lock trace build packages");
    let mut selected = BTreeSet::new();
    let mut pending: Vec<String> = strings(&inputs["toolchain_roots"])
        .into_iter()
        .map(|name| format!("node_modules/{name}"))
        .collect();
    while let Some(location) = pending.pop() {
        if !selected.insert(location.clone()) {
            continue;
        }
        let entry = packages
            .get(&location)
            .unwrap_or_else(|| panic!("should resolve locked trace build package {location}"));
        for field in ["dependencies", "optionalDependencies"] {
            if let Some(dependencies) = entry[field].as_object() {
                for name in dependencies.keys() {
                    let mut parent = location.as_str();
                    let resolved = loop {
                        if parent.is_empty() {
                            break format!("node_modules/{name}");
                        }
                        let candidate = format!("{parent}/node_modules/{name}");
                        if packages.contains_key(&candidate) {
                            break candidate;
                        }
                        parent = parent
                            .rsplit_once("/node_modules/")
                            .map_or("", |(base, _)| base);
                    };
                    assert!(
                        packages.contains_key(&resolved),
                        "should resolve locked trace dependency {name} at {resolved}"
                    );
                    pending.push(resolved);
                }
            }
        }
    }
    for location in selected {
        for field in ["version", "resolved", "integrity"] {
            add(
                &mut hash,
                &format!("{location}/{field}"),
                packages[&location][field].as_str().unwrap_or("").as_bytes(),
            );
        }
    }
    hex::encode(hash.finalize())
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("should read trace build definition"))
        .expect("should parse trace build definition")
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("should list trace inputs")
        .iter()
        .map(|value| {
            value
                .as_str()
                .expect("should name a trace input")
                .to_owned()
        })
        .collect()
}

fn add(hash: &mut Sha256, name: &str, bytes: &[u8]) {
    hash.update(name.as_bytes());
    hash.update([0]);
    hash.update(bytes);
    hash.update([0]);
}
