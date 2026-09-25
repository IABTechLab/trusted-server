#!/usr/bin/env python3
"""Render Compose and exercise smoke command wiring without starting containers."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


if not __debug__:
    sys.exit("Run this check without python -O or PYTHONOPTIMIZE; assertions are required.")

EXAMPLE = Path(__file__).resolve().parent.parent
DOCKER = shutil.which("docker")
assert DOCKER, "docker with the Compose plugin must be on PATH"
ENV = {key: os.environ[key] for key in ("PATH", "HOME") if key in os.environ}


def render_production():
    result = subprocess.run(
        [DOCKER, "compose", "-f", str(EXAMPLE / "runtime/compose.yaml"),
         "config", "--format", "json", "--no-env-resolution"],
        env=ENV, check=True, capture_output=True, text=True,
    )
    service = json.loads(result.stdout)["services"]["pbs"]
    port = service["ports"][0]
    assert port["host_ip"] == "0.0.0.0", port
    assert port["published"] == "8000" and port["target"] == 8000, port
    assert service["env_file"][0]["path"] == "/run/pbs/secrets/examplebidder.env"


def check_smoke():
    with tempfile.TemporaryDirectory(prefix="pbs-smoke-wiring-") as scratch:
        directory = Path(scratch)
        docker = directory / "docker"
        docker.write_text('''#!/usr/bin/env python3
import json, os, pathlib, subprocess, sys
args = sys.argv[1:]
assert args[0] == "compose", args
operation = args[7]
assert operation in ("up", "logs", "down"), args
with open(os.environ["CALLS"], "a") as log:
    log.write(operation + "\\n")
if operation == "up":
    result = subprocess.run(
        [os.environ["REAL_DOCKER"], *args[:7], "config", "--format", "json"],
        capture_output=True, text=True,
    )
    if result.returncode:
        sys.stderr.write(result.stderr)
        sys.exit(result.returncode)
    pathlib.Path(os.environ["RENDERED"]).write_text(result.stdout)
''')
        curl = directory / "curl"
        curl.write_text('''#!/usr/bin/env python3
import sys
assert sys.argv[-1] == "http://127.0.0.1:18081/status", sys.argv
print("ok")
''')
        docker.chmod(0o755)
        curl.chmod(0o755)
        env = {
            **ENV,
            "PATH": f"{directory}:{ENV['PATH']}",
            "REAL_DOCKER": DOCKER,
            "CALLS": str(directory / "calls"),
            "RENDERED": str(directory / "rendered.json"),
            "PBS_SMOKE_PORT": "18081",
            "PBS_HOST_PORT": "19000",
            "PBS_BIND_ADDRESS": "0.0.0.0",
            "PBS_CONFIG_FILE": "/nonexistent/inherited-pbs.yaml",
            "PBS_SECRET_ENV_FILE": "/nonexistent/inherited-secrets.env",
        }
        subprocess.run(
            [str(EXAMPLE / "scripts/smoke-runtime.sh")],
            env=env, check=True, capture_output=True, text=True,
        )
        assert (directory / "calls").read_text().splitlines() == ["up", "logs", "down"]
        service = json.loads((directory / "rendered.json").read_text())["services"]["pbs"]
        port = service["ports"][0]
        assert port.get("host_ip") == "127.0.0.1", port
        assert port["published"] == "18081" and port["target"] == 8000, port
        assert service["volumes"][0]["source"] == str(EXAMPLE / "runtime/pbs.yaml")
        assert service["environment"]["PBS_ADAPTERS_EXAMPLEBIDDER_API_KEY"] == "example-only-api-key"
        assert service["environment"]["PBS_ADAPTERS_EXAMPLEBIDDER_OPTIONAL_TOKEN"] == "example-only-optional-token"


if __name__ == "__main__":
    render_production()
    check_smoke()
    print("Compose production/smoke bindings and dummy selectors passed; no containers started.")
