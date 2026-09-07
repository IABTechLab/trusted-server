// @vitest-environment node

import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import { describe, expect, it } from 'vitest';

import {
  deriveBundleMetadata,
  main,
  parseArgs,
  readAdapterBidderCodes,
  renderIncludedUserIdModulesExport,
} from '../build-prebid-external.mjs';

describe('build-prebid-external metadata', () => {
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

      expect(manifest.userIdModules).toEqual(['pairIdSystem', 'lockrAIMIdSystem']);
      expect(manifest.bidderCodes).toEqual(['rubicon']);
      expect(bundle).toContain('"pairIdSystem"');
      expect(bundle).toContain('"lockrAIMIdSystem"');
    } finally {
      fs.rmSync(outputDirectory, { recursive: true, force: true });
    }
  }, 120_000);

  it('builds and stamps identityLinkIdSystem when explicitly selected', async () => {
    const outputDirectory = fs.mkdtempSync(
      path.join(os.tmpdir(), 'trusted-server-liveramp-prebid-build-test-')
    );

    try {
      await main([
        '--adapters',
        'rubicon',
        '--user-id-modules',
        'identityLinkIdSystem',
        '--out',
        outputDirectory,
      ]);

      const manifest = JSON.parse(
        fs.readFileSync(path.join(outputDirectory, 'manifest.json'), 'utf8')
      );
      const bundle = fs.readFileSync(path.join(outputDirectory, manifest.filename), 'utf8');

      expect(manifest.userIdModules).toEqual(['identityLinkIdSystem']);
      expect(bundle).toContain('identityLinkIdSystem');
    } finally {
      fs.rmSync(outputDirectory, { recursive: true, force: true });
    }
  }, 120_000);

  it('resolves relative output paths against the current working directory', () => {
    const parsed = parseArgs(['--adapters', 'rubicon', '--out', 'dist/prebid']);

    expect(parsed.outDir).toBe(path.resolve(process.cwd(), 'dist/prebid'));
  });
});
