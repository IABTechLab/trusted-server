use super::super::paths::PrefixPaths;
use super::super::remap::{
    REMAPPED_PREFIXES, RemapSources, c_prefix_map_environment, check_cargo_config,
    check_no_extensionless_config, remap_arguments, scan_remapped_bytes,
};
use super::fixture::*;
use super::*;

const TRACKED_CARGO_CONFIG: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../.cargo/config.toml"
));

/// The `--config` value of `plan`'s remap entry.
fn remap_config(plan: &CargoPlan) -> &str {
    plan.arguments
        .iter()
        .rfind(|argument| argument.contains(".rustflags="))
        .expect("should carry the remap config")
}

fn sources<'a>(checkout: &'a str, vendor: &'a str) -> RemapSources<'a> {
    RemapSources {
        checkout,
        vendor,
        cargo_home: "/x/state/cargo-home",
        tools: "/x/tools",
        target: "/x/target",
    }
}

#[test]
fn remap_maps_every_source_root_to_a_fixed_prefix_in_a_fixed_order() {
    let arguments = remap_arguments("wasm32-wasip1", &sources("/x/checkout", "/x/vendor"))
        .expect("should remap disjoint roots");
    assert_eq!(
        arguments,
        [
            "--config".to_owned(),
            [
                "target.\"wasm32-wasip1\".rustflags=[",
                "\"--remap-path-prefix=/x/checkout=/trusted-server\",",
                "\"--remap-path-prefix=/x/vendor=/cargo-vendor\",",
                "\"--remap-path-prefix=/x/state/cargo-home=/cargo-home\",",
                "\"--remap-path-prefix=/x/tools=/rust-tools\",",
                "\"--remap-path-prefix=/x/target=/cargo-target\"]",
            ]
            .concat(),
        ],
        "should emit checkout, vendor, Cargo home, tools, target in that order \
         through one quoted target rustflags entry"
    );
    assert_eq!(
        REMAPPED_PREFIXES,
        [
            "/trusted-server",
            "/cargo-vendor",
            "/cargo-home",
            "/rust-tools",
            "/cargo-target"
        ],
        "should keep the fixed prefixes in emitted order"
    );
}

#[test]
fn nested_or_equal_remapped_roots_are_rejected() {
    for (checkout, vendor) in [
        ("/x/checkout", "/x/checkout/vendor"),
        ("/x/vendor/inner", "/x/vendor"),
        ("/x/same", "/x/same"),
        ("/x/checkout", "/x/tools"),
    ] {
        let error = remap_arguments("wasm32-wasip1", &sources(checkout, vendor))
            .expect_err("should reject overlapping roots whose order would decide the path");
        assert!(
            format!("{error:?}").contains("overlap"),
            "should name the overlap for {checkout} and {vendor}"
        );
    }
}

#[test]
fn plan_rejects_remapped_roots_that_overlap_in_the_execution_namespace() {
    let fixture = Fixture::new(COMMIT);
    let prepared = fixture
        .bind("trusted-server-fastly.wasm", Role::Canonical)
        .expect("should bind the recipe");
    let nested = PrefixPaths::new()
        .with(
            &fixture.base.join("materials"),
            "/boundary/immutable/0/materials",
            false,
        )
        .with(
            &fixture.base.join("checkout-canonical"),
            "/boundary/immutable/0",
            false,
        )
        .with(&fixture.retained(), "/boundary/immutable/1", false)
        .with(&fixture.base.join("tools"), "/boundary/tool/0", false)
        .with(
            &fixture.base.join("cargo-state"),
            "/boundary/writable/0",
            true,
        )
        .with(&fixture.base.join("target"), "/boundary/writable/1", true);
    assert!(
        plan_error(&prepared, &nested).contains("overlap"),
        "should reject a vendor tree mounted inside the checkout"
    );
}

#[test]
fn remap_is_identical_for_both_roles_in_the_same_mount_layout() {
    let fixture = Fixture::new(COMMIT);
    let canonical = fixture
        .bind("trusted-server-fastly.wasm", Role::Canonical)
        .expect("should bind the canonical build")
        .plan(&fixture.paths(Role::Canonical))
        .expect("should plan the canonical build");
    let other = Fixture::new(COMMIT);
    let reproduction = other
        .bind("trusted-server-fastly.wasm", Role::Reproduction)
        .expect("should bind the reproduction build")
        .plan(&other.paths(Role::Reproduction))
        .expect("should plan the reproduction build");
    assert_ne!(
        canonical.working_directory, reproduction.working_directory,
        "should run from distinct host checkouts"
    );
    assert_eq!(
        remap_config(&canonical),
        remap_config(&reproduction),
        "should hand both roles the same remap in the same execution layout"
    );
    for plan in [&canonical, &reproduction] {
        assert_eq!(
            (
                plan.environment.get("LC_ALL").map(String::as_str),
                plan.environment.get("TZ").map(String::as_str),
            ),
            (Some("C"), Some("UTC")),
            "should keep the compiler locale and timezone normalized for each role"
        );
    }
}

#[test]
fn remap_follows_identity_paths_to_the_same_fixed_prefixes() {
    let fixture = Fixture::new(COMMIT);
    let prepared = fixture
        .bind("trusted-server-fastly.wasm", Role::Canonical)
        .expect("should bind the recipe");
    let base = fixture.base.to_str().expect("should be UTF-8");
    let retained = fixture.retained();
    let identity = PrefixPaths::new().with(&fixture.base, base, true).with(
        &retained,
        retained.to_str().expect("should be UTF-8"),
        false,
    );
    let plan = prepared
        .plan(&identity)
        .expect("should plan with host paths");
    assert_eq!(
        remap_config(&plan),
        format!(
            "target.\"wasm32-wasip1\".rustflags=[\
             \"--remap-path-prefix={base}/checkout-canonical=/trusted-server\",\
             \"--remap-path-prefix={base}/materials/vendor=/cargo-vendor\",\
             \"--remap-path-prefix={base}/cargo-state/cargo-home=/cargo-home\",\
             \"--remap-path-prefix={base}/tools=/rust-tools\",\
             \"--remap-path-prefix={base}/target=/cargo-target\"]"
        ),
        "should remap host-identical execution paths, as on macOS, to the fixed prefixes"
    );
}

#[test]
fn tracked_cargo_config_keeps_target_rustflags_reviewable() {
    check_cargo_config(TRACKED_CARGO_CONFIG.as_bytes())
        .expect("should accept the tracked Cargo configuration");
    check_cargo_config(b"[target.wasm32-wasip1]\nrustflags = [\"--cfg\", \"example\"]\n")
        .expect("should accept target rustflags, which the remap extends");
    check_cargo_config(b"[build]\njobs = 2\n").expect("should accept other build keys");
}

#[test]
fn cargo_config_that_the_remap_would_supersede_is_rejected() {
    for (label, bytes) in [
        (
            "build rustflags",
            &b"[build]\nrustflags = [\"--cfg\", \"example\"]\n"[..],
        ),
        (
            "dotted build rustflags",
            b"build.rustflags = \"--cfg example\"\n",
        ),
        ("non-UTF-8", b"\xff\xfe"),
        ("non-TOML", b"[build\n"),
    ] {
        assert!(
            check_cargo_config(bytes).is_err(),
            "should reject a Cargo configuration with {label}"
        );
    }
}

#[test]
fn extensionless_cargo_config_is_rejected_tracked_or_on_disk() {
    let directory = tempfile::tempdir().expect("should create a checkout");
    let checkout = directory.path();
    std::fs::create_dir(checkout.join(".cargo")).expect("should create .cargo");
    std::fs::write(checkout.join(".cargo/config.toml"), b"[alias]\n")
        .expect("should write the reviewed configuration");
    check_no_extensionless_config(checkout, false)
        .expect("should accept a checkout with only config.toml");
    assert!(
        format!(
            "{:?}",
            check_no_extensionless_config(checkout, true).expect_err("should reject tracked")
        )
        .contains("tracks .cargo/config"),
        "should reject a tracked extensionless configuration"
    );
    std::os::unix::fs::symlink("config.toml", checkout.join(".cargo/config"))
        .expect("should alias the legacy name");
    assert!(
        format!(
            "{:?}",
            check_no_extensionless_config(checkout, false).expect_err("should reject on disk")
        )
        .contains("holds .cargo/config"),
        "should reject an extensionless configuration present on disk, even as a link"
    );
}

#[test]
fn remap_scan_requires_a_fixed_prefix_and_no_unremapped_path() {
    let clean = b"\0asm../trusted-server/crates/core/src/lib.rs\0/rustc/abc/library";
    let scan = scan_remapped_bytes(clean, &["/tmp/fixture-root", "/home/runner/work/_temp"])
        .expect("should accept remapped bytes");
    assert_eq!(
        scan.found(),
        ["/trusted-server"],
        "should list found prefixes"
    );
    for (label, bytes) in [
        ("container mount", &b"x/boundary/immutable/0/src/lib.rs"[..]),
        (
            "host fixture root",
            b"/trusted-server/a /tmp/fixture-root/x",
        ),
        (
            "host runner temp",
            b"/cargo-vendor/a /home/runner/work/_temp/y",
        ),
        ("no fixed prefix", b"/rustc/abc/library/core/src/lib.rs"),
        ("prefix without separator", b"/trusted-server-core"),
    ] {
        assert!(
            scan_remapped_bytes(bytes, &["/tmp/fixture-root", "/home/runner/work/_temp"]).is_err(),
            "should reject bytes with {label}"
        );
    }
    let too_many = vec!["/x"; 65];
    for forbidden in [&too_many[..], &[""][..]] {
        assert!(
            scan_remapped_bytes(clean, forbidden).is_err(),
            "should bound the forbidden prefixes"
        );
    }
}

#[test]
fn native_c_code_gets_the_same_remap_and_wasm_gets_none() {
    let roots = sources("/x/checkout", "/x/vendor");
    assert_eq!(
        c_prefix_map_environment(ReleaseTarget::Wasm32Wasip1, &roots)
            .expect("should accept disjoint roots"),
        None,
        "should leave the Fastly plan without C flags"
    );
    let (name, value) = c_prefix_map_environment(ReleaseTarget::X86_64UnknownLinuxGnu, &roots)
        .expect("should accept disjoint roots")
        .expect("should remap C code of a native asset");
    assert_eq!(
        name, "CFLAGS_x86_64_unknown_linux_gnu",
        "should use the target-scoped name cc reads first"
    );
    assert_eq!(
        value,
        [
            "-ffile-prefix-map=/x/checkout=/trusted-server",
            "-fdebug-prefix-map=/x/checkout=/trusted-server",
            "-ffile-prefix-map=/x/vendor=/cargo-vendor",
            "-fdebug-prefix-map=/x/vendor=/cargo-vendor",
            "-ffile-prefix-map=/x/state/cargo-home=/cargo-home",
            "-fdebug-prefix-map=/x/state/cargo-home=/cargo-home",
            "-ffile-prefix-map=/x/tools=/rust-tools",
            "-fdebug-prefix-map=/x/tools=/rust-tools",
            "-ffile-prefix-map=/x/target=/cargo-target",
            "-fdebug-prefix-map=/x/target=/cargo-target",
        ]
        .join(" "),
        "should map every root to the same fixed prefix rustc uses"
    );
}

#[test]
fn native_c_remap_rejects_split_or_overlapping_roots() {
    for (label, roots) in [
        ("whitespace", sources("/x/check out", "/x/vendor")),
        ("nested", sources("/x/checkout", "/x/checkout/vendor")),
    ] {
        assert!(
            c_prefix_map_environment(ReleaseTarget::X86_64UnknownLinuxGnu, &roots).is_err(),
            "should reject a {label} root"
        );
    }
}
