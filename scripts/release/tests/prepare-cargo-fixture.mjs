/**
 * Acquire ordinary CI Cargo fixture inputs.
 *
 * This is unauthenticated ordinary CI acquisition: it copies the runner's
 * rustup toolchain and downloads crates and Git objects over the network. It
 * does not authenticate or qualify any release tool, image or input.
 */
import crypto from 'node:crypto'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { execFileSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'

const WASM_TARGET = 'wasm32-wasip1'
const CRATES_IO = 'registry+https://github.com/rust-lang/crates.io-index'
const MAXIMUM_CRATE = 64 * 1024 * 1024

const root = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  '../../..'
)

/** The pinned Rust version: `.tool-versions` and `rust-toolchain.toml` must agree. */
function pinnedRustVersion() {
  const pins = fs
    .readFileSync(path.join(root, '.tool-versions'), 'utf8')
    .split('\n')
    .map((line) => line.trim().split(/\s+/))
    .filter(([tool]) => tool === 'rust')
  const channel = /^channel = "([^"]+)"$/m.exec(
    fs.readFileSync(path.join(root, 'rust-toolchain.toml'), 'utf8')
  )
  if (
    pins.length !== 1 ||
    !/^\d+\.\d+\.\d+$/.test(pins[0][1] ?? '') ||
    channel?.[1] !== pins[0][1]
  )
    throw new Error('Expected one exact Rust pin shared by both tool files')
  return pins[0][1]
}
const RUST_VERSION = pinnedRustVersion()
const [destinationArgument, producerArgument] = process.argv.slice(2)
if (process.platform !== 'linux')
  throw new Error('Cargo fixture requires Linux')
if (!destinationArgument || !producerArgument || process.argv.length !== 4)
  throw new Error(
    'Expected fresh fixture destination and source-built producer'
  )
const destination = path.resolve(destinationArgument)
const producer = fs.realpathSync(producerArgument)
if (fs.existsSync(destination))
  throw new Error('Fixture destination must be fresh')
fs.mkdirSync(destination, { recursive: true, mode: 0o700 })
const tools = path.join(destination, 'tools')
const work = path.join(destination, 'work')
const source = path.join(work, 'source')
const cargoHome = path.join(work, 'cargo-home')
const vendor = path.join(work, 'vendor')
fs.mkdirSync(work, { mode: 0o700 })

const gitEnvironment = {
  GIT_CONFIG_NOSYSTEM: '1',
  GIT_CONFIG_GLOBAL: '/dev/null',
  GIT_TERMINAL_PROMPT: '0',
}
const hostEnvironment = {
  PATH: process.env.PATH,
  HOME: os.homedir(),
  RUSTUP_TOOLCHAIN: RUST_VERSION,
  ...(process.env.RUSTUP_HOME ? { RUSTUP_HOME: process.env.RUSTUP_HOME } : {}),
  ...gitEnvironment,
}
// Cargo runs from the private tool copy with a fresh home and no rustup.
const cargoEnvironment = {
  PATH: `${path.join(tools, 'bin')}:/usr/bin:/bin`,
  HOME: os.homedir(),
  CARGO_HOME: cargoHome,
  RUSTC: path.join(tools, 'bin/rustc'),
  ...gitEnvironment,
}

const run = (program, args, { cwd = root, env = hostEnvironment } = {}) =>
  execFileSync(program, args, {
    cwd,
    env,
    timeout: 20 * 60_000,
    maxBuffer: 256 * 1024 * 1024,
    stdio: ['ignore', 'pipe', 'inherit'],
  })
const text = (program, args, options) =>
  run(program, args, options).toString('utf8').trim()
const sha256 = (bytes) =>
  crypto.createHash('sha256').update(bytes).digest('hex')

// 1. The private single-link Rust tool root, with the staged probe inside.
const sysroot = text('rustc', ['--print', 'sysroot'])
const rustcVersion = text(path.join(sysroot, 'bin/rustc'), ['--version'])
if (!rustcVersion.startsWith(`rustc ${RUST_VERSION} `))
  throw new Error(
    `Fixture requires Rust ${RUST_VERSION}, found ${rustcVersion}`
  )
const wasmStd = path.join(sysroot, 'lib/rustlib', WASM_TARGET, 'lib')
if (
  !fs.existsSync(wasmStd) ||
  !fs.readdirSync(wasmStd).some((name) => name.startsWith('libstd-'))
)
  throw new Error(
    `Selected toolchain lacks the ${WASM_TARGET} standard library`
  )
fs.cpSync(sysroot, tools, {
  recursive: true,
  dereference: false,
  verbatimSymlinks: true,
})
fs.copyFileSync(producer, path.join(tools, 'probe'), fs.constants.COPYFILE_EXCL)
fs.chmodSync(path.join(tools, 'probe'), 0o700)

// 2. The exact tracked source at HEAD, never the developer working tree.
run('git', ['clone', '--no-hardlinks', '--no-checkout', '--', root, source])
run('git', ['checkout', '--detach', '--force', 'HEAD'], { cwd: source })
const commit = text('git', ['rev-parse', 'HEAD'], { cwd: source })
const lockBytes = fs.readFileSync(path.join(source, 'Cargo.lock'))

// 3. Unauthenticated network acquisition into a fresh Cargo home.
process.stderr.write(
  'Acquiring Cargo originals without authentication (ordinary CI only)\n'
)
run(path.join(tools, 'bin/cargo'), ['fetch', '--locked'], {
  cwd: source,
  env: cargoEnvironment,
})

/** Minimal `Cargo.lock` reader: one record per `[[package]]` table. */
function lockedPackages(bytes) {
  const packages = []
  let current = null
  for (const line of bytes.toString('utf8').split('\n')) {
    if (line === '[[package]]') {
      current = {}
      packages.push(current)
      continue
    }
    const field = /^(name|version|source|checksum) = "([^"]*)"$/.exec(line)
    if (current && field) current[field[1]] = field[2]
  }
  return packages
}
const locked = lockedPackages(lockBytes)

// 4. Each original registry archive, checked against its locked checksum.
const caches = fs.readdirSync(path.join(cargoHome, 'registry/cache'))
if (caches.length !== 1)
  throw new Error('Expected exactly one crates.io archive cache')
const cache = path.join(cargoHome, 'registry/cache', caches[0])
fs.mkdirSync(path.join(destination, 'crates'), { mode: 0o700 })
const registry = []
for (const record of locked.filter((entry) => entry.source === CRATES_IO)) {
  if (!/^[a-f0-9]{64}$/.test(record.checksum ?? ''))
    throw new Error(`Registry crate ${record.name} has no locked checksum`)
  const file = `${record.name}-${record.version}.crate`
  const bytes = fs.readFileSync(path.join(cache, file))
  if (bytes.length > MAXIMUM_CRATE || sha256(bytes) !== record.checksum)
    throw new Error(`Registry crate ${file} differs from its locked checksum`)
  fs.writeFileSync(path.join(destination, 'crates', file), bytes, {
    flag: 'wx',
    mode: 0o600,
  })
  registry.push({
    name: record.name,
    version: record.version,
    path: `crates/${file}`,
  })
}

// 5. Git objects of every locked Git source; loose and pack objects only.
const gitPackages = locked
  .filter((entry) => entry.source?.startsWith('git+'))
  .map((entry) => {
    const [selector, revision] = entry.source.split('#')
    if (!/^[a-f0-9]{40}$/.test(revision ?? ''))
      throw new Error(`Git package ${entry.name} has no full revision`)
    return { ...entry, selector, revision }
  })
const databases = fs
  .readdirSync(path.join(cargoHome, 'git/db'))
  .map((name) => path.join(cargoHome, 'git/db', name))
const containsCommit = (database, revision) => {
  try {
    execFileSync(
      'git',
      [`--git-dir=${database}`, 'cat-file', '-e', `${revision}^{commit}`],
      { env: hostEnvironment, timeout: 60_000, stdio: 'ignore' }
    )
    return true
  } catch {
    return false
  }
}
function copyObjects(input, output) {
  fs.mkdirSync(output, { recursive: true, mode: 0o700 })
  for (const name of fs.readdirSync(input)) {
    // Commit graphs and alternates live under info/; the verifier needs none.
    if (name === 'info') continue
    const loose = /^[a-f0-9]{2}$/.test(name)
    if (!loose && name !== 'pack')
      throw new Error(`Unexpected Git object entry ${name}`)
    fs.mkdirSync(path.join(output, name), { mode: 0o700 })
    for (const child of fs.readdirSync(path.join(input, name))) {
      if (name === 'pack' && child.endsWith('.keep')) continue
      const accepted = loose
        ? /^[a-f0-9]{38}$/.test(child)
        : /^pack-[a-f0-9]{40}\.(pack|idx|rev)$/.test(child)
      if (!accepted) throw new Error(`Unexpected Git object file ${child}`)
      fs.copyFileSync(
        path.join(input, name, child),
        path.join(output, name, child),
        fs.constants.COPYFILE_EXCL
      )
    }
  }
}
const git = []
const databaseOf = new Map()
for (const { selector, revision } of gitPackages) {
  if (git.some((entry) => entry.selector === selector)) continue
  const matching = databases.filter((database) =>
    containsCommit(database, revision)
  )
  if (matching.length !== 1)
    throw new Error(`Expected one Git database holding ${revision}`)
  const objects = `git/${git.length}/objects`
  copyObjects(
    path.join(matching[0], 'objects'),
    path.join(destination, objects)
  )
  databaseOf.set(selector, matching[0])
  git.push({ selector, revision, objects })
}

// 6. Normalized Git manifests from `cargo vendor`, paired with the committed
//    original read from the Git object database at the locked revision.
run(
  path.join(tools, 'bin/cargo'),
  ['vendor', '--locked', '--offline', '--versioned-dirs', vendor],
  { cwd: source, env: cargoEnvironment }
)
const metadata = JSON.parse(
  run(
    path.join(tools, 'bin/cargo'),
    ['metadata', '--format-version', '1', '--locked', '--offline'],
    { cwd: source, env: cargoEnvironment }
  ).toString('utf8')
)
const checkouts = path.join(cargoHome, 'git/checkouts') + path.sep
const normalized = []
let vendoredOriginals = 0
for (const record of gitPackages) {
  const described = metadata.packages.filter(
    (entry) =>
      entry.name === record.name &&
      entry.version === record.version &&
      entry.source === record.source
  )
  if (described.length !== 1)
    throw new Error(`Expected one metadata record for ${record.name}`)
  const manifestPath = described[0].manifest_path
  if (!manifestPath.startsWith(checkouts))
    throw new Error(`Git manifest ${manifestPath} is outside the checkouts`)
  // <checkouts>/<repository>/<short revision>/<path in repository>
  const relative = manifestPath.slice(checkouts.length).split(path.sep)
  const repositoryPath = relative.slice(2).join('/')
  const original = run('git', [
    `--git-dir=${databaseOf.get(record.selector)}`,
    'show',
    `${record.revision}:${repositoryPath}`,
  ])
  if (!original.equals(fs.readFileSync(manifestPath)))
    throw new Error(`Checkout manifest of ${record.name} differs from its blob`)
  const directory = `${record.name}-${record.version}`
  const vendored = path.join(vendor, directory)
  if (fs.existsSync(path.join(vendored, 'Cargo.toml.orig'))) vendoredOriginals++
  const output = `normalized/${directory}`
  fs.mkdirSync(path.join(destination, output), {
    recursive: true,
    mode: 0o700,
  })
  fs.copyFileSync(
    path.join(vendored, 'Cargo.toml'),
    path.join(destination, output, 'Cargo.toml'),
    fs.constants.COPYFILE_EXCL
  )
  fs.writeFileSync(
    path.join(destination, output, 'Cargo.toml.orig'),
    original,
    {
      flag: 'wx',
      mode: 0o600,
    }
  )
  normalized.push({
    name: record.name,
    version: record.version,
    selector: record.selector,
    cargo_toml: `${output}/Cargo.toml`,
    cargo_toml_orig: `${output}/Cargo.toml.orig`,
  })
}

// 7. The ordinary CI image with a host linker, identified only by its ID.
const dockerConfig = path.join(work, 'docker-config')
fs.mkdirSync(dockerConfig, { mode: 0o700 })
const imageFile = path.join(work, 'image-id')
run('/usr/bin/docker', [
  '--config',
  dockerConfig,
  '--host',
  'unix:///var/run/docker.sock',
  'build',
  '--pull',
  '--iidfile',
  imageFile,
  '--file',
  path.join(source, 'scripts/release/tests/cargo-fixture.Dockerfile'),
  path.join(source, 'scripts/release/tests'),
])
const image = fs.readFileSync(imageFile, 'utf8').trim()
if (!/^sha256:[a-f0-9]{64}$/.test(image))
  throw new Error('Expected actual local fixture image identity')

fs.rmSync(work, { recursive: true, force: true })
fs.writeFileSync(
  path.join(destination, 'fixture.json'),
  JSON.stringify({
    schema_version: 1,
    kind: 'ordinary-ci-cargo-fixture',
    acquisition: 'unauthenticated-ordinary-ci',
    source_commit: commit,
    cargo_lock_sha256: sha256(lockBytes),
    image_id: image,
    rustc_version: rustcVersion,
    registry,
    git,
    normalized,
  }) + '\n',
  { flag: 'wx', mode: 0o600 }
)
process.stdout.write(
  `Prepared ${registry.length} registry originals, ${git.length} Git object ` +
    `stores and ${normalized.length} normalized Git manifests ` +
    `(cargo vendor wrote Cargo.toml.orig for ${vendoredOriginals}) by ` +
    'unauthenticated ordinary CI acquisition; this is not release qualification\n'
)
