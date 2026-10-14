"""Source-owned candidate boundaries; local probes never qualify hosted builds."""

import copy
import re
import subprocess
import sys
from pathlib import Path

RELEASE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(RELEASE))

from common import deterministic_json_bytes, load_json_bytes, sha256_bytes
from prepare import LocalMacBoundary

SECCOMP_SOURCE_REVISION = "e295db0a4d7b67853387fef0b2ef0a29e796b1c1"
SECCOMP_SOURCE_SHA256 = "536529b665dd0972c37bfb569f5d4ac8a53592e7b00752bc39ff063ca9864c74"


def linux_seccomp_profile():
    """Augment the exact reviewed Moby default baseline with AF_UNIX only.

    Preserve its default deny and other syscall conditions. Remove every socket,
    socketpair and old socketcall allowance before adding argument-zero filtering.
    Linux execution remains unmeasured until the authorized hosted run.
    """
    data = (Path(__file__).parent / "seccomp-default.json").read_bytes()
    if sha256_bytes(data) != SECCOMP_SOURCE_SHA256:
        raise ValueError("reviewed default seccomp baseline bytes changed")
    baseline = load_json_bytes(data, max_bytes=1048576)
    if baseline["defaultAction"] != "SCMP_ACT_ERRNO" or baseline.get("defaultErrnoRet", 1) != 1:
        raise ValueError("default seccomp denial required")
    profile = copy.deepcopy(baseline)
    rules = []
    for rule in profile["syscalls"]:
        rule["names"] = [name for name in rule["names"] if name not in {"socket", "socketpair", "socketcall"}]
        if rule["names"]:
            rules.append(rule)
    rules.append({"names": ["socket", "socketpair"], "action": "SCMP_ACT_ALLOW",
                  "args": [{"index": 0, "value": 1, "op": "SCMP_CMP_EQ"}]})
    profile["syscalls"] = rules
    return profile


def _roots(private_root, readonly):
    root = Path(private_root).resolve(strict=True)
    reads = [Path(path).resolve(strict=True) for path in readonly]
    if (not root.is_dir() or root == Path("/") or Path(private_root).is_symlink()
            or any(path == Path("/") or root.is_relative_to(path) or path.is_relative_to(root)
                   for path in reads)):
        raise ValueError("private and readonly roots must be disjoint and confined")
    return root, reads


def macos_profile(private_root, readonly, *, immutable=()):
    """Reuse tested local file isolation and add private Unix sockets only.

    Default denial covers INET, Mach lookup and AppleEvent delegation. This is a
    candidate profile: full hosted ARM/Intel recipes still need measurement.
    """
    root, reads = _roots(private_root, readonly)
    # Profile construction is inspectable on any host; only execution is macOS.
    boundary = object.__new__(LocalMacBoundary)
    boundary.private_root, boundary.readonly = root, reads
    profile = boundary.command([])[2]
    quoted = deterministic_json_bytes(str(root)).decode().strip()
    protected_ancestors = set()
    for path in immutable:
        path = Path(path)
        resolved = path.resolve(strict=True)
        if path.is_symlink() or not resolved.is_relative_to(root) or resolved == root:
            raise ValueError("immutable source must be a real member of the private workspace")
        predicate = "subpath" if resolved.is_dir() else "literal"
        quoted_path = deterministic_json_bytes(str(resolved)).decode().strip()
        profile += f"(deny file-write* ({predicate} {quoted_path}))"
        protected_ancestors.update(parent for parent in resolved.parents if parent.is_relative_to(root))
    # Path-based write denial must remain attached to the same source pathname.
    # Deny unlink/rename of its ancestors while permitting writes to their private
    # node_modules/output children. Broad ancestor file-write denial would block
    # the required private installations as well.
    for ancestor in sorted(protected_ancestors):
        quoted_ancestor = deterministic_json_bytes(str(ancestor)).decode().strip()
        profile += f"(deny file-write-unlink (literal {quoted_ancestor}))"
    return (profile + "(allow system-socket (socket-domain AF_UNIX))"
            + f"(allow network-bind network-inbound (local unix-socket (subpath {quoted})))"
            + f"(allow network-outbound (remote unix-socket (subpath {quoted})))")


class MacBoundary:
    """Use the same generated candidate profile for probes and compilation."""

    def __init__(self, private_root, readonly, *, immutable=()):
        if sys.platform != "darwin":
            raise ValueError("macOS candidate boundary requires an actual macOS host")
        self.profile = macos_profile(private_root, readonly, immutable=immutable)
        self.sha256 = sha256_bytes(self.profile.encode())

    def command(self, command):
        """Wrap source-owned argv, closing descriptors in the caller."""
        return ["/usr/bin/sandbox-exec", "-p", self.profile, *map(str, command)]


class LinuxBoundary:
    """Construct a digest-selected isolated GNU candidate container.

    Only the later source-owned authenticated staging selector may instantiate
    this class for recipes. The image argument is internal, never a CLI input.
    Preparation must create writable ownership for the fixed unprivileged UID.
    """

    def __init__(self, private_root, readonly, image):
        self.private_root, self.readonly = _roots(private_root, readonly)
        if not isinstance(image, str) or not re.fullmatch(r"[a-z0-9./_-]+@sha256:[0-9a-f]{64}", image):
            raise ValueError("selected OCI manifest digest required; tags are forbidden")
        self.image = image
        self.profile_path = self.private_root / "network-seccomp.json"
        self.profile_path.write_bytes(deterministic_json_bytes(linux_seccomp_profile()))
        self.profile_path.chmod(0o444)
        self.sha256 = sha256_bytes(self.profile_path.read_bytes())
        self.control_profile_path = self.private_root / "control-seccomp.json"
        self.control_profile_path.write_bytes((Path(__file__).parent / "seccomp-default.json").read_bytes())
        self.control_profile_path.chmod(0o444)
        for root in [self.private_root, *self.readonly]:
            for path in root.rglob("*"):
                if path.is_symlink():
                    resolved = path.resolve(strict=True)
                    if not any(resolved.is_relative_to(approved) for approved in [self.private_root, *self.readonly]):
                        raise ValueError("container mount symlink escapes approved roots")
                if path.is_socket():
                    raise ValueError("host Unix/daemon sockets must not enter container mounts")

    def command(self, command):
        """Use no host namespace, daemon socket, privileges or writable tools."""
        return self._container_command(command, confined=True)

    def _container_command(self, command, *, confined):
        for path in [self.private_root, *self.readonly]:
            if "," in str(path) or "\n" in str(path):
                raise ValueError("unsupported bind mount pathname")
        result = ["/usr/bin/docker", "run", "--rm", "--pull=never", "--platform=linux/amd64",
                  "--cap-drop", "ALL", "--security-opt", "no-new-privileges=true",
                  "--security-opt", f"seccomp={self.profile_path if confined else self.control_profile_path}",
                  "--read-only", "--ipc=private",
                  "--pid=private", "--user", "65532:65532", "--pids-limit", "512", "--memory", "6g",
                  "--cpus", "4", "--workdir", "/work", "--env", "HOME=/work/home", "--env", "TMPDIR=/work/tmp",
                  "--mount", f"type=bind,src={self.private_root},dst=/work"]
        if confined:
            result.extend(["--network", "none"])
        for path in self.readonly:
            result.extend(["--mount", f"type=bind,src={path},dst={path},readonly"])
        return [*result, self.image, *map(str, command)]


def probe_commands(boundary, private_root, python):
    """Construct controls in the same OS/image and map private container paths.

    No public denial toggle exists. Only this source-owned control orchestration
    invokes the internal baseline container command; compilation uses command().
    """
    root = Path(private_root).resolve(strict=True)
    python = Path(python)
    if isinstance(boundary, LinuxBoundary):
        # This identity is selected and measured inside the OCI builder. Resolving
        # a host Python symlink could select a different container executable.
        if not python.is_absolute() or ".." in python.parts:
            raise ValueError("explicit in-container prepared Python path required")
    else:
        python = python.resolve(strict=True)
    command = [str(python), "-I", "-B", str(Path(__file__).parent / "socket_probe.py"),
               "--private-root", "/work" if isinstance(boundary, LinuxBoundary) else str(root)]
    if isinstance(boundary, LinuxBoundary):
        if root != boundary.private_root:
            raise ValueError("probe workspace differs from selected private container mount")
        return {"unconfined": boundary._container_command(command, confined=False),
                "confined": boundary.command(command)}
    if not isinstance(boundary, MacBoundary):
        raise ValueError("unknown source-owned candidate boundary")
    return {"unconfined": command, "confined": boundary.command(command)}


def probe_controls(boundary, private_root, python):
    """Measure positive then denied probes with this same boundary instance.

    This local helper does not perform compilation or qualification. The shared
    staging/production orchestrator must run its full recipe through command().
    """
    root = Path(private_root).resolve(strict=True)
    python = Path(python)
    if not isinstance(boundary, LinuxBoundary) and not python.resolve(strict=True).is_file():
        raise ValueError("explicit prepared Python executable required")
    observations = {}
    for phase, argv in probe_commands(boundary, root, python).items():
        result = subprocess.run(argv, env={"HOME": str(root), "TMPDIR": str(root), "LANG": "C", "LC_ALL": "C"},
                                stdin=subprocess.DEVNULL, close_fds=True, capture_output=True,
                                timeout=30, check=False)
        if result.returncode or len(result.stdout) > 65536 or len(result.stderr) > 65536:
            raise ValueError(f"{phase} probe failed; actual local boundary unavailable")
        observations[phase] = load_json_bytes(result.stdout, max_bytes=65536)
    return observations
