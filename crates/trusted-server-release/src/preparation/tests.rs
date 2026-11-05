use super::*;

fn request() -> Value {
    json!({"schema_version":1,"nonce":"a".repeat(64),"source_commit":"b".repeat(40),"repo_root":"/example/source","cache":"/example/cache","guard_path":"/example/tools/guard.node","native_esbuild":{"lock_path":"node_modules/@esbuild/linux-x64","sha256":"c".repeat(64),"size":1,"mode":493}})
}
fn summary() -> Value {
    let mut value = request();
    value["success"] = json!(true);
    value["npm_exit_code"] = json!(0);
    value["events"] = json!([]);
    value["normalizations"] = json!([]);
    value["preparation"] = json!({"schema_version":1});
    value
}
fn seal(value: &Value, key: &[u8; 32]) -> Vec<u8> {
    let bytes = serde_json::to_vec(value).expect("should encode fixture observation");
    let mut mac =
        Hmac::<Sha256>::new_from_slice(key).expect("should initialize fixture authenticator");
    mac.update(&bytes);
    format!(
        "{}{} {}\n",
        std::str::from_utf8(PREFIX).expect("should decode fixed prefix"),
        STANDARD.encode(bytes),
        hex::encode(mac.finalize().into_bytes())
    )
    .into_bytes()
}
#[test]
fn unauthenticated_child_output_cannot_construct_an_install_observation() {
    let key = random_bytes::<32>().expect("should read an authentication key");
    let mut forged_key = key;
    forged_key[0] ^= 1;
    assert!(
        authenticate_observation(
            &seal(&summary(), &forged_key),
            &key,
            &request(),
            &BTreeMap::new()
        )
        .is_err(),
        "should reject a child-forged observation"
    );
}
#[test]
fn observation_requires_one_successful_private_request() {
    let key = random_bytes::<32>().expect("should read an authentication key");
    authenticate_observation(&seal(&summary(), &key), &key, &request(), &BTreeMap::new())
        .expect("should accept an authenticated protocol fixture");
    for field in [
        "nonce",
        "source_commit",
        "repo_root",
        "cache",
        "guard_path",
        "native_esbuild",
        "schema_version",
        "success",
        "npm_exit_code",
    ] {
        let mut changed = summary();
        changed[field] = Value::Null;
        assert!(
            authenticate_observation(&seal(&changed, &key), &key, &request(), &BTreeMap::new())
                .is_err(),
            "should reject an altered binding or failed observation"
        );
    }
    let mut duplicate = seal(&summary(), &key);
    duplicate.extend(seal(&summary(), &key));
    assert!(
        authenticate_observation(&duplicate, &key, &request(), &BTreeMap::new()).is_err(),
        "should reject repeated authenticated completion"
    );
}
#[test]
fn hook_start_does_not_substitute_for_successful_terminal_execution() {
    let key = random_bytes::<32>().expect("should read an authentication key");
    let digest = "d".repeat(64);
    let declarations = BTreeMap::from([(
        ("node_modules/example".to_owned(), "postinstall".to_owned()),
        digest.clone(),
    )]);
    let start = json!({"lock_path":"node_modules/example","event":"postinstall","script_sha256":digest,"stage":"start","changes":[]});
    let mut success = start.clone();
    success["stage"] = json!("success");
    success["exit_code"] = json!(0);
    success["signal"] = Value::Null;
    let mut value = summary();
    value["events"] = json!([start.clone(), success.clone()]);
    authenticate_observation(&seal(&value, &key), &key, &request(), &declarations)
        .expect("should join start and successful completion");
    for events in [
        json!([]),
        json!([start.clone()]),
        json!([success]),
        json!([start.clone(), start]),
    ] {
        value["events"] = events;
        assert!(
            authenticate_observation(&seal(&value, &key), &key, &request(), &declarations).is_err(),
            "should reject missing, repeated or out-of-order hook execution"
        );
    }
}

#[cfg(target_os = "linux")]
pub(crate) mod full;

#[test]
fn normalization_receipt_cannot_assert_another_native_tool_identity() {
    let key = random_bytes::<32>().expect("should read an authentication key");
    let mut value = summary();
    value["normalizations"] = json!([{"kind":"esbuild-hardlink-copy-v1","path":"node_modules/esbuild/bin/esbuild","native_path":"node_modules/@esbuild/linux-x64/bin/esbuild","sha256":"f".repeat(64),"size":1,"mode":493}]);
    assert!(
        authenticate_observation(&seal(&value, &key), &key, &request(), &BTreeMap::new()).is_err(),
        "should bind normalization to independently authenticated native bytes"
    );
}
