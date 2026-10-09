// @vitest-environment node
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

import { traceSourceDigest } from '../trace-asset-sources.mjs';

const library = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex');

describe('versioned trace assets', () => {
  it('retains independently embedded committed bytes with exact URLs and strong digests', () => {
    const manifestFile = path.join(library, 'trace-assets-manifest.json');
    expect(fs.existsSync(manifestFile), 'should commit the trace asset manifest').toBe(true);
    const manifest = JSON.parse(fs.readFileSync(manifestFile, 'utf8'));
    expect(manifest.schema_version).toBe(1);
    expect(manifest.source_sha256).toBe(traceSourceDigest(library));
    expect(manifest.assets.map((asset) => asset.path)).toEqual([
      '/_ts/trace/assets/v1.js',
      '/_ts/trace/assets/v1.css',
    ]);
    for (const asset of manifest.assets) {
      const frozen = fs.readFileSync(path.join(library, 'trace-assets', asset.file));
      const built = fs.readFileSync(path.join(library, '..', 'dist', 'trace', asset.file));
      expect(built).toEqual(frozen);
      expect(sha256(frozen)).toBe(asset.sha256);
      expect(asset.sha256).toMatch(/^[a-f\d]{64}$/);
    }
    expect(fs.existsSync(path.join(library, '..', 'dist', 'tsjs-trace.js'))).toBe(false);
  });
});
