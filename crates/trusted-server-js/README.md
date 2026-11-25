# trusted-server-js

Rust wrapper and TypeScript build for Trusted Server's browser runtime.

The build script discovers the checked TypeScript entries, invokes the Node
build, and embeds the resulting IIFE bundles in Rust. The Rust API returns
individual bundles, deterministic concatenations, and content hashes used by
adapter responses. Browser behavior lives under `lib/src`. Cargo builds the
bundles into a private directory under its `OUT_DIR` and fails if any expected
bundle is missing or empty; the `dist` directory is written only by a manual
`npm run build` (for browser tests) and is never embedded unless you pass it
explicitly through `TSJS_PREBUILT_DIR`. The external Prebid.js artifact is
built separately and is not the deferred Trusted Server Prebid shim.

Ordinary attested release builds still run npm and embed fresh private output;
their release script refuses prebuilt overrides. The separate checked-prebuilt
path requires both `TSJS_PREBUILT_DIR` and
`TRUSTED_SERVER__TSJS_PREBUILT_INVENTORY` when a release build explicitly selects
prebuilt input. It validates the inventory's source commit and exact bundle set,
then checks the retained copies. This input check does not qualify the closed
controlled recipes or prove complete composition.

Run the JavaScript checks from `crates/trusted-server-js/lib`:

```bash
npm ci
npm run lint
npx vitest run
npm run format
npm run build
```

Rust target checks include this crate through `cargo test-fastly`. See
[Trusted Server JavaScript](../../docs/guide/tsjs.md) for the 12 integration
entries, 13 emitted bundles, and immediate, deferred, and standalone loading
modes.
