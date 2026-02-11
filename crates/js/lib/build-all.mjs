/**
 * Multi-entry Vite build script.
 *
 * Builds each integration as a separate IIFE file so the Rust server can
 * concatenate only the enabled modules at runtime.
 *
 * Output (in ../dist/):
 *   tsjs-core.js          — core API (always included)
 *   tsjs-<integration>.js — one per discovered integration
 */

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { build } from 'vite';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const srcDir = path.resolve(__dirname, 'src');
const distDir = path.resolve(__dirname, '..', 'dist');
const integrationsDir = path.join(srcDir, 'integrations');

// Clean dist directory
fs.rmSync(distDir, { recursive: true, force: true });
fs.mkdirSync(distDir, { recursive: true });

// Discover integration modules: directories in src/integrations/ with index.ts
const integrationModules = fs.existsSync(integrationsDir)
  ? fs
      .readdirSync(integrationsDir)
      .filter((name) => {
        const fullPath = path.join(integrationsDir, name);
        return (
          fs.statSync(fullPath).isDirectory() &&
          fs.existsSync(path.join(fullPath, 'index.ts'))
        );
      })
      .sort()
  : [];

console.log('[build-all] Discovered integrations:', integrationModules);

/** Build a single module as a self-contained IIFE. */
async function buildModule(name, entryPath) {
  const outFile = `tsjs-${name}.js`;
  console.log(`[build-all] Building ${outFile} from ${path.relative(__dirname, entryPath)}`);

  await build({
    configFile: false,
    root: __dirname,
    build: {
      emptyOutDir: false,
      outDir: distDir,
      assetsDir: '.',
      sourcemap: false,
      minify: 'esbuild',
      rollupOptions: {
        input: entryPath,
        output: {
          format: 'iife',
          dir: distDir,
          entryFileNames: outFile,
          inlineDynamicImports: true,
          extend: false,
          // Use a unique IIFE name per module to avoid conflicts
          name: name === 'core' ? 'tsjs' : `tsjs_${name}`,
        },
      },
    },
    logLevel: 'warn',
  });

  console.log(`[build-all] Built ${outFile}`);
}

// Build core first (synchronously), then all integrations in parallel
await buildModule('core', path.join(srcDir, 'core', 'index.ts'));

await Promise.all(
  integrationModules.map((name) =>
    buildModule(name, path.join(integrationsDir, name, 'index.ts')),
  ),
);

// List all built files
const builtFiles = fs
  .readdirSync(distDir)
  .filter((f) => f.startsWith('tsjs-') && f.endsWith('.js'))
  .sort();

console.log('[build-all] Built files:', builtFiles);
console.log(`[build-all] Total: ${builtFiles.length} modules`);
