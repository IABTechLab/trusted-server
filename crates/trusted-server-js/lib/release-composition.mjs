/** Conservative Rollup evidence; rendered modules can lose more code in esbuild. */
import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';

export const sha256 = (bytes) => crypto.createHash('sha256').update(bytes).digest('hex');
const compare = (a, b) => (a < b ? -1 : a > b ? 1 : 0);
const relative = (root, file) => path.relative(root, file).split(path.sep).join('/');
const inside = (root, file) => file === root || file.startsWith(`${root}${path.sep}`);
const MAX_JSON = 1024 * 1024;

/** Parse bounded UTF-8 JSON without duplicate keys or non-finite numbers. */
export function strictJson(bytes) {
  if (bytes.length > MAX_JSON) throw new Error('JSON exceeds 1 MiB');
  const text = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bytes);
  let offset = 0;
  const space = () => {
    while (/[\t\n\r ]/.test(text[offset] ?? '\0')) offset++;
  };
  const string = () => {
    const start = offset++;
    while (offset < text.length) {
      const character = text[offset++];
      if (character === '\\') offset++;
      else if (character === '"') {
        const parsed = JSON.parse(text.slice(start, offset));
        if (/[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/u.test(parsed))
          throw new Error('Unpaired JSON surrogate');
        return parsed;
      }
    }
    throw new Error('Unterminated JSON string');
  };
  const value = (depth) => {
    if (depth > 64) throw new Error('JSON nesting exceeds 64');
    space();
    const character = text[offset];
    if (character === '"') return string();
    if (character === '{' || character === '[') {
      if (depth >= 64) throw new Error('JSON nesting exceeds 64');
      offset++;
      const object = character === '{';
      const result = object ? Object.create(null) : [];
      const end = object ? '}' : ']';
      space();
      if (text[offset] === end) {
        offset++;
        return result;
      }
      for (;;) {
        space();
        let key;
        if (object) {
          if (text[offset] !== '"') throw new Error('Expected JSON key');
          key = string();
          if (Object.hasOwn(result, key)) throw new Error(`Duplicate JSON key: ${key}`);
          space();
          if (text[offset++] !== ':') throw new Error('Expected JSON colon');
        }
        const parsed = value(depth + 1);
        if (object) result[key] = parsed;
        else result.push(parsed);
        space();
        const delimiter = text[offset++];
        if (delimiter === end) return result;
        if (delimiter !== ',') throw new Error('Expected JSON delimiter');
      }
    }
    const token = /^(?:true|false|null|-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?)/.exec(
      text.slice(offset)
    )?.[0];
    if (!token) throw new Error('Invalid JSON value');
    offset += token.length;
    const parsed = JSON.parse(token);
    if (
      typeof parsed === 'number' &&
      (!Number.isFinite(parsed) || (Number.isInteger(parsed) && !Number.isSafeInteger(parsed)))
    )
      throw new Error('Non-finite JSON number');
    return parsed;
  };
  const result = value(0);
  space();
  if (offset !== text.length) throw new Error('Trailing JSON data');
  return result;
}

/** Derive release settings from the existing build control only. */
export function releaseBuild(env = process.env) {
  if (!Object.hasOwn(env, 'TRUSTED_SERVER__RELEASE_BUILD')) return false;
  if (Object.hasOwn(env, 'TSJS_SKIP_BUILD')) throw new Error('TSJS_SKIP_BUILD is forbidden');
  if (env.TRUSTED_SERVER__RELEASE_BUILD !== '1')
    throw new Error('Release build control must be exactly 1');
  if (!/^[a-f0-9]{40}$/.test(env.TRUSTED_SERVER__GIT_VERSION ?? ''))
    throw new Error('Release requires full source commit');
  return true;
}

/** Join real installed locations to exact package-lock package identities. */
export function installedOrigins({ repoRoot, lockFile, installRoot = path.dirname(lockFile) }) {
  repoRoot = fs.realpathSync(repoRoot);
  installRoot = fs.realpathSync(installRoot);
  const lock = strictJson(readBounded(lockFile));
  if (
    lock.lockfileVersion !== 3 ||
    !lock.packages ||
    typeof lock.packages !== 'object' ||
    Array.isArray(lock.packages)
  )
    throw new Error('Expected package-lock v3 packages');
  const packages = new Map();
  const realRoots = new Map();
  for (const [lockPath, record] of Object.entries(lock.packages)) {
    if (lockPath === '') continue;
    if (
      !/^(?:node_modules\/(?:@[a-zA-Z0-9._-]+\/)?[a-zA-Z0-9._-]+)(?:\/node_modules\/(?:@[a-zA-Z0-9._-]+\/)?[a-zA-Z0-9._-]+)*$/.test(
        lockPath
      )
    )
      throw new Error(`Unknown lock source: ${lockPath}`);
    const root = path.join(installRoot, lockPath);
    if (!fs.existsSync(root)) continue;
    const real = fs.realpathSync(root);
    if (real !== root || !inside(repoRoot, real))
      throw new Error(`Installed package symlink escape or alias: ${lockPath}`);
    if (realRoots.has(real)) throw new Error(`Duplicate installed identity: ${lockPath}`);
    realRoots.set(real, lockPath);
    packages.set(lockPath, { root, record });
  }
  function packageIdentity(lockPath) {
    const installed = packages.get(lockPath);
    if (!installed) throw new Error(`Missing installed package: ${lockPath}`);
    const { root, record } = installed;
    if (Object.hasOwn(record, 'link') || typeof record.resolved !== 'string')
      throw new Error(`Unsupported lock archive source: ${lockPath}`);
    let archive;
    try {
      archive = new URL(record.resolved);
    } catch {
      throw new Error(`Unsupported lock archive source: ${lockPath}`);
    }
    if (
      archive.protocol !== 'https:' ||
      archive.hostname !== 'registry.npmjs.org' ||
      archive.port ||
      archive.username ||
      archive.password ||
      archive.search ||
      archive.hash ||
      archive.href !== record.resolved ||
      !archive.pathname.endsWith('.tgz')
    )
      throw new Error(`Unsupported lock archive source: ${lockPath}`);
    const manifest = strictJson(readBounded(path.join(root, 'package.json')));
    const name = lockPath.slice(lockPath.lastIndexOf('node_modules/') + 'node_modules/'.length);
    if (manifest.name !== name || (record.name && record.name !== name))
      throw new Error(`Package name mismatch: ${lockPath}`);
    if (!record.version || manifest.version !== record.version)
      throw new Error(`Package version mismatch: ${lockPath}`);
    if (!/^sha512-[A-Za-z0-9+/]{86}==$/.test(record.integrity ?? ''))
      throw new Error(`Missing or unsupported package integrity: ${lockPath}`);
    return {
      kind: 'npm',
      lock_path: lockPath,
      package_name: name,
      package_version: manifest.version,
      integrity: record.integrity,
    };
  }
  function origin(id) {
    const [file, ...queries] = id.split('?');
    const transform =
      queries.length === 0
        ? 'identity'
        : queries.length === 1 && ['raw', 'inline'].includes(queries[0])
          ? queries[0]
          : null;
    if (!transform) throw new Error(`Unknown module query: ${id}`);
    if (!path.isAbsolute(file)) throw new Error(`Unknown module source: ${id}`);
    const real = fs.realpathSync(file);
    if (!inside(repoRoot, real)) throw new Error(`Source symlink escape: ${id}`);
    const bytes = readBounded(real, 32 * 1024 * 1024);
    if (bytes.length > 32 * 1024 * 1024) throw new Error('Source exceeds 32 MiB');
    let directory = path.dirname(real);
    while (inside(installRoot, directory)) {
      const lockPath = relative(installRoot, directory);
      if (packages.has(lockPath)) {
        if (!inside(directory, real)) throw new Error(`Package source symlink escape: ${id}`);
        return {
          ...packageIdentity(lockPath),
          path: relative(directory, real),
          transform,
          sha256: sha256(bytes),
        };
      }
      if (
        fs.existsSync(path.join(directory, 'package.json')) &&
        /(?:^|\/)node_modules\/(?:@[^/]+\/)?[^/]+$/.test(lockPath)
      )
        throw new Error(`Unmapped installed ownership: ${id}`);
      if (directory === installRoot) break;
      directory = path.dirname(directory);
    }
    if (relative(repoRoot, real).split('/').includes('node_modules'))
      throw new Error(`Unmapped installed ownership: ${id}`);
    return { kind: 'source', path: relative(repoRoot, real), transform, sha256: sha256(bytes) };
  }
  return { repoRoot, installRoot, packages, packageIdentity, origin };
}

/** Observe exactly one IIFE's rendered modules without altering emitted output. */
export function compositionPlugin({ name, owners: checkedOwners, ...settings }) {
  const owners = checkedOwners ?? installedOrigins(settings);
  return {
    name: 'trusted-server-release-composition',
    generateBundle(options, bundle) {
      const outputs = Object.values(bundle);
      if (
        options.format !== 'iife' ||
        outputs.length !== 1 ||
        outputs[0].type !== 'chunk' ||
        !outputs[0].isEntry ||
        outputs[0].fileName !== `tsjs-${name}.js`
      )
        throw new Error('Expected exactly one IIFE chunk with no assets');
      const contributions = [];
      for (const [id, rendered] of Object.entries(outputs[0].modules)) {
        if (rendered.renderedLength === 0) continue;
        let contribution;
        if (!id.startsWith('\0')) contribution = owners.origin(id);
        else {
          let plugin;
          let input;
          let stableId = id;
          if (id === '\0commonjsHelpers.js') plugin = 'commonjs';
          else if (id.startsWith('\0') && id.endsWith('?commonjs-es-import')) {
            plugin = 'commonjs';
            input = owners.origin(id.slice(1, -'?commonjs-es-import'.length));
            stableId = `\0${input.kind === 'npm' ? `${input.lock_path}/` : ''}${input.path}?commonjs-es-import`;
          } else if (id === '\0vite/preload-helper.js') plugin = 'vite:build-import-analysis';
          else throw new Error(`Unknown virtual producer: ${id}`);
          owners.packageIdentity('node_modules/vite');
          const code = this.getModuleInfo(id)?.code;
          if (typeof code !== 'string') throw new Error(`Missing generated code: ${id}`);
          contribution = {
            kind: 'generated',
            id: stableId,
            plugin,
            owner_lock_path: 'node_modules/vite',
            generated_sha256: sha256(code),
            ...(input ? { input } : {}),
          };
        }
        contributions.push({
          ...contribution,
          stage: 'rollup-rendered',
          rendered_length: rendered.renderedLength,
        });
      }
      contributions.sort((a, b) => compare(JSON.stringify(a), JSON.stringify(b)));
      settings.onContributions(contributions);
    },
  };
}

function stableFile(file, consume) {
  const descriptor = fs.openSync(
    file,
    fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW | fs.constants.O_NONBLOCK
  );
  try {
    const before = fs.fstatSync(descriptor);
    if (!before.isFile()) throw new Error(`Expected regular file: ${file}`);
    const result = consume(descriptor, before.size, before.mode);
    const after = fs.fstatSync(descriptor);
    const current = fs.lstatSync(file);
    if (
      before.dev !== current.dev ||
      before.ino !== current.ino ||
      before.size !== after.size ||
      before.size !== current.size ||
      before.mode !== after.mode ||
      before.mode !== current.mode ||
      before.mtimeMs !== after.mtimeMs ||
      before.mtimeMs !== current.mtimeMs ||
      before.ctimeMs !== after.ctimeMs ||
      before.ctimeMs !== current.ctimeMs
    )
      throw new Error(`File changed while measured: ${file}`);
    return result;
  } finally {
    fs.closeSync(descriptor);
  }
}

/** Read at most the specified input budget through a held regular-file descriptor. */
export function readBounded(file, maximum = MAX_JSON) {
  return stableFile(file, (descriptor, size) => {
    if (size > maximum) throw new Error(`File exceeds size budget: ${file}`);
    const bytes = Buffer.alloc(size + 1);
    let count = 0;
    while (count < bytes.length) {
      const read = fs.readSync(descriptor, bytes, count, bytes.length - count, null);
      if (!read) break;
      count += read;
    }
    if (count !== size) throw new Error(`File changed or exceeds size budget: ${file}`);
    return bytes.subarray(0, count);
  });
}

/** Hash file bytes with fixed-size buffers, preserving the held-file identity. */
export function fileDigest(file) {
  return stableFile(file, (descriptor, size) => {
    const digest = crypto.createHash('sha256');
    const buffer = Buffer.alloc(64 * 1024);
    let count = 0;
    for (;;) {
      const read = fs.readSync(descriptor, buffer, 0, buffer.length, null);
      if (!read) break;
      count += read;
      if (count > size) throw new Error(`File grew while measured: ${file}`);
      digest.update(buffer.subarray(0, read));
    }
    if (count !== size) throw new Error(`File changed while measured: ${file}`);
    return digest.digest('hex');
  });
}

/** Hash sorted relative paths, types, lengths, bytes and normalized executable modes. */
export function installedTreeHash(root, excludedRoots = new Set()) {
  const digest = crypto.createHash('sha256');
  digest.update('trusted-server-installed-tree-v1\n');
  function visit(directory) {
    for (const name of fs.readdirSync(directory).sort(compare)) {
      const file = path.join(directory, name);
      if (excludedRoots.has(file)) continue;
      const stat = fs.lstatSync(file);
      const filename = relative(root, file);
      if (stat.isDirectory()) {
        digest.update(`${JSON.stringify([filename, 'directory'])}\n`);
        visit(file);
      } else if (stat.isFile()) {
        stableFile(file, (descriptor, size, mode) => {
          digest.update(`${JSON.stringify([filename, 'file', size, Boolean(mode & 0o111)])}\n`);
          const buffer = Buffer.alloc(64 * 1024);
          let count = 0;
          for (;;) {
            const read = fs.readSync(descriptor, buffer, 0, buffer.length, null);
            if (!read) break;
            count += read;
            if (count > size) throw new Error(`Tree file grew: ${file}`);
            digest.update(buffer.subarray(0, read));
          }
          if (count !== size || size !== stat.size) throw new Error(`Tree file changed: ${file}`);
        });
        digest.update('\n');
      } else if (stat.isSymbolicLink()) {
        const target = fs.readlinkSync(file);
        if (path.isAbsolute(target) || !inside(root, fs.realpathSync(file)))
          throw new Error(`Installed tree symlink escape: ${file}`);
        digest.update(`${JSON.stringify([filename, 'symlink', target])}\n`);
      } else throw new Error(`Unsupported installed tree entry: ${file}`);
    }
  }
  visit(root);
  return digest.digest('hex');
}

/** Measure installed packages independently of their lockfile archive integrity. */
export function installedMaterials(owners) {
  const excluded = new Set([...owners.packages.values()].map(({ root }) => root));
  return [...owners.packages.keys()].sort(compare).map((lockPath) => {
    const identity = owners.packageIdentity(lockPath);
    return {
      kind: 'npm-package',
      id: `npm:${lockPath}`,
      lock_path: lockPath,
      name: identity.package_name,
      version: identity.package_version,
      integrity: identity.integrity,
      installed_tree_sha256: installedTreeHash(owners.packages.get(lockPath).root, excluded),
    };
  });
}

const hashPattern = /^[a-f0-9]{64}$/;
function fields(record, keys) {
  if (
    !record ||
    typeof record !== 'object' ||
    Array.isArray(record) ||
    Object.keys(record).sort().join(',') !== [...keys].sort().join(',')
  )
    throw new Error('Unexpected preparation fields');
}
function digest(value) {
  if (typeof value !== 'string' || !hashPattern.test(value))
    throw new Error('Invalid preparation digest');
}
function safePath(value) {
  if (
    typeof value !== 'string' ||
    value.length === 0 ||
    value.includes('\\') ||
    value.includes('\0') ||
    value.startsWith('/') ||
    value.split('/').some((part) => part === '.' || part === '..' || part === '')
  )
    throw new Error('Unsafe preparation path');
}

/** Declare expected registry-install hooks; these declarations are not execution observations. */
export function expectedLifecycleEvents({ installRoot, packages }) {
  const events = [];
  for (const material of packages) {
    const root = path.join(installRoot, material.lock_path);
    const manifest = strictJson(readBounded(path.join(root, 'package.json')));
    if (
      fs.existsSync(path.join(root, 'binding.gyp')) &&
      !manifest.scripts?.install &&
      !manifest.scripts?.preinstall
    )
      throw new Error(`Unsupported implicit lifecycle install: ${material.lock_path}`);
    for (const event of ['preinstall', 'install', 'postinstall']) {
      const script = manifest.scripts?.[event];
      if (script === undefined) continue;
      if (typeof script !== 'string' || !script)
        throw new Error('Invalid declared lifecycle script');
      events.push({
        kind: 'lifecycle-event',
        id: `lifecycle:${material.lock_path}:${event}`,
        package_material_id: material.id,
        event,
        script_sha256: sha256(script),
      });
    }
  }
  return events;
}

/** Check preparation observations; this does not authenticate acquisition or lifecycle execution. */
export function validatePreparation(prepared, { installRoot, packages }) {
  fields(prepared, [
    'schema_version',
    'tools',
    'distributions',
    'packages',
    'lifecycle_events',
    'lifecycle_outputs',
  ]);
  if (
    prepared.schema_version !== 1 ||
    !['tools', 'distributions', 'packages', 'lifecycle_events', 'lifecycle_outputs'].every((key) =>
      Array.isArray(prepared[key])
    )
  )
    throw new Error('Expected preparation version 1 arrays');
  const materials = [];
  const identities = new Set();
  function add(material) {
    if (typeof material.id !== 'string' || !material.id || identities.has(material.id))
      throw new Error('Duplicate or invalid preparation identity');
    identities.add(material.id);
    materials.push(material);
  }
  const measured = new Map(packages.map((record) => [record.id, record]));
  for (const material of prepared.packages) {
    fields(material, [
      'kind',
      'id',
      'lock_path',
      'name',
      'version',
      'integrity',
      'installed_tree_sha256',
    ]);
    if (material.kind !== 'npm-package' || material.id !== `npm:${material.lock_path}`)
      throw new Error('Invalid package material identity');
    const actual = measured.get(material.id);
    if (!actual || Object.keys(material).some((key) => material[key] !== actual[key]))
      throw new Error(`Installed package material mismatch: ${material.id}`);
    add(material);
  }
  if (prepared.packages.length !== packages.length)
    throw new Error('Incomplete prepared package set');
  for (const distribution of prepared.distributions) {
    fields(distribution, ['material', 'path']);
    const { material, path: file } = distribution;
    fields(material, ['kind', 'id', 'name', 'version', 'sha256']);
    if (
      material.kind !== 'tool-distribution' ||
      typeof material.name !== 'string' ||
      !material.name ||
      typeof material.version !== 'string' ||
      !material.version
    )
      throw new Error('Invalid tool distribution');
    digest(material.sha256);
    if (fileDigest(file) !== material.sha256)
      throw new Error(`Tool distribution digest mismatch: ${material.id}`);
    add(material);
  }
  const expectedEvents = new Set(
    expectedLifecycleEvents({ installRoot, packages }).map((event) =>
      JSON.stringify([event.package_material_id, event.event, event.script_sha256])
    )
  );
  const events = new Set();
  for (const event of prepared.lifecycle_events) {
    fields(event, ['kind', 'id', 'package_material_id', 'event', 'script_sha256']);
    const owner = measured.get(event.package_material_id);
    if (
      event.kind !== 'lifecycle-event' ||
      !owner ||
      !['preinstall', 'install', 'postinstall', 'prepare'].includes(event.event)
    )
      throw new Error('Invalid lifecycle event join');
    digest(event.script_sha256);
    const manifest = strictJson(
      readBounded(path.join(installRoot, owner.lock_path, 'package.json'))
    );
    const script = manifest.scripts?.[event.event];
    if (typeof script !== 'string' || sha256(script) !== event.script_sha256)
      throw new Error('Lifecycle script digest mismatch');
    const identity = JSON.stringify([event.package_material_id, event.event, event.script_sha256]);
    if (events.has(identity)) throw new Error('Duplicate lifecycle event');
    if (!expectedEvents.has(identity)) throw new Error('Unexpected lifecycle event');
    events.add(identity);
    add(event);
  }
  if (events.size !== expectedEvents.size)
    throw new Error('Missing expected lifecycle observations');
  const packageRoots = packages
    .map((material) => ({
      id: material.id,
      root: fs.realpathSync(path.join(installRoot, material.lock_path)),
    }))
    .sort((first, second) => second.root.length - first.root.length);
  for (const output of prepared.lifecycle_outputs) {
    fields(output, [
      'kind',
      'id',
      'package_material_id',
      'event',
      'script_sha256',
      'path',
      'size',
      'sha256',
    ]);
    if (
      output.kind !== 'lifecycle-output' ||
      !measured.has(output.package_material_id) ||
      !['preinstall', 'install', 'postinstall', 'prepare'].includes(output.event)
    )
      throw new Error('Invalid lifecycle output join');
    digest(output.script_sha256);
    digest(output.sha256);
    safePath(output.path);
    if (!Number.isSafeInteger(output.size) || output.size < 0 || output.size > 32 * 1024 * 1024)
      throw new Error('Invalid lifecycle size');
    const owner = measured.get(output.package_material_id);
    const ownerRoot = path.join(installRoot, owner.lock_path);
    const manifest = strictJson(readBounded(path.join(ownerRoot, 'package.json')));
    const script = manifest.scripts?.[output.event];
    if (typeof script !== 'string' || sha256(script) !== output.script_sha256)
      throw new Error('Lifecycle script digest mismatch');
    const file = path.join(installRoot, output.path);
    if (!inside(ownerRoot, file)) throw new Error('Lifecycle output owner mismatch');
    const realFile = fs.realpathSync(file);
    if (!inside(fs.realpathSync(installRoot), realFile)) throw new Error('Lifecycle output escape');
    if (
      !inside(fs.realpathSync(ownerRoot), realFile) ||
      packageRoots.find(({ root }) => inside(root, realFile))?.id !== output.package_material_id
    )
      throw new Error('Lifecycle output owner mismatch');
    if (
      !events.has(JSON.stringify([output.package_material_id, output.event, output.script_sha256]))
    )
      throw new Error('Lifecycle output has no observed event');
    const bytes = readBounded(file, 32 * 1024 * 1024);
    if (bytes.length !== output.size || sha256(bytes) !== output.sha256)
      throw new Error('Lifecycle output digest mismatch');
    add(output);
  }
  const tools = [];
  const names = new Set();
  for (const tool of prepared.tools) {
    fields(tool, ['name', 'version', 'role', 'material_id']);
    const material = materials.find((record) => record.id === tool.material_id);
    if (
      !material ||
      material.kind !== 'tool-distribution' ||
      material.name !== tool.name ||
      material.version !== tool.version ||
      !['runtime', 'package-manager', 'bundler', 'minifier', 'test-runner'].includes(tool.role) ||
      names.has(tool.name)
    )
      throw new Error('Invalid or duplicate tool material join');
    names.add(tool.name);
    tools.push(tool);
  }
  return {
    materials: materials.sort((a, b) => compare(a.id, b.id)),
    tools: tools.sort((a, b) => compare(a.name, b.name)),
  };
}

/** Measure the actual tool entry files and native executables; no host authentication is claimed. */
export function actualToolFiles({ installRoot }) {
  const npmRoot = path.resolve(path.dirname(process.execPath), '../lib/node_modules/npm');
  const tools = [
    { name: 'node', role: 'runtime', version: process.versions.node, path: process.execPath },
    {
      name: 'npm',
      role: 'package-manager',
      version: strictJson(readBounded(path.join(npmRoot, 'package.json'))).version,
      path: path.join(npmRoot, 'bin/npm-cli.js'),
    },
  ];
  const modules = [
    ['vite', 'bundler', 'dist/node/index.js'],
    ['rollup', 'bundler', 'dist/es/rollup.js'],
    ['esbuild', 'minifier', 'lib/main.js'],
    ['vitest', 'test-runner', 'vitest.mjs'],
  ];
  for (const [name, role, entry] of modules) {
    const root = path.join(installRoot, 'node_modules', name);
    tools.push({
      name,
      role,
      version: strictJson(readBounded(path.join(root, 'package.json'))).version,
      path: path.join(root, entry),
    });
  }
  if (!['darwin', 'linux'].includes(process.platform) || !['arm64', 'x64'].includes(process.arch))
    throw new Error('Unsupported native tool platform');
  const platform = `${process.platform}-${process.arch}`;
  const esbuildRoot = path.join(installRoot, 'node_modules/@esbuild', platform);
  const rollupSuffix =
    process.platform === 'linux'
      ? process.report.getReport().header.glibcVersionRuntime
        ? '-gnu'
        : '-musl'
      : '';
  const rollupPlatform = `${platform}${rollupSuffix}`;
  const rollupRoot = path.join(installRoot, 'node_modules/@rollup', `rollup-${rollupPlatform}`);
  tools.push({
    name: `esbuild:${platform}`,
    role: 'minifier',
    version: strictJson(readBounded(path.join(esbuildRoot, 'package.json'))).version,
    path: path.join(esbuildRoot, 'bin/esbuild'),
  });
  tools.push({
    name: `rollup:${platform}`,
    role: 'bundler',
    version: strictJson(readBounded(path.join(rollupRoot, 'package.json'))).version,
    path: path.join(rollupRoot, `rollup.${rollupPlatform}.node`),
  });
  return tools.map((tool) => ({ ...tool, sha256: fileDigest(tool.path) }));
}
