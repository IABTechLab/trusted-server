#!/usr/bin/env python3
"""Native Chrome/Chromium trust proof. Requires a built ts, browser and certutil.

Uses disposable HOME/XDG/CA/NSS/profile directories, never the caller's trust store.
The mapped browser HTTPS request terminates at ts, which forwards to a local HTTP
fixture. Existing proxy E2E tests cover upstream TLS verification separately.
"""

import argparse
import http.server
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import threading
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ts", default="target/x86_64-unknown-linux-gnu/debug/ts")
    parser.add_argument("--browser", required=True, help="native Chrome/Chromium executable")
    parser.add_argument("--layout", choices=["xdg", "default", "legacy"], default="xdg")
    parser.add_argument("--logs", required=True, type=Path, help="evidence directory outside the repository")
    args = parser.parse_args()
    browser = shutil.which(args.browser)
    if not browser or not shutil.which("certutil"):
        parser.error("native browser and certutil must already be installed")
    ts = str(Path(args.ts).resolve())
    args.logs.mkdir(parents=True, exist_ok=True)
    version = subprocess.check_output([browser, "--version"], text=True).strip()

    class Origin(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"<h1>TS_ISOLATED_BROWSER_PROOF</h1>")

        def log_message(self, *_args):
            pass

    with tempfile.TemporaryDirectory(prefix="ts-browser-proof-") as temporary:
        home = Path(temporary)
        env = os.environ.copy()
        env.update(HOME=str(home), XDG_DATA_HOME=str(home / "data"),
                   XDG_CONFIG_HOME=str(home / "config"), XDG_CACHE_HOME=str(home / "cache"),
                   XDG_RUNTIME_DIR=str(home / "runtime"))
        (home / "runtime").mkdir(mode=0o700)
        env.pop("DBUS_SESSION_BUS_ADDRESS", None)
        if args.layout == "default":
            env.pop("XDG_DATA_HOME")
        if args.layout == "legacy":
            # Both directories exist; only the legacy destination should be trusted.
            for db in [home / ".pki/nssdb", home / "data/pki/nssdb"]:
                db.mkdir(parents=True)
                subprocess.run(["certutil", "-N", "--empty-password", "-d", "sql:" + str(db)], env=env, check=True)
        base = [ts, "dev", "proxy", "--ca-dir", str(home / "ca")]

        def ca(action):
            result = subprocess.run(base + ["ca", action], env=env, capture_output=True, text=True, check=True)
            (args.logs / (action + ".log")).write_text(result.stdout + result.stderr)

        origin = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Origin)
        threading.Thread(target=origin.serve_forever, daemon=True).start()
        # Let ts choose an unused listen port, then read its startup line.
        with (args.logs / "proxy.log").open("w+") as log:
            proxy = subprocess.Popen(base + ["--listen", "127.0.0.1:0", "--map",
                f"example.com=origin.example.com:{origin.server_port}", "--resolve",
                "origin.example.com:127.0.0.1", "--upstream-plaintext"], env=env, stdout=log, stderr=log)
            try:
                address = None
                for _ in range(200):
                    log.seek(0)
                    for line in log.read().splitlines():
                        if "ts dev proxy listening on " in line:
                            address = line.split("ts dev proxy listening on ", 1)[1].strip()
                    if address:
                        break
                    if proxy.poll() is not None:
                        raise RuntimeError("proxy exited; inspect proxy.log")
                    time.sleep(0.05)
                if not address:
                    raise RuntimeError("proxy did not start")

                def navigate(label, url, trusted):
                    profile = home / ("profile-" + label)
                    command = [browser, "--headless", "--no-first-run", "--no-default-browser-check",
                        "--user-data-dir=" + str(profile), "--proxy-server=https=" + address,
                        "--host-resolver-rules=MAP example.com 127.0.0.1", "--dump-dom", url]
                    result = subprocess.run(command, env=env, capture_output=True, text=True, timeout=45, check=True)
                    (args.logs / (label + ".html")).write_text(result.stdout)
                    (args.logs / (label + ".stderr")).write_text(result.stderr)
                    loaded = "TS_ISOLATED_BROWSER_PROOF" in result.stdout
                    assert loaded == trusted, f"{label}: expected loaded={trusted}, got {loaded}"
                    if not trusted:
                        assert "ERR_CERT_AUTHORITY_INVALID" in result.stdout, f"{label}: not a certificate trust failure"
                    print(f"{label}: loaded={loaded}", flush=True)

                ca("path")
                navigate("absent", "https://example.com", False)
                ca("install")
                (args.logs / "managed-nss-trust.json").write_bytes((home / "ca/managed-nss-trust.json").read_bytes())
                navigate("trusted", "https://example.com", True)
                ca("uninstall")
                navigate("revoked", "https://example.com", False)
                proxy.send_signal(signal.SIGINT)
                proxy.wait(timeout=10)
                # The proxy is stopped. HTTP for the same mapped host must still load.
                navigate("http-direct-proxy-stopped", f"http://example.com:{origin.server_port}", True)
                print(f"PASS: {version}; layout={args.layout}; all trust/profile paths under {home}")
            finally:
                if proxy.poll() is None:
                    proxy.terminate()
                    proxy.wait(timeout=10)
                origin.shutdown()


if __name__ == "__main__":
    main()
