import path from 'node:path';

import { configDefaults, defineConfig } from 'vitest/config';

import { RELEASE_CATALOG } from './src/kernel/release_catalog.ts';

const integrationIds = RELEASE_CATALOG.map(({ id }) => id);

export default defineConfig({
  define: {
    __TSJS_EMBEDDED_RELEASE_ID_V1__: JSON.stringify('a'.repeat(64)),
    __TSJS_EMBEDDED_INTEGRATION_IDS_V1__: JSON.stringify(integrationIds),
    __TSJS_EMBEDDED_RUNTIME_CATALOG_V1__: JSON.stringify(
      RELEASE_CATALOG.map(({ id, phase, trigger, consumes, provides }) => ({
        id,
        phase,
        trigger,
        consumes,
        provides,
      }))
    ),
  },
  resolve: {
    alias: {
      // prebid.js doesn't expose src/adapterManager.js via its package
      // "exports" map, but we need it for client-side bidder validation.
      // Map the specifier to the actual dist file.
      'prebid.js/src/adapterManager.js': path.resolve(
        import.meta.dirname,
        'node_modules/prebid.js/dist/src/src/adapterManager.js'
      ),
      'prebid.js/src/adRendering.js': path.resolve(
        import.meta.dirname,
        'node_modules/prebid.js/dist/src/src/adRendering.js'
      ),
    },
  },
  test: {
    environment: 'jsdom',
    globals: true,
    // These suites deliberately use node:test. CI invokes them through their
    // package scripts; importing them through Vitest either rewrites the VM
    // contract fixture or leaves Vitest with no registered suite.
    exclude: [
      ...configDefaults.exclude,
      'test/contract/aps-renderer-es5.test.mjs',
      'test/eslint/no-adtech-globals.test.mjs',
      'test/build/*.test.mjs',
    ],
    // Bound JSDOM concurrency so the package-wide suite does not starve its
    // five-second lifecycle assertions while retaining per-file isolation.
    maxWorkers: 2,
    // Use worker threads rather than child processes.
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
