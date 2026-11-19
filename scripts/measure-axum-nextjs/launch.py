"""Run owned Next.js, TCP recorder, managed Axum and browser processes."""

import json
import os
import signal
import socket
import subprocess
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent
REPOSITORY = Path(os.environ["MEASURE_REPO"])
PORTS = {"origin": 18980, "relay": 18981, "axum": 18982}


def stop_on_signal(signum, _frame):
    """Unwind through owned-process cleanup on interruption."""
    raise SystemExit(128 + signum)


def main():
    signal.signal(signal.SIGTERM, stop_on_signal)
    signal.signal(signal.SIGINT, stop_on_signal)
    for port in PORTS.values():
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", port))
    environment = {**os.environ, "NEXT_TELEMETRY_DISABLED": "1"}
    processes = []

    def start(arguments, cwd, name, extra=None):
        with (ROOT / f"{name}.log").open("w") as log:
            process = subprocess.Popen(
                arguments, cwd=cwd, env={**environment, **(extra or {})},
                stdout=log, stderr=subprocess.STDOUT, start_new_session=True,
            )
        processes.append(process)
        for _attempt in range(200):
            if process.poll() is not None:
                raise RuntimeError(f"{name} exited; see its log")
            try:
                with socket.create_connection(("127.0.0.1", PORTS[name]), .1):
                    return
            except OSError:
                time.sleep(.1)
        raise RuntimeError(f"{name} readiness timeout")

    try:
        start(["node", "node_modules/next/dist/bin/next", "start", "-H", "127.0.0.1", "-p", "18980"], ROOT / "origin-app", "origin")
        start(["node", str(ROOT / "relay.mjs")], ROOT, "relay")
        envelope_path = Path(os.environ["MEASURE_ENVELOPE"])
        envelope = json.loads(envelope_path.read_text())["trusted_server_config"]
        start([os.environ["MEASURE_BIN"]], REPOSITORY, "axum", {
            "EDGEZERO__ADAPTER__HOST": "127.0.0.1",
            "EDGEZERO__ADAPTER__PORT": "18982",
            "EDGEZERO__LOGGING__LEVEL": "warn",
            "TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG": envelope,
            "TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_HANDLER_PASSWORD": "fixture-admin-password-32-bytes-ok",
            "TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_PUBLISHER_PROXY_SECRET": "fixture-proxy-secret-32-bytes-test",
            "TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_EC_PASSPHRASE": "fixture-ec-passphrase-32-bytes-test",
        })
        probe = subprocess.Popen(["node", str(ROOT / "exercise.mjs")], cwd=ROOT, env=environment, start_new_session=True)
        processes.append(probe)
        try:
            status = probe.wait(timeout=300)
        except subprocess.TimeoutExpired as error:
            raise RuntimeError("measurement exceeded its independent five-minute deadline") from error
        if status:
            raise RuntimeError("measurement assertions failed; see results.json")
    finally:
        for process in reversed(processes):
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                pass
            # A leader exiting does not prove that every browser child exited.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()


if __name__ == "__main__":
    main()
