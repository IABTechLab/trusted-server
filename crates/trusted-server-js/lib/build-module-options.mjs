import path from 'node:path';
import { fileURLToPath } from 'node:url';

const libDir = path.dirname(fileURLToPath(import.meta.url));

/** Production Vite options for one independently bundled tsjs IIFE. */
export function moduleBuildOptions({ name, entryPath, outDir }) {
  return {
    configFile: false,
    root: libDir,
    // The publisher bootstrap is embedded in HTML script text. Keep escaping
    // enabled explicitly for strings such as </script> and <!-- in templates.
    esbuild: { supported: { 'inline-script': true } },
    build: {
      emptyOutDir: false,
      outDir,
      assetsDir: '.',
      sourcemap: false,
      minify: 'esbuild',
      rollupOptions: {
        input: entryPath,
        output: {
          format: 'iife',
          dir: outDir,
          entryFileNames: `tsjs-${name}.js`,
          inlineDynamicImports: true,
          extend: false,
          // Use a unique IIFE name per module to avoid conflicts.
          name: name === 'core' ? 'tsjs' : `tsjs_${name}`,
        },
      },
    },
    logLevel: 'warn',
  };
}
