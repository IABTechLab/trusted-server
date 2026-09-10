# Template-cache terminology

## Goal

Replace the experimental shared-template cache's `C2` shorthand with names that tell an
operator or contributor what the component does. The preferred term is **template
cache**, and the public diagnostic header becomes `X-TS-Template-Cache`.

## Scope

Rename the active implementation and its interfaces consistently:

- Rust types, constants, functions, variables, test modules, assertions, comments, and
  log messages use `TemplateCache` or `template_cache` rather than `C2` or `c2`.
- The response header changes from `X-TS-C2-Cache` to `X-TS-Template-Cache`. The old
  header is not emitted as an alias because the feature is an experimental #1009 spike,
  not a stable interface.
- The local harness becomes `scripts/template-cache-local-test.sh`; CI, documentation,
  and references move with it.
- All non-archived operator documentation, examples, plans, and specifications say
  "template cache" rather than relying on the numbered cache taxonomy.
- Both opaque platform-key namespace components change from `ts-c2` to
  `ts-template-cache`: the canonical hash-domain bytes and the rendered key prefix.
  Fixed-length and key-format assertions move with them. The new namespace naturally
  makes old entries miss after deployment. The existing template schema version does
  not change because the transformed template bytes and assembly seam do not change.
- Active test fixtures such as `reserved-c2-seam`, diagnostic identifiers, assertions,
  and comments are renamed even when they are not externally visible.

Only four categories may retain the old spelling:

1. the exact `<!--ts-c2-v3-seam-7f4c9e2d-bids-->` marker in the template schema-version
   history;
2. before/after compatibility references in this migration design and its implementation
   plan; and
3. documents already under `docs/superpowers/archive/`, which remain unchanged as
   historical records; and
4. negative compatibility-test literals that prove the old `x-ts-c2-cache` header
   is absent on both cold and warm responses.

Unrelated `c2` substrings in creative fixtures, cookie or EC identifiers, lockfiles,
checksums, styling, or third-party content are outside scope.

## Behaviour and compatibility

This is a terminology change, not a cache-policy or assembly change. Header values,
eligibility decisions, TTL calculation, privacy enforcement, template bytes, and
miss/hit assembly remain identical.

The deliberate compatibility effects are:

1. Consumers of `X-TS-C2-Cache` must move to `X-TS-Template-Cache`.
2. Monitoring that matches `c2_template_cache` logs must move to `template_cache` logs.
3. Existing objects in the old opaque key namespace are not read. They expire normally;
   the first request in the new namespace safely creates a new template.
4. Callers of `scripts/c2-local-test.sh` must use the new script path.

No dual header, dual read, or script shim is warranted for an unmerged experimental
branch because each would preserve the terminology this change is removing.

## Verification

- Focused Rust tests cover diagnostic header values, cache keys, cache states, and
  miss/hit assembly.
- The renamed local harness passes in `esi` and `inline` modes.
- Fastly tests and target-matched Clippy pass.
- Formatting passes for Rust, JavaScript, and documentation.
- A final search finds no active cache-related `C2`/`c2` naming outside the exact
  exceptions listed above.
