// Real Fastly geo hostcall with the shared case table and an owned Viceroy manifest.
// Usage: node scripts/test-geo-consent.mjs /path/to/viceroy [test-binary.wasm]
import assert from 'node:assert/strict'
import { execFile } from 'node:child_process'
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { promisify } from 'node:util'

const execute = promisify(execFile)
const [viceroy, suppliedBinary, ...extra] = process.argv.slice(2)
assert(
  viceroy && !extra.length,
  'usage: node scripts/test-geo-consent.mjs /path/to/viceroy [test-binary.wasm]'
)
const version = await execute(viceroy, ['--version'])
assert.match(version.stdout, /\b0\.21\.0\b/, 'should use pinned Viceroy 0.21.0')
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
  addresses['2001:db8::7'] = addresses['192.0.2.3']
  const mapping = join(directory, 'geo.json')
  await writeFile(mapping, JSON.stringify(addresses))
  const manifest = join(directory, 'fastly.toml')
  await writeFile(
    manifest,
    `[local_server.geolocation]\nformat = "json"\nuse_default_loopback = false\nfile = ${JSON.stringify(mapping)}\n`
  )
  let binary = suppliedBinary
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
      { maxBuffer: 32 * 1024 * 1024, timeout: 900_000 }
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
    viceroy,
    [
      'run',
      '-C',
      manifest,
      binary,
      'identity_conformance::geo_consent_fastly',
      '--ignored',
      '--nocapture',
    ],
    { maxBuffer: 16 * 1024 * 1024, timeout: 180_000 }
  )
  process.stdout.write(result.stdout)
  process.stderr.write(result.stderr)
  for (const name of ['geo_consent_fastly', 'geo_consent_fastly_forwarding']) {
    assert(
      result.stdout.includes(`test identity_conformance::${name} ... ok`),
      `should actually execute ${name}`
    )
  }
} catch (error) {
  if (error.stdout) {
    for (const line of error.stdout.split('\n')) {
      if (line.startsWith('{')) {
        const item = JSON.parse(line)
        if (item.reason === 'compiler-message' && item.message?.rendered)
          process.stderr.write(item.message.rendered)
      } else process.stderr.write(`${line}\n`)
    }
  }
  if (error.stderr) process.stderr.write(error.stderr)
  process.stderr.write(
    `Geo consent launcher failed: ${error.code ?? error.message}\n`
  )
  process.exitCode = 1
} finally {
  await rm(directory, { recursive: true, force: true })
}
