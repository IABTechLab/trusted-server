#!/usr/bin/env node
// Exercise production HTTP clients against delayed local origins in worker runtimes.
import assert from 'node:assert/strict'
import { execFile, spawn } from 'node:child_process'
import { open, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import http from 'node:http'
import net from 'node:net'
import os from 'node:os'
import path from 'node:path'
import { setTimeout as delay } from 'node:timers/promises'
import { fileURLToPath } from 'node:url'
import { parseArgs, promisify } from 'node:util'

const { values, positionals } = parseArgs({
  allowPositionals: true,
  options: {
    help: { type: 'boolean' },
    runtime: { type: 'string' },
    'wasm-bindgen': { type: 'string', default: 'wasm-bindgen' },
    'target-dir': { type: 'string' },
  },
})
if (values.help) {
  console.log(
    'Usage: node scripts/test-worker-auction-deadlines.mjs <cloudflare|spin> --runtime <executable> [--wasm-bindgen <executable>] [--target-dir <directory>]'
  )
  process.exit(0)
}
const [adapter] = positionals
assert(
  positionals.length === 1 && ['cloudflare', 'spin'].includes(adapter),
  'Supply cloudflare or spin as the adapter'
)
assert(
  values.runtime,
  'Supply --runtime with the local worker runtime executable'
)
const interrupted = new AbortController()
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const artifact = path.resolve(
  values['target-dir'] ?? path.join(root, 'target'),
  adapter === 'cloudflare' ? 'wasm32-unknown-unknown' : 'wasm32-wasip1',
  'debug/examples/deadline_runtime.wasm'
)

async function listen(server) {
  await new Promise((resolve, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', resolve)
  })
  return server.address().port
}

async function closeServer(server) {
  server.closeAllConnections?.()
  await new Promise((resolve) => server.close(resolve))
}

async function availablePort() {
  const server = net.createServer()
  const port = await listen(server)
  await closeServer(server)
  return port
}

function connects(port) {
  return new Promise((resolve) => {
    const socket = net.createConnection({ host: '127.0.0.1', port })
    const done = (result) => {
      socket.destroy()
      resolve(result)
    }
    socket.setTimeout(100)
    socket.once('connect', () => done(true))
    socket.once('error', () => done(false))
    socket.once('timeout', () => done(false))
  })
}

function get(uri) {
  return new Promise((resolve, reject) => {
    const request = http.get(
      uri,
      { signal: interrupted.signal },
      (response) => {
        const chunks = []
        response.on('data', (chunk) => chunks.push(chunk))
        response.on('error', reject)
        response.on('end', () => {
          if (response.statusCode !== 200) {
            reject(
              new Error(
                `Fixture returned ${response.statusCode}: ${Buffer.concat(chunks)}`
              )
            )
          } else {
            resolve(Buffer.concat(chunks).toString())
          }
        })
      }
    )
    request.on('error', reject)
    request.setTimeout(5000, () =>
      request.destroy(new Error('Fixture request timed out'))
    )
  })
}

const observed = new Set()
const origin = http.createServer(async (request, response) => {
  const cancellation = new AbortController()
  response.once('close', () => cancellation.abort())
  response.on('error', () => {})
  observed.add(request.url)
  try {
    if (request.url === '/headers') {
      await delay(700, undefined, { signal: cancellation.signal })
    }
    if (request.url === '/large') {
      const body = Buffer.alloc(8 * 1024 * 1024 + 1)
      response.writeHead(200, { 'Content-Length': String(body.length) })
      response.end(body)
      return
    }
    response.writeHead(200, { 'Content-Length': '4' })
    if (request.url === '/body') {
      response.write('a')
      await delay(700, undefined, { signal: cancellation.signal })
      response.end('bcd')
    } else {
      response.end('okay')
    }
  } catch (error) {
    if (!cancellation.signal.aborted) response.destroy(error)
  }
})

let workspace
let log
let child
let childStopped
let childError
const grouped = process.platform !== 'win32'
function signalRuntime(signal) {
  if (!child?.pid) return
  try {
    if (grouped) process.kill(-child.pid, signal)
    else child.kill(signal)
  } catch (error) {
    if (error.code !== 'ESRCH') throw error
  }
}

const interrupt = (signal) => {
  interrupted.abort(new Error(`Harness interrupted by ${signal}`))
  signalRuntime('SIGTERM')
}
process.on('SIGINT', interrupt)
process.on('SIGTERM', interrupt)

try {
  const originPort = await listen(origin)
  const port = await availablePort()
  workspace = await mkdtemp(path.join(os.tmpdir(), 'ts-worker-deadline-'))
  let argumentsForRuntime
  if (adapter === 'cloudflare') {
    await promisify(execFile)(
      values['wasm-bindgen'],
      [
        artifact,
        '--target',
        'web',
        '--out-dir',
        workspace,
        '--out-name',
        'fixture',
      ],
      { timeout: 60_000, signal: interrupted.signal }
    )
    await writeFile(
      path.join(workspace, 'index.js'),
      'import module from "./fixture_bg.wasm";\nimport {initSync, fetch} from "./fixture.js";\ninitSync({module});\nexport default {fetch};\n'
    )
    const configuration = path.join(workspace, 'wrangler.toml')
    await writeFile(
      configuration,
      'name = "deadline-fixture"\nmain = "index.js"\ncompatibility_date = "2026-04-01"\n'
    )
    argumentsForRuntime = [
      'dev',
      '--local',
      '--port',
      String(port),
      '--ip',
      '127.0.0.1',
      '--config',
      configuration,
    ]
  } else {
    const configuration = path.join(workspace, 'spin.toml')
    await writeFile(
      configuration,
      `spin_manifest_version = 2
[application]
name = "deadline-fixture"
version = "0.1.0"
[[trigger.http]]
route = "/..."
component = "fixture"
[component.fixture]
source = ${JSON.stringify(artifact)}
allowed_outbound_hosts = ["http://127.0.0.1:*"]
`
    )
    argumentsForRuntime = [
      'up',
      '--from',
      configuration,
      '--listen',
      `127.0.0.1:${port}`,
    ]
  }
  log = await open(path.join(workspace, 'runtime.log'), 'w')
  child = spawn(values.runtime, argumentsForRuntime, {
    cwd: workspace,
    stdio: ['ignore', log.fd, log.fd],
    detached: grouped,
  })
  childStopped = new Promise((resolve) => {
    child.once('exit', resolve)
    child.once('error', (error) => {
      childError = error
      resolve()
    })
  })
  let ready = false
  for (let attempt = 0; attempt < 300; attempt++) {
    interrupted.signal.throwIfAborted()
    if (childError) throw childError
    assert(
      child.exitCode === null && child.signalCode === null,
      'Worker exited during startup'
    )
    if (await connects(port)) {
      ready = true
      break
    }
    await delay(100, undefined, { signal: interrupted.signal })
  }
  assert(ready, 'Worker did not listen')
  const cases = [
    ['/fast', 80, 200, 'complete', 500],
    ['/headers', 80, 0, 'timeout', 350],
    ['/body', 80, 0, 'timeout', 350],
    ['/headers', 80, 200, 'timeout', 500],
  ]
  if (adapter === 'spin') cases.push(['/large', 2000, 0, 'oversized', 2500])
  assert.equal(
    cases.length,
    adapter === 'spin' ? 5 : 4,
    'should exercise every runtime case'
  )
  for (const [endpoint, budget, originDelay, expected, maximum] of cases) {
    observed.delete(endpoint)
    const uri = `http://127.0.0.1:${port}/?upstream=http://127.0.0.1:${originPort}${endpoint}&budget_ms=${budget}&origin_ms=${originDelay}`
    const result = await get(uri)
    const [status, elapsed] = result.split(':')
    assert.equal(
      status,
      expected,
      `${endpoint} should return ${expected}: ${result}`
    )
    assert(
      Number.isFinite(Number(elapsed)) && Number(elapsed) < maximum,
      `${endpoint} should complete within ${maximum} ms: ${result}`
    )
    assert(
      observed.has(endpoint),
      `${endpoint} request should start at dispatch`
    )
    console.log(
      `PASS ${adapter} ${endpoint} budget=${budget} origin=${originDelay}: ${result}`
    )
  }
} catch (error) {
  if (log)
    console.error(await readFile(path.join(workspace, 'runtime.log'), 'utf8'))
  throw error
} finally {
  try {
    if (child?.pid) {
      signalRuntime('SIGTERM')
      const timeout = new AbortController()
      await Promise.race([
        childStopped,
        delay(10_000, undefined, { signal: timeout.signal }),
      ])
      timeout.abort()
      signalRuntime('SIGKILL')
      await childStopped
    }
  } finally {
    await log?.close()
    if (workspace) await rm(workspace, { recursive: true, force: true })
    await closeServer(origin)
    process.off('SIGINT', interrupt)
    process.off('SIGTERM', interrupt)
  }
}
