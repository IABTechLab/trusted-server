//! Apple developer tool records, Darwin binding and the exact Darwin plan.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::super::apple::{plist_string, sdk_settings_version};
use super::super::paths::PrefixPaths;
use super::fixture::*;
use super::*;
use crate::process::Cancellation;

fn detached_apple(fixture: &Fixture) -> AppleDeveloperTools {
    AppleDeveloperTools::with_detached_sdk_for_tests(
        fixture.apple_facts(),
        Instant::now() + Duration::from_secs(60),
        &Cancellation::default(),
    )
    .expect("should stage selected SDK")
}

#[test]
fn owned_sdk_identity_is_path_independent_and_binds_the_selected_tree() {
    let fixture = Fixture::new(COMMIT);
    let first = detached_apple(&fixture);
    let second = detached_apple(&fixture);
    assert_ne!(
        first.sdk_root(),
        second.sdk_root(),
        "should use independent private roots"
    );
    assert_eq!(
        first.identity_sha256().expect("should identify first SDK"),
        second
            .identity_sha256()
            .expect("should identify second SDK"),
        "should not bind random snapshot paths"
    );
    fs::write(
        fixture.apple_facts().sdk_root.join("example.h"),
        b"new selected SDK input",
    )
    .expect("should add SDK header");
    let changed = detached_apple(&fixture);
    assert_ne!(
        first.identity_sha256().expect("should identify first SDK"),
        changed
            .identity_sha256()
            .expect("should identify changed SDK"),
        "should bind complete tree digest into Apple identity"
    );
    first
        .verify()
        .expect("should independently retain original selected SDK");
}

#[test]
fn owned_sdk_closure_rechecks_headers_stubs_and_frameworks_and_retains_clone_lifetime() {
    for relative in [
        "usr/include/example.h",
        "usr/lib/libExample.tbd",
        "System/Library/Frameworks/Example.framework/Example",
    ] {
        let fixture = Fixture::new(COMMIT);
        let source = fixture.apple_facts().sdk_root.join(relative);
        fs::create_dir_all(source.parent().expect("should have parent"))
            .expect("should create SDK directories");
        fs::write(source, b"selected SDK input").expect("should write SDK input");
        let apple = detached_apple(&fixture);
        let clone = apple.clone();
        let root = apple.sdk_root().to_owned();
        let identity = apple.identity_sha256().expect("should measure identity");
        drop(apple);
        clone
            .verify()
            .expect("should retain SDK while clone exists");
        let value =
            serde_json::to_value(clone.candidate_measurement().expect("should measure SDK"))
                .expect("should serialize SDK");
        assert!(
            value["sdk_tree"]["tree_sha256"]
                .as_str()
                .is_some_and(|digest| digest.len() == 64),
            "should bind typed SDK receipt"
        );
        assert_eq!(
            value["coverage_exclusions"],
            serde_json::json!([
                "tool_helpers",
                "liblto",
                "clang_resource_directory",
                "toolchain_libraries"
            ]),
            "should remove only covered SDK exclusions"
        );
        fs::write(root.join(relative), b"changed SDK input").expect("should mutate owned input");
        assert_eq!(
            clone
                .identity_sha256()
                .expect("should retain original identity"),
            identity,
            "should retain original digest for comparison"
        );
        assert!(
            clone.verify().is_err(),
            "should detect changed {relative} while four existing file hashes remain unchanged"
        );
        drop(clone);
        assert!(!root.exists(), "should remove SDK after last clone");
    }
}

#[test]
fn detached_sdk_is_the_actual_readonly_plan_sysroot_and_has_stable_remaps() {
    let fixture = Fixture::new(COMMIT);
    let apple = detached_apple(&fixture);
    let root = apple.sdk_root().to_owned();
    let tools = fixture
        .rust_tools_for(ReleaseTarget::Aarch64AppleDarwin)
        .with_apple_developer_tools(apple);
    let prepared = fixture
        .bind_parts(
            fixture.request("ts-aarch64-apple-darwin", "A", COMMIT),
            Role::Canonical,
            COMMIT,
            tools,
            fixture.materials(COMMIT),
        )
        .expect("should bind detached SDK");
    assert!(
        prepared
            .boundary_roots()
            .expect("should bind roots")
            .immutable()
            .contains(&root),
        "should scan SDK as readonly owned root"
    );
    let plan = prepared
        .plan(&mac_paths(&fixture).with(&root, root.to_str().expect("should be UTF-8"), false))
        .expect("should plan selected SDK");
    assert_eq!(
        plan.environment["SDKROOT"],
        text(&root),
        "should select copy as SDKROOT"
    );
    let cflags = &plan.environment["CFLAGS_aarch64_apple_darwin"];
    for flag in [
        format!("-isysroot {}", text(&root)),
        format!("-ffile-prefix-map={}=/apple-sdk", text(&root)),
        format!("-fdebug-prefix-map={}=/apple-sdk", text(&root)),
    ] {
        assert!(
            cflags.contains(&flag),
            "should use selected SDK C flag {flag}"
        );
    }
    let flags = plan.arguments.join(" ");
    assert!(
        flags.contains(&format!("--remap-path-prefix={}=/apple-sdk", text(&root))),
        "should remap selected SDK in Rust"
    );
    assert!(
        flags.contains("-Clink-arg=-isysroot")
            && flags.contains(&format!("-Clink-arg={}", text(&root))),
        "should explicitly select linker SDK"
    );
    let paths = mac_paths(&fixture).with(&root, "/selected-sdk", false);
    for prefix in [text(&root), "/selected-sdk".to_owned()] {
        let bytes = format!("/trusted-server/src/main.rs {prefix}/usr/include/example.h");
        assert!(
            prepared
                .check_final_paths(bytes.as_bytes(), &paths)
                .is_err(),
            "should reject literal SDK path {prefix} in final bytes"
        );
    }
}

#[test]
fn candidate_apple_evidence_records_only_measured_files_and_explicit_exclusions() {
    let fixture = Fixture::new(COMMIT);
    let apple = fixture.apple_tools();
    let evidence = apple
        .candidate_measurement()
        .expect("should export checked Apple candidate evidence");
    let value = serde_json::to_value(evidence).expect("should serialize Apple candidate evidence");
    assert_eq!(
        value["identity_sha256"],
        apple.identity_sha256().expect("should bind Apple identity"),
        "should preserve the bound identity record"
    );
    assert_eq!(
        value["tools"][0]["sha256"],
        fixture.apple_facts().clang_sha256,
        "should retain actual clang bytes identity"
    );
    assert_eq!(
        value["sdk_settings"]["sha256"],
        fixture.apple_facts().sdk_settings_sha256,
        "should retain SDK settings bytes identity"
    );
    assert_eq!(
        value["xcode_version"], "16.4",
        "should retain measured Xcode version"
    );
    assert_eq!(
        value["sdk_version"], "15.5.0",
        "should retain measured SDK version"
    );
    assert_eq!(
        value["origin"], "candidate_measurement",
        "should not authenticate original acquisitions"
    );
    assert_eq!(
        value["qualification"], "unqualified",
        "should not grant release authority"
    );
    assert_eq!(
        value["coverage_exclusions"],
        serde_json::json!([
            "sdk_headers",
            "sdk_stubs",
            "sdk_frameworks",
            "tool_helpers",
            "liblto",
            "clang_resource_directory",
            "toolchain_libraries"
        ]),
        "should explicitly retain every unmeasured SDK/tool closure class"
    );
    let serialized = serde_json::to_string(&value).expect("should serialize measurement");
    assert!(
        !serialized.contains(fixture.base.to_str().expect("should have UTF-8 root")),
        "should serialize relative component roles without host roots"
    );
}

#[test]
fn candidate_apple_evidence_rechecks_every_owned_measured_file() {
    for relative in [
        "Toolchains/XcodeDefault.xctoolchain/usr/bin/clang",
        "Toolchains/XcodeDefault.xctoolchain/usr/bin/ar",
        "Toolchains/XcodeDefault.xctoolchain/usr/bin/ld",
        "Platforms/MacOSX.platform/Developer/SDKs/MacOSX15.5.sdk/SDKSettings.json",
    ] {
        let fixture = Fixture::new(COMMIT);
        let apple = fixture.apple_tools();
        fs::write(
            fixture.developer_dir().join(relative),
            b"changed measured input",
        )
        .expect("should alter selected Apple input");
        assert!(
            apple.candidate_measurement().is_err(),
            "should reject changed {relative}"
        );
    }
}

/// The macOS boundary namespace: every root keeps its host path.
fn mac_paths(fixture: &Fixture) -> PrefixPaths {
    let identity = |paths: PrefixPaths, root: &Path, writable: bool| {
        paths.with(root, root.to_str().expect("should be UTF-8"), writable)
    };
    let mut paths = PrefixPaths::new();
    for root in [
        fixture.base.join("checkout-canonical"),
        fixture.retained(),
        fixture.base.join("materials"),
        fixture.base.join("tools"),
        fixture.developer_dir(),
    ] {
        paths = identity(paths, &root, false);
    }
    for root in [
        fixture.base.join("cargo-state"),
        fixture.base.join("target"),
    ] {
        paths = identity(paths, &root, true);
    }
    paths
}

fn text(path: &Path) -> String {
    path.to_str().expect("should be UTF-8").to_owned()
}

#[test]
fn apple_tool_facts_are_checked_before_any_use() {
    let fixture = Fixture::new(COMMIT);
    AppleDeveloperTools::from_facts_for_tests(fixture.apple_facts())
        .expect("should accept the fictional developer directory");
    type Edit = fn(&mut AppleToolFacts, &Path);
    let edits: [(&str, Edit); 10] = [
        ("a relative developer directory", |facts, _| {
            facts.developer_dir = PathBuf::from("apple-developer");
        }),
        ("clang outside the developer directory", |facts, base| {
            facts.clang = base.join("tools/bin/clang");
        }),
        ("an SDK outside the developer directory", |facts, base| {
            facts.sdk_root = base.join("other-source");
        }),
        ("the developer directory as its own SDK", |facts, _| {
            facts.sdk_root = facts.developer_dir.clone();
        }),
        ("a misnamed archiver", |facts, _| {
            facts.ar = facts.clang.with_file_name("llvm-ar");
        }),
        ("a linker beside another directory", |facts, _| {
            facts.ld = facts.developer_dir.join("usr/bin/ld");
        }),
        ("whitespace in a path", |facts, _| {
            facts.developer_dir = facts.developer_dir.join("Xcode 16.app");
        }),
        ("a non-decimal Xcode version", |facts, _| {
            facts.xcode_version = "16.4 beta".to_owned();
        }),
        ("an uppercase digest", |facts, _| {
            facts.clang_sha256 = facts.clang_sha256.to_ascii_uppercase();
        }),
        ("a short digest", |facts, _| {
            facts.sdk_settings_sha256.truncate(63);
        }),
    ];
    for (label, edit) in edits {
        let mut facts = fixture.apple_facts();
        edit(&mut facts, &fixture.base);
        assert!(
            AppleDeveloperTools::from_facts_for_tests(facts).is_err(),
            "should reject {label}"
        );
    }
}

#[test]
fn qualified_apple_tools_stay_closed_and_ordinary_ci_is_unqualified() {
    let never: fn(QualifiedAppleTools) -> ! = |qualification| match qualification {};
    let _constructor: fn(AppleToolFacts, QualifiedAppleTools) -> AppleDeveloperTools =
        AppleDeveloperTools::qualified;
    let _ = never;
    let fixture = Fixture::new(COMMIT);
    assert_eq!(
        fixture.apple_tools().authority(),
        Authority::Unqualified,
        "should carry no release authority"
    );
}

#[test]
fn sdk_settings_and_bundle_versions_are_read_strictly() {
    assert_eq!(
        sdk_settings_version(br#"{"Version":"15.5","DisplayName":"macOS 15.5"}"#)
            .expect("should read the SDK version"),
        crate::inspect::MacosVersion::new(15, 5)
    );
    for settings in [
        &br#"{"Version":"15"}"#[..],
        br#"{"Version":15.5}"#,
        b"not json",
    ] {
        assert!(
            sdk_settings_version(settings).is_err(),
            "should reject {}",
            String::from_utf8_lossy(settings)
        );
    }
    let plist =
        b"<dict>\n\t<key>CFBundleShortVersionString</key>\n\t<string>16.4</string>\n</dict>";
    assert_eq!(
        plist_string(plist, "CFBundleShortVersionString").expect("should read the version"),
        "16.4"
    );
    assert!(
        plist_string(plist, "CFBundleVersion").is_err(),
        "should reject an absent key"
    );
}

#[test]
fn apple_tools_rehash_against_their_record() {
    let fixture = Fixture::new(COMMIT);
    let tools = fixture.apple_tools();
    tools.verify().expect("should accept the untouched record");
    let clang = fixture.developer_dir().join(TOOLCHAIN_BIN).join("clang");
    fs::write(&clang, b"swapped compiler").expect("should swap clang");
    assert!(
        format!(
            "{:?}",
            tools.verify().expect_err("should reject a swapped clang")
        )
        .contains("Apple clang changed"),
        "should name the swapped tool"
    );
    fs::write(&clang, b"fictional Apple clang").expect("should restore clang");
    tools.verify().expect("should accept the restored clang");
    let settings = fixture
        .developer_dir()
        .join(SDK_ROOT)
        .join("SDKSettings.json");
    fs::write(&settings, br#"{"Version":"26.1"}"#).expect("should swap the SDK settings");
    assert!(
        tools.verify().is_err(),
        "should reject changed SDK settings"
    );
}

#[test]
fn darwin_binding_requires_apple_tools_and_others_refuse_them() {
    let fixture = Fixture::new(COMMIT);
    let darwin = fixture.request("ts-aarch64-apple-darwin", "A", COMMIT);
    let bare = fixture.rust_tools_for(ReleaseTarget::Aarch64AppleDarwin);
    let message = bind_error(fixture.bind_parts(
        darwin,
        Role::Canonical,
        COMMIT,
        bare,
        fixture.materials(COMMIT),
    ));
    assert!(
        message.contains("Darwin assets require Apple developer tools"),
        "should require Apple tools for Darwin: {message}"
    );

    let fixture = Fixture::new(COMMIT);
    let linux = fixture.request("ts-x86_64-unknown-linux-gnu", "A", COMMIT);
    let with_apple = fixture
        .rust_tools_for(ReleaseTarget::X86_64UnknownLinuxGnu)
        .with_apple_developer_tools(fixture.apple_tools());
    let message = bind_error(fixture.bind_parts(
        linux,
        Role::Canonical,
        COMMIT,
        with_apple,
        fixture.materials(COMMIT),
    ));
    assert!(
        message.contains("only Darwin assets take Apple developer tools"),
        "should refuse Apple tools for Linux: {message}"
    );
}

#[test]
fn apple_tools_changed_before_binding_are_rejected() {
    let fixture = Fixture::new(COMMIT);
    let tools = fixture.tools_for(ReleaseTarget::Aarch64AppleDarwin);
    fs::write(
        fixture.developer_dir().join(TOOLCHAIN_BIN).join("ld"),
        b"swapped linker",
    )
    .expect("should swap ld");
    let request = fixture.request("ts-aarch64-apple-darwin", "A", COMMIT);
    let message = bind_error(fixture.bind_parts(
        request,
        Role::Canonical,
        COMMIT,
        tools,
        fixture.materials(COMMIT),
    ));
    assert!(
        message.contains("Apple ld changed"),
        "should reject a linker changed after measurement: {message}"
    );
}

#[test]
fn darwin_tool_root_and_target_must_share_one_parent() {
    let fixture = Fixture::new(COMMIT);
    let nested = fixture.base.join("other-work/target");
    let request = fixture.request_at("ts-aarch64-apple-darwin", "A", COMMIT, &nested);
    let message = bind_error(fixture.bind_with(request, Role::Canonical, COMMIT));
    assert!(
        message.contains("direct children of one recipe-owned parent"),
        "should require one recipe-owned parent: {message}"
    );
    assert!(
        !nested.exists(),
        "should create nothing after the rejection"
    );

    let fixture = Fixture::new(COMMIT);
    let request = fixture.request_at(
        "ts-x86_64-unknown-linux-gnu",
        "A",
        COMMIT,
        &fixture.base.join("other-work/target"),
    );
    fixture
        .bind_with(request, Role::Canonical, COMMIT)
        .expect("should not constrain a Linux target directory");
}

#[test]
fn exact_darwin_plan_binds_the_linker_debug_map_and_apple_tools() {
    let fixture = Fixture::new(COMMIT);
    let prepared = fixture
        .bind("ts-aarch64-apple-darwin", Role::Canonical)
        .expect("should bind the Darwin recipe");
    let plan = prepared
        .plan(&mac_paths(&fixture))
        .expect("should derive the plan in the macOS namespace");
    let base = text(&fixture.base);
    let developer = text(&fixture.developer_dir());
    let clang = format!("{developer}/{TOOLCHAIN_BIN}/clang");
    let flags = [
        format!("\"--remap-path-prefix={base}/checkout-canonical=/trusted-server\""),
        format!("\"--remap-path-prefix={base}/materials/vendor=/cargo-vendor\""),
        format!("\"--remap-path-prefix={base}/cargo-state/cargo-home=/cargo-home\""),
        format!("\"--remap-path-prefix={base}/tools=/rust-tools\""),
        format!("\"--remap-path-prefix={base}/target=/cargo-target\""),
        format!("\"-Clink-arg=-Wl,-oso_prefix,{base}/\""),
    ];
    let remap = format!(
        "target.\"aarch64-apple-darwin\".rustflags=[{}]",
        flags.join(",")
    );
    let linker = format!("target.\"aarch64-apple-darwin\".linker=\"{clang}\"");
    let remap_at = plan
        .arguments
        .iter()
        .position(|argument| *argument == remap)
        .expect("should carry the debug-map prefix inside the one target rustflags entry");
    assert_eq!(
        plan.arguments.get(remap_at + 1..remap_at + 3),
        Some(["--config".to_owned(), linker].as_slice()),
        "should select clang as the target linker right after the remap"
    );
    let expected = [
        ("DEVELOPER_DIR", developer.clone()),
        ("SDKROOT", format!("{developer}/{SDK_ROOT}")),
        ("CC_aarch64_apple_darwin", clang),
        (
            "AR_aarch64_apple_darwin",
            format!("{developer}/{TOOLCHAIN_BIN}/ar"),
        ),
        ("ZERO_AR_DATE", "1".to_owned()),
        ("OPENSSL_CONF", "/dev/null".to_owned()),
    ];
    for (name, value) in expected {
        assert_eq!(
            plan.environment.get(name),
            Some(&value),
            "should set {name} exactly"
        );
    }
    let cflags = &plan.environment["CFLAGS_aarch64_apple_darwin"];
    assert!(
        cflags.contains(&format!("-ffile-prefix-map={developer}=/apple-developer"))
            && cflags.contains(&format!("-fdebug-prefix-map={developer}=/apple-developer")),
        "should remap the developer directory for C code: {cflags}"
    );
    for denied in ["CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER", "LD", "CC", "AR"] {
        assert!(
            !plan.environment.contains_key(denied),
            "should never select a tool through {denied}"
        );
    }
    assert!(
        !plan
            .arguments
            .iter()
            .any(|argument| argument.contains("xcrun")),
        "should never route through xcrun"
    );
}

#[test]
fn darwin_plan_digest_binds_the_apple_tool_record() {
    let fixture = Fixture::new(COMMIT);
    let digest = |tools: SelectedRustTools| {
        let request = fixture.request("ts-aarch64-apple-darwin", "A", COMMIT);
        let prepared = fixture
            .bind_parts(
                request,
                Role::Canonical,
                COMMIT,
                tools,
                fixture.materials(COMMIT),
            )
            .expect("should bind the Darwin recipe");
        let digest = prepared
            .plan(&mac_paths(&fixture))
            .expect("should derive the plan")
            .digest()
            .expect("should digest the plan");
        fs::remove_dir(fixture.base.join("target")).expect("should free the target");
        for state in ["home", "cargo-home", "tmp"] {
            fs::remove_dir(fixture.base.join("cargo-state").join(state))
                .expect("should free the cargo state");
        }
        digest
    };
    let measured = digest(fixture.tools_for(ReleaseTarget::Aarch64AppleDarwin));
    assert_eq!(
        measured,
        digest(fixture.tools_for(ReleaseTarget::Aarch64AppleDarwin)),
        "should derive the same digest from the same record"
    );
    let mut facts = fixture.apple_facts();
    facts.xcode_version = "16.3".to_owned();
    let relabelled = fixture
        .rust_tools_for(ReleaseTarget::Aarch64AppleDarwin)
        .with_apple_developer_tools(
            AppleDeveloperTools::from_facts_for_tests(facts).expect("should record the facts"),
        );
    assert_ne!(
        measured,
        digest(relabelled),
        "should bind the Apple tool record into the plan digest"
    );
}

#[test]
fn darwin_final_paths_forbid_every_root_and_the_developer_directory() {
    let fixture = Fixture::new(COMMIT);
    let prepared = fixture
        .bind("ts-aarch64-apple-darwin", Role::Canonical)
        .expect("should bind the Darwin recipe");
    let roots = [
        (
            fixture.base.join("checkout-canonical"),
            "/work/source",
            false,
        ),
        (fixture.retained(), "/work/scripts", false),
        (fixture.base.join("materials"), "/work/dependencies", false),
        (fixture.base.join("tools"), "/work/compiler", false),
        (fixture.base.join("cargo-state"), "/work/state", true),
        (fixture.base.join("target"), "/work/products", true),
        (fixture.developer_dir(), "/work/developer", false),
    ];
    let paths = roots
        .iter()
        .fold(PrefixPaths::new(), |paths, (host, execution, writable)| {
            paths.with(host, execution, *writable)
        });
    prepared
        .check_final_paths(b"/trusted-server/src/main.rs", &paths)
        .expect("should accept remapped bytes");
    let mut denied: Vec<String> = roots
        .iter()
        .flat_map(|(host, execution, _)| [text(host), (*execution).to_owned()])
        .collect();
    denied.push(text(&fixture.base.join("output")));
    for prefix in denied {
        let bytes = format!("/trusted-server/src/main.rs {prefix}/leaked.h");
        let error = prepared
            .check_final_paths(bytes.as_bytes(), &paths)
            .expect_err("should reject a bound root in final bytes");
        assert!(
            format!("{error:?}").contains("unremapped path prefix"),
            "should reject {prefix} by the final-path gate: {error:?}"
        );
    }
}

#[test]
fn darwin_plan_requires_the_developer_directory_in_the_namespace() {
    let fixture = Fixture::new(COMMIT);
    let prepared = fixture
        .bind("ts-aarch64-apple-darwin", Role::Canonical)
        .expect("should bind the Darwin recipe");
    let without_developer = PrefixPaths::new()
        .with(
            &fixture.base.join("checkout-canonical"),
            "/m/checkout",
            false,
        )
        .with(&fixture.retained(), "/m/retained", false)
        .with(&fixture.base.join("materials"), "/m/materials", false)
        .with(&fixture.base.join("tools"), "/m/tools", false)
        .with(&fixture.base.join("cargo-state"), "/m/state", true)
        .with(&fixture.base.join("target"), "/m/target", true);
    let message = plan_error(&prepared, &without_developer);
    assert!(
        message.contains("Apple developer directory is outside the boundary roots"),
        "should name the unmapped developer directory: {message}"
    );
}
