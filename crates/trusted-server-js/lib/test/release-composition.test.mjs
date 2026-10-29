// @vitest-environment node
import crypto from 'node:crypto';
import { execFileSync, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { build } from 'vite';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { buildAll } from '../build-all.mjs';
import { moduleBuildOptions } from '../build-module-options.mjs';

const libDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const repoRoot = path.resolve(libDir, '../../..');
const lockFile = path.join(libDir, 'package-lock.json');
const archiveSource = JSON.parse(fs.readFileSync(lockFile, 'utf8')).packages['node_modules/dlv']
  .resolved;
const temporary = [];
const sha256 = (bytes) => crypto.createHash('sha256').update(bytes).digest('hex');
function fixture() {
  const root = fs.mkdtempSync(path.join(libDir, '.release-test-'));
  temporary.push(root);
  return root;
}
afterEach(() => {
  vi.unstubAllEnvs();
  for (const root of temporary.splice(0)) {
    if (fs.lstatSync(root, { throwIfNoEntry: false })?.isSymbolicLink()) fs.unlinkSync(root);
    else fs.rmSync(root, { recursive: true, force: true });
  }
});
async function bundle(root, entry, composition, plugins = []) {
  const outDir = path.join(root, composition ? 'recorded' : 'plain');
  const options = moduleBuildOptions({ name: 'fixture', entryPath: entry, outDir, composition });
  options.plugins = [...(options.plugins ?? []), ...plugins];
  await build(options);
  return fs.readFileSync(path.join(outDir, 'tsjs-fixture.js'));
}
function recorder(onContributions, overrides = {}) {
  return { repoRoot, lockFile, onContributions, ...overrides };
}

describe('release Rollup composition', () => {
  it('reuses checked installed ownership for recorded module builds', async () => {
    const { installedOrigins } = await import('../release-composition.mjs');
    const owners = installedOrigins({ repoRoot, lockFile });
    const root = fixture();
    const entry = path.join(root, 'index.ts');
    fs.writeFileSync(entry, 'globalThis.exampleResult = 7;');
    let rows;
    const recorded = await bundle(
      root,
      entry,
      recorder(
        (value) => {
          rows = value;
        },
        {
          owners,
          lockFile: path.join(root, 'unused-lock.json'),
        }
      )
    );
    expect(recorded).toEqual(await bundle(root, entry));
    expect(rows).toHaveLength(1);
  });
  it('omits a rendered zero-length namespace module without changing bundle bytes', async () => {
    const root = fixture();
    const empty = path.join(root, 'empty.ts');
    const entry = path.join(root, 'index.ts');
    fs.writeFileSync(empty, 'export {};');
    fs.writeFileSync(entry, "import * as empty from './empty'; globalThis.exampleResult = empty;");
    let rows;
    let renderedLength;
    const observe = {
      name: 'example-rendered-module-observer',
      generateBundle(_options, output) {
        renderedLength = Object.values(output)[0].modules[empty].renderedLength;
      },
    };
    const recorded = await bundle(
      root,
      entry,
      recorder((value) => {
        rows = value;
      }),
      [observe]
    );
    expect(renderedLength).toBe(0);
    expect(rows.some((row) => row.path?.endsWith('/empty.ts'))).toBe(false);
    expect(recorded).toEqual(await bundle(root, entry));
  });
  it('records retained source and transitive npm helpers without changing final bytes', async () => {
    const root = fixture();
    const entry = path.join(root, 'index.ts');
    fs.writeFileSync(path.join(root, 'unused.ts'), 'export const discarded = 42;');
    fs.writeFileSync(path.join(root, 'retained.ts'), 'export const retained = "example-retained";');
    fs.writeFileSync(
      entry,
      `import type { BidRequest } from 'prebid.js';
      import { discarded } from './unused';
      import { retained } from './retained';
      import dlv from 'dlv/dist/dlv.js';
      const request: BidRequest | undefined = undefined;
      globalThis.exampleResult = [retained, dlv({ x: 7 }, 'x'), request];`
    );
    let contributions;
    const plain = await bundle(root, entry);
    const recorded = await bundle(
      root,
      entry,
      recorder((rows) => {
        contributions = rows;
      })
    );
    expect(recorded).toEqual(plain);
    expect(contributions).toBeDefined();
    expect(contributions.some((row) => row.path?.endsWith('/retained.ts'))).toBe(true);
    expect(contributions.some((row) => row.path?.endsWith('/unused.ts'))).toBe(false);
    expect(contributions.some((row) => row.package_name === 'prebid.js')).toBe(false);
    const npm = contributions.find((row) => row.package_name === 'dlv');
    expect(npm).toMatchObject({
      kind: 'npm',
      lock_path: 'node_modules/dlv',
      path: 'dist/dlv.js',
      stage: 'rollup-rendered',
    });
    expect(npm.sha256).toBe(
      sha256(fs.readFileSync(path.join(libDir, 'node_modules/dlv/dist/dlv.js')))
    );
    expect(contributions.find((row) => row.id === '\0commonjsHelpers.js')).toMatchObject({
      kind: 'generated',
      plugin: 'commonjs',
      owner_lock_path: 'node_modules/vite',
    });
    expect(contributions.every((row) => row.rendered_length > 0)).toBe(true);
  });

  it('preserves the actual core and Prebid bundle bytes', async () => {
    for (const name of ['core', 'prebid']) {
      const root = fixture();
      const entry = path.join(
        libDir,
        'src',
        name === 'core' ? 'core' : 'integrations/prebid',
        'index.ts'
      );
      let rows;
      expect(
        await bundle(
          root,
          entry,
          recorder((value) => {
            rows = value;
          })
        )
      ).toEqual(await bundle(root, entry));
      expect(rows).toBeDefined();
      expect(rows.some((row) => row.package_name === 'prebid.js')).toBe(false);
    }
  });

  it('records original raw HTML and inline CSS hashes with their transformations', async () => {
    const root = fixture();
    fs.writeFileSync(path.join(root, 'example.html'), '<div>example</div>');
    fs.writeFileSync(path.join(root, 'example.css'), '.example { color: red; }');
    const entry = path.join(root, 'index.ts');
    fs.writeFileSync(
      entry,
      `import html from './example.html?raw'; import css from './example.css?inline'; globalThis.exampleResult = [html, css];`
    );
    let rows;
    const bytes = await bundle(
      root,
      entry,
      recorder((value) => {
        rows = value;
      })
    );
    expect(bytes).toEqual(await bundle(root, entry));
    expect(rows.find((row) => row.path?.endsWith('/example.html'))).toMatchObject({
      transform: 'raw',
      sha256: sha256(fs.readFileSync(path.join(root, 'example.html'))),
    });
    expect(rows.find((row) => row.path?.endsWith('/example.css'))).toMatchObject({
      transform: 'inline',
    });
  });

  it('rejects an unrecognized virtual producer rather than assigning it to Vite', async () => {
    const root = fixture();
    const entry = path.join(root, 'index.ts');
    fs.writeFileSync(
      entry,
      `import value from 'virtual:example'; globalThis.exampleResult = value;`
    );
    const plugin = {
      name: 'example-unreviewed',
      resolveId(id) {
        if (id === 'virtual:example') return '\0example-unreviewed';
      },
      load(id) {
        if (id === '\0example-unreviewed') return 'export default 42;';
      },
    };
    await expect(
      bundle(
        root,
        entry,
        recorder(() => {}),
        [plugin]
      )
    ).rejects.toThrow(/unknown virtual/i);
  });

  it.each(['version', 'integrity'])(
    'rejects contributing installed package %s ambiguity',
    async (field) => {
      const root = fixture();
      const lock = JSON.parse(fs.readFileSync(lockFile, 'utf8'));
      if (field === 'version') lock.packages['node_modules/dlv'].version = '0.0.0';
      else delete lock.packages['node_modules/dlv'].integrity;
      const changedLock = path.join(root, 'package-lock.json');
      fs.writeFileSync(changedLock, JSON.stringify(lock));
      const entry = path.join(root, 'index.ts');
      fs.writeFileSync(
        entry,
        `import dlv from 'dlv/dist/dlv.js'; globalThis.exampleResult = dlv({ x: 7 }, 'x');`
      );
      await expect(
        bundle(
          root,
          entry,
          recorder(() => {}, { lockFile: changedLock, installRoot: libDir })
        )
      ).rejects.toThrow(/version|integrity/i);
    }
  );
});

describe('release collector boundaries', () => {
  it('accepts only absent or exact 1 for the optional full collector test control', async () => {
    for (const value of ['', '0', 'true']) {
      vi.stubEnv('TRUSTED_SERVER__RELEASE_JS_TESTS', value);
      vi.resetModules();
      await expect(import('../vitest.config.ts')).rejects.toThrow(/RELEASE_JS_TESTS.*exactly 1/);
    }
    vi.stubEnv('TRUSTED_SERVER__RELEASE_JS_TESTS', undefined);
    vi.resetModules();
    await expect(import('../vitest.config.ts')).resolves.toHaveProperty('default.test');
  });
  it('refuses enabling the full collector against the unsupported observer pin', async () => {
    vi.stubEnv('TRUSTED_SERVER__RELEASE_JS_TESTS', '1');
    vi.resetModules();
    await expect(import('../vitest.config.ts')).rejects.toThrow(
      /Controlled observer pins esbuild 0\.27\.3; current lock is .*compatibility doc/
    );
  });
  it('runs symlinked build and collector CLIs while imported entry points stay inert', async () => {
    const root = fixture();
    const direct = path.join(root, 'direct');
    const linked = path.join(root, 'linked');
    const buildScript = path.join(libDir, 'build-all.mjs');
    const buildLink = path.join(root, 'build-link.mjs');
    fs.symlinkSync(buildScript, buildLink);
    const env = { ...process.env, TSJS_SKIP_BUILD: '1' };
    delete env.TRUSTED_SERVER__RELEASE_BUILD;
    const run = (script, output) =>
      spawnSync(process.execPath, [script, '--out-dir', output], { env, encoding: 'utf8' });
    expect(run(buildScript, direct).status).toBe(0);
    expect(run(buildLink, linked).status).toBe(0);
    const files = fs.readdirSync(direct).sort();
    expect(files).toContain('tsjs-core.js');
    expect(files.length).toBeGreaterThan(1);
    expect(fs.readdirSync(linked).sort()).toEqual(files);
    for (const file of files)
      expect(fs.readFileSync(path.join(linked, file))).toEqual(
        fs.readFileSync(path.join(direct, file))
      );
    const collector = path.join(repoRoot, 'scripts/release/collect-js.mjs');
    const collectorLink = path.join(root, 'collector-link.mjs');
    fs.symlinkSync(collector, collectorLink);
    const invalid = spawnSync(process.execPath, [collectorLink], { env, encoding: 'utf8' });
    expect(invalid.status).not.toBe(0);
    expect(invalid.stderr).toMatch(/Usage: collect-js/);
    const imported = spawnSync(
      process.execPath,
      [
        '--input-type=module',
        '-e',
        `import ${JSON.stringify(pathToFileURL(buildScript).href)}; import ${JSON.stringify(pathToFileURL(collector).href)};`,
      ],
      { env, encoding: 'utf8' }
    );
    expect(imported.status).toBe(0);
    expect(imported.stdout).toBe('');
    const guardedLink = path.join(libDir, `.release-test-entrypoint-${crypto.randomUUID()}.mjs`);
    fs.symlinkSync(buildScript, guardedLink);
    temporary.push(guardedLink);
    const rejectedOutput = path.join(root, 'rejected-entrypoint');
    const rejected = spawnSync(
      process.execPath,
      ['--preserve-symlinks-main', guardedLink, '--out-dir', rejectedOutput],
      { env, encoding: 'utf8' }
    );
    expect(rejected.status).not.toBe(0);
    expect(rejected.stderr).toMatch(/build-all entrypoint identity check failed/);
    expect(fs.existsSync(rejectedOutput)).toBe(false);
  }, 30_000);
  it('reaches bootstrap refusal from absolute, relative and own-directory shell invocation', () => {
    const directory = path.join(repoRoot, 'scripts/release');
    for (const file of ['build.sh', 'build-js.sh']) {
      for (const [cwd, script] of [
        [repoRoot, path.join(directory, file)],
        [repoRoot, `scripts/release/${file}`],
        [directory, file],
      ]) {
        const result = spawnSync('/bin/sh', [script], { cwd, encoding: 'utf8' });
        expect(result.status).toBe(1);
        expect(result.stderr.trim()).toBe(
          'Release bootstrap is pending the reviewed Rust tooling replacement'
        );
      }
    }
  });
  it.each([
    '{"a":1,"a":2}',
    '{"a":{"b":1,"b":2}}',
    '[1e999]',
    '['.repeat(66) + '0' + ']'.repeat(66),
  ])('rejects ambiguous JSON %s', async (text) => {
    const { strictJson } = await import('../release-composition.mjs');
    expect(() => strictJson(Buffer.from(text))).toThrow();
  });
  it('rejects invalid UTF-8 and oversized preparation JSON', async () => {
    const { strictJson } = await import('../release-composition.mjs');
    expect(() => strictJson(Buffer.from([0xff]))).toThrow();
    expect(() => strictJson(Buffer.alloc(1024 * 1024 + 1, 32))).toThrow();
  });
  it('validates the existing release controls, including any skip presence', async () => {
    const { releaseBuild } = await import('../release-composition.mjs');
    for (const control of ['', '0', 'true'])
      expect(() => releaseBuild({ TRUSTED_SERVER__RELEASE_BUILD: control })).toThrow();
    for (const skip of ['', '0', '1']) {
      expect(releaseBuild({ TSJS_SKIP_BUILD: skip })).toBe(false);
      expect(() =>
        releaseBuild({
          TSJS_SKIP_BUILD: skip,
          TRUSTED_SERVER__RELEASE_BUILD: '1',
          TRUSTED_SERVER__GIT_VERSION: 'a'.repeat(40),
        })
      ).toThrow(/skip/i);
    }
    expect(() => releaseBuild({ TRUSTED_SERVER__RELEASE_BUILD: '1' })).toThrow();
    expect(
      releaseBuild({
        TRUSTED_SERVER__RELEASE_BUILD: '1',
        TRUSTED_SERVER__GIT_VERSION: 'a'.repeat(40),
      })
    ).toBe(true);
  });
  it('suppresses all four ambient env files in release build and Vitest options', async () => {
    const root = fixture();
    const entry = path.join(root, 'index.ts');
    fs.writeFileSync(entry, 'globalThis.exampleResult = import.meta.env.VITE_EXAMPLE;');
    vi.stubEnv('TRUSTED_SERVER__RELEASE_BUILD', '1');
    vi.stubEnv('TRUSTED_SERVER__GIT_VERSION', 'a'.repeat(40));
    vi.stubEnv('TSJS_SKIP_BUILD', undefined);
    try {
      const options = moduleBuildOptions({
        name: 'fixture',
        entryPath: entry,
        outDir: path.join(root, 'out'),
      });
      expect(options.envDir).toBe(false);
      expect(options.mode).toBe('production');
      options.root = root;
      await build(options);
      const expected = fs.readFileSync(path.join(root, 'out/tsjs-fixture.js'));
      for (const file of ['.env', '.env.local', '.env.production', '.env.production.local'])
        fs.writeFileSync(path.join(root, file), 'VITE_EXAMPLE=fictional-environment-value');
      await build(options);
      expect(fs.readFileSync(path.join(root, 'out/tsjs-fixture.js'))).toEqual(expected);
      vi.resetModules();
      const config = (await import('../vitest.config.ts')).default;
      expect(config.envDir).toBe(false);
      expect(config.mode).toBe('production');
      expect(config.server.host).toBe('127.0.0.1');
      const nativeImports = config.plugins.find(
        (plugin) => plugin.name === 'release-test-node-builtins'
      );
      expect(nativeImports.resolveId('node:fs')).toEqual({ id: 'node:fs', external: true });
      expect(nativeImports.resolveId('dlv')).toBeUndefined();
      vi.stubEnv('TRUSTED_SERVER__RELEASE_BUILD', undefined);
      vi.resetModules();
      const ordinary = (await import('../vitest.config.ts')).default;
      expect(ordinary.server).toBeUndefined();
      expect(ordinary.plugins).toBeUndefined();
      expect(ordinary.envDir).toBeUndefined();
      expect(ordinary.mode).toBeUndefined();
    } finally {
      vi.unstubAllEnvs();
    }
  });
  it('rejects source symlink escape and unknown transform queries', async () => {
    const { installedOrigins } = await import('../release-composition.mjs');
    const root = fixture();
    const source = path.join(root, 'source');
    const external = path.join(root, 'external');
    fs.mkdirSync(source);
    fs.mkdirSync(external);
    const sourceLock = path.join(source, 'package-lock.json');
    fs.writeFileSync(sourceLock, JSON.stringify({ lockfileVersion: 3, packages: { '': {} } }));
    fs.writeFileSync(path.join(external, 'outside.ts'), 'export const example = 7;');
    const link = path.join(source, 'alias.ts');
    fs.symlinkSync(path.join(external, 'outside.ts'), link);
    expect(path.relative(fs.realpathSync(source), fs.realpathSync(link))).toMatch(/^\.\.[/\\]/);
    const origins = installedOrigins({ repoRoot: source, lockFile: sourceLock });
    expect(() => origins.origin(link)).toThrow(/escape/i);
    expect(() => origins.origin(path.join(source, 'entry.ts?unknown'))).toThrow(/query/i);
  });
  it('requires checked preparation records rather than inventing lifecycle observations', async () => {
    const collector = await import(path.join(repoRoot, 'scripts/release/collect-js.mjs'));
    await expect(
      collector.collectJs({
        repoRoot,
        outDir: path.join(fixture(), 'bundles'),
        inventoryPath: path.join(fixture(), 'inventory.json'),
      })
    ).rejects.toThrow(/preparation/i);
  });
});

describe('prepared evidence and installed trees', () => {
  it('hashes normalized executable modes and excludes separately owned nested packages', async () => {
    const { installedTreeHash } = await import('../release-composition.mjs');
    const root = fixture();
    fs.writeFileSync(path.join(root, 'example.js'), 'example bytes');
    const nested = path.join(root, 'node_modules/example-dependency');
    fs.mkdirSync(nested, { recursive: true });
    fs.writeFileSync(path.join(nested, 'example.js'), 'nested bytes');
    const exclusions = new Set([nested]);
    const first = installedTreeHash(root, exclusions);
    fs.writeFileSync(path.join(nested, 'example.js'), 'changed nested bytes');
    expect(installedTreeHash(root, exclusions)).toBe(first);
    fs.chmodSync(path.join(root, 'example.js'), 0o755);
    expect(installedTreeHash(root, exclusions)).not.toBe(first);
  });
  it('checks preparation distribution file and lifecycle output hashes', async () => {
    const { validatePreparation } = await import('../release-composition.mjs');
    const root = fixture();
    const file = path.join(root, 'tool');
    fs.writeFileSync(file, 'fictional distribution bytes');
    const packageRoot = path.join(root, 'node_modules/example');
    fs.mkdirSync(packageRoot, { recursive: true });
    fs.writeFileSync(
      path.join(packageRoot, 'package.json'),
      JSON.stringify({ scripts: { install: 'example install command' } })
    );
    const output = path.join(packageRoot, 'generated.js');
    fs.writeFileSync(output, 'fictional lifecycle output');
    const npm = {
      kind: 'npm-package',
      id: 'npm:node_modules/example',
      lock_path: 'node_modules/example',
      name: 'example',
      version: '1.0.0',
      integrity: `sha512-${Buffer.alloc(64).toString('base64')}`,
      installed_tree_sha256: 'a'.repeat(64),
    };
    const material = {
      kind: 'tool-distribution',
      id: 'tool:example',
      name: 'example',
      version: '1.0.0',
      sha256: sha256(fs.readFileSync(file)),
    };
    const prepared = {
      schema_version: 1,
      tools: [{ name: 'example', version: '1.0.0', role: 'bundler', material_id: material.id }],
      distributions: [{ material, path: file }],
      packages: [npm],
      lifecycle_events: [
        {
          kind: 'lifecycle-event',
          id: 'event:example',
          package_material_id: npm.id,
          event: 'install',
          script_sha256: sha256('example install command'),
        },
      ],
      lifecycle_outputs: [
        {
          kind: 'lifecycle-output',
          id: 'lifecycle:example',
          package_material_id: npm.id,
          event: 'install',
          script_sha256: sha256('example install command'),
          path: 'node_modules/example/generated.js',
          size: fs.statSync(output).size,
          sha256: sha256(fs.readFileSync(output)),
        },
      ],
    };
    const checked = validatePreparation(prepared, { installRoot: root, packages: [npm] });
    expect(checked.materials).toContainEqual(material);
    expect(checked.materials.some((record) => record.kind === 'lifecycle-output')).toBe(true);
    const broken = structuredClone(prepared);
    broken.distributions[0].material.sha256 = 'b'.repeat(64);
    expect(() => validatePreparation(broken, { installRoot: root, packages: [npm] })).toThrow(
      /digest/i
    );
    const duplicate = structuredClone(prepared);
    duplicate.packages.push(npm);
    expect(() => validatePreparation(duplicate, { installRoot: root, packages: [npm] })).toThrow(
      /duplicate/i
    );
  });
});

describe('complete inventory producer', () => {
  it('binds measured bundler native files separately from npm archive integrity', async () => {
    const { actualToolFiles } = await import('../release-composition.mjs');
    const tools = actualToolFiles({ installRoot: libDir });
    expect(tools.map((tool) => tool.name)).toEqual(
      expect.arrayContaining([
        'node',
        'npm',
        'vite',
        'rollup',
        'esbuild',
        'vitest',
        `esbuild:${process.platform}-${process.arch}`,
        `rollup:${process.platform}-${process.arch}`,
      ])
    );
    for (const tool of tools) {
      expect(tool.sha256).toMatch(/^[a-f0-9]{64}$/);
      expect(tool.path).toBeTruthy();
      expect(tool.version).toBeTruthy();
    }
  });

  it('rejects lifecycle records that are not bound to the package install script', async () => {
    const { validatePreparation } = await import('../release-composition.mjs');
    const root = fixture();
    const packageRoot = path.join(root, 'node_modules/example');
    fs.mkdirSync(packageRoot, { recursive: true });
    fs.writeFileSync(
      path.join(packageRoot, 'package.json'),
      JSON.stringify({
        name: 'example',
        version: '1.0.0',
        scripts: { install: 'example observed command' },
      })
    );
    fs.writeFileSync(path.join(packageRoot, 'generated.js'), 'example output');
    const npm = {
      kind: 'npm-package',
      id: 'npm:node_modules/example',
      lock_path: 'node_modules/example',
      name: 'example',
      version: '1.0.0',
      integrity: `sha512-${Buffer.alloc(64).toString('base64')}`,
      installed_tree_sha256: 'a'.repeat(64),
    };
    const preparation = {
      schema_version: 1,
      tools: [],
      distributions: [],
      packages: [npm],
      lifecycle_events: [
        {
          kind: 'lifecycle-event',
          id: 'event:example',
          package_material_id: npm.id,
          event: 'install',
          script_sha256: sha256('example install command'),
        },
      ],
      lifecycle_outputs: [
        {
          kind: 'lifecycle-output',
          id: 'lifecycle:example',
          package_material_id: npm.id,
          event: 'install',
          script_sha256: sha256('unrelated command'),
          path: 'node_modules/example/generated.js',
          size: 14,
          sha256: sha256('example output'),
        },
      ],
    };
    expect(() => validatePreparation(preparation, { installRoot: root, packages: [npm] })).toThrow(
      /script/i
    );
  });

  it('joins a nested transitive package by its exact installation location', async () => {
    const { installedOrigins } = await import('../release-composition.mjs');
    const root = fixture();
    const installed = path.join(root, 'node_modules/example-parent/node_modules/example-child');
    fs.mkdirSync(installed, { recursive: true });
    fs.writeFileSync(
      path.join(installed, 'package.json'),
      JSON.stringify({ name: 'example-child', version: '1.2.3' })
    );
    fs.writeFileSync(path.join(installed, 'index.js'), 'export default 7;');
    const key = 'node_modules/example-parent/node_modules/example-child';
    const lock = path.join(root, 'package-lock.json');
    fs.writeFileSync(
      lock,
      JSON.stringify({
        lockfileVersion: 3,
        packages: {
          [key]: {
            version: '1.2.3',
            resolved: archiveSource,
            integrity: `sha512-${Buffer.alloc(64).toString('base64')}`,
          },
        },
      })
    );
    const alias = `${root}-alias`;
    fs.symlinkSync(root, alias);
    temporary.push(alias);
    expect(
      installedOrigins({ repoRoot, lockFile: lock, installRoot: alias }).origin(
        path.join(alias, key, 'index.js')
      )
    ).toMatchObject({ lock_path: key, package_name: 'example-child' });
    expect(
      installedOrigins({ repoRoot, lockFile: lock }).origin(path.join(installed, 'index.js'))
    ).toMatchObject({ lock_path: key, package_name: 'example-child', path: 'index.js' });
  });

  it.skip('disabled: controlled observer pins esbuild 0.27.3; current lock is 0.28.2 — see compat doc; writes deterministic closed inventory records after final Vite/esbuild output', async () => {
    const { actualToolFiles, expectedLifecycleEvents, installedOrigins, installedMaterials } =
      await import('../release-composition.mjs');
    const { collectJs } = await import(path.join(repoRoot, 'scripts/release/collect-js.mjs'));
    const root = fixture();
    const packages = installedMaterials(installedOrigins({ repoRoot, lockFile }));
    const measured = actualToolFiles({ installRoot: libDir });
    const distributions = measured.map(({ name, version, sha256: hash, path: file }) => ({
      material: { kind: 'tool-distribution', id: `tool:${name}`, name, version, sha256: hash },
      path: file,
    }));
    const tools = measured.map(({ name, version, role }) => ({
      name,
      version,
      role,
      material_id: `tool:${name}`,
    }));
    const preparationPath = path.join(root, 'preparation.json');
    fs.writeFileSync(
      preparationPath,
      JSON.stringify({
        schema_version: 1,
        packages,
        distributions,
        tools,
        // Synthetic observations exercise joins; no preparation actually runs in this fixture.
        lifecycle_events: expectedLifecycleEvents({ installRoot: libDir, packages }),
        lifecycle_outputs: [],
      })
    );
    // This is the fixture's source identity, not release approval.
    const sourceContextPath = process.env.TRUSTED_SERVER__RELEASE_JS_SOURCE_CONTEXT;
    const sourceCommit = sourceContextPath
      ? JSON.parse(fs.readFileSync(sourceContextPath, 'utf8')).source_commit
      : execFileSync('git', ['rev-parse', 'HEAD'], {
          cwd: repoRoot,
          encoding: 'utf8',
        }).trim();
    vi.stubEnv('TRUSTED_SERVER__RELEASE_BUILD', '1');
    vi.stubEnv('TRUSTED_SERVER__GIT_VERSION', sourceCommit);
    vi.stubEnv('TSJS_SKIP_BUILD', undefined);
    try {
      const inventoryPath = path.join(root, 'inventory.json');
      const outDir = path.join(root, 'bundles');
      const inventory = await collectJs({
        repoRoot,
        outDir,
        inventoryPath,
        preparationPath,
        sourceContextPath,
      });
      expect(Object.keys(inventory).sort()).toEqual([
        'materials',
        'modules',
        'schema_version',
        'source_commit',
        'tools',
      ]);
      expect(inventory.modules.map((row) => row.id)).toEqual(
        [...inventory.modules.map((row) => row.id)].sort()
      );
      const plainDirectory = path.join(root, 'plain-bundles');
      await buildAll({ outDir: plainDirectory });
      for (const module of inventory.modules) {
        const bytes = fs.readFileSync(path.join(outDir, module.filename));
        expect(bytes).toEqual(fs.readFileSync(path.join(plainDirectory, module.filename)));
        expect(module.size).toBe(bytes.length);
        expect(module.sha256).toBe(sha256(bytes));
        expect(Object.keys(module).sort()).toEqual([
          'contributions',
          'filename',
          'id',
          'sha256',
          'size',
        ]);
      }
      expect(inventory.materials.some((row) => row.kind === 'lifecycle-output')).toBe(false);
      expect(inventory.materials.some((row) => row.kind === 'lifecycle-event')).toBe(true);
      for (const source of [
        'crates/trusted-server-js/lib/tsconfig.json',
        'scripts/release/bootstrap.sh',
      ]) {
        expect(inventory.materials.find((row) => row.id === `source:${source}`)).toMatchObject({
          kind: 'source',
          path: source,
          sha256: sha256(fs.readFileSync(path.join(repoRoot, source))),
        });
      }
      expect(fs.readFileSync(inventoryPath, 'utf8')).toBe(`${JSON.stringify(inventory)}\n`);
      const alias = path.join(root, 'bundle-alias');
      fs.symlinkSync(outDir, alias);
      await expect(
        collectJs({
          repoRoot,
          outDir: alias,
          inventoryPath: path.join(outDir, 'inside.json'),
          preparationPath,
          sourceContextPath,
        })
      ).rejects.toThrow(/outside.*bundle/i);
      await expect(
        collectJs({
          repoRoot: fixture(),
          outDir,
          inventoryPath: path.join(root, 'wrong-checkout.json'),
          preparationPath,
          sourceContextPath,
        })
      ).rejects.toThrow(/collector.*checkout/i);
      const originalPreparation = fs.readFileSync(preparationPath);
      const colliding = JSON.parse(originalPreparation);
      const collision = 'source:crates/trusted-server-js/lib/package.json';
      colliding.distributions.find((row) => row.material.name === 'node').material.id = collision;
      colliding.tools.find((row) => row.name === 'node').material_id = collision;
      fs.writeFileSync(preparationPath, JSON.stringify(colliding));
      await expect(
        collectJs({
          repoRoot,
          outDir,
          inventoryPath: path.join(root, 'collision.json'),
          preparationPath,
          sourceContextPath,
        })
      ).rejects.toThrow(/duplicate.*material|material.*identity/i);
      expect(fs.existsSync(path.join(root, 'collision.json'))).toBe(false);
      fs.writeFileSync(preparationPath, originalPreparation);
      const bad = JSON.parse(fs.readFileSync(preparationPath, 'utf8'));
      bad.distributions.find((row) => row.material.name === 'vite').path = path.join(
        root,
        'fictional-tool'
      );
      const actualVite = measured.find((row) => row.name === 'vite');
      fs.copyFileSync(actualVite.path, path.join(root, 'fictional-tool'));
      expect(sha256(fs.readFileSync(path.join(root, 'fictional-tool')))).toBe(actualVite.sha256);
      fs.writeFileSync(preparationPath, JSON.stringify(bad));
      await expect(
        collectJs({
          repoRoot,
          outDir,
          inventoryPath: path.join(root, 'bad.json'),
          preparationPath,
          sourceContextPath,
        })
      ).rejects.toThrow(/actual.*tool.*identity/i);
    } finally {
      vi.unstubAllEnvs();
    }
  }, 120_000);
});

describe('strict parser parity', () => {
  it.each([
    Buffer.from('\ufeff{}'),
    Buffer.from('"\\ud800"'),
    Buffer.from('{"\\ud800":0}'),
    Buffer.from('9007199254740993'),
    Buffer.from('['.repeat(65) + ']'.repeat(65)),
  ])('rejects ambiguous Unicode, integer or container input %#', async (bytes) => {
    const { strictJson } = await import('../release-composition.mjs');
    expect(() => strictJson(bytes)).toThrow();
  });
  it('accepts exactly 64 containers and paired Unicode', async () => {
    const { strictJson } = await import('../release-composition.mjs');
    expect(() => strictJson(Buffer.from('['.repeat(64) + ']'.repeat(64)))).not.toThrow();
    expect(strictJson(Buffer.from('"\\ud83d\\ude00"'))).toBe('😀');
  });
});

describe('observed lifecycle events', () => {
  it('records an executed lifecycle event even when it produced no output file', async () => {
    const { validatePreparation } = await import('../release-composition.mjs');
    const root = fixture();
    const packageRoot = path.join(root, 'node_modules/example');
    fs.mkdirSync(packageRoot, { recursive: true });
    fs.writeFileSync(
      path.join(packageRoot, 'package.json'),
      JSON.stringify({ scripts: { postinstall: 'example observed command' } })
    );
    const npm = {
      kind: 'npm-package',
      id: 'npm:node_modules/example',
      lock_path: 'node_modules/example',
      name: 'example',
      version: '1.0.0',
      integrity: `sha512-${Buffer.alloc(64).toString('base64')}`,
      installed_tree_sha256: 'a'.repeat(64),
    };
    const event = {
      kind: 'lifecycle-event',
      id: 'lifecycle:example:postinstall',
      package_material_id: npm.id,
      event: 'postinstall',
      script_sha256: sha256('example observed command'),
    };
    const preparation = {
      schema_version: 1,
      tools: [],
      distributions: [],
      packages: [npm],
      lifecycle_events: [event],
      lifecycle_outputs: [],
    };
    const checked = validatePreparation(preparation, { installRoot: root, packages: [npm] });
    expect(checked.materials).toContainEqual(event);
    const changed = structuredClone(preparation);
    changed.lifecycle_events[0].script_sha256 = 'b'.repeat(64);
    expect(() => validatePreparation(changed, { installRoot: root, packages: [npm] })).toThrow(
      /script/i
    );
  });
});

describe('observer output boundary', () => {
  it('rejects an ES chunk even when its filename matches the IIFE contract', async () => {
    const root = fixture();
    const entry = path.join(root, 'index.ts');
    fs.writeFileSync(entry, 'globalThis.exampleResult = 7;');
    const options = moduleBuildOptions({
      name: 'fixture',
      entryPath: entry,
      outDir: path.join(root, 'out'),
      composition: recorder(() => {}),
    });
    options.build.rollupOptions.output.format = 'es';
    await expect(build(options)).rejects.toThrow(/IIFE/i);
  });
});

describe('complete preparation lifecycle coverage', () => {
  it('rejects an omitted installed package even when it has no lifecycle hooks', async () => {
    const { validatePreparation } = await import('../release-composition.mjs');
    const root = fixture();
    const packageRoot = path.join(root, 'node_modules/example');
    fs.mkdirSync(packageRoot, { recursive: true });
    fs.writeFileSync(
      path.join(packageRoot, 'package.json'),
      JSON.stringify({ name: 'example', version: '1.0.0' })
    );
    const npm = {
      kind: 'npm-package',
      id: 'npm:node_modules/example',
      lock_path: 'node_modules/example',
      name: 'example',
      version: '1.0.0',
      integrity: `sha512-${Buffer.alloc(64).toString('base64')}`,
      installed_tree_sha256: 'a'.repeat(64),
    };
    const prepared = {
      schema_version: 1,
      tools: [],
      distributions: [],
      packages: [],
      lifecycle_events: [],
      lifecycle_outputs: [],
    };
    expect(() => validatePreparation(prepared, { installRoot: root, packages: [npm] })).toThrow(
      'Incomplete prepared package set'
    );
  });
  it('rejects a missing observation for an installed registry postinstall hook', async () => {
    const { validatePreparation } = await import('../release-composition.mjs');
    const root = fixture();
    const packageRoot = path.join(root, 'node_modules/example');
    fs.mkdirSync(packageRoot, { recursive: true });
    fs.writeFileSync(
      path.join(packageRoot, 'package.json'),
      JSON.stringify({ scripts: { postinstall: 'example observed command' } })
    );
    const npm = {
      kind: 'npm-package',
      id: 'npm:node_modules/example',
      lock_path: 'node_modules/example',
      name: 'example',
      version: '1.0.0',
      integrity: `sha512-${Buffer.alloc(64).toString('base64')}`,
      installed_tree_sha256: 'a'.repeat(64),
    };
    const preparation = {
      schema_version: 1,
      tools: [],
      distributions: [],
      packages: [npm],
      lifecycle_events: [],
      lifecycle_outputs: [],
    };
    expect(() => validatePreparation(preparation, { installRoot: root, packages: [npm] })).toThrow(
      /missing.*lifecycle|incomplete.*lifecycle/i
    );
  });
});

describe('nested package contribution fixture', () => {
  it('rejects another node_modules tree inside the repository but outside the install root', async () => {
    const { installedOrigins } = await import('../release-composition.mjs');
    const root = fixture();
    const outside = path.join(root, 'node_modules/example');
    fs.mkdirSync(outside, { recursive: true });
    fs.writeFileSync(
      path.join(outside, 'package.json'),
      JSON.stringify({ name: 'example', version: '1.0.0' })
    );
    fs.writeFileSync(path.join(outside, 'index.js'), 'export default 7;');
    const installRoot = path.join(root, 'install');
    fs.mkdirSync(installRoot);
    const fixtureLock = path.join(installRoot, 'package-lock.json');
    fs.writeFileSync(fixtureLock, JSON.stringify({ lockfileVersion: 3, packages: {} }));
    const origins = installedOrigins({ repoRoot, lockFile: fixtureLock });
    expect(() => origins.origin(path.join(outside, 'index.js'))).toThrow(
      /unmapped installed ownership/i
    );
  });
  it('assigns a nested type-only format manifest to its archive package', async () => {
    const { installedOrigins } = await import('../release-composition.mjs');
    const root = fixture();
    const key = 'node_modules/@example/parent';
    const directory = path.join(root, key);
    const format = path.join(directory, 'dist/esm');
    fs.mkdirSync(format, { recursive: true });
    fs.writeFileSync(
      path.join(directory, 'package.json'),
      JSON.stringify({ name: '@example/parent', version: '1.0.0' })
    );
    fs.writeFileSync(path.join(format, 'package.json'), JSON.stringify({ type: 'module' }));
    fs.writeFileSync(path.join(format, 'index.js'), 'export default 7;');
    const fixtureLock = path.join(root, 'package-lock.json');
    fs.writeFileSync(
      fixtureLock,
      JSON.stringify({
        lockfileVersion: 3,
        packages: {
          [key]: {
            version: '1.0.0',
            resolved: archiveSource,
            integrity: `sha512-${Buffer.alloc(64).toString('base64')}`,
          },
        },
      })
    );
    const origins = installedOrigins({ repoRoot, lockFile: fixtureLock });
    expect(origins.origin(path.join(format, 'index.js'))).toMatchObject({
      lock_path: key,
      path: 'dist/esm/index.js',
    });
    const hostile = path.join(directory, 'node_modules/example-unmapped');
    fs.mkdirSync(hostile, { recursive: true });
    fs.writeFileSync(
      path.join(hostile, 'package.json'),
      JSON.stringify({ name: 'example-unmapped', version: '1.0.0' })
    );
    fs.writeFileSync(path.join(hostile, 'index.js'), 'export default 42;');
    expect(() => origins.origin(path.join(hostile, 'index.js'))).toThrow(/unmapped/i);
  });
  it('preserves final bytes while recording the indirect nested installation identity', async () => {
    const root = fixture();
    const parentKey = 'node_modules/example-parent';
    const childKey = `${parentKey}/node_modules/example-child`;
    for (const [key, name, code] of [
      [parentKey, 'example-parent', "import value from 'example-child'; export default value;"],
      [childKey, 'example-child', 'export default 7;'],
    ]) {
      const directory = path.join(root, key);
      fs.mkdirSync(directory, { recursive: true });
      fs.writeFileSync(
        path.join(directory, 'package.json'),
        JSON.stringify({ name, version: '1.0.0', type: 'module', main: 'index.js' })
      );
      fs.writeFileSync(path.join(directory, 'index.js'), code);
    }
    const integrity = `sha512-${Buffer.alloc(64).toString('base64')}`;
    const fixtureLock = path.join(root, 'package-lock.json');
    fs.writeFileSync(
      fixtureLock,
      JSON.stringify({
        lockfileVersion: 3,
        packages: {
          [parentKey]: { version: '1.0.0', resolved: archiveSource, integrity },
          [childKey]: { version: '1.0.0', resolved: archiveSource, integrity },
        },
      })
    );
    const entry = path.join(root, 'index.ts');
    fs.writeFileSync(
      entry,
      "import value from 'example-parent'; globalThis.exampleResult = value;"
    );
    let rows;
    const recorded = await bundle(
      root,
      entry,
      recorder(
        (value) => {
          rows = value;
        },
        { lockFile: fixtureLock }
      )
    );
    expect(recorded).toEqual(await bundle(root, entry));
    expect(rows.find((row) => row.package_name === 'example-child')).toMatchObject({
      lock_path: childKey,
      path: 'index.js',
    });
  });
  it('rejects installed package aliases before contribution recording', async () => {
    const { installedOrigins } = await import('../release-composition.mjs');
    const root = fixture();
    const real = path.join(root, 'node_modules/example');
    fs.mkdirSync(real, { recursive: true });
    fs.writeFileSync(
      path.join(real, 'package.json'),
      JSON.stringify({ name: 'example', version: '1.0.0' })
    );
    fs.symlinkSync(real, path.join(root, 'node_modules/example-alias'));
    const fixtureLock = path.join(root, 'package-lock.json');
    fs.writeFileSync(
      fixtureLock,
      JSON.stringify({
        lockfileVersion: 3,
        packages: {
          'node_modules/example': { version: '1.0.0' },
          'node_modules/example-alias': { version: '1.0.0' },
        },
      })
    );
    expect(() => installedOrigins({ repoRoot, lockFile: fixtureLock })).toThrow(/alias|escape/i);
  });
});

describe('approved source module discovery', () => {
  it.runIf(process.env.TRUSTED_SERVER__RELEASE_JS_TESTS === '1')(
    'uses the selected commit despite index removal or an invented staged integration',
    async () => {
      const { expectedModuleIds } = await import(
        path.join(repoRoot, 'scripts/release/collect-js.mjs')
      );
      const root = fixture();
      const git = (args) =>
        execFileSync(
          'git',
          [
            '-c',
            'commit.gpgsign=false',
            '-c',
            'user.name=Example',
            '-c',
            'user.email=example@example.com',
            ...args,
          ],
          {
            cwd: root,
            encoding: 'utf8',
            env: {
              PATH: process.env.PATH,
              TMPDIR: os.tmpdir(),
              GIT_CONFIG_NOSYSTEM: '1',
              GIT_CONFIG_GLOBAL: '/dev/null',
              GIT_NO_REPLACE_OBJECTS: '1',
            },
          }
        );
      git(['init', '-q']);
      for (const file of [
        'crates/trusted-server-js/lib/src/core/index.ts',
        'crates/trusted-server-js/lib/src/integrations/example/index.ts',
      ]) {
        fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
        fs.writeFileSync(path.join(root, file), 'export const example = 7;');
      }
      git(['add', '.']);
      git(['commit', '-qm', 'Record example source']);
      const source = git(['rev-parse', 'HEAD']).trim();
      git([
        'rm',
        '--cached',
        '-q',
        'crates/trusted-server-js/lib/src/integrations/example/index.ts',
      ]);
      const invented = path.join(
        root,
        'crates/trusted-server-js/lib/src/integrations/invented/index.ts'
      );
      fs.mkdirSync(path.dirname(invented), { recursive: true });
      fs.writeFileSync(invented, 'export const example = 42;');
      git(['add', invented]);
      expect(expectedModuleIds(root, source)).toEqual(['core', 'example']);
      expect(() => expectedModuleIds(root, 'a'.repeat(40))).toThrow();
    }
  );
});

describe('locked archive source policy', () => {
  it.each(['link', 'file', 'git', 'missing'])(
    'rejects unsupported contributing lock source %s',
    async (variant) => {
      const { installedOrigins } = await import('../release-composition.mjs');
      const root = fixture();
      const lock = JSON.parse(fs.readFileSync(lockFile, 'utf8'));
      const record = lock.packages['node_modules/dlv'];
      if (variant === 'link') record.link = true;
      if (variant === 'file') record.resolved = 'file:example.tgz';
      if (variant === 'git') record.resolved = 'git+https://example.com/example.git';
      if (variant === 'missing') delete record.resolved;
      const fixtureLock = path.join(root, 'package-lock.json');
      fs.writeFileSync(fixtureLock, JSON.stringify(lock));
      const origins = installedOrigins({ repoRoot, lockFile: fixtureLock, installRoot: libDir });
      expect(() => origins.packageIdentity('node_modules/dlv')).toThrow(
        /lock.*source|archive.*source/i
      );
    }
  );
});

describe('bounded regular-file readers', () => {
  it('rejects a FIFO without waiting for a writer', () => {
    const root = fixture();
    const fifo = path.join(root, 'example.fifo');
    execFileSync('mkfifo', [fifo]);
    const helper = pathToFileURL(path.join(libDir, 'release-composition.mjs')).href;
    const code = `import { readBounded } from ${JSON.stringify(helper)}; try { readBounded(process.argv[1]); process.exit(2); } catch(error) { if (/regular file/.test(error.message)) process.exit(0); throw error; }`;
    const result = spawnSync(process.execPath, ['--input-type=module', '-e', code, fifo], {
      timeout: 2000,
      encoding: 'utf8',
    });
    expect(result.error).toBeUndefined();
    expect(result.status).toBe(0);
  });
});

describe('lifecycle output package ownership', () => {
  it.each(['nested-child', 'symlink-child'])(
    'rejects a parent event claiming a %s package output',
    async (variant) => {
      const { installedMaterials, installedOrigins, validatePreparation } =
        await import('../release-composition.mjs');
      const root = fixture();
      const parentKey = 'node_modules/example-parent';
      const childKey =
        variant === 'nested-child'
          ? `${parentKey}/node_modules/example-child`
          : 'node_modules/example-child';
      for (const [key, name] of [
        [parentKey, 'example-parent'],
        [childKey, 'example-child'],
      ]) {
        const directory = path.join(root, key);
        fs.mkdirSync(directory, { recursive: true });
        fs.writeFileSync(
          path.join(directory, 'package.json'),
          JSON.stringify({
            name,
            version: '1.0.0',
            ...(key === parentKey ? { scripts: { install: 'example install command' } } : {}),
          })
        );
      }
      const childOutput = path.join(root, childKey, 'output.js');
      fs.writeFileSync(childOutput, 'fictional child output');
      const integrity = `sha512-${Buffer.alloc(64).toString('base64')}`;
      const fixtureLock = path.join(root, 'package-lock.json');
      fs.writeFileSync(
        fixtureLock,
        JSON.stringify({
          lockfileVersion: 3,
          packages: {
            [parentKey]: { version: '1.0.0', resolved: archiveSource, integrity },
            [childKey]: { version: '1.0.0', resolved: archiveSource, integrity },
          },
        })
      );
      const packages = installedMaterials(installedOrigins({ repoRoot, lockFile: fixtureLock }));
      const scriptHash = sha256('example install command');
      const event = {
        kind: 'lifecycle-event',
        id: 'event:parent',
        package_material_id: `npm:${parentKey}`,
        event: 'install',
        script_sha256: scriptHash,
      };
      let outputPath = `${childKey}/output.js`;
      if (variant === 'symlink-child') {
        // Exercise output validation independently of the prior tree measurement.
        fs.symlinkSync(path.join(root, childKey), path.join(root, parentKey, 'linked'));
        outputPath = `${parentKey}/linked/output.js`;
      }
      const prepared = {
        schema_version: 1,
        tools: [],
        distributions: [],
        packages,
        lifecycle_events: [event],
        lifecycle_outputs: [
          {
            kind: 'lifecycle-output',
            id: 'output:parent',
            package_material_id: event.package_material_id,
            event: event.event,
            script_sha256: scriptHash,
            path: outputPath,
            size: fs.statSync(childOutput).size,
            sha256: sha256(fs.readFileSync(childOutput)),
          },
        ],
      };
      expect(() => validatePreparation(prepared, { installRoot: root, packages })).toThrow(
        /owner|escape/i
      );
    }
  );
});

describe('installed tree mode measurement boundaries', () => {
  it('hashes the executable mode observed on the opened descriptor', async () => {
    const { installedTreeHash } = await import('../release-composition.mjs');
    const root = fixture();
    const file = path.join(root, 'example.js');
    fs.writeFileSync(file, 'example bytes');
    fs.chmodSync(file, 0o644);
    const oldHash = installedTreeHash(root);
    const open = fs.openSync;
    let changed = false;
    const boundary = vi.spyOn(fs, 'openSync').mockImplementation((...args) => {
      if (args[0] === file && !changed) {
        changed = true;
        fs.chmodSync(file, 0o755);
      }
      return open(...args);
    });
    let observed;
    try {
      observed = installedTreeHash(root);
    } finally {
      boundary.mockRestore();
    }
    expect(changed).toBe(true);
    expect(observed).not.toBe(oldHash);
    expect(observed).toBe(installedTreeHash(root));
  });

  it('rejects a mode change between the final descriptor and pathname checks', async () => {
    const { installedTreeHash } = await import('../release-composition.mjs');
    const root = fixture();
    const file = path.join(root, 'example.js');
    fs.writeFileSync(file, 'example bytes');
    fs.chmodSync(file, 0o644);
    const lstat = fs.lstatSync;
    let observations = 0;
    const boundary = vi.spyOn(fs, 'lstatSync').mockImplementation((...args) => {
      if (args[0] === file && ++observations === 2) fs.chmodSync(file, 0o755);
      return lstat(...args);
    });
    try {
      expect(() => installedTreeHash(root)).toThrow(/changed/i);
      expect(observations).toBe(2);
    } finally {
      boundary.mockRestore();
    }
  });
});
