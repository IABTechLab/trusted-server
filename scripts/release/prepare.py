"""Authenticate materials and compare independently prepared JS installations.

The official CLI remains closed while qualification is pending. LocalMacBoundary
and local_js_pair are explicitly local fixture APIs: they provide actual denial
and execution observations, never authenticated host/tool acquisition evidence.
"""

import argparse
import base64
import hashlib
import gzip
import io
import logging
import os
import re
import shutil
import stat
import subprocess
import sys
import tarfile
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from urllib.parse import urlsplit

sys.dont_write_bytecode = True
# -I excludes both caller Python paths and this script directory.
sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import (deterministic_json_bytes, load_json_bytes, read_bounded,
                    read_json_file, require_nonnegative_integer, sha256_bytes)
import preflight

MAX_JSON = 1048576
MAX_FILE = 33554432
MAX_ARCHIVE = 134217728
MAX_EXPANDED = 536870912
MAX_MEMBERS = 100000
LIB = Path("crates/trusted-server-js/lib")
HOOKS = ("preinstall", "install", "postinstall")
ROOT_HOOKS = (*HOOKS, "prepublish", "preprepare", "prepare", "postprepare")
REVIEWED_HOOKS = {
    ("esbuild", "0.27.3", "postinstall"): "node install.js",
    ("core-js", "3.48.0", "postinstall"): 'node -e "try{require(\'./postinstall\')}catch(e){}"',
}
# These exact authenticated npm archives contain one identical redundant file.
# The two raw paths are package/./dist/index.js and package/dist/index.js.
# Unknown archives, unequal bytes/modes/types and additional duplicates fail.
NPM_ALIAS_ARCHIVES = {
    "sha512-MnA+YT8fwfJPgBx3m60MNqakm30XOkyIoH1y6huTQvC0PwZG7ki8NacLBcrPbNoo8vEZy7Jpuk7+jMO+CUovTQ==": ("agent-base", "7.1.4"),
    "sha512-T1gkAiYYDWYx3V5Bmyu7HcfcvL7mUrTWiM6yOfa3PIphViJ/gFPbvidQ+veqSOHci/PxBcDabeUNCzpOODJZig==": ("http-proxy-agent", "7.0.2"),
    "sha512-vK9P5/iUfdl95AI+JVyUuIcVtd4ofvtrOr3HNtM2yxC9bnMbEdp3x01OhQNnjb8IJYi38VlTE3mBXwcfvywuSw==": ("https-proxy-agent", "7.0.6"),
}


@dataclass(frozen=True)
class VerifiedArchive:
    """Authenticated original archive identity plus its checked member catalog."""

    path: Path
    integrity: str
    algorithm: str
    root: str
    paths: tuple
    manifest: dict | None


def _regular_bytes(path, maximum):
    path = Path(path)
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
    except OSError as error:
        raise ValueError("material must be a regular file without a symlink") from error
    with os.fdopen(descriptor, "rb") as stream:
        before = os.fstat(stream.fileno())
        identity = lambda value: (value.st_dev, value.st_ino, value.st_mode, value.st_nlink,
                                  value.st_size, value.st_mtime_ns, value.st_ctime_ns)
        if not stat.S_ISREG(before.st_mode) or identity(path.lstat()) != identity(before):
            raise ValueError("material must be a regular file without a symlink")
        data = read_bounded(stream, max_bytes=maximum)
        after = os.fstat(stream.fileno())
        if identity(before) != identity(after) or identity(path.lstat()) != identity(after):
            raise ValueError("material changed during measurement")
        return data


def _authenticated_bytes(path, integrity, algorithm, max_bytes):
    data = _regular_bytes(path, max_bytes)
    if algorithm == "sha512":
        if not isinstance(integrity, str) or not re.fullmatch(r"sha512-[A-Za-z0-9+/]{86}==", integrity):
            raise ValueError("archive integrity must be locked SHA-512")
        expected = base64.b64decode(integrity[7:], validate=True)
        actual = hashlib.sha512(data).digest()
    elif algorithm == "sha256":
        if not isinstance(integrity, str) or not re.fullmatch(r"[0-9a-f]{64}", integrity):
            raise ValueError("archive integrity must be locked SHA-256")
        expected = bytes.fromhex(integrity)
        actual = hashlib.sha256(data).digest()
    else:
        raise ValueError("unsupported archive integrity algorithm")
    if actual != expected:
        raise ValueError("archive integrity mismatch")
    return data


def _approved_alias(integrity, first, second, first_bytes, second_bytes):
    return (integrity in NPM_ALIAS_ARCHIVES and first.isfile() and second.isfile()
            and {first.name, second.name} == {"package/./dist/index.js", "package/dist/index.js"}
            and (first.size, first.mode, first.type) == (second.size, second.mode, second.type)
            and len(first_bytes) == len(second_bytes) == first.size and first_bytes == second_bytes)


def _archive_catalog(data, max_expanded, max_members, integrity=None):
    seen = {}
    roots = set()
    total = 0
    member_count = 0
    merged_alias = False
    # Bound the entire decompressed tar, including PAX/GNU metadata, before parsing.
    with gzip.GzipFile(fileobj=io.BytesIO(data)) as compressed:
        expanded = read_bounded(compressed, max_bytes=max_expanded)
    with tarfile.open(fileobj=io.BytesIO(expanded), mode="r:") as stream:
        for member in stream:
            name = member.name.rstrip("/") if member.isdir() else member.name
            parts = name.split("/")
            if (not name or "\\" in name or "\0" in name or name.startswith("/")
                    or any(part in {"", ".."} for part in parts)):
                raise ValueError("unsafe archive member path")
            parts = [part for part in parts if part != "."]
            if not parts:
                raise ValueError("unsafe archive member path")
            name = "/".join(parts)
            raw_name = member.name.rstrip("/") if member.isdir() else member.name
            if raw_name != name and not (
                    integrity in NPM_ALIAS_ARCHIVES and member.name == "package/./dist/index.js"):
                raise ValueError("unknown archive path alias")
            member_count += 1
            total += member.size
            if total > max_expanded or member_count > max_members:
                raise ValueError("archive expanded size or member bound exceeded")
            if not (member.isdir() or member.isfile()):
                raise ValueError("unsupported archive member")
            if member.mode & 0o7000:
                raise ValueError("privileged archive mode is forbidden")
            if name in seen:
                previous = seen[name]
                if (merged_alias or not previous.isfile() or not member.isfile() or not _approved_alias(
                        integrity, previous, member,
                        read_bounded(stream.extractfile(previous), max_bytes=MAX_FILE),
                        read_bounded(stream.extractfile(member), max_bytes=MAX_FILE))):
                    raise ValueError("duplicate archive member")
                merged_alias = True
                continue
            seen[name] = member
            roots.add(parts[0])
        for name, member in seen.items():
            for parent in PurePosixPath(name).parents:
                if str(parent) in seen and not seen[str(parent)].isdir():
                    raise ValueError("archive file/directory collision")
        if len(roots) != 1:
            raise ValueError("archive must have one root")
        root = next(iter(roots))
        if root in seen and not seen[root].isdir():
            raise ValueError("archive root must be a directory")
        manifest_member = seen.get(root + "/package.json")
        manifest = None
        if manifest_member is not None:
            if not manifest_member.isfile():
                raise ValueError("archive manifest must be a regular file")
            manifest = load_json_bytes(read_bounded(stream.extractfile(manifest_member), max_bytes=MAX_JSON),
                                       max_bytes=MAX_JSON)
            if not isinstance(manifest, dict):
                raise ValueError("archive manifest must be an object")
        if integrity in NPM_ALIAS_ARCHIVES and (not merged_alias or manifest is None or
                (manifest.get("name"), manifest.get("version")) != NPM_ALIAS_ARCHIVES[integrity]):
            raise ValueError("pinned npm archive alias identity mismatch")
        return root, seen, manifest


def verify_archive(path, integrity, *, algorithm="sha512", expected_root=None,
                   max_bytes=MAX_ARCHIVE, max_expanded=MAX_EXPANDED, max_members=MAX_MEMBERS):
    """Authenticate raw registry bytes before bounded safe tar inspection.

    SHA-256 and an exact name-version root also support Cargo registry materials.
    No extraction, trust in restored trees, or checksum rewriting occurs here.
    Raise ValueError for invalid identities, unsafe members or exceeded bounds.
    """
    data = _authenticated_bytes(path, integrity, algorithm, max_bytes)
    root, members, manifest = _archive_catalog(data, max_expanded, max_members, integrity)
    if expected_root is not None and root != expected_root:
        raise ValueError("archive root mismatch")
    return VerifiedArchive(Path(path).absolute(), integrity, algorithm, root,
                           tuple(sorted(name[len(root) + 1:] for name in members if name != root)), manifest)


def materialize_archive(checked, destination):
    """Extract authenticated bytes into a new tree, rejecting stale destinations."""
    destination = Path(destination)
    if os.path.lexists(destination):
        raise ValueError("archive destination must be fresh")
    data = _authenticated_bytes(checked.path, checked.integrity, checked.algorithm, MAX_ARCHIVE)
    root, catalog, _ = _archive_catalog(data, MAX_EXPANDED, MAX_MEMBERS, checked.integrity)
    if root != checked.root:
        raise ValueError("archive root changed")
    destination.mkdir(parents=True)
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as stream:
        for name, member in sorted(catalog.items()):
            if name == root:
                continue
            path = destination / name[len(root) + 1:]
            if member.isdir():
                path.mkdir(parents=True, exist_ok=True)
            else:
                path.parent.mkdir(parents=True, exist_ok=True)
                with path.open("xb") as output:
                    shutil.copyfileobj(stream.extractfile(member), output, 65536)
                path.chmod(0o755 if member.mode & 0o111 else 0o644)


def verify_git_revision(repo, revision, expected_tree=None):
    """Validate exact Git revision/tree with no replacement/configuration channels."""
    preflight._commit_id(revision)
    repo = Path(repo).resolve()
    configuration = preflight._git(repo, ["config", "--local", "--no-includes", "--name-only", "--list"])
    for key in configuration.decode("utf-8").splitlines():
        if key.startswith(("include.", "includeif.", "filter.", "extensions.", "credential.")) or key in {
            "core.worktree", "core.fsmonitor", "core.attributesfile", "core.sshcommand", "core.hookspath",
        }:
            raise ValueError("unreviewed Git configuration channel")
    if preflight._git(repo, ["for-each-ref", "--format=%(refname)", "refs/replace/"]).strip():
        raise ValueError("Git replacement references are forbidden")
    directory = Path(preflight._git(repo, ["rev-parse", "--absolute-git-dir"]).decode().strip())
    for name in ("objects/info/alternates", "info/grafts", "shallow"):
        if os.path.lexists(directory / name):
            raise ValueError("Git alternates, grafts and shallow revisions are forbidden")
    if preflight._git(repo, ["cat-file", "-t", revision]).strip() != b"commit":
        raise ValueError("Git revision must be an actual commit")
    tree = preflight._git(repo, ["rev-parse", revision + "^{tree}"]).decode().strip()
    if expected_tree is not None and expected_tree != tree:
        raise ValueError("Git revision tree mismatch")
    return tree


def materialize_source(repo, revision, destination):
    """Export tracked object bytes/modes, with a sanitized private Git object store.

    Ignore archive attributes, index flags, ambient files and ambient Git metadata.
    Only the fixed CLAUDE.md -> AGENTS.md link is allowed. Raise ValueError for
    unsupported modes, unsafe paths, stale destinations or object disagreement.
    """
    destination = Path(destination)
    if os.path.lexists(destination):
        raise ValueError("source destination must be fresh")
    tree = verify_git_revision(repo, revision)
    entries = []
    for raw in preflight._git(repo, ["ls-tree", "-rtz", "--full-tree", revision]).split(b"\0"):
        if not raw:
            continue
        identity, filename = raw.split(b"\t", 1)
        mode, kind, oid = identity.decode("ascii").split()
        name = filename.decode("utf-8")
        if (kind not in {"blob", "tree"} or mode not in {"040000", "100644", "100755", "120000"}
                or name.startswith("/") or "\\" in name or any(part in {"", ".", "..", ".git"}
                for part in name.split("/"))):
            raise ValueError("unsupported source tree entry")
        entries.append((mode, kind, oid, name))
    objects = {revision: "commit", tree: "tree"}
    objects.update({oid: kind for _, kind, oid, _ in entries})
    blobs = preflight.read_git_objects(repo, objects)
    for mode, _, oid, name in entries:
        if mode == "120000" and (name != "CLAUDE.md" or blobs[oid] != b"AGENTS.md"
                                 or not any(entry[3] == "AGENTS.md" and entry[1] == "blob" for entry in entries)):
            raise ValueError("unsupported source link")
    destination.mkdir(parents=True)
    preflight._git(destination, ["init", "--template="])
    for oid, kind in objects.items():
        actual = preflight._git(destination, ["hash-object", "-w", "-t", kind, "--stdin"], blobs[oid])
        if actual.strip().decode("ascii") != oid:
            raise ValueError("private Git object disagreement")
    for mode, kind, oid, name in entries:
        if kind == "tree":
            continue
        path = destination / name
        path.parent.mkdir(parents=True, exist_ok=True)
        if mode == "120000":
            path.symlink_to("AGENTS.md")
        else:
            path.write_bytes(blobs[oid])
            path.chmod(0o755 if mode == "100755" else 0o644)
    preflight._git(destination, ["update-ref", "HEAD", revision])
    return source_check(destination, revision)


def source_check(repo, revision, before=None):
    """Check all tracked sources plus unexpected tests, data and configuration."""
    snapshot = preflight.source_snapshot(Path(repo), revision, {})
    expected = set(snapshot["files"])
    for directory, children, files in os.walk(repo, followlinks=False):
        directory = Path(directory)
        if directory == Path(repo):
            children[:] = [name for name in children if name != ".git"]
        if directory == Path(repo) / LIB:
            children[:] = [name for name in children if name != "node_modules"]
        for name in [*children, *files]:
            path = directory / name
            if path.is_file() or path.is_symlink():
                if str(path.relative_to(repo)) not in expected:
                    raise ValueError("unexpected source file")
    if before is not None and snapshot != before:
        raise ValueError("source changed during release execution")
    return snapshot


def review_lifecycle(manifest, *, root=False, paths=()):
    """Reject unreviewed commands and implicit native installs before npm runs."""
    scripts = manifest.get("scripts", {})
    if not isinstance(scripts, dict):
        raise ValueError("lifecycle scripts require review")
    if "binding.gyp" in paths and not scripts.get("install") and not scripts.get("preinstall"):
        raise ValueError("unsupported implicit native lifecycle")
    reviewed = {}
    for event in ROOT_HOOKS if root else HOOKS:
        command = scripts.get(event)
        if command is None:
            continue
        if (root or not isinstance(command, str) or not command or
                REVIEWED_HOOKS.get((manifest.get("name"), manifest.get("version"), event)) != command):
            raise ValueError("lifecycle command requires explicit review")
        reviewed[event] = command
    return reviewed


def required_npm_materials(lock, platform, architecture):
    """Select current-platform optional packages with mandatory native tools."""
    if platform not in {"darwin", "linux"} or architecture not in {"arm64", "x64"}:
        raise ValueError("unsupported native platform")
    if lock.get("lockfileVersion") != 3 or not isinstance(lock.get("packages"), dict):
        raise ValueError("expected npm lock v3")
    native = {f"node_modules/@esbuild/{platform}-{architecture}",
              f"node_modules/@rollup/rollup-{platform}-{architecture}" + ("-gnu" if platform == "linux" else "")}
    if not native.issubset(lock["packages"]):
        raise ValueError("required native tools missing from lock")
    selected = {}
    for location, record in lock["packages"].items():
        if not location:
            continue
        if not re.fullmatch(r"node_modules/(?:@[A-Za-z0-9._-]+/)?[A-Za-z0-9._-]+"
                            r"(?:/node_modules/(?:@[A-Za-z0-9._-]+/)?[A-Za-z0-9._-]+)*", location):
            raise ValueError("unsupported npm lock path")
        url = urlsplit(record.get("resolved", ""))
        if (record.get("link") is not None or url.scheme != "https" or url.netloc != "registry.npmjs.org"
                or url.query or url.fragment or not url.path.endswith(".tgz")):
            raise ValueError("unsupported npm registry source")
        for selector, wanted in (("os", platform), ("cpu", architecture), ("libc", "glibc")):
            values = record.get(selector, [])
            if not isinstance(values, list) or any(not isinstance(value, str) for value in values):
                raise ValueError("unsupported npm platform selector")
            positive = [value for value in values if not value.startswith("!")]
            if "!" + wanted in values or (positive and wanted not in positive):
                if not record.get("optional"):
                    raise ValueError("required npm package has incompatible platform")
                break
        else:
            selected[location] = record
    if not native.issubset(selected):
        raise ValueError("required native archives cannot be omitted")
    return selected


class LocalMacBoundary:
    """Actual local macOS denial, explicitly unqualified for hosted releases."""

    def __init__(self, private_root, readonly):
        if sys.platform != "darwin":
            raise ValueError("local fixture boundary requires macOS; hosted qualification pending")
        self.private_root = Path(private_root).resolve(strict=True)
        self.readonly = [Path(path).resolve(strict=True) for path in readonly]
        if any(self.private_root.is_relative_to(path) or path.is_relative_to(self.private_root)
               for path in self.readonly):
            raise ValueError("readonly tool paths must be disjoint from private workspace")

    def command(self, command):
        quote = lambda path: deterministic_json_bytes(str(path)).decode().strip()
        profile = ["(version 1)", "(deny default)", "(allow process*)", "(allow signal)",
                   "(allow sysctl-read)",
                   f"(allow file-read* file-write* (subpath {quote(self.private_root)}))",
                   '(allow file-read* (literal "/"))',
                   '(allow file-read* (literal "/private/var/select/sh"))',
                   '(allow file-read* file-write* (literal "/dev/null"))',
                   '(allow file-read* (literal "/dev/urandom") (literal "/dev/random"))']
        profile.extend(f"(allow file-read* (subpath {quote(path)}))" for path in self.readonly)
        ancestors = {parent for path in [self.private_root, *self.readonly] for parent in path.parents}
        profile.extend(f"(allow file-read-metadata (literal {quote(path)}))" for path in sorted(ancestors))
        # Network, other private workspaces and shared writes remain denied.
        return ["/usr/bin/sandbox-exec", "-p", "".join(profile), *map(str, command)]


def checked_stage(repo, revision, command, environment, log, *, boundary=None, cwd=None):
    """Run a real child and source checks before/after, preserving private logs."""
    before = source_check(repo, revision)
    with Path(log).open("xb") as output:
        result = subprocess.run(boundary.command(command) if boundary else command,
                                env=environment, cwd=cwd or repo, stdout=output,
                                stderr=subprocess.STDOUT, timeout=900, check=False)
    source_check(repo, revision, before)
    if result.returncode:
        raise ValueError(f"release stage failed ({result.returncode}); inspect private log {log}")


def js_commands(workspace, node):
    """Return mandatory local collector and Vitest commands, without npx fallback."""
    workspace = Path(workspace)
    repo = workspace / "source"
    return [
        [str(node), str(repo / "scripts/release/collect-js.mjs"), "--out-dir", str(workspace / "bundles"),
         "--inventory", str(workspace / "inventory.json"), "--preparation", str(workspace / "preparation.json")],
        [str(node), str(repo / LIB / "node_modules/vitest/vitest.mjs"), "run"],
    ]


def local_environment(workspace, node, revision):
    """Construct fresh local fixture environment; no qualified acquisition claim."""
    workspace = Path(workspace).resolve()
    node = Path(node).resolve(strict=True)
    preflight._commit_id(revision)
    for name in ("home", "tmp"):
        (workspace / name).mkdir(exist_ok=True)
    developer_git = [directory for directory in (Path("/Applications/Xcode.app/Contents/Developer/usr/bin"),
                                               Path("/Library/Developer/CommandLineTools/usr/bin"))
                     if (directory / "git").is_file()]
    tool_paths = [str(node.parent), *(str(path) for path in developer_git), "/usr/bin", "/bin"]
    return {
        "HOME": str(workspace / "home"), "TMPDIR": str(workspace / "tmp"),
        "PATH": os.pathsep.join(tool_paths),
        "LANG": "C", "LC_ALL": "C", "TZ": "UTC", "CI": "true",
        "NODE_ENV": "production", "TRUSTED_SERVER__RELEASE_BUILD": "1",
        "TRUSTED_SERVER__GIT_VERSION": revision,
        "SOURCE_DATE_EPOCH": str(source_check(workspace / "source", revision)["source_date_epoch"]),
        "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_NO_REPLACE_OBJECTS": "1", "GIT_OPTIONAL_LOCKS": "0",
    }


def _npm_command(node):
    return [str(node), str(Path(node).parent.parent / "lib/node_modules/npm/bin/npm-cli.js")]


def _check_local_versions(node, environment):
    for command, expected in (([str(node), "--version"], "v24.12.0"),
                              (_npm_command(node) + ["--version"], "11.6.2")):
        result = subprocess.run(command, env=environment, capture_output=True, timeout=30, check=False)
        if result.returncode or result.stdout.decode("utf-8").strip() != expected:
            raise ValueError("actual Node/npm version differs from pinned recipe")


def _owned_files(package, excluded):
    files = {}
    for directory, children, names in os.walk(package, followlinks=False):
        directory = Path(directory)
        children[:] = [name for name in children if directory / name not in excluded]
        for name in children + names:
            path = directory / name
            if path.is_symlink():
                raise ValueError("unsupported lifecycle symlink")
            if path.is_file():
                data = _regular_bytes(path, MAX_FILE)
                files[str(path.relative_to(package))] = {
                    "size": len(data), "sha256": sha256_bytes(data),
                    "executable": bool(path.stat().st_mode & 0o111),
                }
    return files


def lifecycle_shell(arguments, node):
    """Recipe-owned npm script shell observes actual invocation and completion.

    Npm's foreground option serializes dependency hooks. Failed/incomplete hooks
    are retained and rejected even when npm swallows an optional package failure.
    """
    if len(arguments) != 2 or arguments[0] != "-c":
        raise ValueError("unexpected npm lifecycle shell arguments")
    command = arguments[1]
    package = Path.cwd().resolve()
    # The generated script shell supplies only this fixed private workspace path.
    workspace = Path(os.environ["TRUSTED_RELEASE_PRIVATE_WORKSPACE"]).resolve()
    install_root = workspace / "source" / LIB
    review = read_json_file(workspace / "reviewed-lifecycle.json", max_bytes=MAX_JSON)
    location = str(package.relative_to(install_root))
    event = os.environ.get("npm_lifecycle_event")
    if (location not in review or event not in HOOKS or review[location].get(event) != command
            or os.environ.get("npm_lifecycle_script") != command):
        raise ValueError("unreviewed actual lifecycle invocation")
    manifest = read_json_file(package / "package.json", max_bytes=MAX_JSON)
    if review_lifecycle(manifest).get(event) != command:
        raise ValueError("lifecycle source script changed")
    lock = read_json_file(install_root / "package-lock.json", max_bytes=MAX_JSON)
    excluded = {install_root / name for name in lock["packages"] if name and name != location}
    before = _owned_files(package, excluded)
    identity = f"lifecycle:{location}:{event}"
    record_path = workspace / "lifecycle" / (sha256_bytes(identity.encode()) + ".json")
    record = {"id": identity, "lock_path": location, "event": event,
              "script_sha256": sha256_bytes(command.encode()), "state": "started"}
    with record_path.open("xb") as output:
        output.write(deterministic_json_bytes(record))
    # npm prepends dependency .bin directories; remove tool-shadowing search paths.
    child_environment = {**os.environ, "PATH": os.pathsep.join((str(Path(node).parent), "/usr/bin", "/bin"))}
    result = subprocess.run(["/bin/sh", "-c", command], env=child_environment, check=False, timeout=120)
    after = _owned_files(package, excluded)
    if before.keys() - after.keys():
        raise ValueError("lifecycle deletion requires reviewed output representation")
    record.update({"state": "completed", "returncode": result.returncode,
                   "outputs": [{"path": location + "/" + name, **facts}
                               for name, facts in sorted(after.items()) if before.get(name) != facts]})
    record_path.write_bytes(deterministic_json_bytes(record))
    return result.returncode


def observed_lifecycle(workspace):
    """Convert successful actual execution observations to the Task 6 joins."""
    workspace = Path(workspace)
    events, outputs = [], []
    for path in sorted((workspace / "lifecycle").glob("*.json")):
        record = read_json_file(path, max_bytes=MAX_JSON)
        if record.get("state") != "completed" or record.get("returncode") != 0:
            raise ValueError("lifecycle execution failed or is incomplete")
        event = {"kind": "lifecycle-event", "id": record["id"],
                 "package_material_id": "npm:" + record["lock_path"], "event": record["event"],
                 "script_sha256": record["script_sha256"]}
        events.append(event)
        for facts in record["outputs"]:
            outputs.append({"kind": "lifecycle-output", "id": record["id"] + ":" + facts["path"],
                            "package_material_id": event["package_material_id"], "event": event["event"],
                            "script_sha256": event["script_sha256"], "path": facts["path"],
                            "size": facts["size"], "sha256": facts["sha256"]})
    return events, outputs


def install_npm(workspace, revision, archives, node, environment, *, boundary=None):
    """Seed a fresh private cache through npm, then execute reviewed offline ci.

    No restored installation/cache is accepted. Each selected archive is checked
    again and every lifecycle declaration is reviewed before any installation.
    The caller must supply the selected execution boundary for release-shaped
    local fixtures; the official CLI cannot qualify this helper by itself.
    """
    workspace = Path(workspace).resolve()
    repo = workspace / "source"
    library = repo / LIB
    node = Path(node).resolve(strict=True)
    if any(os.path.lexists(path) for path in (library / "node_modules", workspace / "cache",
                                             workspace / "lifecycle", workspace / "script-shell")):
        raise ValueError("npm installation and cache must be fresh")
    _check_local_versions(node, environment)
    review_lifecycle(read_json_file(library / "package.json", max_bytes=MAX_JSON), root=True)
    if os.path.lexists(library / ".npmrc"):
        raise ValueError("unreviewed source npm configuration")
    lock = read_json_file(library / "package-lock.json", max_bytes=MAX_JSON)
    review = {}
    for location, checked in archives.items():
        verified = verify_archive(checked.path, checked.integrity)
        record = lock["packages"].get(location)
        name = location.rsplit("node_modules/", 1)[-1]
        if (not record or record.get("integrity") != verified.integrity or verified.manifest is None
                or verified.manifest.get("name") != name or verified.manifest.get("version") != record.get("version")):
            raise ValueError("authenticated npm archive/lock identity mismatch")
        review[location] = review_lifecycle(verified.manifest, paths=verified.paths)
    (workspace / "reviewed-lifecycle.json").write_bytes(deterministic_json_bytes(review))
    (workspace / "cache").mkdir()
    (workspace / "lifecycle").mkdir()
    for name in ("npm-user.conf", "npm-global.conf"):
        (workspace / name).write_bytes(b"")
    wrapper = (f"#!{sys.executable} -I\n"
               "import sys\n"
               "sys.dont_write_bytecode = True\n"
               f"sys.path.insert(0, {str(repo / 'scripts/release')!r})\n"
               "import prepare\n"
               f"sys.exit(prepare.lifecycle_shell(sys.argv[1:], {str(node)!r}))\n").encode()
    (workspace / "script-shell").write_bytes(wrapper)
    (workspace / "script-shell").chmod(0o755)
    environment = {**environment, "TRUSTED_RELEASE_PRIVATE_WORKSPACE": str(workspace)}
    base = _npm_command(node) + ["--cache", str(workspace / "cache"), "--userconfig",
                                str(workspace / "npm-user.conf"), "--globalconfig",
                                str(workspace / "npm-global.conf"), "--offline", "--no-audit", "--no-fund"]
    unique = sorted({str(checked.path) for checked in archives.values()})
    for index in range(0, len(unique), 32):
        checked_stage(repo, revision, base + ["cache", "add", "--ignore-scripts", *unique[index:index + 32]],
                      environment, workspace / f"cache-{index // 32}.log", boundary=boundary, cwd=library)
    checked_stage(repo, revision, base + ["ci", "--include=dev", "--foreground-scripts",
                                         "--script-shell", str(workspace / "script-shell")], environment,
                  workspace / "install.log", boundary=boundary, cwd=library)
    # Optional hook failures must not be hidden by npm's success exit status.
    events, _ = observed_lifecycle(workspace)
    installed = {location for location in review if (library / location).is_dir()}
    expected = {f"lifecycle:{location}:{event}" for location in installed for event in review[location]}
    if {event["id"] for event in events} != expected:
        raise ValueError("missing or unexpected actual lifecycle observations")
    if (workspace / "script-shell").read_bytes() != wrapper:
        raise ValueError("recipe-owned lifecycle shell changed")


# Reuse Task 6's tree and executable identity measurements, not a second hash dialect.
MEASURE_JS = """
import fs from 'node:fs';
import path from 'node:path';
import {pathToFileURL} from 'node:url';
const [root, output, observations] = process.argv.slice(1);
const {installedOrigins, installedMaterials, actualToolFiles} = await import(
  pathToFileURL(path.join(root, 'crates/trusted-server-js/lib/release-composition.mjs')));
const installRoot = path.join(root, 'crates/trusted-server-js/lib');
const owners = installedOrigins({repoRoot: root, lockFile: path.join(installRoot, 'package-lock.json')});
const observed = JSON.parse(fs.readFileSync(observations, 'utf8'));
const actual = actualToolFiles({installRoot});
const distributions = actual.map(({name, version, sha256, path: file}) => ({
  material: {kind:'tool-distribution', id:'tool:'+name, name, version, sha256}, path:file}));
const tools = actual.map(({name, version, role}) => ({name, version, role, material_id:'tool:'+name}));
fs.writeFileSync(output, JSON.stringify({schema_version:1, packages:installedMaterials(owners),
  distributions, tools, lifecycle_events:observed.events, lifecycle_outputs:observed.outputs})+'\\n', {flag:'wx'});
"""


def preparation_document(workspace, revision, node, environment, *, boundary):
    """Measure installed packages and actual tools after genuine preparation."""
    workspace = Path(workspace)
    events, outputs = observed_lifecycle(workspace)
    observations = workspace / "observations.json"
    observations.write_bytes(deterministic_json_bytes({"events": events, "outputs": outputs}))
    destination = workspace / "preparation.json"
    repo = workspace / "source"
    checked_stage(repo, revision, [str(node), "--input-type=module", "-e", MEASURE_JS,
                                  str(repo), str(destination), str(observations)], environment,
                  workspace / "measurement.log", boundary=boundary, cwd=repo / LIB)
    prepared = read_json_file(destination, max_bytes=MAX_JSON)
    versions = {"node": "24.12.0", "npm": "11.6.2", "vite": "7.3.1", "rollup": "4.57.1",
                "esbuild": "0.27.3", "vitest": "4.0.18"}
    for tool in prepared["tools"]:
        if tool["version"] != versions[tool["name"].split(":")[0]]:
            raise ValueError("actual JS tool version differs from reviewed pins")
    shell_version = subprocess.run(["/bin/sh", "--version"], env=environment, capture_output=True,
                                   timeout=30, check=True).stdout.decode("utf-8").splitlines()[0]
    git_file = Path(shutil.which("git", path=environment["PATH"])).resolve(strict=True)
    git_version = subprocess.run([str(git_file), "--version"], env=environment, capture_output=True,
                                 timeout=30, check=True).stdout.decode("utf-8").strip()
    for name, file, version in (("python", Path(sys.executable).resolve(),
                                ".".join(str(part) for part in sys.version_info[:3])),
                               ("shell", Path("/bin/sh").resolve(), shell_version),
                               ("git", git_file, git_version)):
        material = {"kind": "tool-distribution", "id": "tool:" + name, "name": name,
                    "version": version, "sha256": sha256_bytes(_regular_bytes(file, MAX_ARCHIVE))}
        prepared["distributions"].append({"material": material, "path": str(file)})
        prepared["tools"].append({"name": name, "version": version, "role": "runtime", "material_id": material["id"]})
    destination.write_bytes(deterministic_json_bytes(prepared))
    return prepared


def local_boundary(workspace, node, archive_pool):
    """Select explicit read-only runtime/archive paths for a local denial fixture."""
    developer = [path for path in (Path("/Applications/Xcode.app/Contents/Developer"),
                                  Path("/Library/Developer/CommandLineTools")) if path.is_dir()]
    return LocalMacBoundary(workspace, [Path("/bin"), Path("/usr/bin"), Path("/usr/lib"), Path("/System"),
                                       Path(node).resolve().parent.parent, Path(sys.base_prefix).resolve(),
                                       Path(archive_pool).resolve(), *developer])


def _expected_modules(repo, revision):
    names = preflight._git(repo, ["ls-tree", "-rz", "--name-only", revision]).decode("utf-8").split("\0")
    prefix = str(LIB) + "/src/"
    expected = []
    for name in names:
        if name == prefix + "core/index.ts":
            expected.append("core")
        elif re.fullmatch(re.escape(prefix) + r"integrations/[a-z][a-z0-9_-]*/index\.ts", name):
            expected.append(name.split("/")[-2])
    if "core" not in expected or len(expected) != len(set(expected)):
        raise ValueError("invalid tracked JS entry points")
    return sorted(expected)


def _build_js_pair(repo, revision, archive_pool, work, output, node, boundary_factory):
    preflight._commit_id(revision)
    work = Path(work).resolve()
    if os.path.lexists(work) or os.path.lexists(output):
        raise ValueError("JS work and publication directories must be fresh")
    archive_pool = Path(archive_pool).resolve(strict=True)
    node = Path(node).resolve(strict=True)
    if work.is_relative_to(archive_pool) or archive_pool.is_relative_to(work):
        raise ValueError("shared archives must be outside private workspaces")
    work.mkdir(parents=True)
    expected = _expected_modules(repo, revision)
    for label in ("A", "B"):
        workspace = work / label
        workspace.mkdir()
        materialize_source(repo, revision, workspace / "source")
        environment = local_environment(workspace, node, revision)
        boundary = boundary_factory(workspace, node, archive_pool)
        lock = read_json_file(workspace / "source" / LIB / "package-lock.json", max_bytes=MAX_JSON)
        native = subprocess.run([str(node), "-e", "console.log(JSON.stringify([process.platform,process.arch]))"],
                                env=environment, capture_output=True, timeout=30, check=True)
        platform, architecture = load_json_bytes(native.stdout, max_bytes=MAX_JSON)
        selected = required_npm_materials(lock, platform, architecture)
        archives = {}
        for location, record in selected.items():
            integrity = record.get("integrity")
            if not isinstance(integrity, str) or not re.fullmatch(r"sha512-[A-Za-z0-9+/]{86}==", integrity):
                raise ValueError("missing locked npm archive integrity")
            digest = base64.b64decode(integrity[7:], validate=True).hex()
            archives[location] = verify_archive(archive_pool / (digest + ".tgz"), integrity)
        install_npm(workspace, revision, archives, node, environment, boundary=boundary)
        preparation_document(workspace, revision, node, environment, boundary=boundary)
        for command, log in zip(js_commands(workspace, node), ("build.log", "tests.log")):
            checked_stage(workspace / "source", revision, command, environment, workspace / log,
                          boundary=boundary, cwd=workspace / "source" / LIB)
    compare_and_publish(work / "A", work / "B", expected, output)
    return {"source_commit": revision, "modules": expected, "output": str(output)}


def local_js_pair(repo, revision, archive_pool, work, output, node):
    """Prepare/build/test/compare actual A/B under local denial, never official.

    Archive pool files use their locked SHA-512 hexadecimal digest plus .tgz.
    This API intentionally has no official CLI bypass. Task 8 must qualify
    authenticated acquisitions and hosted boundary selection before official use.
    """
    result = _build_js_pair(repo, revision, archive_pool, work, output, node, local_boundary)
    return {"qualification": "local-unqualified", **result}


def qualified_js_selection(tools):
    """Fail closed until Task 8 supplies source-owned authenticated path selection."""
    preflight.validate_tools(tools, require_qualified=True)
    raise ValueError("release qualification pending: authenticated hosted JS paths/boundary required")


def compare_and_publish(a, b, expected, output):
    """Publish only A's final complete bundles and inventory after byte equality."""
    expected = sorted(f"tsjs-{name}.js" for name in expected)
    if not expected or len(expected) != len(set(expected)) or any(
            not re.fullmatch(r"tsjs-[a-z][a-z0-9_-]*\.js", name) for name in expected):
        raise ValueError("invalid expected final module set")
    measured = []
    for workspace in (a, b):
        directory = Path(workspace) / "bundles"
        if directory.is_symlink() or sorted(path.name for path in directory.iterdir()) != expected:
            raise ValueError("final bundle set mismatch")
        files = {name: _regular_bytes(directory / name, MAX_FILE) for name in expected}
        if any(not data for data in files.values()):
            raise ValueError("empty final bundle")
        measured.append(files)
    if measured[0] != measured[1]:
        raise ValueError("independent final JS bytes differ")
    inventory = _regular_bytes(Path(a) / "inventory.json", MAX_JSON)
    document = load_json_bytes(inventory, max_bytes=MAX_JSON)
    if (not isinstance(document, dict) or set(document) != {
            "schema_version", "source_commit", "modules", "tools", "materials"}
            or type(document["schema_version"]) is not int or document["schema_version"] != 1
            or not isinstance(document["source_commit"], str)
            or not re.fullmatch(r"[0-9a-f]{40}", document["source_commit"])
            or any(not isinstance(document[field], list) for field in ("modules", "tools", "materials"))):
        raise ValueError("invalid final inventory root")
    modules = document["modules"]
    for record in modules:
        if (not isinstance(record, dict) or set(record) != {
                "id", "filename", "size", "sha256", "contributions"}
                or not isinstance(record["filename"], str) or record["filename"] not in measured[0]
                or record["id"] != record["filename"][5:-3]
                or not isinstance(record["contributions"], list)):
            raise ValueError("invalid final inventory module")
        try:
            size = require_nonnegative_integer(record["size"])
        except ValueError as error:
            raise ValueError("invalid final inventory module size") from error
        if (size != len(measured[0][record["filename"]])
                or record["sha256"] != sha256_bytes(measured[0][record["filename"]])):
            raise ValueError("final inventory does not match published A bytes")
    if sorted(record["filename"] for record in modules) != expected:
        raise ValueError("final inventory does not match published A bytes")
    output = Path(output)
    if os.path.lexists(output):
        raise ValueError("publication destination must be fresh")
    temporary = output.with_name(output.name + ".pending")
    temporary.mkdir()
    try:
        (temporary / "bundles").mkdir()
        for name, data in measured[0].items():
            (temporary / "bundles" / name).write_bytes(data)
        (temporary / "tsjs-inventory.json").write_bytes(inventory)
        temporary.rename(output)
    finally:
        if temporary.exists():
            shutil.rmtree(temporary)


def main():
    """Official entry: pending acquisition/boundary qualification always refuses."""
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("source", "commit", "archives", "work", "output"):
        parser.add_argument("--" + name, required=True)
    args = parser.parse_args()
    preflight.audit_environment(os.environ)
    tools = read_json_file(Path(__file__).with_name("release-tools.json"), max_bytes=MAX_JSON)
    node, boundary_factory = qualified_js_selection(tools)
    _build_js_pair(Path(args.source), args.commit, Path(args.archives), Path(args.work), Path(args.output),
                   node, boundary_factory)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.SubprocessError, tarfile.TarError) as error:
        logging.error("Release JS preparation refuses: %s", error)
        sys.exit(1)
