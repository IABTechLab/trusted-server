import crypto from 'node:crypto';
import path from 'node:path';

import { describe, expect, it } from 'vitest';

import {
  deriveBundleMetadata,
  parseArgs,
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

  it('resolves relative output paths against the current working directory', () => {
    const parsed = parseArgs(['--adapters', 'rubicon', '--out', 'dist/prebid']);

    expect(parsed.outDir).toBe(path.resolve(process.cwd(), 'dist/prebid'));
  });
});
