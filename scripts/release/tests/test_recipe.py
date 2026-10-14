"""Closed canonical recipe and honest role observation contract fixtures."""
import copy
import contextlib
import io
import os
import platform
import importlib.util
import sys
import subprocess
import signal
import time
import tempfile
import unittest
from pathlib import Path

RELEASE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(RELEASE))
recipe = None
if importlib.util.find_spec("rust_recipe") is not None:
    import rust_recipe as recipe


class RecipeTests(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(recipe, "canonical Rust recipe is missing")

    def test_unknown_asset_and_variant(self):
        for name in ("other.wasm", "trusted-server-fastly.wasm:reusable-sandbox"):
            with self.assertRaisesRegex(ValueError, "asset"):
                recipe.artifact_selection(name)

    def test_closed_arguments(self):
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            recipe.parse_request(["--variant", "default"])

    def test_fresh_output(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(ValueError, "fresh"):
                recipe.fresh_root(Path(directory))

    def test_fixed_cargo_selection(self):
        artifact = recipe.artifact_selection("trusted-server-fastly.wasm")
        command = recipe.cargo_command(Path("/authenticated/cargo"), artifact, Path("/private/target"), Path("/private/config.toml"))
        self.assertIn("--frozen", command)
        self.assertIn("--message-format=json-render-diagnostics", command)
        self.assertIn("--release", command)
        self.assertNotIn("--features", command)
        self.assertNotIn("sourcebuild", command)

    def test_role_specific_profiles(self):
        a, b = recipe.ambient_profile("A"), recipe.ambient_profile("B")
        self.assertEqual((a["umask"], b["umask"]), ("022", "027"))
        self.assertEqual((a["jobs"], b["jobs"]), (4, 2))
        self.assertNotEqual(a["checkout_root"], b["checkout_root"])
        self.assertEqual(b["locale"], "qualification-selected-alternate")

    def observation(self, role):
        return {"role": role, "checkout_root": "/external/" + role,
                "umask": "022" if role == "A" else "027", "jobs": 4 if role == "A" else 2,
                "locale": "C" if role == "A" else "en_US.UTF-8", "timezone": "UTC" if role == "A" else "Asia/Kolkata",
                "start_time_ns": 1 if role == "A" else 2, "execution_user": "65532",
                "user_variation": "unsupported", "normalized": {"locale": "C", "timezone": "UTC", "epoch": 123, "source_root": "/source"}}

    def test_missing_selected_b_variation(self):
        a, b = self.observation("A"), self.observation("B")
        b["locale"] = "C"
        with self.assertRaisesRegex(ValueError, "variation"):
            recipe.validate_pair_observations(a, b, {"locale": "en_US.UTF-8", "timezone": "Asia/Kolkata", "user_variation": False})

    def test_normalized_values_are_not_reported_varied(self):
        a, b = self.observation("A"), self.observation("B")
        b["normalized"]["epoch"] = 456
        with self.assertRaisesRegex(ValueError, "normalized"):
            recipe.validate_pair_observations(a, b, {"locale": "en_US.UTF-8", "timezone": "Asia/Kolkata", "user_variation": False})

    def test_unsupported_user_variation_is_not_passed(self):
        a, b = self.observation("A"), self.observation("B")
        b["user_variation"] = "passed"
        with self.assertRaisesRegex(ValueError, "user"):
            recipe.validate_pair_observations(a, b, {"locale": "en_US.UTF-8", "timezone": "Asia/Kolkata", "user_variation": False})

    def test_incorrect_observed_concurrency_and_umask(self):
        for field, value in (("jobs", 4), ("umask", "022")):
            a, b = self.observation("A"), self.observation("B")
            b[field] = value
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, field):
                recipe.validate_pair_observations(a, b, {"locale": "en_US.UTF-8", "timezone": "Asia/Kolkata", "user_variation": False})

    def test_official_selector_refuses_pending(self):
        with self.assertRaisesRegex(ValueError, "qualification pending"):
            recipe.qualified_selection(recipe.artifact_selection("trusted-server-fastly.wasm"))

class RecipeEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(recipe, "canonical Rust recipe is missing")

    def test_separate_cargo_stdout_and_diagnostic_stderr(self):
        with tempfile.TemporaryDirectory() as directory:
            stdout, stderr = recipe.capture([sys.executable, "-I", "-c", "import sys;sys.stdout.write('json\\n');sys.stderr.write('diagnostic\\n')"],
                                            {}, Path(directory), Path(directory) / "cargo", timeout=10)
            self.assertEqual(stdout, b"json\n")
            self.assertEqual(stderr, b"diagnostic\n")
            self.assertEqual((Path(directory) / "cargo.stdout").read_bytes(), stdout)

    def test_capture_bound_rejects_output_without_hanging(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(ValueError, "bound"):
                recipe.capture([sys.executable, "-I", "-c", "print('x'*2000)"], {}, Path(directory), Path(directory) / "cargo", timeout=10, maximum=1000)

    def test_mutated_material_tree_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "material").write_text("original")
            expected = recipe.tree_digest(root)
            (root / "material").write_text("mutated")
            with self.assertRaisesRegex(ValueError, "material"):
                recipe.check_tree(root, expected)

    def test_duplicate_or_escaped_cargo_output_rejected(self):
        import json
        artifact = recipe.artifact_selection("ts-x86_64-unknown-linux-gnu")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            final = root / "x86_64-unknown-linux-gnu/release/ts"
            final.parent.mkdir(parents=True)
            final.write_bytes(b"fixture")
            event = {"reason": "compiler-artifact", "package_id": "path+file:///source/crates/trusted-server-cli#0.1.0", "target": {"name": "ts", "kind": ["bin"]}, "features": [], "profile": {"test": False, "opt_level": "3", "debuginfo": 1}, "executable": str(final)}
            stream = json.dumps(event).encode() + b"\n"
            self.assertEqual(recipe.final_cargo_artifact(stream, artifact, root), final)
            with self.assertRaisesRegex(ValueError, "one"):
                recipe.final_cargo_artifact(stream + stream, artifact, root)
            event["executable"] = str(root / "other")
            with self.assertRaisesRegex(ValueError, "output"):
                recipe.final_cargo_artifact(json.dumps(event).encode() + b"\n", artifact, root)

    def test_git_index_cannot_supply_commands(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            lock = b'version = 4\n[[package]]\nname="example"\nversion="1.0.0"\nsource="' + recipe.CARGO_REGISTRY_SOURCE.encode() + b'"\nchecksum="' + b'a' * 64 + b'"\n'
            with self.assertRaisesRegex(ValueError, "index"):
                recipe.verify_cargo_materials(lock, {"schema_version": 1, "registry": [], "git": [], "command": "echo example"}, root)

class BoundaryContextTests(unittest.TestCase):
    def test_linux_same_boundary_maps_cwd_environment_and_readonly_source(self):
        sys.path.insert(0, str(RELEASE / "qualification"))
        from boundaries import LinuxBoundary
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            work = root / "work"
            work.mkdir()
            source = work / "source"
            source.mkdir()
            tools = root / "tools"
            tools.mkdir()
            boundary = LinuxBoundary(work, [tools], "example/builder@sha256:" + "a" * 64, immutable=[source])
            command = boundary.command([str(tools / "cargo"), "build"], environment={"HOME": str(work / "home"), "RUSTFLAGS": "--remap-path-prefix=" + str(source) + "=/source"}, cwd=source)
            self.assertIn("/work/source", command)
            self.assertIn("HOME=/work/home", command)
            self.assertIn("RUSTFLAGS=--remap-path-prefix=/work/source=/source", command)
            self.assertIn(f"type=bind,src={source},dst=/work/source,readonly", command)
            self.assertIn("--network", command)
            self.assertEqual(command[command.index("--network") + 1], "none")

class ProductionPathTests(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(recipe, "canonical Rust recipe is missing")

    def test_private_candidate_selection_rejects_missing_authentication(self):
        with self.assertRaisesRegex(ValueError, "candidate acquisition"):
            recipe.candidate_selection("ts-x86_64-unknown-linux-gnu", "a" * 40)

    def test_recipe_vendor_config_matches_actual_boundary_namespace(self):
        import tomllib
        sys.path.insert(0, str(RELEASE / "qualification"))
        from boundaries import LinuxBoundary, MacBoundary
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            vendor = root / "vendor"
            vendor.mkdir()
            configuration = root / "cargo-release.toml"
            configuration.touch()
            linux = LinuxBoundary(root, [], "example/builder@sha256:" + "a" * 64, immutable=[vendor, configuration])
            lock = b'version=4\n[[package]]\nname="example"\nversion="1.0"\n'
            recipe.write_vendor_configuration(lock, vendor, configuration, linux)
            configured = tomllib.loads(configuration.read_text())["source"]["release-vendor"]["directory"]
            self.assertEqual(configured, "/work/vendor")
            argv = linux.command(["/selected/cargo", "build", "--config", str(configuration)])
            self.assertEqual(argv[-1], "/work/cargo-release.toml")
            self.assertIn(f"type=bind,src={vendor},dst={configured},readonly", argv)
            if sys.platform == "darwin":
                mac = MacBoundary(root, [], immutable=[vendor, configuration])
                recipe.write_vendor_configuration(lock, vendor, configuration, mac)
                self.assertEqual(tomllib.loads(configuration.read_text())["source"]["release-vendor"]["directory"], str(vendor))

    def test_generated_config_is_source_owned_not_cargo_stdout(self):
        lock = b'version=4\n[[package]]\nname="example"\nversion="1.0"\nsource="' + recipe.CARGO_REGISTRY_SOURCE.encode() + b'"\nchecksum="' + b'a' * 64 + b'"\n'
        data = recipe.vendor_configuration(lock, Path("/private/vendor"))
        import tomllib
        config = tomllib.loads(data.decode())
        self.assertEqual(config["source"]["crates-io"]["replace-with"], "release-vendor")
        self.assertEqual(config["source"]["release-vendor"]["directory"], "/private/vendor")
        self.assertTrue(config["net"]["offline"])

    def test_fastly_pair_requires_complete_equality(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            a, b = root / "A", root / "B"
            a.write_bytes(b"example A")
            b.write_bytes(b"example B")
            with self.assertRaisesRegex(ValueError, "Fastly.*differ"):
                recipe.compare_final_bytes(a, b, recipe.artifact_selection("trusted-server-fastly.wasm"))
            measurement = recipe.compare_final_bytes(a, b, recipe.artifact_selection("ts-x86_64-unknown-linux-gnu"))
            self.assertFalse(measurement["equal"])
            self.assertFalse(measurement["blocking"])

class ActualLocalCargoTests(unittest.TestCase):
    @unittest.skipUnless(os.environ.get("TS_RELEASE_BOUNDARY_TESTS") == "1" and sys.platform == "darwin", "explicit actual local macOS Cargo confinement fixture only")
    def test_actual_cargo_build_script_download_is_denied_in_compile_boundary(self):
        self.assertIsNotNone(recipe, "canonical Rust recipe is missing")
        sys.path.insert(0, str(RELEASE / "qualification"))
        from boundaries import MacBoundary
        toolchain = Path.home() / ".rustup/toolchains" / ("1.95.0-" + ("aarch64" if platform.machine() == "arm64" else platform.machine()) + "-apple-darwin")
        self.assertTrue(toolchain.is_dir(), "explicit local Rust fixture toolchain is required")
        with tempfile.TemporaryDirectory() as directory:
            workspace = Path(directory).resolve()
            fixture = recipe.prepare_download_fixture(workspace)
            for name in ("home", "cargo-home", "tmp"):
                (workspace / name).mkdir()
            readonly = [Path("/bin"), Path("/usr/bin"), Path("/usr/lib"), Path("/System"),
                        Path("/Library/Developer/CommandLineTools"), Path(sys.base_prefix), toolchain, Path("/private/etc/ssl/openssl.cnf")]
            ssl_configuration = Path("/private/etc/ssl/openssl.cnf")
            ssl_before = recipe.tool_file_sha256(ssl_configuration)
            boundary = MacBoundary(workspace, readonly, immutable=[fixture])
            tools = {"cargo": str(toolchain / "bin/cargo"), "rustc": str(toolchain / "bin/rustc"), "python": str(Path(sys.executable).resolve())}
            environment = {"HOME": str(workspace / "home"), "CARGO_HOME": str(workspace / "cargo-home"),
                           "TMPDIR": str(workspace / "tmp"), "PATH": str(toolchain / "bin") + ":/usr/bin:/bin",
                           "RUSTC": tools["rustc"], "CARGO_BUILD_JOBS": "4", "CC": "/Library/Developer/CommandLineTools/usr/bin/clang",
                           "CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER": "/Library/Developer/CommandLineTools/usr/bin/clang",
                           "DEVELOPER_DIR": "/Library/Developer/CommandLineTools", "LANG": "C", "LC_ALL": "C", "TZ": "UTC",
                           "SDKROOT": "/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk", "MACOSX_DEPLOYMENT_TARGET": "13.0"}
            observed = recipe.verify_download_fixture(boundary, workspace, tools, environment)
            self.assertEqual(observed["unconfined"]["outcome"], "allowed")
            self.assertEqual(observed["confined"]["outcome"], "denied")
            self.assertIn(observed["confined"]["errno"], (1, 13))
            self.assertTrue(observed["actual_build_script"])
            self.assertEqual(observed["unconfined"]["jobs"], 4)
            self.assertEqual(observed["confined"]["jobs"], 4)
            self.assertEqual(recipe.tool_file_sha256(ssl_configuration), ssl_before)
            (workspace / "local-fixture-inputs.json").write_bytes(recipe.deterministic_json_bytes({"purpose": "unqualified-local-confinement-fixture", "openssl_config_sha256": ssl_before, "boundary_sha256": boundary.sha256}))

class RecordedContractTests(unittest.TestCase):
    def test_catalog_records_source_owned_ambient_roles(self):
        import preflight
        self.assertEqual(preflight.recipe_settings()["ambient_profiles"], recipe.PROFILES)

    def test_every_executable_helper_is_in_qualified_closure(self):
        sys.path.insert(0, str(RELEASE / "qualification"))
        import qualify
        self.assertIn("scripts/release/rust_recipe.py", qualify.RUST_CLOSURE)

class EffectiveSettingsTests(unittest.TestCase):
    def test_linux_execution_umask_applies_inside_container(self):
        sys.path.insert(0, str(RELEASE / "qualification"))
        from boundaries import LinuxBoundary
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            private, tools = root / "private", root / "tools"
            private.mkdir()
            tools.mkdir()
            boundary = LinuxBoundary(private, [tools], "example/builder@sha256:" + "a" * 64,
                                     python=tools / "python", execution_umask=0o027)
            command = boundary.command([str(tools / "cargo"), "build"])
            image = command.index(boundary.image)
            self.assertEqual(command[image + 1], str(tools / "python"))
            self.assertIn("os.umask(23)", command[image + 5])
            self.assertEqual(command[-2:], [str(tools / "cargo"), "build"])


class FinalSnapshotTests(unittest.TestCase):
    def test_final_snapshot_retains_exact_bytes_and_checks_mutation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            selected = root / "cargo-output"
            selected.write_bytes(b"selected final example")
            sealed = root / "accepted-final"
            sealed.mkdir()
            final = recipe.snapshot_final(selected, sealed, "ts")
            selected.write_bytes(b"changed cargo output")
            self.assertEqual(final.read_bytes(), b"selected final example")
            with self.assertRaisesRegex(ValueError, "fresh"):
                recipe.snapshot_final(selected, sealed, "ts")

    def test_whitespace_cannot_inject_compiler_path_flags(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(ValueError, "path"):
                recipe.fresh_root(Path(directory).resolve() / "material -Dexample")

class BoundedBoundaryProcessTests(unittest.TestCase):
    @unittest.skipUnless(os.name == "posix", "process-group cleanup uses POSIX semantics")
    def test_timeout_kills_child_holding_pipes_after_leader_exits(self):
        from boundaries import bounded_process
        with tempfile.TemporaryDirectory() as directory:
            identity = Path(directory) / "fixture-pids"
            script = "import os,subprocess,sys;from pathlib import Path;child=subprocess.Popen([sys.executable,'-I','-c','import time;time.sleep(30)']);Path(sys.argv[1]).write_text(str(os.getpid())+' '+str(child.pid))"
            leader = child = None
            def alive(pid):
                try:
                    os.kill(pid, 0)
                    return True
                except ProcessLookupError:
                    return False
            try:
                started = time.monotonic()
                with self.assertRaisesRegex(ValueError, "timeout"):
                    bounded_process([sys.executable, "-I", "-B", "-c", script, str(identity)], {}, timeout=0.5)
                self.assertLess(time.monotonic() - started, 3, "timeout should remain bounded")
                leader, child = map(int, identity.read_text().split())
                self.assertFalse(alive(leader), "direct child should be reaped")
                deadline = time.monotonic() + 2
                while alive(child) and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertFalse(alive(child), "descendant holding pipes should terminate with its process group")
            finally:
                if identity.exists() and child is None:
                    leader, child = map(int, identity.read_text().split())
                if child is not None and alive(child):
                    self.assertEqual(os.getpgid(child), leader, "cleanup should target only this fixture's process group")
                    os.killpg(leader, signal.SIGKILL)
                    deadline = time.monotonic() + 5
                    while alive(child) and time.monotonic() < deadline:
                        time.sleep(0.01)
                    self.assertFalse(alive(child), "fixture cleanup should leave no descendant running")

    def test_control_and_cleanup_capture_stop_live_output_overflow(self):
        sys.path.insert(0, str(RELEASE / "qualification"))
        from boundaries import bounded_process
        with self.assertRaisesRegex(ValueError, "output bound"):
            bounded_process([sys.executable, "-I", "-c", "import sys,time;sys.stdout.write('x'*100000);sys.stdout.flush();time.sleep(30)"], {}, maximum=1024, timeout=5)


def native_fixture_source(root):
    source = root / "source"
    manifest = source / "crates/trusted-server-cli/Cargo.toml"
    manifest.parent.mkdir(parents=True)
    (source / "Cargo.toml").write_text('[workspace.package]\nversion="1.2.3"\n')
    manifest.write_text('[package]\nname="trusted-server-cli"\nversion={workspace=true}\n')
    scaffold = b'[publisher]\ndomain="publisher.example.com"\n'
    (source / "trusted-server.example.toml").write_bytes(scaffold)
    return source, scaffold


class NativeCommandContractTests(unittest.TestCase):
    def test_source_owned_version_and_exact_embedded_scaffold_are_required(self):
        with tempfile.TemporaryDirectory() as directory:
            source, scaffold = native_fixture_source(Path(directory))
            contract = recipe.native_cli_contract(source, "a" * 40)
            version = b"ts 1.2.3 (" + b"a" * 40 + b")\n"
            help_output = b"Trusted Server CLI\nUsage: ts <COMMAND>\n"
            initialized = b"Initialized config at /work/output/trusted-server.toml\n"
            result = recipe.native_cli_outcomes(contract, help_output, version, initialized, scaffold)
            self.assertEqual(set(result), {"help", "version", "config_init"})
            self.assertTrue(all(command["exit_code"] == 0 for command in result.values()))
            damaged = ((help_output, b"", initialized, scaffold),
                       (help_output, version.replace(b"a" * 40, b"b" * 40), initialized, scaffold),
                       (help_output, version.replace(b"1.2.3", b"1.2.4"), initialized, scaffold),
                       (help_output, version, b"", scaffold),
                       (help_output, version, initialized, b""),
                       (help_output, version, initialized, scaffold.replace(b"publisher", b"other")))
            for outputs in damaged:
                with self.subTest(outputs=outputs), self.assertRaisesRegex(ValueError, "native.*version|native.*scaffold|native.*config"):
                    recipe.native_cli_outcomes(contract, *outputs)
            with self.assertRaisesRegex(ValueError, "source"):
                recipe.native_cli_contract(source, "unknown")
            (source / "trusted-server.example.toml").unlink()
            with self.assertRaises((ValueError, OSError)):
                recipe.native_cli_contract(source, "a" * 40)


class LinuxLifecycleTests(unittest.TestCase):
    def test_map_path_does_not_rewrite_sibling_prefix(self):
        sys.path.insert(0, str(RELEASE / "qualification"))
        from boundaries import LinuxBoundary
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            private, tools = root / "build", root / "build-tools"
            private.mkdir()
            tools.mkdir()
            boundary = LinuxBoundary(private, [tools], "example/builder@sha256:" + "a" * 64)
            self.assertEqual(boundary.map_path(tools / "cargo"), str(tools / "cargo"))
            self.assertEqual(boundary.map_path("--remap-path-prefix=" + str(private / "source") + "=/source"), "--remap-path-prefix=/work/source=/source")

    def test_container_cleanup_identity_is_source_owned_unique_and_closed(self):
        sys.path.insert(0, str(RELEASE / "qualification"))
        from boundaries import LinuxBoundary
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            boundary = LinuxBoundary(root, [], "example/builder@sha256:" + "a" * 64)
            one, two = boundary.command(["/usr/bin/true"]), boundary.command(["/usr/bin/true"])
            self.assertIn("--name", one)
            self.assertNotEqual(one[one.index("--name") + 1], two[two.index("--name") + 1])
            self.assertEqual(boundary.cleanup_command(one)[-1], one[one.index("--name") + 1])
            changed = list(one)
            changed[changed.index("--name") + 1] = "unselected-container"
            with self.assertRaisesRegex(ValueError, "identity"):
                boundary.cleanup_command(changed)

    def test_docker_context_is_explicit_and_empty_for_run_and_cleanup(self):
        sys.path.insert(0, str(RELEASE / "qualification"))
        from boundaries import LinuxBoundary
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            boundary = LinuxBoundary(root, [], "example/builder@sha256:" + "a" * 64)
            argv = boundary.command(["/usr/bin/true"])
            prefix = ["/usr/bin/docker", "--host", "unix:///var/run/docker.sock", "--config", str(root / "docker-config")]
            self.assertEqual(argv[:5], prefix)
            self.assertEqual(boundary.cleanup_command(argv)[:5], prefix)
            self.assertEqual((root / "docker-config/config.json").read_bytes(), b"{}\n")
            self.assertIn(f"type=bind,src={root / 'docker-config'},dst=/work/docker-config,readonly", argv)

    def test_native_container_startup_uses_same_closed_cleanup_lifecycle(self):
        from unittest import mock
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            final = root / "final"
            final.write_bytes(b"example native fixture")
            _, scaffold = native_fixture_source(root)
            selection = {"source_commit": "a" * 40, "runtime": {name: "example/runtime@sha256:" + "a" * 64 for name in ("ubi9-x86_64", "ubuntu22.04-x86_64", "debian12-x86_64")}}
            commands = b'Usage: ts\nTS_RELEASE_VERSION\nts 1.2.3 (' + b'a' * 40 + b')\nTS_RELEASE_INIT\nInitialized config at /work/output/trusted-server.toml\nTS_RELEASE_SCAFFOLD\n' + scaffold
            outputs = [(b'ID=debian\nVERSION_ID="12"\nTS_RELEASE_ARCH\nx86_64\nTS_RELEASE_HELP\n' + commands, b""),
                       (b'ID=rhel\nVERSION_ID="9.7"\nTS_RELEASE_ARCH\nx86_64\nTS_RELEASE_HELP\n' + commands, b""),
                       (b'ID=ubuntu\nVERSION_ID="22.04"\nTS_RELEASE_ARCH\nx86_64\nTS_RELEASE_HELP\n' + commands, b"")]
            with mock.patch.object(recipe, "capture", side_effect=outputs) as captured:
                observed = recipe._native_startup(final, recipe.artifact_selection("ts-x86_64-unknown-linux-gnu"), selection, root, {})
            self.assertEqual(observed["runtime"][0]["os_version"], "12")
            self.assertEqual(observed["runtime"][0]["architecture"], "x86_64")
            self.assertEqual(observed["runtime"][0]["executable_sha256"], recipe.sha256_bytes(final.read_bytes()))
            self.assertEqual(captured.call_count, 3)
            self.assertEqual(set(observed["runtime"][0]["commands"]), {"help", "version", "config_init"})
            for call in captured.call_args_list:
                self.assertIn("--name", call.args[0])
                self.assertIn("--config", call.args[0])
                self.assertIn("boundary", call.kwargs)

    def test_native_runtime_rejects_mismatched_os_and_architecture(self):
        valid = b'ID=ubuntu\nVERSION_ID="22.04"\nTS_RELEASE_ARCH\nx86_64\nTS_RELEASE_HELP\nUsage: ts\n'
        for changed in (valid.replace(b'22.04', b'24.04'), valid.replace(b'x86_64', b'aarch64'),
                        valid.replace(b'Usage: ts', b'healthy')):
            with self.subTest(output=changed), self.assertRaisesRegex(ValueError, "runtime"):
                recipe.native_runtime_identity(changed, "ubuntu22.04-x86_64")

    def test_fixed_uid_preparation_preserves_private_outer_root(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            private = root / "private"
            private.mkdir(mode=0o700)
            workspace = private / "role"
            workspace.mkdir(mode=0o700)
            writable = workspace / "target"
            writable.mkdir(mode=0o700)
            source = workspace / "source"
            source.mkdir(mode=0o700)
            (source / "input").write_text("example")
            recipe.container_permissions(workspace, [writable], [source])
            self.assertEqual(private.stat().st_mode & 0o777, 0o700)
            self.assertEqual(workspace.stat().st_mode & 0o777, 0o777)
            self.assertEqual(writable.stat().st_mode & 0o777, 0o777)
            self.assertEqual((source / "input").stat().st_mode & 0o777, 0o644)


class SafeLogTests(unittest.TestCase):
    def test_capture_refuses_symlink_log_without_modifying_target(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            original = root / "original"
            original.write_bytes(b"original example")
            (root / "cargo.stdout").symlink_to(original)
            with self.assertRaises((OSError, ValueError)):
                recipe.capture([sys.executable, "-I", "-c", "print('changed')"], {}, root, root / "cargo", timeout=10)
            self.assertEqual(original.read_bytes(), b"original example")

class IsolatedQualificationImportTests(unittest.TestCase):
    def test_direct_candidate_selection_uses_source_owned_boundary_in_fresh_python(self):
        script = """
import sys
sys.path.insert(0, sys.argv[1])
import rust_recipe as recipe
names = {"python", "git", "cargo", "rustc", "cc", "cxx", "linker", "ar", "cmake", "shell", "docker"}
policy = {key: None for key in ("environment_id", "tools", "readonly", "trees", "tool_trees", "archive_pool", "cargo_seed", "rustup", "sdk", "variation", "image", "runtime")}
policy.update(environment_id="gnu-builder-x86_64", tools={name: {"path": "/example/selected-tool", "sha256": "a" * 64, "acquisition_sha256": "b" * 64} for name in names})
recipe.CANDIDATE_ACQUISITIONS["gnu-builder-x86_64"] = policy
try:
    recipe.candidate_selection("ts-x86_64-unknown-linux-gnu", "a" * 40)
except ValueError as error:
    assert "actual source-owned Docker executable" in str(error), str(error)
else:
    raise AssertionError("wrong Docker executable accepted")
import boundaries
assert recipe.LinuxBoundary is boundaries.LinuxBoundary
print("source-owned import closed")
"""
        result = subprocess.run([sys.executable, "-I", "-B", "-c", script, str(RELEASE)], capture_output=True, timeout=30, check=False)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        self.assertEqual(result.stdout.strip(), b"source-owned import closed")

    def test_preloaded_foreign_boundary_module_is_rejected(self):
        script = """
import sys, types
sys.path.insert(0, sys.argv[1])
foreign = types.ModuleType("boundaries")
foreign.__file__ = "/example/foreign/boundaries.py"
sys.modules["boundaries"] = foreign
try:
    import rust_recipe
except ValueError as error:
    assert "source-owned qualification" in str(error), str(error)
else:
    raise AssertionError("foreign boundary module accepted")
print("foreign module rejected")
"""
        result = subprocess.run([sys.executable, "-I", "-B", "-c", script, str(RELEASE)], capture_output=True, timeout=30, check=False)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        self.assertEqual(result.stdout.strip(), b"foreign module rejected")


class SourceContextTests(unittest.TestCase):
    def test_actual_executing_source_commit_and_helpers_are_bound(self):
        import subprocess
        from test_prepare import source, git
        names = ("common.py", "preflight.py", "prepare.py", "rust_recipe.py", "inspect-native.py", "validate-fastly.py",
                 "qualification/boundaries.py", "qualification/qualify.py", "qualification/socket_probe.py")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve() / "source"
            commit = source(root, {"scripts/release/" + name: (RELEASE / name).read_bytes() for name in names})
            script = "import sys;from pathlib import Path;sys.path.insert(0,sys.argv[1]);import rust_recipe as r;r.executing_source_context(sys.argv[2]);print('bound')"
            result = subprocess.run([sys.executable, "-I", "-B", "-c", script, str(root / "scripts/release"), commit], capture_output=True, timeout=30, check=False)
            self.assertEqual(result.returncode, 0, result.stderr.decode())
            self.assertEqual(result.stdout.strip(), b"bound")
            # A loaded helper cannot claim the later commit's changed bytes.
            mutated = "import sys,subprocess;from pathlib import Path;sys.path.insert(0,sys.argv[1]);import rust_recipe as r;p=Path(sys.argv[1])/'common.py';p.write_text(p.read_text()+'\\n# Changed fixture helper\\n');root=Path(sys.argv[1]).parents[1];subprocess.run(['/usr/bin/git','-C',str(root),'add','.'],check=True);subprocess.run(['/usr/bin/git','-C',str(root),'commit','-qm','Change fixture helper'],check=True);commit=subprocess.check_output(['/usr/bin/git','-C',str(root),'rev-parse','HEAD']).decode().strip();r.executing_source_context(commit)"
            result = subprocess.run([sys.executable, "-I", "-B", "-c", mutated, str(root / "scripts/release"), commit], capture_output=True, timeout=30, check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(b"executing", result.stderr)

    def test_untracked_executing_helper_is_rejected(self):
        import subprocess
        from test_prepare import source
        names = ("common.py", "preflight.py", "prepare.py", "rust_recipe.py", "inspect-native.py", "validate-fastly.py",
                 "qualification/boundaries.py", "qualification/qualify.py", "qualification/socket_probe.py")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve() / "source"
            commit = source(root, {"scripts/release/" + name: (RELEASE / name).read_bytes() for name in names if name != "rust_recipe.py"})
            (root / "scripts/release/rust_recipe.py").write_bytes((RELEASE / "rust_recipe.py").read_bytes())
            script = "import sys;sys.path.insert(0,sys.argv[1]);import rust_recipe as r;r.executing_source_context(sys.argv[2])"
            result = subprocess.run([sys.executable, "-I", "-B", "-c", script, str(root / "scripts/release"), commit], capture_output=True, timeout=30, check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(b"unexpected source-only file", result.stderr)

    def test_wrong_commit_and_self_auth_json_are_rejected(self):
        with self.assertRaisesRegex(ValueError, "source|Git|executing"):
            recipe.executing_source_context("a" * 40)
        with self.assertRaisesRegex(ValueError, "source commit"):
            recipe.candidate_selection("trusted-server-fastly.wasm", {"approved": True, "commit": "a" * 40})

    def test_acquisition_cannot_attest_a_different_docker_executable(self):
        from unittest import mock
        names = {"python", "git", "cargo", "rustc", "cc", "cxx", "linker", "ar", "cmake", "shell", "docker"}
        policy = {key: None for key in ("environment_id", "tools", "readonly", "trees", "tool_trees", "archive_pool", "cargo_seed", "rustup", "sdk", "variation", "image", "runtime")}
        policy.update(environment_id="gnu-builder-x86_64", tools={name: {"path": "/example/selected-tool", "sha256": "a" * 64, "acquisition_sha256": "b" * 64} for name in names})
        with mock.patch.dict(recipe.CANDIDATE_ACQUISITIONS, {"gnu-builder-x86_64": policy}):
            with self.assertRaisesRegex(ValueError, "actual.*Docker|Docker.*executable"):
                recipe.candidate_selection("ts-x86_64-unknown-linux-gnu", "a" * 40)

    def test_acquisition_policy_cannot_select_an_alternate_checkout(self):
        from unittest import mock
        with mock.patch.dict(recipe.CANDIDATE_ACQUISITIONS, {"gnu-builder-x86_64": {"source": "/alternate/example", "approved_commit": "a" * 40}}):
            with self.assertRaisesRegex(ValueError, "acquisition.*policy"):
                recipe.candidate_selection("trusted-server-fastly.wasm", "a" * 40)

class ToolTreeTests(unittest.TestCase):
    def test_tool_tree_preserves_authenticated_internal_alias(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            versions = root / "Framework/Versions"
            (versions / "A").mkdir(parents=True)
            (versions / "A/header.h").write_text("example header")
            (versions / "Current").symlink_to("A", target_is_directory=True)
            measured = recipe.tool_tree_digest(root, approved_roots=[root])
            (versions / "Current").unlink()
            (versions / "Current").symlink_to("./A", target_is_directory=True)
            self.assertNotEqual(recipe.tool_tree_digest(root, approved_roots=[root]), measured)
            with self.assertRaisesRegex(ValueError, "link"):
                recipe.tree_digest(root)

    def test_tool_tree_rejects_escape_cycle_and_unapproved_root(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            sdk, external = root / "sdk", root / "external"
            sdk.mkdir()
            external.mkdir()
            (external / "header.h").write_text("example")
            (sdk / "escape").symlink_to(external / "header.h")
            with self.assertRaisesRegex(ValueError, "escape"):
                recipe.tool_tree_digest(sdk, approved_roots=[sdk])
            (sdk / "escape").unlink()
            (sdk / "a").symlink_to("b")
            (sdk / "b").symlink_to("a")
            with self.assertRaisesRegex(ValueError, "cycle|resolve"):
                recipe.tool_tree_digest(sdk, approved_roots=[sdk])
            with self.assertRaisesRegex(ValueError, "root"):
                recipe.tool_tree_digest(sdk, approved_roots=[external])

class CargoPreparationTests(unittest.TestCase):
    def test_fresh_cargo_home_uses_verified_original_archives(self):
        import hashlib
        from test_prepare import archive
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            seed, pool, home = root / "seed", root / "pool", root / "cargo-home"
            seed.mkdir()
            pool.mkdir()
            home.mkdir()
            namespace = seed / "registry/index" / recipe.CARGO_REGISTRY_CACHE_NAME
            namespace.mkdir(parents=True)
            (namespace / "config.json").write_text('{"dl":"https://registry.example.com"}')
            path = pool / "example.crate"
            archive(path, [("example-1.0.0/Cargo.toml", b'[package]\nname="example"\nversion="1.0.0"\n')])
            checked = recipe.prepare.verify_archive(path, hashlib.sha256(path.read_bytes()).hexdigest(), algorithm="sha256", expected_root="example-1.0.0")
            index = {"git": []}
            archives = {("example", "1.0.0", recipe.CARGO_REGISTRY_SOURCE): checked}
            recipe.prepare_cargo_home(seed, home, archives, index, pool)
            self.assertEqual((home / "registry/cache" / recipe.CARGO_REGISTRY_CACHE_NAME / "example-1.0.0.crate").read_bytes(), path.read_bytes())
            self.assertFalse((home / "registry/src").exists())

    def test_restored_sources_and_unknown_seed_channels_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            seed, home = root / "seed", root / "home"
            seed.mkdir()
            home.mkdir()
            (seed / "registry/src").mkdir(parents=True)
            with self.assertRaisesRegex(ValueError, "seed|source"):
                recipe.prepare_cargo_home(seed, home, {}, {"git": []}, root)

class SharedJsAuthorityTests(unittest.TestCase):
    def test_self_consistent_inventory_does_not_authenticate_its_producer(self):
        from types import SimpleNamespace
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            bundles = root / "bundles"
            bundles.mkdir()
            (bundles / "tsjs-core.js").write_bytes(b"example bundle")
            inventory = root / "inventory.json"
            inventory.write_bytes(recipe.deterministic_json_bytes({"source_commit": "a" * 40, "authenticated": True}))
            request = SimpleNamespace(source_commit="a" * 40, js_dir=bundles, js_inventory=inventory)
            with self.assertRaisesRegex(ValueError, "producer.*binding|binding.*pending"):
                recipe.verified_shared_js_input(request)
