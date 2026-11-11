import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import * as fs from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { test } from 'node:test'
import {
  checkDownload,
  checkManifest,
  packagePlan,
  packageReference,
  publishPackages,
} from './packages.mjs'
import { releaseAssets } from './publish.mjs'

const repo = 'Example-Org/trusted-server'
const tag = 'v1.2.3'
const commit = 'a'.repeat(40)
const hash = (bytes) => createHash('sha256').update(bytes).digest('hex')

/** Writes a complete fictional release and its SHA256SUMS. */
function releaseDirectory() {
  const directory = fs.mkdtempSync(join(tmpdir(), 'packages-'))
  const names = packagePlan(repo, tag, commit).flatMap(({ files }) =>
    files.map(({ name }) => name)
  )
  const sums = names
    .map((name) => {
      const body = `fictional ${name}\n`
      fs.writeFileSync(join(directory, name), body)
      return `${hash(body)}  ${name}\n`
    })
    .join('')
  fs.writeFileSync(join(directory, 'SHA256SUMS'), sums)
  return directory
}

/** Builds the manifest a correct push of `entry` would store. */
function manifestFor(entry, sums) {
  return {
    artifactType: entry.artifactType,
    config: { mediaType: 'application/vnd.oci.empty.v1+json' },
    annotations: {
      ...entry.annotations,
      'org.opencontainers.image.created': '2026-01-01T00:00:00Z',
    },
    layers: entry.files.map(({ name, mediaType }) => ({
      mediaType,
      digest: `sha256:${sums.get(name)}`,
      annotations: { 'org.opencontainers.image.title': name },
    })),
  }
}

test('maps each release binary to a lowercase GHCR package', () => {
  assert.equal(
    packageReference('Example-Org', tag, 'trusted-server-fastly.wasm'),
    'ghcr.io/example-org/trusted-server-fastly:v1.2.3',
    'should name the Fastly package without a target suffix'
  )
  assert.equal(
    packageReference('Example-Org', tag, 'ts-x86_64-unknown-linux-gnu'),
    'ghcr.io/example-org/trusted-server-cli:v1.2.3-x86_64-unknown-linux-gnu',
    'should tag the CLI package by target'
  )
  assert.throws(
    () => packageReference('Example-Org', tag, 'unexpected.bin'),
    /No package/,
    'should refuse an unknown asset'
  )
})

test('plans every release binary with its evidence files', () => {
  const plan = packagePlan(repo, tag, commit)
  assert.deepEqual(
    plan.map(({ asset }) => asset),
    releaseAssets,
    'should publish exactly the release binaries'
  )
  const wasm = plan.find(({ asset }) => asset.endsWith('.wasm'))
  assert.deepEqual(
    wasm.files.map(({ name }) => name),
    [
      'trusted-server-fastly.wasm',
      'trusted-server-fastly.wasm.spdx.json',
      'trusted-server-fastly.wasm.provenance.sigstore.json',
      'trusted-server-fastly.wasm.sbom.sigstore.json',
      'trusted-server-fastly.wasm.rebuild-check.json',
    ],
    'should carry the rebuild check with the Fastly wasm'
  )
  assert.equal(
    wasm.annotations['org.opencontainers.image.source'],
    'https://github.com/Example-Org/trusted-server',
    'should link the package to its source repository'
  )
  assert.throws(
    () => packagePlan(repo, 'latest', commit),
    /Not a release tag/,
    'should refuse a non-release tag'
  )
  assert.throws(
    () => packagePlan(repo, tag, 'HEAD'),
    /Invalid source commit/,
    'should refuse a symbolic commit'
  )
})

test('accepts a release that matches its SHA256SUMS exactly', () => {
  const directory = releaseDirectory()
  const sums = checkDownload(directory, packagePlan(repo, tag, commit))
  assert.equal(sums.size, 17, 'should list every non-checksum release file')
})

test('rejects changed, extra and missing release files', () => {
  const plan = packagePlan(repo, tag, commit)
  const changed = releaseDirectory()
  fs.appendFileSync(join(changed, 'ts-aarch64-apple-darwin'), 'tampered')
  assert.throws(
    () => checkDownload(changed, plan),
    /Digest mismatch: ts-aarch64-apple-darwin/,
    'should reject bytes that differ from SHA256SUMS'
  )
  const extra = releaseDirectory()
  fs.writeFileSync(join(extra, 'unlisted.txt'), 'extra')
  assert.throws(
    () => checkDownload(extra, plan),
    /differ from SHA256SUMS/,
    'should reject a file SHA256SUMS does not list'
  )
  const missing = releaseDirectory()
  const sums = fs.readFileSync(join(missing, 'SHA256SUMS'), 'utf8')
  fs.writeFileSync(
    join(missing, 'SHA256SUMS'),
    sums.replace(/^.*  ts-x86_64-apple-darwin\.spdx\.json\n/m, '')
  )
  fs.rmSync(join(missing, 'ts-x86_64-apple-darwin.spdx.json'))
  assert.throws(
    () => checkDownload(missing, plan),
    /Release lacks ts-x86_64-apple-darwin\.spdx\.json/,
    'should require every planned evidence file'
  )
})

test('checks the pushed manifest layers, order and annotations', () => {
  const directory = releaseDirectory()
  const plan = packagePlan(repo, tag, commit)
  const sums = checkDownload(directory, plan)
  const [entry] = plan
  checkManifest(manifestFor(entry, sums), entry, sums)
  const swapped = manifestFor(entry, sums)
  swapped.layers.reverse()
  assert.throws(
    () => checkManifest(swapped, entry, sums),
    /differs/,
    'should reject reordered layers'
  )
  const referring = manifestFor(entry, sums)
  referring.subject = {
    mediaType: 'application/vnd.oci.image.manifest.v1+json',
  }
  assert.throws(
    () => checkManifest(referring, entry, sums),
    /Unexpected manifest/,
    'should reject a manifest that refers to another one'
  )
  const unlinked = manifestFor(entry, sums)
  delete unlinked.annotations['org.opencontainers.image.source']
  assert.throws(
    () => checkManifest(unlinked, entry, sums),
    /Annotation org\.opencontainers\.image\.source/,
    'should require the repository link'
  )
})

/** Returns an error shaped like a failed `oras` call with this stderr. */
function registryError(stderr) {
  return Object.assign(Error('Command failed'), { stderr })
}

/** Builds a fake `gh`/`oras` runner over an in-memory registry. */
function fakeRunner(
  directory,
  plan,
  sums,
  { registry = new Map(), fail } = {}
) {
  const calls = []
  const run = (command, args, options = {}) => {
    calls.push([command, ...args])
    if (command === 'gh' && args[1] === 'view')
      return JSON.stringify({ tagName: tag, isDraft: false })
    if (command === 'gh' && args[1] === 'download') {
      const target = args[args.indexOf('--dir') + 1]
      for (const name of fs.readdirSync(directory))
        fs.copyFileSync(join(directory, name), join(target, name))
      return ''
    }
    if (command === 'gh' && args[1] === 'verify-asset') {
      assert.match(args[3], /SHA256SUMS$/, 'should authenticate SHA256SUMS')
      return ''
    }
    if (command === 'oras' && args[0] === 'push') {
      assert.ok(options.cwd, 'should push from the download')
      const entry = plan.find(({ reference }) => reference === args[1])
      registry.set(args[1], manifestFor(entry, sums))
      return ''
    }
    if (command === 'oras' && args[0] === 'manifest') {
      const reference = args.at(-1)
      if (fail?.(reference))
        throw registryError('Error: 503 Service Unavailable')
      if (!registry.has(reference))
        throw registryError(
          `Error response from registry: ${reference}: not found`
        )
      return JSON.stringify(registry.get(reference))
    }
    throw Error(`unexpected call ${command} ${args.join(' ')}`)
  }
  return { run, calls, registry }
}

const enabledEnv = {
  GH_REPO: repo,
  RELEASE_TAG: tag,
  SOURCE_COMMIT: commit,
  PACKAGE_PUBLICATION_ENABLED: 'true',
  RUNNER_TEMP: tmpdir(),
}

test('publishes every package once from the authenticated release', () => {
  const directory = releaseDirectory()
  const plan = packagePlan(repo, tag, commit)
  const sums = checkDownload(directory, plan)
  const { run, calls, registry } = fakeRunner(directory, plan, sums)
  publishPackages({ env: enabledEnv, run, log: () => {} })
  assert.deepEqual(
    [...registry.keys()],
    plan.map(({ reference }) => reference),
    'should push one artifact per release binary'
  )
  const verify = calls.findIndex((call) => call[2] === 'verify-asset')
  const push = calls.findIndex((call) => call[1] === 'push')
  assert.ok(
    verify >= 0 && verify < push,
    'should authenticate SHA256SUMS before pushing'
  )
  calls.length = 0
  publishPackages({ env: enabledEnv, run, log: () => {} })
  assert.equal(
    calls.filter((call) => call[1] === 'push').length,
    0,
    'should keep matching tags on a re-run instead of pushing again'
  )
  assert.throws(
    () =>
      publishPackages({
        env: { ...enabledEnv, PACKAGE_PUBLICATION_ENABLED: 'false' },
        run,
        log: () => {},
      }),
    /not enabled/,
    'should stay closed until a maintainer enables publication'
  )
})

test('completes a partial publish and refuses differing tags', () => {
  const directory = releaseDirectory()
  const plan = packagePlan(repo, tag, commit)
  const sums = checkDownload(directory, plan)
  const registry = new Map([[plan[0].reference, manifestFor(plan[0], sums)]])
  const partial = fakeRunner(directory, plan, sums, { registry })
  publishPackages({ env: enabledEnv, run: partial.run, log: () => {} })
  assert.deepEqual(
    partial.calls.filter((call) => call[1] === 'push').map((call) => call[2]),
    plan.slice(1).map(({ reference }) => reference),
    'should push only the missing packages'
  )
  const changed = manifestFor(plan[1], sums)
  changed.layers[0].digest = `sha256:${'0'.repeat(64)}`
  const differing = fakeRunner(directory, plan, sums, {
    registry: new Map([[plan[1].reference, changed]]),
  })
  assert.throws(
    () =>
      publishPackages({ env: enabledEnv, run: differing.run, log: () => {} }),
    /Refusing to overwrite/,
    'should refuse a tag that holds other content'
  )
  assert.equal(
    differing.calls.filter((call) => call[1] === 'push').length,
    0,
    'should refuse before pushing anything'
  )
})

test('treats only the registry not-found answer for the tag as absent', () => {
  const directory = releaseDirectory()
  const plan = packagePlan(repo, tag, commit)
  const sums = checkDownload(directory, plan)
  const { run } = fakeRunner(directory, plan, sums)
  const credentials = (command, args, options) => {
    if (command === 'oras' && args[0] === 'manifest')
      throw registryError('Error: credentials not found in native keychain')
    return run(command, args, options)
  }
  assert.throws(
    () => publishPackages({ env: enabledEnv, run: credentials, log: () => {} }),
    /Cannot check whether/,
    'should not read an unrelated "not found" as an absent tag'
  )
})

test('stops when the registry cannot say whether a tag exists', () => {
  const directory = releaseDirectory()
  const plan = packagePlan(repo, tag, commit)
  const sums = checkDownload(directory, plan)
  const flaky = fakeRunner(directory, plan, sums, {
    fail: (reference) => reference === plan[2].reference,
  })
  assert.throws(
    () => publishPackages({ env: enabledEnv, run: flaky.run, log: () => {} }),
    /Cannot check whether .* exists: Error: 503/,
    'should not treat a registry error as an absent tag'
  )
  assert.equal(
    flaky.calls.filter((call) => call[1] === 'push').length,
    0,
    'should push nothing after an unknown existence check'
  )
})

test('refuses a draft release', () => {
  const run = (command, args) => {
    if (command === 'gh' && args[1] === 'view')
      return JSON.stringify({ tagName: tag, isDraft: true })
    throw Error('should stop before any other call')
  }
  assert.throws(
    () =>
      publishPackages({
        env: {
          GH_REPO: repo,
          RELEASE_TAG: tag,
          SOURCE_COMMIT: commit,
          PACKAGE_PUBLICATION_ENABLED: 'true',
        },
        run,
        log: () => {},
      }),
    /is not published/,
    'should publish packages only from a published release'
  )
})

test('keeps the Axum image on the repository tool pins', () => {
  const read = (path) =>
    fs.readFileSync(new URL(`../../${path}`, import.meta.url), 'utf8')
  const dockerfile = read('crates/trusted-server-adapter-axum/Dockerfile')
  const node = read('.tool-versions').match(/^nodejs\s+(\S+)$/m)[1]
  const rust = read('rust-toolchain.toml').match(/^channel = "([^"]+)"$/m)[1]
  assert.match(
    dockerfile,
    new RegExp(
      `library/node:${node.replaceAll('.', '\\.')}-bookworm-slim@sha256:[0-9a-f]{64} `
    ),
    'should build with the .tool-versions Node, pinned by digest'
  )
  assert.ok(
    dockerfile.includes(`test "$(node --version)" = v${node};`),
    'should assert the copied Node version'
  )
  assert.match(
    dockerfile,
    new RegExp(
      `library/rust:${rust.replaceAll('.', '\\.')}-bookworm@sha256:[0-9a-f]{64} `
    ),
    'should build with the rust-toolchain.toml channel, pinned by digest'
  )
  assert.ok(
    dockerfile.includes(`ENV RUSTUP_TOOLCHAIN=${rust}\n`),
    'should pin rustup to the installed channel'
  )
  assert.match(
    dockerfile,
    /library\/debian:bookworm-slim@sha256:[0-9a-f]{64}\n/,
    'should pin the runtime base by digest'
  )
  assert.match(dockerfile, /\nUSER 10001:10001\n/, 'should run as UID 10001')
  assert.match(dockerfile, /\nSTOPSIGNAL SIGINT\n/, 'should stop with SIGINT')
})
