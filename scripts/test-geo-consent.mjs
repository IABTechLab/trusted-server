#!/usr/bin/env node

// Real Fastly geo hostcall with the shared case table and an owned Viceroy manifest.
// Usage: node scripts/test-geo-consent.mjs /path/to/viceroy [test-binary.wasm]
import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const repoRoot = fileURLToPath(new URL('../', import.meta.url))

// Detached POSIX children lead their own process group. Kill that group even
// when the direct child has exited but a descendant still holds its pipes.
export function runCommand(
  command,
  args,
  { timeout = 10_000, maxBuffer = 1024 * 1024, ...options } = {}
) {
  return new Promise((resolveCommand, reject) => {
    const child = spawn(command, args, {
      ...options,
      detached: true,
      stdio: ['ignore', 'pipe', 'pipe'],
    })
    const stdout = []
    const stderr = []
    let bytes = 0
    let failure
    const killGroup = () => {
      if (!child.pid) return
      try {
        process.kill(-child.pid, 'SIGKILL')
      } catch (error) {
        if (error.code !== 'ESRCH') failure ??= error
      }
    }
    const interrupt = (signal) => {
      failure = Object.assign(new Error(`Command interrupted: ${command}`), {
        code: 'ABORT_ERR',
        signal,
      })
      killGroup()
    }
    const onInterrupt = () => interrupt('SIGINT')
    const onTerminate = () => interrupt('SIGTERM')
    process.once('SIGINT', onInterrupt)
    process.once('SIGTERM', onTerminate)
    const cleanup = () => {
      clearTimeout(timer)
      process.removeListener('SIGINT', onInterrupt)
      process.removeListener('SIGTERM', onTerminate)
    }
    const timer = setTimeout(() => {
      failure = Object.assign(new Error(`Command timed out: ${command}`), {
        code: 'ETIMEDOUT',
      })
      killGroup()
    }, timeout)
    const collect = (chunks, chunk) => {
      bytes += chunk.length
      if (bytes > maxBuffer) {
        failure ??= Object.assign(
          new Error(`Command output exceeded limit: ${command}`),
          { code: 'ERR_CHILD_PROCESS_STDIO_MAXBUFFER' }
        )
        killGroup()
      } else chunks.push(chunk)
    }
    child.stdout.on('data', (chunk) => collect(stdout, chunk))
    child.stderr.on('data', (chunk) => collect(stderr, chunk))
    child.on('error', (error) => {
      cleanup()
      reject(error)
    })
    child.on('close', (code, signal) => {
      cleanup()
      const output = {
        stdout: Buffer.concat(stdout).toString(),
        stderr: Buffer.concat(stderr).toString(),
      }
      if (failure || code !== 0) {
        reject(
          Object.assign(
            failure ?? new Error(`Command failed: ${command}`),
            output,
            { code: failure?.code ?? code, signal: failure?.signal ?? signal }
          )
        )
      } else resolveCommand(output)
    })
  })
}

export async function runSuite(viceroy, suppliedBinary, execute = runCommand) {
  const runtime = viceroy.includes('/') ? resolve(viceroy) : viceroy
  const version = await execute(runtime, ['--version'], { cwd: repoRoot })
  assert.match(
    version.stdout,
    /\b0\.21\.0\b/,
    'should use pinned Viceroy 0.21.0'
  )
  const directory = await mkdtemp(join(tmpdir(), 'ts-geo-consent-'))
  try {
    const cases = JSON.parse(
      await readFile(
        new URL(
          '../crates/trusted-server-core/src/test_support/fixtures/geo-consent.json',
          import.meta.url
        ),
        'utf8'
      )
    )
    const addresses = {}
    cases.forEach((entry, index) => {
      if (entry.country === null && entry.region === null) return
      addresses[`192.0.2.${index + 1}`] = {
        as_name: 'Example network',
        as_number: 64500,
        area_code: 0,
        city: 'Example city',
        conn_speed: 'broadband',
        conn_type: 'wired',
        continent: 'EU',
        country_code: entry.country ?? '',
        country_code3: 'XXX',
        country_name: 'Example country',
        latitude: 0,
        longitude: 0,
        metro_code: 0,
        postal_code: '?',
        proxy_description: '?',
        proxy_type: 'hosting',
        region: entry.region,
        utc_offset: 0,
      }
    })
    const california = cases.findIndex((entry) => entry.name === 'us-ca')
    assert(california >= 0, 'should find US-CA forwarding fixture')
    addresses['2001:db8::7'] = addresses[`192.0.2.${california + 1}`]
    const mapping = join(directory, 'geo.json')
    await writeFile(mapping, JSON.stringify(addresses))
    const manifest = join(directory, 'fastly.toml')
    await writeFile(
      manifest,
      `[local_server.geolocation]\nformat = "json"\nuse_default_loopback = false\nfile = ${JSON.stringify(mapping)}\n`
    )
    let binary = suppliedBinary && resolve(suppliedBinary)
    if (!binary) {
      const build = await execute(
        'cargo',
        [
          'test',
          '--locked',
          '-p',
          'trusted-server-adapter-fastly',
          '--target',
          'wasm32-wasip1',
          '--bin',
          'trusted-server-adapter-fastly',
          '--no-run',
          '--message-format=json',
        ],
        { cwd: repoRoot, maxBuffer: 32 * 1024 * 1024, timeout: 900_000 }
      )
      const binaries = build.stdout
        .split('\n')
        .filter((line) => line.startsWith('{'))
        .map((line) => JSON.parse(line))
        .filter(
          (item) =>
            item.reason === 'compiler-artifact' &&
            item.profile?.test &&
            item.target?.name === 'trusted-server-adapter-fastly' &&
            item.executable
        )
      assert.equal(
        binaries.length,
        1,
        'should discover exactly one Fastly test artifact'
      )
      binary = binaries[0].executable
    }
    const result = await execute(
      runtime,
      [
        'run',
        '-C',
        manifest,
        binary,
        'identity_conformance::geo_consent_fastly',
        '--ignored',
        '--nocapture',
      ],
      { cwd: repoRoot, maxBuffer: 16 * 1024 * 1024, timeout: 180_000 }
    )
    for (const name of [
      'geo_consent_fastly',
      'geo_consent_fastly_forwarding',
    ]) {
      assert(
        result.stdout.includes(`test identity_conformance::${name} ... ok`),
        `should actually execute ${name}`
      )
    }
    return result
  } finally {
    await rm(directory, { recursive: true, force: true })
  }
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  try {
    const [viceroy, binary, ...extra] = process.argv.slice(2)
    assert(
      viceroy && !extra.length,
      'usage: node scripts/test-geo-consent.mjs /path/to/viceroy [test-binary.wasm]'
    )
    const result = await runSuite(viceroy, binary)
    process.stdout.write(result.stdout)
    process.stderr.write(result.stderr)
  } catch (error) {
    if (error.stdout) {
      for (const line of error.stdout.split('\n')) {
        let item
        // Runtime output need not be a JSON compiler diagnostic.
        try {
          item = JSON.parse(line)
        } catch {
          item = undefined
        }
        if (item?.reason === 'compiler-message' && item.message?.rendered)
          process.stderr.write(item.message.rendered)
        else if (!item) process.stderr.write(`${line}\n`)
      }
    }
    if (error.stderr) process.stderr.write(error.stderr)
    process.stderr.write(`Geo consent launcher failed: ${error.message}\n`)
    process.exitCode =
      error.signal === 'SIGINT' ? 130 : error.signal === 'SIGTERM' ? 143 : 1
  }
}
