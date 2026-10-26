/** Observe pinned npm execution; the Rust supervisor authenticates the sealed result. */
import crypto from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import { createRequire } from 'node:module'
import { fileURLToPath } from 'node:url'
import {
  actualToolFiles,
  expectedLifecycleEvents,
  fileDigest,
  installedMaterials,
  installedOrigins,
  readBounded,
  sha256,
  strictJson,
  validatePreparation,
} from '../../crates/trusted-server-js/lib/release-composition.mjs'

const require = createRequire(import.meta.url)
const MAX_JSON = 1024 * 1024
const MAX_FILE = 32 * 1024 * 1024
const MAX_TREE = 2 * 1024 * 1024 * 1024
const HOOKS = new Map([
  [
    'core-js@3.48.0:postinstall',
    'node -e "try{require(\'./postinstall\')}catch(e){}"',
  ],
  ['esbuild@0.27.3:postinstall', 'node install.js'],
])
const ROOT_HOOKS = [
  'preinstall',
  'install',
  'postinstall',
  'prepublish',
  'preprepare',
  'prepare',
  'postprepare',
  'predependencies',
  'dependencies',
  'postdependencies',
]
const inside = (root, file) =>
  file === root || file.startsWith(`${root}${path.sep}`)
const ordered = (left, right) => (left < right ? -1 : left > right ? 1 : 0)
const fail = () => {
  throw new Error('Release npm observation rejected')
}

/** Parse a bounded parent request without accepting commands or hook-policy overrides. */
export function parseRequest(bytes) {
  const request = strictJson(bytes)
  const keys = [
    'schema_version',
    'key',
    'nonce',
    'source_commit',
    'repo_root',
    'cache',
    'guard_path',
    'native_esbuild',
  ]
  if (
    !request ||
    typeof request !== 'object' ||
    Array.isArray(request) ||
    Object.keys(request).sort().join(',') !== keys.sort().join(',') ||
    request.schema_version !== 1 ||
    !/^[a-f0-9]{64}$/.test(request.key ?? '') ||
    !/^[a-f0-9]{64}$/.test(request.nonce ?? '') ||
    !/^[a-f0-9]{40}$/.test(request.source_commit ?? '')
  )
    fail()
  validateNativeExpectation(request.native_esbuild)
  for (const key of ['repo_root', 'cache', 'guard_path']) {
    const value = request[key]
    if (
      typeof value !== 'string' ||
      !path.isAbsolute(value) ||
      /[\\\x00-\x1f\x7f]/.test(value) ||
      value.split('/').some((part) => part === '.' || part === '..') ||
      fs.realpathSync(value) !== value
    )
      fail()
  }
  if (
    inside(request.repo_root, request.cache) ||
    inside(request.cache, request.repo_root) ||
    inside(request.repo_root, request.guard_path) ||
    inside(request.cache, request.guard_path) ||
    !fs.lstatSync(request.repo_root).isDirectory() ||
    !fs.lstatSync(request.cache).isDirectory() ||
    !fs.lstatSync(request.guard_path).isFile() ||
    fs.lstatSync(request.guard_path).nlink !== 1
  )
    fail()
  return request
}

function validateNativeExpectation(native) {
  if (
    !native ||
    typeof native !== 'object' ||
    Array.isArray(native) ||
    Object.keys(native).sort().join(',') !== 'lock_path,mode,sha256,size' ||
    !/^node_modules\/@esbuild\/linux-(?:x64|arm64)$/.test(
      native.lock_path ?? ''
    ) ||
    !/^[a-f0-9]{64}$/.test(native.sha256 ?? '') ||
    !Number.isSafeInteger(native.size) ||
    native.size < 1 ||
    native.size > MAX_FILE ||
    !Number.isSafeInteger(native.mode) ||
    native.mode < 0 ||
    native.mode > 0o777 ||
    !(native.mode & 0o111)
  )
    fail()
}

/** Detach only esbuild's reviewed native hard-link optimization without changing executable bytes. */
export function normalizeEsbuildHardlink({ installRoot, native }) {
  validateNativeExpectation(native)
  const nativePath = `${native.lock_path}/bin/esbuild`
  const ownedPath = 'node_modules/esbuild/bin/esbuild'
  const original = path.join(installRoot, nativePath)
  const owned = path.join(installRoot, ownedPath)
  if (
    fs.realpathSync(original) !== original ||
    fs.realpathSync(owned) !== owned
  )
    fail()
  const first = fs.lstatSync(original)
  const second = fs.lstatSync(owned)
  if (
    !first.isFile() ||
    !second.isFile() ||
    first.size !== native.size ||
    (first.mode & 0o777) !== native.mode ||
    fileDigest(original) !== native.sha256
  )
    fail()
  if (first.nlink === 1 && second.nlink === 1) return null
  if (
    first.nlink !== 2 ||
    second.nlink !== 2 ||
    first.dev !== second.dev ||
    first.ino !== second.ino ||
    second.size !== native.size ||
    (second.mode & 0o777) !== native.mode ||
    fileDigest(owned) !== native.sha256
  )
    fail()
  const temporary = path.join(
    path.dirname(owned),
    `.release-esbuild-${crypto.randomBytes(16).toString('hex')}`
  )
  let descriptor
  try {
    descriptor = fs.openSync(
      temporary,
      fs.constants.O_WRONLY |
        fs.constants.O_CREAT |
        fs.constants.O_EXCL |
        fs.constants.O_NOFOLLOW,
      0o600
    )
    const bytes = readBounded(original, MAX_FILE)
    const held = fs.lstatSync(original)
    if (
      bytes.length !== native.size ||
      sha256(bytes) !== native.sha256 ||
      held.dev !== first.dev ||
      held.ino !== first.ino ||
      held.nlink !== 2 ||
      held.size !== first.size ||
      held.mode !== first.mode ||
      held.mtimeMs !== first.mtimeMs ||
      held.ctimeMs !== first.ctimeMs
    )
      fail()
    let offset = 0
    while (offset < bytes.length) {
      const written = fs.writeSync(
        descriptor,
        bytes,
        offset,
        bytes.length - offset
      )
      if (!written) fail()
      offset += written
    }
    fs.fchmodSync(descriptor, native.mode)
    fs.closeSync(descriptor)
    descriptor = undefined
    if (
      fs.lstatSync(owned).ino !== second.ino ||
      fs.lstatSync(owned).dev !== second.dev
    )
      fail()
    fs.renameSync(temporary, owned)
    const afterOriginal = fs.lstatSync(original)
    const afterOwned = fs.lstatSync(owned)
    if (
      afterOriginal.nlink !== 1 ||
      afterOwned.nlink !== 1 ||
      afterOwned.ino === afterOriginal.ino ||
      afterOwned.size !== native.size ||
      (afterOwned.mode & 0o777) !== native.mode ||
      fileDigest(original) !== native.sha256 ||
      fileDigest(owned) !== native.sha256
    )
      fail()
    return {
      kind: 'esbuild-hardlink-copy-v1',
      path: ownedPath,
      native_path: nativePath,
      sha256: native.sha256,
      size: native.size,
      mode: native.mode,
    }
  } finally {
    if (descriptor !== undefined) fs.closeSync(descriptor)
    fs.rmSync(temporary, { force: true })
  }
}

/** Measure bounded file facts, rejecting special files, hard links and concurrent mutation. */
export function snapshotTree(root) {
  const facts = new Map()
  let total = 0
  function visit(directory, depth) {
    if (depth > 64) fail()
    for (const name of fs.readdirSync(directory).sort(ordered)) {
      if (!name || /[\\\x00-\x1f\x7f]/.test(name)) fail()
      const file = path.join(directory, name)
      const relative = path.relative(root, file).split(path.sep).join('/')
      const stat = fs.lstatSync(file)
      if (facts.size >= 100000) fail()
      if (stat.isDirectory()) {
        facts.set(relative, { kind: 'directory' })
        visit(file, depth + 1)
      } else if (stat.isFile()) {
        if (
          stat.nlink !== 1 ||
          stat.size > MAX_FILE ||
          (total += stat.size) > MAX_TREE
        )
          fail()
        const digest = fileDigest(file)
        const after = fs.lstatSync(file)
        if (
          after.dev !== stat.dev ||
          after.ino !== stat.ino ||
          after.nlink !== 1 ||
          after.size !== stat.size ||
          after.mtimeMs !== stat.mtimeMs ||
          after.ctimeMs !== stat.ctimeMs ||
          after.mode !== stat.mode
        )
          fail()
        facts.set(relative, {
          kind: 'file',
          size: stat.size,
          executable: Boolean(stat.mode & 0o111),
          sha256: digest,
        })
      } else if (stat.isSymbolicLink()) {
        const target = fs.readlinkSync(file)
        if (
          path.isAbsolute(target) ||
          /[\\\x00-\x1f\x7f]/.test(target) ||
          !inside(root, fs.realpathSync(file))
        )
          fail()
        facts.set(relative, { kind: 'symlink', target })
      } else fail()
    }
  }
  visit(root, 0)
  return facts
}

function changesAfter(before, after, owner) {
  const paths = [...new Set([...before.keys(), ...after.keys()])].sort(ordered)
  const changes = []
  for (const filename of paths) {
    const first = before.get(filename) ?? null
    const last = after.get(filename) ?? null
    if (JSON.stringify(first) === JSON.stringify(last)) continue
    if (!filename.startsWith(`${owner}/`)) fail()
    changes.push({ path: filename, before: first, after: last })
  }
  return changes
}

/** Adapt exact pinned npm lifecycle calls before npm loads its dependency graph. */
export function installLifecycleObserver({
  npmRoot,
  installRoot,
  nodePath,
  nativeEsbuild,
}) {
  if (
    strictJson(readBounded(path.join(npmRoot, 'package.json'))).version !==
      '11.19.0' ||
    process.versions.node !== '24.20.0' ||
    fs.realpathSync(nodePath) !== fs.realpathSync(process.execPath)
  )
    fail()
  const lock = strictJson(
    readBounded(path.join(installRoot, 'package-lock.json'))
  )
  const manifest = strictJson(
    readBounded(path.join(installRoot, 'package.json'))
  )
  if (
    lock.lockfileVersion !== 3 ||
    !lock.packages ||
    ROOT_HOOKS.some((event) => Object.hasOwn(manifest.scripts ?? {}, event)) ||
    fs.existsSync(path.join(installRoot, 'binding.gyp'))
  )
    fail()
  const root = path.join(npmRoot, 'node_modules/@npmcli/run-script')
  const entry = path.join(root, 'lib/run-script.js')
  const spawnEntry = path.join(root, 'lib/make-spawn-args.js')
  // Already captured imports would retain an unobserved lifecycle function.
  if (
    require.cache[entry] ||
    require.cache[path.join(root, 'lib/run-script-pkg.js')]
  )
    fail()
  const makeSpawnArgs = require(spawnEntry)
  const events = []
  const normalizations = []
  let active = null
  let rejected = false
  require.cache[spawnEntry].exports = (options) => {
    if (
      !active ||
      options.event !== active.event ||
      options.path !== active.path ||
      options.cmd !== active.command ||
      options.args?.length
    ) {
      rejected = true
      fail()
    }
    const result = makeSpawnArgs(options)
    const spawn = result[2]
    // npm prepends package bins; bind node after that transformation too.
    for (const key of Object.keys(spawn.env)) {
      if (/^path$/i.test(key))
        spawn.env[key] =
          `${path.dirname(nodePath)}${path.delimiter}${spawn.env[key]}`
      if (
        /^(?:NODE_OPTIONS|NODE_PATH|TS_RELEASE_OBSERVER|TS_RELEASE_KEY)/.test(
          key
        )
      )
        delete spawn.env[key]
    }
    // Both reviewed commands start with bare node; resolve that word independently of PATH.
    result[0] = `'${nodePath.replaceAll("'", "'\\''")}'${active.command.slice(4)}`
    spawn.stdio = ['ignore', 'inherit', 'inherit']
    active.spawned = true
    return result
  }
  const original = require(entry)
  const observed = async (options) => {
    let row
    let before
    try {
      if (active || !options || typeof options.path !== 'string') fail()
      const owner = fs.realpathSync(options.path)
      // npm invokes absent root hooks too. Preserve those real no-op calls, never a root command.
      if (owner === installRoot) {
        const current = strictJson(
          readBounded(path.join(installRoot, 'package.json'))
        )
        if (
          !ROOT_HOOKS.includes(options.event) ||
          options.cmd !== undefined ||
          options.args?.length ||
          ROOT_HOOKS.some((event) =>
            Object.hasOwn(current.scripts ?? {}, event)
          ) ||
          fs.existsSync(path.join(installRoot, 'binding.gyp'))
        )
          fail()
        return await original(options)
      }
      const location = path
        .relative(installRoot, owner)
        .split(path.sep)
        .join('/')
      const locked = lock.packages[location]
      const pkg = strictJson(readBounded(path.join(owner, 'package.json')))
      const command = pkg.scripts?.[options.event]
      const expected = HOOKS.get(`${pkg.name}@${pkg.version}:${options.event}`)
      const lockName = location
        .split('/node_modules/')
        .at(-1)
        .replace(/^node_modules\//, '')
      if (
        !inside(installRoot, owner) ||
        owner === installRoot ||
        !locked ||
        pkg.name !== lockName ||
        pkg.version !== locked.version ||
        !expected ||
        command !== expected ||
        options.cmd !== undefined ||
        options.args?.length ||
        options.pkg?.name !== pkg.name ||
        options.pkg?.version !== pkg.version ||
        options.pkg?.scripts?.[options.event] !== command ||
        events.some(
          (event) =>
            event.stage === 'start' &&
            event.lock_path === location &&
            event.event === options.event
        )
      )
        fail()
      before = snapshotTree(installRoot)
      if (nativeEsbuild && location === 'node_modules/esbuild') {
        validateNativeExpectation(nativeEsbuild)
        const native = before.get(`${nativeEsbuild.lock_path}/bin/esbuild`)
        if (
          native?.kind !== 'file' ||
          native.sha256 !== nativeEsbuild.sha256 ||
          native.size !== nativeEsbuild.size ||
          native.executable !== true ||
          lock.packages[nativeEsbuild.lock_path]?.version !== '0.27.3'
        )
          fail()
      }
      row = {
        lock_path: location,
        event: options.event,
        script_sha256: sha256(command),
      }
      events.push({ ...row, stage: 'start', changes: [] })
      active = { ...row, path: options.path, command, spawned: false }
      const result = await original(options)
      if (!active.spawned || result.code !== 0 || result.signal) fail()
      if (nativeEsbuild && location === 'node_modules/esbuild') {
        const normalization = normalizeEsbuildHardlink({
          installRoot,
          native: nativeEsbuild,
        })
        if (normalization) normalizations.push(normalization)
      }
      const changes = changesAfter(before, snapshotTree(installRoot), location)
      events.push({
        ...row,
        stage: 'success',
        exit_code: result.code,
        signal: null,
        changes,
      })
      return result
    } catch (error) {
      rejected = true
      if (row) {
        let changes = []
        try {
          changes = changesAfter(
            before,
            snapshotTree(installRoot),
            row.lock_path
          )
        } catch {
          /* Failed observations authorize nothing. */
        }
        events.push({
          ...row,
          stage: 'failure',
          exit_code: Number.isInteger(error?.code) ? error.code : null,
          signal: typeof error?.signal === 'string' ? error.signal : null,
          changes,
        })
      }
      throw error
    } finally {
      active = null
    }
  }
  Object.assign(observed, original)
  require.cache[entry].exports = observed
  return { events, normalizations, failed: () => rejected || active !== null }
}

/** Measure preparation using existing collector semantics after actual successful hooks. */
export function buildPreparation({ repoRoot, observation }) {
  if (observation.failed()) fail()
  const installRoot = path.join(repoRoot, 'crates/trusted-server-js/lib')
  const owners = installedOrigins({
    repoRoot,
    lockFile: path.join(installRoot, 'package-lock.json'),
  })
  const packages = installedMaterials(owners)
  const lifecycle = observation.events.filter(
    (event) => event.stage === 'success'
  )
  const expected = expectedLifecycleEvents({ installRoot, packages })
  if (lifecycle.length !== expected.length) fail()
  const lifecycle_events = expected.map((event) => {
    if (
      !lifecycle.some(
        (row) =>
          `npm:${row.lock_path}` === event.package_material_id &&
          row.event === event.event &&
          row.script_sha256 === event.script_sha256
      )
    )
      fail()
    return event
  })
  const lifecycle_outputs = lifecycle.flatMap((event) =>
    event.changes
      .filter((change) => change.after?.kind === 'file')
      .map((change) => ({
        kind: 'lifecycle-output',
        id: `lifecycle-output:${event.lock_path}:${event.event}:${change.path}`,
        package_material_id: `npm:${event.lock_path}`,
        event: event.event,
        script_sha256: event.script_sha256,
        path: change.path,
        size: change.after.size,
        sha256: change.after.sha256,
      }))
  )
  const measured = actualToolFiles({ installRoot })
  const distributions = measured.map((tool) => ({
    material: {
      kind: 'tool-distribution',
      id: `tool:${tool.name}`,
      name: tool.name,
      version: tool.version,
      sha256: tool.sha256,
    },
    path: tool.path,
  }))
  const tools = measured.map((tool) => ({
    name: tool.name,
    version: tool.version,
    role: tool.role,
    material_id: `tool:${tool.name}`,
  }))
  const preparation = {
    schema_version: 1,
    packages,
    distributions,
    tools,
    lifecycle_events,
    lifecycle_outputs,
  }
  validatePreparation(preparation, { installRoot, packages })
  return preparation
}

/** Seal exact JSON bytes; child output cannot forge a valid result without the parent key. */
export function sealObservation(value, key) {
  const bytes = Buffer.from(JSON.stringify(value))
  if (bytes.length > MAX_JSON || !/^[a-f0-9]{64}$/.test(key)) fail()
  const signature = crypto
    .createHmac('sha256', Buffer.from(key, 'hex'))
    .update(bytes)
    .digest('hex')
  return `TS_RELEASE_NPM_OBSERVATION_V1 ${bytes.toString('base64')} ${signature}\n`
}

function readRequest() {
  const bytes = Buffer.alloc(MAX_JSON + 1)
  let count = 0
  while (count < bytes.length) {
    const read = fs.readSync(0, bytes, count, bytes.length - count, null)
    if (!read) break
    count += read
  }
  if (count > MAX_JSON) fail()
  return parseRequest(bytes.subarray(0, count))
}

function main() {
  // Protect the trusted npm process before reading the parent's private key.
  // The Rust supervisor authenticates and mounts this selected native guard read-only.
  if (
    process.platform !== 'linux' ||
    process.argv.length !== 4 ||
    process.argv[2] !== '--guard'
  )
    fail()
  const guard = process.argv[3]
  if (
    !path.isAbsolute(guard) ||
    !guard.endsWith('.node') ||
    fs.realpathSync(guard) !== guard ||
    /[\\\x00-\x1f\x7f]/.test(guard) ||
    guard.split('/').some((part) => part === '.' || part === '..') ||
    require(guard).protected !== true
  )
    fail()
  const request = readRequest()
  const key = request.key
  delete request.key
  if (
    process.platform !== 'linux' ||
    request.guard_path !== guard ||
    process.cwd() !==
      path.join(request.repo_root, 'crates/trusted-server-js/lib') ||
    process.env.NODE_OPTIONS !== undefined ||
    process.env.NODE_PATH !== undefined ||
    Object.keys(process.env).some((name) => /^npm_config_/i.test(name)) ||
    fs.existsSync(path.join(process.cwd(), '.npmrc')) ||
    fs.existsSync(path.join(request.cache, 'release-disabled-global.npmrc')) ||
    process.env.TRUSTED_SERVER__RELEASE_BUILD !== '1' ||
    process.env.TRUSTED_SERVER__GIT_VERSION !== request.source_commit
  )
    fail()
  const npmRoot = path.resolve(
    path.dirname(process.execPath),
    '../lib/node_modules/npm'
  )
  const observation = installLifecycleObserver({
    npmRoot,
    installRoot: process.cwd(),
    nodePath: process.execPath,
    nativeEsbuild: request.native_esbuild,
  })
  process.once('exit', (code) => {
    let preparation = null
    let success = code === 0 && !observation.failed()
    if (success) {
      try {
        preparation = buildPreparation({
          repoRoot: request.repo_root,
          observation,
        })
      } catch {
        success = false
      }
    }
    const summary = {
      ...request,
      success,
      npm_exit_code: code,
      events: observation.events,
      normalizations: observation.normalizations,
      preparation,
    }
    try {
      fs.writeSync(1, `\n${sealObservation(summary, key)}`)
    } catch {
      success = false
    }
    if (!success) process.exitCode = 1
  })
  process.argv = [
    process.execPath,
    path.join(npmRoot, 'bin/npm-cli.js'),
    'ci',
    '--offline',
    '--include=dev',
    '--no-audit',
    '--no-fund',
    '--foreground-scripts',
    '--ignore-scripts=false',
    '--userconfig=/dev/null',
    `--globalconfig=${path.join(request.cache, 'release-disabled-global.npmrc')}`,
    '--script-shell=/bin/sh',
    '--progress=false',
    '--cache',
    request.cache,
  ]
  require(path.join(npmRoot, 'bin/npm-cli.js'))
}

if (
  process.argv[1] &&
  path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  try {
    main()
  } catch {
    process.stderr.write('Release npm observation rejected\n')
    process.exitCode = 1
  }
}
