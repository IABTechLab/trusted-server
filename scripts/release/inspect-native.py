#!/usr/bin/env python3
"""Inspect bounded final CLI bytes without selecting an ambient external tool.

This structural gate is not startup qualification. Actual clean selected runtime
startup must independently measure these same final bytes outside the checkout.
"""
import argparse
import logging
import re
import struct
import sys
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import deterministic_json_bytes, sha256_bytes
from prepare import _regular_bytes

MAX_EXECUTABLE = 536870912
MAX_VERSION_RECORDS = 16384

LINUX_LIBRARIES = frozenset({"libc.so.6", "libm.so.6", "libdl.so.2", "libpthread.so.0",
                             "librt.so.1", "libgcc_s.so.1", "ld-linux-x86-64.so.2"})
MAC_LIBRARIES = frozenset({"/usr/lib/libSystem.B.dylib", "/usr/lib/libobjc.A.dylib",
    "/System/Library/Frameworks/Security.framework/Versions/A/Security",
    "/System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation",
    "/System/Library/Frameworks/SystemConfiguration.framework/Versions/A/SystemConfiguration"})


def _span(data, offset, size):
    if type(offset) is not int or type(size) is not int or offset < 0 or size < 0 or offset + size > len(data):
        raise ValueError("truncated native executable")
    return data[offset:offset + size]


def _unpack(data, offset, format):
    return struct.unpack(format, _span(data, offset, struct.calcsize(format)))


def _cstring(data, offset):
    if offset < 0 or offset >= len(data):
        raise ValueError("native string offset outside table")
    end = data.find(b"\0", offset)
    if end < 0 or end - offset > 4096:
        raise ValueError("native string unterminated or too long")
    try:
        return data[offset:end].decode("utf-8")
    except UnicodeError as error:
        raise ValueError("native loader string must be UTF-8") from error


def _elf(data):
    if _span(data, 0, 7) != b"\x7fELF\x02\x01\x01":
        raise ValueError("ELF64 little-endian executable required")
    kind, machine, version, _, phoff, _, _, ehsize, phsize, phnum, _, _, _ = _unpack(data, 16, "<HHIQQQIHHHHHH")
    if kind not in {2, 3} or machine != 62 or version != 1 or ehsize != 64 or phsize != 56 or not 1 <= phnum <= 128:
        raise ValueError("ELF executable architecture/header mismatch")
    headers = [_unpack(data, phoff + index * phsize, "<IIQQQQQQ") for index in range(phnum)]
    loads, dynamic, interpreters = [], [], []
    for tag, _, offset, address, _, filesz, memsz, _ in headers:
        _span(data, offset, filesz)
        if filesz > memsz:
            raise ValueError("ELF segment file size exceeds memory size")
        if tag == 1:
            loads.append((offset, address, filesz))
        elif tag == 2:
            dynamic.append((offset, filesz))
        elif tag == 3:
            interpreters.append(_cstring(_span(data, offset, filesz), 0))
    if len(dynamic) != 1 or interpreters != ["/lib64/ld-linux-x86-64.so.2"]:
        raise ValueError("ELF dynamic interpreter selection mismatch")

    def mapped(address, size):
        matching = [offset + address - base for offset, base, length in loads if base <= address and address + size <= base + length]
        if len(matching) != 1:
            raise ValueError("ELF dynamic address is not uniquely file-backed")
        return matching[0]

    offset, size = dynamic[0]
    if size % 16 or size > 65536:
        raise ValueError("ELF dynamic table bound exceeded")
    entries = []
    for index in range(size // 16):
        entry = _unpack(data, offset + 16 * index, "<qQ")
        if entry[0] == 0:
            break
        entries.append(entry)
    else:
        raise ValueError("ELF dynamic table lacks terminator")

    def one(tag):
        values = [value for key, value in entries if key == tag]
        if len(values) != 1:
            raise ValueError("ELF required dynamic selector missing or repeated")
        return values[0]

    strings = _span(data, mapped(one(5), one(10)), one(10))
    libraries = [_cstring(strings, value) for tag, value in entries if tag == 1]
    if not libraries or len(libraries) != len(set(libraries)) or any(name not in LINUX_LIBRARIES for name in libraries):
        raise ValueError("unselected native library")
    paths = [_cstring(strings, value) for tag, value in entries if tag in {15, 29}]
    if paths:
        raise ValueError("native search path is not selected")
    # DT_AUDIT, DT_DEPAUDIT, DT_FILTER and DT_AUXILIARY are loader delegation.
    if any(tag in {0x6ffffefc, 0x6ffffefb, 0x7fffffff, 0x7ffffffd} for tag, _ in entries):
        raise ValueError("unselected native loader library delegation")
    versions = []
    verneed = [value for tag, value in entries if tag == 0x6ffffffe]
    if bool(verneed) != any(tag == 0x6fffffff for tag, _ in entries):
        raise ValueError("ELF version need selectors missing or inconsistent")
    if verneed:
        count = one(0x6fffffff)
        if len(verneed) != 1 or not 1 <= count <= 1024:
            raise ValueError("ELF version need bound exceeded")
        address, visited, traversed = verneed[0], set(), 0
        for index in range(count):
            if address in visited:
                raise ValueError("ELF version need cycle")
            visited.add(address)
            version, number, file, aux, following = _unpack(data, mapped(address, 16), "<HHIII")
            if version != 1 or not 1 <= number <= 4096 or _cstring(strings, file) not in libraries:
                raise ValueError("ELF version need library mismatch")
            auxiliary, seen = address + aux, set()
            for member in range(number):
                traversed += 1
                if traversed > MAX_VERSION_RECORDS:
                    raise ValueError("ELF version traversal bound exceeded")
                if auxiliary in seen:
                    raise ValueError("ELF version auxiliary cycle")
                seen.add(auxiliary)
                _, _, _, name, next_aux = _unpack(data, mapped(auxiliary, 16), "<IHHII")
                symbol = _cstring(strings, name)
                if symbol.startswith("GLIBC_"):
                    match = re.fullmatch(r"GLIBC_([0-9]+(?:\.[0-9]+){1,2})", symbol)
                    if not match:
                        raise ValueError("unselected GLIBC symbol namespace")
                    components = tuple(map(int, match[1].split(".")))
                    if components + (0,) * (3 - len(components)) > (2, 34, 0):
                        raise ValueError("GLIBC symbol ceiling exceeds 2.34")
                    versions.append(match[1])
                if bool(next_aux) != (member < number - 1):
                    raise ValueError("ELF version auxiliary membership mismatch")
                auxiliary += next_aux
            if bool(following) != (index < count - 1):
                raise ValueError("ELF version need membership mismatch")
            address += following
    return {"format": "elf", "architecture": "x86_64", "interpreter": interpreters[0],
            "libraries": sorted(libraries), "rpaths": paths,
            "glibc_versions": sorted(set(versions), key=lambda value: tuple(map(int, value.split("."))))}


def _version(value):
    return f"{value >> 16}.{value >> 8 & 255}.{value & 255}"


def _macho(data, target):
    magic, cpu, _, kind, count, size, _, _ = _unpack(data, 0, "<IIIIIIII")
    expected = 0x100000c if target == "aarch64-apple-darwin" else 0x1000007
    if magic != 0xfeedfacf or kind != 2 or cpu != expected:
        raise ValueError("thin Mach-O executable architecture mismatch")
    commands = _span(data, 32, size)
    if not 1 <= count <= 4096 or size > 1048576:
        raise ValueError("Mach-O load command bound exceeded")
    offset, libraries, rpaths, minimum, sdk, interpreters, entrypoints = 0, [], [], [], [], [], []
    for _ in range(count):
        command, length = _unpack(commands, offset, "<II")
        if length < 8 or length % 8:
            raise ValueError("invalid Mach-O load command size")
        body = _span(commands, offset, length)
        if command in {0xc, 0x80000018, 0x8000001f, 0x20, 0x80000023}:
            name, _, _, _ = _unpack(body, 8, "<IIII")
            if name < 24:
                raise ValueError("invalid Mach-O library name")
            libraries.append(_cstring(body, name))
        elif command == 0xe:
            name, = _unpack(body, 8, "<I")
            if name < 12:
                raise ValueError("invalid Mach-O interpreter name")
            interpreters.append(_cstring(body, name))
        elif command == 0x8000001c:
            name, = _unpack(body, 8, "<I")
            rpaths.append(_cstring(body, name))
        elif command == 0x32:
            platform, version, sdk_version, tools = _unpack(body, 8, "<IIII")
            if platform != 1 or length != 24 + tools * 8:
                raise ValueError("Mach-O deployment platform mismatch")
            minimum.append(version)
            sdk.append(_version(sdk_version))
        elif command == 0x24:
            version, sdk_version = _unpack(body, 8, "<II")
            minimum.append(version)
            sdk.append(_version(sdk_version))
        elif command == 0x80000028:
            if length != 24:
                raise ValueError("invalid Mach-O main entry command")
            entrypoints.append(_unpack(body, 8, "<QQ"))
        elif command == 0x27:
            raise ValueError("Mach-O environment or loader path delegation forbidden")
        offset += length
    if offset != size or minimum != [13 << 16]:
        raise ValueError("Mach-O deployment target must be exactly 13.0")
    if not libraries or len(libraries) != len(set(libraries)) or any(name not in MAC_LIBRARIES for name in libraries):
        raise ValueError("unselected native library")
    if interpreters != ["/usr/lib/dyld"]:
        raise ValueError("unselected Mach-O interpreter")
    if len(entrypoints) != 1:
        raise ValueError("selected Mach-O main entry command required")
    if rpaths:
        raise ValueError("unselected native search path")
    return {"format": "macho", "architecture": target.split("-")[0], "libraries": sorted(libraries),
            "rpaths": rpaths, "minimum_os": "13.0.0", "sdk_version": sdk[0], "interpreter": interpreters[0], "main_entry": entrypoints[0][0]}


def inspect(path, target, *, forbidden_paths=(), operator_sentinels=()):
    """Reject unselected loaders, paths, libraries and compatibility requirements."""
    data = _regular_bytes(path, MAX_EXECUTABLE)
    if target not in {"x86_64-unknown-linux-gnu", "aarch64-apple-darwin", "x86_64-apple-darwin"}:
        raise ValueError("unknown native target")
    for value in (*forbidden_paths, *operator_sentinels):
        if not isinstance(value, str) or not value:
            raise ValueError("empty forbidden path or operator sentinel")
        if value.encode() in data:
            raise ValueError("embedded runner path or operator sentinel")
    if re.search(rb"/(?:Users|home|work|private/tmp|private/var/folders)/[A-Za-z0-9_.-]", data):
        raise ValueError("embedded runner-only path")
    result = _elf(data) if target.endswith("linux-gnu") else _macho(data, target)
    return {**result, "sha256": sha256_bytes(data), "size": len(data)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--executable", required=True, type=Path)
    parser.add_argument("--target", required=True)
    args = parser.parse_args()
    sys.stdout.buffer.write(deterministic_json_bytes(inspect(args.executable, args.target)))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError) as error:
        logging.error("Native inspection refuses: %s", error)
        sys.exit(1)
