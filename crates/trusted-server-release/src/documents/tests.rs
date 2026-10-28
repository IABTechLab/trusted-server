use std::cell::Cell;
use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::symlink;

use serde_json::json;
use sha2::Sha256;
use tempfile::tempdir;

use super::*;

#[test]
fn controlled_reads_cancel_before_and_between_bounded_chunks() {
    let root = tempdir().expect("should create controlled input");
    let path = root.path().join("input");
    fs::write(&path, vec![7_u8; 4 * 64 * 1024]).expect("should write multiple chunks");
    let calls = Cell::new(0);
    let check = || {
        calls.set(calls.get() + 1);
        if calls.get() >= 6 {
            return Err(invalid("cancelled fixture"));
        }
        Ok(())
    };
    assert!(
        read_bounded_with_check(&path, Role::Executable, &check).is_err(),
        "should cancel during bounded read"
    );
    assert_eq!(
        calls.get(),
        6,
        "should stop between the first and second chunks"
    );
    let before = || Err(invalid("already cancelled fixture"));
    assert!(
        hash_bounded_with_check(&path, 1024 * 1024, &before).is_err(),
        "should reject before hashing"
    );
}

#[test]
fn controlled_stream_hash_enforces_limit_and_exact_digest() {
    let root = tempdir().expect("should create hash input");
    let path = root.path().join("input");
    let bytes = vec![9_u8; 3 * 64 * 1024];
    fs::write(&path, &bytes).expect("should write hash input");
    let measured = hash_bounded_with_check(&path, bytes.len() as u64, &|| Ok(()))
        .expect("should stream exact bytes");
    assert_eq!(
        measured.sha256,
        hex::encode(Sha256::digest(&bytes)),
        "should hash all original bytes"
    );
    assert_eq!(
        measured.size,
        bytes.len() as u64,
        "should retain original size"
    );
    assert_eq!(
        measured.mode,
        fs::metadata(&path)
            .expect("should inspect measured mode")
            .mode(),
        "should retain mode from the same held input"
    );
    assert!(
        hash_bounded_with_check(&path, bytes.len() as u64 - 1, &|| Ok(())).is_err(),
        "should reject oversized files before reading"
    );
}

#[test]
fn controlled_stream_hash_detects_mutation_and_leaf_replacement() {
    let root = tempdir().expect("should create changing input");
    let path = root.path().join("input");
    let bytes = vec![7_u8; 3 * 64 * 1024];
    fs::write(&path, &bytes).expect("should write original input");
    let calls = Cell::new(0);
    let result = hash_bounded_with_check(&path, bytes.len() as u64, &|| {
        calls.set(calls.get() + 1);
        if calls.get() == 4 {
            fs::write(&path, b"changed").expect("should mutate held input");
        }
        Ok(())
    });
    assert!(
        result.is_err(),
        "should reject mutations between hash chunks"
    );
    fs::write(&path, &bytes).expect("should restore input fixture");
    let calls = Cell::new(0);
    let result = hash_bounded_with_check(&path, bytes.len() as u64, &|| {
        calls.set(calls.get() + 1);
        if calls.get() == 4 {
            fs::rename(&path, root.path().join("retained")).expect("should replace leaf name");
            fs::write(&path, &bytes).expect("should write identical replacement bytes");
        }
        Ok(())
    });
    assert!(
        result.is_err(),
        "should reject changed path identity even with matching bytes"
    );
}

fn parse(bytes: &[u8]) -> Result<Value, Report<ReleaseError>> {
    parse_json(bytes, Role::Metadata)
}

#[test]
fn rejects_duplicate_keys_at_every_depth() {
    for depth in 0..64 {
        let input = format!(
            "{}{{\"same\":1,\"same\":2}}{}",
            "[".repeat(depth),
            "]".repeat(depth)
        );
        assert!(
            parse(input.as_bytes()).is_err(),
            "should reject duplicate at depth {depth}"
        );
    }
}

#[test]
fn rejects_duplicate_decoded_keys_in_opaque_objects() {
    assert!(
        parse(br#"{"opaque":{"x":0,"\u0078":1}}"#).is_err(),
        "should reject escaped duplicate key"
    );
}

#[test]
fn rejects_top_level_integer_overflow() {
    for input in ["18446744073709551616", "-9223372036854775809"] {
        assert!(
            parse(input.as_bytes()).is_err(),
            "should reject integer overflow: {input}"
        );
    }
}

#[test]
fn rejects_opaque_integer_overflow() {
    for input in ["18446744073709551616", "-9223372036854775809"] {
        let bytes = format!("{{\"opaque\":[{{\"value\":{input}}}]}}");
        assert!(
            parse(bytes.as_bytes()).is_err(),
            "should reject opaque integer overflow"
        );
    }
}

#[test]
fn rejects_nonfinite_and_decimal_exponent_overflow() {
    for input in [
        "NaN",
        "Infinity",
        "-Infinity",
        "1e309",
        "-1e309",
        "1.8e308",
        "9".repeat(400).as_str(),
    ] {
        assert!(
            parse(input.as_bytes()).is_err(),
            "should reject nonfinite or overflowing number"
        );
    }
    for number in [
        format!("{}.0", "9".repeat(400)),
        "1e999".to_owned(),
        "-1.8e308".to_owned(),
    ] {
        let opaque = format!("{{\"opaque\":{{\"number\":{number}}}}}");
        assert!(
            parse(opaque.as_bytes()).is_err(),
            "should reject decimal/exponent overflow in opaque values"
        );
    }
}

#[test]
fn retains_exact_integer_boundaries() {
    assert_eq!(
        parse(b"18446744073709551615")
            .expect("should accept u64 boundary")
            .as_u64(),
        Some(u64::MAX),
        "should retain exact u64"
    );
    assert_eq!(
        parse(b"-9223372036854775808")
            .expect("should accept i64 boundary")
            .as_i64(),
        Some(i64::MIN),
        "should retain exact i64"
    );
}

#[test]
fn number_like_quoted_text_and_escapes_remain_text() {
    let value = json!({"text": "18446744073709551616 -9223372036854775809 1e999 \" quoted \\ backslash \\\" 1.8e308", "nested": ["9".repeat(400)]});
    let bytes = serde_json::to_vec(&value).expect("should serialize text controls");
    assert_eq!(
        parse(&bytes).expect("should preserve number-like text"),
        value,
        "should skip strings including escaped quotes and backslashes"
    );
}

#[test]
fn rejects_invalid_utf8_before_deserialization() {
    let error = parse(b"\"\xff\"").expect_err("should reject invalid UTF-8");
    assert!(
        format!("{error:?}").contains("UTF-8"),
        "should diagnose byte encoding before deserialization"
    );
}

#[test]
fn rejects_bom_before_deserialization() {
    let error = parse(b"\xef\xbb\xbf{}").expect_err("should reject BOM");
    assert!(
        format!("{error:?}").contains("BOM"),
        "should diagnose BOM before deserialization"
    );
}

#[test]
fn rejects_surrogates_and_trailing_controls() {
    for input in [
        br#""\ud800""#.as_slice(),
        br#""\udfff""#,
        b"{}\0",
        b"{}\x0b",
        b"{}\x1f",
        b"{}{}",
    ] {
        assert!(
            parse(input).is_err(),
            "should reject surrogate or trailing data"
        );
    }
}

#[test]
fn rejects_container_depth_65_including_root() {
    for (open, close) in [("[", "]"), ("{\"nested\":", "}")] {
        let good = format!("{}0{}", open.repeat(64), close.repeat(64));
        let bad = format!("{}0{}", open.repeat(65), close.repeat(65));
        assert!(
            parse(good.as_bytes()).is_ok(),
            "should accept 64 containers"
        );
        assert!(
            parse(bad.as_bytes()).is_err(),
            "should reject 65 containers"
        );
    }
}

#[test]
fn rejects_oversized_json_before_encoding_or_parsing() {
    for role in [Role::Metadata, Role::Companion, Role::Bundle, Role::Spdx] {
        let bytes = vec![0xff; role.limit() as usize + 1];
        let error = parse_json(&bytes, role).expect_err("should reject role byte overflow");
        assert!(
            format!("{error:?}").contains("byte limit"),
            "should enforce size before encoding"
        );
    }
}

#[test]
fn rejects_oversized_files_before_reading() {
    let root = tempdir().expect("should create fixtures");
    for role in [
        Role::Metadata,
        Role::Companion,
        Role::Bundle,
        Role::Spdx,
        Role::Executable,
    ] {
        let path = root.path().join("oversized");
        let file = fs::File::create(&path).expect("should create sparse oversized file");
        file.set_len(role.limit() + 1).expect("should size fixture");
        assert!(
            read_bounded(&path, role).is_err(),
            "should reject oversized role before read"
        );
    }
}

#[test]
fn retains_hash_of_exact_original_bytes() {
    let root = tempdir().expect("should create fixture");
    let path = root.path().join("source.json");
    let bytes = b" { \"a\" : 1 } \n";
    fs::write(&path, bytes).expect("should write original bytes");
    let raw = read_bounded(&path, Role::Metadata).expect("should read exact bytes");
    assert_eq!(raw.bytes, bytes, "should retain original bytes");
    assert_eq!(
        raw.sha256,
        hex::encode(Sha256::digest(bytes)),
        "should hash original bytes before normalization"
    );
}

#[test]
fn rejects_unsafe_flat_names() {
    for name in [
        "", ".", "..", ".hidden", "../a", "a/b", "a\\b", "a..b", "a.", "a b", "a\n", "a\0", "é",
        "-a", "a:b",
    ] {
        assert!(
            safe_name(name).is_err(),
            "should reject unsafe name {name:?}"
        );
    }
    assert!(
        safe_name(&"a".repeat(161)).is_err(),
        "should reject long name"
    );
    for name in [
        "a",
        "ts-x86_64-unknown-linux-gnu",
        "file.spdx.json",
        "A_0-z",
    ] {
        assert!(safe_name(name).is_ok(), "should accept safe flat name");
    }
}

#[test]
fn rejects_symlinks_hardlinks_and_directories_at_read() {
    let root = tempdir().expect("should create fixtures");
    let real = root.path().join("real");
    fs::write(&real, b"{}").expect("should write fixture");
    let link = root.path().join("symlink");
    symlink(&real, &link).expect("should create symlink");
    assert!(
        read_bounded(&link, Role::Metadata).is_err(),
        "should reject symbolic leaf"
    );
    let hard = root.path().join("hardlink");
    fs::hard_link(&real, &hard).expect("should create hardlink");
    assert_eq!(
        fs::metadata(&hard).expect("should inspect links").nlink(),
        2,
        "should exercise hardlink fixture"
    );
    assert!(
        read_bounded(&hard, Role::Metadata).is_err(),
        "should reject hardlink"
    );
    assert!(
        read_bounded(root.path(), Role::Metadata).is_err(),
        "should reject directory"
    );
}

#[test]
fn canonical_serialization_sorts_recursively_and_adds_one_newline() {
    let value = json!({"z":[{"z": "é", "a":true}], "a":0});
    let bytes = canonical_json(&value, Role::Metadata).expect("should serialize canonical bytes");
    assert_eq!(
        bytes,
        "{\"a\":0,\"z\":[{\"a\":true,\"z\":\"é\"}]}\n".as_bytes(),
        "should sort recursively and retain compact UTF-8"
    );
}

#[test]
fn rejects_oversized_serialization_before_replacement() {
    let root = tempdir().expect("should create fixture");
    let path = root.path().join("output.json");
    fs::write(&path, b"old").expect("should write previous document");
    let value = json!({"large":"x".repeat(Role::Metadata.limit() as usize)});
    assert!(
        atomic_json(&path, &value, Role::Metadata).is_err(),
        "should reject oversized output"
    );
    assert_eq!(
        fs::read(&path).expect("should retain old file"),
        b"old",
        "should preserve old bytes on rejected output"
    );
    assert_eq!(
        fs::read_dir(root.path())
            .expect("should list fixtures")
            .count(),
        1,
        "should leave no temporary output"
    );
}

#[test]
fn atomic_output_replaces_inode_with_private_mode() {
    let root = tempdir().expect("should create fixture");
    let path = root.path().join("output.json");
    fs::write(&path, b"old").expect("should write previous document");
    let old = fs::metadata(&path).expect("should inspect old file").ino();
    atomic_json(&path, &json!({"answer":42}), Role::Metadata)
        .expect("should replace complete document");
    let new = fs::metadata(&path).expect("should inspect replaced file");
    assert_ne!(
        old,
        new.ino(),
        "should replace rather than truncate original inode"
    );
    assert_eq!(
        new.mode() & 0o777,
        0o600,
        "should install private 0600 output"
    );
    assert_eq!(
        fs::read(&path).expect("should read complete output"),
        b"{\"answer\":42}\n",
        "should install full bytes"
    );
}

#[test]
fn rejects_output_links_and_cleans_failed_replacement() {
    let root = tempdir().expect("should create fixture");
    let real = root.path().join("real");
    fs::write(&real, b"old").expect("should write linked target");
    let link = root.path().join("link");
    symlink(&real, &link).expect("should create symbolic output");
    assert!(
        atomic_json(&link, &json!({}), Role::Metadata).is_err(),
        "should reject symbolic output"
    );
    let hard = root.path().join("hard");
    fs::hard_link(&real, &hard).expect("should create hardlinked output");
    assert!(
        atomic_json(&hard, &json!({}), Role::Metadata).is_err(),
        "should reject hardlinked output"
    );
    let directory = root.path().join("directory");
    fs::create_dir(&directory).expect("should create invalid destination");
    assert!(
        atomic_json(&directory, &json!({}), Role::Metadata).is_err(),
        "should reject directory output"
    );
    assert_eq!(
        fs::read(&real).expect("should retain original target"),
        b"old",
        "should preserve linked target bytes"
    );
    assert_eq!(
        fs::read_dir(root.path())
            .expect("should inspect cleanup")
            .count(),
        4,
        "should remove every failed sibling output"
    );
}

#[test]
fn rejects_negative_noninteger_and_out_of_role_lengths() {
    for size in [
        json!(-1),
        json!(1.0),
        json!(1.5),
        json!("1"),
        json!(9007199254740992_u64),
        json!(Role::Metadata.limit() + 1),
    ] {
        let record = json!({"name":"fixture.json", "size":size, "sha256":"a".repeat(64)});
        assert!(
            validate_file_records(&[record], Role::Metadata).is_err(),
            "should reject invalid length representation or bounds"
        );
    }
    for size in [0, 1, Role::Metadata.limit()] {
        assert!(
            validate_file_records(
                &[json!({"name":"fixture.json", "size":size, "sha256":"a".repeat(64)})],
                Role::Metadata
            )
            .is_ok(),
            "should accept exact valid role boundary"
        );
    }
}

#[test]
fn rejects_ambiguous_duplicate_file_identities() {
    for names in [["same.json", "same.json"], ["File.json", "file.json"]] {
        let records = names.map(|name| json!({"name":name,"size":1,"sha256":"a".repeat(64)}));
        assert!(
            validate_file_records(&records, Role::Metadata).is_err(),
            "should reject duplicate identity including portable case collisions"
        );
    }
}

#[test]
fn rejects_invalid_file_record_fields() {
    for record in [
        json!({"name":"../file", "size":1, "sha256":"a".repeat(64)}),
        json!({"name":"file", "size":1, "sha256":"A".repeat(64)}),
        json!({"name":"file", "size":1, "sha256":"a".repeat(64), "extra":true}),
    ] {
        assert!(
            validate_file_records(&[record], Role::Metadata).is_err(),
            "should reject unsafe, malformed or extra fields"
        );
    }
}

#[test]
fn draft_202012_prefix_items_and_items_are_enforced() {
    let schema =
        json!({"type":"array","prefixItems":[{"const":"first"},{"type":"integer"}],"items":false});
    assert!(
        schema_accepts(&schema, Draft::Draft202012, &json!(["first", 2]))
            .expect("should compile explicit 2020-12 schema"),
        "should accept prefix items"
    );
    for value in [
        json!(["wrong", 2]),
        json!(["first", "wrong"]),
        json!(["first", 2, 3]),
    ] {
        assert!(
            !schema_accepts(&schema, Draft::Draft202012, &value).expect("should compile schema"),
            "should reject invalid prefix or additional item"
        );
    }
}

#[test]
fn refuses_unregistered_remote_and_file_references() {
    for reference in [
        "https://example.com/unregistered.json",
        "http://example.com/unregistered.json",
        "file:///tmp/unregistered-schema.json",
    ] {
        assert!(
            schema_accepts(&json!({"$ref":reference}), Draft::Draft202012, &json!(null)).is_err(),
            "should refuse unregistered references without retrieval"
        );
    }
}

#[test]
fn refuses_existing_local_reference_even_with_file_resolution_feature() {
    let root = tempdir().expect("should create local schema fixture");
    let path = root.path().join("unregistered.json");
    fs::write(
        &path,
        serde_json::to_vec(&json!({"type":"null"}))
            .expect("should serialize fictional local schema"),
    )
    .expect("should write local schema");
    let reference = format!("file://{}", path.display());
    let result = schema_accepts(&json!({"$ref":reference}), Draft::Draft202012, &Value::Null);
    assert!(
        result.is_err(),
        "should refuse existing file without reading an unregistered reference"
    );
}

#[test]
fn retained_spdx_identity_and_draft7_are_exact() {
    let bytes = KnownSchema::Spdx.bytes();
    assert_eq!(
        bytes.len(),
        45312,
        "should retain exact original schema length"
    );
    assert_eq!(
        hex::encode(Sha256::digest(bytes)),
        "239208b7ac287b3cf5d9a9af23f9d69863971102a5e1587a27a398b43490b89b",
        "should retain pinned original schema hash"
    );
    let value = parse(bytes).expect("should parse official retained schema");
    assert_eq!(
        value["$schema"], "http://json-schema.org/draft-07/schema#",
        "should use official Draft 7 dialect"
    );
}

#[test]
fn rejects_altered_spdx_schema_before_evaluation() {
    let original = KnownSchema::Spdx.bytes();
    let mut altered = original.to_vec();
    let title = altered
        .windows(8)
        .position(|bytes| bytes == b"SPDX 2.3")
        .expect("should find official schema title");
    altered[title] = b'T';
    assert!(
        parse_pinned_spdx_schema(&altered).is_err(),
        "should reject altered but parseable retained schema"
    );
    let mut extended = original.to_vec();
    extended.push(b'\n');
    assert!(
        parse_pinned_spdx_schema(&extended).is_err(),
        "should reject changed raw byte count before use"
    );
    assert!(
        parse_pinned_spdx_schema(original).is_ok(),
        "should accept exact retained schema identity and dialect"
    );
}

#[test]
fn official_spdx_schema_checks_structure_only() {
    let valid = json!({"SPDXID":"SPDXRef-DOCUMENT", "spdxVersion":"SPDX-2.3", "creationInfo":{"creators":["Tool: fictional-fixture"],"created":"2026-01-01T00:00:00Z"}, "name":"Fictional fixture", "dataLicense":"CC0-1.0", "documentNamespace":"https://example.com/spdx/fictional-fixture"});
    assert!(
        validate_schema(KnownSchema::Spdx, &valid).is_ok(),
        "should accept a structural toy fixture only"
    );
    for value in [json!({}), json!({"SPDXID":42}), {
        let mut invalid = valid.clone();
        invalid["packages"] = json!([{"SPDXID":"SPDXRef-Package","name":"fictional"}]);
        invalid
    }] {
        assert!(
            validate_schema(KnownSchema::Spdx, &value).is_err(),
            "should reject structurally incomplete SPDX"
        );
    }
}

#[test]
fn retained_governing_schemas_reject_missing_fields() {
    for schema in [KnownSchema::ReleaseIndex, KnownSchema::Qualification] {
        assert!(
            validate_schema(schema, &json!({})).is_err(),
            "should apply retained governing required fields"
        );
    }
}

#[test]
fn retained_qualification_schema_accepts_structural_observation_only() {
    let mut value = json!({
        "schema_version":1, "purpose":"disposable-rehearsal-observation", "source_commit":"a".repeat(40),
        "closure_sha256":"a".repeat(64), "selection_sha256":"a".repeat(64), "raw_tools_sha256":"a".repeat(64), "dependency_sha256":"a".repeat(64),
        "run":{"repository_id":"1","owner_id":"1","run_id":"1","attempt":1,"job":"fictional-fixture","runner":"github-hosted"},
        "host":{"environment_id":"macos15-aarch64","kind":"github-hosted-macos","architecture":"aarch64","kernel_architecture":"aarch64",
            "os_version":"fictional-fixture","translated":false,"runner_image_version":null,"oci_index_sha256":null,"oci_manifest_sha256":null,
            "glibc_version":null,"identity_log_sha256":"a".repeat(64)},
        "sdk":{"kind":"apple-sdk","version":"fictional","build_version":"fictional","tree_sha256":"a".repeat(64),"vendor_evidence_sha256":"a".repeat(64)},
        "tools":[],
        "recipe":{"id":"tsjs","boundary_sha256":"a".repeat(64),"preparation_log_sha256":"a".repeat(64),"compile_log_sha256":"a".repeat(64),
            "native_log_sha256":"a".repeat(64),"exit_code":1,"full_recipe":false,"same_boundary":false,"executable_sha256":"a".repeat(64),
            "native_materials_sha256":"a".repeat(64),"deployment_target":null,"glibc_symbols":[],"unconfined":[],"confined":[],
            "download_attempts":{"cargo":"unavailable","npm":"unavailable","missing_tool":"unavailable"}},
        "runtime":[]
    });
    assert!(
        validate_schema(KnownSchema::Qualification, &value).is_ok(),
        "should accept structural observation fixture without qualifying a release"
    );
    value["run"]["attempt"] = json!(1.0);
    assert!(
        validate_schema(KnownSchema::Qualification, &value).is_err(),
        "should reject float representation of integer observation"
    );
    value["run"]["attempt"] = json!(1);
    value["purpose"] = json!("production-qualified");
    assert!(
        validate_schema(KnownSchema::Qualification, &value).is_err(),
        "should enforce retained observation purpose"
    );
}

fn index_fixture() -> Value {
    let identities = [
        (
            "trusted-server-fastly.wasm",
            "wasm32-wasip1",
            "wasm32",
            "fastly-compute",
            "wasip1",
        ),
        (
            "ts-x86_64-unknown-linux-gnu",
            "x86_64-unknown-linux-gnu",
            "x86_64",
            "linux-gnu",
            "glibc-2.34",
        ),
        (
            "ts-aarch64-apple-darwin",
            "aarch64-apple-darwin",
            "aarch64",
            "macos",
            "13.0",
        ),
        (
            "ts-x86_64-apple-darwin",
            "x86_64-apple-darwin",
            "x86_64",
            "macos",
            "13.0",
        ),
    ];
    let artifacts: Vec<_> = identities.iter().map(|(name,target,architecture,runtime,minimum)| {
        let is_server = *runtime == "fastly-compute";
        let companions: Map<_,_> = [("sbom", ".spdx.json"), ("provenance", ".provenance.sigstore.json"), ("sbom_attestation", ".sbom.sigstore.json"), ("build_evidence", ".build-evidence.json"), ("build_recipe", ".build-recipe.json"), ("rebuild_check", ".rebuild-check.json")].iter()
            .map(|(role, suffix)| (role.to_string(), json!({"name":format!("{name}{suffix}"), "size":1, "sha256":"a".repeat(64)}))).collect();
        json!({"name":name, "size":1, "sha256":"a".repeat(64), "role":if is_server {"server"} else {"operator-cli"},
            "kind":if is_server {"wasm-core"} else {"native-executable"}, "package":if is_server {"trusted-server-adapter-fastly"} else {"trusted-server-cli"},
            "target":target, "variant":"default", "assurance_profile":if is_server {"provenance-sbom-repeatable"} else {"provenance-sbom"},
            "capabilities":if is_server {vec!["fastly-production"]} else {vec!["cli-execute"]},
            "compatibility":{"runtime":runtime,"minimum_version":minimum,"architecture":architecture},
            "js_inventory_sha256":"a".repeat(64), "companions":companions})
    }).collect();
    let schema =
        parse(KnownSchema::ReleaseIndex.bytes()).expect("should load retained index schema");
    json!({"schema_version":1,"repository":schema["properties"]["repository"]["const"],"tag":"v1.0.0",
        "source_commit":"a".repeat(40),"recipe_source_commit":"a".repeat(40),"cohort_source_commit":"a".repeat(40),
        "cohort":"phase1-fastly-cli", "policy_version":1,"policy_sha256":"a".repeat(64),
        "tsjs_inventory":{"name":"tsjs-inventory.json","size":1,"sha256":"a".repeat(64)},"artifacts":artifacts})
}

#[test]
fn retained_index_schema_accepts_only_structural_fixed_cohort() {
    let value = index_fixture();
    assert!(
        validate_schema(KnownSchema::ReleaseIndex, &value).is_ok(),
        "should accept structural fixed-cohort fixture only"
    );
    let mut extra = value.clone();
    extra["artifacts"]
        .as_array_mut()
        .expect("should access artifacts")
        .push(value["artifacts"][0].clone());
    assert!(
        validate_schema(KnownSchema::ReleaseIndex, &extra).is_err(),
        "should reject fifth artifact"
    );
    let mut wrong = value;
    wrong["artifacts"][1]["target"] = json!("aarch64-apple-darwin");
    assert!(
        validate_schema(KnownSchema::ReleaseIndex, &wrong).is_err(),
        "should reject mismatched fixed cohort identity"
    );
}

#[test]
fn index_role_checks_reject_fractional_lengths_and_duplicate_companions() {
    let good = index_fixture();
    for size in [
        json!(-1),
        json!(1.0),
        json!(1.5),
        json!(9007199254740992_u64),
    ] {
        let mut value = good.clone();
        value["artifacts"][0]["companions"]["sbom"]["size"] = size;
        assert!(
            validate_schema(KnownSchema::ReleaseIndex, &value).is_err(),
            "should reject noninteger or unsafe lengths after schema checks"
        );
    }
    let mut duplicate = good;
    duplicate["artifacts"][1]["companions"]["sbom"]["name"] =
        duplicate["artifacts"][0]["companions"]["sbom"]["name"].clone();
    assert!(
        validate_schema(KnownSchema::ReleaseIndex, &duplicate).is_err(),
        "should reject duplicate companion identities across artifacts"
    );
}

#[test]
fn index_role_checks_reject_unsafe_policy_integer() {
    let mut value = index_fixture();
    value["policy_version"] = json!(9007199254740992_u64);
    assert!(
        validate_schema(KnownSchema::ReleaseIndex, &value).is_err(),
        "should reject integers above interoperable safe limit"
    );
    value["policy_version"] = json!(1.0);
    assert!(
        validate_schema(KnownSchema::ReleaseIndex, &value).is_err(),
        "should require integer token representation"
    );
}

#[test]
fn detects_mutation_on_held_input_descriptor() {
    let root = tempdir().expect("should create fixture");
    let path = root.path().join("held");
    fs::write(&path, b"old").expect("should create input");
    let file = File::open(&path).expect("should hold original descriptor");
    let before = file.metadata().expect("should capture original metadata");
    fs::write(&path, b"new").expect("should mutate same-length input");
    assert!(
        read_held(&file, &before, Role::Metadata).is_err(),
        "should detect same-length mutation against captured metadata"
    );
}

#[test]
fn held_read_does_not_reopen_replaced_path() {
    let root = tempdir().expect("should create fixture");
    let path = root.path().join("held");
    fs::write(&path, b"original").expect("should create input");
    let file = File::open(&path).expect("should hold original descriptor");
    let before = file.metadata().expect("should capture input metadata");
    fs::remove_file(&path).expect("should remove input pathname");
    fs::write(&path, b"replacement").expect("should create different file");
    assert!(
        read_held(&file, &before, Role::Metadata).is_err(),
        "should reject removed held file rather than reopen replacement"
    );
}

#[test]
fn rejects_fifo_without_waiting_for_a_writer() {
    let root = tempdir().expect("should create fixture");
    let path = root.path().join("input-fifo");
    let name = CString::new(path.as_os_str().as_bytes()).expect("should encode fixture pathname");
    // The CString supplies a valid NUL-terminated pathname for this Unix syscall.
    assert_eq!(
        unsafe { libc::mkfifo(name.as_ptr(), 0o600) },
        0,
        "should create FIFO fixture"
    );
    assert!(
        read_bounded(&path, Role::Metadata).is_err(),
        "should reject FIFO without blocking on open or read"
    );
}

#[test]
fn failed_rename_removes_private_sibling() {
    let root = tempdir().expect("should create fixture");
    let destination = root.path().join("directory");
    fs::create_dir(&destination).expect("should create conflicting destination");
    let directory = File::open(root.path()).expect("should hold parent");
    let temporary = tempfile::Builder::new()
        .prefix(".release-")
        .tempfile_in(root.path())
        .expect("should create sibling");
    let temporary_path = temporary.path().to_owned();
    assert!(
        commit_output(temporary, &destination, &directory).is_err(),
        "should fail file-to-directory replacement"
    );
    assert!(
        !temporary_path.exists(),
        "should clean sibling after actual rename failure"
    );
    assert_eq!(
        fs::read_dir(root.path())
            .expect("should inspect cleanup")
            .count(),
        1,
        "should leave only existing destination"
    );
}

#[test]
fn index_role_checks_require_role_specific_companion_names() {
    let good = index_fixture();
    let mut swapped = good.clone();
    let sbom = swapped["artifacts"][0]["companions"]["sbom"]["name"].clone();
    swapped["artifacts"][0]["companions"]["sbom"]["name"] =
        swapped["artifacts"][0]["companions"]["provenance"]["name"].clone();
    swapped["artifacts"][0]["companions"]["provenance"]["name"] = sbom;
    assert!(
        validate_schema(KnownSchema::ReleaseIndex, &swapped).is_err(),
        "should reject companion names swapped between roles"
    );
    let mut foreign = good;
    foreign["artifacts"][0]["companions"]["sbom"]["name"] =
        json!("ts-x86_64-unknown-linux-gnu-other.spdx.json");
    assert!(
        validate_schema(KnownSchema::ReleaseIndex, &foreign).is_err(),
        "should reject companion names not derived from their asset"
    );
}

#[test]
fn phase3_preserves_frozen_v1_policy_and_release_index_schema() {
    for (bytes, expected) in [
        (
            include_bytes!("../../../trusted-server-cli/release-policy.json").as_slice(),
            "f08ce10cd16ff2d1acc482db2b85eb53b667ec2b4753792f277d12f41d1f636c",
        ),
        (
            include_bytes!(
                "../../../../docs/superpowers/specs/schemas/release-index-v1.schema.json"
            )
            .as_slice(),
            "06a7e598f1d14b499d087ec0c894829b3aab26a690b35e5922d96987a877bb9e",
        ),
    ] {
        assert_eq!(
            hex::encode(Sha256::digest(bytes)),
            expected,
            "should preserve the frozen v1 contract bytes during the Phase 3 source port"
        );
    }
}

#[test]
fn phase3_restores_exact_frozen_v1_release_index_fixtures() {
    for (bytes, expected) in [
        (
            include_bytes!("../../../trusted-server-cli/tests/fixtures/release/release-index.json")
                .as_slice(),
            "4ff7a4b64e2724172e19d8ef0b02f500f133719411a15b0ce74d8fce4c8b7034",
        ),
        (
            include_bytes!("../../tests/fixtures/index/candidate/candidate-release-index.json")
                .as_slice(),
            "2e35dd6998ec958eae1f23454c50a5b6306be29d2f5eabdb60fd7360f0ca4203",
        ),
    ] {
        assert_eq!(
            hex::encode(Sha256::digest(bytes)),
            expected,
            "should restore the exact frozen v1 index fixture bytes"
        );
    }
}
