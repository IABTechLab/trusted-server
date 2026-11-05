import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { pathToFileURL, fileURLToPath } from 'node:url';

import { assertBrowserBuildLease } from '../../../scripts/browser-build-lease.mjs';

await assertBrowserBuildLease();
const library = path.dirname(fileURLToPath(import.meta.url));
const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'browser-two-root-canary-'));
const root = path.join(temporary, 'crates/trusted-server-js/lib');
const sibling = path.join(temporary, 'crates/trusted-server-integrations-js/lib/src');
try {
  fs.mkdirSync(path.join(root, 'src'), { recursive: true });
  fs.mkdirSync(sibling, { recursive: true });
  for (const file of [
    'tsconfig.json',
    'tsconfig.production.json',
    'browser-env.d.ts',
    'browser-resolution.mjs',
  ]) {
    fs.copyFileSync(path.join(library, file), path.join(root, file));
  }
  fs.symlinkSync(path.join(library, 'node_modules'), path.join(root, 'node_modules'), 'dir');
  fs.writeFileSync(path.join(root, 'src/index.ts'), 'export const neutral: number = 1;\n');
  const source = [
    "import type prebid from 'prebid.js';",
    "import inline from './asset.html?inline';",
    "import raw from './asset.html?raw';",
    "const elements = [...document.querySelectorAll('div')];",
    'export const selected: number = 1;',
  ].join('\n');
  const canary = path.join(sibling, 'canary.ts');
  fs.writeFileSync(canary, source);
  const check = () =>
    spawnSync(
      process.execPath,
      [
        path.join(library, 'node_modules/typescript/bin/tsc'),
        '-p',
        path.join(root, 'tsconfig.production.json'),
      ],
      { cwd: root, encoding: 'utf8' }
    );
  let result = check();
  assert.equal(result.status, 0, result.stdout + result.stderr);
  fs.writeFileSync(
    canary,
    source.replace('selected: number = 1', "selected: number = 'sibling-selected'")
  );
  result = check();
  assert.equal(result.status, 2, 'the sibling canary must fail production typechecking');
  assert.equal(result.stderr, '');
  assert.equal(
    result.stdout.trim(),
    "../../trusted-server-integrations-js/lib/src/canary.ts(5,14): error TS2322: Type 'string' is not assignable to type 'number'."
  );
  console.log(
    'Two-root canary: positive fixture passed; sibling fixture failed with exactly TS2322 at line 5.'
  );

  const { createServer } = await import('vite');
  const { browserPackageResolution } = await import(
    pathToFileURL(path.join(root, 'browser-resolution.mjs')).href
  );
  const server = await createServer({
    configFile: false,
    root,
    plugins: [browserPackageResolution()],
    optimizeDeps: { noDiscovery: true, exclude: ['prebid.js'] },
    server: { middlewareMode: true, watch: null },
  });
  try {
    const resolved = await server.pluginContainer.resolveId(
      'prebid.js/modules/rubiconBidAdapter.js',
      canary
    );
    assert.ok(
      resolved?.id.endsWith('/prebid.js/dist/src/public/rubiconBidAdapter.js'),
      JSON.stringify(resolved)
    );
    await assert.rejects(
      server.pluginContainer.resolveId('prebid.js/src/adapterManager.js', canary),
      /Missing.*specifier/
    );
    console.log(
      'Two-root resolver: exported Prebid module resolves; unexported subpath is rejected.'
    );
  } finally {
    await server.close();
  }
} finally {
  fs.rmSync(temporary, { recursive: true, force: true });
}
