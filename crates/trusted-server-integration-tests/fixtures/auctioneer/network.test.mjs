import { test } from 'node:test'
import assert from 'node:assert/strict'
import { chmodSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
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
