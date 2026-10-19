use object::macho as macho_format;
use serde_json::{Value, json};

use super::*;
use crate::inspect::tests::builders::{ElfSpec, MachoSpec, elf, macho, wasm_module};
use crate::inspect::{ElfPolicy, MachoPolicy, inspect_elf, inspect_macho, inspect_wasm};

fn encoded(requirements: &ExternalRequirements) -> Value {
    serde_json::to_value(requirements).expect("should encode typed runtime requirements")
}

#[test]
fn elf_requirements_retain_only_inspected_loader_libraries_and_versions() {
    let mut spec = ElfSpec::conforming();
    spec.needed.retain(|library| *library != "libm.so.6");
    let facts = inspect_elf(&elf(&spec), &ElfPolicy::linux_x86_64_gnu())
        .expect("should accept ELF fixture");
    let requirements = ExternalRequirements::from_elf(&facts);
    assert_eq!(
        requirements.target(),
        ReleaseTarget::X86_64UnknownLinuxGnu,
        "should derive supported GNU target"
    );
    assert_eq!(
        requirements.artifact_sha256(),
        facts.sha256(),
        "should bind inspector artifact digest"
    );
    let value = encoded(&requirements);
    assert_eq!(
        value["runtime"]["interpreter"],
        facts.interpreter(),
        "should retain actual ELF interpreter"
    );
    assert_eq!(
        value["runtime"]["needed"],
        json!(["libgcc_s.so.1", "libc.so.6"]),
        "should not add allowed but absent libraries"
    );
    assert_eq!(
        value["runtime"]["max_glibc"], "2.34",
        "should retain observed GLIBC maximum"
    );
    assert_eq!(
        value["runtime"]["version_requirements"]
            .as_array()
            .expect("should hold versions")
            .len(),
        5,
        "should retain every library/version pair"
    );
    assert!(
        value["runtime"]["version_requirements"]
            .as_array()
            .expect("should hold versions")
            .contains(&json!({"file":"libc.so.6","name":"GLIBC_2.34"})),
        "should bind symbol version to its observed library"
    );
}

#[test]
fn macho_requirements_retain_actual_loader_cpu_and_deployment_versions() {
    let facts = inspect_macho(
        &macho(&MachoSpec::conforming()),
        &MachoPolicy::macos(MachoCpu::Arm64),
    )
    .expect("should accept Mach-O fixture");
    let requirements = ExternalRequirements::from_macho(&facts);
    assert_eq!(
        requirements.target(),
        ReleaseTarget::Aarch64AppleDarwin,
        "should derive Apple CPU target"
    );
    assert_eq!(
        requirements.artifact_sha256(),
        facts.sha256(),
        "should bind inspector artifact digest"
    );
    let value = encoded(&requirements);
    assert_eq!(
        value["runtime"]["loader"], "/usr/lib/dyld",
        "should retain validated observed loader"
    );
    assert_eq!(
        value["runtime"]["dylibs"],
        json!(facts.dylibs()),
        "should retain only observed dylibs"
    );
    assert_eq!(
        value["runtime"]["minimum_os"], "13.0.0",
        "should retain observed minimum OS"
    );
    assert_eq!(
        value["runtime"]["sdk"], "15.5.0",
        "should retain observed SDK"
    );
    assert_eq!(
        value["runtime"]["cpu"], "arm64",
        "should retain observed CPU"
    );
}

#[test]
fn wasm_requirements_retain_only_observed_modules_and_inspector_digest() {
    for imports in [
        vec![],
        vec![
            ("wasi_snapshot_preview1", "fd_write"),
            ("fastly_http_req", "send"),
        ],
    ] {
        let facts = inspect_wasm(&wasm_module(&imports)).expect("should accept core Wasm fixture");
        let requirements = ExternalRequirements::from_wasm(&facts);
        assert_eq!(
            requirements.target(),
            ReleaseTarget::Wasm32Wasip1,
            "should derive only supported Wasm release target"
        );
        assert_eq!(
            requirements.artifact_sha256(),
            facts.sha256(),
            "should bind inspector artifact digest"
        );
        let value = encoded(&requirements);
        assert_eq!(
            value["runtime"]["import_modules"],
            json!(facts.import_modules()),
            "should not add assumed host modules"
        );
        assert!(
            value["runtime"].get("needed").is_none(),
            "should not claim native dynamic libraries for Wasm"
        );
    }
}

#[test]
fn x86_64_macho_facts_cannot_be_relabelled_as_arm64() {
    let mut spec = MachoSpec::conforming();
    spec.cputype = macho_format::CPU_TYPE_X86_64;
    spec.cpusubtype = macho_format::CPU_SUBTYPE_X86_64_ALL;
    let facts = inspect_macho(&macho(&spec), &MachoPolicy::macos(MachoCpu::X86_64))
        .expect("should accept x86_64 Mach-O fixture");
    let requirements = ExternalRequirements::from_macho(&facts);
    assert_eq!(
        requirements.target(),
        ReleaseTarget::X86_64AppleDarwin,
        "should retain supported CPU mapping"
    );
    assert_eq!(
        encoded(&requirements)["runtime"]["cpu"],
        "x86_64",
        "should retain inspected CPU"
    );
}

#[test]
fn changed_accepted_bytes_produce_different_artifact_bindings() {
    let mut spec = ElfSpec::conforming();
    let original = inspect_elf(&elf(&spec), &ElfPolicy::linux_x86_64_gnu())
        .expect("should accept original ELF");
    spec.payload = b"different fictional payload";
    let changed = inspect_elf(&elf(&spec), &ElfPolicy::linux_x86_64_gnu())
        .expect("should accept changed ELF");
    assert_ne!(
        ExternalRequirements::from_elf(&original).artifact_sha256(),
        ExternalRequirements::from_elf(&changed).artifact_sha256(),
        "should bind final bytes rather than only equal loader requirements"
    );
}
