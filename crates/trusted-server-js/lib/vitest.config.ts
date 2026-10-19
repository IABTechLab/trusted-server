import path from 'node:path';

import { defineConfig } from 'vitest/config';

import { moduleAliases, sharedCodeAliases } from './build-module-options.mjs';

const cratesDir = path.resolve(__dirname, '..', '..');

export default defineConfig({
  // Modules keep their scripts and tests in their own crates, outside this
  // folder, so the test server may read the whole crates folder.
  server: { fs: { allow: [cratesDir] } },
  resolve: {
    alias: {
      ...sharedCodeAliases,
      ...moduleAliases(),
      // Test data more than one module's tests share.
      '@tsjs/test': path.resolve(__dirname, 'test'),
      // prebid.js doesn't expose src/adapterManager.js via its package
      // "exports" map, but we need it for client-side bidder validation.
      // Map the specifier to the actual dist file.
      'prebid.js/src/adapterManager.js': path.resolve(
        __dirname,
        'node_modules/prebid.js/dist/src/src/adapterManager.js'
      ),
      'prebid.js/src/adRendering.js': path.resolve(
        __dirname,
        'node_modules/prebid.js/dist/src/src/adRendering.js'
      ),
    },
  },
  test: {
    // A module's tests live in its own crate, in `js/test/`, beside the
    // script they test, so the tests are found from the crates folder.
    dir: cratesDir,
    include: ['trusted-server-js/lib/test/**/*.test.{ts,mjs}', '*/*/js/test/**/*.test.{ts,mjs}'],
    environment: 'jsdom',
    globals: true,
    // Run tests in the main thread to avoid spawning
    // child processes/workers, which are blocked in this sandbox.
    threads: false,
    // Explicitly use thread pool (no forks) when workers are enabled.
    // Kept for clarity if threads are re-enabled later.
    pool: 'threads',
    setupFiles: [],
    // The GPT diagnostics export contract is expressed as `expectTypeOf`
    // assertions, which `vitest run` alone never evaluates. Scope the type
    // gate to those files: a package-wide `tsc --noEmit` still fails on
    // pre-existing errors elsewhere.
    typecheck: {
      enabled: true,
      include: ['trusted-server-js/lib/test/**/types.test.ts', '*/*/js/test/**/types.test.ts'],
      // Errors reported from files outside `include` are pre-existing and
      // unrelated; only the type assertions in the included files gate here.
      ignoreSourceErrors: true,
    },
    coverage: {
      provider: 'v8',
    },
  },
});
