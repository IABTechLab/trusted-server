//! Minimal hand-built ELF, Mach-O and Wasm images.
//!
//! The images contain only the structures the inspectors read; they are not
//! runnable. Building them in-process keeps the fixtures host-independent.

use object::elf as elf_format;
use object::macho as macho_format;

/// Parameters of a synthetic `x86_64` ELF executable.
pub(super) struct ElfSpec {
    pub(super) machine: u16,
    pub(super) interpreter: Option<&'static str>,
    pub(super) needed: Vec<&'static str>,
    pub(super) runpath: Option<&'static str>,
    pub(super) requirements: Vec<(&'static str, Vec<&'static str>)>,
    pub(super) payload: &'static [u8],
}

impl ElfSpec {
    /// A conforming `ts`-like executable at the glibc 2.34 ceiling.
    pub(super) fn conforming() -> Self {
        Self {
            machine: elf_format::EM_X86_64,
            interpreter: Some("/lib64/ld-linux-x86-64.so.2"),
            needed: vec!["libgcc_s.so.1", "libm.so.6", "libc.so.6"],
            runpath: None,
            requirements: vec![
                ("libgcc_s.so.1", vec!["GCC_3.0", "GCC_4.2.0"]),
                ("libc.so.6", vec!["GLIBC_2.2.5", "GLIBC_2.34", "GLIBC_2.18"]),
            ],
            payload: b"fictional rodata /opt/example/src/main.rs",
        }
    }
}

/// Little-endian byte writer.
#[derive(Default)]
struct Writer(Vec<u8>);

impl Writer {
    fn u16(&mut self, value: u16) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn align(&mut self, alignment: usize) {
        while !self.0.len().is_multiple_of(alignment) {
            self.0.push(0);
        }
    }

    fn offset(&self) -> u64 {
        u64::try_from(self.0.len()).expect("should fit u64")
    }

    /// Appends `bytes` and returns their range.
    fn append(&mut self, bytes: &[u8]) -> Range {
        let offset = self.offset();
        self.0.extend_from_slice(bytes);
        (offset, self.offset() - offset)
    }
}

/// A NUL-separated string table that returns each string's offset.
#[derive(Default)]
struct Strings(Vec<u8>);

impl Strings {
    fn new() -> Self {
        Self(vec![0])
    }

    fn add(&mut self, text: &str) -> u32 {
        let offset = u32::try_from(self.0.len()).expect("should fit u32");
        self.0.extend_from_slice(text.as_bytes());
        self.0.push(0);
        offset
    }
}

/// A `(file offset, size)` range; addresses equal offsets in these images.
type Range = (u64, u64);

/// One ELF section header record.
struct Section {
    name: u32,
    kind: u32,
    range: Range,
    link: u32,
    info: u32,
    entry_size: u64,
}

const ELF_HEADER_SIZE: u64 = 64;
const PROGRAM_HEADER_SIZE: u64 = 56;
/// Section index of `.dynstr`, linked from `.dynamic` and `.gnu.version_r`.
const DYNSTR_INDEX: u32 = 1;

/// Builds a synthetic ELF64 executable from `spec`.
pub(super) fn elf(spec: &ElfSpec) -> Vec<u8> {
    let mut dynstr = Strings::new();
    let mut dynamic = spec
        .needed
        .iter()
        .map(|name| (elf_format::DT_NEEDED, u64::from(dynstr.add(name))))
        .collect::<Vec<_>>();
    if let Some(path) = spec.runpath {
        dynamic.push((elf_format::DT_RUNPATH, u64::from(dynstr.add(path))));
    }
    let verneed = verneed(&spec.requirements, &mut dynstr);
    let mut symbols = Writer::default();
    symbols.0.resize(24, 0);
    let mut versions = Writer::default();
    versions.u16(0);
    for index in 0..spec.requirements.iter().map(|(_, names)| names.len()).sum() {
        symbols.u32(dynstr.add(&format!("fictional_symbol_{index}")));
        symbols.0.extend_from_slice(&[0x12, 0]);
        symbols.u16(0);
        symbols.u64(0);
        symbols.u64(0);
        versions.u16(u16::try_from(index + 2).expect("should fit version index"));
    }
    let segments = 3 + u64::from(spec.interpreter.is_some());
    let mut body = Writer::default();
    body.0.resize(
        usize::try_from(ELF_HEADER_SIZE + segments * PROGRAM_HEADER_SIZE).expect("should fit"),
        0,
    );
    let interp = body.append(&spec.interpreter.map(nul_terminated).unwrap_or_default());
    let dynstr_range = body.append(&dynstr.0);
    body.align(4);
    let verneed_range = body.append(&verneed);
    body.align(8);
    let symbols_range = body.append(&symbols.0);
    let versions_range = body.append(&versions.0);
    body.align(8);
    dynamic.push((elf_format::DT_STRTAB, dynstr_range.0));
    dynamic.push((elf_format::DT_STRSZ, dynstr_range.1));
    dynamic.push((elf_format::DT_VERNEED, verneed_range.0));
    dynamic.push((elf_format::DT_VERNEEDNUM, spec.requirements.len() as u64));
    dynamic.push((elf_format::DT_SYMTAB, symbols_range.0));
    dynamic.push((elf_format::DT_SYMENT, 24));
    dynamic.push((elf_format::DT_VERSYM, versions_range.0));
    dynamic.push((elf_format::DT_NULL, 0));
    dynamic.push((elf_format::DT_NULL, 0));
    let dynamic_range = body.append(&encode_dynamic(&dynamic));
    body.0.extend_from_slice(spec.payload);
    let count = u32::try_from(spec.requirements.len()).expect("should fit");
    let table = section_table(
        &mut body,
        [
            dynstr_range,
            dynamic_range,
            verneed_range,
            symbols_range,
            versions_range,
        ],
        count,
    );
    let mut header = Writer::default();
    write_elf_header(&mut header, spec.machine, segments, body.offset(), &table);
    write_sections(&mut body, &table);
    if spec.interpreter.is_some() {
        program_header(&mut header, elf_format::PT_INTERP, interp);
    }
    program_header(&mut header, elf_format::PT_DYNAMIC, dynamic_range);
    program_header(&mut header, elf_format::PT_NOTE, (0, 0));
    program_header(&mut header, elf_format::PT_LOAD, (0, body.offset()));
    body.0[..header.0.len()].copy_from_slice(&header.0);
    body.0
}

fn nul_terminated(text: &str) -> Vec<u8> {
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(0);
    bytes
}

fn encode_dynamic(entries: &[(u32, u64)]) -> Vec<u8> {
    let mut out = Writer::default();
    for (tag, value) in entries {
        out.u64(u64::from(*tag));
        out.u64(*value);
    }
    out.0
}

/// Appends `.shstrtab` to `body` and returns the non-null section headers
/// for `[.dynstr, .dynamic, .gnu.version_r]` plus `.shstrtab`.
fn section_table(body: &mut Writer, ranges: [Range; 5], requirements: u32) -> Vec<Section> {
    let mut names = Strings::new();
    let [dynstr, dynamic, verneed, symbols, versions] = ranges;
    let mut table = vec![
        Section {
            name: names.add(".dynstr"),
            kind: elf_format::SHT_STRTAB,
            range: dynstr,
            link: 0,
            info: 0,
            entry_size: 0,
        },
        Section {
            name: names.add(".dynamic"),
            kind: elf_format::SHT_DYNAMIC,
            range: dynamic,
            link: DYNSTR_INDEX,
            info: 0,
            entry_size: 16,
        },
        Section {
            name: names.add(".gnu.version_r"),
            kind: elf_format::SHT_GNU_VERNEED,
            range: verneed,
            link: DYNSTR_INDEX,
            info: requirements,
            entry_size: 0,
        },
        Section {
            name: names.add(".dynsym"),
            kind: elf_format::SHT_DYNSYM,
            range: symbols,
            link: DYNSTR_INDEX,
            info: 1,
            entry_size: 24,
        },
        Section {
            name: names.add(".gnu.version"),
            kind: elf_format::SHT_GNU_VERSYM,
            range: versions,
            link: 4,
            info: 0,
            entry_size: 2,
        },
    ];
    let name = names.add(".shstrtab");
    let range = body.append(&names.0);
    body.align(8);
    table.push(Section {
        name,
        kind: elf_format::SHT_STRTAB,
        range,
        link: 0,
        info: 0,
        entry_size: 0,
    });
    table
}

/// Encodes `.gnu.version_r` entries against `strings`.
fn verneed(requirements: &[(&str, Vec<&str>)], strings: &mut Strings) -> Vec<u8> {
    let mut out = Writer::default();
    let mut version_index = 2;
    for (index, (file, names)) in requirements.iter().enumerate() {
        let last_need = index + 1 == requirements.len();
        let need_size = 16 + 16 * names.len();
        out.u16(1);
        out.u16(u16::try_from(names.len()).expect("should fit"));
        out.u32(strings.add(file));
        out.u32(16);
        out.u32(if last_need {
            0
        } else {
            u32::try_from(need_size).expect("should fit")
        });
        for (aux, name) in names.iter().enumerate() {
            out.u32(0);
            out.u16(0);
            out.u16(version_index);
            version_index += 1;
            out.u32(strings.add(name));
            out.u32(if aux + 1 == names.len() { 0 } else { 16 });
        }
    }
    out.0
}

fn write_elf_header(
    out: &mut Writer,
    machine: u16,
    segments: u64,
    section_offset: u64,
    table: &[Section],
) {
    out.0.extend_from_slice(&elf_format::ELFMAG);
    out.0.extend_from_slice(&[
        elf_format::ELFCLASS64,
        elf_format::ELFDATA2LSB,
        elf_format::EV_CURRENT,
    ]);
    out.0.resize(16, 0);
    out.u16(elf_format::ET_DYN);
    out.u16(machine);
    out.u32(u32::from(elf_format::EV_CURRENT));
    out.u64(0);
    out.u64(ELF_HEADER_SIZE);
    out.u64(section_offset);
    out.u32(0);
    out.u16(64);
    out.u16(56);
    out.u16(u16::try_from(segments).expect("should fit"));
    out.u16(64);
    out.u16(u16::try_from(table.len() + 1).expect("should fit"));
    out.u16(u16::try_from(table.len()).expect("should fit"));
}

fn program_header(out: &mut Writer, kind: u32, (offset, size): Range) {
    out.u32(kind);
    out.u32(elf_format::PF_R);
    out.u64(offset);
    out.u64(offset);
    out.u64(offset);
    out.u64(size);
    out.u64(size);
    out.u64(8);
}

fn write_sections(out: &mut Writer, table: &[Section]) {
    out.0.extend_from_slice(&[0; 64]);
    for section in table {
        out.u32(section.name);
        out.u32(section.kind);
        out.u64(0);
        out.u64(section.range.0);
        out.u64(section.range.0);
        out.u64(section.range.1);
        out.u32(section.link);
        out.u32(section.info);
        out.u64(1);
        out.u64(section.entry_size);
    }
}

/// One synthetic Mach-O load command.
pub(super) enum Command {
    BuildVersion { platform: u32, minos: u32 },
    VersionMin { version: u32 },
    Dylinker(&'static str),
    Dylib { cmd: u32, name: &'static str },
    Rpath(&'static str),
}

/// Parameters of a synthetic thin 64-bit Mach-O executable.
pub(super) struct MachoSpec {
    pub(super) cputype: u32,
    pub(super) cpusubtype: u32,
    pub(super) commands: Vec<Command>,
    pub(super) payload: &'static [u8],
}

/// Encodes `major.minor` in the Mach-O nibble form.
pub(super) const fn macos(major: u32, minor: u32) -> u32 {
    (major << 16) | (minor << 8)
}

impl MachoSpec {
    /// A conforming arm64 `ts`-like executable targeting macOS 13.0.
    pub(super) fn conforming() -> Self {
        Self {
            cputype: macho_format::CPU_TYPE_ARM64,
            cpusubtype: macho_format::CPU_SUBTYPE_ARM64_ALL,
            commands: vec![
                Command::BuildVersion {
                    platform: macho_format::PLATFORM_MACOS,
                    minos: macos(13, 0),
                },
                Command::Dylinker("/usr/lib/dyld"),
                Command::Dylib {
                    cmd: macho_format::LC_LOAD_DYLIB,
                    name: "/System/Library/Frameworks/Security.framework/Versions/A/Security",
                },
                Command::Dylib {
                    cmd: macho_format::LC_LOAD_DYLIB,
                    name: "/usr/lib/libSystem.B.dylib",
                },
            ],
            payload: b"fictional cstring /opt/example/src/main.rs",
        }
    }
}

/// Builds a synthetic thin Mach-O executable from `spec`.
pub(super) fn macho(spec: &MachoSpec) -> Vec<u8> {
    let mut commands = Writer::default();
    for command in &spec.commands {
        encode_command(&mut commands, command);
    }
    let mut out = Writer::default();
    out.u32(macho_format::MH_MAGIC_64);
    out.u32(spec.cputype);
    out.u32(spec.cpusubtype);
    out.u32(macho_format::MH_EXECUTE);
    out.u32(u32::try_from(spec.commands.len()).expect("should fit"));
    out.u32(u32::try_from(commands.0.len()).expect("should fit"));
    out.u32(0);
    out.u32(0);
    out.0.extend_from_slice(&commands.0);
    out.0.extend_from_slice(spec.payload);
    out.0
}

fn encode_command(out: &mut Writer, command: &Command) {
    match command {
        Command::BuildVersion { platform, minos } => {
            out.u32(macho_format::LC_BUILD_VERSION);
            out.u32(24);
            out.u32(*platform);
            out.u32(*minos);
            out.u32(macos(15, 5));
            out.u32(0);
        }
        Command::VersionMin { version } => {
            out.u32(macho_format::LC_VERSION_MIN_MACOSX);
            out.u32(16);
            out.u32(*version);
            out.u32(macos(15, 5));
        }
        Command::Dylinker(name) => string_command(out, macho_format::LC_LOAD_DYLINKER, &[], name),
        Command::Dylib { cmd, name } => {
            string_command(out, *cmd, &[2, 0x0001_0000, 0x0001_0000], name);
        }
        Command::Rpath(path) => string_command(out, macho_format::LC_RPATH, &[], path),
    }
}

/// Encodes a command whose string follows `fields` after the name offset.
fn string_command(out: &mut Writer, cmd: u32, fields: &[u32], text: &str) {
    let fixed = 12 + 4 * fields.len();
    let size = (fixed + text.len() + 1).div_ceil(8) * 8;
    out.u32(cmd);
    out.u32(u32::try_from(size).expect("should fit"));
    out.u32(u32::try_from(fixed).expect("should fit"));
    for field in fields {
        out.u32(*field);
    }
    out.0.extend_from_slice(text.as_bytes());
    out.0.resize(out.0.len() + size - fixed - text.len(), 0);
}

/// Builds a two-architecture fat header around `thin`.
pub(super) fn fat(thin: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for value in [
        macho_format::FAT_MAGIC,
        1,
        macho_format::CPU_TYPE_ARM64,
        0,
        4096,
    ] {
        out.extend_from_slice(&value.to_be_bytes());
    }
    for value in [u32::try_from(thin.len()).expect("should fit"), 12] {
        out.extend_from_slice(&value.to_be_bytes());
    }
    out.resize(4096, 0);
    out.extend_from_slice(thin);
    out
}

/// Builds a core Wasm module importing `() -> ()` functions.
pub(super) fn wasm_module(imports: &[(&str, &str)]) -> Vec<u8> {
    let mut out = b"\0asm\x01\0\0\0".to_vec();
    if imports.is_empty() {
        return out;
    }
    section(&mut out, 1, &[1, 0x60, 0, 0]);
    let mut body = vec![u8::try_from(imports.len()).expect("should fit")];
    for (module, name) in imports {
        name_bytes(&mut body, module);
        name_bytes(&mut body, name);
        body.extend_from_slice(&[0, 0]);
    }
    section(&mut out, 2, &body);
    out
}

/// Appends a section with a single-byte LEB128 length.
pub(super) fn section(out: &mut Vec<u8>, id: u8, body: &[u8]) {
    out.push(id);
    out.push(u8::try_from(body.len()).expect("should fit one LEB128 byte"));
    out.extend_from_slice(body);
}

fn name_bytes(out: &mut Vec<u8>, text: &str) {
    out.push(u8::try_from(text.len()).expect("should fit one LEB128 byte"));
    out.extend_from_slice(text.as_bytes());
}

/// Builds a core Wasm module importing `() -> ()` functions, then appending
/// custom sections `(name, payload)` in order.
pub(super) fn wasm_module_with_customs(
    imports: &[(&str, &str)],
    customs: &[(&str, &[u8])],
) -> Vec<u8> {
    let mut out = wasm_module(imports);
    for (name, payload) in customs {
        let mut body = Vec::new();
        name_bytes(&mut body, name);
        body.extend_from_slice(payload);
        section(&mut out, 0, &body);
    }
    out
}
