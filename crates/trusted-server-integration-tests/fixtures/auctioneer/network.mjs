#!/usr/bin/env node
// Host coordinator. All product, origin and browser traffic stays in one worker.
import { createHash, randomUUID } from 'node:crypto'
import { execFile, spawn } from 'node:child_process'
import { promisify } from 'node:util'
import {
  constants,
  accessSync,
  mkdirSync,
  readFileSync,
  realpathSync,
  statSync,
  writeFileSync,
} from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { parseArgs } from 'node:util'
import { fileURLToPath } from 'node:url'

const execute = promisify(execFile)
export function digest(file) {
  return createHash('sha256').update(readFileSync(file)).digest('hex')
}
export function verifyTopology(container, network, name, expectedMounts) {
  if (!network.Internal || network.Name !== name || network.Driver !== 'bridge')
    throw Error('Expected the owned internal bridge network')
  if (
    !container.HostConfig.ReadonlyRootfs ||
    container.HostConfig.NetworkMode !== name ||
    container.HostConfig.Privileged ||
    container.HostConfig.PublishAllPorts ||
    Object.keys(container.HostConfig.PortBindings ?? {}).length ||
    Object.keys(container.NetworkSettings.Networks).join() !== name
  )
    throw Error('Worker isolation differs from the declared topology')
  const mounts = container.Mounts.filter((mount) => mount.Type !== 'tmpfs')
  if (mounts.length !== expectedMounts.length)
    throw Error('Unexpected runtime mount')
  for (const expected of expectedMounts) {
    const actual = mounts.find(
      (mount) => mount.Destination === expected.destination
    )
    if (
      !actual ||
      actual.Type !== 'bind' ||
      actual.Source !== expected.source ||
      actual.RW !== expected.write
    )
      throw Error(`Unexpected mount at ${expected.destination}`)
  }
  if (container.HostConfig.Binds?.length)
    throw Error('Unexpected legacy runtime binds')
}
export function validateArtifacts(input, root) {
  if (
    !/^[a-f0-9]{40}$/.test(input.productCommit) ||
    input.variant !== 'default' ||
    input.profile !== 'release' ||
    input.testUtils !== false ||
    input.producer !== 'scripts/release/phase1-build.sh'
  )
    throw Error('Require the non-test default release producer identity')
  const records = []
  for (const [name, field] of [
    ['guest', 'wasm'],
    ['cli', 'cli'],
  ]) {
    const record = input[field]
    const source = realpathSync(resolve(root, record.path))
    if (
      !statSync(source).isFile() ||
      !/^[a-f0-9]{64}$/.test(record.sha256) ||
      digest(source) !== record.sha256
    )
      throw Error(`Supplied ${name} digest mismatch`)
    if (record.sourceCommit !== input.productCommit)
      throw Error(`Supplied ${name} source mismatch`)
    records.push({ name, source, sha256: record.sha256 })
  }
  const magic = readFileSync(records[0].source).subarray(0, 8)
  if (!magic.equals(Buffer.from([0, 97, 115, 109, 1, 0, 0, 0])))
    throw Error('Expected a WASI wasm input')
  const elf = readFileSync(records[1].source).subarray(0, 20)
  if (
    elf.length !== 20 ||
    elf[0] !== 127 ||
    elf.subarray(1, 4).toString() !== 'ELF' ||
    elf[4] !== 2 ||
    elf[5] !== 1 ||
    elf.readUInt16LE(18) !== 62
  )
    throw Error('Expected the matching Linux x86_64 ELF CLI')
  accessSync(records[1].source, constants.X_OK)
  return records
}
async function docker(args) {
  const result = await execute('docker', args, {
    encoding: 'utf8',
    maxBuffer: 8 * 1024 * 1024,
  })
  return result.stdout.trim()
}

async function main() {
  const { values } = parseArgs({
    options: {
      image: { type: 'string' },
      artifacts: { type: 'string' },
      out: { type: 'string' },
    },
    strict: true,
  })
  if (
    !/^sha256:[a-f0-9]{64}$/.test(values.image ?? '') ||
    !values.artifacts ||
    !values.out
  )
    throw Error(
      'Usage: network.mjs --image sha256:<prepared-image> --artifacts <producer-inputs.json> --out <fresh-directory>'
    )
  const identityPath = realpathSync(values.artifacts)
  const input = JSON.parse(readFileSync(identityPath, 'utf8'))
  const records = validateArtifacts(input, dirname(identityPath))
  const out = resolve(values.out)
  mkdirSync(out, { mode: 0o700 })
  for (const name of ['work', 'results']) {
    mkdirSync(join(out, name), { mode: 0o700 })
  }
  const image = JSON.parse(await docker(['image', 'inspect', values.image]))[0]
  if (
    image.Os !== 'linux' ||
    image.Architecture !== 'amd64' ||
    !/^[a-f0-9]{40}$/.test(
      image.Config.Labels?.['org.opencontainers.image.revision'] ?? ''
    )
  )
    throw Error(
      'Expected a prepared Linuxamd64 worker with frozen harness revision'
    )
  const prefix = `ts-production-${randomUUID()}`
  const network = `${prefix}-net`
  const name = `${prefix}-worker`
  const mounts = [
    {
      source: records[0].source,
      destination: '/inputs/guest.wasm',
      write: false,
    },
    { source: records[1].source, destination: '/inputs/ts', write: false },
    {
      source: identityPath,
      destination: '/inputs/manifest.json',
      write: false,
    },
    { source: join(out, 'work'), destination: '/work', write: true },
    { source: join(out, 'results'), destination: '/results', write: true },
  ]
  let networkCreated = false
  let containerCreated = false
  let cancelled = false
  const interrupt = () => {
    cancelled = true
    if (containerCreated)
      void docker(['stop', '--time', '15', name]).catch(() => {})
  }
  process.on('SIGINT', interrupt)
  process.on('SIGTERM', interrupt)
  try {
    await docker([
      'network',
      'create',
      '--internal',
      '--label',
      'trusted-server.fixture=production-pbs',
      network,
    ])
    networkCreated = true
    const args = [
      'create',
      '--name',
      name,
      '--platform',
      'linux/amd64',
      '--network',
      network,
      '--read-only',
      '--cap-drop',
      'ALL',
      '--security-opt',
      'no-new-privileges',
      '--pids-limit',
      '256',
      '--memory',
      '3g',
      '--shm-size',
      '512m',
      '--tmpfs',
      '/tmp:rw,nosuid,size=512m',
      '--user',
      `${process.getuid()}:${process.getgid()}`,
      '--env',
      'PRODUCTION_AUCTION_WORKER=pbs',
    ]
    for (const host of [
      'pbs.example.com',
      'assets.example.com',
      'creative.example.com',
    ])
      args.push('--add-host', `${host}:127.0.0.1`)
    for (const mount of mounts) {
      if (mount.source.includes(','))
        throw Error('Docker bind source cannot contain a comma')
      args.push(
        '--mount',
        `type=bind,src=${mount.source},dst=${mount.destination}${mount.write ? '' : ',readonly'}`
      )
    }
    args.push(values.image)
    await docker(args)
    containerCreated = true
    const container = JSON.parse(await docker(['inspect', name]))[0]
    const topology = JSON.parse(
      await docker(['network', 'inspect', network])
    )[0]
    verifyTopology(container, topology, network, mounts)
    writeFileSync(
      join(out, 'topology.json'),
      JSON.stringify(
        { image, container, network: topology, product: input },
        null,
        2
      )
    )
    if (cancelled) throw Error('Production run cancelled')
    const child = spawn('docker', ['start', '--attach', name], {
      stdio: ['ignore', 'pipe', 'pipe'],
    })
    const output = []
    child.stdout.on('data', (chunk) => {
      output.push(chunk)
      process.stdout.write(chunk)
    })
    child.stderr.on('data', (chunk) => {
      output.push(chunk)
      process.stderr.write(chunk)
    })
    const timer = setTimeout(interrupt, 10 * 60 * 1000)
    let code
    try {
      code = await new Promise((resolve, reject) => {
        child.once('error', reject)
        child.once('exit', resolve)
      })
    } finally {
      clearTimeout(timer)
      writeFileSync(join(out, 'worker.log'), Buffer.concat(output))
    }
    const final = JSON.parse(await docker(['inspect', name]))[0]
    verifyTopology(
      final,
      JSON.parse(await docker(['network', 'inspect', network]))[0],
      network,
      mounts
    )
    writeFileSync(
      join(out, 'final-container.json'),
      JSON.stringify(final, null, 2)
    )
    for (const record of records)
      if (digest(record.source) !== record.sha256)
        throw Error(`Supplied ${record.name} changed during execution`)
    if (
      cancelled ||
      code !== 0 ||
      final.State.ExitCode !== 0 ||
      final.State.OOMKilled
    )
      throw Error(
        `Production worker failed: attached=${code}, container=${final.State.ExitCode}, OOM=${final.State.OOMKilled}`
      )
    const worker = JSON.parse(
      readFileSync(join(out, 'results', 'worker.json'), 'utf8')
    )
    writeFileSync(
      join(out, 'evidence.json'),
      JSON.stringify(
        {
          mode: 'production-pbs-baseline',
          auctioneerAcceptance: false,
          harnessCommit:
            image.Config.Labels['org.opencontainers.image.revision'],
          image: values.image,
          product: input,
          worker,
          records,
        },
        null,
        2
      )
    )
  } finally {
    const cleanupErrors = []
    try {
      if (containerCreated) {
        try {
          await docker(['rm', '--force', name])
        } catch (error) {
          cleanupErrors.push(error)
        }
      }
      if (networkCreated) {
        try {
          await docker(['network', 'rm', network])
        } catch (error) {
          cleanupErrors.push(error)
        }
      }
    } finally {
      process.removeListener('SIGINT', interrupt)
      process.removeListener('SIGTERM', interrupt)
    }
    writeFileSync(
      join(out, 'cleanup.json'),
      JSON.stringify(
        {
          containerRemoved: !containerCreated || !cleanupErrors.length,
          errors: cleanupErrors.map((error) => error.message),
        },
        null,
        2
      )
    )
    if (cleanupErrors.length)
      throw new AggregateError(cleanupErrors, 'Owned runtime cleanup failed')
  }
}
if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
)
  await main()
