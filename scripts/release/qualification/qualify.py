"""Validate closed disposable hosted observations without conferring trust.

Production qualification remains pending authenticated hosted resources and the
shared complete production recipes. JSON observations never select commands.
"""

import argparse
import errno
import logging
import re
import stat
import sys
from pathlib import Path

RELEASE = Path(__file__).resolve().parents[1]
sys.dont_write_bytecode = True
sys.path.insert(0, str(RELEASE))

from common import deterministic_json_bytes, read_json_file, require_nonnegative_integer, sha256_bytes
import preflight

BASE_TOOLS = {"python", "git", "shell", "rustup", "rustc", "cargo", "compiler", "linker",
              "archiver", "node", "npm"}
RECIPE_ENVIRONMENTS = {
    "ts-aarch64-apple-darwin": "macos15-aarch64",
    "ts-x86_64-apple-darwin": "macos15-x86_64",
    "ts-x86_64-unknown-linux-gnu": "gnu-builder-x86_64",
    "trusted-server-fastly.wasm": "gnu-builder-x86_64", "tsjs": "gnu-builder-x86_64",
}
SHARED_CLOSURE = (
    ".cargo/config.toml", ".tool-versions", "Cargo.toml", "Cargo.lock", "rust-toolchain.toml",
    "scripts/release/common.py", "scripts/release/preflight.py", "scripts/release/prepare.py",
    "scripts/release/build-js.sh", "scripts/release/collect-js.mjs", "scripts/release/requirements.txt",
    "scripts/release/qualification/qualify.py", "scripts/release/qualification/boundaries.py",
    "scripts/release/qualification/socket_probe.py", "scripts/release/qualification/results.schema.json",
    "scripts/release/qualification/seccomp-default.json",
    "crates/trusted-server-core/build.rs", "crates/trusted-server-core/build_support/git_version.rs",
    "crates/trusted-server-cli/build.rs", "crates/trusted-server-adapter-fastly/build.rs",
    "crates/trusted-server-js/build.rs", "crates/trusted-server-js/build/bundle_set.rs",
    "crates/trusted-server-js/build/inventory.rs", "crates/trusted-server-js/build/prebuilt.rs",
    "crates/trusted-server-js/lib/build-all.mjs", "crates/trusted-server-js/lib/build-module-options.mjs",
    "crates/trusted-server-js/lib/release-composition.mjs", "crates/trusted-server-js/lib/vitest.config.ts",
    "crates/trusted-server-js/lib/vite.config.ts", "crates/trusted-server-js/lib/tsconfig.json",
    "crates/trusted-server-js/lib/package.json", "crates/trusted-server-js/lib/package-lock.json",
    "crates/trusted-server-cli/Cargo.toml", "crates/trusted-server-core/Cargo.toml",
    "crates/trusted-server-js/Cargo.toml", "crates/trusted-server-adapter-fastly/Cargo.toml",
)
RUST_CLOSURE = ("scripts/release/build.sh", "scripts/release/inspect-native.py",
                "scripts/release/validate-fastly.py")


def _version(value):
    if not isinstance(value, str) or not re.fullmatch(r"[0-9]+(?:\.[0-9]+){0,2}", value):
        raise ValueError("unknown or nonnumeric compatibility version")
    components = list(map(int, value.split(".")))
    while len(components) > 1 and components[-1] == 0:
        components.pop()
    return tuple(components)


def validate_catalog(tools):
    """Validate source-owned selections; null observations stay pending."""
    preflight.validate_tools(tools)


def selection_digest(tools):
    """Bind executable selections, excluding derived recipe/evidence digests.

    The raw whole tools JSON digest is separately recorded. Including it in this
    fingerprint would require a result to hash the future file containing itself.
    """
    selected = {
        "schema_version": tools["schema_version"],
        "tools": [{key: value for key, value in tool.items()
                   if key != "qualification"} for tool in tools["tools"]],
        "environments": [{key: value for key, value in environment.items()
                          if key != "qualification"}
                         for environment in tools["environments"]],
        "recipes": [{key: value for key, value in recipe.items()
                     if key not in {"sha256", "qualification"}} for recipe in tools["recipes"]],
    }
    return sha256_bytes(b"trusted-server-execution-selection-v1\n" + deterministic_json_bytes(selected))


def candidate_fingerprints(root, tools, recipe_id):
    """Bind the reviewed recipe closure; absent Task 9 helpers refuse staging."""
    validate_catalog(tools)
    if recipe_id not in RECIPE_ENVIRONMENTS:
        raise ValueError("unknown fixed recipe identity")
    # The complete cohort needs the shared production Rust/runtime gates even
    # when inspecting its JS leg. No version-only substitute closes this gate.
    paths = (*SHARED_CLOSURE, *RUST_CLOSURE)
    if any(not (Path(root) / path).is_file() for path in paths):
        raise ValueError("Task 9 missing reviewed closure files")
    recipe = next(recipe for recipe in tools["recipes"] if recipe["id"] == recipe_id)
    return {"closure_sha256": closure_digest(root, paths, recipe["settings"]),
            "selection_sha256": selection_digest(tools)}


def closure_digest(root, paths, settings):
    """Hash a reviewed sorted regular-file closure and effective settings.

    The caller is source code, never a downloaded descriptor. Symlinks, escaped
    paths, duplicate names and derived tools/results files are rejected.
    """
    root = Path(root).resolve(strict=True)
    names = sorted(paths)
    if len(names) != len(set(names)) or not names:
        raise ValueError("closure requires unique reviewed files")
    files = []
    for name in names:
        path = root / name
        if (not isinstance(name, str) or Path(name).is_absolute() or ".." in Path(name).parts
                or path.is_symlink() or not path.resolve(strict=True).is_relative_to(root)
                or any(parent.is_symlink() for parent in path.parents if parent.is_relative_to(root))
                or path.name in {"release-tools.json", "results.json"}):
            raise ValueError("unsafe or self-referencing closure path")
        information = path.stat()
        if not stat.S_ISREG(information.st_mode) or information.st_size > 33554432:
            raise ValueError("closure requires bounded regular files")
        files.append({"path": name, "executable": bool(information.st_mode & 0o111),
                      "sha256": sha256_bytes(path.read_bytes())})
    return sha256_bytes(b"trusted-server-recipe-closure-v1\n" +
                        deterministic_json_bytes({"files": files, "settings": settings}))


def _schema_validate(result):
    try:
        from jsonschema import Draft202012Validator
    except ImportError as error:
        raise ValueError("locked qualification schema validator is required") from error
    schema = read_json_file(RELEASE / "qualification/results.schema.json", max_bytes=1048576)
    Draft202012Validator.check_schema(schema)
    errors = sorted(Draft202012Validator(schema).iter_errors(result), key=lambda error: str(error.path))
    if errors:
        raise ValueError(f"qualification result schema: {errors[0].message}")


def _probe_key(probe):
    return tuple(probe[key] for key in ("depth", "family", "protocol", "operation", "destination"))


def validate_socket_pair(positive, confined):
    """Require actual denial across the process tree and private Unix success.

    Unsupported external/IPv6 positive operations remain unavailable; they never
    count as successful controls. Every confined INET operation must deny with
    EPERM/EACCES, even when its matching positive routing test was unavailable.
    """
    expected = {(depth, family, protocol, operation, destination) for depth in range(3)
                for family in ("ipv4", "ipv6") for protocol in ("tcp", "udp")
                for operation in ("bind", "connect", "send")
                for destination in ("loopback", "external")}
    expected.update((depth, "unix", "unix", "exchange", "private") for depth in range(3))
    for records in (positive, confined):
        if len(records) != len(expected) or {_probe_key(probe) for probe in records} != expected:
            raise ValueError("missing or duplicate socket controls/descendant coverage")
    controls = {_probe_key(probe): probe for probe in positive}
    for probe in confined:
        control = controls[_probe_key(probe)]
        for observation in (probe, control):
            if ((observation["outcome"] == "allowed" and observation["errno"] != 0)
                    or (observation["outcome"] != "allowed" and observation["errno"] == 0)):
                raise ValueError("socket outcome contradicts actual errno")
            if observation["outcome"] == "allowed" and observation["phase"] != observation["operation"]:
                raise ValueError("successful socket observation must complete its intended operation")
        if probe["family"] == "unix":
            if any(p["outcome"] != "allowed" or p["errno"] != 0 for p in (probe, control)):
                raise ValueError("private Unix IPC must succeed")
        else:
            phases = {"socket", probe["operation"]}
            if probe["operation"] == "send" and probe["protocol"] == "tcp":
                phases.add("connect")
            if probe["phase"] not in phases:
                raise ValueError("socket denial happened at an unrelated helper operation")
            if probe["outcome"] != "denied" or probe["errno"] not in {errno.EACCES, errno.EPERM}:
                raise ValueError("confined internet operation was not denied")
            if control["outcome"] == "denied" or control["errno"] in {errno.EACCES, errno.EPERM}:
                raise ValueError("unconfined control was denied")
            if control["family"] == "ipv4" and control["destination"] == "loopback":
                if control["outcome"] != "allowed" or control["errno"] != 0:
                    raise ValueError("unconfined positive IPv4 loopback control required")


def validate_observation(result):
    """Reject incomplete or contradictory hosted observations with ValueError.

    This checks claims and their joins, not remote authenticity. All referenced
    logs, archives, signatures and run identities still need independent review.
    """
    _schema_validate(result)
    for value in (result["schema_version"], result["run"]["attempt"], result["recipe"]["exit_code"],
                  *(runtime["exit_code"] for runtime in result["runtime"]),
                  *(probe[field] for phase in ("unconfined", "confined")
                    for probe in result["recipe"][phase] for field in ("depth", "errno"))):
        require_nonnegative_integer(value)
    host, recipe = result["host"], result["recipe"]
    if host["environment_id"] != RECIPE_ENVIRONMENTS[recipe["id"]]:
        raise ValueError("recipe builder mismatch")
    mac = host["environment_id"].startswith("macos")
    architecture = "aarch64" if host["environment_id"] == "macos15-aarch64" else "x86_64"
    if (host["architecture"] != architecture or host["kernel_architecture"] != architecture
            or host["translated"]):
        raise ValueError("wrong or translated host architecture")
    if mac:
        if (_version(host["os_version"])[0] != 15 or host["kind"] != "github-hosted-macos"
                or not host["runner_image_version"] or host["oci_index_sha256"] is not None
                or host["oci_manifest_sha256"] is not None or host["glibc_version"] is not None
                or result["sdk"]["kind"] != "apple-sdk" or recipe["deployment_target"] != "13.0"
                or recipe["glibc_symbols"]):
            raise ValueError("macOS hosted identity/SDK/loader floor mismatch")
    else:
        if (host["kind"] != "oci-builder" or not host["oci_index_sha256"]
                or not host["oci_manifest_sha256"] or host["runner_image_version"] is not None
                or _version(host["glibc_version"]) > (2, 34)
                or result["sdk"]["kind"] != "gnu-sysroot" or recipe["deployment_target"] is not None):
            raise ValueError("unqualified GNU builder/sysroot identity")
        for symbol in recipe["glibc_symbols"]:
            if not symbol.startswith("GLIBC_") or _version(symbol[6:]) > (2, 34):
                raise ValueError("GNU symbol exceeds glibc 2.34 floor")
    names = [tool["name"] for tool in result["tools"]]
    required = BASE_TOOLS | ({"viceroy", "wasmtime"} if recipe["id"] == "trusted-server-fastly.wasm" else set())
    if set(names) != required or len(names) != len(required):
        raise ValueError("missing, duplicate or unselected tool identity")
    if recipe["exit_code"] or not recipe["full_recipe"] or not recipe["same_boundary"]:
        raise ValueError("full recipe must succeed inside the same boundary")
    if recipe["download_attempts"] != {"cargo": "denied", "npm": "denied", "missing_tool": "failed"}:
        raise ValueError("download/missing-tool negative fixtures required")
    validate_socket_pair(recipe["unconfined"], recipe["confined"])
    if recipe["id"] == "trusted-server-fastly.wasm":
        expected = {"fastly-viceroy"}
        runtime_kind = "publisher-request"
        runtime_architecture = "wasm32"
    elif recipe["id"] == "tsjs":
        expected = {"js-pair"}
        runtime_kind = "js-tests-and-equality"
        runtime_architecture = architecture
    else:
        expected = {host["environment_id"]} if mac else {"ubi9-x86_64", "ubuntu22.04-x86_64", "debian12-x86_64"}
        runtime_kind = "cli-startup"
        runtime_architecture = architecture
    runtimes = result["runtime"]
    if {runtime["environment_id"] for runtime in runtimes} != expected or len(runtimes) != len(expected):
        raise ValueError("missing actual compatibility/application execution")
    for runtime in runtimes:
        if (runtime["exit_code"] or not runtime["outside_checkout"] or runtime["kind"] != runtime_kind
                or runtime["architecture"] != runtime_architecture
                or runtime["executable_sha256"] != recipe["executable_sha256"]
                or runtime["executable_after_sha256"] != recipe["executable_sha256"]):
            raise ValueError("failed startup/application request or changed final bytes")
        if mac and (_version(runtime["os_version"])[0] != 15
                    or runtime["os_version"] != host["os_version"]):
            raise ValueError("invented macOS compatibility")
        if not mac and runtime_kind == "cli-startup":
            version = _version(runtime["os_version"])
            expected_version = {"ubi9-x86_64": (9,), "ubuntu22.04-x86_64": (22, 4),
                                "debian12-x86_64": (12,)}[runtime["environment_id"]]
            if version[:len(expected_version)] != expected_version:
                raise ValueError("invented Linux runtime compatibility")
        if runtime_kind != "cli-startup" and runtime["response_sha256"] is None:
            raise ValueError("complete application/equality response evidence required")
    return result


def require_qualified(result):
    """Refuse self-conferred qualification even for complete observations."""
    validate_observation(result)
    raise ValueError("qualification pending: independently authenticated hosted evidence and source approval required")


def check_execution_prerequisites(root, output):
    """Refuse execution before shared Task 9 recipes and authentic pins exist."""
    root = Path(root).resolve(strict=True)
    for name in ("build.sh", "inspect-native.py", "validate-fastly.py"):
        if not (root / "scripts/release" / name).is_file():
            raise ValueError("Task 9 shared production recipe/runtime gates are not implemented")
    tools = read_json_file(root / "scripts/release/release-tools.json", max_bytes=1048576)
    preflight.validate_tools(tools)
    # Staging cannot require production qualification: it measures candidate
    # selections before the later source-reviewed result pins exist.
    raise ValueError("authenticated candidate staging dispatcher pending resource authorization")


def main():
    """Offer bounded observation validation and a closed readiness check."""
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    validate = commands.add_parser("validate-observation")
    validate.add_argument("result", type=Path)
    commands.add_parser("check-ready")
    arguments = parser.parse_args()
    try:
        if arguments.command == "validate-observation":
            validate_observation(read_json_file(arguments.result, max_bytes=1048576))
            sys.stdout.write("valid observation; release qualification remains pending\n")
        else:
            check_execution_prerequisites(RELEASE.parents[1], None)
    except (ValueError, OSError) as error:
        logging.error("Qualification refuses input: %s", error)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
