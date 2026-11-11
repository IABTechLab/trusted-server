import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

test(
  'Cloudflare smoke builds a missing CLI before isolating HOME',
  {
    skip: process.platform !== 'linux',
  },
  () => {
    const root = mkdtempSync(join(tmpdir(), 'ts-cloudflare-home-'));
    try {
      const scripts = join(root, 'scripts');
      const bin = join(root, 'bin');
      const home = join(root, 'caller-home');
      for (const directory of [scripts, bin, home]) mkdirSync(directory);
      for (const name of ['smoke-cloudflare.sh', 'smoke-common.sh']) {
        copyFileSync(fileURLToPath(new URL(`../${name}`, import.meta.url)), join(scripts, name));
      }
      writeFileSync(join(root, '.tool-versions'), 'wrangler 4.151.3\n');
      writeFileSync(join(bin, 'wrangler'), '#!/bin/sh\necho 4.151.3\n', { mode: 0o755 });
      writeFileSync(
        join(bin, 'cargo'),
        '#!/bin/sh\nprintf "%s" "$HOME" > "$HOME_TRACE"\nexit 42\n',
        { mode: 0o755 }
      );
      const trace = join(root, 'cargo-home');
      const result = spawnSync('bash', [join(scripts, 'smoke-cloudflare.sh')], {
        env: {
          PATH: `${bin}:${process.env.PATH}`,
          HOME: home,
          TMPDIR: root,
          HOME_TRACE: trace,
        },
        encoding: 'utf8',
        timeout: 10_000,
      });
      assert.equal(result.error, undefined);
      assert.equal(result.status, 42, result.stderr);
      assert.equal(readFileSync(trace, 'utf8'), home);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }
);
