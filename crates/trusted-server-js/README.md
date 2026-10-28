# trusted-server-js

Rust wrapper and TypeScript build for Trusted Server's browser runtime.

The build script discovers the checked TypeScript entries, invokes the Node
build, and embeds the resulting IIFE bundles in Rust. The Rust API returns
individual bundles, deterministic concatenations, and content hashes used by
adapter responses. The shared browser code lives under `lib/src`, and a
vendor module's script lives in the crate that supplies the module, under
`js/src/<module>/`, with its tests under `js/test/<module>/`, apart from
Prebid's shim and the APS renderer, which are still under
`lib/src/integrations`. A script in a crate reaches the shared code as
`@tsjs/core/...` and `@tsjs/shared/...`, another module's code as
`@tsjs/module/<module>/...`, and its own files by relative paths. It cannot
import an npm package, because packages resolve only from this crate's
`node_modules`. `tsconfig.json` lists each crate's `js/src` folder for
`@tsjs/module`, and a test checks the list. The `crates` folder holds a
`tsconfig.json` and a Prettier config that editors find for crate scripts,
and an ESLint config that `npm run lint` uses from the crates folder. Cargo
reruns the build when a watched script folder changes, and a crate's first
`js/src` folder is watched from the next build that runs for another reason,
so touch `lib/package.json` after adding one. Cargo builds the bundles into a
private directory under its `OUT_DIR` and fails if any expected
bundle is missing or empty; the `dist` directory is written only by a manual
`npm run build` (for browser tests) and is never embedded unless you pass it
explicitly through `TSJS_PREBUILT_DIR`. The external Prebid.js artifact is
built separately and is not the deferred Trusted Server Prebid shim.

Run the JavaScript checks from `crates/trusted-server-js/lib`:

```bash
npm ci
npm run lint
npx vitest run
npm run format
npm run build
```

Rust target checks include this crate through `cargo test-fastly`. See
[Trusted Server JavaScript](../../docs/guide/tsjs.md) for the 13 integration
entries, 14 emitted bundles, and immediate, deferred, and standalone loading
modes.
