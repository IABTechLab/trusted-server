/** Acquire ordinary CI fixture inputs; this does not authenticate release tools. */
import crypto from 'node:crypto'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { execFileSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'

const root = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  '../../..'
)
const [destinationArgument, producerArgument] = process.argv.slice(2)
if (process.platform !== 'linux' || process.version !== 'v24.20.0')
  throw new Error('Full JS fixture requires Linux and Node 24.20.0')
if (!destinationArgument || !producerArgument || process.argv.length !== 4)
  throw new Error(
    'Expected fresh fixture destination and source-built producer'
  )
const destination = path.resolve(destinationArgument)
const producer = fs.realpathSync(producerArgument)
if (fs.existsSync(destination))
  throw new Error('Fixture destination must be fresh')
fs.mkdirSync(destination, { recursive: true, mode: 0o700 })
const source = path.join(destination, 'source')
const tools = path.join(destination, 'tools')
const originals = path.join(destination, 'originals')
const environment = {
  PATH: process.env.PATH,
  HOME: os.homedir(),
  RUSTUP_TOOLCHAIN: '1.98.1',
  ...(process.env.RUSTUP_HOME ? { RUSTUP_HOME: process.env.RUSTUP_HOME } : {}),
  GIT_CONFIG_NOSYSTEM: '1',
  GIT_CONFIG_GLOBAL: '/dev/null',
  GIT_TERMINAL_PROMPT: '0',
}
const execute = (program, args, cwd = root) =>
  execFileSync(program, args, {
    cwd,
    env: environment,
    timeout: 180_000,
    maxBuffer: 1024 * 1024,
    encoding: 'utf8',
  }).trim()
execute('git', ['clone', '--no-hardlinks', '--no-checkout', '--', root, source])
execute('git', ['checkout', '--detach', '--force', 'HEAD'], source)
fs.writeFileSync(
  path.join(source, '.git/config'),
  '[core]\nrepositoryformatversion = 0\nbare = false\nfilemode = true\n'
)
const commit = execute('git', ['rev-parse', 'HEAD'], source)
const nodeRoot = path.resolve(
  path.dirname(fs.realpathSync(process.execPath)),
  '..'
)
const npm = JSON.parse(
  fs.readFileSync(path.join(nodeRoot, 'lib/node_modules/npm/package.json'))
)
if (npm.version !== '11.19.0') throw new Error('Fixture requires npm 11.19.0')
fs.cpSync(nodeRoot, tools, {
  recursive: true,
  dereference: false,
  verbatimSymlinks: true,
})
fs.copyFileSync(producer, path.join(tools, 'probe'), fs.constants.COPYFILE_EXCL)
fs.chmodSync(path.join(tools, 'probe'), 0o700)
if (!execute('rustc', ['--version']).startsWith('rustc 1.98.1 '))
  throw new Error('Fixture requires selected Rust 1.98.1')
execute('rustc', [
  '--crate-type',
  'cdylib',
  '--edition',
  '2024',
  path.join(source, 'crates/trusted-server-release/src/lifecycle_guard.rs'),
  '-o',
  path.join(tools, 'guard.node'),
])
execute(path.join(tools, 'bin/node'), [
  '-e',
  `
  const assert = require('node:assert/strict');
  const { spawnSync } = require('node:child_process');
  assert.equal(require(process.argv[1]).protected, true);
  const child = spawnSync(process.execPath, ['-e', String.raw\`
    const fs = require('node:fs');
    for (const member of ['mem', 'environ', 'fd/0']) {
      try {
        const descriptor = fs.openSync('/proc/' + process.argv[1] + '/' + member, 'r');
        fs.closeSync(descriptor);
        throw new Error('Observer process input or memory is accessible');
      } catch (error) {
        if (!['EACCES', 'EPERM'].includes(error.code)) throw error;
      }
    }
    process.stdout.write('guarded\\n');
  \`, String(process.pid)], { env: {}, encoding: 'utf8', timeout: 10000, stdio: ['ignore', 'pipe', 'pipe'] });
  assert.equal(child.status, 0, child.stderr);
  assert.equal(child.stdout, 'guarded\\n');
`,
  path.join(tools, 'guard.node'),
])
fs.mkdirSync(originals, { mode: 0o700 })
const lock = JSON.parse(
  fs.readFileSync(
    path.join(source, 'crates/trusted-server-js/lib/package-lock.json')
  )
)
if (lock.lockfileVersion !== 3)
  throw new Error('Expected locked npm v3 fixture')
const requests = new Map()
for (const [location, record] of Object.entries(lock.packages)) {
  if (location === '') continue
  if (!/^sha512-[A-Za-z0-9+/]{86}==$/.test(record.integrity ?? ''))
    throw new Error('Fixture requires exact locked original SHA-512')
  const digest = Buffer.from(record.integrity.slice(7), 'base64').toString(
    'hex'
  )
  const url = new URL(record.resolved)
  if (
    url.protocol !== 'https:' ||
    url.hostname !== 'registry.npmjs.org' ||
    url.port ||
    url.username ||
    url.password ||
    url.search ||
    url.hash ||
    url.href !== record.resolved ||
    !url.pathname.endsWith('.tgz')
  )
    throw new Error('Fixture rejects noncanonical registry URL')
  requests.set(digest, url.href)
}
let total = 0
const maximum = 64 * 1024 * 1024
const deadline = Date.now() + 10 * 60_000
const queue = [...requests]
async function acquire([digest, url]) {
  const cached = path.join(
    os.homedir(),
    '.npm/_cacache/content-v2/sha512',
    digest.slice(0, 2),
    digest.slice(2, 4),
    digest.slice(4)
  )
  let bytes
  try {
    const metadata = fs.lstatSync(cached)
    if (metadata.isFile() && metadata.size <= maximum) {
      const candidate = fs.readFileSync(cached)
      if (
        crypto.createHash('sha512').update(candidate).digest('hex') === digest
      )
        bytes = candidate
    }
  } catch (error) {
    if (error.code !== 'ENOENT') throw error
  }
  if (!bytes) {
    const response = await fetch(url, {
      redirect: 'error',
      signal: AbortSignal.timeout(60_000),
    })
    if (
      !response.ok ||
      Number(response.headers.get('content-length') ?? 0) > maximum
    )
      throw new Error('Fixture original download failed or exceeds bound')
    const pieces = []
    let size = 0
    for await (const piece of response.body) {
      size += piece.length
      if (size > maximum || Date.now() > deadline)
        throw new Error('Fixture download limit exceeded')
      pieces.push(piece)
    }
    bytes = Buffer.concat(pieces)
  }
  if (crypto.createHash('sha512').update(bytes).digest('hex') !== digest)
    throw new Error('Fixture original differs from approved lock integrity')
  total += bytes.length
  if (total > 2 * 1024 ** 3 || Date.now() > deadline)
    throw new Error('Fixture acquisition budget exceeded')
  fs.writeFileSync(path.join(originals, `${digest}.tgz`), bytes, {
    flag: 'wx',
    mode: 0o600,
  })
}
await Promise.all(
  Array.from({ length: 4 }, async () => {
    for (;;) {
      const next = queue.pop()
      if (!next) return
      await acquire(next)
    }
  })
)
const dockerConfig = path.join(destination, 'docker-config')
fs.mkdirSync(dockerConfig, { mode: 0o700 })
const imageFile = path.join(destination, 'image-id')
execute('/usr/bin/docker', [
  '--config',
  dockerConfig,
  '--host',
  'unix:///var/run/docker.sock',
  'build',
  '--pull',
  '--iidfile',
  imageFile,
  '--file',
  path.join(source, 'scripts/release/tests/js-fixture.Dockerfile'),
  path.join(source, 'scripts/release/tests'),
])
const image = fs.readFileSync(imageFile, 'utf8').trim()
if (!/^sha256:[a-f0-9]{64}$/.test(image))
  throw new Error('Expected actual local fixture image identity')
fs.writeFileSync(
  path.join(destination, 'fixture.json'),
  JSON.stringify({
    schema_version: 1,
    source_commit: commit,
    original_count: requests.size,
    image_id: image,
  }) + '\n',
  { flag: 'wx', mode: 0o600 }
)
process.stdout.write(
  `Prepared ${requests.size} original-only archives for the full offline JS fixture\n`
)
