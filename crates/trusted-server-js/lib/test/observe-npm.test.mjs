// @vitest-environment node
import crypto from 'node:crypto';
import { execFileSync, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { afterEach, describe, expect, it } from 'vitest';
import {
  normalizeEsbuildHardlink,
  parseRequest,
  sealObservation,
  snapshotTree,
} from '../../../../scripts/release/observe-npm.mjs';

const observer = pathToFileURL(
  path.resolve(
    path.dirname(fileURLToPath(import.meta.url)),
    '../../../../scripts/release/observe-npm.mjs'
  )
).href;
const npmRoot = path.resolve(path.dirname(process.execPath), '../lib/node_modules/npm');
const temporary = [];
function fixture(name = 'esbuild') {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'ts-observer-')));
  temporary.push(root);
  const install = path.join(root, 'installation');
  const owner = path.join(install, 'node_modules', name);
  fs.mkdirSync(owner, { recursive: true });
  const version = name === 'esbuild' ? '0.27.3' : '3.48.0';
  const command =
    name === 'esbuild' ? 'node install.js' : 'node -e "try{require(\'./postinstall\')}catch(e){}"';
  fs.writeFileSync(
    path.join(install, 'package.json'),
    JSON.stringify({ name: 'example-root', version: '1.0.0' })
  );
  fs.writeFileSync(
    path.join(install, 'package-lock.json'),
    JSON.stringify({
      lockfileVersion: 3,
      packages: { '': {}, [`node_modules/${name}`]: { version } },
    })
  );
  const pkg = { name, version, scripts: { postinstall: command } };
  fs.writeFileSync(path.join(owner, 'package.json'), JSON.stringify(pkg));
  return { root, install, owner, pkg };
}
function executeHook(value, body, { second = false, shadow = false, native } = {}) {
  fs.writeFileSync(
    path.join(value.owner, value.pkg.name === 'esbuild' ? 'install.js' : 'postinstall.js'),
    body
  );
  if (shadow) {
    const bin = path.join(value.owner, 'node_modules', '.bin');
    fs.mkdirSync(bin, { recursive: true });
    fs.writeFileSync(path.join(bin, 'node'), '#!/bin/sh\nexit 91\n', { mode: 0o755 });
  }
  const script = `
    import { createRequire } from 'node:module';
    import { installLifecycleObserver } from ${JSON.stringify(observer)};
    const require = createRequire(import.meta.url);
    const [npmRoot, installRoot, owner, second, native] = process.argv.slice(1);
    const state = installLifecycleObserver({ npmRoot, installRoot, nodePath: process.execPath, nativeEsbuild: JSON.parse(native) });
    const run = require(npmRoot + '/node_modules/@npmcli/run-script/lib/run-script.js');
    const pkg = require(owner + '/package.json');
    let caught = false;
    for (let index = 0; index < (second === 'true' ? 2 : 1); index++) {
      try { await run({ event: 'postinstall', path: owner, pkg, stdio: 'inherit' }); } catch { caught = true; }
    }
    console.log('OBSERVER_TEST_RESULT ' + JSON.stringify({ caught, failed: state.failed(), events: state.events, normalizations: state.normalizations }));
  `;
  const child = spawnSync(
    process.execPath,
    [
      '--input-type=module',
      '-e',
      script,
      npmRoot,
      value.install,
      value.owner,
      String(second),
      JSON.stringify(native ?? null),
    ],
    {
      env: {
        PATH: `${path.dirname(process.execPath)}:/usr/bin:/bin`,
        HOME: value.root,
        TMPDIR: value.root,
      },
      encoding: 'utf8',
      input: 'private parent input',
      timeout: 15000,
      maxBuffer: 1024 * 1024,
    }
  );
  expect(child.status, child.stderr).toBe(0);
  const result = child.stdout.split('\n').find((line) => line.startsWith('OBSERVER_TEST_RESULT '));
  expect(result, child.stderr).toBeTruthy();
  return JSON.parse(result.slice('OBSERVER_TEST_RESULT '.length));
}

function executeFreshCi({ optional, failHook }) {
  const value = fixture();
  const original = path.join(value.root, 'original');
  const packaged = path.join(original, 'package');
  fs.mkdirSync(packaged, { recursive: true });
  fs.copyFileSync(path.join(value.owner, 'package.json'), path.join(packaged, 'package.json'));
  fs.writeFileSync(path.join(packaged, 'install.js'), failHook ? 'process.exit(7);' : '');
  const archive = path.join(value.root, 'original.tgz');
  execFileSync('tar', ['czf', archive, '-C', original, 'package']);
  const integrity = `sha512-${crypto.createHash('sha512').update(fs.readFileSync(archive)).digest('base64')}`;
  const pkg = {
    name: 'example-root',
    version: '1.0.0',
    [optional ? 'optionalDependencies' : 'dependencies']: { esbuild: '0.27.3' },
  };
  fs.writeFileSync(path.join(value.install, 'package.json'), JSON.stringify(pkg));
  fs.writeFileSync(
    path.join(value.install, 'package-lock.json'),
    JSON.stringify({
      name: pkg.name,
      version: pkg.version,
      lockfileVersion: 3,
      requires: true,
      packages: {
        '': pkg,
        'node_modules/esbuild': {
          version: '0.27.3',
          resolved: 'https://registry.example.com/esbuild/-/esbuild-0.27.3.tgz',
          integrity,
          hasInstallScript: true,
          ...(optional ? { optional: true } : {}),
        },
      },
    })
  );
  fs.rmSync(path.join(value.install, 'node_modules'), { recursive: true });
  const cache = path.join(value.root, 'cache');
  fs.mkdirSync(cache);
  const env = {
    PATH: `${path.dirname(process.execPath)}:/usr/bin:/bin`,
    HOME: value.root,
    TMPDIR: value.root,
  };
  const seeded = spawnSync(
    process.execPath,
    [
      path.join(npmRoot, 'bin/npm-cli.js'),
      'cache',
      'add',
      archive,
      '--offline',
      '--ignore-scripts',
      '--no-audit',
      '--no-fund',
      '--userconfig=/dev/null',
      `--globalconfig=${path.join(cache, 'release-disabled-global.npmrc')}`,
      '--cache',
      cache,
    ],
    { env, encoding: 'utf8', timeout: 15000 }
  );
  expect(seeded.status, seeded.stderr).toBe(0);
  const script = `
    import { createRequire } from 'node:module';
    import { installLifecycleObserver } from ${JSON.stringify(observer)};
    const require = createRequire(import.meta.url);
    const [npmRoot, installRoot, cache] = process.argv.slice(1);
    const state = installLifecycleObserver({ npmRoot, installRoot, nodePath: process.execPath });
    process.once('exit', code => console.log('OBSERVER_TEST_RESULT ' + JSON.stringify({ code, failed: state.failed(), events: state.events })));
    process.argv = [process.execPath, npmRoot + '/bin/npm-cli.js', 'ci', '--offline', '--include=dev', '--no-audit', '--no-fund', '--foreground-scripts', '--ignore-scripts=false', '--userconfig=/dev/null', '--globalconfig=' + cache + '/release-disabled-global.npmrc', '--script-shell=/bin/sh', '--cache', cache];
    require(npmRoot + '/bin/npm-cli.js');
  `;
  const child = spawnSync(
    process.execPath,
    ['--input-type=module', '-e', script, npmRoot, value.install, cache],
    { cwd: value.install, env, encoding: 'utf8', timeout: 15000, maxBuffer: 1024 * 1024 }
  );
  expect(child.status, child.stderr).toBe(0);
  const result = child.stdout.split('\n').find((line) => line.startsWith('OBSERVER_TEST_RESULT '));
  expect(result, child.stderr).toBeTruthy();
  return JSON.parse(result.slice('OBSERVER_TEST_RESULT '.length));
}
afterEach(() => {
  for (const root of temporary.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});

describe('release npm lifecycle observer', () => {
  it('rejects requests without a private full-length signing key', () => {
    expect(() => parseRequest(Buffer.from('{}'))).toThrow();
  });
  it('binds closed requests to disjoint canonical roots and exact original identities', () => {
    const first = fixture();
    const cache = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'ts-cache-')));
    temporary.push(cache);
    const request = {
      schema_version: 1,
      key: 'a'.repeat(64),
      nonce: 'b'.repeat(64),
      source_commit: 'c'.repeat(40),
      repo_root: first.root,
      cache,
      guard_path: fs.realpathSync(process.execPath),
      native_esbuild: {
        lock_path: 'node_modules/@esbuild/linux-x64',
        sha256: 'd'.repeat(64),
        size: 4,
        mode: 0o755,
      },
    };
    expect(parseRequest(Buffer.from(JSON.stringify(request)))).toEqual(request);
    for (const change of [
      { key: 'a' },
      { nonce: 'b' },
      { source_commit: 'HEAD' },
      { cache: first.install },
      { command: 'other' },
    ]) {
      expect(() => parseRequest(Buffer.from(JSON.stringify({ ...request, ...change })))).toThrow();
    }
    expect(() =>
      parseRequest(
        Buffer.from(
          JSON.stringify(request).replace(
            '"schema_version":1',
            '"schema_version":1,"schema_version":1'
          )
        )
      )
    ).toThrow();
  });
  it('observes a genuine no-output hook start and successful completion', () => {
    const value = fixture('core-js');
    const result = executeHook(value, '');
    expect(result.failed).toBe(false);
    expect(result.events.map((event) => event.stage)).toEqual(['start', 'success']);
    expect(result.events[1].changes).toEqual([]);
    expect(result.events[0].script_sha256).toBe(
      crypto.createHash('sha256').update(value.pkg.scripts.postinstall).digest('hex')
    );
  });
  it('records actual changed owned files and pins node after package bin shadowing', () => {
    const value = fixture();
    const result = executeHook(
      value,
      "require('node:fs').writeFileSync('observed.txt', process.execPath);",
      { shadow: true }
    );
    expect(result.failed).toBe(false);
    expect(fs.readFileSync(path.join(value.owner, 'observed.txt'), 'utf8')).toBe(process.execPath);
    expect(result.events[1].changes).toEqual([
      {
        path: 'node_modules/esbuild/observed.txt',
        before: null,
        after: {
          kind: 'file',
          executable: false,
          size: Buffer.byteLength(process.execPath),
          sha256: crypto.createHash('sha256').update(process.execPath).digest('hex'),
        },
      },
    ]);
  });
  it('retains a failed event even when its caller swallows the optional failure', () => {
    const result = executeHook(fixture(), 'process.exit(7);');
    expect(result.caught).toBe(true);
    expect(result.failed).toBe(true);
    expect(result.events.map((event) => event.stage)).toEqual(['start', 'failure']);
    expect(result.events.at(-1).exit_code).toBe(7);
  });
  it('runs real fresh offline npm ci including its absent root lifecycle calls', () => {
    const result = executeFreshCi({ optional: false, failHook: false });
    expect(result.code).toBe(0);
    expect(result.failed).toBe(false);
    expect(result.events.map((event) => event.stage)).toEqual(['start', 'success']);
  });
  it('detects an optional hook failure swallowed by actual npm ci and Arborist', () => {
    const result = executeFreshCi({ optional: true, failHook: true });
    expect(result.code).toBe(0);
    expect(result.failed).toBe(true);
    expect(result.events.map((event) => event.stage)).toEqual(['start', 'failure']);
    expect(result.events.at(-1).exit_code).toBe(7);
  });
  it('does not pass the parent input pipe to the lifecycle child', () => {
    const value = fixture();
    const result = executeHook(
      value,
      "require('node:fs').writeFileSync('stdin.txt', require('node:fs').readFileSync(0, 'utf8'));"
    );
    expect(result.failed).toBe(false);
    expect(fs.readFileSync(path.join(value.owner, 'stdin.txt'), 'utf8')).toBe('');
  });
  it('rejects writes outside the lifecycle owner and duplicate executions', () => {
    const outside = executeHook(
      fixture(),
      "require('node:fs').writeFileSync('../../unexpected.txt', 'changed');"
    );
    expect(outside.failed).toBe(true);
    expect(outside.events.at(-1).stage).toBe('failure');
    const repeated = executeHook(fixture(), '', { second: true });
    expect(repeated.failed).toBe(true);
    expect(repeated.events.map((event) => event.stage)).toEqual(['start', 'success']);
  });
  it('rejects an unreviewed script before executing it', () => {
    const value = fixture();
    value.pkg.scripts.postinstall = 'node unreviewed.js';
    fs.writeFileSync(path.join(value.owner, 'package.json'), JSON.stringify(value.pkg));
    const result = executeHook(
      value,
      "require('node:fs').writeFileSync('unwanted.txt', 'changed');"
    );
    expect(result.failed).toBe(true);
    expect(result.events).toEqual([]);
    expect(fs.existsSync(path.join(value.owner, 'unwanted.txt'))).toBe(false);
  });
  it('rejects hard links and escaping aliases during owned file measurement', () => {
    const value = fixture();
    fs.writeFileSync(path.join(value.root, 'outside'), 'example');
    fs.linkSync(path.join(value.root, 'outside'), path.join(value.install, 'linked'));
    expect(() => snapshotTree(value.install)).toThrow();
    fs.unlinkSync(path.join(value.install, 'linked'));
    fs.symlinkSync('../outside', path.join(value.install, 'alias'));
    expect(() => snapshotTree(value.install)).toThrow();
  });
  it('normalizes only the original-matched esbuild native hard-link pair after its real hook', () => {
    const value = fixture();
    const lockPath = 'node_modules/@esbuild/linux-x64';
    const binary = path.join(value.install, lockPath, 'bin/esbuild');
    fs.mkdirSync(path.dirname(binary), { recursive: true });
    const bytes = Buffer.from('example native executable bytes');
    fs.writeFileSync(binary, bytes, { mode: 0o755 });
    const owned = path.join(value.owner, 'bin/esbuild');
    fs.mkdirSync(path.dirname(owned), { recursive: true });
    fs.writeFileSync(owned, 'example wrapper', { mode: 0o755 });
    const lock = JSON.parse(fs.readFileSync(path.join(value.install, 'package-lock.json')));
    lock.packages[lockPath] = { version: '0.27.3' };
    fs.writeFileSync(path.join(value.install, 'package-lock.json'), JSON.stringify(lock));
    const native = {
      lock_path: lockPath,
      sha256: crypto.createHash('sha256').update(bytes).digest('hex'),
      size: bytes.length,
      mode: 0o755,
    };
    const result = executeHook(
      value,
      "const fs = require('node:fs'); fs.unlinkSync('bin/esbuild'); fs.linkSync('../@esbuild/linux-x64/bin/esbuild', 'bin/esbuild');",
      { native }
    );
    expect(result.failed).toBe(false);
    expect(result.normalizations).toEqual([
      {
        kind: 'esbuild-hardlink-copy-v1',
        path: 'node_modules/esbuild/bin/esbuild',
        native_path: `${lockPath}/bin/esbuild`,
        sha256: native.sha256,
        size: bytes.length,
        mode: 0o755,
      },
    ]);
    expect(fs.lstatSync(binary).nlink).toBe(1);
    expect(fs.lstatSync(owned).nlink).toBe(1);
    expect(fs.lstatSync(binary).ino).not.toBe(fs.lstatSync(owned).ino);
    expect(fs.readFileSync(binary)).toEqual(bytes);
    expect(fs.readFileSync(owned)).toEqual(bytes);
    expect(result.events.at(-1).changes.map((change) => change.path)).toEqual([
      'node_modules/esbuild/bin/esbuild',
    ]);
    expect(() =>
      normalizeEsbuildHardlink({
        installRoot: value.install,
        native: { ...native, sha256: '0'.repeat(64) },
      })
    ).toThrow();
    fs.unlinkSync(owned);
    fs.linkSync(binary, owned);
    fs.linkSync(binary, path.join(value.owner, 'extra-link'));
    expect(() => normalizeEsbuildHardlink({ installRoot: value.install, native })).toThrow();
  });
  it('authenticates exact summary bytes with a key absent from the encoded payload', () => {
    const key = crypto.randomBytes(32).toString('hex');
    const value = { schema_version: 1, nonce: 'b'.repeat(64), success: true, events: [] };
    const sealed = sealObservation(value, key);
    const [prefix, encoded, signature] = sealed.trim().split(' ');
    expect(prefix).toBe('TS_RELEASE_NPM_OBSERVATION_V1');
    const bytes = Buffer.from(encoded, 'base64');
    expect(JSON.parse(bytes)).toEqual(value);
    expect(bytes.includes(Buffer.from(key))).toBe(false);
    expect(signature).toBe(
      crypto.createHmac('sha256', Buffer.from(key, 'hex')).update(bytes).digest('hex')
    );
    expect(signature).not.toBe(
      crypto
        .createHmac('sha256', Buffer.from(key, 'hex'))
        .update(Buffer.from(JSON.stringify({ ...value, success: false })))
        .digest('hex')
    );
  });
});

describe('parent-derived source context', () => {
  it('discovers selected module identities without a Git store in the fresh checkout', async () => {
    const { expectedModuleIds } = await import('../../../../scripts/release/collect-js.mjs');
    const value = fixture();
    const contextRoot = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'ts-context-')));
    temporary.push(contextRoot);
    const context = path.join(contextRoot, 'source.json');
    const source = 'c'.repeat(40);
    const record = { schema_version: 1, source_commit: source, module_ids: ['core', 'example'] };
    fs.writeFileSync(context, JSON.stringify(record));
    expect(expectedModuleIds(value.install, source, context)).toEqual(['core', 'example']);
    for (const change of [
      { source_commit: 'd'.repeat(40) },
      { module_ids: ['example'] },
      { module_ids: ['example', 'core'] },
      { module_ids: ['core', 'core'] },
      { module_ids: ['core', '../example'] },
      { extra: true },
    ]) {
      fs.writeFileSync(context, JSON.stringify({ ...record, ...change }));
      expect(() => expectedModuleIds(value.install, source, context)).toThrow();
    }
    const writable = path.join(value.install, 'source.json');
    fs.writeFileSync(writable, JSON.stringify(record));
    expect(() => expectedModuleIds(value.install, source, writable)).toThrow(/outside/);
  });
});
