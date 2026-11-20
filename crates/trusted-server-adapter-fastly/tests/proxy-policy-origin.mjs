// Real Fastly transport qualification; this fixture makes no cache-hit claim.
import assert from 'node:assert/strict'
import { execFile, spawn } from 'node:child_process'
import { once } from 'node:events'
import { existsSync } from 'node:fs'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'
import { promisify } from 'node:util'

const execute = promisify(execFile)
const names = [
  'network_two_readers_share_static_origin_uri',
  'network_redirect_removes_echoed_identity',
  'network_exhausted_redirect_never_returns_raw_location',
]

async function testArtifact(signal) {
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
    { signal, killSignal: 'SIGKILL', maxBuffer: 16 * 1024 * 1024 }
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

function serveOrigin(request, response) {
  const url = new URL(request.url, 'http://localhost')
  if (url.pathname === '/redirect') {
    response.writeHead(302, {
      Location: '/static/redirect.jpg?size=728&ts-ec=echoed',
    })
  } else if (url.pathname === '/loop') {
    const hop = Number(url.searchParams.get('hop') ?? '0')
    response.writeHead(302, { Location: `/loop?hop=${hop + 1}&ts-ec=echoed` })
  } else {
    response.writeHead(200, {
      'Cache-Control': 'public, max-age=300',
      'Content-Type': 'image/jpeg',
    })
  }
  response.end('fixture-object')
}

export async function runFixture({
  viceroy,
  binary,
  timeoutMs = 120_000,
  temporaryRoot = tmpdir(),
  serve = serveOrigin,
  signal,
} = {}) {
  assert(viceroy, 'should supply a Viceroy executable')
  const cancellation = new AbortController()
  const interrupt = () =>
    cancellation.abort(new Error('runtime fixture interrupted'))
  process.once('SIGINT', interrupt)
  process.once('SIGTERM', interrupt)
  signal?.addEventListener('abort', interrupt, { once: true })
  const watchdog = setTimeout(
    () => cancellation.abort(new Error('runtime fixture watchdog expired')),
    timeoutMs
  )
  watchdog.unref()
  let server
  let directory
  let child
  let closed
  let originError
  const observed = []
  try {
    binary ??= await testArtifact(cancellation.signal)
    server = createServer((request, response) => {
      observed.push(request.url)
      Promise.resolve()
        .then(() => serve(request, response))
        .catch((error) => {
          originError = error
          response.destroy()
          cancellation.abort(error)
        })
    })
    server.listen(0, '127.0.0.1')
    await once(server, 'listening', { signal: cancellation.signal })
    directory = await mkdtemp(join(temporaryRoot, 'fastly-proxy-policy-'))
    const config = join(directory, 'fastly.toml')
    await writeFile(
      config,
      '[local_server.config_stores.proxy_policy_fixture]\nformat = "inline-toml"\n[local_server.config_stores.proxy_policy_fixture.contents]\n' +
        `origin = "http://127.0.0.1:${server.address().port}"\n`
    )
    const environment = { ...process.env }
    if (existsSync('/etc/ssl/cert.pem'))
      environment.SSL_CERT_FILE ??= '/etc/ssl/cert.pem'
    child = spawn(
      viceroy,
      [
        'run',
        '-C',
        config,
        binary,
        'proxy_policy_runtime::network_',
        '--ignored',
        '--nocapture',
      ],
      {
        env: environment,
        stdio: ['ignore', 'pipe', 'pipe'],
        signal: cancellation.signal,
        killSignal: 'SIGKILL',
      }
    )
    closed = new Promise((resolve) =>
      child.once('close', (code, signal) => resolve({ code, signal }))
    )
    let output = ''
    let errors = ''
    child.stdout.on('data', (chunk) => {
      output += chunk
      process.stdout.write(chunk)
    })
    child.stderr.on('data', (chunk) => {
      errors += chunk
    })
    const error = new Promise((_, reject) => child.once('error', reject))
    const result = await Promise.race([closed, error])
    if (originError) throw originError
    if (cancellation.signal.aborted) throw cancellation.signal.reason
    assert.equal(
      result.code,
      0,
      `should run real proxy network tests: ${errors}`
    )
    assert.match(
      output,
      /^test result: ok\. 3 passed; 0 failed; 0 ignored; 0 measured;/m,
      'should execute all three proxy runtime tests'
    )
    for (const name of names)
      assert.match(
        output,
        new RegExp(`test proxy_policy_runtime::${name} \\.\\.\\. ok`),
        `should execute ${name}`
      )
    const staticUris = observed.filter((uri) =>
      uri.startsWith('/static/banner.jpg')
    )
    assert.equal(
      staticUris.length,
      2,
      'should observe both canonical readers at the origin'
    )
    assert.equal(
      staticUris[0],
      staticUris[1],
      'should observe identical static requests from both readers'
    )
    assert.match(
      staticUris[0],
      /^\/static\/banner\.jpg\?size=728&tsexp=\d+$/,
      'should preserve the sign endpoint expiry without adding identity'
    )
    assert(
      observed.some((uri) => /^\/redirect\?tsexp=\d+$/.test(uri)),
      'should observe the real redirect request'
    )
    assert(
      observed.includes('/static/redirect.jpg?size=728'),
      'should strip echoed identity on the redirected static request'
    )
    assert(
      observed.filter((uri) => uri.startsWith('/loop')).length > 1,
      'should exercise redirect exhaustion over the network'
    )
    assert(
      observed.every((uri) => !/ts-ec/i.test(uri)),
      'should never send EC to the local HTTP origin'
    )
  } catch (error) {
    throw (
      originError ??
      (cancellation.signal.aborted ? cancellation.signal.reason : error)
    )
  } finally {
    clearTimeout(watchdog)
    if (child && child.exitCode === null && child.signalCode === null)
      child.kill('SIGKILL')
    if (closed) await closed
    if (server) {
      const closing = new Promise((resolve) => server.close(resolve))
      server.closeAllConnections()
      await closing
    }
    if (directory) await rm(directory, { recursive: true, force: true })
    process.removeListener('SIGINT', interrupt)
    process.removeListener('SIGTERM', interrupt)
    signal?.removeEventListener('abort', interrupt)
  }
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  const [viceroy, binary, ...extra] = process.argv.slice(2)
  assert(
    viceroy && !extra.length,
    'usage: node proxy-policy-origin.mjs /path/to/viceroy [test-binary.wasm]'
  )
  runFixture({ viceroy, binary }).catch((error) => {
    console.error(error)
    process.exitCode = 1
  })
}
