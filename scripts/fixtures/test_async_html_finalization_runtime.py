"""Verify process cleanup used by the actual-runtime finalization harness."""

import importlib.util
import os
import select
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import textwrap
import time
import tomllib
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location(
    "example_finalization_runtime",
    Path(__file__).with_name("async-html-finalization-runtime.py"),
)
RUNTIME = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNTIME)


class ProcessCleanupTests(unittest.TestCase):
    def test_already_exited_process_is_safe(self):
        process = subprocess.Popen([sys.executable, "-c", "pass"], start_new_session=True)
        process.wait(timeout=5)
        RUNTIME.stop(process)
        self.assertEqual(process.returncode, 0, "should tolerate an already exited group")

    def test_graceful_termination_reaps_process(self):
        process = subprocess.Popen(
            [sys.executable, "-c", "import time; time.sleep(300)"],
            start_new_session=True,
        )
        try:
            RUNTIME.stop(process)
            self.assertIsNotNone(process.returncode, "should reap a terminated process")
        finally:
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=5)

    def test_stubborn_termination_reaps_process(self):
        process = subprocess.Popen(
            [sys.executable, "-c",
             "import signal, time; signal.signal(signal.SIGTERM, signal.SIG_IGN); "
             "print('example-ready', flush=True); time.sleep(300)"],
            stdout=subprocess.PIPE,
            text=True,
            start_new_session=True,
        )
        try:
            self.assertEqual(process.stdout.readline(), "example-ready\n")
            RUNTIME.stop(process)
            self.assertIsNotNone(process.returncode, "should reap a forcibly killed process")
            self.assertEqual(process.returncode, -signal.SIGKILL)
        finally:
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=5)
            process.stdout.close()


class HarnessConfigurationTests(unittest.TestCase):
    def test_build_environment_preserves_compiler_settings_without_operator_config(self):
        source = {
            "PATH": "/private/tmp/example-bin", "RUSTFLAGS": "-D warnings",
            "CARGO_ENCODED_RUSTFLAGS": "-D\x1fwarnings", "CARGO_INCREMENTAL": "0",
            "CARGO_PROFILE_DEV_DEBUG": "0", "CARGO_PROFILE_RELEASE_LTO": "thin",
            "TSJS_PREBUILT_DIR": "/private/tmp/example-bundles",
            "TRUSTED_SERVER__PUBLISHER__ORIGIN_URL": "https://origin.example.com",
            "TRUSTED_SERVER_SECRET_EXAMPLE": "example-secret",
            "TSJS_SKIP_BUILD": "1",
            "CARGO_BUILD_TARGET": "wasm32-wasip1",
        }
        preserved = RUNTIME.build_environment(source)
        for key in ["RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_INCREMENTAL",
                    "CARGO_PROFILE_DEV_DEBUG", "CARGO_PROFILE_RELEASE_LTO", "TSJS_PREBUILT_DIR"]:
            self.assertEqual(preserved.get(key), source[key], f"should preserve explicit {key}")
        self.assertNotIn("TRUSTED_SERVER__PUBLISHER__ORIGIN_URL", preserved)
        self.assertNotIn("TRUSTED_SERVER_SECRET_EXAMPLE", preserved)
        self.assertNotIn("TSJS_SKIP_BUILD", preserved)
        self.assertNotIn("CARGO_BUILD_TARGET", preserved, "harness must select native/adapter targets itself")

    def test_checked_replacement_rejects_missing_or_ambiguous_template_fields(self):
        for text in ["missing", "token token"]:
            with self.subTest(text=text), self.assertRaisesRegex(RuntimeError, "example field"):
                RUNTIME.checked_replace(text, "token", "value", "example field")
        self.assertEqual(RUNTIME.checked_replace("token", "token", "value", "example field"), "value")

    def test_fixture_config_checks_loopback_budget_and_permission(self):
        template = (RUNTIME.ROOT / "trusted-server.example.toml").read_text()
        for case in ["disabled", "success", "hung", "auction", "streaming"]:
            with self.subTest(case=case):
                text = RUNTIME.fixture_config(template, case, 12345, 12346 if case == "auction" else None)
                RUNTIME.validate_fixture_config(text, case, 12345, 12346 if case == "auction" else None)
                if case == "success":
                    for old, new in [("html_finalization_timeout_ms = 750", "html_finalization_timeout_ms = 5000"),
                                     ("allow_async_html_finish = true", "allow_async_html_finish = false"),
                                     ("http://127.0.0.1:12345/success", "https://lookup.example.com/success")]:
                        with self.subTest(mutation=old), self.assertRaises(RuntimeError):
                            RUNTIME.validate_fixture_config(text.replace(old, new), case, 12345)
        drifted = template.replace('endpoint = "https://prebid.example.com/openrtb2/auction"',
                                   'endpoint="https://prebid.example.com/openrtb2/auction"', 1)
        with self.assertRaisesRegex(RuntimeError, "auction endpoint"):
            RUNTIME.fixture_config(drifted, "auction", 12345, 12346)

    def test_toml_paths_round_trip_quotes_backslashes_and_newlines(self):
        value = Path('/private/tmp/example 🚀 "quoted"\\path\nnext')
        parsed = tomllib.loads("path = " + RUNTIME.toml_string(value))
        self.assertEqual(parsed["path"], str(value), "should preserve path characters in TOML")
        workspace = value / "workspace"
        template = (RUNTIME.ROOT / "edgezero.toml").read_text()
        with mock.patch.object(RUNTIME, "ROOT", value):
            parsed = tomllib.loads(RUNTIME.fixture_manifest(template, workspace))
        self.assertEqual(parsed["app"]["entry"], str(value / "crates/trusted-server-core"))
        self.assertEqual(parsed["adapters"]["fastly"]["adapter"]["manifest"], str(workspace / "fastly.toml"))
        self.assertEqual(parsed["adapters"]["spin"]["adapter"]["manifest"], str(workspace / "spin.toml"))

    def test_fastly_preflight_rejects_old_viceroy_before_building(self):
        with tempfile.TemporaryDirectory() as directory, \
             mock.patch.object(RUNTIME.tempfile, "mkdtemp", return_value=directory), \
             mock.patch.object(RUNTIME, "free_port", return_value=12345), \
             mock.patch.object(RUNTIME.shutil, "which", return_value="/private/tmp/example-bin"), \
             mock.patch.object(RUNTIME.subprocess, "check_output", return_value="viceroy 0.17.0\n"), \
             mock.patch.object(RUNTIME.subprocess, "Popen", side_effect=RuntimeError("unexpected build before Viceroy preflight")) as build:
            with self.assertRaisesRegex(RuntimeError, "Viceroy.*0.19"):
                RUNTIME.run("fastly")
            build.assert_not_called()
        for version in ["viceroy 0.19.0\n", "viceroy 0.20.1\n", "viceroy 1.0.0\n"]:
            with self.subTest(version=version), mock.patch.object(RUNTIME.subprocess, "check_output", return_value=version):
                RUNTIME.preflight_viceroy({})

    def test_cooperative_timeout_requires_a_measured_diagnostic(self):
        for adapter in ["axum", "cloudflare", "spin"]:
            with self.subTest(adapter=adapter):
                with self.assertRaisesRegex(RuntimeError, "timeout diagnostic"):
                    RUNTIME.measured_terminal_ms(adapter, "times out without a duration")
                self.assertEqual(RUNTIME.measured_terminal_ms(adapter, "times out after 751 ms"), 751)
        self.assertIsNone(RUNTIME.measured_terminal_ms("fastly", "retains input: unavailable", 0.749))

    def test_fastly_transport_fallback_requires_measured_lookup_duration(self):
        with self.assertRaisesRegex(RuntimeError, "lookup duration"):
            RUNTIME.measured_terminal_ms("fastly", "retains input: unavailable")
        for elapsed in [0.1, 3.0]:
            with self.subTest(elapsed=elapsed), self.assertRaises(AssertionError):
                RUNTIME.measured_terminal_ms("fastly", "retains input: unavailable", elapsed)

    def test_eof_lookup_rejects_an_earlier_requests_eof(self):
        lookup = {"event": "lookup_start", "time": 2.2}
        for current_eof in [[], [{"event": "origin_eof", "time": 2.3}]]:
            events = [{"event": "origin_eof", "time": 1.0}, lookup, *current_eof]
            with self.subTest(events=events), self.assertRaisesRegex(AssertionError, "origin finishes"):
                RUNTIME.lookup_for_request(events, 2.0, 3.0, True)

    def test_eof_lookup_uses_only_the_current_request(self):
        lookup = {"event": "lookup_start", "time": 2.2}
        events = [
            {"event": "origin_eof", "time": 1.0},
            {"event": "lookup_start", "time": 1.1},
            {"event": "origin_eof", "time": 2.1},
            lookup,
        ]
        self.assertIs(RUNTIME.lookup_for_request(events, 2.0, 3.0, True), lookup)
        self.assertIsNone(RUNTIME.lookup_for_request(events, 4.0, 5.0, False))
        with self.assertRaisesRegex(AssertionError, "exactly one"):
            RUNTIME.lookup_for_request([*events, lookup], 2.0, 3.0, True)
        with self.assertRaisesRegex(AssertionError, "exactly one"):
            RUNTIME.lookup_for_request(events, 2.0, 3.0, False)


class CancellationTests(unittest.TestCase):
    def test_sigterm_unwinds_actual_run_and_stops_owned_origin(self):
        with tempfile.TemporaryDirectory() as directory:
            workspace = Path(directory)
            target = workspace / "target/debug"
            target.mkdir(parents=True)
            (target / "trusted-server-axum").touch()
            marker = workspace / "origin-stopped"
            runner = workspace / "runner.py"
            runner.write_text(textwrap.dedent('''
                import importlib.util, os, subprocess, sys, time
                spec = importlib.util.spec_from_file_location("runtime", sys.argv[1])
                runtime = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(runtime)
                runtime.free_port = lambda: 12345
                runtime.tempfile.mkdtemp = lambda **kwargs: sys.argv[2]
                real_popen = subprocess.Popen
                origin = None
                origin_program = "import os, pathlib, signal, sys, time; pathlib.Path(sys.argv[2]).write_text(str(os.getpid())); signal.signal(signal.SIGTERM, lambda *_: (pathlib.Path(sys.argv[1]).write_text('stopped'), sys.exit(0))); pathlib.Path(sys.argv[2] + '.ready').write_text('ready'); time.sleep(300)"
                def launch_origin(kwargs):
                    global origin
                    origin = real_popen([sys.executable, "-c", origin_program, sys.argv[3], os.path.join(sys.argv[2], "origin.pid")], **kwargs)
                    ready = os.path.join(sys.argv[2], "origin.pid.ready")
                    deadline = time.monotonic() + 5
                    while not os.path.exists(ready):
                        if time.monotonic() >= deadline:
                            raise RuntimeError("origin probe failed to become ready")
                        time.sleep(0.01)
                    return origin
                def fake_popen(args, **kwargs):
                    if args[0] == "python3":
                        return launch_origin(kwargs)
                    if args[1:3] == ["config", "init"]:
                        print("READY", flush=True)
                        return real_popen([sys.executable, "-c", "import os, pathlib, sys, time; pathlib.Path(sys.argv[1]).write_text(str(os.getpid())); time.sleep(300)", os.path.join(sys.argv[2], "command.pid")], **kwargs)
                    return real_popen([sys.executable, "-c", "pass"], **kwargs)
                def fake_run(args, **kwargs):
                    if args[1:3] == ["config", "init"]:
                        print("READY", flush=True)
                        time.sleep(300)
                runtime.subprocess.run = fake_run
                runtime.subprocess.Popen = fake_popen
                runtime.run("axum")
            '''))
            environment = dict(os.environ)
            environment.update(CARGO_TARGET_DIR=str(workspace / "target"), PYTHONDONTWRITEBYTECODE="1")
            process = subprocess.Popen([sys.executable, str(runner), str(Path(RUNTIME.__file__)), str(workspace), str(marker)],
                                       env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                       start_new_session=True)
            try:
                ready = False
                until = time.monotonic() + 5
                transcript = b""
                while time.monotonic() < until:
                    readable, _, _ = select.select([process.stdout], [], [], 0.1)
                    if readable:
                        transcript += os.read(process.stdout.fileno(), 65536)
                        if b"READY\n" in transcript:
                            ready = True
                            break
                    if process.poll() is not None:
                        break
                self.assertTrue(ready, "should reach the owned-origin cancellation probe")
                time.sleep(0.1)
                process.terminate()
                process.wait(timeout=10)
                self.assertTrue(marker.exists(), "SIGTERM should unwind run() and stop its owned origin")
                self.assertEqual(process.returncode, 128 + signal.SIGTERM, "should preserve cancellation exit status")
                for filename in ["origin.pid", "command.pid"]:
                    pid_file = workspace / filename
                    self.assertTrue(pid_file.exists(), "should launch the owned process probe")
                    with self.assertRaises(ProcessLookupError):
                        os.kill(int(pid_file.read_text()), 0)
            finally:
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=5)
                # The child writes its own PID so failures can also clean the detached group.
                for path in workspace.glob("*.pid"):
                    try:
                        os.killpg(int(path.read_text()), signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                process.stdout.close()
                process.stderr.close()


if __name__ == "__main__":
    unittest.main()
