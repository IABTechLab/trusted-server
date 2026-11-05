import { defineConfig } from 'vite';

import { assertBrowserBuildLease } from '../../../scripts/browser-build-lease.mjs';

import { browserPackageResolution } from './browser-resolution.mjs';

await assertBrowserBuildLease();

// Build configuration has moved to build-all.mjs.
// This file is retained for vitest, which uses it for test resolution.
export default defineConfig({ plugins: [browserPackageResolution()] });
