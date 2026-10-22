"""Run ignored Fastly deadline tests against a local, deliberately slow origin.

Usage: python3 tests/deadline_origin.py /path/to/viceroy /path/to/test-binary.wasm
"""

import http.server
import os
import pathlib
import subprocess
import sys
import tempfile
import threading
import time


class Origin(http.server.BaseHTTPRequestHandler):
    def do_HEAD(self):
        self.send_response(200)
        self.send_header("Content-Length", str(20 * 1024 * 1024))
        self.end_headers()

    def do_GET(self):
        try:
            if self.path == "/stalled":
                time.sleep(2)
            if self.path == "/trickle":
                self.send_response(200)
                self.send_header("Content-Length", "100")
                self.end_headers()
                for _ in range(100):
                    self.wfile.write(b"x")
                    self.wfile.flush()
                    time.sleep(0.01)
                return
            time.sleep(0.03)
            self.send_response(200)
            self.send_header("Content-Length", "4")
            self.end_headers()
            self.wfile.write(b"fast")
        except (BrokenPipeError, ConnectionResetError):
            # Deadline expiry deliberately closes the response before this slow
            # origin finishes writing, so a disconnected client is expected.
            pass

    def log_message(self, *_args):
        pass


def main():
    with http.server.ThreadingHTTPServer(("127.0.0.1", 0), Origin) as server:
        server.daemon_threads = True
        threading.Thread(target=server.serve_forever, daemon=True).start()
        with tempfile.TemporaryDirectory(prefix="fastly-deadline-") as directory:
            config = pathlib.Path(directory) / "fastly.toml"
            config.write_text(
                '[local_server.backends.deadline_origin]\n'
                f'url = "http://127.0.0.1:{server.server_port}"\n'
            )
            environment = os.environ.copy()
            if pathlib.Path("/etc/ssl/cert.pem").exists():
                environment.setdefault("SSL_CERT_FILE", "/etc/ssl/cert.pem")
            result = subprocess.run(
                [sys.argv[1], "run", "-C", str(config), sys.argv[2],
                 "platform::deadline::tests::network_", "--ignored", "--nocapture"],
                env=environment,
                check=False,
            )
        server.shutdown()
    return result.returncode


if __name__ == "__main__":
    sys.exit(main())
