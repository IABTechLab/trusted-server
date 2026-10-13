"""Fail-closed release input checks, using isolated fictional source trees."""

import copy
import hashlib
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
sys.path.insert(0, str(RELEASE))
if importlib.util.find_spec("preflight") is not None:
    import preflight
else:
    preflight = None


class ContractTests(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(preflight, "should provide release preflight implementation")
        self.cohort = json.loads((RELEASE / "release-cohort.json").read_bytes())
        self.tools = json.loads((RELEASE / "release-tools.json").read_bytes())
        self.policy = json.loads(
            (ROOT / "crates/trusted-server-cli/release-policy.json").read_bytes()
        )

    def test_fixed_cohort_accepts_four_exact_artifacts(self):
        preflight.validate_cohort(self.cohort)
        self.assertEqual(len(self.cohort["artifacts"]), 4, "should fix four artifacts")

    def test_rejects_missing_extra_and_duplicate_artifacts(self):
        for artifacts in (
            self.cohort["artifacts"][:-1],
            self.cohort["artifacts"] + [self.cohort["artifacts"][0]],
            [self.cohort["artifacts"][0]] * 4,
        ):
            with self.subTest(artifacts=artifacts):
                self.cohort["artifacts"] = artifacts
                with self.assertRaises(ValueError):
                    preflight.validate_cohort(self.cohort)

    def test_rejects_downgrade_target_feature_and_commands(self):
        for field, value in (
            ("assurance_profile", "provenance-sbom"),
            ("target", "wasm32-unknown-unknown"),
            ("features", ["reusable-sandbox"]),
            ("command", "example-command"),
        ):
            with self.subTest(field=field):
                changed = copy.deepcopy(self.cohort)
                fastly = next(a for a in changed["artifacts"] if a["role"] == "server")
                fastly[field] = value
                with self.assertRaises(ValueError):
                    preflight.validate_cohort(changed)

    def test_branches_are_exact_sorted_and_reviewed(self):
        for branches in (["*"], ["main", "main"], ["rc/*"], ["rc/202613"], ["dev"]):
            with self.subTest(branches=branches):
                self.cohort["eligible_branches"] = branches
                with self.assertRaises(ValueError):
                    preflight.validate_cohort(self.cohort)
        self.cohort["eligible_branches"] = ["main", "rc/202610"]
        preflight.validate_cohort(self.cohort)

    def test_pending_tools_are_structurally_valid_but_never_qualified(self):
        preflight.validate_tools(self.tools)
        with self.assertRaisesRegex(ValueError, "qualif"):
            preflight.validate_tools(self.tools, require_qualified=True)
        for record in self.tools["tools"]:
            record["sha256"] = "a" * 64
        with self.assertRaisesRegex(ValueError, "qualif"):
            preflight.validate_tools(self.tools, require_qualified=True)

    def test_rejects_missing_pins_unknown_version_and_unreviewed_recipe(self):
        for change in ("missing", "version", "command", "vite", "sqlite"):
            with self.subTest(change=change):
                changed = copy.deepcopy(self.tools)
                if change == "missing":
                    changed["tools"].pop()
                elif change == "version":
                    changed["tools"][0]["version"] = "999.0.0"
                elif change == "command":
                    changed["recipes"][0]["command"] = "example-command"
                elif change == "vite":
                    changed["recipes"][0]["settings"]["vite_env_loading"] = True
                else:
                    changed["recipes"][0]["settings"]["sqlite_features"] = ["sqlcipher"]
                with self.assertRaises(ValueError):
                    preflight.validate_tools(changed)

    def test_policy_fixes_identity_bounds_actions_and_has_no_self_hash(self):
        preflight.validate_policy(self.policy)
        self.assertNotIn("policy_sha256", self.policy, "should avoid self-hash cycles")
        for field in ("bounds", "identity", "artifacts", "tool_versions"):
            changed = copy.deepcopy(self.policy)
            changed[field] = {}
            with self.subTest(field=field):
                with self.assertRaises(ValueError):
                    preflight.validate_policy(changed)

    def test_rejects_wrong_types_unsorted_arrays_and_unknown_contract_fields(self):
        for document in (self.cohort, self.tools, self.policy):
            validate = (preflight.validate_cohort if document is self.cohort else
                        preflight.validate_tools if document is self.tools else preflight.validate_policy)
            changed = copy.deepcopy(document)
            changed["unexpected"] = None
            with self.assertRaises(ValueError):
                validate(changed)
        for field in ("schema_version",):
            self.cohort[field] = True
            with self.assertRaises(ValueError):
                preflight.validate_cohort(self.cohort)
        self.tools["tools"].reverse()
        with self.assertRaises(ValueError):
            preflight.validate_tools(self.tools)


class EnvironmentTests(unittest.TestCase):
    def setUp(self):
        self.assertTrue(hasattr(preflight, "audit_environment"), "should audit inherited inputs")

    def test_skip_presence_is_rejected_for_every_value(self):
        for value in ("", "0", "1"):
            with self.subTest(value=value):
                with self.assertRaisesRegex(ValueError, "TSJS_SKIP_BUILD"):
                    preflight.audit_environment({"TSJS_SKIP_BUILD": value})

    def test_rejects_compiler_cargo_node_native_and_loader_channels(self):
        for name in (
            "RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "RUSTFLAGS",
            "CARGO_ENCODED_RUSTFLAGS", "CARGO_PROFILE_RELEASE_DEBUG", "CARGO_HOME",
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER", "RUSTUP_TOOLCHAIN",
            "NODE_OPTIONS", "npm_config_userconfig", "VITE_MODE", "CC", "TARGET_CC",
            "TSJS_PREBUILT_DIR", "TSJS_PREBUILT_INVENTORY", "TSJS_RELEASE_MODE",
            "CC_x86_64_unknown_linux_gnu", "x86_64_unknown_linux_gnu_OPENSSL_DIR",
            "CFLAGS", "LDFLAGS", "SDKROOT", "SYSROOT", "RUST_LIB_SRC",
            "AWS_LC_SYS_USE_SYSTEM", "AWS_LC_SYS_USE_SYSTEM_x86_64_unknown_linux_gnu",
            "AWS_LC_SYS_SYSTEM_DIR", "AWS_LC_SYS_SKIP_VERSION_CHECK", "EFFECTIVE_TARGET",
            "LIBSQLITE3_SYS_USE_PKG_CONFIG", "SQLITE3_LIB_DIR", "SQLCIPHER_LIB_DIR",
            "PKG_CONFIG_PATH", "VCPKG_ROOT", "GIT_CONFIG_COUNT", "GIT_REPLACE_REF_BASE",
            "PYTHONPATH", "LD_PRELOAD", "DYLD_INSERT_LIBRARIES", "BASH_ENV", "ENV",
        ):
            with self.subTest(name=name):
                with self.assertRaisesRegex(ValueError, name):
                    preflight.audit_environment({name: "example-value"})

    def test_fresh_environment_removes_operator_and_provider_values(self):
        with tempfile.TemporaryDirectory() as temporary:
            paths = {key: temporary for key in (
                "home", "cargo_home", "rustup_home", "tmpdir", "sysroot"
            )}
            paths["tool_path"] = [temporary]
            inherited = {
                "HOME": temporary, "PATH": "/example/ambient-tools",
                "TRUSTED_SERVER__PUBLISHER__NAME": "example publisher",
                "TRUSTED_SERVER__RELEASE_BUILD": "1", "EDGEZERO__CONFIG": "example",
                "GH_TOKEN": "example-token", "FASTLY_API_TOKEN": "example-token",
                "AWS_ACCESS_KEY_ID": "example-token", "UNREVIEWED_VALUE": "example",
            }
            result = preflight.build_environment(
                inherited, "a" * 40, 1234567890, "x86_64-unknown-linux-gnu", paths
            )
            self.assertEqual(result["SOURCE_DATE_EPOCH"], "1234567890", "should fix epoch")
            self.assertIn("TRUSTED_SERVER__GIT_VERSION", result,
                          "should inject the approved Git version channel")
            self.assertEqual(result["TRUSTED_SERVER__GIT_VERSION"], "a" * 40,
                             "should inject full approved commit")
            self.assertNotIn("TRUSTED_SERVER__SOURCE_COMMIT", result,
                             "should use only the approved Git version channel")
            self.assertEqual(result["TRUSTED_SERVER__RELEASE_BUILD"], "1", "should enable facts")
            self.assertEqual(result["AWS_LC_SYS_USE_SYSTEM"], "0", "should choose bundled AWS-LC")
            self.assertEqual(result["AWS_LC_SYS_USE_SYSTEM_x86_64_unknown_linux_gnu"], "0",
                             "should select the stronger target-specific AWS-LC variable")
            self.assertEqual(result["LIBSQLITE3_SYS_USE_PKG_CONFIG"], "0", "should choose bundled SQLite")
            self.assertEqual(result["NODE_ENV"], "production", "should select production mode")
            for name in inherited.keys() - {"HOME", "PATH", "TRUSTED_SERVER__RELEASE_BUILD"}:
                self.assertNotIn(name, result, "should remove unreviewed inherited values")

    def test_release_control_is_strict_before_publisher_removal(self):
        for value in ("", "0", "true", "yes", " 1", "1\n"):
            with self.subTest(value=value):
                with self.assertRaisesRegex(ValueError, "RELEASE_BUILD"):
                    preflight.audit_environment({"TRUSTED_SERVER__RELEASE_BUILD": value})
        preflight.audit_environment({"TRUSTED_SERVER__RELEASE_BUILD": "1"})

    def test_environment_rejects_skip_before_sanitation_and_invalid_facts(self):
        with self.assertRaisesRegex(ValueError, "TSJS_SKIP_BUILD"):
            preflight.build_environment({"TSJS_SKIP_BUILD": "0"}, "a" * 40, 1,
                                        "wasm32-wasip1", {})
        for commit, epoch, target in (("HEAD", 1, "wasm32-wasip1"),
                                      ("a" * 40, True, "wasm32-wasip1"),
                                      ("a" * 40, 1, "unknown")):
            with self.subTest(commit=commit, epoch=epoch, target=target):
                with self.assertRaises(ValueError):
                    preflight.build_environment({}, commit, epoch, target, {})


class SourceTests(unittest.TestCase):
    def setUp(self):
        self.assertTrue(hasattr(preflight, "source_snapshot"), "should verify committed source bytes")
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.repo = Path(self.temporary.name) / "checkout"
        self.repo.mkdir()
        self.environment = {
            "HOME": self.temporary.name, "PATH": "/usr/bin:/bin",
            "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_AUTHOR_NAME": "Example Author", "GIT_AUTHOR_EMAIL": "author@example.com",
            "GIT_COMMITTER_NAME": "Example Author", "GIT_COMMITTER_EMAIL": "author@example.com",
            "GIT_AUTHOR_DATE": "2009-02-13T23:31:30Z",
            "GIT_COMMITTER_DATE": "2009-02-13T23:31:30Z",
        }
        self.git("init", "-q")
        for name, content in {
            "AGENTS.md": "Example instructions\n",
            "Cargo.toml": "[profile.release]\ndebug = 1\n",
            ".cargo/config.toml": "[alias]\nexample = 'check'\n",
            ".gitignore": "trusted-server.toml\n*.ignored\n",
            "crates/example/src/lib.rs": "// Example source\n",
            "crates/trusted-server-core/build_support/example.rs": "// Example build support\n",
            "crates/trusted-server-js/lib/src/core/index.ts": "export const example = 1;\n",
            "scripts/release/example.py": "# Example release source\n",
        }.items():
            path = self.repo / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)
        (self.repo / "CLAUDE.md").symlink_to("AGENTS.md")
        self.git("add", ".")
        self.git("-c", "commit.gpgsign=false", "commit", "-qm", "Add example source")
        self.commit = self.git("rev-parse", "HEAD").decode().strip()

    def git(self, *arguments):
        return subprocess.run(
            ["/usr/bin/git", "-C", str(self.repo), *arguments],
            env=self.environment, check=True, capture_output=True,
        ).stdout

    def test_clean_source_epoch_and_legitimate_tracked_link(self):
        snapshot = preflight.source_snapshot(self.repo, self.commit, {})
        self.assertEqual(snapshot["source_date_epoch"], 1234567890,
                         "should derive epoch from approved committer timestamp")
        self.assertEqual(snapshot["files"]["CLAUDE.md"]["mode"], "120000",
                         "should retain the exact tracked link mode")
        preflight.assert_source_unchanged(self.repo, self.commit, snapshot, {})

    def test_hidden_dirty_bytes_and_deletions_are_rejected(self):
        source = "crates/example/src/lib.rs"
        for flag in ("--assume-unchanged", "--skip-worktree"):
            with self.subTest(flag=flag):
                self.git("update-index", flag, source)
                (self.repo / source).write_text("// Unexpected mutation\n")
                with self.assertRaisesRegex(ValueError, "source"):
                    preflight.source_snapshot(self.repo, self.commit, {})
                (self.repo / source).write_text("// Example source\n")
        (self.repo / source).unlink()
        with self.assertRaises(ValueError):
            preflight.source_snapshot(self.repo, self.commit, {})

    def test_mode_and_link_mutation_are_rejected(self):
        source = self.repo / "crates/example/src/lib.rs"
        source.chmod(0o755)
        with self.assertRaisesRegex(ValueError, "mode"):
            preflight.source_snapshot(self.repo, self.commit, {})
        source.chmod(0o644)
        link = self.repo / "CLAUDE.md"
        link.unlink()
        link.symlink_to("Cargo.toml")
        with self.assertRaisesRegex(ValueError, "source"):
            preflight.source_snapshot(self.repo, self.commit, {})

    def test_executable_umask_variation_and_post_execution_permissions(self):
        executable = self.repo / "scripts/release/example.py"
        executable.chmod(0o755)
        self.git("add", ".")
        self.git("-c", "commit.gpgsign=false", "commit", "-qm", "Make example executable")
        self.commit = self.git("rev-parse", "HEAD").decode().strip()
        for permission in (0o755, 0o750):
            with self.subTest(permission=permission):
                executable.chmod(permission)
                snapshot = preflight.source_snapshot(self.repo, self.commit, {})
                self.assertEqual(snapshot["files"]["scripts/release/example.py"]["permissions"], permission,
                                 "should capture actual permissions for the selected umask")
                preflight.assert_source_unchanged(self.repo, self.commit, snapshot, {})
                executable.chmod(permission ^ 0o010)
                with self.assertRaisesRegex(ValueError, "source changed"):
                    preflight.assert_source_unchanged(self.repo, self.commit, snapshot, {})
        executable.chmod(0o644)
        with self.assertRaisesRegex(ValueError, "mode"):
            preflight.source_snapshot(self.repo, self.commit, {})

    def test_ignored_unexpected_source_files_are_rejected(self):
        for directory in (
            "crates/example/src", "crates/trusted-server-core/build_support",
            "crates/trusted-server-js/lib/src", "scripts/release", ".cargo",
        ):
            with self.subTest(directory=directory):
                unexpected = self.repo / directory / "example.ignored"
                unexpected.write_text("Unexpected source\n")
                with self.assertRaisesRegex(ValueError, "unexpected"):
                    preflight.source_snapshot(self.repo, self.commit, {})
                unexpected.unlink()

    def test_symlinked_source_parent_and_external_link_are_rejected(self):
        source_directory = self.repo / "crates/example/src"
        saved = self.repo / "crates/example/saved"
        source_directory.rename(saved)
        source_directory.symlink_to(saved, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "symlink"):
            preflight.source_snapshot(self.repo, self.commit, {})
        source_directory.unlink()
        saved.rename(source_directory)
        (source_directory / "example.ignored").symlink_to("/example/outside")
        with self.assertRaises(ValueError):
            preflight.source_snapshot(self.repo, self.commit, {})

    def test_post_execution_mutation_and_new_source_are_rejected(self):
        snapshot = preflight.source_snapshot(self.repo, self.commit, {})
        unexpected = self.repo / "scripts/release/new.ignored"
        unexpected.write_text("Unexpected source\n")
        with self.assertRaises(ValueError):
            preflight.assert_source_unchanged(self.repo, self.commit, snapshot, {})
        unexpected.unlink()
        (self.repo / "crates/example/src/lib.rs").write_text("// Unexpected source\n")
        with self.assertRaises(ValueError):
            preflight.assert_source_unchanged(self.repo, self.commit, snapshot, {})

    def test_fictional_operator_config_is_independent_and_preserved(self):
        snapshot = preflight.source_snapshot(self.repo, self.commit, {})
        operator = self.repo / "trusted-server.toml"
        operator.write_text('[publisher]\nname = "Example Publisher"\n')
        self.assertEqual(preflight.source_snapshot(self.repo, self.commit, {}), snapshot,
                         "should exclude operator files from compile inputs")
        self.assertTrue(operator.exists(), "should preserve operator configuration")

    def test_cargo_config_layers_and_git_replacements_are_rejected(self):
        ancestor = Path(self.temporary.name) / ".cargo"
        ancestor.mkdir()
        config = ancestor / "config.toml"
        config.write_text("[build]\nrustflags = ['--cfg=example']\n")
        with self.assertRaisesRegex(ValueError, "Cargo"):
            preflight.source_snapshot(self.repo, self.commit, {"HOME": self.temporary.name})
        config.unlink()
        self.git("config", "include.path", str(self.repo / "example-config"))
        with self.assertRaisesRegex(ValueError, "Git"):
            preflight.source_snapshot(self.repo, self.commit, {})
        self.git("config", "--unset", "include.path")
        self.git("update-ref", "refs/replace/" + "a" * 40, self.commit)
        with self.assertRaisesRegex(ValueError, "replacement"):
            preflight.source_snapshot(self.repo, self.commit, {})

    def test_approved_revision_is_a_full_actual_commit(self):
        blob = self.git("rev-parse", f"{self.commit}:Cargo.toml").decode().strip()
        for revision in ("HEAD", self.commit.upper(), blob):
            with self.subTest(revision=revision):
                with self.assertRaises(ValueError):
                    preflight.source_snapshot(self.repo, revision, {})


class PreflightTests(unittest.TestCase):
    git = SourceTests.git

    def setUp(self):
        SourceTests.setUp(self)
        self.assertTrue(hasattr(preflight, "preflight"), "should expose complete preflight")
        for relative in (
            "scripts/release/release-cohort.json", "scripts/release/release-tools.json",
            "crates/trusted-server-cli/release-policy.json", "scripts/release/preflight.py",
            "scripts/release/common.py",
        ):
            destination = self.repo / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes((ROOT / relative).read_bytes())
        for relative in ("rust-toolchain.toml", ".tool-versions",
                         "crates/trusted-server-adapter-fastly/Cargo.toml",
                         "crates/trusted-server-cli/Cargo.toml"):
            destination = self.repo / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes((ROOT / relative).read_bytes())
        self.git("add", ".")
        self.git("-c", "commit.gpgsign=false", "commit", "-qm", "Add example contracts")
        self.commit = self.git("rev-parse", "HEAD").decode().strip()

    def test_preflight_hashes_raw_bytes_and_reports_pending_qualification(self):
        result = preflight.preflight(self.repo, self.commit, inherited={})
        self.assertEqual(result["source_commit"], self.commit, "should retain source approval")
        self.assertEqual(result["source_date_epoch"], 1234567890, "should use approved epoch")
        self.assertFalse(result["qualified"], "should report pending qualification")
        for relative, digest in result["contract_sha256"].items():
            self.assertEqual(digest, hashlib.sha256((self.repo / relative).read_bytes()).hexdigest(),
                             "should bind exact raw contract bytes")

    def test_preflight_fails_qualification_and_audits_skip_before_contract_loading(self):
        with self.assertRaisesRegex(ValueError, "qualification"):
            preflight.preflight(self.repo, self.commit, inherited={}, require_qualified=True)
        (self.repo / "scripts/release/release-cohort.json").unlink()
        with self.assertRaisesRegex(ValueError, "TSJS_SKIP_BUILD"):
            preflight.preflight(self.repo, self.commit, inherited={"TSJS_SKIP_BUILD": ""})

    def test_preflight_rejects_external_contract_and_compiler_cargo_config(self):
        with self.assertRaisesRegex(ValueError, "source-owned"):
            preflight.preflight(self.repo, self.commit, inherited={},
                                cohort_path=Path(self.temporary.name) / "cohort.json")
        (self.repo / ".cargo/config.toml").write_text("[build]\nrustflags = ['--cfg=example']\n")
        self.git("add", ".")
        self.git("-c", "commit.gpgsign=false", "commit", "-qm", "Change example config")
        self.commit = self.git("rev-parse", "HEAD").decode().strip()
        with self.assertRaisesRegex(ValueError, "Cargo"):
            preflight.preflight(self.repo, self.commit, inherited={})

    def test_cli_fails_before_preparation_for_unqualified_contract(self):
        result = subprocess.run(
            [sys.executable, "-B", str(RELEASE / "preflight.py"),
             "--repo", str(self.repo), "--cohort", str(self.repo / "scripts/release/release-cohort.json"),
             "--source-commit", self.commit, "--require-qualified"],
            env={"PATH": "/usr/bin:/bin"}, capture_output=True, check=False,
        )
        self.assertNotEqual(result.returncode, 0, "should fail incomplete release contracts")
        self.assertIn(b"qualification", result.stderr, "should explain the missing real qualification")
        self.assertEqual(result.stdout, b"", "should produce no successful release output")
        self.assertFalse((self.repo / "target").exists(), "should not prepare or build before qualification")

    def test_committed_build_profile_and_tool_pins_cannot_drift(self):
        for relative, content in (
            ("Cargo.toml", "[profile.release]\ndebug = 0\n"),
            ("rust-toolchain.toml", "[toolchain]\nchannel = '999.0.0'\n"),
            (".tool-versions", "nodejs 999.0.0\n"),
        ):
            with self.subTest(relative=relative):
                path = self.repo / relative
                original = path.read_bytes()
                path.write_text(content)
                self.git("add", ".")
                self.git("-c", "commit.gpgsign=false", "commit", "-qm", "Change example build pin")
                commit = self.git("rev-parse", "HEAD").decode().strip()
                with self.assertRaises(ValueError):
                    preflight.preflight(self.repo, commit, inherited={})
                path.write_bytes(original)

    def test_plain_cli_does_not_generate_source_bytecode(self):
        result = subprocess.run(
            [sys.executable, str(self.repo / "scripts/release/preflight.py"),
             "--repo", str(self.repo), "--cohort", str(self.repo / "scripts/release/release-cohort.json"),
             "--source-commit", self.commit],
            env={"PATH": "/usr/bin:/bin"}, capture_output=True, check=False,
        )
        self.assertEqual(result.returncode, 0, "should permit explicit structural checks")
        self.assertFalse((self.repo / "scripts/release/__pycache__").exists(),
                         "should not mutate the source with caches")


if __name__ == "__main__":
    unittest.main()
