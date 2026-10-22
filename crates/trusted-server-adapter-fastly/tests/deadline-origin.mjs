// Run the actual Fastly deadline tests against a deliberately slow local origin.
// Usage: node tests/deadline-origin.mjs /path/to/viceroy [test-binary.wasm]
import assert from 'node:assert/strict'
import { execFile, spawn } from 'node:child_process'
import { once } from 'node:events'
import { existsSync } from 'node:fs'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { setTimeout as delay } from 'node:timers/promises'
import { promisify } from 'node:util'

const execute = promisify(execFile)
const interruption = new AbortController()
process.once('SIGINT', () => interruption.abort())
process.once('SIGTERM', () => interruption.abort())

async function testArtifact() {
  const { stdout } = await execute(
    'cargo',
    [
      'test',
      '-p',
      'trusted-server-adapter-fastly',
      '--target',
      'wasm32-wasip1',
      '--bin',
      'trusted-server-adapter-fastly',
      '--no-run',
      '--message-format=json',
    ],
    {
      maxBuffer: 16 * 1024 * 1024,
      signal: interruption.signal,
      killSignal: 'SIGKILL',
    }
  )
  const binaries = stdout
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
    .map((item) => item.executable)
  assert.equal(
    binaries.length,
    1,
    'should discover exactly one Fastly test binary'
  )
  return binaries[0]
}

async function serveOrigin(request, response) {
  const cancellation = new AbortController()
  response.once('close', () => cancellation.abort())
  const pause = (milliseconds) =>
    delay(milliseconds, undefined, { signal: cancellation.signal })
  try {
    if (request.method === 'HEAD') {
      response.writeHead(200, { 'Content-Length': 20 * 1024 * 1024 })
      response.end()
      return
    }
    if (request.url === '/stalled') await pause(2000)
    if (request.url === '/trickle') {
      response.writeHead(200, { 'Content-Length': 100 })
      for (let index = 0; index < 100; index += 1) {
        response.write('x')
        await pause(10)
      }
      response.end()
      return
    }
    await pause(30)
    response.writeHead(200, { 'Content-Length': 4 })
    response.end('fast')
  } catch (error) {
    // Deadline expiry deliberately disconnects the slow origin and cancels its timer.
    if (error.name !== 'AbortError') throw error
  }
}

async function main() {
  const [viceroy, suppliedBinary, ...extra] = process.argv.slice(2)
  assert(
    viceroy && extra.length === 0,
    'usage: node deadline-origin.mjs /path/to/viceroy [test-binary.wasm]'
  )
  const binary = suppliedBinary ?? (await testArtifact())
  let originError
  const server = createServer((request, response) => {
    void serveOrigin(request, response).catch((error) => {
      originError = error
      response.destroy()
    })
  })
  server.listen(0, '127.0.0.1')
  await once(server, 'listening')
  const directory = await mkdtemp(join(tmpdir(), 'fastly-deadline-'))
  try {
    const config = join(directory, 'fastly.toml')
    await writeFile(
      config,
      '[local_server.backends.deadline_origin]\n' +
        `url = "http://127.0.0.1:${server.address().port}"\n`
    )
    const environment = { ...process.env }
    if (existsSync('/etc/ssl/cert.pem'))
      environment.SSL_CERT_FILE ??= '/etc/ssl/cert.pem'
    const child = spawn(
      viceroy,
      [
        'run',
        '-C',
        config,
        binary,
        'platform::deadline::tests::network_',
        '--ignored',
        '--nocapture',
      ],
      {
        env: environment,
        stdio: ['inherit', 'pipe', 'inherit'],
        signal: interruption.signal,
        killSignal: 'SIGKILL',
      }
    )
    let output = ''
    child.stdout.setEncoding('utf8')
    child.stdout.on('data', (chunk) => {
      output += chunk
      process.stdout.write(chunk)
    })
    // Clear the watchdog even if spawning fails: Node's built-in child timeout
    // can otherwise keep the harness alive after an ENOENT startup error.
    const watchdog = setTimeout(() => child.kill('SIGKILL'), 120_000)
    watchdog.unref()
    try {
      const [code] = await once(child, 'close')
      if (originError) throw originError
      assert.equal(code, 0, 'should run the network tests successfully')
      assert.match(
        output,
        /^test result: ok\. 5 passed; 0 failed; 0 ignored; 0 measured;/m,
        'should execute all five network tests, including when the filter changes'
      )
      return code ?? 1
    } finally {
      clearTimeout(watchdog)
    }
  } finally {
    const closed = new Promise((resolve) => server.close(resolve))
    server.closeAllConnections()
    await closed
    await rm(directory, { recursive: true, force: true })
  }
}

main()
  .then((code) => {
    process.exitCode = code
  })
  .catch((error) => {
    console.error(error)
    process.exitCode = 1
  })
