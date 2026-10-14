"""Check the fixed release contract before any preparation or signing.

This module selects no executable from downloaded JSON. Qualification remains
pending until authenticated acquisition and the real hosted recipes are reviewed.
"""

import argparse
import io
import logging
import os
import re
import stat
import subprocess
import sys
import tomllib
from pathlib import Path

# The preflight must not add Python cache files to its source-only directory.
sys.dont_write_bytecode = True

from common import deterministic_json_bytes, read_json_file, require_nonnegative_integer, sha256_bytes

LIMIT = 1048576
GIT_OBJECT_LIMIT = 33554432
GIT_TOTAL_LIMIT = 536870912
GIT_MEMBER_LIMIT = 100000
GIT_METADATA_BATCH = 256
SOURCE_DIRECTORIES = ("src", "build_support", "tests", "benches", "examples")
PLATFORMS = (
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
)
ATTEST_REVISION = "1e69f48acb82d1966a394da916b4c1698aa569d6"
VERSIONS = {
    "actions/attest": "4.2.2",
    "fastly": "15.1.0",
    "gh": "2.97.0",
    "node": "24.12.0",
    "npm": "11.6.2",
    "rust": "1.95.0",
    "viceroy": "0.17.0",
    "wasmtime": "44.0.1",
}


def fixed_artifacts():
    """Return the four source-owned artifact identities and build selections."""
    artifacts = []
    for target in PLATFORMS:
        linux = target.endswith("linux-gnu")
        artifacts.append({
            "name": f"ts-{target}",
            "role": "operator-cli",
            "kind": "native-executable",
            "package": "trusted-server-cli",
            "bin": "ts",
            "target": target,
            "variant": "default",
            "default_features": True,
            "features": [],
            "assurance_profile": "provenance-sbom",
            "capabilities": ["cli-execute"],
            "compatibility": {
                "runtime": "linux-gnu" if linux else "macos",
                "minimum_version": "glibc-2.34" if linux else "13.0",
                "architecture": target.split("-")[0],
            },
        })
    artifacts.append({
        "name": "trusted-server-fastly.wasm",
        "role": "server",
        "kind": "wasm-core",
        "package": "trusted-server-adapter-fastly",
        "bin": "trusted-server-adapter-fastly",
        "target": "wasm32-wasip1",
        "variant": "default",
        "default_features": True,
        "features": [],
        "assurance_profile": "provenance-sbom-repeatable",
        "capabilities": ["fastly-production"],
        "compatibility": {
            "runtime": "fastly-compute",
            "minimum_version": "wasip1",
            "architecture": "wasm32",
        },
    })
    return sorted(artifacts, key=lambda artifact: artifact["name"])


def fixed_tools():
    """Return exact version selections with explicitly unmeasured digests."""
    tools = []
    for name, version in sorted(VERSIONS.items()):
        platforms = ("github-action",) if name == "actions/attest" else PLATFORMS
        for platform in platforms:
            tools.append({
                "name": f"{name}:{platform}",
                "version": version,
                "revision": ATTEST_REVISION if name == "actions/attest" else None,
                "sha256": None,
                "acquisition": None,
                "qualification": "pending",
            })
    # Hosted image versions are platform observations, never VM byte hashes.
    environments = []
    for identity in ("debian12-x86_64", "gnu-builder-x86_64", "macos15-aarch64",
                     "macos15-x86_64", "ubi9-x86_64", "ubuntu22.04-x86_64"):
        mac = identity.startswith("macos")
        builder = identity == "gnu-builder-x86_64"
        environment = {
            "id": identity,
            "kind": "github-hosted-macos" if mac else "oci-builder" if builder else "oci-runtime",
            "role": "compilation" if mac or builder else "compatibility-runtime",
            "acquisition": None,
            "qualification": "pending",
        }
        if mac:
            environment.update(runner_label="macos-15" if identity.endswith("aarch64")
                               else "macos-15-intel", runner_image_version=None, sdk_sha256=None)
        else:
            environment.update(image_reference=None, oci_index_sha256=None, oci_manifest_sha256=None)
            if builder:
                environment.update(sdk_sha256=None, glibc_maximum="2.34")
        environments.append(environment)
    recipes = []
    for artifact in fixed_artifacts():
        recipes.append({
            "id": artifact["name"],
            "environment_id": "macos15-aarch64" if artifact["target"] == "aarch64-apple-darwin"
                              else "macos15-x86_64" if artifact["target"] == "x86_64-apple-darwin"
                              else "gnu-builder-x86_64",
            "script": "scripts/release/build.sh",
            "sha256": None,
            "qualification": "pending",
            "settings": recipe_settings(),
        })
    recipes.append({
        "id": "tsjs",
        "environment_id": "gnu-builder-x86_64",
        "script": "scripts/release/build-js.sh",
        "sha256": None,
        "qualification": "pending",
        "settings": js_recipe_settings(),
    })
    return {
        "schema_version": 1,
        "tools": tools,
        "environments": environments,
        "recipes": sorted(recipes, key=lambda recipe: recipe["id"]),
    }


def recipe_settings():
    """Return reviewed effective settings for the later executable recipes."""
    return {
        "cargo_locked": True,
        "cargo_config": ".cargo/config.toml",
        "cargo_home": "private-prepared",
        "rust_toolchain": "1.95.0",
        "release_debug": 1,
        "fresh_target": True,
        "network_boundary": "qualification-pending",
        "tool_paths": "authenticated-absolute-paths-required",
        "sysroot": "authenticated-sdk-required",
        "aws_lc_use_system": False,
        "aws_lc_target_selector": True,
        "sqlite_features": ["bundled"],
        "sqlite_use_pkg_config": False,
        "vite_mode": "production",
        "vite_env_loading": False,
        "vite_config_loading": False,
        "npm_lifecycle": "reviewed-preparation-only",
    }


def js_recipe_settings():
    """Return only effective selectors executed by the independent JS recipe."""
    return {
        "fresh_source": "tracked-objects-only",
        "fresh_installations": 2,
        "fresh_private_npm_caches": True,
        "npm_version": "11.6.2",
        "node_version": "24.12.0",
        "npm_offline": True,
        "npm_include_dev": True,
        "npm_audit": False,
        "npm_fund": False,
        "npm_lifecycle": "reviewed-observed-foreground",
        "mandatory_tests": True,
        "compare": "all-tracked-final-module-bytes",
        "publish": "A-only-after-equality",
        "network_boundary": "qualification-pending",
        "tool_paths": "authenticated-absolute-paths-required",
        "vite_mode": "production",
        "vite_env_loading": False,
        "vite_config_loading": False,
    }


def fixed_policy():
    """Return the embedded consumer contract; its hash is computed externally."""
    repository = "https://github.com/IABTechLab/trusted-server"
    return {
        "policy_version": 1,
        "cohort": "phase1-fastly-cli",
        "github_host": "github.com",
        "repository": "IABTechLab/trusted-server",
        "repository_id": "950860906",
        "repository_owner_id": "82903538",
        "identity": {
            "issuer": "https://token.actions.githubusercontent.com",
            "certificate_san": repository + "/.github/workflows/release-build.yml@refs/tags/<tag>",
            "builder_id": repository + "/.github/workflows/release-build.yml@refs/tags/<tag>",
            "caller_path": ".github/workflows/release.yml",
            "caller_repository": repository,
            "source_ref": "refs/tags/<tag>",
            "source_dependency": "git+" + repository + "@refs/tags/<tag>",
            "source_commit": "independently-approved-40-lowercase-hex",
            "signer_commit": "same-approved-source-commit",
            "caller_commit": "same-approved-source-commit",
            "event": "push",
            "runner": "github-hosted",
            "invocation": "same-repository-run-and-attempt-across-certificate-and-provenance",
            "external_parameters": "exact-caller-workflow-only-no-inputs",
            "certificate_fields": [
                "buildConfigDigest", "buildConfigURI", "buildTrigger", "runInvocationURI",
                "sourceRepositoryIdentifier", "sourceRepositoryOwnerIdentifier",
            ],
        },
        "predicates": {
            "provenance": "https://slsa.dev/provenance/v1",
            "build_type": "https://actions.github.io/buildtypes/workflow/v1",
            "sbom": "https://spdx.dev/Document/v2.3",
        },
        "tool_versions": {
            "gh": "2.97.0",
            "actions_attest": "4.2.2",
            "actions_attest_revision": ATTEST_REVISION,
        },
        "historical_producer_policies": [],
        "public_actions": ["readonly-verify"],
        "artifacts": [dict(artifact, actions=["readonly-verify"])
                      for artifact in fixed_artifacts()],
        "bounds": {
            "child_seconds": 120,
            "attempt_seconds": 900,
            "stdout_bytes": 134217728,
            "stderr_bytes": 1048576,
            "metadata_bytes": 1048576,
            "index_bytes": 1048576,
            "policy_bytes": 1048576,
            "inventory_bytes": 1048576,
            "trusted_roots_bytes": 1048576,
            "companion_bytes": 33554432,
            "bundle_bytes": 33554432,
            "spdx_bytes": 16777216,
            "executable_bytes": 536870912,
            "json_depth": 64,
            "cohort_executables": 4,
            "matching_attestations_per_bundle": 1,
        },
    }


def _same_contract(actual, expected, label):
    # Type comparison prevents Python's True == 1 from weakening JSON types.
    if type(actual) is not type(expected):
        raise ValueError(f"{label}: unexpected type")
    if isinstance(expected, dict):
        if actual.keys() != expected.keys():
            raise ValueError(f"{label}: missing or unknown fields")
        for key, value in expected.items():
            _same_contract(actual[key], value, f"{label}.{key}")
    elif isinstance(expected, list):
        if len(actual) != len(expected):
            raise ValueError(f"{label}: unexpected membership")
        for index, (value, wanted) in enumerate(zip(actual, expected)):
            _same_contract(value, wanted, f"{label}[{index}]")
    elif actual != expected:
        raise ValueError(f"{label}: unreviewed selection")


def validate_cohort(cohort):
    """Reject unknown artifacts, selections and branch patterns with ValueError."""
    if not isinstance(cohort, dict) or set(cohort) != {
        "schema_version", "cohort", "eligible_branches", "artifacts"
    }:
        raise ValueError("cohort: missing or unknown fields")
    branches = cohort["eligible_branches"]
    if (not isinstance(branches, list) or not branches
            or any(not isinstance(branch, str) for branch in branches)
            or branches != sorted(set(branches)) or "main" not in branches
            or any(branch != "main" and not re.fullmatch(
                r"rc/[0-9]{4}(?:0[1-9]|1[0-2])", branch
            ) for branch in branches)):
        raise ValueError("cohort: branches must be exact reviewed main/rc entries")
    _same_contract(cohort, {
        "schema_version": 1,
        "cohort": "phase1-fastly-cli",
        "eligible_branches": branches,
        "artifacts": fixed_artifacts(),
    }, "cohort")


def validate_tools(tools, *, require_qualified=False):
    """Check selections, refusing release use until Task 8 qualification exists."""
    expected = fixed_tools()
    # Digest strings alone do not confer qualification. Task 8 owns evidence.
    for section, digest_fields in (
        ("tools", ("sha256",)),
        ("environments", ("oci_index_sha256", "oci_manifest_sha256", "sdk_sha256")),
        ("recipes", ("sha256",)),
    ):
        records = tools.get(section) if isinstance(tools, dict) else None
        if not isinstance(records, list) or len(records) != len(expected[section]):
            raise ValueError(f"tools.{section}: unexpected membership")
        for record, wanted in zip(records, expected[section]):
            if not isinstance(record, dict):
                raise ValueError(f"tools.{section}: expected record")
            for field in digest_fields:
                if field not in wanted:
                    continue
                digest = record.get(field)
                if digest is not None and (
                    not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest)
                ):
                    raise ValueError("tools: invalid measured digest")
                wanted[field] = digest
    _same_contract(tools, expected, "tools")
    if require_qualified:
        raise ValueError("release qualification pending: authenticated acquisition and Task 8 evidence required")


def validate_policy(policy):
    """Reject consumer policy weakening or self-selected trust with ValueError."""
    _same_contract(policy, fixed_policy(), "policy")


def load_contracts(cohort_path, tools_path, policy_path, *, require_qualified=False):
    """Load bounded strict contracts and reject incomplete release qualification."""
    cohort = read_json_file(cohort_path, max_bytes=LIMIT)
    tools = read_json_file(tools_path, max_bytes=LIMIT)
    policy = read_json_file(policy_path, max_bytes=LIMIT)
    validate_cohort(cohort)
    validate_tools(tools, require_qualified=require_qualified)
    validate_policy(policy)
    return cohort, tools, policy


def audit_environment(inherited):
    """Reject unreviewed build overrides before clearing inherited values.

    Diagnostics contain variable names only. Publisher and provider values are
    removed by the fresh child environment rather than interpreted as build facts.
    """
    for name in inherited:
        upper = name.upper()
        if name == "TRUSTED_SERVER__RELEASE_BUILD" and inherited[name] != "1":
            raise ValueError("TRUSTED_SERVER__RELEASE_BUILD must be exactly 1 when present")
        if upper.startswith(("TRUSTED_SERVER__", "EDGEZERO__")):
            continue
        if upper == "TSJS_SKIP_BUILD":
            raise ValueError("TSJS_SKIP_BUILD is forbidden with every value")
        forbidden_prefix = upper.startswith((
            "AWS_LC_SYS_", "CARGO", "RUST", "NODE_", "NPM_", "VITE_",
            "LIBSQLITE3_", "SQLITE", "SQLCIPHER", "PKG_CONFIG", "VCPKG",
            "GIT_", "PYTHON", "LD_", "DYLD_", "BINDGEN_", "CLANG_", "TSJS_",
        ))
        compiler = re.search(
            r"(?:^|_)(?:CC|CXX|CPP|AR|AS|RANLIB|CFLAGS|CXXFLAGS|CPPFLAGS|LDFLAGS|"
            r"OPENSSL|SYSROOT)(?:_|$)", upper
        )
        if forbidden_prefix or compiler or upper in {
            "ENV", "BASH_ENV", "SHELLOPTS", "BASHOPTS", "CDPATH", "IFS",
            "SDKROOT", "DEVELOPER_DIR", "MACOSX_DEPLOYMENT_TARGET",
            "EFFECTIVE_TARGET", "TARGET", "HOST", "PROFILE", "OPT_LEVEL",
            "DEBUG", "OUT_DIR", "SOURCE_DATE_EPOCH", "MAKEFLAGS", "MFLAGS",
            "CMAKE_TOOLCHAIN_FILE", "CMAKE_PREFIX_PATH", "CPATH", "LIBRARY_PATH",
            "C_INCLUDE_PATH", "CPLUS_INCLUDE_PATH", "COMPILER_PATH", "GCC_EXEC_PREFIX",
        }:
            raise ValueError(f"unreviewed inherited override: {name}")


def _commit_id(commit):
    if not isinstance(commit, str) or not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("source commit must be independently approved full lowercase hex")
    return commit


def build_environment(inherited, commit, epoch, target, paths):
    """Construct fresh children from recipe-owned absolute paths and facts.

    Paths must come from authenticated recipe selection, never ambient PATH or
    downloaded descriptors. This structural helper does not qualify those paths.
    """
    audit_environment(inherited)
    _commit_id(commit)
    require_nonnegative_integer(epoch)
    if target not in (*PLATFORMS, "wasm32-wasip1"):
        raise ValueError("unknown release target")
    if not isinstance(paths, dict) or set(paths) != {
        "home", "cargo_home", "rustup_home", "tmpdir", "tool_path", "sysroot"
    }:
        raise ValueError("explicit authenticated recipe paths required")
    selected = list(paths.values())
    tool_path = paths["tool_path"]
    if not isinstance(tool_path, list) or not tool_path:
        raise ValueError("explicit tool path directories required")
    selected.remove(tool_path)
    selected.extend(tool_path)
    if any(not isinstance(path, str) or not Path(path).is_absolute()
           or not Path(path).is_dir() for path in selected):
        raise ValueError("recipe paths must be existing absolute directories")
    for filename in ("config", "config.toml"):
        if (Path(paths["cargo_home"]) / filename).exists():
            raise ValueError("private Cargo home must not supply unreviewed configuration")
    environment = {
        "HOME": paths["home"],
        "CARGO_HOME": paths["cargo_home"],
        "RUSTUP_HOME": paths["rustup_home"],
        "TMPDIR": paths["tmpdir"],
        "PATH": os.pathsep.join(tool_path),
        "LANG": "C",
        "LC_ALL": "C",
        "TZ": "UTC",
        "SOURCE_DATE_EPOCH": str(epoch),
        "TRUSTED_SERVER__GIT_VERSION": commit,
        "TRUSTED_SERVER__RELEASE_BUILD": "1",
        "AWS_LC_SYS_USE_SYSTEM": "0",
        f"AWS_LC_SYS_USE_SYSTEM_{target.replace('-', '_')}": "0",
        "LIBSQLITE3_SYS_USE_PKG_CONFIG": "0",
        "NODE_ENV": "production",
        "PYTHONDONTWRITEBYTECODE": "1",
    }
    if target.endswith("apple-darwin"):
        environment["SDKROOT"] = paths["sysroot"]
        environment["MACOSX_DEPLOYMENT_TARGET"] = "13.0"
    return environment


def _git(repo, arguments, data=None):
    # Never inherit replacement objects, config injection, loaders or credentials.
    environment = {
        "PATH": "/usr/bin:/bin",
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_NO_REPLACE_OBJECTS": "1",
        "LC_ALL": "C",
    }
    result = subprocess.run(
        ["/usr/bin/git", "--no-replace-objects", "-C", str(repo), *arguments],
        input=data, env=environment, capture_output=True, timeout=120, check=False,
    )
    if result.returncode:
        raise ValueError("Git source inspection failed")
    return result.stdout


def read_git_objects(repo, objects, *, max_object=GIT_OBJECT_LIMIT,
                     max_total=GIT_TOTAL_LIMIT, max_members=GIT_MEMBER_LIMIT):
    """Check pinned object sizes before capturing any selected Git bodies.

    Use the sanitized trusted Git executable and immutable object store, never
    replacement/alternate channels. At most 256 validated full OIDs are queried
    per metadata request; Git's fixed SHA-1/type/64-bit-size format bounds each
    response. Reject oversized objects or aggregate bodies before requesting
    them, then rejoin body headers to the measured OID, type and size. This is
    metadata prechecking, not race-proof streaming against a mutable/corrupt
    object store. Raise ValueError for invalid pins, metadata, limits or bodies.
    """
    for bound in (max_object, max_total, max_members):
        require_nonnegative_integer(bound)
    if not isinstance(objects, dict) or len(objects) > max_members:
        raise ValueError("source object member bound exceeded")
    for oid, kind in objects.items():
        if (not isinstance(oid, str) or not re.fullmatch(r"[0-9a-f]{40}", oid)
                or not isinstance(kind, str) or kind not in {"blob", "tree", "commit"}):
            raise ValueError("invalid pinned source object identity")
    selected = list(objects)
    sizes = {}
    total = 0
    for start in range(0, len(selected), GIT_METADATA_BATCH):
        batch = selected[start:start + GIT_METADATA_BATCH]
        metadata = _git(repo, ["cat-file", "--batch-check"],
                        "".join(oid + "\n" for oid in batch).encode("ascii"))
        if len(metadata) > 80 * len(batch):
            raise ValueError("source object metadata bound exceeded")
        lines = metadata.splitlines()
        if len(lines) != len(batch):
            raise ValueError("source object metadata membership mismatch")
        for oid, line in zip(batch, lines):
            fields = line.decode("ascii").split()
            if (len(fields) != 3 or fields[:2] != [oid, objects[oid]]
                    or not re.fullmatch(r"[0-9]+", fields[2])):
                raise ValueError("source object metadata identity/type/size mismatch")
            size = int(fields[2])
            total += size
            if size > max_object or total > max_total:
                raise ValueError("source object size bound exceeded")
            sizes[oid] = size
    if not selected:
        return {}
    bodies = io.BytesIO(_git(repo, ["cat-file", "--batch"],
                            "".join(oid + "\n" for oid in selected).encode("ascii")))
    result = {}
    for oid, size in sizes.items():
        header = bodies.readline().decode("ascii").strip().split()
        if header != [oid, objects[oid], str(size)]:
            raise ValueError("source object body disagrees with checked metadata")
        content = bodies.read(size)
        if len(content) != size or bodies.read(1) != b"\n":
            raise ValueError("incomplete source object body")
        result[oid] = content
    if bodies.read(1):
        raise ValueError("extra source object bodies")
    return result


def _audit_configuration(repo, inherited):
    configuration = _git(repo, ["config", "--local", "--no-includes", "--name-only", "--list"])
    for key in configuration.decode("utf-8").splitlines():
        if key.startswith(("include.", "includeif.", "filter.", "extensions.")) or key in {
            "core.worktree", "core.fsmonitor", "core.attributesfile", "core.sshcommand",
        }:
            raise ValueError("unreviewed Git configuration channel")
    if _git(repo, ["for-each-ref", "--format=%(refname)", "refs/replace/"]).strip():
        raise ValueError("Git replacement references are forbidden")
    git_directory = Path(_git(repo, ["rev-parse", "--absolute-git-dir"]).decode().strip())
    for relative in ("objects/info/alternates", "info/grafts"):
        if (git_directory / relative).exists():
            raise ValueError("Git alternative object or graft channel is forbidden")
    locations = list(repo.parents)
    home = inherited.get("HOME")
    if home:
        locations.append(Path(home))
    for location in locations:
        if location == repo:
            continue
        for filename in ("config", "config.toml"):
            if os.path.lexists(location / ".cargo" / filename):
                raise ValueError("unreviewed ancestor/home Cargo configuration")
    crate_locations = [repo / "crates", repo / "crates/trusted-server-js/lib"]
    crate_locations.extend(path for path in repo.glob("crates/*") if path.is_dir())
    for location in crate_locations:
        configuration_directory = location / ".cargo"
        if configuration_directory.is_symlink():
            raise ValueError("unsupported nested Cargo configuration symlink")
        for filename in ("config", "config.toml"):
            if os.path.lexists(configuration_directory / filename):
                raise ValueError("unsupported nested workspace Cargo configuration layer")
    if os.path.lexists(repo / ".cargo/config"):
        raise ValueError("unreviewed legacy Cargo configuration layer")
    selected = repo / ".cargo/config.toml"
    if selected.exists():
        configuration = tomllib.loads(selected.read_text(encoding="utf-8"))
        if set(configuration) - {"alias", "target"}:
            raise ValueError("Cargo release configuration may select only reviewed aliases and test runners")
        aliases = configuration.get("alias", {})
        targets = configuration.get("target", {})
        if not isinstance(aliases, dict) or any(not isinstance(value, str) for value in aliases.values()):
            raise ValueError("Cargo aliases must be strings")
        if not isinstance(targets, dict) or any(
            name != 'cfg(all(target_arch = "wasm32"))'
            or not isinstance(settings, dict) or set(settings) != {"runner"}
            or settings["runner"] != "viceroy run -C ../../fastly.toml -- "
            for name, settings in targets.items()
        ):
            raise ValueError("unreviewed Cargo target compiler/linker settings")


def _source_roots(repo, entries):
    roots = {".cargo", "scripts/release", "crates/trusted-server-js/lib"}
    for name in entries:
        parts = Path(name).parts
        if parts[0] == "crates":
            for directory in SOURCE_DIRECTORIES:
                if directory in parts:
                    roots.add(str(Path(*parts[:parts.index(directory) + 1])))
    crates = repo / "crates"
    if crates.exists():
        if crates.is_symlink():
            raise ValueError("symlinked source parent")
        for crate in crates.iterdir():
            if crate.is_symlink():
                raise ValueError("symlinked source crate")
            if crate.is_dir():
                for directory in SOURCE_DIRECTORIES:
                    if os.path.lexists(crate / directory):
                        roots.add(str((crate / directory).relative_to(repo)))
    return roots


def _check_source_parents(repo, path):
    for parent in path.parents:
        if parent == repo:
            break
        if parent.is_symlink():
            raise ValueError("symlinked source parent")


def _unexpected_sources(repo, entries):
    """Audit all JS validation sources, separating direct npm build materials.

    Prepared node_modules contents require a separate material audit. Cargo's
    exact empty npm-ci lock is coordination state; other lib files are sources.
    """
    library = repo / "crates/trusted-server-js/lib"
    for relative in sorted(_source_roots(repo, entries)):
        root = repo / relative
        _check_source_parents(repo, root)
        if root.is_symlink():
            raise ValueError("symlinked source directory")
        if not root.exists():
            continue
        for directory, children, filenames in os.walk(root, followlinks=False):
            for name in children + filenames:
                path = Path(directory) / name
                relative_path = str(path.relative_to(repo))
                if path == library / "node_modules":
                    if not stat.S_ISDIR(path.lstat().st_mode):
                        raise ValueError("direct node_modules must be a regular directory")
                    children.remove(name)
                    continue
                if path == library / ".tsjs-npm-ci.lock":
                    information = path.lstat()
                    if (not stat.S_ISREG(information.st_mode)
                            or information.st_size != 0 or information.st_nlink != 1):
                        raise ValueError(".tsjs-npm-ci.lock must be an empty single-link regular file")
                    continue
                if path.is_symlink() and relative_path not in entries:
                    raise ValueError("unexpected source symlink")
                if not path.is_dir() and relative_path not in entries:
                    raise ValueError(f"unexpected source-only file: {relative_path}")
    # Root build scripts and JS discovery/configuration scripts are compile inputs.
    for directory in [repo, *repo.glob("crates/*"), repo / "crates/trusted-server-js/lib"]:
        if not directory.is_dir():
            continue
        for path in directory.iterdir():
            if path.suffix in {".rs", ".mjs", ".cjs", ".js", ".ts"} or path.name in {
                "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "package.json", "package-lock.json",
                ".npmrc",
            }:
                if str(path.relative_to(repo)) not in entries:
                    raise ValueError("unexpected build source/configuration file")


def source_snapshot(repo, commit, inherited):
    """Compare bytes, modes and links to the committed tree, ignoring index flags.

    Raise ValueError for mutations, unreviewed source additions, Git/Cargo input
    channels or a revision that is not an actual approved full commit object.
    """
    audit_environment(inherited)
    _commit_id(commit)
    repo = Path(repo).absolute()
    if repo.is_symlink():
        raise ValueError("symlinked source checkout")
    # OS aliases such as /var -> /private/var are outside the source boundary.
    repo = repo.resolve()
    _audit_configuration(repo, inherited)
    if _git(repo, ["cat-file", "-t", commit]).strip() != b"commit":
        raise ValueError("source revision must be an actual commit object")
    entries = {}
    for record in _git(repo, ["ls-tree", "-rz", "--full-tree", commit]).split(b"\0"):
        if not record:
            continue
        identity, filename = record.split(b"\t", 1)
        mode, kind, object_id = identity.decode("ascii").split()
        name = filename.decode("utf-8")
        if kind != "blob" or mode not in {"100644", "100755", "120000"}:
            raise ValueError("unsupported source tree type or mode")
        if Path(name).is_absolute() or ".." in Path(name).parts:
            raise ValueError("unsafe source tree path")
        entries[name] = (mode, object_id)
    _unexpected_sources(repo, entries)
    objects = {commit: "commit"}
    objects.update({oid: "blob" for _, oid in entries.values()})
    blobs = read_git_objects(repo, objects)
    # Read the selected object directly; tracked snapshots need no parent history.
    header = blobs[commit].split(b"\n\n", 1)[0]
    committers = [line for line in header.splitlines() if line.startswith(b"committer ")]
    if len(committers) != 1 or not re.search(rb" [0-9]+ [+-][0-9]{4}$", committers[0]):
        raise ValueError("invalid committed source timestamp")
    epoch = int(committers[0].rsplit(b" ", 2)[1])
    require_nonnegative_integer(epoch)
    files = {}
    for name, (mode, object_id) in entries.items():
        expected = blobs[object_id]
        path = repo / name
        _check_source_parents(repo, path)
        try:
            information = path.lstat()
        except FileNotFoundError as error:
            raise ValueError("missing committed source file") from error
        if mode == "120000":
            if not stat.S_ISLNK(information.st_mode):
                raise ValueError("source link mode changed")
            actual = os.fsencode(os.readlink(path))
            if not path.resolve().is_relative_to(repo):
                raise ValueError("source link escapes checkout")
        else:
            if not stat.S_ISREG(information.st_mode):
                raise ValueError("source file mode changed")
            expected_execute = mode == "100755"
            if bool(information.st_mode & stat.S_IXUSR) != expected_execute:
                raise ValueError("source executable mode changed")
            actual = path.read_bytes()
        if actual != expected:
            raise ValueError(f"committed source bytes changed: {name}")
        files[name] = {
            "mode": mode,
            "permissions": stat.S_IMODE(information.st_mode),
            "sha256": sha256_bytes(actual),
        }
    return {"source_commit": commit, "source_date_epoch": epoch, "files": files}


def assert_source_unchanged(repo, commit, before, inherited):
    """Repeat the committed-tree check after execution and reject any mutation."""
    if source_snapshot(repo, commit, inherited) != before:
        raise ValueError("source changed during release execution")


def _validate_build_inputs(repo):
    workspace = tomllib.loads((repo / "Cargo.toml").read_text(encoding="utf-8"))
    release = workspace.get("profile", {}).get("release", {})
    if type(release.get("debug")) is not int or release["debug"] != 1:
        raise ValueError("source release profile must retain debug=1")
    toolchain = tomllib.loads((repo / "rust-toolchain.toml").read_text(encoding="utf-8"))
    if toolchain.get("toolchain", {}).get("channel") != VERSIONS["rust"]:
        raise ValueError("source Rust toolchain pin disagrees with release contract")
    versions = {}
    for line in (repo / ".tool-versions").read_text(encoding="utf-8").splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        fields = line.split()
        if len(fields) != 2 or fields[0] in versions:
            raise ValueError("ambiguous source tool version pin")
        versions[fields[0]] = fields[1]
    for name, key in (("rust", "rust"), ("node", "nodejs"), ("fastly", "fastly"),
                      ("viceroy", "viceroy"), ("wasmtime", "wasmtime")):
        if versions.get(key) != VERSIONS[name]:
            raise ValueError("source tool pin disagrees with release contract")
    for artifact in fixed_artifacts():
        manifest = tomllib.loads((repo / "crates" / artifact["package"] / "Cargo.toml").read_text(encoding="utf-8"))
        if manifest.get("package", {}).get("name") != artifact["package"]:
            raise ValueError("source package identity disagrees with release contract")
        binaries = manifest.get("bin", [])
        if binaries and not any(binary.get("name") == artifact["bin"] for binary in binaries):
            raise ValueError("source binary identity disagrees with release contract")
        if manifest.get("features", {}).get("default", []):
            raise ValueError("source default features require release contract review")


def preflight(repo, commit, *, inherited=None, cohort_path=None, require_qualified=False):
    """Validate source-owned inputs and report exact contract byte digests.

    Structural success is explicitly unqualified. Release callers must require
    qualification; this gate fails before any preparation or signing is possible.
    """
    inherited = dict(os.environ) if inherited is None else inherited
    audit_environment(inherited)
    _commit_id(commit)
    repo = Path(repo).resolve()
    selected_cohort = repo / "scripts/release/release-cohort.json"
    if cohort_path is not None and Path(cohort_path).resolve() != selected_cohort:
        raise ValueError("cohort must be the fixed source-owned contract path")
    before = source_snapshot(repo, commit, inherited)
    _validate_build_inputs(repo)
    paths = (
        selected_cohort,
        repo / "scripts/release/release-tools.json",
        repo / "crates/trusted-server-cli/release-policy.json",
    )
    for path in paths:
        if str(path.relative_to(repo)) not in before["files"] or path.is_symlink():
            raise ValueError("release contracts must be tracked regular source files")
    cohort, _, _ = load_contracts(*paths, require_qualified=require_qualified)
    digests = {str(path.relative_to(repo)): sha256_bytes(path.read_bytes()) for path in paths}
    assert_source_unchanged(repo, commit, before, inherited)
    return {
        "source_commit": commit,
        "source_date_epoch": before["source_date_epoch"],
        "cohort": cohort["cohort"],
        "contract_sha256": digests,
        "qualified": False,
        "qualification": "pending-authenticated-acquisition-and-hosted-recipes",
    }


def main():
    """Run the read-only preflight command, returning nonzero on any failure."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--cohort", type=Path, required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--require-qualified", action="store_true")
    arguments = parser.parse_args()
    try:
        result = preflight(
            arguments.repo, arguments.source_commit, cohort_path=arguments.cohort,
            require_qualified=arguments.require_qualified,
        )
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        logging.error("Release preflight fails: %s", error)
        return 1
    sys.stdout.buffer.write(deterministic_json_bytes(result))
    return 0


if __name__ == "__main__":
    sys.exit(main())
