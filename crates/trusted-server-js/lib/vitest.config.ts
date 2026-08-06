import path from 'node:path';

import { configDefaults, defineConfig } from 'vitest/config';

export default defineConfig({
  resolve: {
    alias: {
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
    environment: 'jsdom',
    globals: true,
    // This suite deliberately uses node:test + vm so it executes the generated
    // ES5 artifact without Vite transforms. CI invokes it separately with
    // `node --test`; importing it through Vitest rewrites import.meta.url.
    exclude: [...configDefaults.exclude, 'test/contract/aps-renderer-es5.test.mjs'],
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
      include: ['test/**/types.test.ts'],
      // Errors reported from files outside `include` are pre-existing and
      // unrelated; only the type assertions in the included files gate here.
      ignoreSourceErrors: true,
    },
    coverage: {
      provider: 'v8',
    },
  },
});
