"""Verify that socket evidence rejects truncated or incorrect request bodies."""

import hashlib
import unittest

from analyze import parse_stream, validate_history


class RequestEvidenceTests(unittest.TestCase):
    def test_two_requests_on_same_stream_keep_exact_bodies(self):
        wire = (
            b"POST / HTTP/1.1\r\nNext-Action: test-action-id\r\nContent-Length: 2\r\n\r\n[]"
            b"GET /api/hello HTTP/1.1\r\n\r\n"
        )
        rows = parse_stream(7, wire)
        self.assertEqual([row["connection"] for row in rows], [7, 7])
        self.assertEqual(rows[0]["sha256"], hashlib.sha256(b"[]").hexdigest())
        self.assertEqual(rows[1]["bytes"], 0)

    def test_truncated_content_length_is_rejected(self):
        with self.assertRaises(ValueError):
            parse_stream(1, b"POST / HTTP/1.1\r\nContent-Length: 2\r\n\r\n[")

    def test_truncated_chunk_is_rejected(self):
        with self.assertRaises(ValueError):
            parse_stream(1, b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n[")

    def test_chunked_input_preserves_body_hash(self):
        rows = parse_stream(
            1,
            b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n1\r\n[\r\n1\r\n]\r\n0\r\n\r\n",
        )
        self.assertEqual(rows[0]["bytes"], 2)
        self.assertEqual(rows[0]["sha256"], hashlib.sha256(b"[]").hexdigest())

    def test_wrong_historical_body_is_rejected(self):
        rows = parse_stream(
            1,
            b"POST / HTTP/1.1\r\nNext-Action: test-action-id\r\nContent-Length: 2\r\n\r\n{}",
        )
        with self.assertRaises(ValueError):
            validate_history(rows, expected_count=1)

    def test_separate_connection_for_every_post_does_not_prove_reuse(self):
        rows = []
        for connection in range(2):
            rows.extend(parse_stream(
                connection,
                b"POST / HTTP/1.1\r\nNext-Action: test-action-id\r\nContent-Length: 2\r\n\r\n[]",
            ))
        with self.assertRaises(ValueError):
            validate_history(rows, expected_count=2)


if __name__ == "__main__":
    unittest.main()
