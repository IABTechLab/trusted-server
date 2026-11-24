import assert from 'node:assert/strict'
import { access, mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { setTimeout as delay } from 'node:timers/promises'
import { fileURLToPath } from 'node:url'
import test from 'node:test'
import { runCommand, runSuite } from './test-geo-consent.mjs'

const repoRoot = fileURLToPath(new URL('../', import.meta.url))
const pinned = (
  await readFile(new URL('../.tool-versions', import.meta.url), 'utf8')
).match(/^viceroy\s+(\S+)\s*$/m)[1]
const passing = ['geo_consent_fastly', 'geo_consent_fastly_forwarding']
  .map((name) => `test identity_conformance::${name} ... ok`)
  .join('\n')

function fixtureRunner(output = passing) {
  const calls = []
  return {
    calls,
    execute: async (command, args, options) => {
      calls.push({ command, args, options })
      if (args[0] === '--version')
        return { stdout: `viceroy ${pinned}\n`, stderr: '' }
      if (command === 'cargo')
        return {
          stdout: JSON.stringify({
            reason: 'compiler-artifact',
            profile: { test: true },
            target: { name: 'trusted-server-adapter-fastly' },
            executable: '/example/test.wasm',
          }),
          stderr: '',
        }
      return { stdout: output, stderr: '' }
    },
  }
}

test('build and runtime commands use the script repository root', async () => {
  const fixture = fixtureRunner()
  await runSuite('viceroy', undefined, fixture.execute)
  assert.equal(fixture.calls.length, 3)
  assert.ok(fixture.calls.every((call) => call.options.cwd === repoRoot))
  assert.ok(fixture.calls[1].args.includes('--locked'))
  assert.ok(fixture.calls[2].args.includes('/example/test.wasm'))
})

test('zero-test success and missing forwarding coverage are rejected', async () => {
  for (const output of [
    '',
    'test identity_conformance::geo_consent_fastly ... ok',
  ]) {
    await assert.rejects(
      runSuite('viceroy', '/example/test.wasm', fixtureRunner(output).execute),
      /should actually execute/
    )
  }
})

test('a mismatched Viceroy version is rejected before building', async () => {
  await assert.rejects(
    runSuite('viceroy', undefined, async () => ({
      stdout: 'viceroy 0.17.0',
      stderr: '',
    })),
    (error) => error.message.includes(`pinned Viceroy ${pinned}`)
  )
})

test('spawn failures reject cleanly', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'geo-launcher-missing-'))
  try {
    await assert.rejects(
      runCommand(join(directory, 'missing'), [], { cwd: repoRoot }),
      { code: 'ENOENT' }
    )
  } finally {
    await rm(directory, { recursive: true, force: true })
  }
})

test('handled interruptions terminate the owned command group', async () => {
  for (const signal of ['SIGINT', 'SIGTERM']) {
    const script = `process.kill(process.ppid, '${signal}');setInterval(()=>{},1000)`
    await assert.rejects(
      runCommand(process.execPath, ['-e', script], {
        cwd: repoRoot,
        timeout: 2000,
      }),
      { code: 'ABORT_ERR', signal }
    )
  }
})

for (const parentLives of [true, false]) {
  test(`timeout kills descendants with ${parentLives ? 'a living' : 'an exited'} parent`, async () => {
    const directory = await mkdtemp(join(tmpdir(), 'geo-launcher-timeout-'))
    const ready = join(directory, 'ready')
    const survived = join(directory, 'survived')
    const descendant = `const fs=require('node:fs');fs.writeFileSync(process.argv[1],'ready');setTimeout(()=>fs.writeFileSync(process.argv[2],'survived'),3500)`
    const parent = `const child=require('node:child_process').spawn(process.execPath,['-e',${JSON.stringify(descendant)},process.argv[1],process.argv[2]],{stdio:'inherit'});${parentLives ? 'setInterval(()=>{},1000)' : 'child.unref()'}`
    try {
      await assert.rejects(
        runCommand(process.execPath, ['-e', parent, ready, survived], {
          cwd: repoRoot,
          timeout: 2000,
        }),
        { code: 'ETIMEDOUT' }
      )
      assert.equal(
        await readFile(ready, 'utf8'),
        'ready',
        'should start the descendant before timeout'
      )
      await delay(3700)
      await assert.rejects(readFile(survived), { code: 'ENOENT' })
    } finally {
      await rm(directory, { recursive: true, force: true })
    }
  })
}

test('temporary runtime directory is removed after success and failure', async () => {
  for (const fails of [false, true]) {
    const fixture = fixtureRunner()
    let manifest
    const execute = async (command, args, options) => {
      if (args[0] === 'run') {
        manifest = args[2]
        await access(manifest)
        if (fails) throw new Error('fixture runtime failure')
      }
      return fixture.execute(command, args, options)
    }
    const run = runSuite('viceroy', '/example/test.wasm', execute)
    if (fails) await assert.rejects(run, /fixture runtime failure/)
    else await run
    await assert.rejects(access(dirname(manifest)), { code: 'ENOENT' })
  }
})
