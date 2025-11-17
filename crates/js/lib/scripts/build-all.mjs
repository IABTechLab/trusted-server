#!/usr/bin/env node

import { spawnSync } from 'node:child_process';
import { readdirSync, existsSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const projectRoot = path.resolve(__dirname, '..');
const integrationsDir = path.resolve(projectRoot, 'src', 'integrations');

function discoverIntegrationBundles() {
  if (!existsSync(integrationsDir)) {
    return [];
  }
  return readdirSync(integrationsDir, { withFileTypes: true })
    .filter((entry) => entry.isFile() && entry.name.endsWith('.ts'))
    .map((entry) => entry.name.replace(/\.ts$/i, ''));
}

function runBundle(key) {
  const env = { ...process.env, TSJS_BUNDLE: key };
  const result = spawnSync('npx', ['vite', 'build'], {
    cwd: projectRoot,
    env,
    stdio: 'inherit',
  });
  if (result.status !== 0) {
    process.exit(result.status ?? 1);
  }
}

const baseBundles = ['core', 'ext', 'creative'];
const integrationBundles = discoverIntegrationBundles().map((name) => `integration:${name}`);
const bundles = [...baseBundles, ...integrationBundles];

if (bundles.length === 0) {
  console.warn('tsjs: no bundles discovered; skipping build');
  process.exit(0);
}

for (const bundle of bundles) {
  console.log(`tsjs: building bundle "${bundle}"`);
  runBundle(bundle);
}
