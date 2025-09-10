import { defineConfig } from 'vite'

// Build in library mode to a sibling dist/ folder so the Rust crate can embed it.
export default defineConfig({
  build: {
    lib: {
      entry: 'src/index.ts',
      name: 'tsjs',
      formats: ['iife'],
      fileName: () => 'tsjs-core.js',
    },
    minify: 'esbuild',
    sourcemap: false,
    outDir: '../dist',
    emptyOutDir: false,
    assetsDir: '.',
  },
})
