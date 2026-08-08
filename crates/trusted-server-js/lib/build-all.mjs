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

import { discoverIntegrationModules } from './scripts/integration-inventory-v1.mjs';
import { computeReleaseId, RELEASE_SENTINEL, stampRelease } from './scripts/release-v1.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const srcDir = path.resolve(__dirname, 'src');
const distDir = path.resolve(__dirname, '..', 'dist');
const integrationsDir = path.join(srcDir, 'integrations');
const metricsFile = 'tsjs-build-metrics-v1.json';
const releaseFile = 'tsjs-release-v1.json';
const fallbackFile = 'gpt-bootstrap-fallback.js';

const REFERENCE_INTEGRATIONS = ['creative', 'gpt', 'prebid', 'datadome'];

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
const integrationModules = discoverIntegrationModules(integrationsDir);

console.log('[build-all] Discovered integrations:', integrationModules);

/** Build a single module as a self-contained IIFE. */
async function buildModule(name, entryPath, outFile = `tsjs-${name}.js`) {
  console.log(`[build-all] Building ${outFile} from ${path.relative(__dirname, entryPath)}`);

  await build({
    configFile: false,
    root: __dirname,
    define: {
      __TSJS_EMBEDDED_RELEASE_ID_V1__: JSON.stringify(RELEASE_SENTINEL),
      __TSJS_EMBEDDED_INTEGRATION_IDS_V1__: JSON.stringify(integrationModules),
    },
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
await buildModule('core', path.join(srcDir, 'composition', 'index.ts'));

await Promise.all(
  integrationModules.map((name) => buildModule(name, path.join(integrationsDir, name, 'index.ts')))
);
await buildModule(
  'gpt_bootstrap_fallback',
  path.join(integrationsDir, 'gpt', 'bootstrap_fallback.ts'),
  fallbackFile
);

// List all built files
const builtFiles = fs
  .readdirSync(distDir)
  .filter((f) => f.startsWith('tsjs-') && f.endsWith('.js'))
  .sort((left, right) => {
    if (left === 'tsjs-core.js') return -1;
    if (right === 'tsjs-core.js') return 1;
    return left < right ? -1 : left > right ? 1 : 0;
  });

for (const file of builtFiles) {
  const filePath = path.join(distDir, file);
  const source = fs.readFileSync(filePath, 'utf8');
  const sentinelCount = source.split(RELEASE_SENTINEL).length - 1;
  if (sentinelCount > 1) {
    throw new Error(`[build-all] Multiple release sentinels before stamping: ${file}`);
  }
  if (sentinelCount === 0) {
    fs.writeFileSync(filePath, `${source}\n;void"${RELEASE_SENTINEL}";\n`);
  }
}

const releaseId = computeReleaseId(
  builtFiles.map((file) => ({
    id: file.slice('tsjs-'.length, -'.js'.length),
    bytes: fs.readFileSync(path.join(distDir, file)),
  }))
);
for (const file of builtFiles) {
  const filePath = path.join(distDir, file);
  const source = fs.readFileSync(filePath, 'utf8');
  fs.writeFileSync(filePath, stampRelease(source, releaseId));
}
const fallbackPath = path.join(distDir, fallbackFile);
const fallbackSource = fs.readFileSync(fallbackPath, 'utf8');
fs.writeFileSync(fallbackPath, stampRelease(fallbackSource, releaseId));

const releaseManifest = {
  version: 1,
  releaseId,
  bundles: builtFiles.map((file) => ({
    id: file.slice('tsjs-'.length, -'.js'.length),
    file,
  })),
};
fs.writeFileSync(path.join(distDir, releaseFile), `${JSON.stringify(releaseManifest)}\n`);

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
console.log(`[build-all] Wrote release manifest: ${releaseFile}`);
console.log(`[build-all] Wrote proposed fallback artifact: ${fallbackFile}`);
