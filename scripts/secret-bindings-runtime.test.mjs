import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { test } from 'node:test'
import { setTimeout as delay } from 'node:timers/promises'
import { fileURLToPath } from 'node:url'

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
        return Number(await readFile(marker, 'utf8'))
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
import { writeFileSync } from "node:fs";
writeFileSync(process.env.SECRET_BINDINGS_RUNTIME_MARKER, String(process.pid));
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
      runtimePid = await waitFor(async () => {
        assert.equal(spawnError, undefined, 'driver must spawn successfully')
        assert.equal(
          exited,
          undefined,
          `driver exited before fake runtime startup:\n${output}`
        )
        return readMarker()
      }, 'fake runtime startup marker')
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
      runtimePid ??= await readMarker()
      killOwnedGroup(runtimePid)
      if (child && !exited) {
        child.kill('SIGKILL')
        await waitFor(() => exited, 'test driver termination', 2000)
      }
      await rm(scratch, { recursive: true, force: true })
    }
  }
)
