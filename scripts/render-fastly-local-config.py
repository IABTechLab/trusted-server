#!/usr/bin/env python3
import argparse
import json
import pathlib
import subprocess
import sys

CONFIG_STORE_NAME = "ts_config_store"
CONFIG_KEY = "ts-config"


def host_target() -> str:
    result = subprocess.run(
        ["rustc", "-vV"],
        check=True,
        capture_output=True,
        text=True,
    )
    for line in result.stdout.splitlines():
        if line.startswith("host: "):
            return line.removeprefix("host: ").strip()
    raise RuntimeError("failed to determine rust host target")


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Render a Fastly/Viceroy local config with runtime app config projected into a config store."
    )
    parser.add_argument("--app-config", required=True, help="Path to trusted-server TOML")
    parser.add_argument("--template", required=True, help="Path to fastly/viceroy template TOML")
    parser.add_argument("--output", required=True, help="Path to write rendered TOML")
    args = parser.parse_args()

    repo_root = pathlib.Path(__file__).resolve().parent.parent
    app_config = pathlib.Path(args.app_config).resolve(strict=False)
    template = pathlib.Path(args.template).resolve(strict=False)
    output = pathlib.Path(args.output).resolve(strict=False)

    try:
        result = subprocess.run(
            [
                "cargo",
                "run",
                "--quiet",
                "--target",
                host_target(),
                "--package",
                "trusted-server-core",
                "--bin",
                "ts-config-canonicalize",
                "--",
                str(app_config),
            ],
            cwd=repo_root,
            check=True,
            capture_output=True,
            text=True,
        )
    except subprocess.CalledProcessError as error:
        if error.stderr:
            print(error.stderr.strip(), file=sys.stderr)
        if error.stdout:
            print(error.stdout.strip(), file=sys.stderr)
        return error.returncode

    canonical_toml = result.stdout
    if result.stderr:
        print(result.stderr.strip(), file=sys.stderr)

    rendered = template.read_text(encoding="utf-8")
    rendered += "\n"
    rendered += f"[local_server.config_stores.{CONFIG_STORE_NAME}]\n"
    rendered += '    format = "inline-toml"\n'
    rendered += f"[local_server.config_stores.{CONFIG_STORE_NAME}.contents]\n"
    rendered += f"    {CONFIG_KEY} = {json.dumps(canonical_toml)}\n"

    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(rendered, encoding="utf-8")
    print(output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
