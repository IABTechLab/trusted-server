# Release document fixtures

The utility tests construct small byte fixtures in `test_common.py` so duplicate
keys, invalid UTF-8, surrogate escapes, and trailing control bytes retain their
exact representations. These cases cover strict parsing, bounded reads, nesting,
flat asset names, lengths, identity tuples, original-byte hashes, deterministic
producer output, and atomic replacement failures. Future document fixtures belong
in this directory and must use fictional names and `example.com` URLs only.

Run the standard-library utility tests from the repository root:

```sh
python3 -m unittest discover -s scripts/release/tests -p 'test_common.py' -v
```

Schema validation requires Python 3.11 or newer and an isolated environment. Use
the environment's interpreter explicitly; do not rely on an ambient `jsonschema`
installation.

This also qualifies the validator against the Draft 2020-12 `prefixItems` and
`items` semantics, which distinguish this dialect from older drafts. The temporary
root is private, and the cleanup trap removes the test environment when the shell
exits.

```sh
set -eu
RELEASE_TEST_ROOT=$(mktemp -d)
trap 'rm -rf "$RELEASE_TEST_ROOT"' EXIT
python3 -m venv "$RELEASE_TEST_ROOT/venv"
"$RELEASE_TEST_ROOT/venv/bin/python" -m pip install --require-hashes --only-binary=:all: -r scripts/release/requirements.txt
"$RELEASE_TEST_ROOT/venv/bin/python" -m pip check
"$RELEASE_TEST_ROOT/venv/bin/python" -m unittest discover -s scripts/release/tests -p 'test_common.py' -v
"$RELEASE_TEST_ROOT/venv/bin/python" - <<'PY'
from jsonschema import Draft202012Validator
from jsonschema.exceptions import ValidationError

schema = {"type": "array", "prefixItems": [{"type": "integer"}], "items": False}
Draft202012Validator.check_schema(schema)
validator = Draft202012Validator(schema)
validator.validate([1])
for invalid in (["example"], [1, 2]):
    try:
        validator.validate(invalid)
    except ValidationError:
        continue
    raise AssertionError("should reject invalid Draft 2020-12 arrays")
print("Draft 2020-12 qualification passed")
PY
```

`requirements.txt` pins `jsonschema` and every transitive dependency, with SHA-256
hashes for the published distributions. It includes `typing-extensions`, which
`referencing` requires below Python 3.13. Future schemas use the explicit
`jsonschema.Draft202012Validator`; installing this dependency does not replace the
strict byte parser or authorize schema-based reserialization before hashing.

The parsing and reading helpers require callers to choose `max_bytes` for the
document role. Their default maximum depth is 64 containers, including the root;
brackets inside strings do not increase depth. Hashing always uses the supplied
original bytes. Producer JSON uses sorted keys, compact separators, UTF-8, and one
final newline. Atomic writes replace a destination using a private sibling file
with mode `0600`, syncing its bytes first and cleaning up the temporary file on
failure.
