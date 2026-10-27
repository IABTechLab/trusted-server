/**
 * Multi-entry Vite build script.
 *
 * Builds each integration as a separate IIFE file so the Rust server can
 * concatenate only the enabled modules at runtime.
 *
 * Output (in ../dist/, or the directory passed as `--out-dir <dir>`):
 *   tsjs-core.js          — core API (always included)
 *   tsjs-<integration>.js — one per discovered integration
 *
 * The prebid integration builds here as the tsjs shim only — Prebid.js itself
 * is never bundled into tsjs. Use build-prebid-external.mjs to generate the
 * pure Prebid.js external bundle (core + adapters + user ID modules) that the
 * shim requires at runtime via integrations.prebid.external_bundle_url.
 *
 * The Rust build script passes `--out-dir` with a private directory under
 * Cargo's OUT_DIR, so concurrent cargo builds never share (or clean) the same
 * output directory.
 */

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { build } from 'vite';

import { moduleBuildOptions } from './build-module-options.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

/** Resolve `--out-dir <dir>` (relative to the cwd), defaulting to ../dist. */
function resolveOutDir(args) {
  const flagIndex = args.indexOf('--out-dir');
  if (flagIndex === -1) {
    return path.resolve(__dirname, '..', 'dist');
  }
  const value = args[flagIndex + 1];
  if (!value || value.startsWith('--')) {
    throw new Error('[build-all] --out-dir requires a directory argument');
  }
  return path.resolve(value);
}

/** Build the discovered modules; recording is opt-in and cannot change bundle bytes. */
export async function buildAll({
  outDir,
  composition,
  sourceDir = path.resolve(__dirname, 'src'),
}) {
  const srcDir = sourceDir;
  const distDir = path.resolve(outDir);
  const integrationsDir = path.join(srcDir, 'integrations');
  const records = [];

  // Remove only the bundles this script writes, so a wrong --out-dir cannot
  // delete unrelated files.
  fs.mkdirSync(distDir, { recursive: true });
  for (const name of fs.readdirSync(distDir)) {
    if (name.startsWith('tsjs-') && name.endsWith('.js')) {
      fs.rmSync(path.join(distDir, name));
    }
  }

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

    let contributions;
    const recording = composition
      ? {
          ...composition,
          onContributions: (rows) => {
            contributions = rows;
          },
        }
      : undefined;
    await build(moduleBuildOptions({ name, entryPath, outDir: distDir, composition: recording }));
    records.push({ id: name, filename: outFile, contributions });

    console.log(`[build-all] Built ${outFile}`);
  }

  // Build core first (synchronously), then all integrations in parallel
  await buildModule('core', path.join(srcDir, 'core', 'index.ts'));

  await Promise.all(
    integrationModules.map((name) =>
      buildModule(name, path.join(integrationsDir, name, 'index.ts'))
    )
  );

  // List all built files
  const builtFiles = fs
    .readdirSync(distDir)
    .filter((f) => f.startsWith('tsjs-') && f.endsWith('.js'))
    .sort();

  console.log('[build-all] Built files:', builtFiles);
  console.log(`[build-all] Total: ${builtFiles.length} modules`);

  return records.sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
}

if (import.meta.main) {
  if (
    !process.argv[1] ||
    import.meta.url !== pathToFileURL(fs.realpathSync(process.argv[1])).href
  ) {
    throw new Error('build-all entrypoint identity check failed');
  }
  await buildAll({ outDir: resolveOutDir(process.argv.slice(2)) });
}
