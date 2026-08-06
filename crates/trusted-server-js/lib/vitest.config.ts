import path from 'node:path';

import { configDefaults, defineConfig } from 'vitest/config';

export default defineConfig({
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
    ],
    // Run tests in the main thread to avoid spawning
    // child processes/workers, which are blocked in this sandbox.
    threads: false,
    // Explicitly use thread pool (no forks) when workers are enabled.
    // Kept for clarity if threads are re-enabled later.
    pool: 'threads',
    setupFiles: [],
    coverage: {
      provider: 'v8',
    },
  },
});
