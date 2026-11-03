// @vitest-environment node

import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { Script } from 'node:vm';

import { beforeAll, describe, expect, it } from 'vitest';

import { checkGptBootstrap, renderGptBootstrap } from '../build-gpt-bootstrap.mjs';

const bootstrapPath = new URL(
  '../../../trusted-server-core/src/integrations/gpt_bootstrap.js',
  import.meta.url
);
let generated;

describe('generated standalone GPT bootstrap', () => {
  beforeAll(async () => {
    generated = await renderGptBootstrap();
  });

  it('matches the checked-in artifact that Rust embeds and the shared contracts execute', async () => {
    expect(generated).toBe(await fs.readFile(bootstrapPath, 'utf8'));
    await expect(checkGptBootstrap()).resolves.toBeUndefined();
    expect(generated).toContain('npm run build:gpt-bootstrap -- --write');
  });

  it('rejects stale output without overwriting it', async () => {
    const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'tsjs-gpt-bootstrap-stale-'));
    try {
      const artifact = path.join(directory, 'gpt_bootstrap.js');
      await fs.writeFile(artifact, '// stale output\n');
      await expect(checkGptBootstrap(artifact)).rejects.toThrow(
        'npm run build:gpt-bootstrap -- --write'
      );
      expect(await fs.readFile(artifact, 'utf8')).toBe('// stale output\n');
    } finally {
      await fs.rm(directory, { recursive: true, force: true });
    }
  });

  it('is a classic script without a new global and preserves the preinstalled runtime guard', () => {
    const adInit = () => {};
    const scope = { window: { tsjs: { adInit } } };
    new Script(generated).runInNewContext(scope);
    expect(Object.keys(scope)).toEqual(['window']);
    expect(scope.window).toEqual({ tsjs: { adInit } });
  });

  it('escapes closing inline script tags while preserving string values', async () => {
    const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'tsjs-gpt-bootstrap-'));
    try {
      const entry = path.join(directory, 'entry.js');
      await fs.writeFile(entry, "globalThis.exampleText = '</script><!--';\n");
      const script = await renderGptBootstrap(entry);
      expect(script).not.toMatch(/<\/script/i);
      const scope = {};
      new Script(script).runInNewContext(scope);
      expect(scope.exampleText).toBe('</script><!--');
    } finally {
      await fs.rm(directory, { recursive: true, force: true });
    }
  });
});
