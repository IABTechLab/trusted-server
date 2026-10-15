const assert = require('node:assert/strict')
const { readFileSync } = require('node:fs')
const path = require('node:path')
const { test } = require('node:test')
const vm = require('node:vm')
const typescript = require('../../trusted-server-js/lib/node_modules/typescript')

function load(filename, infra, state = {}) {
  const saved = []
  const removed = []
  const fs = {
    writeFileSync: (_filename, value) => saved.push(JSON.parse(value)),
    readFileSync: () => JSON.stringify(state),
    unlinkSync: (filename) => removed.push(filename),
  }
  const source = typescript.transpileModule(
    readFileSync(path.join(__dirname, filename), 'utf8'),
    {
      compilerOptions: {
        module: typescript.ModuleKind.CommonJS,
        target: typescript.ScriptTarget.ES2022,
      },
    }
  ).outputText
  const module = { exports: {} }
  vm.runInNewContext(source, {
    module,
    exports: module.exports,
    __dirname,
    process: {
      env: {
        TEST_FRAMEWORK: 'nextjs',
        TRACE_VICEROY_CONFIG_PATH: 'trace-public-fixture.toml',
        TRACE_AUTH_VICEROY_CONFIG_PATH: 'trace-auth-fixture.toml',
      },
    },
    console: { log() {}, warn() {}, error() {} },
    require(name) {
      if (name === 'node:fs') return fs
      if (name === 'node:path') return path
      if (name === './helpers/infra.js') return infra
      throw new Error(`should explicitly mock dependency ${name}`)
    },
  })
  return { run: module.exports.default, saved, removed }
}

test('failed trace runtime setup stops every already allocated runtime and the container even if one stop fails', async () => {
  const stopped = []
  let allocations = 0
  const fixture = load('global-setup.ts', {
    startContainer: async () => 'owned-container',
    startViceroy: async () => {
      allocations += 1
      if (allocations === 3) throw new Error('fixture-auth-runtime-failed')
      return {
        process: { pid: allocations * 101 },
        baseUrl: `http://127.0.0.1:${8000 + allocations}`,
      }
    },
    stopViceroy: async (pid) => {
      stopped.push(pid)
      if (pid === 101) throw new Error('fixture-first-stop-failed')
    },
    stopContainer: (id) => stopped.push(id),
  })
  await assert.rejects(fixture.run(), /fixture-auth-runtime-failed/)
  assert.deepEqual(stopped, [101, 202, 'owned-container'])
  assert.equal(fixture.removed.length, 1)
  assert.equal(fixture.saved.at(-1).traceBaseUrl, 'http://127.0.0.1:8002')
  assert.deepEqual(fixture.saved.at(-1).extraViceroyPids, [202])
})

test('normal teardown attempts all owned resources and removes state after a runtime stop failure', async () => {
  const stopped = []
  const fixture = load(
    'global-teardown.ts',
    {
      stopViceroy: async (pid) => {
        stopped.push(pid)
        if (pid === 101) throw new Error('fixture-first-stop-failed')
      },
      stopContainer: (id) => stopped.push(id),
    },
    {
      viceroyPid: 101,
      extraViceroyPids: [202, 303],
      containerId: 'owned-container',
    }
  )
  await assert.rejects(fixture.run(), /fixture-first-stop-failed/)
  assert.deepEqual(stopped, [101, 202, 303, 'owned-container'])
  assert.equal(fixture.removed.length, 1)
})
