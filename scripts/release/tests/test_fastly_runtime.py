"""Fastly final-byte acceptance negatives and response fixture contract."""
import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path

RELEASE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(RELEASE))
SPEC = importlib.util.spec_from_file_location("validate_fastly", RELEASE / "validate-fastly.py")
fastly = None
if SPEC and SPEC.loader and Path(SPEC.origin).exists():
    fastly = importlib.util.module_from_spec(SPEC)
    SPEC.loader.exec_module(fastly)


class FastlyTests(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(fastly, "canonical Fastly validator is missing")

    def test_non_wasm_and_component_rejected(self):
        for data in (b"hello", b"\0asm\x0d\0\x01\0", b"\0asm\x01\0\0\0"):
            with self.subTest(data=data), self.assertRaisesRegex(ValueError, "Wasm|ABI"):
                fastly.core_wasm(data)

    def test_health_only_is_not_positive_publisher_fixture(self):
        with self.assertRaisesRegex(ValueError, "publisher"):
            fastly.publisher_response(200, b"healthy", {})

    def test_injection_and_origin_marker_required(self):
        with self.assertRaisesRegex(ValueError, "injection"):
            fastly.publisher_response(200, b"<html><head></head><body>release-origin-example</body></html>", {"content-type": "text/html"})

    def test_wrong_abi_rejected(self):
        # A valid empty module has no Fastly imports or WASI request entry.
        with self.assertRaisesRegex(ValueError, "ABI"):
            fastly.core_wasm(b"\0asm\x01\0\0\0")

class RuntimeConfigTests(unittest.TestCase):
    def test_source_owned_runtime_config_matches_manifest_store(self):
        self.assertIsNotNone(fastly, "canonical Fastly validator is missing")
        with tempfile.TemporaryDirectory() as directory:
            import tomllib
            destination = Path(directory) / "viceroy.toml"
            fastly.runtime_config(RELEASE.parents[1], destination, 12345, "A")
            generated = tomllib.loads(destination.read_text())
            manifest = tomllib.loads((RELEASE.parents[1] / "edgezero.toml").read_text())
            store = manifest["stores"]["config"]["default"]
            self.assertIn(store, generated["local_server"]["config_stores"])

class AbiFramingTests(unittest.TestCase):
    def test_core_framing_accepts_required_abi_before_maintained_validation(self):
        # Framing-only bytes deliberately omit instruction/type semantics; the
        # maintained validator must still reject these as a complete program.
        imports = bytes([2, 15]) + b"fastly_http_req" + bytes([4]) + b"send" + bytes([0, 0])
        imports += bytes([22]) + b"wasi_snapshot_preview1" + bytes([8]) + b"fd_write" + bytes([0, 0])
        exports = bytes([1, 6]) + b"_start" + bytes([0, 0])
        data = b"\0asm\x01\0\0\0" + bytes([2, len(imports)]) + imports + bytes([7, len(exports)]) + exports
        self.assertEqual(fastly.core_wasm(data)["abi"], "wasip1-fastly-default")

    def test_positive_full_publisher_fixture_extracts_script(self):
        path = b"/static/tsjs=tsjs-unified.min.js?v=" + b"a" * 64
        html = b'<html><head><script src="' + path + b'" id="trustedserver-js"></script></head><body>release-origin-example</body></html>'
        self.assertEqual(fastly.publisher_response(200, html, {"content-type": "text/html"}), path.decode())

class ExactJavascriptTests(unittest.TestCase):
    def test_unrelated_javascript_success_is_rejected(self):
        expected = b"expected example core;\nexpected example creative"
        path = "/static/tsjs=tsjs-unified.min.js?v=" + fastly.sha256_bytes(expected)
        with self.assertRaisesRegex(ValueError, "JavaScript.*bytes"):
            fastly.javascript_response(200, b"unrelated example", {"content-type": "application/javascript"}, path, expected)
        wrong = path[:-1] + ("b" if path[-1] != "b" else "c")
        with self.assertRaisesRegex(ValueError, "hash"):
            fastly.javascript_response(200, expected, {"content-type": "application/javascript"}, wrong, expected)
        self.assertEqual(fastly.javascript_response(200, expected, {"content-type": "application/javascript"}, path, expected), fastly.sha256_bytes(expected))

    def test_expected_runtime_bundle_uses_exact_prepared_modules_and_separator(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "tsjs-core.js").write_bytes(b"example core\n")
            (root / "tsjs-creative.js").write_bytes(b"example creative\n")
            self.assertEqual(fastly.expected_javascript(RELEASE.parents[1], root), b"example core\n;\nexample creative\n")


class RuntimeLogBoundsTests(unittest.TestCase):
    def test_live_runtime_logs_are_bounded_and_process_is_reaped(self):
        import subprocess
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            process = subprocess.Popen([sys.executable, "-I", "-c", "import sys,time;sys.stdout.write('x'*100000);sys.stdout.flush();time.sleep(30)"], stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
            logs = None
            try:
                logs = fastly.RuntimeLogs(process, root / "viceroy", maximum=1024)
                self.assertTrue(logs.failure.wait(3), "live log bound should trigger")
                with self.assertRaisesRegex(ValueError, "bound"):
                    logs.check()
            finally:
                if logs is not None:
                    logs.close()
                else:
                    process.kill()
                    process.wait()
            self.assertIsNotNone(process.poll())
            self.assertLessEqual((root / "viceroy.stdout").stat().st_size, 1024)
