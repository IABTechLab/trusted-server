#!/usr/bin/env python3
"""Run production HTTP clients against delayed local origins in worker runtimes."""

import argparse
import http.server
import pathlib
import socket
import subprocess
import tempfile
import threading
import time
import urllib.request


class Origin(http.server.BaseHTTPRequestHandler):
    events = {}

    def do_GET(self):
        self.events.setdefault(self.path, threading.Event()).set()
        if self.path == "/headers":
            time.sleep(0.7)
        self.send_response(200)
        self.send_header("Content-Length", "4")
        self.end_headers()
        try:
            if self.path == "/body":
                self.wfile.write(b"a")
                self.wfile.flush()
                time.sleep(0.7)
                self.wfile.write(b"bcd")
            else:
                self.wfile.write(b"okay")
        except (BrokenPipeError, ConnectionResetError):
            # Deadline cancellation deliberately disconnects this slow origin.
            pass

    def log_message(self, *_):
        pass


def free_port():
    with socket.socket() as connection:
        connection.bind(("127.0.0.1", 0))
        return connection.getsockname()[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("adapter", choices=["cloudflare", "spin"])
    parser.add_argument("--runtime", required=True)
    parser.add_argument("--wasm-bindgen", default="wasm-bindgen")
    parser.add_argument("--target-dir", type=pathlib.Path)
    args = parser.parse_args()
    root = pathlib.Path(__file__).resolve().parent.parent
    artifact = (args.target_dir or root / "target") / ("wasm32-unknown-unknown" if args.adapter == "cloudflare" else "wasm32-wasip1") / "debug/examples/deadline_runtime.wasm"
    origin = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Origin)
    thread = threading.Thread(target=origin.serve_forever, daemon=True)
    thread.start()
    port = free_port()
    with tempfile.TemporaryDirectory(prefix="ts-worker-deadline-") as temporary:
        workspace = pathlib.Path(temporary)
        if args.adapter == "cloudflare":
            subprocess.run([args.wasm_bindgen, str(artifact), "--target", "web", "--out-dir", str(workspace), "--out-name", "fixture"], check=True)
            (workspace / "index.js").write_text('import module from "./fixture_bg.wasm";\nimport {initSync, fetch} from "./fixture.js";\ninitSync({module});\nexport default {fetch};\n')
            (workspace / "wrangler.toml").write_text('name = "deadline-fixture"\nmain = "index.js"\ncompatibility_date = "2026-04-01"\n')
            command = [args.runtime, "dev", "--local", "--port", str(port), "--ip", "127.0.0.1", "--config", str(workspace / "wrangler.toml")]
        else:
            (workspace / "spin.toml").write_text(f'''spin_manifest_version = 2
[application]
name = "deadline-fixture"
version = "0.1.0"
[[trigger.http]]
route = "/..."
component = "fixture"
[component.fixture]
source = "{artifact}"
allowed_outbound_hosts = ["http://127.0.0.1:*"]
''')
            command = [args.runtime, "up", "--from", str(workspace / "spin.toml"), "--listen", f"127.0.0.1:{port}"]
        with (workspace / "runtime.log").open("w+") as log:
            process = subprocess.Popen(command, cwd=workspace, stdout=log, stderr=subprocess.STDOUT)
            try:
                for _ in range(300):
                    if process.poll() is not None:
                        raise RuntimeError("worker exited during startup")
                    try:
                        with socket.create_connection(("127.0.0.1", port), timeout=0.1):
                            break
                    except OSError:
                        time.sleep(0.1)
                else:
                    raise RuntimeError("worker did not listen")
                cases = [
                    ("/fast", 80, 200, "complete", 500),
                    ("/headers", 80, 0, "timeout", 350),
                    ("/body", 80, 0, "timeout", 350),
                    ("/headers", 80, 200, "timeout", 500),
                ]
                for path, budget, origin_delay, expected, maximum in cases:
                    Origin.events[path] = threading.Event()
                    upstream = f"http://127.0.0.1:{origin.server_port}{path}"
                    uri = f"http://127.0.0.1:{port}/?upstream={upstream}&budget_ms={budget}&origin_ms={origin_delay}"
                    with urllib.request.urlopen(uri, timeout=5) as response:
                        result = response.read().decode()
                    status, elapsed = result.split(":")
                    assert status == expected, (path, expected, result)
                    assert int(elapsed) < maximum, (path, maximum, result)
                    assert Origin.events[path].is_set(), (path, "request should start at dispatch")
                    print(f"PASS {args.adapter} {path} budget={budget} origin={origin_delay}: {result}", flush=True)
            except BaseException:
                log.flush()
                log.seek(0)
                print(log.read())
                raise
            finally:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
    origin.shutdown()


if __name__ == "__main__":
    main()
