import { test } from 'node:test'
import { spawnSync } from 'node:child_process'
import assert from 'node:assert/strict'
import {
  chmodSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { digest, validateArtifacts, verifyTopology } from './network.mjs'

function topology() {
  return {
    container: {
      HostConfig: {
        ReadonlyRootfs: true,
        NetworkMode: 'owned',
        Privileged: false,
        PublishAllPorts: false,
        PortBindings: {},
      },
      NetworkSettings: { Networks: { owned: {} } },
      Mounts: [
        {
          Type: 'bind',
          Source: '/tmp/example-input',
          Destination: '/inputs/guest.wasm',
          RW: false,
        },
      ],
    },
    network: { Name: 'owned', Internal: true, Driver: 'bridge' },
    mounts: [
      {
        source: '/tmp/example-input',
        destination: '/inputs/guest.wasm',
        write: false,
      },
    ],
  }
}
test('accepts exactly one internal worker with immutable input mounts', () => {
  const { container, network, mounts } = topology()
  assert.doesNotThrow(() => verifyTopology(container, network, 'owned', mounts))
})
for (const [label, mutate] of [
  ['external network', ({ network }) => (network.Internal = false)],
  [
    'extra network',
    ({ container }) => (container.NetworkSettings.Networks.external = {}),
  ],
  [
    'host network',
    ({ container }) => (container.HostConfig.NetworkMode = 'host'),
  ],
  [
    'published port',
    ({ container }) =>
      (container.HostConfig.PortBindings = { '18081/tcp': [{}] }),
  ],
  [
    'writable root',
    ({ container }) => (container.HostConfig.ReadonlyRootfs = false),
  ],
  ['writable input', ({ container }) => (container.Mounts[0].RW = true)],
  [
    'Docker socket',
    ({ container }) =>
      container.Mounts.push({
        Type: 'bind',
        Destination: '/var/run/docker.sock',
      }),
  ],
  [
    'privileged worker',
    ({ container }) => (container.HostConfig.Privileged = true),
  ],
]) {
  test(`refuses ${label}`, () => {
    const state = topology()
    mutate(state)
    assert.throws(() =>
      verifyTopology(state.container, state.network, 'owned', state.mounts)
    )
  })
}
test('refuses mismatched artifact bytes, source, features and native platform', () => {
  const root = mkdtempSync(join(tmpdir(), 'example-artifacts-'))
  try {
    const wasm = join(root, 'guest.wasm')
    writeFileSync(wasm, Buffer.from([0, 97, 115, 109, 1, 0, 0, 0]))
    const cli = join(root, 'ts')
    const elf = Buffer.alloc(20)
    elf.set([127, 69, 76, 70, 2, 1])
    elf.writeUInt16LE(62, 18)
    writeFileSync(cli, elf)
    chmodSync(cli, 0o755)
    const source = 'a'.repeat(40)
    const input = {
      productCommit: source,
      variant: 'default',
      profile: 'release',
      testUtils: false,
      producer: 'scripts/release/phase1-build.sh',
      wasm: { path: 'guest.wasm', sha256: digest(wasm), sourceCommit: source },
      cli: { path: 'ts', sha256: digest(cli), sourceCommit: source },
    }
    assert.equal(validateArtifacts(input, root).length, 2)
    for (const change of [
      (copy) => (copy.testUtils = true),
      (copy) => (copy.profile = 'debug'),
      (copy) => (copy.cli.sourceCommit = 'b'.repeat(40)),
      (copy) => (copy.wasm.sha256 = '0'.repeat(64)),
    ]) {
      const copy = structuredClone(input)
      change(copy)
      assert.throws(() => validateArtifacts(copy, root))
    }
    elf.writeUInt16LE(183, 18)
    writeFileSync(cli, elf)
    input.cli.sha256 = digest(cli)
    assert.throws(() => validateArtifacts(input, root), /x86_64/)
  } finally {
    rmSync(root, { recursive: true })
  }
})

for (const resource of ['container', 'network']) {
  for (const cleanupMode of [
    'removed',
    'absent',
    'present',
    'inventory-failed',
  ]) {
    test(`cleans uncertain ${resource} creation; cleanup=${cleanupMode}`, () => {
      const root = mkdtempSync(join(tmpdir(), 'example-rpc-failure-'))
      try {
        const wasm = join(root, 'guest.wasm')
        writeFileSync(wasm, Buffer.from([0, 97, 115, 109, 1, 0, 0, 0]))
        const cli = join(root, 'ts')
        const elf = Buffer.alloc(20)
        elf.set([127, 69, 76, 70, 2, 1])
        elf.writeUInt16LE(62, 18)
        writeFileSync(cli, elf)
        chmodSync(cli, 0o755)
        const source = 'a'.repeat(40)
        const identity = join(root, 'inputs.json')
        writeFileSync(
          identity,
          JSON.stringify({
            productCommit: source,
            variant: 'default',
            profile: 'release',
            testUtils: false,
            producer: 'scripts/release/phase1-build.sh',
            wasm: { path: wasm, sha256: digest(wasm), sourceCommit: source },
            cli: { path: cli, sha256: digest(cli), sourceCommit: source },
          })
        )
        const log = join(root, 'calls.jsonl')
        const fake = join(root, 'docker')
        writeFileSync(
          fake,
          `#!/usr/bin/env node
const fs = require('node:fs')
const args = process.argv.slice(2)
fs.appendFileSync(process.env.FIXTURE_DOCKER_LOG, JSON.stringify(args) + '\\n')
if (args[0] === 'image') process.stdout.write(JSON.stringify([{ Os: 'linux', Architecture: 'amd64', Config: { Labels: { 'org.opencontainers.image.revision': 'a'.repeat(40) } } }]))
const resource = process.env.FIXTURE_CREATE_RESOURCE
if ((resource === 'container' && args[0] === 'create') || (resource === 'network' && args[0] === 'network' && args[1] === 'create')) { process.stderr.write('Simulated lost create reply'); process.exit(1) }
if (process.env.FIXTURE_CLEANUP_MODE !== 'removed' && (args[0] === 'rm' || args[0] === 'network' && args[1] === 'rm')) { process.stderr.write('Simulated already absent resource'); process.exit(1) }
if (args[1] === 'ls') {
  if (process.env.FIXTURE_CLEANUP_MODE === 'inventory-failed') { process.stderr.write('Simulated inventory failure'); process.exit(1) }
  if (process.env.FIXTURE_CLEANUP_MODE === 'present') process.stdout.write(args[args.indexOf('--filter') + 1].slice('name='.length))
}

`
        )
        chmodSync(fake, 0o755)
        const result = spawnSync(
          process.execPath,
          [
            new URL('./network.mjs', import.meta.url).pathname,
            '--image',
            'sha256:' + 'd'.repeat(64),
            '--artifacts',
            identity,
            '--out',
            join(root, 'evidence'),
          ],
          {
            env: {
              ...process.env,
              PATH: root + ':' + process.env.PATH,
              FIXTURE_DOCKER_LOG: log,
              FIXTURE_CREATE_RESOURCE: resource,
              FIXTURE_CLEANUP_MODE: cleanupMode,
            },
            encoding: 'utf8',
          }
        )
        assert.notEqual(
          result.status,
          0,
          'should preserve the failed production run'
        )
        if (['removed', 'absent'].includes(cleanupMode))
          assert.match(result.stderr, /Simulated lost create reply/)
        else assert.match(result.stderr, /Owned runtime cleanup failed/)

        const calls = readFileSync(log, 'utf8')
          .trim()
          .split('\n')
          .map(JSON.parse)
        if (resource === 'container') {
          const create = calls.find((args) => args[0] === 'create')
          const name = create[create.indexOf('--name') + 1]
          assert(
            calls.some((args) => args[0] === 'rm' && args.at(-1) === name),
            'should attempt removal after an uncertain create result'
          )
        }
        assert(
          calls.some((args) => args[0] === 'network' && args[1] === 'rm'),
          'should remove the owned network despite an uncertain create result'
        )
        const cleanup = JSON.parse(
          readFileSync(join(root, 'evidence/cleanup.json'), 'utf8')
        )
        const clean = ['removed', 'absent'].includes(cleanupMode)
        assert.equal(cleanup.networkRemoved, clean)
        assert.equal(cleanup.containerRemoved, resource === 'network' || clean)
        assert.equal(
          cleanup.errors.length,
          clean ? 0 : resource === 'network' ? 1 : 2,
          'cleanup must retain failures unless absence is proved'
        )
      } finally {
        rmSync(root, { recursive: true })
      }
    })
  }
}

test('runtime image input requires the qualified immutable manifest digest', () => {
  const source = readFileSync(new URL('./Dockerfile', import.meta.url), 'utf8')
  const fragment = source.match(
    /RUN (case "\$PLAYWRIGHT_IMAGE" in [\s\S]*?esac)/
  )?.[1]
  assert(fragment, 'should enforce the qualified runtime image digest')
  const command = fragment.replaceAll('\\\n', ' ')
  const qualified =
    '65cefd09a5e943921ecd3a6e5414c603db2eb161e9eb48f2e2ccc63486dc7dc0'
  for (const [reference, expected] of [
    [`registry.example.com/fixture@sha256:${qualified}`, 0],
    [`sha256:${qualified}`, 1],
    ['registry.example.com/fixture:latest', 1],
    [`registry.example.com/fixture@sha256:${'0'.repeat(64)}`, 1],
  ]) {
    const result = spawnSync('/bin/sh', ['-c', command], {
      env: { PLAYWRIGHT_IMAGE: reference },
    })
    assert.equal(result.status, expected, reference)
  }
})
