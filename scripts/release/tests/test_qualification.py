"""Adversarial observational qualification fixtures; none confer release trust."""

import copy
import errno
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

RELEASE = Path(__file__).resolve().parents[1]
ROOT = RELEASE.parents[1]
sys.path.insert(0, str(RELEASE / "qualification"))
sys.path.insert(0, str(RELEASE))
if importlib.util.find_spec("qualify") is not None:
    import qualify
else:
    qualify = None


def observation():
    """Return fictional complete observations for semantic tests only."""
    digest = "a" * 64
    probes = []
    for depth in range(3):
        for family in ("ipv4", "ipv6"):
            for protocol in ("tcp", "udp"):
                for operation in ("bind", "connect", "send"):
                    for destination in ("loopback", "external"):
                        probes.append({"depth": depth, "family": family, "protocol": protocol,
                                       "operation": operation, "destination": destination,
                                       "outcome": "denied", "errno": errno.EPERM, "phase": "socket"})
        probes.append({"depth": depth, "family": "unix", "operation": "exchange",
                       "destination": "private", "outcome": "allowed", "errno": 0,
                       "phase": "exchange", "protocol": "unix"})
    positive = copy.deepcopy(probes)
    for probe in positive:
        probe.update(outcome="allowed", errno=0, phase=probe["operation"])
        if probe["destination"] == "external":
            probe.update(outcome="unavailable", errno=errno.ENETUNREACH)
    return {
        "schema_version": 1, "purpose": "disposable-rehearsal-observation",
        "source_commit": "a" * 40, "closure_sha256": digest,
        "selection_sha256": digest, "raw_tools_sha256": digest,
        "dependency_sha256": digest,
        "run": {"repository_id": "123", "owner_id": "456", "run_id": "789",
                "attempt": 1, "job": "macos-arm", "runner": "github-hosted"},
        "host": {"environment_id": "macos15-aarch64", "kind": "github-hosted-macos",
                 "architecture": "aarch64", "kernel_architecture": "aarch64",
                 "os_version": "15.7", "translated": False, "runner_image_version": "20261001.1",
                 "oci_index_sha256": None, "oci_manifest_sha256": None, "glibc_version": None,
                 "identity_log_sha256": digest},
        "sdk": {"kind": "apple-sdk", "version": "15.5", "build_version": "24F74",
                "tree_sha256": digest, "vendor_evidence_sha256": digest},
        "tools": [{"name": name, "version": "fixture", "file_sha256": digest,
                   "archive_sha256": digest, "tree_sha256": digest,
                   "authentication": {"method": "vendor-signature", "evidence_sha256": digest,
                                      "trust_root_sha256": digest}}
                  for name in ("python", "git", "shell", "rustup", "rustc", "cargo",
                               "compiler", "linker", "archiver", "node", "npm")],
        "recipe": {"id": "ts-aarch64-apple-darwin", "boundary_sha256": digest,
                   "preparation_log_sha256": digest, "compile_log_sha256": digest,
                   "native_log_sha256": digest, "exit_code": 0, "full_recipe": True,
                   "same_boundary": True, "executable_sha256": digest,
                   "native_materials_sha256": digest, "deployment_target": "13.0",
                   "glibc_symbols": [], "unconfined": positive, "confined": probes,
                   "download_attempts": {"cargo": "denied", "npm": "denied",
                                         "missing_tool": "failed"}},
        "runtime": [{"environment_id": "macos15-aarch64", "architecture": "aarch64",
                     "os_version": "15.7", "outside_checkout": True, "exit_code": 0,
                     "kind": "cli-startup", "executable_sha256": digest,
                     "executable_after_sha256": digest, "log_sha256": digest,
                     "response_sha256": None}],
    }


class QualificationTests(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(qualify, "should provide local qualification framework")
        self.result = observation()

    def test_complete_observation_is_structurally_valid_without_qualifying(self):
        qualify.validate_observation(self.result)
        with self.assertRaisesRegex(ValueError, "authenticated.*hosted|pending"):
            qualify.require_qualified(self.result)

    def test_rejects_wrong_architecture_and_translation(self):
        for field, value in (("architecture", "x86_64"), ("kernel_architecture", "x86_64"),
                             ("translated", True), ("os_version", "26.0")):
            with self.subTest(field=field):
                changed = copy.deepcopy(self.result)
                changed["host"][field] = value
                with self.assertRaises(ValueError):
                    qualify.validate_observation(changed)

    def test_rejects_missing_sdk_tool_and_authentication(self):
        for change in ("sdk", "tool", "missing", "checksum"):
            with self.subTest(change=change):
                changed = copy.deepcopy(self.result)
                if change == "sdk":
                    changed["sdk"]["tree_sha256"] = None
                elif change == "tool":
                    changed["tools"][0]["file_sha256"] = None
                elif change == "missing":
                    changed["tools"].pop()
                else:
                    changed["tools"][0]["authentication"]["method"] = "raw-checksum"
                with self.assertRaises(ValueError):
                    qualify.validate_observation(changed)

    def test_rejects_failed_startup_changed_bytes_and_health_only(self):
        for field, value in (("exit_code", 1), ("kind", "health"),
                             ("outside_checkout", False), ("executable_after_sha256", "b" * 64)):
            with self.subTest(field=field):
                changed = copy.deepcopy(self.result)
                changed["runtime"][0][field] = value
                with self.assertRaises(ValueError):
                    qualify.validate_observation(changed)

    def test_rejects_invented_compatibility_and_missing_actual_runtime(self):
        changed = copy.deepcopy(self.result)
        changed["runtime"] = []
        with self.assertRaises(ValueError):
            qualify.validate_observation(changed)
        self.result["runtime"][0]["os_version"] = "13.0"
        with self.assertRaises(ValueError):
            qualify.validate_observation(self.result)

    def test_rejects_permitted_confined_socket_and_missing_descendant(self):
        for change in ("allowed", "timeout", "missing", "unix"):
            with self.subTest(change=change):
                changed = copy.deepcopy(self.result)
                if change in ("allowed", "timeout"):
                    changed["recipe"]["confined"][0]["outcome"] = change
                elif change == "missing":
                    changed["recipe"]["confined"] = [p for p in changed["recipe"]["confined"]
                                                       if p["depth"] != 2]
                else:
                    changed["recipe"]["confined"][-1]["outcome"] = "denied"
                with self.assertRaises(ValueError):
                    qualify.validate_observation(changed)

    def test_rejects_failed_positive_control_and_incomplete_compilation(self):
        for field, value in (("exit_code", 1), ("full_recipe", False), ("same_boundary", False)):
            changed = copy.deepcopy(self.result)
            changed["recipe"][field] = value
            with self.assertRaises(ValueError):
                qualify.validate_observation(changed)
        self.result["recipe"]["unconfined"][0].update(outcome="denied", errno=1)
        with self.assertRaises(ValueError):
            qualify.validate_observation(self.result)

    def test_closed_schema_rejects_unknown_commands_boolean_integers_and_status(self):
        for changed in (dict(self.result, command="example-command"),
                        dict(self.result, qualification="passed"),
                        dict(self.result, schema_version=True)):
            with self.assertRaises(ValueError):
                qualify.validate_observation(changed)
        self.result["recipe"]["confined"][0]["depth"] = True
        with self.assertRaises(ValueError):
            qualify.validate_observation(self.result)

    def test_pending_catalog_distinguishes_sdk_and_runtime_identities(self):
        import preflight
        tools = preflight.fixed_tools()
        qualify.validate_catalog(tools)
        mac = next(e for e in tools["environments"] if e["id"] == "macos15-aarch64")
        self.assertEqual(mac["kind"], "github-hosted-macos")
        self.assertNotIn("image_sha256", mac, "should not label image-version hash as image bytes")
        runtime = next(e for e in tools["environments"] if e["id"] == "debian12-x86_64")
        self.assertEqual(runtime["role"], "compatibility-runtime")
        self.assertNotIn("sdk_sha256", runtime)

    def test_hashes_do_not_self_reference_derived_result_pins(self):
        import preflight
        tools = preflight.fixed_tools()
        first = qualify.selection_digest(tools)
        changed = copy.deepcopy(tools)
        changed["recipes"][0]["sha256"] = "a" * 64
        self.assertEqual(first, qualify.selection_digest(changed))
        changed["recipes"][0]["settings"]["release_debug"] = 2
        self.assertNotEqual(first, qualify.selection_digest(changed))
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "helper.py"
            path.write_bytes(b"fixture\n")
            first = qualify.closure_digest(Path(directory), ("helper.py",), {"mode": "fixture"})
            path.write_bytes(b"changed\n")
            self.assertNotEqual(first, qualify.closure_digest(Path(directory), ("helper.py",),
                                                            {"mode": "fixture"}))

    def test_real_unconfined_probe_covers_child_and_grandchild(self):
        probe = RELEASE / "qualification/socket_probe.py"
        with tempfile.TemporaryDirectory(prefix="ts-q-") as directory:
            result = subprocess.run([sys.executable, "-I", "-B", str(probe), "--private-root", directory],
                                    check=False, capture_output=True, timeout=30)
        self.assertEqual(result.returncode, 0, "should execute actual probes")
        probes = json.loads(result.stdout)
        self.assertEqual({p["depth"] for p in probes}, {0, 1, 2})
        self.assertEqual(len(probes), 75)
        self.assertEqual({p["protocol"] for p in probes}, {"tcp", "udp", "unix"})
        self.assertTrue(all(p["outcome"] == "allowed" for p in probes
                            if p["destination"] in ("loopback", "private") and p["family"] != "ipv6"))

    def test_full_recipe_gate_stops_before_creating_work(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "work"
            with self.assertRaisesRegex(ValueError, "Task 9|production recipe"):
                qualify.check_execution_prerequisites(ROOT, output)
            self.assertFalse(output.exists(), "should fail before materializing work")

    def test_linux_rejects_unknown_libc_and_above_floor_symbols(self):
        result = copy.deepcopy(self.result)
        result["host"].update(environment_id="gnu-builder-x86_64", kind="oci-builder",
                              architecture="x86_64", kernel_architecture="x86_64",
                              os_version="9.6", runner_image_version=None,
                              oci_index_sha256="a" * 64, oci_manifest_sha256="a" * 64,
                              glibc_version="2.34")
        result["sdk"]["kind"] = "gnu-sysroot"
        result["recipe"].update(id="ts-x86_64-unknown-linux-gnu", deployment_target=None,
                                glibc_symbols=["GLIBC_2.34"])
        result["runtime"] = [dict(self.result["runtime"][0], environment_id=identity,
                                  architecture="x86_64", os_version=version)
                             for identity, version in (("ubi9-x86_64", "9.6"),
                                                       ("ubuntu22.04-x86_64", "22.04"),
                                                       ("debian12-x86_64", "12.11"))]
        qualify.validate_observation(result)
        for version in ("2.35", "unknown", "2.34vendor"):
            changed = copy.deepcopy(result)
            changed["host"]["glibc_version"] = version
            with self.assertRaises(ValueError):
                qualify.validate_observation(changed)
        result["recipe"]["glibc_symbols"] = ["GLIBC_2.35"]
        with self.assertRaises(ValueError):
            qualify.validate_observation(result)

    def test_linux_rejects_claiming_new_runtime_as_old_baseline(self):
        result = copy.deepcopy(self.result)
        result["host"].update(environment_id="gnu-builder-x86_64", kind="oci-builder",
                              architecture="x86_64", kernel_architecture="x86_64",
                              os_version="9.6", runner_image_version=None,
                              oci_index_sha256="a" * 64, oci_manifest_sha256="a" * 64,
                              glibc_version="2.34")
        result["sdk"]["kind"] = "gnu-sysroot"
        result["recipe"].update(id="ts-x86_64-unknown-linux-gnu", deployment_target=None,
                                glibc_symbols=["GLIBC_2.34"])
        result["runtime"] = [dict(self.result["runtime"][0], environment_id=identity,
                                  architecture="x86_64", os_version="26.0")
                             for identity in ("ubi9-x86_64", "ubuntu22.04-x86_64", "debian12-x86_64")]
        with self.assertRaisesRegex(ValueError, "runtime|compatibility"):
            qualify.validate_observation(result)

    def test_complete_fastly_publisher_observation_requires_runtime_tools_and_response(self):
        result = copy.deepcopy(self.result)
        result["host"].update(environment_id="gnu-builder-x86_64", kind="oci-builder",
                              architecture="x86_64", kernel_architecture="x86_64",
                              os_version="9.6", runner_image_version=None,
                              oci_index_sha256="a" * 64, oci_manifest_sha256="a" * 64,
                              glibc_version="2.34")
        result["sdk"]["kind"] = "gnu-sysroot"
        result["recipe"].update(id="trusted-server-fastly.wasm", deployment_target=None)
        result["tools"].extend(dict(result["tools"][0], name=name) for name in ("viceroy", "wasmtime"))
        result["runtime"][0].update(environment_id="fastly-viceroy", architecture="wasm32",
                                    os_version="fixture", kind="publisher-request", response_sha256="a" * 64)
        qualify.validate_observation(result)
        for field, value in (("kind", "cli-startup"), ("response_sha256", None)):
            changed = copy.deepcopy(result)
            changed["runtime"][0][field] = value
            with self.assertRaises(ValueError):
                qualify.validate_observation(changed)

    def test_selection_digest_binds_acquisition_authentication_roots(self):
        import preflight
        tools = preflight.fixed_tools()
        digest = qualify.selection_digest(tools)
        tools["tools"][0]["acquisition"] = {"trust_root_sha256": "a" * 64}
        self.assertNotEqual(digest, qualify.selection_digest(tools),
                            "should invalidate qualification when acquisition trust changes")

    def test_source_owned_complete_closure_refuses_missing_recipe_files(self):
        self.assertTrue(hasattr(qualify, "candidate_fingerprints"), "should define reviewed closure")
        import preflight
        with self.assertRaisesRegex(ValueError, "Task 9|missing.*closure"):
            qualify.candidate_fingerprints(ROOT, preflight.fixed_tools(), "ts-aarch64-apple-darwin")

    def test_linux_profile_restricts_socket_domains_on_pinned_default_baseline(self):
        self.assertIsNotNone(importlib.util.find_spec("boundaries"), "should provide real profiles")
        import boundaries
        profile = boundaries.linux_seccomp_profile()
        self.assertEqual(profile["defaultAction"], "SCMP_ACT_ERRNO")
        rules = [r for r in profile["syscalls"] if set(r["names"]) & {"socket", "socketpair", "socketcall"}]
        self.assertEqual(len(rules), 1)
        self.assertEqual(set(rules[0]["names"]), {"socket", "socketpair"})
        self.assertEqual(rules[0]["args"], [{"index": 0, "value": 1, "op": "SCMP_CMP_EQ"}])

    def test_linux_boundary_drops_caps_and_uses_private_mounts(self):
        self.assertIsNotNone(importlib.util.find_spec("boundaries"), "should provide real profiles")
        import boundaries
        with tempfile.TemporaryDirectory() as directory:
            private = Path(directory) / "private"
            readonly = Path(directory) / "readonly"
            private.mkdir()
            readonly.mkdir()
            boundary = boundaries.LinuxBoundary(private, [readonly],
                                                "example.com/builder@sha256:" + "a" * 64)
            command = boundary.command(["/usr/bin/python3", "-I", "-B", "/work/probe.py"])
            self.assertIn("none", command)
            self.assertIn("ALL", command)
            self.assertIn("no-new-privileges=true", command)
            self.assertIn("--read-only", command)
            self.assertIn("--ipc=private", command)
            self.assertIn("--pid=private", command)
            self.assertNotIn("--privileged", command)
            self.assertTrue(any("readonly" in arg for arg in command if arg.startswith("type=bind")))
            with self.assertRaises(ValueError):
                boundaries.LinuxBoundary(private, [Path("/")], "example.com/builder:latest")

    def test_linux_control_uses_same_container_and_maps_private_root(self):
        import boundaries
        with tempfile.TemporaryDirectory() as directory:
            private = Path(directory) / "private"
            readonly = Path(directory) / "readonly"
            private.mkdir()
            readonly.mkdir()
            boundary = boundaries.LinuxBoundary(private, [readonly],
                                                "example.com/builder@sha256:" + "a" * 64)
            commands = boundaries.probe_commands(boundary, private, Path("/usr/bin/python3"))
            for phase in ("unconfined", "confined"):
                self.assertEqual(commands[phase][0], "/usr/bin/docker")
                self.assertIn(boundary.image, commands[phase])
                self.assertEqual(commands[phase][-1], "/work")
            self.assertIn("none", commands["confined"])
            self.assertNotIn("none", commands["unconfined"])
            self.assertTrue(any("control-seccomp.json" in item for item in commands["unconfined"]))

    def test_closure_includes_transitive_source_owned_build_helpers(self):
        required = {
            "crates/trusted-server-js/build.rs", "crates/trusted-server-js/build/prebuilt.rs",
            "crates/trusted-server-js/build/inventory.rs", "crates/trusted-server-js/build/bundle_set.rs",
            "crates/trusted-server-core/build.rs", "crates/trusted-server-core/build_support/git_version.rs",
            "crates/trusted-server-cli/build.rs", "crates/trusted-server-adapter-fastly/build.rs",
            "crates/trusted-server-js/lib/build-all.mjs", "crates/trusted-server-js/lib/build-module-options.mjs",
            "crates/trusted-server-js/lib/release-composition.mjs", "crates/trusted-server-js/lib/vitest.config.ts",
        }
        self.assertTrue(required.issubset(set(qualify.SHARED_CLOSURE)),
                        "should invalidate qualification when any source-owned build helper changes")

    def test_socket_failure_phase_and_errno_cannot_masquerade_as_denial(self):
        for phase, outcome, error in (("exchange", "denied", errno.EPERM),
                                     ("socket", "denied", errno.ENETUNREACH)):
            changed = copy.deepcopy(self.result)
            changed["recipe"]["confined"][0].update(phase=phase, outcome=outcome, errno=error)
            with self.assertRaises(ValueError):
                qualify.validate_observation(changed)
        self.result["recipe"]["unconfined"][1].update(outcome="unavailable", errno=0)
        with self.assertRaises(ValueError):
            qualify.validate_observation(self.result)

    def test_successful_socket_controls_must_complete_the_intended_operation(self):
        for phase, index, wrong_phase in (("unconfined", 0, "exchange"),
                                           ("unconfined", 2, "bind"),
                                           ("unconfined", 4, "connect"),
                                           ("unconfined", 24, "bind"),
                                           ("confined", 24, "bind")):
            with self.subTest(phase=phase, index=index, wrong_phase=wrong_phase):
                changed = copy.deepcopy(self.result)
                changed["recipe"][phase][index]["phase"] = wrong_phase
                with self.assertRaisesRegex(ValueError, "operation|exchange"):
                    qualify.validate_observation(changed)

    def test_tcp_send_denial_can_record_an_earlier_actual_client_phase(self):
        for phase in ("socket", "connect", "send"):
            with self.subTest(phase=phase):
                changed = copy.deepcopy(self.result)
                changed["recipe"]["confined"][4]["phase"] = phase
                qualify.validate_observation(changed)

    def test_major_debian_and_zero_suffixed_glibc_preserve_numeric_floors(self):
        result = copy.deepcopy(self.result)
        result["host"].update(environment_id="gnu-builder-x86_64", kind="oci-builder",
                              architecture="x86_64", kernel_architecture="x86_64",
                              os_version="9.6", runner_image_version=None,
                              oci_index_sha256="a" * 64, oci_manifest_sha256="a" * 64,
                              glibc_version="2.34.0")
        result["sdk"]["kind"] = "gnu-sysroot"
        result["recipe"].update(id="ts-x86_64-unknown-linux-gnu", deployment_target=None,
                                glibc_symbols=["GLIBC_2.34.0"])
        result["runtime"] = [dict(self.result["runtime"][0], environment_id=identity,
                                  architecture="x86_64", os_version=version)
                             for identity, version in (("ubi9-x86_64", "9.6"),
                                                       ("ubuntu22.04-x86_64", "22.04"),
                                                       ("debian12-x86_64", "12"))]
        for version in ("2.34", "2.34.0"):
            with self.subTest(valid_glibc=version):
                changed = copy.deepcopy(result)
                changed["host"]["glibc_version"] = version
                changed["recipe"]["glibc_symbols"] = [f"GLIBC_{version}"]
                qualify.validate_observation(changed)
        for value in ("2.34.1", "2.35.0", "unknown", "2.34vendor"):
            with self.subTest(glibc=value):
                changed = copy.deepcopy(result)
                changed["host"]["glibc_version"] = value
                with self.assertRaises(ValueError):
                    qualify.validate_observation(changed)
        for value in ("11", "13", "12vendor"):
            with self.subTest(debian=value):
                changed = copy.deepcopy(result)
                changed["runtime"][-1]["os_version"] = value
                with self.assertRaises(ValueError):
                    qualify.validate_observation(changed)

    def test_rejects_integral_floats_for_integer_identities_and_statuses(self):
        for selector in ("schema", "attempt", "depth", "errno", "compile", "runtime"):
            with self.subTest(selector=selector):
                changed = copy.deepcopy(self.result)
                if selector == "schema":
                    changed["schema_version"] = 1.0
                elif selector == "attempt":
                    changed["run"]["attempt"] = 1.0
                elif selector in {"depth", "errno"}:
                    changed["recipe"]["confined"][0][selector] = 0.0 if selector == "depth" else 1.0
                elif selector == "compile":
                    changed["recipe"]["exit_code"] = 0.0
                else:
                    changed["runtime"][0]["exit_code"] = 0.0
                with self.assertRaises(ValueError):
                    qualify.validate_observation(changed)

    def test_container_mounts_reject_symlink_directory_escape(self):
        import boundaries
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            private, readonly, outside = (root / name for name in ("private", "readonly", "outside"))
            for path in (private, readonly, outside):
                path.mkdir()
            (readonly / "escape").symlink_to(outside, target_is_directory=True)
            with self.assertRaisesRegex(ValueError, "symlink|escape"):
                boundaries.LinuxBoundary(private, [readonly], "example.com/builder@sha256:" + "a" * 64)

    def test_recipe_closure_rejects_internal_symlink_parent(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            actual = root / "actual"
            actual.mkdir()
            (actual / "helper.py").write_text("fixture\n")
            (root / "alias").symlink_to(actual, target_is_directory=True)
            with self.assertRaises(ValueError):
                qualify.closure_digest(root, ("alias/helper.py",), {"mode": "fixture"})

    def test_macos_profile_rejects_shared_write_roots_and_has_private_unix_only(self):
        self.assertIsNotNone(importlib.util.find_spec("boundaries"), "should provide real profiles")
        import boundaries
        with tempfile.TemporaryDirectory() as directory:
            private = Path(directory) / "private"
            readonly = Path(directory) / "readonly"
            private.mkdir()
            readonly.mkdir()
            profile = boundaries.macos_profile(private, [readonly])
            self.assertIn("(deny default)", profile)
            self.assertIn("(local unix-socket", profile)
            self.assertIn("(remote unix-socket", profile)
            self.assertNotIn("(allow network*)", profile)
            self.assertNotIn("mach-lookup", profile)
            self.assertNotIn("appleevent", profile)
            with self.assertRaises(ValueError):
                boundaries.macos_profile(private, [Path(directory)])

    def test_macos_profile_protects_tracked_source_and_git_with_private_install_writes(self):
        import boundaries
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            manifest = root / "package.json"
            objects = root / "git-objects"
            manifest.write_text("{}\n")
            objects.mkdir()
            profile = boundaries.macos_profile(root, [], immutable=[manifest, objects])
            self.assertIn(f'(deny file-write* (literal "{manifest}"))', profile)
            self.assertIn(f'(deny file-write* (subpath "{objects}"))', profile)

    @unittest.skipUnless(os.environ.get("TS_RELEASE_BOUNDARY_TESTS") == "1" and sys.platform == "darwin",
                         "explicit actual macOS ancestor alias protection probe")
    def test_actual_immutable_ancestors_deny_rename_removal_and_hardlink_aliases(self):
        import boundaries
        with tempfile.TemporaryDirectory(prefix="ts-q-") as directory:
            root = Path(directory).resolve()
            source = root / "crates/source"
            source.mkdir(parents=True)
            manifest = source / "Cargo.toml"
            manifest.write_text("fixture\n")
            objects = source / "git/objects"
            objects.mkdir(parents=True)
            (source / "node_modules").mkdir()
            (source / "output").mkdir()
            python = Path(sys.executable).resolve()
            boundary = boundaries.MacBoundary(root, [python.parents[1], Path("/usr/lib"),
                                                       Path("/System/Library")], immutable=[manifest, objects])
            script = """import errno, os, pathlib, sys
root = pathlib.Path(sys.argv[1])
source = root / 'crates/source'
for ancestor in (root / 'crates', source, source / 'git'):
    moved = root / 'moved'
    try:
        ancestor.rename(moved)
    except OSError as error:
        if error.errno not in (errno.EACCES, errno.EPERM):
            raise
    else:
        manifest = moved / ('source/Cargo.toml' if ancestor == root / 'crates' else 'Cargo.toml')
        if manifest.is_file():
            manifest.write_text('mutation-via-ancestor-alias')
        moved.rename(ancestor)
        sys.exit('immutable ancestor rename was permitted')
    try:
        ancestor.rmdir()
    except OSError as error:
        if error.errno not in (errno.EACCES, errno.EPERM):
            raise
    else:
        sys.exit('immutable ancestor removal was permitted')
try:
    os.link(source / 'Cargo.toml', root / 'hardlink-alias')
except OSError as error:
    if error.errno not in (errno.EACCES, errno.EPERM):
        raise
else:
    sys.exit('immutable hard-link alias was permitted')
(source / 'node_modules/generated').write_text('private-install-output')
(source / 'output/generated').write_text('private-build-output')
"""
            result = subprocess.run(boundary.command([str(python), "-I", "-B", "-c", script, str(root)]),
                                    env={"HOME": str(root), "TMPDIR": str(root)}, stdin=subprocess.DEVNULL,
                                    capture_output=True, close_fds=True, timeout=30, check=False)
            self.assertEqual(result.returncode, 0,
                             "should deny ancestor aliases/removal and permit private writes: "
                             + result.stderr.decode())
            self.assertEqual(manifest.read_text(), "fixture\n", "should retain original tracked source")
            self.assertEqual((source / "node_modules/generated").read_text(), "private-install-output")
            self.assertEqual((source / "output/generated").read_text(), "private-build-output")

    @unittest.skipUnless(os.environ.get("TS_RELEASE_BOUNDARY_TESTS") == "1" and sys.platform == "darwin",
                         "explicit actual local macOS immutable source probe")
    def test_actual_source_writes_denied_while_node_modules_writes_allowed(self):
        import boundaries
        with tempfile.TemporaryDirectory(prefix="ts-q-") as directory:
            root = Path(directory).resolve()
            manifest = root / "package.json"
            objects = root / "git-objects"
            manifest.write_text("{}\n")
            objects.mkdir()
            node_modules = root / "node_modules"
            node_modules.mkdir()
            python = Path(sys.executable).resolve()
            boundary = boundaries.MacBoundary(root, [python.parents[1], Path("/usr/lib"),
                                                       Path("/System/Library")], immutable=[manifest, objects])
            script = """import errno, pathlib, sys
root = pathlib.Path(sys.argv[1])
for path in (root / 'package.json', root / 'git-objects' / 'new-object'):
    try:
        path.write_text('mutation')
    except OSError as error:
        if error.errno not in (errno.EACCES, errno.EPERM):
            raise
    else:
        sys.exit('immutable source was writable')
(root / 'node_modules' / 'generated').write_text('private-install-output')
"""
            result = subprocess.run(boundary.command([str(python), "-I", "-B", "-c", script, str(root)]),
                                    env={"HOME": str(root), "TMPDIR": str(root)}, stdin=subprocess.DEVNULL,
                                    capture_output=True, close_fds=True, timeout=30, check=False)
            self.assertEqual(result.returncode, 0, "should permit only private installation writes")
            self.assertEqual(manifest.read_text(), "{}\n")
            self.assertEqual((node_modules / "generated").read_text(), "private-install-output")

    @unittest.skipUnless(os.environ.get("TS_RELEASE_BOUNDARY_TESTS") == "1" and sys.platform == "darwin",
                         "explicit actual local macOS profile probe")
    def test_actual_local_mac_profile_denies_descendants_and_permits_private_unix(self):
        self.assertIsNotNone(importlib.util.find_spec("boundaries"), "should provide real profiles")
        import boundaries
        with tempfile.TemporaryDirectory(prefix="ts-q-") as directory:
            root = Path(directory).resolve()
            readonly = [RELEASE, Path(sys.executable).resolve().parents[1],
                        Path("/usr/lib"), Path("/System/Library")]
            boundary = boundaries.MacBoundary(root, readonly)
            probes = boundaries.probe_controls(boundary, root, Path(sys.executable).resolve())
            qualify.validate_socket_pair(probes["unconfined"], probes["confined"])


if __name__ == "__main__":
    unittest.main()
