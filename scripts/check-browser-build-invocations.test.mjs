import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { test } from 'node:test';

import { checkCommands, checkRepository, checkWorkflow } from './check-browser-build-invocations.mjs';

const root = 'crates/trusted-server-js/lib';
const wrapped = 'node ../../../scripts/browser-build-runner.mjs -- ';

test('repository scan finds new workflow/tool violations and missing direct guards', (t) => {
  const repository = fs.mkdtempSync(path.join(os.tmpdir(), 'browser-invocation-guard-'));
  t.after(() => fs.rmSync(repository, { recursive: true, force: true }));
  const put = (relative, text) => {
    const file = path.join(repository, relative);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, text);
  };
  put('.github/workflows/new.yml', `jobs:\n  build:\n    defaults:\n      run:\n        working-directory: ${root}\n    steps:\n      - run: npm ci\n`);
  put('scripts/new.sh', `cd ${root}\nnpx tsc`);
  put(`${root}/package.json`, JSON.stringify({ scripts: { build: wrapped + 'node build-all.mjs' } }));
  for (const file of ['build-all.mjs', 'build-prebid-external.mjs', 'build-trace.mjs', 'build-trace-assets.mjs', 'browser-tools.mjs', 'check-two-root-typecheck.mjs']) put(`${root}/${file}`, 'await assertBrowserBuildLease();');
  put(`${root}/new-builder.mjs`, "import { build } from 'vite';\nawait build({});");
  put('crates/trusted-server-js/build.rs', 'browser-build-runner.mjs assert-browser-build-lease.mjs');
  put('crates/trusted-server-cli/src/prebid_bundle.rs', 'browser-build-runner.mjs');
  const findings = checkRepository(repository);
  assert.equal(findings.length, 3, findings.join('\n'));
  assert.ok(findings.some((line) => line.includes('new.yml')));
  assert.ok(findings.some((line) => line.includes('new.sh')));
  assert.ok(findings.some((line) => line.includes('new-builder.mjs: missing live lease assertion')));
});

test('canonical npm/tool calls require the runner', () => {
  for (const tool of ['npm ci', 'npm run build', 'npx vitest run', 'vite build', 'tsc --noEmit', 'eslint src', 'prettier --check src', 'node build-all.mjs', './node_modules/.bin/tsc --noEmit', 'node node_modules/prettier/bin/prettier.cjs --check src']) {
    assert.equal(checkCommands(tool, root).length, 1, tool);
    assert.deepEqual(checkCommands(wrapped + tool, root), []);
  }
});

test('root-relative explicit production paths are checked', () => {
  assert.equal(checkCommands(`npm ci --prefix ${root}`).length, 1);
  assert.equal(checkCommands(`node ${root}/build-all.mjs`).length, 1);
  assert.deepEqual(checkCommands('node scripts/browser-build-runner.mjs -- npm ci'), []);
});

test('shell cwd and variable expansion select canonical project only', () => {
  assert.equal(checkCommands(`TSJS_LIB_DIR="${root}"\ncd "$REPO_ROOT/$TSJS_LIB_DIR"\nnpx tsc`).length, 1);
  assert.deepEqual(checkCommands('BROWSER_DIR="crates/trusted-server-integration-tests/browser"\ncd "$REPO_ROOT/$BROWSER_DIR"\nnpm ci\nnpx playwright test'), []);
  assert.deepEqual(checkCommands('cd docs\nnpm ci\nnpm run build'), []);
});

test('subshells, relative cwd and continuation lines do not bypass selection', () => {
  assert.equal(checkCommands(`(cd ${root} && npm ci)`).length, 1);
  assert.equal(checkCommands('cd ../trusted-server-js/lib\nnpx tsc').length, 1);
  assert.equal(checkCommands(`npm \\\n ci --prefix ${root}`).length, 1);
});

test('a wrapper does not bless subsequent bare commands', () => {
  assert.equal(checkCommands(`${wrapped}npm run build && npx tsc`, root).length, 1);
  assert.equal(checkCommands(`${wrapped}npm run build; npm ci`, root).length, 1);
  assert.equal(checkCommands(`npm ci | ${wrapped}node browser-tools.mjs test`, root).length, 1);
});

test('workflow defaults and step cwd overrides retain project boundaries', () => {
  const source = `jobs:
  checks:
    defaults:
      run:
        working-directory: ${root}
    steps:
      - name: Unsafe canonical
        run: npm ci
      - name: Wrapped canonical
        run: ${wrapped}npm ci
      - name: Independent docs
        working-directory: docs
        run: npm ci
      - name: Reset to canonical default
        run: npx vitest
      - name: Post-run cwd override
        run: npm ci
        working-directory: docs
  playwright:
    defaults:
      run:
        working-directory: crates/trusted-server-integration-tests/browser
    steps:
      - name: Independent browser
        run: |
          npm ci
          npx playwright test
`;
  const findings = checkWorkflow(source);
  assert.equal(findings.length, 2, findings.join('\n'));
  assert.ok(findings.some((line) => line.includes('npm ci')));
  assert.ok(findings.some((line) => line.includes('npx vitest')));
});

test('folded commands and cwd after run still select explicit production tools', () => {
  const source = `jobs:
  build:
    steps:
      - name: Canonical folded invocation
        run: >-
          npm ci --prefix
          ${root}
      - name: Canonical post-run cwd
        run: npx tsc
        working-directory: ${root}
`;
  assert.equal(checkWorkflow(source).length, 2);
});
