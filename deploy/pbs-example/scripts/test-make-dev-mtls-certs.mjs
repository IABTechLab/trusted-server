#!/usr/bin/env node
import assert from 'node:assert/strict'
import { execFileSync, spawnSync } from 'node:child_process'
import { mkdtempSync, readdirSync, readFileSync, rmSync, statSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

const script = fileURLToPath(new URL('./make-dev-mtls-certs.sh', import.meta.url))
const env = Object.fromEntries(
  ['PATH', 'HOME']
    .filter((key) => process.env[key] !== undefined)
    .map((key) => [key, process.env[key]])
)

function run(command, args, cwd) {
  return execFileSync(command, args, {
    cwd,
    env,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'inherit'],
  })
}

test('generates private development files and refuses to overwrite them', (t) => {
  const directory = mkdtempSync(join(tmpdir(), 'pbs-dev-mtls-'))
  t.after(() => rmSync(directory, { recursive: true, force: true }))
  const output = join(directory, 'dev certs')

  assert.match(run(script, [output], directory), /development only/i)
  const files = ['ca-bundle.pem', 'ca.key', 'ca.pem', 'client.key', 'client.pem']
  assert.deepEqual(readdirSync(output).sort(), files)
  const contents = files.map((file) => {
    const path = join(output, file)
    assert.equal(statSync(path).mode & 0o777, 0o600, file)
    return readFileSync(path, 'utf8')
  })
  assert.match(
    run(
      'openssl',
      ['verify', '-CAfile', 'ca.pem', '-purpose', 'sslclient', 'client.pem'],
      output
    ),
    /^client\.pem: OK\s*$/
  )
  const [bundle, , ca] = contents
  assert.equal(bundle, ca)
  assert.doesNotMatch(
    bundle.replace(/\n$/, ''),
    /^[ \t]*$/m,
    'bundle must not have blank lines'
  )
  const caExtensions = run(
    'openssl',
    ['x509', '-in', 'ca.pem', '-noout', '-ext', 'basicConstraints,keyUsage'],
    output
  )
  assert.match(caExtensions, /Basic Constraints: critical\s+CA:TRUE/)
  assert.match(caExtensions, /Key Usage: critical\s+Certificate Sign, CRL Sign/)
  const clientExtensions = run(
    'openssl',
    ['x509', '-in', 'client.pem', '-noout', '-ext', 'extendedKeyUsage'],
    output
  )
  assert.match(clientExtensions, /TLS Web Client Authentication/)

  for (const name of ['ca', 'client']) {
    assert.equal(
      run('openssl', ['pkey', '-in', `${name}.key`, '-pubout'], output),
      run('openssl', ['x509', '-in', `${name}.pem`, '-noout', '-pubkey'], output)
    )
  }

  const rerun = spawnSync(script, [output], {
    cwd: directory,
    env,
    encoding: 'utf8',
  })
  assert.equal(rerun.status, 1, rerun.stderr)
  assert.match(rerun.stderr, /non-empty directory/)
  for (const [index, file] of files.entries()) {
    assert(readFileSync(join(output, file), 'utf8') === contents[index], file)
  }
})
