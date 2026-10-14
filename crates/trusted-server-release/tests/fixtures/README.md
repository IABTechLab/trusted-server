# Release document fixtures

Private document unit tests in `src/documents/tests.rs` are registered by `src/documents.rs`; public process tests exercise only the closed producer dispatch in `tests/commands.rs`. Fixture documents, names and digests are fictional controls. Schema-valid fixtures demonstrate structural validation, not source approval, authenticated acquisition, complete SPDX coverage or release qualification.

The source-owned `../../schemas/spdx-2.3.schema.json` is retained byte-for-byte from the SPDX specification at commit `aadf3b0b8dbbabdb4d880b0fc714255fea436ff7`, path `schemas/spdx-schema.json`: 45,312 bytes, SHA-256 `239208b7ac287b3cf5d9a9af23f9d69863971102a5e1587a27a398b43490b89b`, JSON Schema Draft 7. SPDX specification contributors provide this material under Creative Commons Attribution 3.0 Unported; the original repository license is retained in `../../schemas/SPDX-LICENSE`. The schema has not been modified.

Source: [SPDX specification schema](https://github.com/spdx/spdx-spec/blob/aadf3b0b8dbbabdb4d880b0fc714255fea436ff7/schemas/spdx-schema.json), [upstream license](https://github.com/spdx/spdx-spec/blob/aadf3b0b8dbbabdb4d880b0fc714255fea436ff7/LICENSE).

The exact `jsonschema` 0.58.6 dependency is a locally tested evaluator candidate. Default HTTP, file, TLS and IDNA features and optional language bindings are disabled. Every evaluator construction selects a draft explicitly and uses `offline()` to reject unregistered references, including under feature unification. Production code selects only the three retained schemas; no schema-file CLI override exists. The synthetic prefix-items control qualifies Draft 2020-12 behavior alongside structural positive and negative tests of the governing index and qualification-result schemas.

Atomic-output callers must select a trusted parent directory. Authenticated source/path policy and the complete helper source closure belong to Task 3. Producer execution and the POSIX bootstrap remain closed pending that stage and independent authentication. Full real-artifact SPDX coverage remains Task 11 work.
