"""Binary format negatives use bounded synthetic fixtures, never qualification."""
import importlib.util
import struct
import sys
import tempfile
import unittest
from pathlib import Path

RELEASE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(RELEASE))
SPEC = importlib.util.spec_from_file_location("inspect_native", RELEASE / "inspect-native.py")
native = None
if SPEC and SPEC.loader and Path(SPEC.origin).exists():
    native = importlib.util.module_from_spec(SPEC)
    SPEC.loader.exec_module(native)


def elf(library="libc.so.6", version="GLIBC_2.34", rpath=None):
    strings = b"\0" + library.encode() + b"\0" + version.encode() + b"\0"
    version_offset = len(library) + 2
    entries = [(1, 1), (5, 0x400300), (10, len(strings)), (0x6ffffffe, 0x400400), (0x6fffffff, 1)]
    if rpath is not None:
        entries.insert(0, (29, len(strings)))
        strings += rpath.encode() + b"\0"
        entries = [(tag, len(strings) if tag == 10 else value) for tag, value in entries]
    entries.append((0, 0))
    data = bytearray(0x500)
    data[:16] = b"\x7fELF\x02\x01\x01" + b"\0" * 9
    struct.pack_into("<HHIQQQIHHHHHH", data, 16, 3, 62, 1, 0, 64, 0, 0, 64, 56, 3, 0, 0, 0)
    struct.pack_into("<IIQQQQQQ", data, 64, 1, 5, 0, 0x400000, 0, len(data), len(data), 4096)
    struct.pack_into("<IIQQQQQQ", data, 120, 2, 4, 0x200, 0x400200, 0, len(entries) * 16, len(entries) * 16, 8)
    loader = b"/lib64/ld-linux-x86-64.so.2\0"
    struct.pack_into("<IIQQQQQQ", data, 176, 3, 4, 0x280, 0, 0, len(loader), len(loader), 1)
    for index, entry in enumerate(entries):
        struct.pack_into("<qQ", data, 0x200 + index * 16, *entry)
    data[0x280:0x280 + len(loader)] = loader
    data[0x300:0x300 + len(strings)] = strings
    struct.pack_into("<HHIII", data, 0x400, 1, 1, 1, 16, 0)
    struct.pack_into("<IHHII", data, 0x410, 0, 0, 2, version_offset, 0)
    return bytes(data)


def macho(minimum=(13, 0, 0), library="/usr/lib/libSystem.B.dylib", cpu=0x100000c, *, with_loader=True):
    name = library.encode() + b"\0"
    size = (24 + len(name) + 7) // 8 * 8
    command = struct.pack("<IIIIII", 0xc, size, 24, 0, 0, 0) + name
    command += b"\0" * (size - len(command))
    packed = minimum[0] << 16 | minimum[1] << 8 | minimum[2]
    commands = struct.pack("<IIIIII", 0x32, 24, 1, packed, 15 << 16, 0) + command
    count = 2
    if with_loader:
        name = b"/usr/lib/dyld\0"
        length = (12 + len(name) + 7) // 8 * 8
        command = struct.pack("<III", 0xe, length, 12) + name
        commands += command + b"\0" * (length - len(command))
        commands += struct.pack("<IIQQ", 0x80000028, 24, 0, 0)
        count += 2
    return struct.pack("<IIIIIIII", 0xfeedfacf, cpu, 0, 2, count, len(commands), 0, 0) + commands


class NativeTests(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(native, "canonical native inspector is missing")

    def inspect(self, data, target="x86_64-unknown-linux-gnu"):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "ts"
            path.write_bytes(data)
            return native.inspect(path, target, forbidden_paths=("/private/example-build",))

    def test_elf_selected_library_and_ceiling(self):
        self.assertEqual(self.inspect(elf())["glibc_versions"], ["2.34"])

    def test_unselected_native_library(self):
        with self.assertRaisesRegex(ValueError, "library"):
            self.inspect(elf(library="libexample.so.1"))

    def test_numeric_glibc_ceiling(self):
        for version in ("GLIBC_2.35", "GLIBC_2.100", "GLIBC_PRIVATE"):
            with self.subTest(version=version), self.assertRaisesRegex(ValueError, "GLIBC"):
                self.inspect(elf(version=version))

    def test_runner_only_path(self):
        with self.assertRaisesRegex(ValueError, "path"):
            self.inspect(elf() + b"/private/example-build/source/config")

    def test_rpath_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "path"):
            self.inspect(elf(rpath="/work/vendor/lib"))

    def test_macho_deployment_target(self):
        self.assertEqual(self.inspect(macho(), "aarch64-apple-darwin")["minimum_os"], "13.0.0")
        with self.assertRaisesRegex(ValueError, "deployment"):
            self.inspect(macho((14, 0, 0)), "aarch64-apple-darwin")

    def test_macho_wrong_architecture(self):
        with self.assertRaisesRegex(ValueError, "architecture"):
            self.inspect(macho(cpu=0x1000007), "aarch64-apple-darwin")

    def test_truncated_and_non_executable(self):
        for data in (b"hello", elf()[:180], macho()[:33]):
            with self.subTest(length=len(data)), self.assertRaises(ValueError):
                self.inspect(data)

    def test_macho_unselected_library(self):
        with self.assertRaisesRegex(ValueError, "library"):
            self.inspect(macho(library="/opt/example/lib.dylib"), "aarch64-apple-darwin")

class RealLocalMacTests(unittest.TestCase):
    @unittest.skipUnless(sys.platform == "darwin", "actual local macOS fixture only; never macOS15 qualification")
    def test_actual_compiled_macho_with_lc_main(self):
        import subprocess
        self.assertIsNotNone(native, "canonical native inspector is missing")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source = root / "example.c"
            source.write_text("int main(void) { return 0; }\n")
            executable = root / "example"
            result = subprocess.run(["/usr/bin/clang", "-mmacosx-version-min=13.0", str(source), "-o", str(executable)], capture_output=True, timeout=30, check=False)
            self.assertEqual(result.returncode, 0, result.stderr.decode())
            target = "aarch64-apple-darwin" if __import__("platform").machine() == "arm64" else "x86_64-apple-darwin"
            observed = native.inspect(executable, target)
            self.assertEqual(observed["minimum_os"], "13.0.0")
            self.assertEqual(observed["interpreter"], "/usr/lib/dyld")
            startup = subprocess.run([str(executable)], cwd=root, env={"HOME": str(root), "PATH": "/usr/bin:/bin"}, capture_output=True, timeout=10, check=False)
            self.assertEqual(startup.returncode, 0)

    def test_arbitrary_macho_dylinker_rejected(self):
        self.assertIsNotNone(native, "canonical native inspector is missing")
        data = bytearray(macho())
        name = b"/opt/example/dyld\0"
        size = (12 + len(name) + 7) // 8 * 8
        command = struct.pack("<III", 0xe, size, 12) + name
        command += b"\0" * (size - len(command))
        count, size_before = struct.unpack_from("<II", data, 16)
        struct.pack_into("<II", data, 16, count + 1, size_before + size)
        data.extend(command)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "ts"
            path.write_bytes(data)
            with self.assertRaisesRegex(ValueError, "interpreter"):
                native.inspect(path, "aarch64-apple-darwin")

class MachEntryTests(unittest.TestCase):
    def test_missing_dyld_or_main_is_rejected(self):
        self.assertIsNotNone(native, "canonical native inspector is missing")
        # The earlier minimal fixture intentionally lacks entry/loader commands.
        data = macho(with_loader=False)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "ts"
            path.write_bytes(data)
            with self.assertRaisesRegex(ValueError, "interpreter|entry"):
                native.inspect(path, "aarch64-apple-darwin")
