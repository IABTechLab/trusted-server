import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { createHash, createHmac } from 'node:crypto'
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { test } from 'node:test'
import { setTimeout as delay } from 'node:timers/promises'
import { fileURLToPath } from 'node:url'

import { verifyS3 } from './secret-bindings-runtime.mjs'

const runner = fileURLToPath(
  new URL('./secret-bindings-runtime.mjs', import.meta.url)
)

async function waitFor(check, description, timeout = 15000) {
  const deadline = Date.now() + timeout
  do {
    const result = await check()
    if (result) return result
    await delay(25)
  } while (Date.now() < deadline)
  assert.fail(`Timed out waiting for ${description}`)
}

function signedEvent(credentials, method) {
  const hash = (value) => createHash('sha256').update(value).digest('hex')
  const hmac = (key, value) => createHmac('sha256', key).update(value).digest()
  const headers = {
    host: 'localhost:12342',
    'x-amz-content-sha256': 'UNSIGNED-PAYLOAD',
    'x-amz-date': '20261102T120000Z',
    'x-amz-security-token': credentials.session,
  }
  const signed = Object.keys(headers).sort().join(';')
  const canonical = [
    method,
    '/sink-control/success.svg',
    '',
    signed
      .split(';')
      .map((name) => `${name}:${headers[name]}\n`)
      .join(''),
    signed,
    'UNSIGNED-PAYLOAD',
  ].join('\n')
  const scope = '20261102/us-east-1/s3/aws4_request'
  const toSign = [
    'AWS4-HMAC-SHA256',
    headers['x-amz-date'],
    scope,
    hash(canonical),
  ].join('\n')
  const key = hmac(
    hmac(
      hmac(hmac(`AWS4${credentials.secret}`, '20261102'), 'us-east-1'),
      's3'
    ),
    'aws4_request'
  )
  headers.authorization = `AWS4-HMAC-SHA256 Credential=${credentials.access_id}/${scope}, SignedHeaders=${signed}, Signature=${hmac(key, toSign).toString('hex')}`
  return { headers, method, url: '/sink-control/success.svg' }
}

test('S3 verification rejects every credential from the wrong route', () => {
  const first = {
    access_id: 'fictional-route-a-access',
    secret: 'fictional-route-a-secret',
    session: 'fictional-route-a-session',
  }
  const expected = {
    access_id: 'fictional-route-b-access',
    secret: 'fictional-route-b-secret',
    session: 'fictional-route-b-session',
  }
  for (const method of ['GET', 'HEAD']) {
    verifyS3(signedEvent(expected, method), expected)
    assert.throws(() => verifyS3(signedEvent(first, method), expected))
    for (const name of Object.keys(expected)) {
      const wrong = { ...expected, [name]: first[name] }
      assert.throws(
        () => verifyS3(signedEvent(wrong, method), expected),
        /expected route|signature does not match/,
        `${method} must reject another route's ${name}`
      )
    }
  }
})

function groupExists(pid) {
  try {
    process.kill(-pid, 0)
    return true
  } catch (error) {
    if (error.code === 'ESRCH') return false
    throw error
  }
}

function killOwnedGroup(pid) {
  if (!pid) return
  try {
    process.kill(-pid, 'SIGKILL')
  } catch (error) {
    if (error.code !== 'ESRCH') throw error
  }
}

test(
  'SIGTERM during runtime readiness stops the owned process group',
  {
    timeout: 30000,
    skip: process.platform === 'win32' && 'requires POSIX process groups',
  },
  async () => {
    const scratch = await mkdtemp(
      join(tmpdir(), 'secret-bindings-startup-test-')
    )
    const marker = join(scratch, 'runtime.pid')
    const fixture = join(scratch, 'fake-fixture.mjs')
    const runtime = join(scratch, 'fake-runtime.mjs')
    let child, runtimePid, exited, spawnError
    let output = ''
    const readMarker = async () => {
      try {
        return JSON.parse(await readFile(marker, 'utf8'))
      } catch (error) {
        if (error.code === 'ENOENT') return undefined
        throw error
      }
    }
    try {
      // This case stops before app readiness, so no bindings or WASM are needed.
      await writeFile(
        fixture,
        `#!/usr/bin/env node\nconsole.log(JSON.stringify({ bindings: [], backends: [], cases: { original: "{}" } }));\n`,
        { mode: 0o700 }
      )
      await writeFile(
        runtime,
        `#!/usr/bin/env node
import { spawn } from "node:child_process";
import { writeFileSync } from "node:fs";
const grandchild = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { stdio: 'ignore' });
// Reap the grandchild after a group signal. Do not kill it on a child-only signal.
process.on('SIGTERM', () => grandchild.once('exit', () => process.exit(0)));
grandchild.once('spawn', () => {
  writeFileSync(process.env.SECRET_BINDINGS_RUNTIME_MARKER, JSON.stringify({ runtimePid: process.pid, grandchildPid: grandchild.pid }));
});
// Deliberately never listen. The driver must still own us while ready() waits.
setInterval(() => {}, 1000);
`,
        { mode: 0o700 }
      )
      child = spawn(process.execPath, [runner, 'fastly'], {
        env: {
          ...process.env,
          TMPDIR: scratch,
          SECRET_BINDINGS_FIXTURE_BIN: fixture,
          SECRET_BINDINGS_ARTIFACT: join(scratch, 'unused.wasm'),
          SECRET_BINDINGS_RUNTIME_MARKER: marker,
          VICEROY_BIN: runtime,
        },
        stdio: ['ignore', 'pipe', 'pipe'],
      })
      child.stdout.on('data', (chunk) => {
        output += chunk
      })
      child.stderr.on('data', (chunk) => {
        output += chunk
      })
      child.on('error', (error) => {
        spawnError = error
      })
      child.on('exit', (code, signal) => {
        exited = { code, signal }
      })
      const markerData = await waitFor(async () => {
        assert.equal(spawnError, undefined, 'driver must spawn successfully')
        assert.equal(
          exited,
          undefined,
          `driver exited before fake runtime startup:\n${output}`
        )
        return readMarker()
      }, 'fake runtime startup marker')
      runtimePid = markerData.runtimePid
      assert.ok(
        Number.isSafeInteger(markerData.grandchildPid) &&
          markerData.grandchildPid > 1,
        'marker must identify an owned long-lived grandchild PID'
      )
      assert.notEqual(markerData.grandchildPid, runtimePid)
      process.kill(markerData.grandchildPid, 0)
      assert.ok(
        Number.isSafeInteger(runtimePid) && runtimePid > 1,
        'marker must identify an owned child PID'
      )
      assert.notEqual(runtimePid, child.pid)
      assert.ok(
        groupExists(runtimePid),
        'fake runtime must start in its own live process group'
      )

      assert.ok(
        child.kill('SIGTERM'),
        'should signal driver while runtime is not ready'
      )
      await waitFor(() => exited, 'driver SIGTERM cleanup', 8000)
      assert.deepEqual(
        exited,
        { code: 130, signal: null },
        `driver should exit through signal cleanup:\n${output}`
      )
      await waitFor(
        () => !groupExists(runtimePid),
        'owned runtime group to disappear',
        2000
      )
    } finally {
      // Also clean the regression's leaked group when running against broken code.
      runtimePid ??= (await readMarker())?.runtimePid
      killOwnedGroup(runtimePid)
      if (child && !exited) {
        child.kill('SIGKILL')
        await waitFor(() => exited, 'test driver termination', 2000)
      }
      await rm(scratch, { recursive: true, force: true })
    }
  }
)
