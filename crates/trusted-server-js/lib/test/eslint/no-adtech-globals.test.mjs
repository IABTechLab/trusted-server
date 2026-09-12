import assert from 'node:assert/strict';
import { readFile, readdir } from 'node:fs/promises';
import path from 'node:path';
import test from 'node:test';

import { ESLint, Linter } from 'eslint';
import ts from 'typescript';

import noAdtechGlobals from '../../eslint-rules/no-adtech-globals.js';
import { ARCHITECTURE_INTEGRATION_DIRECTORIES } from '../../eslint.config.js';

const ruleId = 'tsjs/no-adtech-globals';
const packageRoot = path.resolve(import.meta.dirname, '../..');

function lint(source, filename = 'src/kernel/new-runtime.js') {
  const linter = new Linter({ configType: 'flat' });

  return linter.verify(
    source,
    [
      {
        files: ['**/*.js', '**/*.ts', '**/*.tsx'],
        languageOptions: {
          ecmaVersion: 'latest',
          sourceType: 'module',
          globals: {
            globalThis: 'readonly',
            self: 'readonly',
            window: 'readonly',
          },
        },
        plugins: {
          tsjs: {
            rules: {
              'no-adtech-globals': noAdtechGlobals,
            },
          },
        },
        rules: {
          [ruleId]: 'error',
        },
      },
    ],
    { filename }
  );
}

function assertRejected(source, filename) {
  const messages = lint(source, filename);
  assert.ok(messages.length > 0, `expected an ad-tech-global error for: ${source}`);
  assert.ok(messages.every((message) => message.ruleId === ruleId));
  assert.ok(messages.every((message) => message.messageId === 'externalGlobalOwnedByAdapter'));
}

async function collectRelativeModuleGraph(entry) {
  const pending = [path.resolve(packageRoot, entry)];
  const visited = new Set();
  while (pending.length > 0) {
    const filename = pending.pop();
    if (!filename || visited.has(filename)) continue;
    visited.add(filename);
    const source = await readFile(filename, 'utf8');
    const sourceFile = ts.createSourceFile(filename, source, ts.ScriptTarget.Latest, false);
    for (const statement of sourceFile.statements) {
      const moduleSpecifier =
        (ts.isImportDeclaration(statement) || ts.isExportDeclaration(statement)) &&
        statement.moduleSpecifier &&
        ts.isStringLiteral(statement.moduleSpecifier)
          ? statement.moduleSpecifier.text
          : undefined;
      if (!moduleSpecifier?.startsWith('.')) continue;
      const unresolved = path.resolve(path.dirname(filename), moduleSpecifier);
      const candidates = [
        unresolved,
        `${unresolved}.ts`,
        `${unresolved}.tsx`,
        path.join(unresolved, 'index.ts'),
      ];
      let resolved;
      for (const candidate of candidates) {
        try {
          await readFile(candidate, 'utf8');
          resolved = candidate;
          break;
        } catch {
          // Try the next TypeScript module resolution candidate.
        }
      }
      assert.ok(resolved, `could not resolve ${moduleSpecifier} from ${filename}`);
      pending.push(resolved);
    }
  }
  return [...visited].map((filename) => path.relative(packageRoot, filename).replaceAll('\\', '/'));
}

test('rejects direct GPT and Prebid access through every browser global root', () => {
  for (const source of [
    'window.googletag.cmd.push(run);',
    "globalThis['pbjs'].requestBids();",
    'window[`googletag`].cmd.push(run);',
    'self.googletag?.pubads();',
    'googletag.cmd.push(run);',
    'pbjs.requestBids();',
  ]) {
    assertRejected(source);
  }
});

test('rejects same-file aliases of roots and external objects', () => {
  for (const source of [
    'const root = window; root.googletag.cmd.push(run);',
    'const first = globalThis; const second = first; second.pbjs.requestBids();',
    'let root; root = self; root.googletag.pubads();',
    'const tag = window.googletag; tag.cmd.push(run);',
    'const prebid = globalThis.pbjs; prebid.requestBids();',
    'const { googletag: tag } = window; tag.cmd.push(run);',
    'const { pbjs: prebid } = self; prebid.requestBids();',
    'const { googletag } = window;',
    "let prebid; ({ ['pbjs']: prebid } = globalThis);",
    'let root; ({ window: root } = globalThis); root.pbjs.requestBids();',
    'const { ...root } = window; root.googletag.cmd.push(run);',
    'const root = (0, window); root.pbjs.requestBids();',
    'class Owner { bind() { this.root = window; } read() { return this.root.googletag; } }',
    'class Owner { root = window; read() { return this.root.pbjs; } }',
    'class Owner { bind() { this.root = this.root ?? window; } read() { return this.root.pbjs; } }',
    'class Owner { #root = window; read() { return this.#root.googletag; } }',
    'function inspect(root = window) { return root.googletag; }',
    'function inspect({ window: root } = globalThis) { return root.pbjs; }',
    'for (const root of [window]) { root.googletag; }',
    'let root; for (root of [globalThis]) { root.pbjs; }',
  ]) {
    assertRejected(source);
  }
});

test('is scope-aware and permits unrelated shadowed values', () => {
  assert.deepEqual(
    lint(`
      function inspect(window, globalThis, self, googletag, pbjs) {
        window.googletag;
        globalThis.pbjs;
        self.googletag;
        googletag.cmd;
        pbjs.requestBids;
      }
      void inspect;
    `),
    []
  );
  assert.deepEqual(
    lint(`
      const values = [window];
      values.googletag;
      [window].pbjs;
    `),
    []
  );
  assert.deepEqual(
    lint(`
      class SelfReference {
        bind() { this.root = this.root; }
        read() { return this.root.googletag; }
      }
      void SelfReference;
    `),
    []
  );
  assert.deepEqual(
    lint(`
      class BrowserState { bind() { this.root = window; } }
      class LocalState {
        constructor() { this.root = { googletag: 'local' }; }
        read() { return this.root.googletag; }
      }
      void BrowserState;
      void LocalState;
    `),
    []
  );
  assert.deepEqual(
    lint(`
      class LocalState {
        constructor() { this.window = { googletag: 'local' }; }
        read() { return this.window.googletag; }
      }
      void LocalState;
    `),
    []
  );
});

test('permits TSJS API and messaging access outside adapters', () => {
  assert.deepEqual(
    lint(`
      window.tsjs?.requestAds();
      globalThis.window?.postMessage({ type: 'TSJS_V1' }, '*');
      self.addEventListener('message', onMessage, { capture: true });
    `),
    []
  );
});

test('permits external-global ownership only in adapter source files', () => {
  assert.deepEqual(
    lint('const root = window; root.googletag; globalThis.pbjs;', 'src/adapters/googletag.js'),
    []
  );
  assert.deepEqual(
    lint('const root = window; root.googletag;', 'src/first_display/adapters/googletag.ts'),
    []
  );
  assertRejected('window.googletag;', 'src/adapters-pretender/googletag.js');
  assertRejected('window.googletag;', 'src/first_display/leaf/gpt_protocol.ts');
});

test('integration entrypoints have no ad-tech-global exemption', () => {
  assertRejected('window.googletag;', 'src/integrations/gpt/index.ts');
  assertRejected('globalThis.pbjs;', 'src/integrations/prebid/index.ts');
});

test('restricted paths enforce dependency direction and exact target-file exemptions', async () => {
  const eslint = new ESLint({ cwd: packageRoot });
  const restrictedRuleId = 'import-x/no-restricted-paths';

  async function restrictedMessages(source, relativeFilename) {
    const [result] = await eslint.lintText(source, {
      filePath: path.join(packageRoot, relativeFilename),
    });
    assert.ok(result);
    assert.equal(result.fatalErrorCount, 0);
    return result.messages.filter((message) => message.ruleId === restrictedRuleId);
  }

  async function projectRuleMessages(source, relativeFilename, projectRuleId) {
    const [result] = await eslint.lintText(source, {
      filePath: path.join(packageRoot, relativeFilename),
    });
    assert.ok(result);
    assert.equal(result.fatalErrorCount, 0);
    return result.messages.filter((message) => message.ruleId === projectRuleId);
  }

  assert.ok(
    (await restrictedMessages("import '../integrations/aps/render';", 'src/core/new-request.ts'))
      .length > 0
  );
  assert.ok(
    (await restrictedMessages("import '../adapters/googletag';", 'src/kernel/probe.tsx')).length > 0
  );
  assert.ok(
    (
      await projectRuleMessages(
        'window.googletag;',
        'src/kernel/probe.tsx',
        'tsjs/no-adtech-globals'
      )
    ).length > 0
  );
  assert.ok(
    (await restrictedMessages("import '../adapters/googletag';", 'src/core/new-index.ts')).length >
      0
  );
  assert.ok((await restrictedMessages("import '../index';", 'src/adapters/probe.ts')).length > 0);
  assert.ok(
    (await restrictedMessages("import '../core/log.js';", 'src/adapters/probe.ts')).length > 0
  );
  assert.ok(
    (await restrictedMessages("import '../index.js';", 'src/adapters/probe.ts')).length > 0
  );
  assert.ok(
    (await restrictedMessages("import '../adapters/googletag.js';", 'src/kernel/probe.ts')).length >
      0
  );
  assert.ok(
    (await restrictedMessages("import '../core/types';", 'src/adapters/new-adapter.ts')).length > 0
  );
  assert.ok(
    (await restrictedMessages("import '../core/types';", 'src/services/new-service.ts')).length > 0
  );
  assert.ok(
    (
      await restrictedMessages(
        "import '../composition/runtime_transport';",
        'src/kernel/new-runtime.ts'
      )
    ).length > 0
  );
  assert.ok(
    (
      await restrictedMessages(
        "import '../../composition/runtime_transport';",
        'src/integrations/gpt/new-module.ts'
      )
    ).length > 0
  );
  assert.ok(
    (await restrictedMessages("import '../prebid/index';", 'src/integrations/gpt/new-module.ts'))
      .length > 0
  );

  assert.ok(
    (await restrictedMessages("import '../integrations/aps/render';", 'src/core/request.ts'))
      .length > 0
  );
  assert.ok(
    (await restrictedMessages("import '../integrations/aps/render';", 'src/kernel/request.ts'))
      .length > 0
  );
  assert.deepEqual(
    await restrictedMessages("import '../adapters/googletag';", 'src/composition/new-browser.ts'),
    []
  );
  assert.deepEqual(
    await restrictedMessages("import './script_guard';", 'src/integrations/gpt/new-module.ts'),
    []
  );
});

test('generated bootstrap source graph excludes APS integration implementation', async () => {
  const graph = await collectRelativeModuleGraph('src/core/bootstrap.ts');

  assert.ok(graph.includes('src/core/release_id.ts'));
  assert.deepEqual(
    graph.filter((filename) => filename.startsWith('src/integrations/aps/')),
    []
  );
});

test('every current integration directory participates in cross-integration isolation', async () => {
  const entries = await readdir(path.join(packageRoot, 'src/integrations'), {
    withFileTypes: true,
  });
  const actual = entries
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name)
    .sort();

  assert.deepEqual(actual, [...ARCHITECTURE_INTEGRATION_DIRECTORIES].sort());
});

test('documents exotic global-analysis blind spots and the defense-in-depth layers', async () => {
  const source = await readFile(
    path.join(packageRoot, 'eslint-rules/no-adtech-globals.js'),
    'utf8'
  );

  assert.match(source, /computed composition/u);
  assert.match(source, /function-returned roots/u);
  assert.match(source, /bundle\/source scans/u);
  assert.match(source, /browser ownership tests/u);
});
