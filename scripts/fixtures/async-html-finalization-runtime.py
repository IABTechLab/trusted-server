#!/usr/bin/env python3
"""Fresh-build local runtime matrix with isolated config and process cleanup."""

import argparse
import base64
import re
import json
import os
import shutil
import tomllib
import signal
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SECRETS = {
    "handler_password": "example-handler-password-32-bytes-ok",
    "publisher_proxy_secret": "example-publisher-secret-32-bytes-ok",
    "ec_passphrase": "example-cookie-passphrase-32-bytes-ok",
}


def free_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def stop(process):
    if process is None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
        process.wait(timeout=5)
    except (ProcessLookupError, subprocess.TimeoutExpired):
        # An exited group needs no TERM; a graceful-wait timeout proceeds to KILL.
        pass
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        # The process group may already have exited after TERM.
        pass
    process.wait(timeout=5)


def run(adapter, auction=False, release=False):
    workspace = Path(tempfile.mkdtemp(prefix=f"trusted-server-finalize-{adapter}-"))
    print(f"Artifacts: {workspace}", flush=True)
    allowed = {"PATH", "RUSTUP_HOME", "CARGO_HOME", "CARGO_TARGET_DIR", "TMPDIR", "WASM_BINDGEN_BIN", "WASM_OPT_BIN", "ESBUILD_BIN", "CARGO_NET_OFFLINE"}
    env = {key: value for key, value in os.environ.items() if key in allowed}
    target = Path(env.get("CARGO_TARGET_DIR", workspace / "target")).resolve()
    env["CARGO_TARGET_DIR"] = str(target)
    env["XDG_CONFIG_HOME"] = str(workspace / "cli-config")
    env["WRANGLER_SEND_METRICS"] = "false"
    env["WRANGLER_LOG_PATH"] = str(workspace / "wrangler.log")
    origin_port, app_port = free_port(), free_port()
    env.update({
        "TRUSTED_SERVER__PUBLISHER__DOMAIN": "127.0.0.1",
        "TRUSTED_SERVER__PUBLISHER__COOKIE_DOMAIN": "127.0.0.1",
        "TRUSTED_SERVER__PUBLISHER__ORIGIN_URL": f"http://127.0.0.1:{origin_port}",
    })
    required = ["cargo", "python3"] + ({"fastly": ["viceroy"], "cloudflare": ["worker-build", "wrangler"], "spin": ["spin"]}.get(adapter, []))
    for executable in required:
        if shutil.which(executable, path=env["PATH"]) is None:
            raise RuntimeError(f"required executable missing: {executable}")
    if adapter == "cloudflare":
        lock = tomllib.loads((ROOT / "Cargo.lock").read_text())
        expected = next(package["version"] for package in lock["package"] if package["name"] == "worker")
        actual = subprocess.check_output(["worker-build", "--version"], env=env, text=True).strip()
        if actual != expected:
            raise RuntimeError(f"worker-build {expected} required for the locked worker SDK; found {actual}")
    processes = []

    def command(args, cwd=ROOT, overrides=None):
        subprocess.run(args, cwd=cwd, env=env | (overrides or {}), check=True)

    def launch(args, log, cwd=workspace, runtime_env=None):
        output = log.open("w")
        process = subprocess.Popen(args, cwd=cwd, env=runtime_env or env,
                                   stdout=output, stderr=subprocess.STDOUT,
                                   start_new_session=True)
        output.close()
        processes.append(process)
        return process

    def get(path, auction_case=False):
        headers = {"Accept": "text/html", "User-Agent": "Mozilla/5.0 (Example browser)"}
        if auction_case:
            # A fictional core TCF v2 segment grants Purpose 1. No identifiers
            # or vendor-specific data are included in this local control.
            bits = bytearray(29)
            def field(offset, width, value):
                for index in range(width):
                    if value & (1 << (width - index - 1)):
                        position = offset + index
                        bits[position // 8] |= 1 << (7 - position % 8)
            field(0, 6, 2)
            field(6, 36, int(time.time() * 10))
            field(42, 36, int(time.time() * 10))
            field(108, 6, 4)
            field(114, 6, 13)
            field(152, 1, 1)
            headers["Cookie"] = "euconsent-v2=" + base64.urlsafe_b64encode(bits).decode().rstrip("=")
        request = urllib.request.Request(f"http://127.0.0.1:{app_port}{path}", headers=headers)
        with urllib.request.urlopen(request, timeout=10) as response:
            return response.read(), response.headers

    try:
        command(["cargo", "build", "-p", "trusted-server-cli", "--bin", "ts", "--locked"])
        cli = str(target / "debug/ts")
        crate = ROOT / f"crates/trusted-server-adapter-{adapter}"
        if adapter == "cloudflare":
            worker_args = ["worker-build", "--release" if release else "--dev", "--out-dir", str(workspace / "build"), "--features", "cloudflare,test-utils", "--locked"]
            if not release:
                worker_args += ["--no-opt"]
            command(worker_args, cwd=crate)
            artifact = workspace / "build/index.js"
        else:
            args = ["cargo", "build", "-p", f"trusted-server-adapter-{adapter}", "--locked",
                    "--features", "trusted-server-core/test-utils" + (",spin,test-utils" if adapter == "spin" else "")]
            if adapter != "axum":
                args += ["--target", "wasm32-wasip1"]
            if release:
                args += ["--release"]
            command(args)
            profile = "release" if release else "debug"
            artifact = target / (f"{profile}/trusted-server-axum" if adapter == "axum" else
                                 f"wasm32-wasip1/{profile}/{'trusted_server_adapter_spin' if adapter == 'spin' else 'trusted-server-adapter-fastly'}.wasm")
        assert artifact.exists(), f"fresh artifact missing: {artifact}"
        launch(["python3", str(ROOT / "scripts/fixtures/async-html-finalization-origin.py"),
                "--port", str(origin_port), "--events", str(workspace / "events.jsonl")], workspace / "origin.log")
        config = workspace / "trusted-server.toml"
        command([cli, "config", "init", "--app-config", str(config)])
        base_config = config.read_text().replace("[publisher]\n", "[publisher]\nhtml_finalization_timeout_ms = 750\n", 1)
        manifest = (ROOT / "edgezero.toml").read_text().replace('= "crates/', f'= "{ROOT}/crates/')
        manifest = manifest.replace('manifest = "fastly.toml"', f'manifest = "{workspace}/fastly.toml"')
        manifest = manifest.replace(str(crate / "spin.toml"), str(workspace / "spin.toml"))
        (workspace / "edgezero.toml").write_text(manifest)
        fastly_base = 'manifest_version = 3\nname = "example-finalization"\nlanguage = "rust"\n[local_server]\n'
        (workspace / "fastly.toml").write_text(fastly_base)
        if adapter == "spin":
            (workspace / "spin.toml").write_text((crate / "spin.toml").read_text().replace(
                '../../target/wasm32-wasip1/release/trusted_server_adapter_spin.wasm', str(artifact)))

        baseline = None
        cases = ["disabled", "success", "hung"] + (["auction"] if auction else [])
        for case in cases:
            case_config = base_config
            if case == "auction":
                # A closed loopback bid endpoint exercises transport failure and
                # empty-bid assembly without sending traffic outside this host.
                bid_port = free_port()
                table = case_config.index("[auction]\n")
                enabled = case_config.index("enabled = false", table)
                case_config = case_config[:enabled] + case_config[enabled:].replace("enabled = false", "enabled = true", 1)
                case_config = case_config.replace('endpoint = "https://prebid.example.com/openrtb2/auction"', f'endpoint = "https://127.0.0.1:{bid_port}/bid"', 1)
                case_config += '\n[[creative_opportunities.slot]]\nid = "example-slot"\npage_patterns = ["/article"]\nformats = [{ width = 300, height = 250 }]\n'
            lookup_case = "success" if case == "auction" else case
            config.write_text(case_config + f'\n[integrations.html_finalization_fixture]\nenabled = {str(case != "disabled").lower()}\nlookup_url = "http://127.0.0.1:{origin_port}/{lookup_case}"\ntransport_timeout_ms = 30000\n')
            # Axum's local JSON provides the same neutral app-config envelope
            # used by the other runtime bindings; no operator config is read.
            command([cli, "config", "push", "--adapter", "axum", "--local", "--manifest", str(workspace / "edgezero.toml"),
                     "--app-config", str(config), "--yes", "--no-diff"], cwd=workspace)
            envelope = json.loads((workspace / ".edgezero/local-config-trusted_server_config.json").read_text())["trusted_server_config"]
            runtime_env = env.copy()
            if adapter == "axum":
                runtime_env["TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG"] = envelope
                for key, value in SECRETS.items():
                    runtime_env[f"TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_{key.upper()}"] = value
                runtime_env["PORT"] = str(app_port)
                args = [str(artifact)]
            elif adapter == "cloudflare":
                text = (crate / "wrangler.ci.toml").read_text().replace('main = "build/index.js"', f'main = "{artifact}"')
                text = text.replace('TRUSTED_SERVER_CONFIG = "{}"', "TRUSTED_SERVER_CONFIG = '''" + json.dumps({"app_config": envelope}) + "'''")
                text += "\n" + "\n".join(f'{key} = "{value}"' for key, value in SECRETS.items()) + "\n"
                (workspace / "wrangler.toml").write_text(text)
                args = ["wrangler", "dev", "--local", "--config", str(workspace / "wrangler.toml"),
                        "--ip", "127.0.0.1", "--port", str(app_port), "--persist-to", str(workspace / "worker-state")]
            else:
                if adapter == "fastly":
                    (workspace / "fastly.toml").write_text(fastly_base)
                command([cli, "config", "push", "--adapter", adapter, "--local", "--manifest", str(workspace / "edgezero.toml"),
                         "--app-config", str(config), "--yes", "--no-diff"], cwd=workspace, overrides={"EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME": "default"} if adapter == "spin" else None)
                if adapter == "fastly":
                    with (workspace / "fastly.toml").open("a") as output:
                        for key, value in SECRETS.items():
                            output.write(f'\n[[local_server.secret_stores.trusted_server_secrets]]\nkey = "{key}"\ndata = "{value}"\n')
                    args = ["viceroy", "--config", str(workspace / "fastly.toml"), "--addr", f"127.0.0.1:{app_port}", str(artifact)]
                else:
                    args = ["spin", "up", "--from", str(workspace / "spin.toml"), "--listen", f"127.0.0.1:{app_port}",
                            "--state-dir", str(workspace / ".spin"), "--cache-dir", str(workspace / "spin-cache"), "--log-dir", str(workspace / "spin-logs")]
                    for key, value in SECRETS.items():
                        encoded = "v_trusted_x5fserver_x5fsecrets_v_" + key.replace("_", "_x5f")
                        args += ["--variable", f"{encoded}={value}"]
            log = workspace / f"{case}.log"
            process = launch(args, log, runtime_env=runtime_env)
            for _ in range(80):
                if process.poll() is not None:
                    raise RuntimeError(f"{adapter} exited: {log.read_text()}")
                try:
                    with urllib.request.urlopen(urllib.request.Request(f"http://127.0.0.1:{app_port}/health", method="HEAD"), timeout=1) as ready:
                        if ready.status == 200:
                            break
                except (OSError, urllib.error.URLError):
                    time.sleep(0.1)
            else:
                raise RuntimeError(f"runtime did not become ready: {log.read_text()}")
            started = time.monotonic()
            try:
                body, headers = get(f"/article?case={case}", case == "auction")
            except urllib.error.HTTPError as error:
                (workspace / f"{case}.error.body").write_bytes(error.read())
                (workspace / f"{case}.headers").write_text(str(error.headers))
                time.sleep(0.3)
                raise RuntimeError(f"{adapter} {case} returned HTTP {error.code}; see {log}") from error
            completed = time.monotonic()
            elapsed = time.monotonic() - started
            (workspace / f"{case}.body").write_bytes(body)
            (workspace / f"{case}.headers").write_text(str(headers))
            if case == "disabled":
                assert b"EXAMPLE_UNSIGNED" in body and b"EXAMPLE_FINALIZED" not in body
                baseline = body
            elif case == "success":
                assert body == baseline.replace(b"EXAMPLE_UNSIGNED", b"EXAMPLE_FINALIZED"), "successful terminal output must match the exact expected rewrite"
                assert elapsed >= 0.15, "success must actually await delayed lookup"
            elif case == "auction":
                assert b"EXAMPLE_FINALIZED" in body, "auction-enabled output must await finalization"
                assert b"adSlots" in body and b"bids" in body, "auction control must reach slot and bid assembly"
                assert b"ts-inline-body-close-" not in body, "auction assembly must consume the opaque parser marker"
            else:
                assert body == baseline, "timeout must emit byte-identical processed input"
                assert 0.6 <= elapsed < 3, f"timeout escaped the 750ms budget: {elapsed}"
                diagnostic = log.read_text()
                if adapter == "spin":
                    diagnostic += "\n".join(path.read_text() for path in (workspace / "spin-logs").glob("*stderr*"))
                assert "HTML finalizer html_finalization_fixture times out" in diagnostic or (adapter == "fastly" and "retains input: unavailable" in diagnostic), "missing controlled fallback diagnostic"
            if case != "disabled":
                assert headers["Cache-Control"] == "no-store, private"
            current_events = [json.loads(line) for line in (workspace / "events.jsonl").read_text().splitlines()]
            lookups = [event for event in current_events if event["event"] == "lookup_start"]
            lookup_elapsed = completed - lookups[-1]["time"] if case != "disabled" else None
            terminal_ms = None
            if case == "hung":
                measured = re.search(r"times out after (\d+) ms", diagnostic)
                if measured:
                    terminal_ms = int(measured.group(1))
                    assert 700 <= terminal_ms < 2500, "terminal budget must exclude origin transfer"
            (workspace / f"{case}.timing.json").write_text(json.dumps({"request_seconds": elapsed, "terminal_ms": terminal_ms, "lookup_to_response_seconds": lookup_elapsed, "configured_budget_ms": 750, "cooperative_timer": adapter != "fastly"}))
            print(f"PASS {adapter} {case}: request={elapsed:.3f}s, {len(body)} bytes, chain_budget=750ms, terminal_ms={terminal_ms}, lookup_to_response={lookup_elapsed}, cooperative_timer={adapter != 'fastly'}", flush=True)
            stop(process)
        events = [json.loads(line) for line in (workspace / "events.jsonl").read_text().splitlines()]
        starts = [event for event in events if event["event"] == "lookup_start"]
        assert len(starts) == 2 + int(auction) and all(event["complete"] and len(event["sha256"]) == 64 for event in starts)
        for event in starts:
            assert any(previous["event"] == "origin_eof" and previous["time"] <= event["time"] for previous in events)
            if adapter == "fastly":
                assert 0 < event["transport_timeout_ms"] <= 750, "blocking host calls must use the remaining budget"
            else:
                assert event["transport_timeout_ms"] == 30000, "SDK timer must release before the transport deadline"
        assert any(event["event"] == "lookup_complete" and event["path"] == "/success" for event in events)
    finally:
        for process in reversed(processes):
            stop(process)



if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("adapter", choices=["fastly", "cloudflare", "spin", "axum"])
    parser.add_argument("--auction", action="store_true", help="also exercise enabled auction transport-failure assembly")
    parser.add_argument("--release", action="store_true", help="build production-profile fixture artifacts")
    args = parser.parse_args()
    run(args.adapter, args.auction, args.release)
