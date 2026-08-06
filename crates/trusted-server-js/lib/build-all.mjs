/**
 * Multi-entry Vite build script.
 *
 * Builds each integration as a separate IIFE file so the Rust server can
 * concatenate only the enabled modules at runtime.
 *
 * Output (in ../dist/):
 *   tsjs-core.js          — core API (always included)
 *   tsjs-<integration>.js — one per discovered integration
 *
 * The prebid integration builds here as the tsjs shim only — Prebid.js itself
 * is never bundled into tsjs. Use build-prebid-external.mjs to generate the
 * pure Prebid.js external bundle (core + adapters + user ID modules) that the
 * shim requires at runtime via integrations.prebid.external_bundle_url.
 */

import fs from 'node:fs';
import { createHash } from 'node:crypto';
import path from 'node:path';
import { brotliCompressSync, constants as zlibConstants, gzipSync } from 'node:zlib';
import { fileURLToPath } from 'node:url';
import { build } from 'vite';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const srcDir = path.resolve(__dirname, 'src');
const distDir = path.resolve(__dirname, '..', 'dist');
const integrationsDir = path.join(srcDir, 'integrations');
const metricsFile = 'tsjs-build-metrics-v1.json';

const REFERENCE_INTEGRATIONS = ['creative', 'gpt', 'prebid'];

function compress(bytes) {
  return {
    gzipBytes: gzipSync(bytes, { level: 9, mtime: 0 }).byteLength,
    brotliBytes: brotliCompressSync(bytes, {
      params: {
        [zlibConstants.BROTLI_PARAM_MODE]: zlibConstants.BROTLI_MODE_TEXT,
        [zlibConstants.BROTLI_PARAM_QUALITY]: 11,
        [zlibConstants.BROTLI_PARAM_SIZE_HINT]: bytes.byteLength,
      },
    }).byteLength,
  };
}

function measureBundleSet(files) {
  const separator = Buffer.from('\n;\n', 'utf8');
  const parts = files.flatMap((file, index) => {
    const bytes = fs.readFileSync(path.join(distDir, file));
    return index === files.length - 1 ? [bytes] : [bytes, separator];
  });
  const bytes = Buffer.concat(parts);
  return {
    files,
    rawBytes: bytes.byteLength,
    ...compress(bytes),
    sha256: createHash('sha256').update(bytes).digest('hex'),
  };
}

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
          fs.statSync(fullPath).isDirectory() && fs.existsSync(path.join(fullPath, 'index.ts'))
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
  integrationModules.map((name) => buildModule(name, path.join(integrationsDir, name, 'index.ts')))
);

// List all built files
const builtFiles = fs
  .readdirSync(distDir)
  .filter((f) => f.startsWith('tsjs-') && f.endsWith('.js'))
  .sort();

const referenceFiles = ['tsjs-core.js', ...REFERENCE_INTEGRATIONS.map((name) => `tsjs-${name}.js`)];
for (const file of referenceFiles) {
  if (!builtFiles.includes(file)) {
    throw new Error(`[build-all] Reference bundle file was not built: ${file}`);
  }
}

const metrics = {
  schemaVersion: 1,
  compression: {
    concatenationSeparator: '\\n;\\n',
    gzipLevel: 9,
    gzipMtime: 0,
    brotliMode: 'text',
    brotliQuality: 11,
  },
  modules: builtFiles.map((file) => {
    const bytes = fs.readFileSync(path.join(distDir, file));
    return {
      file,
      rawBytes: bytes.byteLength,
      sha256: createHash('sha256').update(bytes).digest('hex'),
    };
  }),
  sets: {
    minimal: measureBundleSet(['tsjs-core.js']),
    reference: measureBundleSet(referenceFiles),
    maximal: measureBundleSet(builtFiles),
  },
};

fs.writeFileSync(path.join(distDir, metricsFile), `${JSON.stringify(metrics, null, 2)}\n`);

console.log('[build-all] Built files:', builtFiles);
console.log(`[build-all] Total: ${builtFiles.length} modules`);
console.log(`[build-all] Wrote deterministic metrics: ${metricsFile}`);
