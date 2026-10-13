"""Tests for strict release document utilities."""

import hashlib
import io
import os
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import common


class JsonBytesTests(unittest.TestCase):
    def test_rejects_duplicate_keys(self):
        with self.assertRaisesRegex(ValueError, "duplicate"):
            common.load_json_bytes(b'{"name":1,"name":2}', max_bytes=1024)

    def test_rejects_nonfinite_numbers(self):
        for number in (b"NaN", b"Infinity", b"-Infinity", b"1e999", b"-1e999"):
            with self.subTest(number=number):
                with self.assertRaisesRegex(ValueError, "finite"):
                    common.load_json_bytes(b'{"value":' + number + b"}", max_bytes=1024)

    def test_rejects_invalid_utf8_and_unpaired_surrogates(self):
        for data in (
            b'"\xff"',
            b'"\xed\xa0\x80"',
            b'"\\ud800"',
            b'"\\udfff"',
            b'{"\\ud800":0}',
        ):
            with self.subTest(data=data):
                with self.assertRaises(ValueError):
                    common.load_json_bytes(data, max_bytes=1024)

    def test_rejects_trailing_controls_and_accepts_json_whitespace(self):
        for control in (b"\x00", b"\x0b", b"\x0c", b"\x1f"):
            with self.subTest(control=control):
                with self.assertRaises(ValueError):
                    common.load_json_bytes(b"{}" + control, max_bytes=1024)
        self.assertEqual(
            common.load_json_bytes(b" \t\r\n{} \t\r\n", max_bytes=1024),
            {},
            "should accept only the JSON whitespace characters",
        )

    def test_accepts_valid_unicode_and_finite_numbers(self):
        self.assertEqual(
            common.load_json_bytes(
                b'{"value":1.25,"text":"\\ud83d\\ude00"}', max_bytes=1024
            ),
            {"value": 1.25, "text": "😀"},
            "should preserve finite numbers and paired surrogate escapes",
        )

    def test_rejects_container_depth_65_and_accepts_64(self):
        with self.assertRaisesRegex(ValueError, "depth"):
            common.load_json_bytes(b"[" * 65 + b"0" + b"]" * 65, max_bytes=1024)
        document = common.load_json_bytes(
            b"[" * 64 + b"0" + b"]" * 64, max_bytes=1024
        )
        for _ in range(64):
            self.assertIsInstance(document, list, "should accept every allowed level")
            document = document[0]
        self.assertEqual(document, 0, "should preserve the nested scalar")

    def test_depth_check_ignores_brackets_and_escaped_quotes_in_strings(self):
        data = b'{"text":"[\\\"{]"}'
        self.assertEqual(
            common.load_json_bytes(data, max_bytes=1024, max_depth=1),
            {"text": '["{]'},
            "should count containers rather than bracket characters in strings",
        )

    def test_rejects_oversized_bytes_before_decoding(self):
        with self.assertRaisesRegex(ValueError, "size"):
            common.load_json_bytes(b"\xff" * 5, max_bytes=4)
        self.assertEqual(
            common.load_json_bytes(b"{}", max_bytes=2),
            {},
            "should accept exactly the configured byte limit",
        )

    def test_requires_bytes_and_nonnegative_integer_limits(self):
        with self.assertRaisesRegex(ValueError, "bytes"):
            common.load_json_bytes("{}", max_bytes=1024)
        for invalid in (-1, True, False, 1.5, "4"):
            with self.subTest(invalid=invalid):
                with self.assertRaisesRegex(ValueError, "integer"):
                    common.load_json_bytes(b"{}", max_bytes=invalid)
                with self.assertRaisesRegex(ValueError, "integer"):
                    common.load_json_bytes(b"{}", max_bytes=1024, max_depth=invalid)


class IntegerTests(unittest.TestCase):
    def test_rejects_negative_lengths_and_booleans(self):
        for value in (-1, True, False, 1.0, "1", None):
            with self.subTest(value=value):
                with self.assertRaisesRegex(ValueError, "integer"):
                    common.require_nonnegative_integer(value)
        for value in (0, 1, 512 * 1024 * 1024):
            self.assertEqual(
                common.require_nonnegative_integer(value),
                value,
                "should preserve a valid nonnegative integer",
            )


class BoundedReadTests(unittest.TestCase):
    def test_rejects_oversized_stream_without_unbounded_read(self):
        stream = io.BytesIO(b"x" * 10)
        with self.assertRaisesRegex(ValueError, "size"):
            common.read_bounded(stream, max_bytes=4)
        self.assertEqual(stream.tell(), 5, "should read only the limit and one byte")

    def test_accepts_limit_and_short_stream_reads(self):
        class ShortReads(io.BytesIO):
            def read(self, size=-1):
                return super().read(min(size, 2))

        for stream in (io.BytesIO(b"1234"), ShortReads(b"1234")):
            with self.subTest(stream=type(stream).__name__):
                self.assertEqual(
                    common.read_bounded(stream, max_bytes=4),
                    b"1234",
                    "should read through short reads up to EOF",
                )
        self.assertEqual(
            common.read_bounded(io.BytesIO(), max_bytes=0),
            b"",
            "should accept empty data at a zero byte limit",
        )

    def test_rejects_invalid_stream_limits_and_text_streams(self):
        for limit in (-1, True):
            with self.subTest(limit=limit):
                with self.assertRaisesRegex(ValueError, "integer"):
                    common.read_bounded(io.BytesIO(b"{}"), max_bytes=limit)
        with self.assertRaisesRegex(ValueError, "binary"):
            common.read_bounded(io.StringIO("{}"), max_bytes=4)

    def test_read_json_file_checks_actual_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "index.json"
            path.write_bytes(b'{"name":"example"}')
            with self.assertRaisesRegex(ValueError, "size"):
                common.read_json_file(path, max_bytes=4)
            self.assertEqual(
                common.read_json_file(path, max_bytes=1024),
                {"name": "example"},
                "should parse a document read from bounded binary data",
            )


class DigestTests(unittest.TestCase):
    def test_sha256_hashes_original_bytes(self):
        compact = b'{"name":"example"}'
        spaced = b'{ "name" : "example" }\n'
        self.assertEqual(
            common.load_json_bytes(compact, max_bytes=1024),
            common.load_json_bytes(spaced, max_bytes=1024),
            "should parse both byte representations to the same document",
        )
        for data in (compact, spaced):
            self.assertEqual(
                common.sha256_bytes(data),
                hashlib.sha256(data).hexdigest(),
                "should match the digest of the exact original bytes",
            )
        self.assertNotEqual(
            common.sha256_bytes(compact),
            common.sha256_bytes(spaced),
            "should distinguish different serialization bytes",
        )

    def test_sha256_rejects_text(self):
        with self.assertRaisesRegex(ValueError, "bytes"):
            common.sha256_bytes("{}")


class AssetNameTests(unittest.TestCase):
    def test_rejects_unsafe_flat_asset_names(self):
        for name in (
            "", ".", "..", "../index.json", "a/b", "a\\b", "/index.json",
            "C:index.json", "a\x00b", "a\nb", "a\x7fb", ".hidden",
            "-option", "a b", "é.json", "a%2fb", None, 12,
        ):
            with self.subTest(name=name):
                with self.assertRaisesRegex(ValueError, "asset name"):
                    common.require_safe_asset_name(name)
        for name in ("index.json", "trusted-server-linux-x86_64.tar.gz", "a_b-1.wasm"):
            self.assertEqual(
                common.require_safe_asset_name(name), name,
                "should preserve an unambiguous flat ASCII asset name",
            )

    def test_rejects_names_outside_release_asset_contract(self):
        for name in ("name+extra", "a..b", "a.", "a" * 161):
            with self.subTest(name=name):
                with self.assertRaisesRegex(ValueError, "asset name"):
                    common.require_safe_asset_name(name)
        self.assertEqual(
            common.require_safe_asset_name("a" * 160), "a" * 160,
            "should accept the maximum contract name length",
        )


class IdentityTests(unittest.TestCase):
    def test_rejects_ambiguous_duplicate_identities(self):
        records = [
            {"kind": "asset", "name": "index.json", "sha256": "a" * 64},
            {"kind": "asset", "name": "index.json", "sha256": "b" * 64},
        ]
        with self.assertRaisesRegex(ValueError, "duplicate identity"):
            common.require_unique_identities(records, ("kind", "name"))

    def test_accepts_distinct_composite_identities(self):
        common.require_unique_identities(
            [{"kind": "asset", "name": "index.json"},
             {"kind": "statement", "name": "index.json"}],
            ("kind", "name"),
        )

    def test_rejects_missing_and_nonscalar_identity_fields(self):
        for record in ({}, {"name": []}, {"name": {}}, {"name": True}, {"name": None}):
            with self.subTest(record=record):
                with self.assertRaisesRegex(ValueError, "identity"):
                    common.require_unique_identities([record], ("name",))


class SerializationTests(unittest.TestCase):
    def test_deterministic_json_bytes_sorts_keys_and_uses_utf8(self):
        expected = '{"a":"é","z":{"a":1,"b":2}}\n'.encode("utf-8")
        for document in (
            {"z": {"b": 2, "a": 1}, "a": "é"},
            {"a": "é", "z": {"a": 1, "b": 2}},
        ):
            self.assertEqual(
                common.deterministic_json_bytes(document), expected,
                "should write compact sorted UTF-8 JSON with a final newline",
            )

    def test_serialization_rejects_nonfinite_and_unpaired_surrogates(self):
        for value in (float("nan"), float("inf"), float("-inf"), "\ud800"):
            with self.subTest(value=repr(value)):
                with self.assertRaises(ValueError):
                    common.deterministic_json_bytes({"value": value})

    def test_serialization_rejects_nonstring_object_keys(self):
        with self.assertRaisesRegex(ValueError, "keys"):
            common.deterministic_json_bytes({1: "example"})

    def test_serialization_rejects_cycles_promptly(self):
        source = """
import sys
sys.path.insert(0, sys.argv[1])
import common
document = []
document.append(document)
try:
    common.deterministic_json_bytes(document)
except ValueError:
    sys.exit(0)
raise AssertionError('should reject a cyclic producer document')
"""
        try:
            result = subprocess.run(
                [sys.executable, "-c", source, str(Path(common.__file__).parent)],
                capture_output=True, text=True, timeout=2, check=False,
            )
        except subprocess.TimeoutExpired:
            self.fail("should reject a cyclic producer document promptly")
        self.assertEqual(
            result.returncode, 0, f"should reject cyclic documents: {result.stderr}"
        )


class AtomicWriteTests(unittest.TestCase):
    def test_atomic_write_replaces_existing_bytes_privately(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "index.json"
            path.write_bytes(b"old")
            common.atomic_write(path, b"new")
            self.assertEqual(path.read_bytes(), b"new", "should replace the old bytes")
            self.assertEqual(
                stat.S_IMODE(path.stat().st_mode), 0o600,
                "should publish the private temporary file's permissions",
            )
            self.assertEqual(
                list(Path(directory).iterdir()), [path],
                "should leave no temporary files after success",
            )

    def test_atomic_write_failure_preserves_old_file_and_removes_temporary(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "index.json"
            path.write_bytes(b"old")
            with mock.patch.object(
                os, "replace", side_effect=OSError("replace failed")
            ):
                with self.assertRaisesRegex(OSError, "replace failed"):
                    common.atomic_write(path, b"new")
            self.assertEqual(
                path.read_bytes(), b"old", "should preserve existing bytes"
            )
            self.assertEqual(
                list(Path(directory).iterdir()), [path],
                "should remove the unpublished temporary file",
            )

    def test_atomic_write_rejects_text_before_creating_a_file(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "index.json"
            with self.assertRaisesRegex(ValueError, "bytes"):
                common.atomic_write(path, "text")
            self.assertEqual(
                list(Path(directory).iterdir()), [],
                "should leave the directory empty for invalid input",
            )

    def test_atomic_write_sync_failure_cleans_up_before_replace(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "index.json"
            path.write_bytes(b"old")
            with mock.patch.object(os, "fsync", side_effect=OSError("sync failed")):
                with self.assertRaisesRegex(OSError, "sync failed"):
                    common.atomic_write(path, b"new")
            self.assertEqual(
                path.read_bytes(), b"old", "should preserve existing bytes"
            )
            self.assertEqual(
                list(Path(directory).iterdir()), [path],
                "should remove a temporary file after write or sync failure",
            )


if __name__ == "__main__":
    unittest.main()
