"""Closed canonical Rust recipe shared by production and private qualification.

Selectors remain closed until independently authenticated source-owned candidate
acquisitions exist. A candidate build never grants consumer or publishing trust.
"""
import argparse
import copy
from contextlib import ExitStack
import importlib.util
import hashlib
import logging
import os
import re
import selectors
import shutil
import shlex
import signal
import stat
import subprocess
import sys
import time
import tomllib
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import deterministic_json_bytes, load_json_bytes, read_json_file, sha256_bytes
import preflight
import prepare

RELEASE = Path(__file__).resolve().parent
ROOT = RELEASE.parents[1]
sys.path.insert(0, str(RELEASE / "qualification"))
_BOUNDARY_SOURCE = RELEASE / "qualification/boundaries.py"
_BOUNDARY_MODULE = sys.modules.get("boundaries")
if _BOUNDARY_MODULE is None:
    _boundary_spec = importlib.util.spec_from_file_location("boundaries", _BOUNDARY_SOURCE)
    if _boundary_spec is None or _boundary_spec.loader is None:
        raise ValueError("source-owned qualification boundary module unavailable")
    _BOUNDARY_MODULE = importlib.util.module_from_spec(_boundary_spec)
    sys.modules["boundaries"] = _BOUNDARY_MODULE
    try:
        _boundary_spec.loader.exec_module(_BOUNDARY_MODULE)
    except BaseException:
        sys.modules.pop("boundaries", None)
        raise
if Path(getattr(_BOUNDARY_MODULE, "__file__", "")).resolve() != _BOUNDARY_SOURCE:
    raise ValueError("source-owned qualification boundary module mismatch")
DOCKER_EXECUTABLE = _BOUNDARY_MODULE.DOCKER_EXECUTABLE
LinuxBoundary = _BOUNDARY_MODULE.LinuxBoundary
MacBoundary = _BOUNDARY_MODULE.MacBoundary
probe_controls = _BOUNDARY_MODULE.probe_controls
MAX_CAPTURE = 134217728
PROFILES = preflight.ambient_profiles()
CARGO_REGISTRY_SOURCE = "registry+https://github.com/rust-lang/crates.io-index"
CARGO_REGISTRY_CACHE_NAME = "index.crates.io-1949cf8c6b5b557f"
MAX_TOOL_FILE = 536870912
MAX_TOOL_TREE = 8589934592
MAX_TOOL_MEMBERS = 250000
EXECUTING_HELPERS = ("common.py", "preflight.py", "prepare.py", "rust_recipe.py", "inspect-native.py", "validate-fastly.py",
                     "qualification/boundaries.py", "qualification/qualify.py", "qualification/socket_probe.py")
EXECUTING_HELPER_HASHES = {name: sha256_bytes(prepare._regular_bytes(RELEASE / name, 33554432)) for name in EXECUTING_HELPERS}


def ambient_profile(role):
    """Return a copy of reviewed role policy; callers cannot choose its values."""
    if role not in PROFILES:
        raise ValueError("unknown build role")
    return copy.deepcopy(PROFILES[role])


def artifact_selection(name):
    """Select only the fixed cohort's default package/bin/target/features."""
    for artifact in preflight.fixed_artifacts():
        if artifact["name"] == name:
            return artifact
    raise ValueError("unknown fixed release asset")


def parse_request(arguments=None):
    """Parse the seven closed fields without executable/config/profile inputs."""
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    parser.add_argument("--asset", required=True)
    parser.add_argument("--role", required=True, choices=("A", "B"))
    parser.add_argument("--source-commit", required=True)
    for name in ("js-dir", "js-inventory", "materials", "output"):
        parser.add_argument("--" + name, required=True, type=Path)
    args = parser.parse_args(arguments)
    artifact_selection(args.asset)
    preflight._commit_id(args.source_commit)
    for name in ("js_dir", "js_inventory", "materials", "output"):
        path = getattr(args, name)
        if not path.is_absolute() or ".." in path.parts:
            raise ValueError("absolute normalized request paths required")
    return args


def fresh_root(path):
    """Create private generated state only at a fresh real absolute pathname."""
    path = Path(path)
    if any(character.isspace() or ord(character) < 32 or character in ",:" for character in str(path)):
        raise ValueError("recipe path cannot inject compiler/mount arguments")
    if not path.is_absolute() or os.path.lexists(path) or path.parent.resolve(strict=True) != path.parent:
        raise ValueError("workspace/output root must be fresh and absolute")
    path.mkdir(mode=0o700)
    path.chmod(0o700)
    return path


def tree_digest(root):
    """Measure bounded regular immutable material trees, rejecting escaped links."""
    root = Path(root).resolve(strict=True)
    if not root.is_dir():
        raise ValueError("material tree must be a directory")
    records, total = [], 0
    for path in sorted(root.rglob("*")):
        information = path.lstat()
        name = str(path.relative_to(root))
        if len(records) >= 100000 or stat.S_ISLNK(information.st_mode):
            raise ValueError("material tree links/member bound forbidden")
        if stat.S_ISREG(information.st_mode):
            data = prepare._regular_bytes(path, prepare.MAX_ARCHIVE)
            total += len(data)
            if total > 2147483648:
                raise ValueError("material tree byte bound exceeded")
            records.append({"path": name, "mode": stat.S_IMODE(information.st_mode), "sha256": sha256_bytes(data)})
        elif stat.S_ISDIR(information.st_mode):
            records.append({"path": name, "mode": stat.S_IMODE(information.st_mode), "directory": True})
        else:
            raise ValueError("unsupported material tree file")
    return sha256_bytes(deterministic_json_bytes(records))



def tool_file_sha256(path):
    """Stream an authenticated regular tool file through one held descriptor."""
    path = Path(path)
    identity = lambda value: (value.st_dev, value.st_ino, value.st_mode, value.st_nlink, value.st_size, value.st_mtime_ns, value.st_ctime_ns)
    with os.fdopen(os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW), "rb") as stream:
        before = os.fstat(stream.fileno())
        if not stat.S_ISREG(before.st_mode) or before.st_size > MAX_TOOL_FILE or identity(before) != identity(path.lstat()):
            raise ValueError("bounded regular authenticated tool file required")
        digest, total = hashlib.sha256(), 0
        deadline = time.monotonic() + 120
        while chunk := stream.read(1048576):
            total += len(chunk)
            if total > MAX_TOOL_FILE or time.monotonic() > deadline:
                raise ValueError("tool file hash byte/time bound exceeded")
            digest.update(chunk)
        if total != before.st_size or identity(os.fstat(stream.fileno())) != identity(before) or identity(path.lstat()) != identity(before):
            raise ValueError("tool file changed during held-descriptor measurement")
        return digest.hexdigest()


def _resolve_tool_link(path):
    current, pending, links, components = Path("/"), list(Path(path).parts[1:]), 0, 0
    while pending:
        components += 1
        if components > 8192:
            raise ValueError("tool link resolution component bound exceeded")
        part = pending.pop(0)
        if part in {"", "."}:
            continue
        if part == "..":
            current = current.parent
            continue
        candidate = current / part
        if candidate.is_symlink():
            links += 1
            if links > 64:
                raise ValueError("tool link cycle/resolution bound exceeded")
            raw = os.readlink(candidate)
            if len(os.fsencode(raw)) > 4096:
                raise ValueError("tool link target bound exceeded")
            target = Path(raw)
            if target.is_absolute():
                current = Path("/")
                parts = list(target.parts[1:])
            else:
                parts = list(target.parts)
            pending = parts + pending
        else:
            candidate.lstat()
            current = candidate
    return current


def tool_tree_digest(root, *, approved_roots):
    """Measure tool/SDK aliases without weakening source/vendor material policy.

    Preserve every raw symlink and require bounded final resolution within an
    explicitly authenticated immutable root. Hash regular files by streaming;
    never delete SDK aliases or rewrite vendor distribution contents.
    """
    original = Path(root)
    roots = [Path(path).resolve(strict=True) for path in approved_roots]
    root = original.resolve(strict=True)
    if (not roots or len(roots) > 32 or len(roots) != len(set(roots)) or Path("/") in roots
            or root not in roots or original.is_symlink() or not root.is_dir()):
        raise ValueError("explicit authenticated real tool roots required")
    records, total = [], 0
    deadline = time.monotonic() + 1200
    for path in sorted(root.rglob("*")):
        if len(records) >= MAX_TOOL_MEMBERS or time.monotonic() > deadline:
            raise ValueError("tool tree member/time bound exceeded")
        information = path.lstat()
        record = {"path": str(path.relative_to(root)), "mode": stat.S_IMODE(information.st_mode)}
        if stat.S_ISLNK(information.st_mode):
            raw = os.readlink(path)
            resolved = _resolve_tool_link(path)
            approved = [parent for parent in roots if resolved.is_relative_to(parent)]
            if not approved:
                raise ValueError("tool link escapes authenticated immutable roots")
            if path.lstat() != information or os.readlink(path) != raw:
                raise ValueError("tool link changed during measurement")
            parent = max(approved, key=lambda value: len(value.parts))
            record.update(link=raw, resolved_root=roots.index(parent), resolved_path=str(resolved.relative_to(parent)))
        elif stat.S_ISREG(information.st_mode):
            total += information.st_size
            if total > MAX_TOOL_TREE:
                raise ValueError("tool tree byte bound exceeded")
            record["sha256"] = tool_file_sha256(path)
        elif stat.S_ISDIR(information.st_mode):
            record["directory"] = True
        else:
            raise ValueError("unsupported authenticated tool tree file")
        records.append(record)
    return sha256_bytes(deterministic_json_bytes(records))


def check_tree(root, expected):
    """Reject mutation against a previously measured or authenticated tree."""
    if not isinstance(expected, str) or not re.fullmatch(r"[a-f0-9]{64}", expected) or tree_digest(root) != expected:
        raise ValueError("immutable material tree changed")


class RecipeProcessError(ValueError):
    """Retain bounded process results for expected negative fixture evidence."""

    def __init__(self, code, stdout, stderr):
        super().__init__(f"recipe process failed with exit code {code}")
        self.code, self.stdout, self.stderr = code, stdout, stderr


def capture(argv, environment, cwd, log, *, timeout=7200, maximum=MAX_CAPTURE, umask=0o022, boundary=None):
    """Drain independent bounded stdout/stderr; close/reap and clean OS boundary.

    Fresh log descriptors open before descendants start. Compiler hooks cannot
    redirect parent writes with symlinks. Cargo stdout remains unchanged JSON.
    """
    result = {"stdout": bytearray(), "stderr": bytearray()}
    with ExitStack() as stack:
        logs = {name: stack.enter_context(Path(str(log) + "." + name).open("xb")) for name in result}
        process = subprocess.Popen(list(map(str, argv)), env=environment, cwd=cwd,
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            close_fds=True, start_new_session=True, umask=umask)
        deadline = time.monotonic() + timeout
        poller = selectors.DefaultSelector()
        for name in result:
            poller.register(getattr(process, name), selectors.EVENT_READ, name)
        try:
            while poller.get_map():
                if time.monotonic() >= deadline:
                    raise ValueError("recipe process timeout")
                for key, _ in poller.select(min(0.25, deadline - time.monotonic())):
                    chunk = os.read(key.fileobj.fileno(), 65536)
                    if not chunk:
                        poller.unregister(key.fileobj)
                    else:
                        result[key.data].extend(chunk)
                        if len(result[key.data]) > maximum:
                            raise ValueError("recipe capture bound exceeded")
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise ValueError("recipe process timeout")
            code = process.wait(timeout=remaining)
        finally:
            poller.close()
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
            for name, value in result.items():
                getattr(process, name).close()
                logs[name].write(value)
            if boundary is not None and hasattr(boundary, "cleanup"):
                boundary.cleanup(list(map(str, argv)))
        if code:
            raise RecipeProcessError(code, bytes(result["stdout"]), bytes(result["stderr"]))
    return bytes(result["stdout"]), bytes(result["stderr"])


def cargo_command(cargo, artifact, target, configuration):
    """Construct the sole reviewed compiler invocation from source selection."""
    if artifact != artifact_selection(artifact["name"]):
        raise ValueError("unreviewed artifact selection")
    return [str(cargo), "build", "--package", artifact["package"], "--bin", artifact["bin"],
            "--target", artifact["target"], "--release", "--frozen",
            "--message-format=json-render-diagnostics", "--target-dir", str(target),
            "--config", str(configuration)]


def final_cargo_artifact(stdout, artifact, target_root, *, source_root=None, reader=None):
    """Join exactly one final Cargo event to the source-owned expected output."""
    root = Path(target_root).resolve(strict=True)
    name = artifact["bin"] + (".wasm" if artifact["kind"] == "wasm-core" else "")
    expected = root / artifact["target"] / "release" / name
    events = []
    for line in stdout.splitlines():
        event = load_json_bytes(line, max_bytes=1048576)
        if not isinstance(event, dict):
            raise ValueError("Cargo JSON event must be an object")
        if event.get("reason") != "compiler-artifact" or event.get("executable") is None:
            continue
        target = event.get("target", {})
        package = event.get("package_id", "")
        if (target.get("name") == artifact["bin"] and target.get("kind") == ["bin"]
                and isinstance(package, str) and re.search(r"(?:/|#)" + re.escape(artifact["package"]) + r"(?:#|@)", package)):
            events.append(event)
    if len(events) != 1:
        raise ValueError("exactly one selected final Cargo executable required")
    event = events[0]
    profile = event.get("profile", {})
    if (event.get("features") != artifact["features"] or profile.get("test") is not False
            or profile.get("opt_level") != "3" or type(profile.get("debuginfo")) is not int or profile.get("debuginfo") != 1):
        raise ValueError("final Cargo features/release debug profile mismatch")
    actual = Path(event["executable"])
    if actual != expected:
        raise ValueError("final Cargo output is escaped, unrelated or absent")
    if source_root is not None:
        crate = Path(source_root) / "crates" / artifact["package"]
        if event.get("manifest_path") != str(crate / "Cargo.toml") or not event["package_id"].startswith("path+file://" + str(crate) + "#"):
            raise ValueError("final Cargo output package/source identity mismatch")
    if reader is None:
        if actual.is_symlink() or not actual.is_file() or actual.resolve(strict=True) != expected:
            raise ValueError("final Cargo output is escaped, unrelated or absent")
        data = prepare._regular_bytes(actual, 536870912)
    else:
        data = reader(actual)
    if not isinstance(data, bytes) or not data or len(data) > 536870912:
        raise ValueError("final Cargo output byte observation missing or oversized")
    return actual


def _material_path(root, name):
    if not isinstance(name, str) or not re.fullmatch(r"[a-zA-Z0-9_.-]+(?:/[a-zA-Z0-9_.-]+)*", name) or any(part in {".", ".."} for part in name.split("/")):
        raise ValueError("unsafe Cargo material index path")
    path = Path(root) / name
    if path.is_symlink() or not path.resolve(strict=True).is_relative_to(Path(root).resolve(strict=True)):
        raise ValueError("Cargo material index path escapes pool")
    return path


def verify_cargo_materials(lock_bytes, index, pool):
    """Authenticate original archives/Git pins joined exactly to source Cargo.lock.

    The index is closed data; it never carries executable commands/configuration.
    Git workspace normalization belongs to the selected authenticated Cargo's
    vendor command, not hand-edited dependency manifests.
    """
    if (not isinstance(index, dict) or set(index) != {"schema_version", "registry", "git"}
            or type(index["schema_version"]) is not int or index["schema_version"] != 1
            or not isinstance(index["registry"], list) or not isinstance(index["git"], list)):
        raise ValueError("closed Cargo material index required")
    lock = tomllib.loads(lock_bytes.decode("utf-8"))
    required_registry, required_git = {}, {}
    for package in lock.get("package", []):
        source = package.get("source", "")
        if source.startswith("registry+"):
            key = (package["name"], package["version"], source)
            if key in required_registry:
                raise ValueError("duplicate Cargo lock registry package")
            required_registry[key] = package["checksum"]
        elif source.startswith("git+"):
            if not re.fullmatch(r"git\+https://[^#]+#[a-f0-9]{40}", source):
                raise ValueError("unsupported locked Git source")
            required_git[source] = source.rsplit("#", 1)[1]
        elif source:
            raise ValueError("unsupported locked Cargo source")
    archives, registry_seen, git_seen = {}, set(), set()
    for record in index["registry"]:
        if not isinstance(record, dict) or set(record) != {"name", "version", "source", "checksum", "archive"}:
            raise ValueError("closed Cargo registry index record required")
        key = (record["name"], record["version"], record["source"])
        if key in registry_seen or key not in required_registry or record["checksum"] != required_registry[key]:
            raise ValueError("Cargo material index registry mismatch")
        registry_seen.add(key)
        path = _material_path(pool, record["archive"])
        checked = prepare.verify_archive(path, record["checksum"], algorithm="sha256", expected_root=record["name"] + "-" + record["version"])
        archives[key] = checked
    for record in index["git"]:
        if not isinstance(record, dict) or set(record) != {"source", "commit", "tree", "object_store"}:
            raise ValueError("closed Cargo Git index record required")
        source = record["source"]
        if source in git_seen or source not in required_git or record["commit"] != required_git[source]:
            raise ValueError("Cargo material index Git revision mismatch")
        git_seen.add(source)
        if not isinstance(record["tree"], str) or not re.fullmatch(r"[a-f0-9]{40}", record["tree"]):
            raise ValueError("Cargo Git tree must have authenticated full identity")
        prepare.verify_git_revision(_material_path(pool, record["object_store"]), record["commit"], record["tree"])
    if registry_seen != set(required_registry) or git_seen != set(required_git):
        raise ValueError("Cargo material index missing/extra locked packages")
    return archives



def prepare_cargo_home(seed, home, archives, index, pool):
    """Populate fresh private Cargo state from original verified materials.

    Seed only authenticated registry index metadata and pinned bare Git objects.
    Never trust restored registry source/extraction/cache trees. Pinned Cargo
    expands original archives and normalizes Git workspace inheritance itself.
    """
    seed, home = Path(seed), Path(home)
    if list(home.iterdir()) or {path.name for path in seed.iterdir()} - {"registry", "git"}:
        raise ValueError("fresh Cargo home and closed metadata seed required")
    registry = seed / "registry"
    if registry.exists():
        if {path.name for path in registry.iterdir()} != {"index"}:
            raise ValueError("Cargo seed restored registry sources/cache are forbidden")
        namespaces = list((registry / "index").iterdir())
        if len(namespaces) != 1 or namespaces[0].name != CARGO_REGISTRY_CACHE_NAME or not namespaces[0].is_dir():
            raise ValueError("source-owned default Cargo registry namespace required")
        shutil.copytree(registry / "index", home / "registry/index")
    elif archives:
        raise ValueError("authenticated Cargo registry index metadata required")
    cache = home / "registry/cache" / CARGO_REGISTRY_CACHE_NAME
    if archives:
        cache.mkdir(parents=True)
    for (name, version, source), checked in archives.items():
        if source != CARGO_REGISTRY_SOURCE or not re.fullmatch(r"[a-zA-Z0-9_-]+", name) or not re.fullmatch(r"[0-9][a-zA-Z0-9.+-]*", version):
            raise ValueError("unselected Cargo registry material identity")
        data = prepare._authenticated_bytes(checked.path, checked.integrity, "sha256", prepare.MAX_ARCHIVE)
        (cache / (name + "-" + version + ".crate")).write_bytes(data)
    git = seed / "git"
    if git.exists():
        if {path.name for path in git.iterdir()} != {"db"}:
            raise ValueError("Cargo seed restored Git checkouts are forbidden")
        databases = list((git / "db").iterdir())
        if len(databases) != len(index["git"]):
            raise ValueError("Cargo seed Git object-store membership mismatch")
        matched = set()
        for database in databases:
            matches = []
            for record in index["git"]:
                try:
                    prepare.verify_git_revision(database, record["commit"], record["tree"])
                    matches.append(record["source"])
                except ValueError:
                    continue
            if len(matches) != 1 or matches[0] in matched:
                raise ValueError("Cargo seed Git bytes do not join the authenticated original revision")
            matched.add(matches[0])
        shutil.copytree(git / "db", home / "git/db")
    elif index["git"]:
        raise ValueError("authenticated pinned Cargo Git object metadata required")


def validate_pair_observations(a, b, qualification):
    """Reject dishonest/missing ambient variation and normalized compiler claims."""
    fields = {"role", "checkout_root", "umask", "jobs", "locale", "timezone", "start_time_ns",
              "execution_user", "user_variation", "normalized"}
    for observation, role in ((a, "A"), (b, "B")):
        if not isinstance(observation, dict) or set(observation) != fields or observation["role"] != role:
            raise ValueError("missing role-specific ambient observations")
        selected = ambient_profile(role)
        for field in ("umask", "jobs"):
            if observation[field] != selected[field] or type(observation[field]) is not type(selected[field]):
                raise ValueError(f"actual {field} observation differs from selected role")
        if type(observation["start_time_ns"]) is not int or observation["start_time_ns"] < 0:
            raise ValueError("actual start time observation required")
        if not isinstance(observation["checkout_root"], str) or not Path(observation["checkout_root"]).is_absolute():
            raise ValueError("actual external checkout root observation required")
    if a["checkout_root"] == b["checkout_root"] or a["start_time_ns"] == b["start_time_ns"]:
        raise ValueError("selected root/start-time variation missing")
    if a["locale"] != "C" or b["locale"] != qualification["locale"] or a["locale"] == b["locale"]:
        raise ValueError("selected locale variation missing")
    if a["timezone"] != "UTC" or b["timezone"] != qualification["timezone"] or a["timezone"] == b["timezone"]:
        raise ValueError("selected timezone variation missing")
    if a["normalized"] != b["normalized"] or a["normalized"].get("locale") != "C" or a["normalized"].get("timezone") != "UTC" or a["normalized"].get("source_root") != "/source":
        raise ValueError("normalized values must be identical and cannot be reported varied")
    if qualification["user_variation"]:
        if a["execution_user"] == b["execution_user"] or a["user_variation"] != "passed" or b["user_variation"] != "passed":
            raise ValueError("qualified execution user variation missing")
    elif a["user_variation"] != "unsupported" or b["user_variation"] != "unsupported" or a["execution_user"] != b["execution_user"]:
        raise ValueError("unsupported user variation must be reported honestly")



# Empty until vendor acquisitions, external roots and execution identities have
# independent review. This source table is the authority for candidate staging;
# downloaded JSON and version output can never authenticate their own tools.
CANDIDATE_ACQUISITIONS = {}



def executing_source_context(commit):
    """Bind independent source approval to the actual executing checkout/helpers.

    No acquisition policy embeds its own future Git commit. Compare immutable
    requested objects to executing ROOT, and also to helper bytes captured when
    this module loaded, so later edits cannot retitle already loaded code.
    """
    preflight._commit_id(commit)
    if Path(__file__).resolve() != ROOT / "scripts/release/rust_recipe.py" or RELEASE != ROOT / "scripts/release":
        raise ValueError("executing helper checkout identity mismatch")
    for module in (preflight, prepare, sys.modules["common"]):
        if Path(module.__file__).resolve().parent != RELEASE:
            raise ValueError("executing helper import comes from an alternate checkout")
    try:
        snapshot = preflight.source_snapshot(ROOT, commit, {})
    except (OSError, ValueError) as error:
        raise ValueError("executing source commit/helper snapshot mismatch") from error
    for name, digest in EXECUTING_HELPER_HASHES.items():
        record = snapshot["files"].get("scripts/release/" + name)
        if record is None or record["sha256"] != digest:
            raise ValueError("executing helper bytes changed after import or are untracked")
    return {"source": str(ROOT), "source_commit": commit, "snapshot": snapshot}


def candidate_selection(asset, commit):
    """Authenticate private candidate selections without requiring future results.

    Source-owned acquisition pins precede Task 8 measurement. Results, recipe
    qualification and signing permissions cannot bootstrap these pins. Missing
    pins refuse before workspace creation. There is deliberately no CLI bypass.
    """
    artifact = artifact_selection(asset)
    preflight._commit_id(commit)
    environment_id = ("macos15-aarch64" if artifact["target"] == "aarch64-apple-darwin"
                      else "macos15-x86_64" if artifact["target"] == "x86_64-apple-darwin"
                      else "gnu-builder-x86_64")
    selected = CANDIDATE_ACQUISITIONS.get(environment_id)
    if selected is None:
        raise ValueError("authenticated candidate acquisition pins pending independent review")
    required = {"environment_id", "tools", "readonly", "trees", "tool_trees",
                "archive_pool", "cargo_seed", "rustup", "sdk", "variation", "image", "runtime"}
    if not isinstance(selected, dict) or set(selected) != required or selected["environment_id"] != environment_id:
        raise ValueError("candidate acquisition policy must not select source/context")
    selected = copy.deepcopy(selected)
    required_tools = {"python", "git", "cargo", "rustc", "cc", "cxx", "linker", "ar", "cmake", "shell"}
    if environment_id == "gnu-builder-x86_64":
        required_tools.add("docker")
    if artifact["kind"] == "wasm-core":
        required_tools.update({"wasmtime", "viceroy"})
    if set(selected["tools"]) != required_tools:
        raise ValueError("candidate acquisition tool closure incomplete")
    if environment_id == "gnu-builder-x86_64":
        if selected["tools"]["docker"]["path"] != DOCKER_EXECUTABLE:
            raise ValueError("candidate must bind the actual source-owned Docker executable")
    for name, tool in selected["tools"].items():
        if not isinstance(tool, dict) or set(tool) != {"path", "sha256", "acquisition_sha256"}:
            raise ValueError("candidate acquisition tool binding incomplete")
        path = Path(tool["path"])
        if not path.is_absolute() or path.is_symlink() or not isinstance(tool["acquisition_sha256"], str) or not re.fullmatch(r"[a-f0-9]{64}", tool["acquisition_sha256"]):
            raise ValueError("candidate acquisition authority required")
        if tool_file_sha256(path) != tool["sha256"]:
            raise ValueError("candidate acquired executable digest mismatch")
    if selected["tools"]["git"]["path"] != "/usr/bin/git":
        raise ValueError("source inspection must bind the actual trusted Git executable")
    if Path(selected["tools"]["python"]["path"]).resolve() != Path(sys.executable).resolve():
        raise ValueError("candidate interpreter must be the actual independently authenticated bootstrap")
    for path, digest in selected["trees"].items():
        check_tree(path, digest)
    for key in ("archive_pool", "cargo_seed"):
        if selected[key] not in selected["trees"]:
            raise ValueError("candidate immutable original material tree missing")
    tool_roots = list(selected["tool_trees"])
    for key in ("rustup", "sdk"):
        if selected[key] not in selected["tool_trees"]:
            raise ValueError("candidate authenticated tool/SDK tree missing")
    for path, digest in selected["tool_trees"].items():
        if tool_tree_digest(path, approved_roots=tool_roots) != digest:
            raise ValueError("candidate authenticated tool/SDK tree changed")
    context = executing_source_context(commit)
    selected.update(source=context["source"], source_commit=context["source_commit"])
    return selected


def vendor_configuration(lock_bytes, vendor):
    """Generate reviewed exact source replacement, discarding Cargo's suggestions."""
    lock = tomllib.loads(lock_bytes.decode())
    configuration = {"source": {"release-vendor": {"directory": str(vendor)}}, "net": {"offline": True}}
    git = sorted({package["source"] for package in lock["package"] if package.get("source", "").startswith("git+")})
    registries = {package["source"] for package in lock["package"] if package.get("source", "").startswith("registry+")}
    if registries - {CARGO_REGISTRY_SOURCE}:
        raise ValueError("unselected Cargo registry source")
    if registries:
        configuration["source"]["crates-io"] = {"replace-with": "release-vendor"}
    for index, source in enumerate(git):
        match = re.fullmatch(r"git\+(https://[^?#]+)\?rev=([a-f0-9]{40})#([a-f0-9]{40})", source)
        if not match or match[2] != match[3]:
            raise ValueError("unselected Cargo Git URL/revision")
        configuration["source"][f"release-git-{index}"] = {"git": match[1], "rev": match[2], "replace-with": "release-vendor"}
    # JSON strings are also valid quoted TOML strings for these selected paths.
    quote = lambda value: deterministic_json_bytes(value).decode().strip()
    lines = ["[net]", "offline = true"]
    for name, record in sorted(configuration["source"].items()):
        lines.extend(["", "[source." + quote(name) + "]"])
        lines.extend(key + " = " + quote(value) for key, value in sorted(record.items()))
    data = ("\n".join(lines) + "\n").encode()
    if tomllib.loads(data.decode()) != configuration:
        raise ValueError("source-owned Cargo configuration encoding mismatch")
    return data


def write_vendor_configuration(lock_bytes, vendor, configuration, boundary):
    """Write source-owned Cargo replacement paths in the execution namespace."""
    directory = boundary.map_path(vendor) if isinstance(boundary, LinuxBoundary) else str(vendor)
    Path(configuration).write_bytes(vendor_configuration(lock_bytes, directory))


def compare_final_bytes(a, b, artifact):
    """Require full Fastly equality; retain nonblocking native mismatch measures."""
    left = prepare._regular_bytes(a, 536870912)
    right = prepare._regular_bytes(b, 536870912)
    equal = left == right
    blocking = artifact["kind"] == "wasm-core"
    if blocking and not equal:
        raise ValueError("complete final Fastly A/B bytes differ")
    return {"equal": equal, "blocking": blocking, "A_sha256": sha256_bytes(left),
            "B_sha256": sha256_bytes(right), "A_size": len(left), "B_size": len(right)}



def snapshot_final(selected, protected_root, name, *, data=None):
    """Snapshot exact Cargo bytes into a directory denied to compile descendants.

    The caller creates and protects this directory in the compile boundary
    before launching any build. Inspection/runtime gates use this stable copy,
    so detached compiler children cannot race a mutable target after evidence.
    """
    if not re.fullmatch(r"[a-zA-Z0-9_.-]+", name):
        raise ValueError("invalid fixed final executable name")
    path = Path(protected_root) / name
    if os.path.lexists(path):
        raise ValueError("accepted final executable must be fresh")
    if data is None:
        data = prepare._regular_bytes(selected, 536870912)
    if not isinstance(data, bytes) or not data or len(data) > 536870912:
        raise ValueError("bounded selected executable snapshot required")
    with path.open("xb") as output:
        output.write(data)
    path.chmod(0o755 if not name.endswith(".wasm") else 0o644)
    if sha256_bytes(prepare._regular_bytes(path, 536870912)) != sha256_bytes(data):
        raise ValueError("accepted final bytes changed during snapshot")
    return path


def _load_gate(name):
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), RELEASE / (name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _compile_environment(selection, workspace, artifact, request, snapshot, jobs):
    source, vendor, target = workspace / "source", workspace / "vendor", workspace / "target"
    commit, epoch = request.source_commit, snapshot["source_date_epoch"]
    js_dir, inventory = workspace / "js", workspace / "js-inventory.json"
    tools = {name: record["path"] for name, record in selection["tools"].items()}
    paths = {"home": str(workspace / "home"), "cargo_home": str(workspace / "cargo-home"),
             "rustup_home": selection["rustup"], "tmpdir": str(workspace / "tmp"),
             "tool_path": sorted({str(Path(path).parent) for path in tools.values()}), "sysroot": selection["sdk"]}
    environment = preflight.build_environment({}, commit, epoch, artifact["target"], paths)
    environment.update({"RUSTC": tools["rustc"], "CC": tools["cc"], "CXX": tools["cxx"], "AR": tools["ar"],
                        "CMAKE": tools["cmake"], "CARGO_BUILD_JOBS": str(jobs), "CARGO_PROFILE_RELEASE_DEBUG": "1",
                        "CARGO_PROFILE_RELEASE_STRIP": "none", "TSJS_PREBUILT_DIR": str(js_dir),
                        "TSJS_PREBUILT_INVENTORY": str(inventory)})
    remaps = ((source, "/source"), (vendor, "/vendor"), (target, "/target"),
              (Path(selection["sdk"]), "/sdk"), (Path(selection["rustup"]), "/rust-toolchain"),
              (workspace, "/build"))
    host = artifact["target"] if artifact["target"].endswith("apple-darwin") else "x86_64-unknown-linux-gnu"
    environment["CARGO_TARGET_" + host.replace("-", "_").upper() + "_LINKER"] = tools["linker"]
    environment["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join("--remap-path-prefix=" + str(path) + "=" + normalized for path, normalized in remaps)
    flags = " ".join("-ffile-prefix-map=" + str(path) + "=" + normalized + " -fdebug-prefix-map=" + str(path) + "=" + normalized for path, normalized in remaps)
    environment["CFLAGS"] = flags
    environment["CXXFLAGS"] = flags
    return environment, tools


def native_cli_contract(source, commit):
    """Bind native CLI version and embedded fictional scaffold to prepared source."""
    preflight._commit_id(commit)
    source = Path(source)
    workspace = tomllib.loads(prepare._regular_bytes(source / "Cargo.toml", 1048576).decode())
    package = tomllib.loads(prepare._regular_bytes(source / "crates/trusted-server-cli/Cargo.toml", 1048576).decode())["package"]
    if package.get("name") != "trusted-server-cli":
        raise ValueError("native CLI owning package mismatch")
    version = package.get("version")
    if version == {"workspace": True}:
        version = workspace["workspace"]["package"]["version"]
    if not isinstance(version, str) or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[a-zA-Z0-9.-]+)?(?:\+[a-zA-Z0-9.-]+)?", version):
        raise ValueError("native CLI source package version invalid")
    scaffold = prepare._regular_bytes(source / "trusted-server.example.toml", 1048576)
    if not scaffold or not tomllib.loads(scaffold.decode()):
        raise ValueError("native CLI source scaffold missing")
    return {"package_version": version, "source_commit": commit, "scaffold": scaffold,
            "version_stdout": ("ts " + version + " (" + commit + ")\n").encode()}


def native_cli_outcomes(contract, help_output, version, initialization, scaffold):
    """Require actual help, full source/version binding and exact generated config."""
    if b"Usage: ts" not in help_output:
        raise ValueError("native CLI help startup missing")
    if version != contract["version_stdout"]:
        raise ValueError("native CLI version/source binding mismatch")
    if not re.fullmatch(rb"Initialized config at [^\r\n]+/trusted-server\.toml\n", initialization):
        raise ValueError("native CLI config initialization outcome missing")
    if scaffold != contract["scaffold"]:
        raise ValueError("native CLI generated scaffold missing or differs from prepared source")
    return {"help": {"exit_code": 0, "stdout_sha256": sha256_bytes(help_output)},
            "version": {"exit_code": 0, "stdout_sha256": sha256_bytes(version),
                        "package_version": contract["package_version"], "source_commit": contract["source_commit"]},
            "config_init": {"exit_code": 0, "stdout_sha256": sha256_bytes(initialization),
                            "scaffold_sha256": sha256_bytes(scaffold), "scaffold_size": len(scaffold)}}


def native_cli_probe(executable, runtime, contract, environment, log_root):
    """Run fixed native commands outside checkout with fresh writable config state."""
    generated = runtime / "output"
    generated.mkdir(mode=0o700)
    configuration = generated / "trusted-server.toml"
    help_output, _ = capture([str(executable), "--help"], environment, generated, log_root / "help", timeout=30, maximum=65536)
    version, _ = capture([str(executable), "--version"], environment, generated, log_root / "version", timeout=30, maximum=65536)
    initialized, _ = capture([str(executable), "config", "init", "--app-config", str(configuration)],
                             environment, generated, log_root / "config-init", timeout=30, maximum=65536)
    scaffold = prepare._regular_bytes(configuration, 1048576)
    return native_cli_outcomes(contract, help_output, version, initialized, scaffold)


def native_runtime_identity(output, identity):
    """Parse actual selected GNU OS/architecture and positive CLI help output."""
    expected = {"ubi9-x86_64": ("rhel", r"9(?:\.[0-9]+)*"),
                "ubuntu22.04-x86_64": ("ubuntu", r"22\.04(?:\.[0-9]+)*"),
                "debian12-x86_64": ("debian", r"12(?:\.[0-9]+)*")}
    if identity not in expected or len(output) > 65536:
        raise ValueError("unknown selected runtime identity")
    try:
        release, rest = output.decode("utf-8", errors="strict").split("\nTS_RELEASE_ARCH\n")
        architecture, help_output = rest.split("\nTS_RELEASE_HELP\n")
    except (ValueError, UnicodeError) as error:
        raise ValueError("missing actual runtime OS/architecture evidence") from error
    fields = {}
    for line in release.splitlines():
        if not line or line.startswith("#"):
            continue
        key, separator, value = line.partition("=")
        if not separator or not re.fullmatch(r"[A-Z_]+", key) or key in fields:
            raise ValueError("invalid runtime OS identity fields")
        values = shlex.split(value)
        if len(values) != 1:
            raise ValueError("invalid runtime OS identity value")
        fields[key] = values[0]
    operating_system, version = expected[identity]
    if fields.get("ID") != operating_system or not re.fullmatch(version, fields.get("VERSION_ID", "")):
        raise ValueError("actual runtime OS/version differs from selected image slot")
    if architecture != "x86_64" or "Usage:" not in help_output:
        raise ValueError("actual runtime architecture/CLI startup mismatch")
    return {"os_id": operating_system, "os_version": fields["VERSION_ID"], "architecture": architecture,
            "help_stdout_sha256": sha256_bytes(help_output.encode())}


def _native_startup(final, artifact, selection, workspace, environment):
    """Run exact CLI outside checkout on selected real macOS or clean OCI images."""
    runtime = workspace / "native-runtime"
    runtime.mkdir(mode=0o755)
    executable = runtime / "ts"
    shutil.copyfile(final, executable)
    executable.chmod(0o755)
    before = sha256_bytes(prepare._regular_bytes(executable, 536870912))
    observations = []
    contract = native_cli_contract(workspace / "source", selection["source_commit"])
    if artifact["target"].endswith("apple-darwin"):
        import platform
        if sys.platform != "darwin" or platform.mac_ver()[0].split(".")[0] != "15":
            raise ValueError("actual macOS15 native startup required; newer local macOS is unqualified")
        architecture = "aarch64" if platform.machine() == "arm64" else platform.machine()
        if architecture != artifact["target"].split("-")[0]:
            raise ValueError("actual native startup architecture mismatch")
        translated = subprocess.run(["/usr/sbin/sysctl", "-in", "sysctl.proc_translated"], capture_output=True, timeout=10, check=False)
        if translated.stdout.strip() == b"1":
            raise ValueError("translated native startup forbidden")
        commands = native_cli_probe(executable, runtime, contract,
            {"HOME": str(runtime), "TMPDIR": str(runtime), "LANG": "C", "LC_ALL": "C", "TZ": "UTC"}, runtime)
        observations.append({"environment_id": selection["environment_id"], "os_version": platform.mac_ver()[0],
                             "architecture": architecture, "executable_sha256": before,
                             "stdout_sha256": commands["help"]["stdout_sha256"], "commands": commands})
    else:
        expected = {"ubi9-x86_64", "ubuntu22.04-x86_64", "debian12-x86_64"}
        if not isinstance(selection["runtime"], dict) or set(selection["runtime"]) != expected:
            raise ValueError("all three selected clean GNU runtime images required")
        for identity, image in sorted(selection["runtime"].items()):
            if not isinstance(image, str) or not re.fullmatch(r"[a-z0-9./_-]+@sha256:[a-f0-9]{64}", image):
                raise ValueError("authenticated clean native OCI runtime digest required")
            control = workspace / ("native-control-" + identity)
            control.mkdir(mode=0o755)
            generated = control / "output"
            generated.mkdir(mode=0o777)
            generated.chmod(0o777)
            boundary = LinuxBoundary(control, [runtime], image)
            runtime_environment = {"HOME": "/work/output", "TMPDIR": "/work/output", "LANG": "C", "LC_ALL": "C", "TZ": "UTC"}
            driver = ('umask 022; cat /etc/os-release && printf "\\nTS_RELEASE_ARCH\\n" && uname -m '
                      '&& printf "TS_RELEASE_HELP\\n" && "$1" --help '
                      '&& printf "TS_RELEASE_VERSION\\n" && "$1" --version '
                      '&& printf "TS_RELEASE_INIT\\n" && "$1" config init --app-config "$2" '
                      '&& printf "TS_RELEASE_SCAFFOLD\\n" && test -f "$2" && test ! -L "$2" && cat "$2"')
            argv = boundary.command(["/bin/sh", "-c", driver, "release-runtime", str(executable), "/work/output/trusted-server.toml"],
                                    environment=runtime_environment, cwd=generated)
            output, _ = capture(argv, {"HOME": str(control), "LANG": "C", "LC_ALL": "C"},
                                runtime, runtime / identity, timeout=60, maximum=65536, boundary=boundary)
            try:
                help_and_identity, remaining = output.split(b"TS_RELEASE_VERSION\n")
                version, remaining = remaining.split(b"TS_RELEASE_INIT\n")
                initialized, scaffold = remaining.split(b"TS_RELEASE_SCAFFOLD\n")
                help_output = help_and_identity.split(b"\nTS_RELEASE_HELP\n")[1]
            except (ValueError, IndexError) as error:
                raise ValueError("native runtime missing help/version/config-init evidence") from error
            details = native_runtime_identity(help_and_identity, identity)
            commands = native_cli_outcomes(contract, help_output, version, initialized, scaffold)
            observations.append({"environment_id": identity, "image": image, "executable_sha256": before,
                                 "stdout_sha256": sha256_bytes(output), "commands": commands, **details})
    if sha256_bytes(prepare._regular_bytes(executable, 536870912)) != before or sha256_bytes(prepare._regular_bytes(final, 536870912)) != before:
        raise ValueError("native final bytes changed during clean startup")
    if any(not record["stdout_sha256"] or record["stdout_sha256"] == sha256_bytes(b"") for record in observations):
        raise ValueError("native startup requires actual help output")
    return {"sha256_before": before, "sha256_after": before, "outside_checkout": True, "runtime": observations}



DOWNLOAD_BUILD_SCRIPT = r"""
use std::io::{Read as _, Write as _};
fn main() {
    let address = std::env::var("DOWNLOAD_PROBE_ADDRESS").expect("should receive fixture address");
    let marker = std::env::var("DOWNLOAD_PROBE_MARKER").expect("should receive fixture marker");
    let jobs = std::env::var("NUM_JOBS").expect("should receive actual Cargo concurrency").parse::<u32>().expect("should parse actual Cargo concurrency");
    let result = std::net::TcpStream::connect(address).and_then(|mut stream| {
        stream.write_all(b"GET /fixture HTTP/1.0\r\nHost: example.com\r\n\r\n")?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response)?;
        if response.ends_with(b"fixture-download-example") { Ok(()) }
        else { Err(std::io::Error::other("fixture response mismatch")) }
    });
    match result {
        Ok(()) => {
            let observation = format!("{{\"outcome\":\"allowed\",\"errno\":0,\"jobs\":{jobs}}}");
            std::fs::write(marker, observation).expect("should record fixture success");
        },
        Err(error) => {
            let code = error.raw_os_error().unwrap_or(0);
            let outcome = if code == 1 || code == 13 { "denied" } else { "failed" };
            let observation = format!("{{\"outcome\":\"{outcome}\",\"errno\":{code},\"jobs\":{jobs}}}");
            std::fs::write(marker, observation).expect("should record actual build-script error");
            panic!("fixture download must fail without network access");
        }
    }
}
"""
DOWNLOAD_DRIVER = r"""
import os, subprocess, sys
from pathlib import Path
cargo, fixture, target, marker, phase = sys.argv[1:]
server = None
if phase == 'unconfined':
    import http.server, threading
    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b'fixture-download-example')
        def log_message(self, *args):
            pass
    server = http.server.HTTPServer(('127.0.0.1', 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
address = '127.0.0.1:' + str(server.server_port if server else 9)
environment = dict(os.environ, DOWNLOAD_PROBE_ADDRESS=address, DOWNLOAD_PROBE_MARKER=marker)
try:
    result = subprocess.run([cargo, 'build', '--release', '--frozen', '--manifest-path', fixture + '/Cargo.toml', '--target-dir', target],
                            env=environment, stdin=subprocess.DEVNULL, close_fds=True, timeout=600, check=False)
finally:
    if server:
        server.shutdown()
        server.server_close()
sys.stdout.buffer.write(Path(marker).read_bytes())
sys.exit(result.returncode)
"""


def prepare_download_fixture(workspace):
    """Prepare an immutable dependency-free Cargo project with a real HTTP hook."""
    fixture = Path(workspace) / "download-fixture"
    fixture.mkdir()
    (fixture / "src").mkdir()
    (fixture / "Cargo.toml").write_text('[package]\nname = "release-download-fixture"\nversion = "0.1.0"\nedition = "2024"\n[workspace]\n')
    (fixture / "Cargo.lock").write_text('version = 4\n[[package]]\nname = "release-download-fixture"\nversion = "0.1.0"\n')
    (fixture / "src/main.rs").write_text('fn main() {}\n')
    (fixture / "build.rs").write_text(DOWNLOAD_BUILD_SCRIPT)
    return fixture


def verify_download_fixture(boundary, workspace, tools, environment, *, execution_umask=0o022, log_root=None):
    """Require actual build-script HTTP success outside and denial inside boundary.

    Each phase compiles its own fresh target. A compiler/bootstrap failure cannot
    count as a download denial because the build-script marker must exist and
    contain the actual network syscall's permission errno.
    """
    workspace = Path(workspace)
    fixture = workspace / "download-fixture"
    log_root = Path(log_root) if log_root is not None else workspace
    observations = {}
    for phase in ("unconfined", "confined"):
        target = workspace / ("download-target-" + phase)
        marker = workspace / ("download-marker-" + phase + ".json")
        if target.exists() or marker.exists():
            raise ValueError("actual Cargo download fixture target/marker must be fresh")
        argv = [tools["python"], "-I", "-B", "-c", DOWNLOAD_DRIVER, tools["cargo"], str(fixture), str(target), str(marker), phase]
        if phase == "confined":
            argv = boundary.command(argv, environment=environment, cwd=fixture)
        elif isinstance(boundary, LinuxBoundary):
            argv = boundary._container_command(argv, confined=False, environment=environment, cwd=fixture)
        failure = None
        try:
            measured, _ = capture(argv, environment, fixture, log_root / ("download-" + phase), timeout=660, umask=execution_umask, boundary=boundary if isinstance(boundary, LinuxBoundary) else None)
        except RecipeProcessError as error:
            failure, measured = error, error.stdout
        if not measured:
            raise ValueError("actual build-script download was not reached; missing syscall evidence") from failure
        observed = load_json_bytes(measured, max_bytes=65536)
        if not isinstance(observed, dict) or set(observed) != {"outcome", "errno", "jobs"} or type(observed["errno"]) is not int or type(observed["jobs"]) is not int:
            raise ValueError("actual build-script download evidence invalid")
        if observed["jobs"] != int(environment["CARGO_BUILD_JOBS"]):
            raise ValueError("actual Cargo build-script concurrency differs from selected jobs")
        if phase == "unconfined" and (failure is not None or observed["outcome"] != "allowed" or observed["errno"] != 0):
            raise ValueError("actual unconfined build-script HTTP control must succeed") from failure
        if phase == "confined" and (failure is None or observed["outcome"] != "denied" or observed["errno"] not in {1, 13}):
            raise ValueError("actual confined build-script HTTP attempt must fail with permission denial")
        observations[phase] = observed
    return {"actual_build_script": True, "boundary_sha256": boundary.sha256, **observations}



def container_permissions(workspace, writable, immutable):
    """Permit fixed container UID access while outer host root remains private.

    These modes only make prepared bytes readable and generated state writable.
    Read-only OCI overlays enforce immutability; owner chmod is never its proof.
    Container-produced B files remain private to its UID and are read through an
    authenticated reader in that same container context, not by host DAC bypass.
    """
    workspace = Path(workspace).resolve(strict=True)
    if workspace.parent.stat().st_mode & 0o077:
        raise ValueError("outer host material root must remain private")
    workspace.chmod(0o777)
    for root in writable:
        root = Path(root)
        if not root.resolve(strict=True).is_relative_to(workspace) or root.is_symlink():
            raise ValueError("writable generated state must be inside private workspace")
        for path in (root, *root.rglob("*")):
            if path.is_symlink():
                raise ValueError("writable generated state must not contain symlinks")
            path.chmod(0o777 if path.is_dir() or path.stat().st_mode & 0o111 else 0o666)
    for root in immutable:
        root = Path(root)
        if not root.resolve(strict=True).is_relative_to(workspace) or root.is_symlink():
            raise ValueError("immutable prepared input must be inside private workspace")
        members = (root, *root.rglob("*")) if root.is_dir() else (root,)
        for path in members:
            if path.is_symlink():
                # The only source link was already checked by tracked export;
                # preserve it and never chmod through its target.
                continue
            path.chmod(0o755 if path.is_dir() or path.stat().st_mode & 0o111 else 0o644)



def verified_shared_js_input(request):
    """Select an independently verified current-run shared-JS producer binding.

    Inventory hashes/source labels prove consistency only. This private boundary
    stays closed until the authenticated Task 7 producer integration or a current
    run transfer-receipt verifier supplies its binding. There is no public JSON
    authenticated flag, receipt-path option or candidate bypass. Implement the
    authenticating integration before measuring the complete Task 8 closure.
    """
    preflight._commit_id(request.source_commit)
    raise ValueError("independent shared-JS producer binding pending Task 8/transfer integration")


def _build_recipe(request, selection):
    """Execute preparation, same-boundary controls/compile and final-byte gates.

    The only official/private selectors authenticate source-owned acquisition
    bindings before this entry. No JSON commands, fixture bypass, transforms,
    publication authority or network-enabled compile path exist here.
    """
    from qualify import validate_socket_pair
    artifact = artifact_selection(request.asset)
    role = ambient_profile(request.role)
    if request.source_commit != selection["source_commit"]:
        raise ValueError("independently approved source selection mismatch")
    # Reauthenticate every source-owned acquisition at the entry; no Boolean
    # authenticated flag in an observation/descriptor can substitute for this.
    authentic = candidate_selection(request.asset, request.source_commit)
    if selection != authentic:
        raise ValueError("candidate acquisition selection mismatch")
    shared_js_binding = verified_shared_js_input(request)
    for path in (request.materials, request.output):
        if os.path.lexists(path):
            raise ValueError("recipe materials/output must be fresh")
    if request.materials.is_relative_to(Path(selection["source"]).resolve()) or request.output.is_relative_to(request.materials) or request.materials.is_relative_to(request.output):
        raise ValueError("external disjoint recipe roots required")
    variation = selection["variation"]
    if (not isinstance(variation, dict) or set(variation) != {"locale", "timezone", "user_variation"}
            or variation["locale"] in {"C", "qualification-selected-alternate"}
            or variation["timezone"] in {"UTC", "qualification-selected-alternate"}):
        raise ValueError("reviewed candidate alternate ambient selections required; support remains unqualified")
    if variation["user_variation"] is not False:
        raise ValueError("execution user variation unsupported by current fixed boundary")
    work = fresh_root(request.materials)
    workspace = work / role["checkout_root"]
    workspace.mkdir(mode=0o700)
    source = workspace / "source"
    snapshot = prepare.materialize_source(selection["source"], request.source_commit, source)
    for name in ("home", "tmp", "cargo-home", "target", "js", "accepted-final"):
        (workspace / name).mkdir(mode=0o700)
    target, vendor = workspace / "target", workspace / "vendor"
    # Preparation operates only on independently authenticated original cached
    # materials. Cargo performs its own workspace-inheritance normalization.
    lock_path = source / "Cargo.lock"
    lock_bytes = prepare._regular_bytes(lock_path, 1048576)
    pool = Path(selection["archive_pool"])
    index = read_json_file(pool / "cargo-materials.json", max_bytes=1048576)
    originals = verify_cargo_materials(lock_bytes, index, pool)
    seed = Path(selection["cargo_seed"])
    for name in ("config", "config.toml"):
        if os.path.lexists(seed / name):
            raise ValueError("Cargo preparation seed configuration forbidden")
    prepare_cargo_home(seed, workspace / "cargo-home", originals, index, pool)
    js_inventory = prepare._regular_bytes(request.js_inventory, 1048576)
    inventory = load_json_bytes(js_inventory, max_bytes=1048576)
    if inventory.get("source_commit") != request.source_commit:
        raise ValueError("prepared JS inventory source identity mismatch")
    expected_js = sorted("tsjs-" + name + ".js" for name in prepare._expected_modules(source, request.source_commit))
    if sorted(path.name for path in request.js_dir.iterdir()) != expected_js:
        raise ValueError("complete prepared JS pair required")
    modules = inventory.get("modules", [])
    if sorted(record.get("filename", "") for record in modules) != expected_js:
        raise ValueError("prepared JS inventory module membership mismatch")
    for record in modules:
        data = prepare._regular_bytes(request.js_dir / record["filename"], prepare.MAX_FILE)
        if record.get("size") != len(data) or record.get("sha256") != sha256_bytes(data):
            raise ValueError("prepared JS inventory bytes mismatch")
        (workspace / "js" / record["filename"]).write_bytes(data)
    private_inventory = workspace / "js-inventory.json"
    private_inventory.write_bytes(js_inventory)
    environment, tools = _compile_environment(selection, workspace, artifact, request, snapshot, role["jobs"])
    capture([tools["cargo"], "vendor", "--offline", "--locked", "--versioned-dirs", str(vendor)], environment, source, workspace / "preparation", umask=int(role["umask"], 8))
    prepare.source_check(source, request.source_commit, snapshot)
    if prepare._regular_bytes(lock_path, 1048576) != lock_bytes:
        raise ValueError("Cargo vendor mutated locked source")
    for original in originals.values():
        prepare._authenticated_bytes(original.path, original.integrity, "sha256", prepare.MAX_ARCHIVE)
    configuration = workspace / "cargo-release.toml"
    configuration.touch()
    download_fixture = prepare_download_fixture(workspace)
    evidence_logs = workspace / "accepted-final" / "logs"
    evidence_logs.mkdir(mode=0o755)
    immutable = [source, vendor, workspace / "js", private_inventory, configuration, download_fixture, workspace / "accepted-final"]
    readonly = [Path(path) for path in selection["readonly"]]
    if not artifact["target"].endswith("apple-darwin"):
        container_permissions(workspace, [workspace / name for name in ("home", "tmp", "cargo-home", "target")], immutable)
    vendor_digest = tree_digest(vendor)
    boundary = (MacBoundary(workspace, readonly, immutable=immutable) if artifact["target"].endswith("apple-darwin")
                else LinuxBoundary(workspace, readonly, selection["image"], immutable=immutable, python=tools["python"], execution_umask=int(role["umask"], 8)))
    write_vendor_configuration(lock_bytes, vendor, configuration, boundary)
    controls = probe_controls(boundary, workspace, tools["python"])
    validate_socket_pair(controls["unconfined"], controls["confined"])
    (workspace / "boundary-controls.json").write_bytes(deterministic_json_bytes(controls))
    download = verify_download_fixture(boundary, workspace, tools, environment, execution_umask=int(role["umask"], 8), log_root=evidence_logs)
    (workspace / "download-controls.json").write_bytes(deterministic_json_bytes(download))
    # Actual ambient observations run inside the exact compile boundary before
    # compiler locale/timezone/epoch are normalized. Selected jobs and umask
    # remain effective compiler inputs, and are independently observed here.
    ambient = dict(environment)
    ambient.update({"LANG": "C" if request.role == "A" else variation["locale"], "LC_ALL": "C" if request.role == "A" else variation["locale"], "TZ": "UTC" if request.role == "A" else variation["timezone"]})
    probe = [tools["python"], "-I", "-B", "-c", "import json,locale,os,time;locale.setlocale(locale.LC_ALL,'');time.tzset();mask=os.umask(0);os.umask(mask);print(json.dumps(dict(checkout_root=os.getcwd(),umask=format(mask,'03o'),jobs=int(os.environ['CARGO_BUILD_JOBS']),locale=os.environ['LC_ALL'],timezone=os.environ['TZ'],start_time_ns=time.time_ns(),execution_user=str(os.getuid()))))"]
    observed, _ = capture(boundary.command(probe, environment=ambient, cwd=source), ambient, source, evidence_logs / "ambient", umask=int(role["umask"], 8), timeout=30, boundary=boundary)
    observations = load_json_bytes(observed, max_bytes=65536)
    actual_root = str(source.resolve(strict=True))
    if observations["checkout_root"] != (boundary.map_path(source) if isinstance(boundary, LinuxBoundary) else actual_root):
        raise ValueError("observed boundary cwd differs from actual external checkout")
    observations["checkout_root"] = actual_root
    observations.update({"role": request.role, "user_variation": "unsupported", "normalized": {"locale": "C", "timezone": "UTC", "epoch": snapshot["source_date_epoch"], "source_root": "/source"}})
    if observations["umask"] != role["umask"] or observations["jobs"] != role["jobs"]:
        raise ValueError("actual compiler jobs/umask differ from selected role")
    argv = [*cargo_command(tools["cargo"], artifact, target, configuration), "--jobs", str(role["jobs"])]
    compile_stdout, _ = capture(boundary.command(argv, environment=environment, cwd=source), environment, source, evidence_logs / "cargo", umask=int(role["umask"], 8), boundary=boundary)
    prepare.source_check(source, request.source_commit, snapshot)
    check_tree(vendor, vendor_digest)
    if prepare._regular_bytes(private_inventory, 1048576) != js_inventory:
        raise ValueError("prepared JS inventory mutated during compile")
    # Container Cargo emits its mapped target path. Convert only the exact
    # source-owned mount prefix, then still apply expected final path checks.
    if isinstance(boundary, LinuxBoundary):
        converted = []
        for line in compile_stdout.splitlines():
            event = load_json_bytes(line, max_bytes=1048576)
            if isinstance(event.get("executable"), str) and event["executable"].startswith("/work/"):
                event["executable"] = str(workspace) + event["executable"][len("/work"):]
            converted.append(deterministic_json_bytes(event).rstrip(b"\n"))
        selected_stdout = b"\n".join(converted)
    else:
        selected_stdout = compile_stdout
    captured_final = {}
    def verified_reader(path):
        code = "import sys;from pathlib import Path;sys.path.insert(0,sys.argv[1]);from prepare import _regular_bytes;p=Path(sys.argv[2]);\nif p.resolve(strict=True)!=p: raise ValueError('final executable path alias');\nsys.stdout.buffer.write(_regular_bytes(p,536870912))"
        command = [tools["python"], "-I", "-B", "-c", code, str(source / "scripts/release"), str(path)]
        data, _ = capture(boundary.command(command, environment=environment, cwd=source), environment,
                          source, evidence_logs / "final-read", timeout=60, maximum=536870912, boundary=boundary)
        captured_final["bytes"] = data
        return data
    cargo_final = final_cargo_artifact(selected_stdout, artifact, target,
        source_root=Path(boundary.map_path(source)) if isinstance(boundary, LinuxBoundary) else source,
        reader=verified_reader if isinstance(boundary, LinuxBoundary) else None)
    final = snapshot_final(cargo_final, workspace / "accepted-final", request.asset, data=captured_final.get("bytes"))
    final_before = sha256_bytes(prepare._regular_bytes(final, 536870912))
    if artifact["kind"] == "wasm-core":
        runtime = workspace / "accepted-final" / "fastly-runtime"
        runtime.mkdir()
        inspection = _load_gate("validate-fastly").validate_final(final, source, runtime, tools, environment, workspace / "js")
    else:
        inspection = _load_gate("inspect-native").inspect(final, artifact["target"], forbidden_paths=(str(workspace), str(pool), str(seed), selection["source"]))
        inspection["startup"] = _native_startup(final, artifact, selection, workspace, environment)
    if sha256_bytes(prepare._regular_bytes(final, 536870912)) != final_before:
        raise ValueError("final executable changed after inspection/runtime evidence")
    if candidate_selection(request.asset, request.source_commit) != selection:
        raise ValueError("authenticated source/tool/material selection changed during recipe")
    evidence = {"asset": artifact, "source_commit": request.source_commit, "ambient": observations,
                "effective_environment": environment, "cargo_argv": argv, "boundary_sha256": boundary.sha256,
                "controls_sha256": sha256_bytes(deterministic_json_bytes(controls)), "vendor_sha256": vendor_digest,
                "cargo_executable": str(cargo_final),
                "cargo_stdout_sha256": sha256_bytes(compile_stdout), "js_inventory_sha256": sha256_bytes(js_inventory),
                "shared_js_binding": shared_js_binding, "download_attempt": download, "inspection": inspection, "qualification": "candidate-only"}
    output = fresh_root(request.output)
    final_bytes = prepare._regular_bytes(final, 536870912)
    if sha256_bytes(final_bytes) != final_before:
        raise ValueError("final executable changed before output copy")
    (output / request.asset).write_bytes(final_bytes)
    (output / request.asset).chmod(0o755 if artifact["kind"] == "native-executable" else 0o644)
    (output / "recipe-evidence.json").write_bytes(deterministic_json_bytes(evidence))
    return evidence


def qualified_selection(artifact):
    """Refuse official execution before reviewed measured qualification exists."""
    artifact_selection(artifact["name"])
    tools = read_json_file(RELEASE / "release-tools.json", max_bytes=1048576)
    preflight.validate_tools(tools, require_qualified=True)
    raise ValueError("release qualification pending: authenticated executable/bootstrap selection required")


def main():
    request = parse_request()
    preflight.audit_environment(os.environ)
    selection = qualified_selection(artifact_selection(request.asset))
    _build_recipe(request, selection)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        logging.error("Canonical release build refuses: %s", error)
        sys.exit(1)
