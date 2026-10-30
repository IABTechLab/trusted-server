// @vitest-environment node
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { afterEach, describe, expect, it } from 'vitest';
import { traceSourceDigest } from '../trace-asset-sources.mjs';

const library = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const temporary = [];
afterEach(() => {
  for (const directory of temporary.splice(0))
    fs.rmSync(directory, { recursive: true, force: true });
});
function fixture() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'trace-build-inputs-'));
  temporary.push(root);
  const directory = path.join(root, 'crates/trusted-server-js/lib');
  fs.mkdirSync(directory, { recursive: true });
  fs.copyFileSync(
    path.resolve(library, '../../../.tool-versions'),
    path.join(root, '.tool-versions')
  );
  for (const name of [
    'tsconfig.json',
    'package.json',
    'package-lock.json',
    'build-all.mjs',
    'build-module-options.mjs',
    'build-trace.mjs',
    'build-trace-assets.mjs',
    'trace-build-inputs.json',
    'trace-asset-sources.mjs',
    'trace-assets-manifest.json',
    'trace-assets',
    'src/trace',
  ]) {
    if (fs.existsSync(path.join(library, name))) {
      fs.cpSync(path.join(library, name), path.join(directory, name), { recursive: true });
    }
  }
  return directory;
}
function changePackage(directory, name) {
  const file = path.join(directory, 'package-lock.json');
  const lock = JSON.parse(fs.readFileSync(file, 'utf8'));
  lock.packages[`node_modules/${name}`].version += '-changed';
  fs.writeFileSync(file, JSON.stringify(lock));
}
describe('trace build provenance', () => {
  it('ignores unrelated publisher configuration and dependency changes', () => {
    const directory = fixture();
    const before = traceSourceDigest(directory);
    fs.appendFileSync(path.join(directory, 'build-all.mjs'), '\n// publisher-only change\n');
    fs.appendFileSync(
      path.join(directory, 'build-module-options.mjs'),
      '\n// publisher-only change\n'
    );
    changePackage(directory, 'prebid.js');
    fs.writeFileSync(path.resolve(directory, '../../../.tool-versions'), 'nodejs 25.0.0\n');
    expect(traceSourceDigest(directory)).toBe(before);
  });
  it('tracks trace build settings, nested sources and resolved toolchain dependencies', () => {
    for (const change of [
      (directory) =>
        fs.appendFileSync(path.join(directory, 'build-trace.mjs'), '\n// trace setting\n'),
      (directory) => fs.writeFileSync(path.join(directory, 'src/trace/new-input.json'), '{}'),
      (directory) => fs.appendFileSync(path.join(directory, 'tsconfig.json'), '\n'),
      (directory) => changePackage(directory, 'vite'),
      (directory) => changePackage(directory, 'esbuild'),
      (directory) => changePackage(directory, 'postcss'),
    ]) {
      const directory = fixture();
      const before = traceSourceDigest(directory);
      change(directory);
      expect(traceSourceDigest(directory)).not.toBe(before);
    }
  });
  it('regenerates frozen assets and all manifest digests deterministically', () => {
    const directory = fixture();
    fs.symlinkSync(path.join(library, 'node_modules'), path.join(directory, 'node_modules'), 'dir');
    const build = () =>
      execFileSync(process.execPath, [path.join(directory, 'build-trace-assets.mjs')], {
        cwd: directory,
        stdio: 'pipe',
      });
    const snapshot = () =>
      ['trace-assets-manifest.json', 'trace-assets/v1.js', 'trace-assets/v1.css'].map((file) =>
        fs.readFileSync(path.join(directory, file))
      );
    build();
    const first = snapshot();
    // Ambient PostCSS configuration must not participate in this isolated build.
    fs.writeFileSync(
      path.join(directory, 'postcss.config.mjs'),
      'throw new Error("ambient PostCSS config should not load");\n'
    );
    build();
    expect(snapshot()).toEqual(first);
    const manifest = JSON.parse(first[0].toString('utf8'));
    expect(
      manifest.source_sha256,
      'should match trace build inputs; run `npm run build:trace-assets` to regenerate'
    ).toBe(traceSourceDigest(directory));
    for (const asset of manifest.assets) {
      expect(
        createHash('sha256')
          .update(fs.readFileSync(path.join(directory, 'trace-assets', asset.file)))
          .digest('hex')
      ).toBe(asset.sha256);
    }
  });
});
