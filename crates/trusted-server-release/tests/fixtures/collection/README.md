# Collection candidate fixtures

`build-record.json` is fictional imported diagnostic data. The tests fill its
artifact, inventory, composition, and runtime digests from local fixture bytes.

`composition.json` is a fictional sealed composition receipt in the producer's
exact `composition_export()` shape: Cargo query plan digests, compiler
observations, and the graph, package origin, native, JavaScript and platform
receipts. The tests fill its target, final artifact digest (JavaScript,
platform and external-requirement bindings), approved source commit (package
origins and workspace origins) and shared inventory digest per build. The
collector rejects unknown or missing members at every level it checks, binds
the receipt to the record's artifact, source, inventory, materials digest and
fixed target, requires compiled units and one well-formed origin per observed
package, rejects any complete material native coverage claim, and requires a
Fastly receipt to embed every prepared JS module. The platform's installed-tool
subtrees stay opaque apart from those bindings, Apple evidence present exactly
for Darwin targets, and a runtime format (`wasm`, `elf`, `macho`) matching the
target; the tests add fictional Apple and runtime facts per target.
A Fastly runtime diagnostic names the build role (`A` or `B`) whose own run
accepted the final bytes and must match its record's role. That role is
asserted by the producer inside its own record, so it catches mix-ups and reuse
in an honest pipeline but not a malicious producer copying the other role's
runtime object over equal bytes. Binding strength comes from the trusted upload
role and job isolation; a per-run Viceroy identity remains a named gap. A
missing optional
native reproduction has a separate bounded result with no upload ID, transfer
digest, or invented environment observations. Its outcome derives from the
observed job state: never started, skipped or cancelled before starting is
`unavailable`; started without an upload, or a failed or timed-out conclusion,
is `failed`.

The record's boundary block keeps two identities apart. `fingerprint` and
`execution_fingerprint` name the boundary instance; they must be equal (the
socket denial receipt belongs to the instance that ran the build) and differ
between A and B. Because each instance fingerprint includes a fresh nonce, that
difference only proves separate instances. `invocation_digest` is the exact
build invocation digest (program, arguments including jobs, environment,
checkout-root working directory and umask); A and B must also differ there.

The dependency capability has no production constructor: the qualified adapter
is an uninhabited type. An imported record never constructs local recipe,
network-denial, composition, or runtime proof types. In particular, the imported
composition receipt checks digest and identity consistency; it does not prove
complete source coverage or final executable membership. These diagnostics
cannot authorize signing, publication, or deployment.

The pending workflow adapter must obtain upload identities from trusted current
run dependencies, supply the approved repository to both exact-ID requests,
limit transport bytes and time, and reject transfer warnings. Raw artifact REST
metadata corroborates artifact ID/name/digest, expiration, run, source, and
repository IDs; it does not independently identify a producing job or attempt.
The adapter must never select a caller-supplied download URL.

The ZIP reader accepts a declared flat ZIP32 profile with stored or raw-deflate
members and optional data descriptors. It rejects ZIP64, extras, encryption,
nonregular members, multidisk archives, comments, duplicate identities,
undeclared bytes, and local/central/descriptor disagreements. It retains checked
member bytes in memory and performs no filesystem extraction. Qualification of
the actual upload action archive format and trusted producer export remains
required before official collection.
