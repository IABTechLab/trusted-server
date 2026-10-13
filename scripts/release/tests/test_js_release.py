"""JS recipe failure tests; synthetic bundles never claim release qualification."""

import importlib.util
import base64
import json
import hashlib
import logging
import re
import os
import http.server
import threading
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from test_prepare import RELEASE, archive, git, source
import preflight

prepare = __import__("prepare") if importlib.util.find_spec("prepare") else None


def pinned_node(test):
    executable = shutil.which("node")
    if not executable or subprocess.run([executable, "--version"], capture_output=True).stdout.strip() != b"v24.12.0":
        test.skipTest("exact local Node fixture unavailable; hosted selection belongs to Task 8")
    return Path(executable).resolve()


class JsReleaseTests(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(prepare, "should provide independent JS release orchestration")
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.a = self.root / "A"
        self.b = self.root / "B"
        for workspace in (self.a, self.b):
            (workspace / "bundles").mkdir(parents=True)
            for name in ("tsjs-core.js", "tsjs-example.js"):
                (workspace / "bundles" / name).write_bytes(b"console.log('synthetic');")
            modules = [{"id": name, "filename": f"tsjs-{name}.js", "size": 25,
                        "sha256": hashlib.sha256(b"console.log('synthetic');").hexdigest(), "contributions": []}
                       for name in ("core", "example")]
            (workspace / "inventory.json").write_bytes(json.dumps({"schema_version": 1,
                "source_commit": "a" * 40, "modules": modules, "tools": [], "materials": []}).encode())
        self.expected = ["core", "example"]

    def test_equal_final_bytes_publish_only_a_and_its_inventory(self):
        (self.b / "inventory.json").write_bytes(b'{"synthetic":true,"path":"B"}\n')
        output = self.root / "published"
        prepare.compare_and_publish(self.a, self.b, self.expected, output)
        self.assertEqual(sorted(path.name for path in output.iterdir()), ["bundles", "tsjs-inventory.json"])
        self.assertEqual(sorted(path.name for path in (output / "bundles").iterdir()),
                         ["tsjs-core.js", "tsjs-example.js"])
        self.assertEqual((output / "tsjs-inventory.json").read_bytes(), (self.a / "inventory.json").read_bytes())

    def test_one_byte_difference_publishes_nothing(self):
        (self.b / "bundles/tsjs-example.js").write_bytes(b"console.log('synthetic')!")
        output = self.root / "published"
        with self.assertRaisesRegex(ValueError, "differ"):
            prepare.compare_and_publish(self.a, self.b, self.expected, output)
        self.assertFalse(output.exists(), "should not publish any unequal build")

    def test_inventory_changed_after_build_cannot_publish(self):
        inventory = json.loads((self.a / "inventory.json").read_bytes())
        inventory["modules"][0]["sha256"] = "0" * 64
        (self.a / "inventory.json").write_text(json.dumps(inventory))
        output = self.root / "published"
        with self.assertRaisesRegex(ValueError, "inventory"):
            prepare.compare_and_publish(self.a, self.b, self.expected, output)
        self.assertFalse(output.exists())

    def test_missing_extra_symlink_and_empty_bundles_are_rejected(self):
        for change in ("missing", "extra", "symlink", "empty"):
            path = self.b / "bundles/tsjs-example.js"
            original = path.read_bytes()
            if change == "missing":
                path.unlink()
            elif change == "extra":
                (self.b / "bundles/extra.js").write_bytes(b"x")
            elif change == "symlink":
                path.unlink()
                path.symlink_to(self.a / "bundles/tsjs-example.js")
            else:
                path.write_bytes(b"")
            with self.subTest(change=change), self.assertRaises(ValueError):
                prepare.compare_and_publish(self.a, self.b, self.expected, self.root / "published")
            if change == "extra":
                (self.b / "bundles/extra.js").unlink()
            if path.is_symlink():
                path.unlink()
            path.write_bytes(original)

    def test_official_cli_fails_closed_pending_qualification(self):
        recipe = RELEASE / "build-js.sh"
        self.assertTrue(recipe.exists(), "should provide executable build recipe")
        result = subprocess.run(["/bin/sh", str(recipe), "--source", str(RELEASE.parents[1]),
                                 "--commit", "a" * 40, "--archives", str(self.root),
                                 "--work", str(self.root / "work"), "--output", str(self.root / "output")],
                                capture_output=True, env={"PATH": "/usr/bin:/bin", "HOME": str(self.root)})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b"qualification pending", result.stderr)
        self.assertFalse((self.root / "work").exists())

    def test_no_official_local_bypass_argument(self):
        result = subprocess.run(["/bin/sh", str(RELEASE / "build-js.sh"), "--local-fixture"],
                                capture_output=True, env={"PATH": "/usr/bin:/bin"})
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn(b'"qualification":"qualified"', result.stdout)

    def test_wrapper_cannot_execute_inherited_path_or_python_shell_injection(self):
        marker = self.root / "injection-ran"
        script = self.root / "dirname"
        script.write_text(f"#!/bin/sh\ntouch '{marker}'\n")
        script.chmod(0o755)
        loader = self.root / "loader"
        loader.write_text(f"touch '{marker}'\n")
        result = subprocess.run(["/bin/sh", str(RELEASE / "build-js.sh"), "--source", "/example",
                                 "--commit", "a" * 40, "--archives", "/example", "--work", "/example",
                                 "--output", "/example"], capture_output=True,
                                env={"PATH": str(self.root), "PYTHONPATH": str(self.root),
                                     "BASH_ENV": str(loader), "ENV": str(loader)})
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(marker.exists(), "should not execute caller hooks before guard")

    def test_changed_source_checks_test_data_and_build_config(self):
        repo = self.root / "source"
        commit = source(repo, {"src/index.ts": b"example", "test/data.json": b"{}",
                               "vitest.config.ts": b"example"})
        before = prepare.source_check(repo, commit)
        for name in ("src/index.ts", "test/data.json", "vitest.config.ts"):
            path = repo / name
            original = path.read_bytes()
            path.write_bytes(b"mutated")
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "source"):
                prepare.source_check(repo, commit, before)
            path.write_bytes(original)
        (repo / "test/extra.ts").write_bytes(b"untracked")
        with self.assertRaisesRegex(ValueError, "source"):
            prepare.source_check(repo, commit, before)

    def test_failed_child_and_changed_source_block_build(self):
        # Genuine subprocess failures and mutations, not mocked command success.
        repo = self.root / "source"
        commit = source(repo, {"src/index.ts": b"example"})
        with self.assertRaisesRegex(ValueError, "failed"):
            prepare.checked_stage(repo, commit, ["/bin/sh", "-c", "exit 23"], {}, self.root / "failure.log")
        with self.assertRaisesRegex(ValueError, "source"):
            prepare.checked_stage(repo, commit, ["/bin/sh", "-c", "echo mutated > src/index.ts"],
                                  {}, self.root / "mutation.log")

    def test_mandatory_test_command_cannot_follow_local_skip(self):
        commands = prepare.js_commands(self.root, self.root / "node")
        self.assertTrue(any("vitest.mjs" in str(arg) for cmd in commands for arg in cmd),
                        "should unconditionally run release Vitest")
        self.assertFalse(any("npx" in str(arg) for cmd in commands for arg in cmd),
                         "should never fetch a substitute tool")

    def test_required_native_tools_cannot_be_optional(self):
        lock = {"lockfileVersion": 3, "packages": {"": {"name": "example", "version": "1.0.0"}}}
        with self.assertRaisesRegex(ValueError, "native"):
            prepare.required_npm_materials(lock, "darwin", "arm64")

    def test_recipe_declares_actual_js_settings(self):
        recipe = next(record for record in preflight.fixed_tools()["recipes"] if record["id"] == "tsjs")
        self.assertEqual(recipe["script"], "scripts/release/build-js.sh")
        self.assertNotIn("cargo_locked", recipe["settings"])
        self.assertTrue(recipe["settings"]["mandatory_tests"])

    def test_release_vitest_uses_literal_host_without_dns_resolution(self):
        config = (RELEASE.parents[1] / prepare.LIB / "vitest.config.ts").read_text()
        self.assertIn("host: '127.0.0.1'", config, "should avoid localhost resolver access under denial")


class LocalBoundaryTests(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(prepare, "should provide actual local confinement probes")
        if sys.platform != "darwin" or os.environ.get("TS_RELEASE_BOUNDARY_TESTS") != "1":
            self.skipTest("actual macOS local probes run explicitly outside the outer sandbox")
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.a = self.root / "A"
        self.b = self.root / "B"
        self.a.mkdir()
        self.b.mkdir()
        (self.b / "generated.js").write_text("synthetic output from B")
        self.boundary = prepare.LocalMacBoundary(self.a, [Path("/bin"), Path("/usr/bin"), Path("/usr/lib"),
                                                      Path("/System"), Path(sys.executable).resolve().parent.parent])

    def run_confined(self, script):
        return subprocess.run(self.boundary.command(["/bin/sh", "-c", script]), capture_output=True,
                              cwd=self.a, env={"PATH": "/usr/bin:/bin", "HOME": str(self.a)})

    def test_no_a_b_generated_output_exchange_including_symlink_and_parent(self):
        (self.a / "link").symlink_to(self.b)
        for access in (f"cat '{self.b}/generated.js'", "cat ../B/generated.js", "cat link/generated.js",
                       f"/usr/bin/stat '{self.b}/generated.js'", f"echo exchanged > '{self.b}/generated.js'"):
            with self.subTest(access=access):
                self.assertNotEqual(self.run_confined(access).returncode, 0)
        self.assertEqual((self.b / "generated.js").read_text(), "synthetic output from B")
        self.assertEqual(self.run_confined("echo own > own.js && cat own.js").returncode, 0)

    def test_network_and_writes_to_shared_tools_are_denied(self):
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), http.server.BaseHTTPRequestHandler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        endpoint = f"http://127.0.0.1:{server.server_port}/"
        command = ["/usr/bin/curl", "--noproxy", "*", "--max-time", "2", endpoint]
        positive = subprocess.run(command, capture_output=True)
        self.assertEqual(positive.returncode, 0, "should prove the same local endpoint is reachable without denial")
        self.assertNotEqual(self.run_confined(f"/usr/bin/curl --noproxy '*' --max-time 2 {endpoint}").returncode, 0)
        shared = self.root / "shared"
        shared.mkdir()
        (shared / "tool").write_text("synthetic readonly tool")
        boundary = prepare.LocalMacBoundary(self.a, [Path("/bin"), shared])
        result = subprocess.run(boundary.command(["/bin/sh", "-c", f"echo changed > '{shared}/tool'"]),
                                capture_output=True, cwd=self.a)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual((shared / "tool").read_text(), "synthetic readonly tool")


class OfflineLifecycleTests(unittest.TestCase):
    """Run actual pinned npm on small authenticated synthetic package archives."""

    def setUp(self):
        self.assertIsNotNone(prepare, "should provide actual installation observations")
        self.assertTrue(hasattr(prepare, "install_npm"), "should freshly install and observe actual npm execution")
        self.node = pinned_node(self)
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()

    def fixture(self, name="example", script=None, body=b"", *, optional=False, shadow_node=False):
        manifest = {"name": name, "version": "0.27.3" if name == "esbuild" else "1.0.0"}
        if script:
            manifest["scripts"] = {"postinstall": script}
        if shadow_node:
            manifest["bin"] = {"node": "node-shadow.sh"}
        package = self.root / (name + ".tgz")
        members = [("package/package.json", json.dumps(manifest).encode()),
                   ("package/install.js", body), ("package/index.js", b"// synthetic")]
        if shadow_node:
            members.append(("package/node-shadow.sh", b"#!/bin/sh\necho shadow > shadow-ran\nexit 99\n"))
        integrity = archive(package, members)
        checked = prepare.verify_archive(package, integrity)
        root_manifest = {"name": "synthetic", "version": "1.0.0", "dependencies": {name: manifest["version"]}}
        if optional:
            root_manifest["optionalDependencies"] = root_manifest.pop("dependencies")
        locked_registry = json.loads((RELEASE.parents[1] / prepare.LIB / "package-lock.json").read_bytes())
        registry = locked_registry["packages"]["node_modules/esbuild"]["resolved"].split("/")[2]
        record = {"version": manifest["version"], "resolved": f"https://{registry}/{name}/-/{name}-{manifest['version']}.tgz",
                  "integrity": integrity}
        if script:
            record["hasInstallScript"] = True
        if optional:
            record["optional"] = True
        lock = {"name": "synthetic", "version": "1.0.0", "lockfileVersion": 3,
                "packages": {"": root_manifest, "node_modules/" + name: record}}
        repo = self.root / "repository"
        files = {str(prepare.LIB / "package.json"): json.dumps(root_manifest).encode(),
                 str(prepare.LIB / "package-lock.json"): json.dumps(lock).encode()}
        for file in ("prepare.py", "common.py", "preflight.py"):
            files["scripts/release/" + file] = (RELEASE / file).read_bytes()
        commit = source(repo, files)
        return repo, commit, {"node_modules/" + name: checked}

    def test_fresh_private_cache_installs_without_restored_modules(self):
        repo, commit, archives = self.fixture()
        for label in ("A", "B"):
            workspace = self.root / label
            workspace.mkdir()
            prepare.materialize_source(repo, commit, workspace / "source")
            environment = prepare.local_environment(workspace, self.node, commit)
            prepare.install_npm(workspace, commit, archives, self.node, environment)
            self.assertTrue((workspace / "source" / prepare.LIB / "node_modules/example/index.js").is_file())
            self.assertEqual(prepare.observed_lifecycle(workspace), ([], []))
        self.assertNotEqual(os.stat(self.root / "A/cache").st_ino, os.stat(self.root / "B/cache").st_ino)
        with self.assertRaisesRegex(ValueError, "fresh"):
            prepare.install_npm(self.root / "A", commit, archives, self.node,
                                prepare.local_environment(self.root / "A", self.node, commit))

    def test_actual_execution_records_no_output_hook_and_changes(self):
        for body in (b"// genuine execution with no output", b"require('fs').writeFileSync('generated.js', 'synthetic output');"):
            with self.subTest(body=body):
                # Each iteration uses a different genuine Git/npm fixture.
                case = self.root / ("output" if b"writeFile" in body else "no-output")
                case.mkdir()
                previous = self.root
                self.root = case
                repo, commit, archives = self.fixture("esbuild", "node install.js", body)
                workspace = case / "A"
                workspace.mkdir()
                prepare.materialize_source(repo, commit, workspace / "source")
                prepare.install_npm(workspace, commit, archives, self.node,
                                    prepare.local_environment(workspace, self.node, commit))
                events, outputs = prepare.observed_lifecycle(workspace)
                self.assertEqual(len(events), 1, "should observe a completed actual hook even without outputs")
                self.assertEqual(events[0]["event"], "postinstall")
                self.assertEqual(len(outputs), 1 if b"writeFile" in body else 0)
                self.root = previous

    def test_failed_hook_is_rejected_even_if_npm_ignores_optional_failure(self):
        repo, commit, archives = self.fixture("esbuild", "node install.js", b"process.exit(23);", optional=True)
        workspace = self.root / "A"
        workspace.mkdir()
        prepare.materialize_source(repo, commit, workspace / "source")
        with self.assertRaisesRegex(ValueError, "lifecycle execution failed"):
            prepare.install_npm(workspace, commit, archives, self.node,
                                prepare.local_environment(workspace, self.node, commit))
        with self.assertRaisesRegex(ValueError, "failed"):
            prepare.observed_lifecycle(workspace)

    def test_lifecycle_executes_selected_node_despite_npm_bin_shadow(self):
        repo, commit, archives = self.fixture("esbuild", "node install.js", b"// actual pinned Node", shadow_node=True)
        workspace = self.root / "A"
        workspace.mkdir()
        prepare.materialize_source(repo, commit, workspace / "source")
        shadow = workspace / "npm-prepended-bin"
        shadow.mkdir()
        marker = workspace / "shadow-ran"
        executable = shadow / "node"
        executable.write_text(f"#!/bin/sh\necho shadow > '{marker}'\nexit 99\n")
        executable.chmod(0o755)
        environment = prepare.local_environment(workspace, self.node, commit)
        environment["PATH"] = str(shadow) + os.pathsep + environment["PATH"]
        positive = subprocess.run(["/bin/sh", "-c", "node --version"], env=environment, capture_output=True)
        self.assertEqual(positive.returncode, 99, "should prove the same shadow path would replace bare node")
        self.assertTrue(marker.exists())
        marker.unlink()
        prepare.install_npm(workspace, commit, archives, self.node, environment)
        self.assertFalse(marker.exists(), "should bind hooks to selected Node even after npm extends PATH")
        self.assertFalse((workspace / "source" / prepare.LIB / "node_modules/esbuild/shadow-ran").exists())
        self.assertEqual(len(prepare.observed_lifecycle(workspace)[0]), 1)


class FullLocalJsTests(unittest.TestCase):
    """Actual full-source npm/Vite/Vitest under local denial; never official proof."""

    def setUp(self):
        self.assertIsNotNone(prepare, "should implement the release recipe")
        if os.environ.get("TS_RELEASE_FULL_JS_TESTS") != "1":
            self.skipTest("explicit full local recipe probe; hosted qualification remains pending")
        self.assertTrue(hasattr(prepare, "local_js_pair"), "should orchestrate both actual independent installations")

    def test_two_actual_offline_builds_and_tests_match_all_tracked_modules(self):
        self.node = pinned_node(self)
        repository = RELEASE.parents[1]
        temporary = tempfile.TemporaryDirectory(prefix="ts-release-js-local-", dir="/private/tmp")
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name).resolve()
        files = {}
        tracked = git(repository, "ls-tree", "-r", "--name-only", "HEAD").splitlines()
        for name in tracked:
            path = repository / name
            if path.is_symlink():
                continue
            files[name] = path.read_bytes()
        for name in ("prepare.py", "build-js.sh", "tests/test_prepare.py", "tests/test_js_release.py"):
            files["scripts/release/" + name] = (RELEASE / name).read_bytes()
        # This transient root commit is explicitly synthetic, not release evidence.
        fixture = root / "synthetic-source"
        revision = source(fixture, files)
        pool = root / "archives"
        pool.mkdir()
        lock = json.loads((fixture / prepare.LIB / "package-lock.json").read_bytes())
        native = json.loads(subprocess.run([str(self.node), "-e", "console.log(JSON.stringify([process.platform, process.arch]))"],
                                          capture_output=True, check=True).stdout)
        selected = prepare.required_npm_materials(lock, *native)
        for record in selected.values():
            digest = base64.b64decode(record["integrity"][7:]).hex()
            ambient = Path.home() / ".npm/_cacache/content-v2/sha512" / digest[:2] / digest[2:4] / digest[4:]
            destination = pool / (digest + ".tgz")
            if not destination.exists():
                destination.write_bytes(ambient.read_bytes())
        work = root / "private-pair"
        published = root / "local-unqualified-output"
        try:
            result = prepare.local_js_pair(fixture, revision, pool, work, published, self.node)
        except ValueError as error:
            logs = [path for path in work.glob("*/**/*.log") if path.parent.name in {"A", "B"}]
            tail = max(logs, key=lambda path: path.stat().st_mtime_ns).read_text()[-5000:] if logs else ""
            self.fail(f"{error}\nPrivate local-fixture diagnostic:\n{tail}")
        self.assertEqual(result["qualification"], "local-unqualified")
        self.assertEqual(len(list((published / "bundles").glob("tsjs-*.js"))), 13)
        summary = {"qualification": result["qualification"], "source_commit": revision,
                   "published_layout": ["bundles/", "tsjs-inventory.json"],
                   "inventory_module_count": len(json.loads((published / "tsjs-inventory.json").read_bytes())["modules"])}
        for label in ("A", "B"):
            workspace = work / label
            test_log = re.sub(r"\x1b\[[0-9;]*m", "", (workspace / "tests.log").read_text())
            counts = re.search(r"Test Files\s+(\d+) passed.*?Tests\s+(\d+) passed.*?Type Errors\s+no errors", test_log, re.S)
            self.assertIsNotNone(counts, "should prove complete Vitest and typecheck success for each build")
            self.assertEqual(counts.groups(), ("51", "1253"))
            summary[label] = {"test_files": int(counts[1]), "tests": int(counts[2]), "type_errors": 0,
                              "installed_packages": len(json.loads((workspace / "preparation.json").read_bytes())["packages"])}
            self.assertEqual(len(prepare.observed_lifecycle(workspace)[0]), 2)
        logging.info("Actual local fixture evidence: %s", json.dumps(summary, sort_keys=True))
        self.assertNotEqual(os.stat(work / "A/cache").st_ino, os.stat(work / "B/cache").st_ino)
        # Remove every usable native binary, including esbuild's postinstall hardlink.
        a = work / "A"
        library = a / "source" / prepare.LIB
        native_root = library / "node_modules/@esbuild" / "-".join(native)
        shutil.rmtree(native_root)
        for path in (library / "node_modules/esbuild/bin/esbuild",):
            path.unlink(missing_ok=True)
        for path in (library / "node_modules/esbuild/lib").glob("downloaded-*"):
            path.unlink()
        boundary = prepare.local_boundary(a, self.node, pool)
        environment = prepare.local_environment(a, self.node, revision)
        command = [str(self.node), "--input-type=module", "-e",
                   "import {build} from 'vite'; await build({configFile:false,envDir:false,"
                   "build:{write:false,lib:{entry:'src/core/index.ts',formats:['iife'],name:'Synthetic'}}});"]
        with self.assertRaisesRegex(ValueError, "failed"):
            prepare.checked_stage(a / "source", revision, command, environment,
                                  a / "missing-native.log", boundary=boundary, cwd=library)
        with self.assertRaisesRegex(ValueError, "failed"):
            prepare.checked_stage(a / "source", revision, [str(self.node), "node_modules/esbuild/install.js"],
                                  environment, a / "missing-native-installer.log", boundary=boundary, cwd=library)
        self.assertFalse(native_root.exists(), "should not fetch a substitute native package")
        self.assertFalse((library / "node_modules/esbuild/bin/esbuild").exists())
        self.assertEqual(list((library / "node_modules/esbuild/lib").glob("downloaded-*")), [])


if __name__ == "__main__":
    unittest.main()
