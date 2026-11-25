import assert from 'node:assert/strict'
import { mkdtemp, readdir, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import test from 'node:test'
import { runFixture } from './proxy-policy-origin.mjs'

async function failedRun(t, source, expected, options = {}) {
  const directory = await mkdtemp(join(tmpdir(), 'proxy-runner-check-'))
  t.after(() => rm(directory, { recursive: true, force: true }))
  const executable = join(directory, 'viceroy.mjs')
  await writeFile(executable, `#!/usr/bin/env node\n${source}`, { mode: 0o700 })
  await assert.rejects(
    runFixture({
      viceroy: executable,
      binary: 'unused.wasm',
      temporaryRoot: directory,
      timeoutMs: 5_000,
      ...options,
    }),
    expected
  )
  assert.deepEqual(
    await readdir(directory),
    ['viceroy.mjs'],
    'should remove owned configuration directory'
  )
}

test('rejects a missing runtime test selection and cleans up', (t) =>
  failedRun(
    t,
    "console.log('test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured;')",
    /all three/
  ))

test('rejects a child startup error and cleans up', async (t) => {
  await failedRun(t, '', /ENOENT/, {
    viceroy: '/nonexistent/proxy-policy-viceroy',
  })
})

test('kills a stalled child and removes its configuration', (t) =>
  failedRun(t, 'setInterval(() => {}, 1000)', /watchdog/, { timeoutMs: 100 }))

test('reports origin errors and closes active origin connections', async (t) => {
  let originPort
  await failedRun(
    t,
    `
import { readFileSync } from 'node:fs'
const config = readFileSync(process.argv[process.argv.indexOf('-C') + 1], 'utf8')
const origin = config.match(/origin = "(http:[^"]+)"/)[1]
await fetch(origin + '/static/banner.jpg').catch(() => {})
`,
    /deliberate origin error/,
    {
      serve: (request) => {
        originPort = request.socket.localPort
        throw new Error('deliberate origin error')
      },
    }
  )
  assert(
    Number.isInteger(originPort),
    'should exercise a real origin connection'
  )
  await assert.rejects(
    fetch(`http://127.0.0.1:${originPort}/`),
    'should close the origin listener'
  )
})
