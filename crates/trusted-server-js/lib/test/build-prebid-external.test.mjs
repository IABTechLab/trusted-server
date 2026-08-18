// @vitest-environment node

import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import { describe, expect, it } from 'vitest';

import {
  ARTIFACT_RELEASE_SENTINEL,
  assertNoLegacyRuntimeFlags,
  deriveBundleMetadata,
  main,
  parseArgs,
  readAdapterBidderCodes,
  readAdapterMetadata,
  renderIncludedUserIdModulesExport,
} from '../build-prebid-external.mjs';

describe('build-prebid-external metadata', () => {
  it('rejects any legacy TSJS runtime flag before publishing an artifact', () => {
    expect(() => assertNoLegacyRuntimeFlags('window.' + '__' + 'tsjs_prebid = {};')).toThrow(
      /legacy TSJS runtime flag/
    );
    expect(() => assertNoLegacyRuntimeFlags('window.pbjs = { que: [] };')).not.toThrow();
  });

  it('derives filename, sha256, and SRI from exact bundle bytes', () => {
    const bundleBytes = Buffer.from('console.log("trusted prebid");\n', 'utf8');
    const sha256 = crypto.createHash('sha256').update(bundleBytes).digest('hex');
    const sri = `sha384-${crypto.createHash('sha384').update(bundleBytes).digest('base64')}`;

    expect(deriveBundleMetadata(bundleBytes)).toEqual({
      filename: `trusted-prebid-${sha256}.js`,
      sha256,
      sri,
    });
  });

  it('renders the exact selected User ID modules for runtime diagnostics', () => {
    expect(renderIncludedUserIdModulesExport(['liveIntentIdSystem', 'pairIdSystem'])).toBe(
      'export const INCLUDED_PREBID_USER_ID_MODULES = ["liveIntentIdSystem","pairIdSystem"];'
    );
  });

  it('derives registered bidder codes including aliases from prebid metadata', () => {
    // adfBidAdapter.js registers adf plus the adform/adformOpenRTB aliases.
    expect(readAdapterBidderCodes(['adf'])).toEqual(['adf', 'adform', 'adformOpenRTB']);
    expect(readAdapterMetadata(['adf']).bidderAliases).toEqual([
      { code: 'adform', moduleStem: 'adf' },
      { code: 'adformOpenRTB', moduleStem: 'adf' },
    ]);
  });

  it('maps a module file stem to its registered bidder code', () => {
    // a1MediaBidAdapter.js registers a1media — the stem itself is not a code.
    const bidderCodes = readAdapterBidderCodes(['a1Media']);
    expect(bidderCodes).toContain('a1media');
    expect(bidderCodes).not.toContain('a1Media');
  });

  it('falls back to the module stem when no metadata is shipped', () => {
    expect(readAdapterBidderCodes(['noSuchAdapterEver'])).toEqual(['noSuchAdapterEver']);
  });

  it('includes generated User ID metadata in the production external bundle', async () => {
    const outputDirectory = fs.mkdtempSync(
      path.join(os.tmpdir(), 'trusted-server-prebid-build-test-')
    );

    try {
      await main([
        '--adapters',
        'rubicon',
        '--user-id-modules',
        'pairIdSystem,lockrAIMIdSystem',
        '--out',
        outputDirectory,
      ]);

      const manifest = JSON.parse(
        fs.readFileSync(path.join(outputDirectory, 'manifest.json'), 'utf8')
      );
      const bundle = fs.readFileSync(path.join(outputDirectory, manifest.filename), 'utf8');

      expect(manifest).toMatchObject({
        abi: 1,
        prebidVersion: '10.26.0',
        moduleStems: ['lockrAIMIdSystem', 'pairIdSystem', 'rubicon'],
        bidderCodes: ['rubicon'],
        bidderAliases: [],
        userIdModules: [
          {
            moduleName: 'lockrAIMIdSystem',
            configNames: ['lockrAIMId'],
            eidSources: [],
          },
          {
            moduleName: 'pairIdSystem',
            configNames: ['pairId'],
            eidSources: ['google.com'],
          },
        ],
      });
      expect(manifest.artifactReleaseId).toMatch(/^[0-9a-f]{64}$/);
      expect(manifest.filename).toMatch(/^trusted-prebid-[0-9a-f]{64}\.js$/);
      expect(manifest.sha256).toMatch(/^[0-9a-f]{64}$/);
      expect(manifest.sri).toMatch(/^sha384-/);
      expect(bundle).toContain('__trustedServerArtifactV1');
      expect(bundle).toContain('getBidResponsesForAdUnitCode');
      expect(bundle).toContain(manifest.artifactReleaseId);
      expect(bundle).not.toContain(ARTIFACT_RELEASE_SENTINEL);
      expect(bundle).not.toContain('__' + 'tsjs_');
      expect(manifest.bidderCodes).toEqual(['rubicon']);
      expect(bundle.split(manifest.artifactReleaseId)).toHaveLength(2);
      const normalized = bundle.replace(manifest.artifactReleaseId, ARTIFACT_RELEASE_SENTINEL);
      expect(crypto.createHash('sha256').update(normalized).digest('hex')).toBe(
        manifest.artifactReleaseId
      );
      expect(crypto.createHash('sha256').update(bundle).digest('hex')).toBe(manifest.sha256);
    } finally {
      fs.rmSync(outputDirectory, { recursive: true, force: true });
    }
  }, 120_000);

  it('resolves relative output paths against the current working directory', () => {
    const parsed = parseArgs(['--adapters', 'rubicon', '--out', 'dist/prebid']);

    expect(parsed.outDir).toBe(path.resolve(process.cwd(), 'dist/prebid'));
  });

  it('canonicalizes module order and rejects duplicate module names', () => {
    expect(parseArgs(['--adapters', 'rubicon,adf']).adapters).toEqual(['adf', 'rubicon']);
    expect(() => parseArgs(['--adapters', 'rubicon,rubicon'])).toThrow(/duplicates/);
  });
});
