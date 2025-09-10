import { defineConfig } from 'vite';

// Build the Prebid.js shim extension as a separate IIFE bundle.
export default defineConfig({
  build: {
    lib: {
      entry: 'src/ext/ext.entry.ts',
      name: 'tsjs',
      formats: ['iife'],
      fileName: () => 'tsjs-ext.js',
    },
    minify: 'esbuild',
    sourcemap: false,
    outDir: '../dist',
    emptyOutDir: false,
    assetsDir: '.',
  },
});
