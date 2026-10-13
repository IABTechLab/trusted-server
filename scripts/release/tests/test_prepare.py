"""Preparation negatives use authenticated synthetic archives and Git commits."""

import base64
import gzip
import hashlib
import importlib.util
import io
import json
import os
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest import mock

RELEASE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(RELEASE))
prepare = __import__("prepare") if importlib.util.find_spec("prepare") else None


def archive(path, files, *, kind=None):
    with tarfile.open(path, "w:gz") as stream:
        for name, contents in files:
            entry = tarfile.TarInfo(name)
            entry.size = len(contents)
            if kind:
                entry.type = kind
                entry.linkname = "../escape"
            stream.addfile(entry, io.BytesIO(contents))
    return "sha512-" + base64.b64encode(hashlib.sha512(path.read_bytes()).digest()).decode()


def git(repo, *args):
    result = subprocess.run(["/usr/bin/git", "-C", str(repo), *args], check=True,
                            capture_output=True, env={"PATH": "/usr/bin:/bin",
                            "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull})
    return result.stdout.decode().strip()


def source(repo, files):
    repo.mkdir()
    git(repo, "init", "--template=")
    for name, data in files.items():
        path = repo / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    git(repo, "add", ".")
    git(repo, "-c", "user.name=Example", "-c", "user.email=example@example.com",
        "-c", "commit.gpgsign=false", "commit", "-m", "Synthetic preparation fixture")
    return git(repo, "rev-parse", "HEAD")


class PreparationTests(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(prepare, "should provide authenticated fresh preparation")
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.archive = self.root / "example.tgz"
        self.manifest = {"name": "example", "version": "1.0.0"}
        self.integrity = archive(self.archive, [
            ("package/package.json", json.dumps(self.manifest).encode()),
            ("package/index.js", b"export const example = 1;"),
        ])

    def test_registry_archive_authenticates_original_bytes(self):
        checked = prepare.verify_archive(self.archive, self.integrity)
        self.assertEqual(checked.manifest, self.manifest)
        self.assertEqual(checked.integrity, self.integrity)
        with self.assertRaisesRegex(ValueError, "integrity"):
            prepare.verify_archive(self.archive, "sha512-" + "A" * 86 + "==")

    def test_archive_supports_crates_sha256_and_exact_root(self):
        digest = hashlib.sha256(self.archive.read_bytes()).hexdigest()
        prepare.verify_archive(self.archive, digest, algorithm="sha256", expected_root="package")
        with self.assertRaisesRegex(ValueError, "root"):
            prepare.verify_archive(self.archive, digest, algorithm="sha256", expected_root="example-1.0.0")

    def test_tar_nonstandard_single_root_is_supported(self):
        integrity = archive(self.archive, [("example-v1/package.json", b"{}")])
        self.assertEqual(prepare.verify_archive(self.archive, integrity).root, "example-v1")

    def test_unknown_dot_segments_and_alias_duplicates_are_rejected(self):
        integrity = archive(self.archive, [("package/./index.js", b"synthetic")])
        with self.assertRaisesRegex(ValueError, "alias"):
            prepare.verify_archive(self.archive, integrity)
        integrity = archive(self.archive, [("package/./a", b"x"), ("package/a", b"y")])
        with self.assertRaises(ValueError):
            prepare.verify_archive(self.archive, integrity)
        integrity = archive(self.archive, [("package/./directory/", b"")], kind=tarfile.DIRTYPE)
        with self.assertRaisesRegex(ValueError, "alias"):
            prepare.verify_archive(self.archive, integrity)

    def test_only_pinned_identical_alias_pairs_can_be_merged(self):
        self.assertTrue(hasattr(prepare, "_approved_alias"), "should verify explicit identical alias exceptions")
        integrity = "sha512-MnA+YT8fwfJPgBx3m60MNqakm30XOkyIoH1y6huTQvC0PwZG7ki8NacLBcrPbNoo8vEZy7Jpuk7+jMO+CUovTQ=="
        first = tarfile.TarInfo("package/./dist/index.js")
        second = tarfile.TarInfo("package/dist/index.js")
        first.size = second.size = 1
        self.assertTrue(prepare._approved_alias(integrity, first, second, b"x", b"x"))
        for change in ("bytes", "mode", "path", "integrity", "type", "size"):
            altered = tarfile.TarInfo(second.name)
            altered.size = 1
            if change == "mode":
                altered.mode = 0o755
            elif change == "path":
                altered.name = "package/dist/other.js"
            elif change == "type":
                altered.type = tarfile.SYMTYPE
            elif change == "size":
                altered.size = 2
            with self.subTest(change=change):
                self.assertFalse(prepare._approved_alias("sha512-" + "A" * 86 + "==" if change == "integrity" else integrity,
                                                        first, altered, b"x", b"y" if change == "bytes" else b"x"))

    def test_rejects_traversal_duplicates_links_and_collisions(self):
        for files, kind in [
            ([("package/../escape", b"x")], None),
            ([("/package/escape", b"x")], None),
            ([("package/a", b"x"), ("package/a", b"y")], None),
            ([("package/a", b"x"), ("package/a/b", b"y")], None),
            ([("package/a", b"")], tarfile.SYMTYPE),
            ([("package/a", b"")], tarfile.LNKTYPE),
            ([("package/a", b"")], tarfile.CHRTYPE),
        ]:
            with self.subTest(files=files, kind=kind):
                integrity = archive(self.archive, files, kind=kind)
                with self.assertRaises(ValueError):
                    prepare.verify_archive(self.archive, integrity)

    def test_rejects_expanded_size_and_member_count(self):
        for limits in ({"max_expanded": 1}, {"max_members": 1}, {"max_bytes": 1}):
            with self.subTest(limits=limits), self.assertRaises(ValueError):
                prepare.verify_archive(self.archive, self.integrity, **limits)

    def test_rejects_metadata_bomb_and_privileged_archive_modes(self):
        data = gzip.compress(b"x" * 200000)
        self.archive.write_bytes(data)
        integrity = "sha512-" + base64.b64encode(hashlib.sha512(data).digest()).decode()
        with self.assertRaisesRegex(ValueError, "byte size"):
            prepare.verify_archive(self.archive, integrity, max_expanded=10000)
        with tarfile.open(self.archive, "w:gz", format=tarfile.PAX_FORMAT) as stream:
            entry = tarfile.TarInfo("package/file")
            entry.pax_headers = {"comment": "x" * 100000}
            stream.addfile(entry, io.BytesIO(b""))
        integrity = "sha512-" + base64.b64encode(hashlib.sha512(self.archive.read_bytes()).digest()).decode()
        with self.assertRaisesRegex(ValueError, "byte size"):
            prepare.verify_archive(self.archive, integrity, max_expanded=10000)
        with tarfile.open(self.archive, "w:gz") as stream:
            entry = tarfile.TarInfo("package/file")
            entry.mode = 0o4755
            stream.addfile(entry, io.BytesIO(b""))
        integrity = "sha512-" + base64.b64encode(hashlib.sha512(self.archive.read_bytes()).digest()).decode()
        with self.assertRaisesRegex(ValueError, "mode"):
            prepare.verify_archive(self.archive, integrity)

    def test_registry_input_rejects_fifo_and_symlink_without_opening_stream(self):
        fifo = self.root / "fifo"
        os.mkfifo(fifo)
        with self.assertRaises(ValueError):
            prepare.verify_archive(fifo, self.integrity)
        link = self.root / "link"
        link.symlink_to(self.archive)
        with self.assertRaises(ValueError):
            prepare.verify_archive(link, self.integrity)

    def test_materialization_rejects_stale_tree_and_archive_mutation(self):
        checked = prepare.verify_archive(self.archive, self.integrity)
        destination = self.root / "extracted"
        prepare.materialize_archive(checked, destination)
        self.assertEqual((destination / "index.js").read_bytes(), b"export const example = 1;")
        with self.assertRaisesRegex(ValueError, "fresh"):
            prepare.materialize_archive(checked, destination)
        self.archive.write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "integrity"):
            prepare.materialize_archive(checked, self.root / "other")

    def test_git_revision_and_whole_tree_are_exact(self):
        repo = self.root / "repository"
        revision = source(repo, {"src/example.rs": b"pub fn example() {}"})
        tree = git(repo, "rev-parse", revision + "^{tree}")
        self.assertEqual(prepare.verify_git_revision(repo, revision, tree), tree)
        for rev, expected in (("HEAD", tree), (revision, "a" * 40), ("b" * 40, tree)):
            with self.subTest(rev=rev), self.assertRaises(ValueError):
                prepare.verify_git_revision(repo, rev, expected)

    def test_oversized_source_rejected_before_any_batch_body_request(self):
        repo = self.root / "repository"
        revision = source(repo, {"oversized.txt": b"x" * (prepare.MAX_FILE + 1)})
        for operation in (lambda: prepare.materialize_source(repo, revision, self.root / "fresh"),
                          lambda: prepare.source_check(repo, revision)):
            with self.subTest(operation=operation), mock.patch.object(prepare.preflight, "_git", wraps=prepare.preflight._git) as observed:
                with self.assertRaisesRegex(ValueError, "size bound"):
                    operation()
                self.assertFalse(any(call.args[1] == ["cat-file", "--batch"] for call in observed.call_args_list),
                                 "should reject metadata before capturing any oversized batch body")

    def test_git_object_reader_binds_bounded_metadata_to_selected_objects(self):
        self.assertTrue(hasattr(prepare.preflight, "read_git_objects"), "should inspect bounded metadata before object bodies")
        repo = self.root / "repository"
        revision = source(repo, {"file": b"example"})
        oid = git(repo, "rev-parse", revision + ":file")
        self.assertEqual(prepare.preflight.read_git_objects(repo, {oid: "blob"})[oid], b"example")
        for identities, limits in (({oid: "tree"}, {}), ({"f" * 40: "blob"}, {}),
                                   ({oid: "blob"}, {"max_object": 6}),
                                   ({oid: "blob", revision: "commit"}, {"max_total": 7}),
                                   ({oid: "blob"}, {"max_members": 0}),
                                   ({oid: "blob"}, {"max_object": True})):
            with self.subTest(identities=identities, limits=limits), self.assertRaises(ValueError):
                prepare.preflight.read_git_objects(repo, identities, **limits)

    def test_git_metadata_malformed_pin_kind_and_size_fail_before_body(self):
        self.assertTrue(hasattr(prepare.preflight, "read_git_objects"), "should provide checked Git metadata joins")
        oid = "a" * 40
        for response in (f"{'b' * 40} blob 1\n", f"{oid} tree 1\n", f"{oid} blob -1\n",
                         f"{oid} blob True\n", f"{oid} blob 1.0\n", f"{oid} missing\n",
                         f"{oid} blob 1\n{oid} blob 1\n", "x" * 65537):
            with self.subTest(response=response[:80]), mock.patch.object(prepare.preflight, "_git", return_value=response.encode()) as observed:
                with self.assertRaises(ValueError):
                    prepare.preflight.read_git_objects(self.root, {oid: "blob"})
                self.assertEqual(len(observed.call_args_list), 1, "should not request bodies after invalid metadata")

    def test_git_metadata_batches_are_bounded_before_body_capture(self):
        repo = self.root / "repository"
        revision = source(repo, {f"file-{index}.txt": f"example-{index}".encode() for index in range(257)})
        objects = {line.split()[2]: "blob" for line in git(repo, "ls-tree", "-r", revision).splitlines()}
        with mock.patch.object(prepare.preflight, "_git", wraps=prepare.preflight._git) as observed:
            actual = prepare.preflight.read_git_objects(repo, objects)
        self.assertEqual(len(actual), 257)
        calls = observed.call_args_list
        self.assertEqual([call.args[1] for call in calls],
                         [["cat-file", "--batch-check"], ["cat-file", "--batch-check"], ["cat-file", "--batch"]])
        self.assertEqual([len(call.args[2].splitlines()) for call in calls[:2]], [256, 1])
        with mock.patch.object(prepare.preflight, "_git", wraps=prepare.preflight._git) as observed:
            with self.assertRaisesRegex(ValueError, "size bound"):
                prepare.preflight.read_git_objects(repo, objects, max_total=1)
            self.assertFalse(any(call.args[1] == ["cat-file", "--batch"] for call in observed.call_args_list))

    def test_git_materialization_ignores_export_filters_and_ambient_outputs(self):
        repo = self.root / "repository"
        revision = source(repo, {".gitattributes": b"src/* export-ignore\n",
                                "src/example.rs": b"$Format:%H$"})
        (repo / "node_modules").mkdir()
        (repo / "node_modules/stale").write_bytes(b"stale")
        (repo / "src/example.rs").write_bytes(b"ambient mutation")
        snapshot = self.root / "fresh"
        prepare.materialize_source(repo, revision, snapshot)
        self.assertEqual((snapshot / "src/example.rs").read_bytes(), b"$Format:%H$")
        self.assertFalse((snapshot / "node_modules").exists())
        self.assertEqual(git(snapshot, "ls-tree", "-r", revision), git(repo, "ls-tree", "-r", revision))
        self.assertFalse((snapshot / ".git/hooks").exists())
        with self.assertRaisesRegex(ValueError, "fresh"):
            prepare.materialize_source(repo, revision, snapshot)

    def test_private_git_without_parent_history_keeps_exact_committer_epoch(self):
        repo = self.root / "repository"
        source(repo, {"file": b"first"})
        (repo / "file").write_bytes(b"second")
        git(repo, "add", "file")
        git(repo, "-c", "user.name=Example", "-c", "user.email=example@example.com",
            "commit", "-m", "Second synthetic commit")
        revision = git(repo, "rev-parse", "HEAD")
        snapshot = self.root / "fresh"
        measured = prepare.materialize_source(repo, revision, snapshot)
        self.assertEqual(measured["source_date_epoch"], int(git(repo, "show", "-s", "--format=%ct", revision)))
        parent = git(repo, "rev-parse", revision + "^")
        result = subprocess.run(["/usr/bin/git", "-C", str(snapshot), "cat-file", "-e", parent], capture_output=True)
        self.assertNotEqual(result.returncode, 0, "should never export/traverse parent history")

    def test_selected_commit_rejects_malformed_or_duplicate_committer_timestamps(self):
        repo = self.root / "repository"
        revision = source(repo, {"file": b"example"})
        tree = git(repo, "rev-parse", revision + "^{tree}")
        for committer in ("committer Example <example@example.com> invalid +0000\n",
                          "committer Example <example@example.com> 1 +0000\n" * 2):
            data = f"tree {tree}\nauthor Example <example@example.com> 1 +0000\n{committer}\nSynthetic\n"
            result = subprocess.run(["/usr/bin/git", "-C", str(repo), "hash-object", "-t", "commit",
                                     "--literally", "-w", "--stdin"], input=data.encode(), capture_output=True, check=True)
            with self.subTest(committer=committer), self.assertRaisesRegex(ValueError, "timestamp"):
                prepare.source_check(repo, result.stdout.decode().strip())

    def test_source_export_only_allows_fixed_claude_link(self):
        repo = self.root / "repository"
        revision = source(repo, {"AGENTS.md": b"example"})
        (repo / "CLAUDE.md").symlink_to("AGENTS.md")
        git(repo, "add", "CLAUDE.md")
        git(repo, "-c", "user.name=Example", "-c", "user.email=example@example.com",
            "commit", "-m", "Add fixed link")
        revision = git(repo, "rev-parse", "HEAD")
        prepare.materialize_source(repo, revision, self.root / "fixed")
        (repo / "escape").symlink_to("../escape")
        git(repo, "add", "escape")
        git(repo, "-c", "user.name=Example", "-c", "user.email=example@example.com",
            "commit", "-m", "Add prohibited link")
        with self.assertRaisesRegex(ValueError, "link"):
            prepare.materialize_source(repo, git(repo, "rev-parse", "HEAD"), self.root / "bad")

    def test_git_rejects_configuration_replacements_and_alternates(self):
        for channel in ("filter", "replacement", "alternates"):
            repo = self.root / channel
            revision = source(repo, {"file": b"example"})
            if channel == "filter":
                git(repo, "config", "filter.example.clean", "example-command")
            elif channel == "replacement":
                git(repo, "update-ref", "refs/replace/" + "a" * 40, revision)
            else:
                (repo / ".git/objects/info/alternates").write_text("/example/objects\n")
            with self.subTest(channel=channel), self.assertRaises(ValueError):
                prepare.verify_git_revision(repo, revision)

    def test_unreviewed_install_fetch_and_root_hooks_fail_before_execution(self):
        for scripts in ({"postinstall": "curl https://example.com/tool"},
                        {"prepare": "node fetch.js"}, {"install": "npm install example"}):
            with self.subTest(scripts=scripts), self.assertRaisesRegex(ValueError, "review"):
                prepare.review_lifecycle({"name": "example", "version": "1.0.0", "scripts": scripts},
                                         root=True)
        with self.assertRaisesRegex(ValueError, "review"):
            prepare.review_lifecycle({"name": "example", "version": "1.0.0",
                                      "scripts": {"postinstall": "node fetch.js"}})
        with self.assertRaisesRegex(ValueError, "implicit"):
            prepare.review_lifecycle(self.manifest, paths=["binding.gyp"])

    def test_only_exact_pinned_registry_lifecycle_commands_are_reviewed(self):
        prepare.review_lifecycle({"name": "esbuild", "version": "0.27.3",
                                  "scripts": {"postinstall": "node install.js"}})
        for version, command in (("0.27.4", "node install.js"), ("0.27.3", "node fetch.js")):
            with self.subTest(version=version), self.assertRaises(ValueError):
                prepare.review_lifecycle({"name": "esbuild", "version": version,
                                          "scripts": {"postinstall": command}})


if __name__ == "__main__":
    unittest.main()
