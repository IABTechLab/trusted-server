import path from 'node:path';
import fs from 'node:fs';

import { defineConfig } from 'vitest/config';

import { releaseBuild } from './release-composition.mjs';

if (
  Object.hasOwn(process.env, 'TRUSTED_SERVER__RELEASE_JS_TESTS') &&
  process.env.TRUSTED_SERVER__RELEASE_JS_TESTS !== '1'
) {
  throw new Error('TRUSTED_SERVER__RELEASE_JS_TESTS must be exactly 1 when present');
}

// The frozen controlled observer is not an ordinary release dependency.
// Updating this gate and the observer requires the controlled-recipe review.
if (process.env.TRUSTED_SERVER__RELEASE_JS_TESTS === '1') {
  const lock = JSON.parse(fs.readFileSync(new URL('./package-lock.json', import.meta.url), 'utf8'));
  const version = lock.packages?.['node_modules/esbuild']?.version;
  if (version !== '0.27.3') {
    throw new Error(
      `Controlled observer pins esbuild 0.27.3; current lock is ${version}; see Phase 2 compatibility doc`
    );
  }
}

export default defineConfig({
  ...(releaseBuild()
    ? {
        envDir: false,
        mode: 'production',
        server: { host: '127.0.0.1' },
        // Preserve native imports that Vite otherwise replaces with browser stubs in production.
        plugins: [
          {
            name: 'release-test-node-builtins',
            enforce: 'pre' as const,
            resolveId(id: string) {
              if (id.startsWith('node:')) return { id, external: true };
            },
          },
        ],
      }
    : {}),
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
