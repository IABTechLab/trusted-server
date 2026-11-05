//! Mach-O inspection for thin 64-bit macOS executables.
//!
//! Beyond identity, loader and libraries, the inspector records the
//! `LC_UUID`, the symbol table and its debug map, and the code signature
//! form. Every `N_OSO` debug-map object path must be relative and carry a
//! zero modification time (the recipe links with `-oso_prefix` and
//! `ZERO_AR_DATE=1`). An arm64 executable must carry the linker's ad-hoc
//! signature, which macOS requires to run it; an `x86_64` executable may be
//! unsigned or carry the same signature.

mod signature;

use error_stack::Report;
use object::macho::{self as format, MachHeader64};
use object::read::macho::{LoadCommandData, LoadCommandVariant, MachHeader as _, Nlist as _};
use object::{Endianness, FileKind};

use super::{
    MachoFacts, MacosVersion, RUNNER_PATH_PREFIXES, Violation, malformed, reject_denied_paths,
    rejected, sha256_hex, utf8,
};
use crate::error::ReleaseError;

type Header = MachHeader64<Endianness>;

/// The only dynamic linker a macOS executable may name.
const MACOS_DYLINKER: &str = "/usr/lib/dyld";

/// Reviewed `LC_LOAD_DYLIB` set for the `ts` CLI.
///
/// Observed with `otool -L` on a local `aarch64-apple-darwin` dev build of
/// `trusted-server-cli`: Security and CoreFoundation (via
/// `security-framework`), libiconv and libSystem.
const MACOS_DYLIBS: &[&str] = &[
    "/System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation",
    "/System/Library/Frameworks/Security.framework/Versions/A/Security",
    "/usr/lib/libSystem.B.dylib",
    "/usr/lib/libiconv.2.dylib",
];

/// Selected macOS CPU type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
pub(crate) enum MachoCpu {
    /// `aarch64-apple-darwin` (`CPU_SUBTYPE_ARM64_ALL`; arm64e is rejected).
    #[display("arm64")]
    Arm64,
    /// `x86_64-apple-darwin` (`CPU_SUBTYPE_X86_64_ALL`).
    #[display("x86_64")]
    X86_64,
}

impl MachoCpu {
    /// Returns the expected `(cputype, cpusubtype & !CPU_SUBTYPE_MASK)`.
    const fn expected(self) -> (u32, u32) {
        match self {
            Self::Arm64 => (format::CPU_TYPE_ARM64, format::CPU_SUBTYPE_ARM64_ALL),
            Self::X86_64 => (format::CPU_TYPE_X86_64, format::CPU_SUBTYPE_X86_64_ALL),
        }
    }
}

/// Compiled-in Mach-O acceptance policy.
#[derive(Debug, Clone)]
pub(crate) struct MachoPolicy {
    pub(crate) cpu: MachoCpu,
    pub(crate) minimum: MacosVersion,
    pub(crate) allowed_dylibs: &'static [&'static str],
    pub(crate) denied_path_prefixes: &'static [&'static str],
}

impl MachoPolicy {
    /// The reviewed macOS policy for `cpu`: deployment target exactly 13.0.
    pub(crate) const fn macos(cpu: MachoCpu) -> Self {
        Self {
            cpu,
            minimum: MacosVersion::new(13, 0),
            allowed_dylibs: MACOS_DYLIBS,
            denied_path_prefixes: RUNNER_PATH_PREFIXES,
        }
    }
}

/// The code signature form an accepted executable carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
pub(crate) enum CodeSignature {
    /// No `LC_CODE_SIGNATURE`; accepted for `x86_64` only.
    #[display("unsigned")]
    Unsigned,
    /// The linker's ad-hoc signature: one code directory with flags
    /// `0x20002`, no CMS blob and no team identifier.
    #[display("linker-signed-ad-hoc")]
    LinkerSignedAdhoc,
}

/// Symbol table counts after every debug-map entry was checked.
#[derive(Default)]
struct SymbolSummary {
    symbols: usize,
    debug_map_objects: usize,
}

/// Load-command observations accumulated before policy checks.
#[derive(Default)]
struct Observed {
    build_versions: Vec<(u32, MacosVersion, MacosVersion)>,
    dylinkers: Vec<String>,
    dylibs: Vec<String>,
    uuids: Vec<[u8; 16]>,
    symbol_tables: Vec<SymbolSummary>,
    signatures: Vec<(u32, u32)>,
}

/// Inspects a final Mach-O executable against `policy`.
///
/// # Errors
///
/// Returns [`ReleaseError::Inspect`] with a [`Violation`] when the bytes are
/// fat, 32-bit, big-endian or not `MH_EXECUTE`; the CPU differs; there is
/// not exactly one macOS `LC_BUILD_VERSION` with `minos` equal to the floor
/// (or an `LC_VERSION_MIN_*` is present); the dynamic linker is not
/// `/usr/lib/dyld`; a dylib is outside the allowlist or loaded weak,
/// re-exported, lazy or upward; `LC_RPATH`/`LC_DYLD_ENVIRONMENT` is present;
/// a runner-only path is embedded anywhere in the file; a debug-map object
/// path is absolute or time-stamped; there is not exactly one `LC_UUID`
/// (the linker always writes one); `LC_SYMTAB` or `LC_CODE_SIGNATURE` is
/// duplicated; the code signature is not the linker's ad-hoc form; or an
/// arm64 executable is unsigned.
pub(crate) fn inspect_macho(
    bytes: &[u8],
    policy: &MachoPolicy,
) -> Result<MachoFacts, Report<ReleaseError>> {
    check_container(bytes)?;
    let header = Header::parse(bytes, 0).map_err(|error| malformed("Mach-O header", error))?;
    let endian = header
        .endian()
        .map_err(|error| malformed("Mach-O endianness", error))?;
    check_identity(header, endian, policy.cpu)?;
    let mut observed = load_commands(header, endian, bytes)?;
    let (minos, sdk) = deployment_target(&observed, policy.minimum)?;
    if observed.dylinkers.as_slice() != [MACOS_DYLINKER] {
        return Err(rejected(
            Violation::Interpreter,
            format!(
                "LC_LOAD_DYLINKER {:?}, expected [{MACOS_DYLINKER:?}]",
                observed.dylinkers
            ),
        ));
    }
    if let Some(name) = observed
        .dylibs
        .iter()
        .find(|name| !policy.allowed_dylibs.contains(&name.as_str()))
    {
        return Err(rejected(
            Violation::Library,
            format!("LC_LOAD_DYLIB {name:?}"),
        ));
    }
    reject_denied_paths(bytes, policy.denied_path_prefixes)?;
    let signature = code_signature(&observed, bytes, policy.cpu)?;
    let [uuid] = observed.uuids.as_slice() else {
        return Err(rejected(
            Violation::MachoIdentity,
            format!(
                "expected exactly one LC_UUID, found {}",
                observed.uuids.len()
            ),
        ));
    };
    let uuid = *uuid;
    let summary = match observed.symbol_tables.as_mut_slice() {
        [] => SymbolSummary::default(),
        [summary] => std::mem::take(summary),
        _ => {
            return Err(rejected(
                Violation::MachoDebugMap,
                "more than one LC_SYMTAB",
            ));
        }
    };
    Ok(MachoFacts {
        sha256: sha256_hex(bytes),
        loader: observed
            .dylinkers
            .pop()
            .expect("should retain the validated dynamic linker"),
        cpu: policy.cpu,
        minos,
        sdk,
        dylibs: observed.dylibs,
        uuid,
        symbols: summary.symbols,
        debug_map_objects: summary.debug_map_objects,
        signature,
    })
}

/// Classifies the code signature and applies the per-CPU requirement.
fn code_signature(
    observed: &Observed,
    bytes: &[u8],
    cpu: MachoCpu,
) -> Result<CodeSignature, Report<ReleaseError>> {
    let signature = match observed.signatures.as_slice() {
        [] => CodeSignature::Unsigned,
        [(offset, size)] => {
            let start = usize::try_from(*offset).ok();
            let end = start.and_then(|start| start.checked_add(usize::try_from(*size).ok()?));
            let blob = start
                .zip(end)
                .and_then(|(start, end)| bytes.get(start..end))
                .ok_or_else(|| {
                    rejected(
                        Violation::CodeSignature,
                        "LC_CODE_SIGNATURE data lies outside the file",
                    )
                })?;
            signature::check_linker_signed(blob)?;
            CodeSignature::LinkerSignedAdhoc
        }
        _ => {
            return Err(rejected(
                Violation::CodeSignature,
                "more than one LC_CODE_SIGNATURE",
            ));
        }
    };
    if cpu == MachoCpu::Arm64 && signature == CodeSignature::Unsigned {
        return Err(rejected(
            Violation::CodeSignature,
            "an arm64 executable must carry the linker's ad-hoc signature",
        ));
    }
    Ok(signature)
}

/// Counts the symbol table, requiring every `N_OSO` debug-map entry to name
/// a non-empty relative UTF-8 path with a zero modification time.
fn symbol_summary(
    symtab: &format::SymtabCommand<Endianness>,
    endian: Endianness,
    bytes: &[u8],
) -> Result<SymbolSummary, Report<ReleaseError>> {
    let parse_error = |error| malformed("Mach-O symbol table", error);
    // The parser rejects an empty slice it cannot align, so an empty table
    // is counted without reading it.
    if symtab.nsyms.get(endian) == 0 {
        return Ok(SymbolSummary::default());
    }
    let table = symtab
        .symbols::<Header, _>(endian, bytes)
        .map_err(parse_error)?;
    let mut summary = SymbolSummary::default();
    for symbol in table.iter() {
        summary.symbols += 1;
        if symbol.n_type() != format::N_OSO {
            continue;
        }
        let name = symbol.name(endian, table.strings()).map_err(parse_error)?;
        let name = utf8(name, "Mach-O debug-map object")?;
        if name.is_empty() || name.starts_with('/') {
            return Err(rejected(
                Violation::MachoDebugMap,
                format!("debug-map object {name:?} is not a relative path"),
            ));
        }
        let modified = symbol.n_value(endian);
        if modified != 0 {
            return Err(rejected(
                Violation::MachoDebugMap,
                format!("debug-map object {name:?} records modification time {modified}"),
            ));
        }
        summary.debug_map_objects += 1;
    }
    Ok(summary)
}

/// Requires a thin 64-bit Mach-O container.
fn check_container(bytes: &[u8]) -> Result<(), Report<ReleaseError>> {
    match FileKind::parse(bytes) {
        Ok(FileKind::MachO64) => Ok(()),
        Ok(kind @ (FileKind::MachO32 | FileKind::MachOFat32 | FileKind::MachOFat64)) => {
            Err(rejected(
                Violation::Architecture,
                format!("{kind:?}; expected thin 64-bit Mach-O"),
            ))
        }
        Ok(kind) => Err(malformed("Mach-O container", format!("{kind:?}"))),
        Err(error) => Err(malformed("Mach-O container", error)),
    }
}

/// Requires a little-endian `MH_EXECUTE` for the selected CPU.
fn check_identity(
    header: &Header,
    endian: Endianness,
    cpu: MachoCpu,
) -> Result<(), Report<ReleaseError>> {
    let observed = (
        header.cputype(endian),
        header.cpusubtype(endian) & !format::CPU_SUBTYPE_MASK,
    );
    let file_type = header.filetype(endian);
    if endian != Endianness::Little || observed != cpu.expected() || file_type != format::MH_EXECUTE
    {
        return Err(rejected(
            Violation::Architecture,
            format!(
                "Mach-O {endian:?} cpu {:#x}/{:#x} filetype {file_type}; expected little-endian \
                 {cpu} MH_EXECUTE",
                observed.0, observed.1
            ),
        ));
    }
    Ok(())
}

/// Walks every load command, rejecting forbidden kinds immediately.
fn load_commands(
    header: &Header,
    endian: Endianness,
    bytes: &[u8],
) -> Result<Observed, Report<ReleaseError>> {
    let parse_error = |error| malformed("Mach-O load command", error);
    let mut commands = header
        .load_commands(endian, bytes, 0)
        .map_err(parse_error)?;
    let mut observed = Observed::default();
    while let Some(command) = commands.next().map_err(parse_error)? {
        record_command(&command, endian, bytes, &mut observed)?;
    }
    Ok(observed)
}

/// Records or rejects one load command.
fn record_command(
    command: &LoadCommandData<'_, Endianness>,
    endian: Endianness,
    bytes: &[u8],
    observed: &mut Observed,
) -> Result<(), Report<ReleaseError>> {
    let parse_error = |error| malformed("Mach-O load command", error);
    match command.variant().map_err(parse_error)? {
        LoadCommandVariant::Uuid(uuid) => observed.uuids.push(uuid.uuid),
        LoadCommandVariant::Symtab(symtab) => observed
            .symbol_tables
            .push(symbol_summary(symtab, endian, bytes)?),
        LoadCommandVariant::LinkeditData(data) if command.cmd() == format::LC_CODE_SIGNATURE => {
            observed
                .signatures
                .push((data.dataoff.get(endian), data.datasize.get(endian)));
        }
        LoadCommandVariant::BuildVersion(build) => observed.build_versions.push((
            build.platform.get(endian),
            MacosVersion::from_encoded(build.minos.get(endian)),
            MacosVersion::from_encoded(build.sdk.get(endian)),
        )),
        LoadCommandVariant::LoadDylinker(dylinker) => {
            let name = command.string(endian, dylinker.name).map_err(parse_error)?;
            observed
                .dylinkers
                .push(utf8(name, "Mach-O dylinker")?.to_owned());
        }
        LoadCommandVariant::Dylib(dylib) => {
            let name = command
                .string(endian, dylib.dylib.name)
                .map_err(parse_error)?;
            observed.dylibs.push(plain_dylib(command.cmd(), name)?);
        }
        LoadCommandVariant::Rpath(rpath) => {
            let path = command.string(endian, rpath.path).map_err(parse_error)?;
            return Err(rejected(
                Violation::SearchPath,
                format!("LC_RPATH {:?}", String::from_utf8_lossy(path)),
            ));
        }
        other => reject_forbidden(&other)?,
    }
    Ok(())
}

/// Rejects load commands that carry no string but are never acceptable in a
/// release executable.
fn reject_forbidden(
    variant: &LoadCommandVariant<'_, Endianness>,
) -> Result<(), Report<ReleaseError>> {
    let (violation, detail) = match variant {
        LoadCommandVariant::VersionMin(_) => {
            (Violation::DeploymentTarget, "LC_VERSION_MIN_* present")
        }
        LoadCommandVariant::IdDylib(_) | LoadCommandVariant::PreboundDylib(_) => {
            (Violation::LinkKind, "dylib identity in an executable")
        }
        LoadCommandVariant::DyldEnvironment(_) => {
            (Violation::SearchPath, "LC_DYLD_ENVIRONMENT present")
        }
        _ => return Ok(()),
    };
    Err(rejected(violation, detail))
}

/// Returns the install name of a plain `LC_LOAD_DYLIB`; weak, re-exported,
/// lazy and upward loads are rejected.
fn plain_dylib(cmd: u32, name: &[u8]) -> Result<String, Report<ReleaseError>> {
    let name = utf8(name, "Mach-O dylib")?;
    if cmd != format::LC_LOAD_DYLIB {
        return Err(rejected(
            Violation::LinkKind,
            format!("load command {cmd:#x} for {name:?}"),
        ));
    }
    Ok(name.to_owned())
}

/// Requires exactly one macOS `LC_BUILD_VERSION` whose `minos` equals the
/// floor; returns `(minos, sdk)`.
fn deployment_target(
    observed: &Observed,
    minimum: MacosVersion,
) -> Result<(MacosVersion, MacosVersion), Report<ReleaseError>> {
    let [(platform, minos, sdk)] = observed.build_versions.as_slice() else {
        return Err(rejected(
            Violation::DeploymentTarget,
            format!(
                "expected exactly one LC_BUILD_VERSION, found {}",
                observed.build_versions.len()
            ),
        ));
    };
    if *platform != format::PLATFORM_MACOS || *minos != minimum {
        return Err(rejected(
            Violation::DeploymentTarget,
            format!("platform {platform} minos {minos}; expected macOS minos {minimum}"),
        ));
    }
    Ok((*minos, *sdk))
}
