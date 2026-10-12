"""Verify process cleanup used by the actual-runtime finalization harness."""

import importlib.util
import os
from pathlib import Path
import signal
import subprocess
import sys
import unittest

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


if __name__ == "__main__":
    unittest.main()
