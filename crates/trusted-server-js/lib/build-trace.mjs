import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { build } from 'vite';

const library = path.dirname(fileURLToPath(import.meta.url));

/** Build only the independent versioned trace viewer into the supplied directory. */
export async function buildTraceAssets(outDir) {
  // The full-document trace viewer is independent of publisher integration bundles.
  await build({
    configFile: false,
    root: library,
    css: { postcss: {} },
    build: {
      emptyOutDir: false,
      outDir,
      sourcemap: false,
      minify: 'esbuild',
      cssCodeSplit: false,
      rollupOptions: {
        input: path.join(library, 'src/trace/viewer.ts'),
        output: {
          format: 'iife',
          inlineDynamicImports: true,
          entryFileNames: 'trace/v1.js',
          assetFileNames: 'trace/v1.css',
          name: 'tsTraceViewer',
        },
      },
    },
    logLevel: 'warn',
  });
}
