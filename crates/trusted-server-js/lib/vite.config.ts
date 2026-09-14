import { defineConfig } from 'vite';

// Build configuration has moved to build-all.mjs.
// This file is retained for vitest, which uses it for test resolution.
export default defineConfig({
  define: {
    __TSJS_EMBEDDED_MAX_MANIFEST_MODULES_V1__: JSON.stringify(20),
  },
});
