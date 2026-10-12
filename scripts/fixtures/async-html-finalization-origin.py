#!/usr/bin/env python3
"""Loopback origin and delayed lookup for the fictional terminal consumer."""

import argparse
import json
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

DOCUMENT = (
    b"<!doctype html><html><head></head><body><p>EXAMPLE_UNSIGNED</p>"
    b"</body></html><!--example-document-eof-->"
)


def serve(port, event_path):
    lock = threading.Lock()

    def event(kind, **fields):
        with lock:
            with event_path.open("a") as output:
                output.write(json.dumps({"event": kind, "time": time.monotonic(), **fields}) + "\n")

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_HEAD(self):
            self.send_response(200)
            self.send_header("Content-Type", "text/html")
            self.send_header("Content-Length", str(len(DOCUMENT)))
            self.end_headers()

        def do_GET(self):
            self.send_response(200)
            self.send_header("Content-Type", "text/html")
            self.send_header("Cache-Control", "public, max-age=60")
            self.send_header("Content-Length", str(len(DOCUMENT)))
            self.end_headers()
            self.wfile.write(DOCUMENT[:-len(b"<!--example-document-eof-->")])
            self.wfile.flush()
            time.sleep(0.05)
            event("origin_eof")
            self.wfile.write(b"<!--example-document-eof-->")
            self.wfile.flush()

        def do_POST(self):
            if self.headers.get("Transfer-Encoding", "").lower() == "chunked":
                body = bytearray()
                while True:
                    size = int(self.rfile.readline().split(b";", 1)[0], 16)
                    if size == 0:
                        while self.rfile.readline() != b"\r\n":
                            pass
                        break
                    body.extend(self.rfile.read(size))
                    assert len(body) <= 2048, "fixture request must remain bounded"
                    assert self.rfile.read(2) == b"\r\n"
            else:
                size = int(self.headers.get("Content-Length", "0"))
                assert size <= 2048, "fixture request must remain bounded"
                body = self.rfile.read(size)
            payload = json.loads(body)
            event("lookup_start", path=self.path, **payload)
            time.sleep(30 if self.path == "/hung" else 0.15)
            self.send_response(200)
            self.send_header("Content-Length", str(len(b"EXAMPLE_APPROVED")))
            self.end_headers()
            try:
                self.wfile.write(b"EXAMPLE_APPROVED")
                event("lookup_complete", path=self.path)
            except (BrokenPipeError, ConnectionResetError):
                event("lookup_cancelled", path=self.path)

    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--events", type=Path, required=True)
    args = parser.parse_args()
    serve(args.port, args.events)
