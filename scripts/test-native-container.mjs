#!/usr/bin/env node
// Synthetic, account-free smoke. Usage: node scripts/test-native-container.mjs IMAGE
import assert from 'node:assert/strict'
import { execFile } from 'node:child_process'
import { createHash, randomUUID } from 'node:crypto'
import { chmod, mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import http from 'node:http'
import net from 'node:net'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { setTimeout as delay } from 'node:timers/promises'
import { promisify } from 'node:util'

const exec = promisify(execFile)
const image = process.argv[2]
assert(image, 'supply a built local image tag')
const work = await mkdtemp(join(tmpdir(), 'ts-container-'))
const owned = new Set()
let interrupted = false
const interrupt = () => {
  interrupted = true
}
process.on('SIGINT', interrupt)
process.on('SIGTERM', interrupt)
const docker = async (...args) =>
  (
    await exec('docker', args, { timeout: 20000, maxBuffer: 1024 * 1024 })
  ).stdout.trim()
const check = () => assert(!interrupted, 'interrupted')
const bounded = async (operation, milliseconds, message) => {
  const end = Date.now() + milliseconds
  while (Date.now() < end) {
    check()
    if (await operation()) return
    await delay(50)
  }
  throw new Error(message)
}
function sorted(value) {
  if (Array.isArray(value)) return value.map(sorted)
  if (value !== null && typeof value === 'object')
    return Object.fromEntries(
      Object.keys(value)
        .sort()
        .map((key) => [key, sorted(value[key])])
    )
  return value
}
function envelope(data) {
  data = sorted(data)
  return JSON.stringify({
    version: 1,
    generated_at: '2026-10-20T00:00:00Z',
    data,
    sha256: createHash('sha256').update(JSON.stringify(data)).digest('hex'),
  })
}
async function request(endpoint, path) {
  // Direct HTTP avoids inherited operator proxy settings and redirect following.
  return new Promise((resolve, reject) => {
    const req = http.get(`${endpoint}${path}`, { agent: false }, (res) => {
      const chunks = []
      let size = 0
      res.on('data', (chunk) => {
        size += chunk.length
        if (size > 2 * 1024 * 1024) req.destroy(new Error('response limit'))
        else chunks.push(chunk)
      })
      res.on('error', reject)
      res.on('end', () =>
        resolve({
          status: res.statusCode,
          headers: new Headers(
            Object.entries(res.headers).map(([key, value]) => [
              key,
              String(value),
            ])
          ),
          body: Buffer.concat(chunks).toString('utf8'),
        })
      )
    })
    const timer = setTimeout(
      () => req.destroy(new Error('request deadline')),
      10000
    )
    req.on('close', () => clearTimeout(timer))
    req.on('error', reject)
  })
}
const network = `ts-container-network-${randomUUID()}`
let networkCreated = false
const originName = `ts-container-origin-${randomUUID()}`
try {
  await chmod(work, 0o755)
  // Register before creation so a timed-out Docker client cannot orphan the network.
  networkCreated = true
  await docker('network', 'create', network)
  await writeFile(
    join(work, 'origin.mjs'),
    `
import http from 'node:http';
import { writeFileSync } from 'node:fs';
http.createServer((req, res) => {
  const send = () => {
    res.writeHead(200, { 'content-type': 'text/html' });
    res.end('<!doctype html><html><head><title>synthetic-origin</title></head><body>container-fixture</body></html>');
  };
  if (req.url === '/slow') {
    writeFileSync('/tmp/slow-started', 'started');
    setTimeout(send, 1200);
  } else send();
}).listen(8788, '0.0.0.0');
`
  )
  await chmod(join(work, 'origin.mjs'), 0o444)
  owned.add(originName)
  await docker(
    'run',
    '-d',
    '--name',
    originName,
    '--network',
    network,
    '--network-alias',
    'origin.publisher.example.com',
    '--read-only',
    '--user',
    '10001:10001',
    '--cap-drop=ALL',
    '--security-opt=no-new-privileges',
    '--tmpfs',
    '/tmp:rw,noexec,nosuid,size=1m,uid=10001,gid=10001,mode=0700',
    '--mount',
    `type=bind,src=${work}/origin.mjs,dst=/origin.mjs,readonly`,
    'node:24.12.0-bookworm-slim@sha256:7326fb2dbdce998edd72140946851be64ef4a643e8715e138ca467e8e9d92c99',
    'node',
    '/origin.mjs'
  )
  const fixture = {
    ec: { passphrase: 'ec_key' },
    handlers: [
      { path: '^/_ts/admin', username: 'admin', password: 'admin_key' },
    ],
    publisher: {
      domain: 'publisher.example.com',
      cookie_domain: '.publisher.example.com',
      origin_url: 'http://origin.publisher.example.com:8788',
      proxy_secret: 'proxy_key',
    },
    request_signing: null,
    response_headers: { 'x-container-snapshot': 'synthetic' },
  }
  const secrets =
    'EDGEZERO__SECRETS__EC_KEY=synthetic-container-ec-passphrase-32-bytes\nEDGEZERO__SECRETS__PROXY_KEY=synthetic-container-proxy-secret-32-bytes\nEDGEZERO__SECRETS__ADMIN_KEY=synthetic-container-admin-secret-32-bytes\n'
  const launch = async (label, data, secretText) => {
    check()
    const root = join(work, label)
    await mkdir(root, { mode: 0o755 })
    await writeFile(
      join(root, 'local-config-trusted_server_config.json'),
      JSON.stringify({ trusted_server_config: data })
    )
    await writeFile(join(root, 'local.env'), secretText)
    // Synthetic values only. Make fixtures readable by the image's numeric UID,
    // independently of the caller's umask; bind mounts remain read-only.
    await chmod(root, 0o755)
    await chmod(join(root, 'local-config-trusted_server_config.json'), 0o444)
    await chmod(join(root, 'local.env'), 0o444)
    const reservation = net.createServer()
    await new Promise((resolve, reject) => {
      reservation.once('error', reject)
      reservation.listen(0, '127.0.0.1', resolve)
    })
    const hostPort = reservation.address().port
    await new Promise((resolve) => reservation.close(resolve))
    const name = `ts-container-${randomUUID()}`
    // Register before launch so even an interrupted/failed Docker client is cleaned up.
    owned.add(name)
    await docker(
      'run',
      '-d',
      '--name',
      name,
      '--read-only',
      '--cap-drop=ALL',
      '--security-opt=no-new-privileges',
      '--network',
      network,
      '--publish',
      `127.0.0.1:${hostPort}:8787`,
      '--mount',
      `type=bind,src=${root},dst=/config,readonly`,
      '--mount',
      `type=bind,src=${root}/local.env,dst=/run/secrets/local.env,readonly`,
      '--tmpfs',
      '/data:rw,noexec,nosuid,size=16m,uid=10001,gid=10001,mode=0700',
      '--tmpfs',
      '/tmp:rw,noexec,nosuid,size=16m,uid=10001,gid=10001,mode=0700',
      '--env',
      'EDGEZERO__ADAPTER__SHUTDOWN_GRACE_SECONDS=3',
      '--env',
      'TRUSTED_SERVER__EC__PASSPHRASE=poison',
      '--env',
      'TRUSTED_SERVER__PUBLISHER__ORIGIN_URL=https://poison.example.com',
      '--entrypoint',
      '/bin/sh',
      image,
      '-ec',
      'set -a; . /run/secrets/local.env; exec /usr/local/bin/app'
    )
    // Fast startup rejection can remove Docker's active port mapping before inspect.
    return { name, endpoint: `http://127.0.0.1:${hostPort}` }
  }
  const state = async (name) =>
    JSON.parse(await docker('inspect', '--format={{json .State}}', name))
  for (const [label, data, secretText] of [
    ['invalid-envelope', 'invalid', secrets],
    ['missing-secret', envelope(fixture), ''],
  ]) {
    const app = await launch(label, data, secretText)
    await bounded(
      async () => {
        // A response of any status is a failure, not readiness.
        let response
        try {
          response = await request(app.endpoint, '/health')
        } catch {
          /* Connection refusal is expected. */
        }
        assert(!response, `${label} must not listen`)
        const current = await state(app.name)
        if (current.Running) return false
        assert.notEqual(current.ExitCode, 0)
        assert.equal(current.OOMKilled, false)
        return true
      },
      15000,
      `${label} startup rejection deadline`
    )
    const logs = await docker('logs', app.name)
    assert.match(
      logs,
      /Trusted Server initialization failed/,
      'must reject application inputs, not fail the launcher'
    )
    assert.doesNotMatch(
      logs,
      /phase=ready/,
      'invalid inputs must never reach readiness'
    )
    console.log(`PASS ${label} fails before listen`)
    console.log(logs)
  }
  const app = await launch('valid', envelope(fixture), secrets)
  await bounded(
    async () => {
      assert((await state(app.name)).Running, 'valid startup exited')
      let result
      try {
        result = await request(app.endpoint, '/health')
      } catch {
        return false
      }
      assert.equal(result.status, 200)
      assert.equal(result.body, 'ok')
      assert.equal(result.headers.get('x-container-snapshot'), 'synthetic')
      return true
    },
    15000,
    'valid readiness deadline'
  )
  console.log('PASS published port reaches non-loopback container listener')
  const page = await request(app.endpoint, '/')
  assert.equal(page.status, 200)
  assert.match(page.body, /container-fixture/)
  const jsPath = page.body.match(/\/static\/tsjs=[^"'\s<>]+/)?.[0]
  assert(jsPath, 'page must inject actual TSJS URL')
  const js = await request(app.endpoint, jsPath)
  assert.equal(js.status, 200)
  assert(js.body.length > 100, 'embedded TSJS must not be empty')
  assert.equal(js.headers.get('x-geo-info-available'), 'false')
  console.log('PASS ordinary TS envelope, page rewrite and embedded TSJS')
  await docker(
    'exec',
    app.name,
    'sh',
    '-ec',
    'test $(id -u) = 10001; test -s /etc/ssl/certs/ca-certificates.crt; ! command -v node; ! command -v rustc; ! touch /app/read-only-proof; touch /data/write-proof /tmp/write-proof'
  )
  console.log(
    'PASS numeric non-root, CA roots, no Node/Rust, read-only root and explicit writable mounts'
  )
  const inFlight = request(app.endpoint, '/slow')
  // Attach immediately to avoid an unhandled rejection during signal testing.
  const outcome = inFlight.then(
    (value) => ({ value }),
    (error) => ({ error })
  )
  await bounded(
    async () => {
      try {
        await docker('exec', originName, 'test', '-f', '/tmp/slow-started')
        return true
      } catch {
        return false
      }
    },
    5000,
    'in-flight origin deadline'
  )
  const started = Date.now()
  await docker('kill', '--signal=TERM', app.name)
  await bounded(
    async () => !(await state(app.name)).Running,
    7000,
    'SIGTERM exit deadline'
  )
  const final = await state(app.name)
  assert.equal(final.ExitCode, 0, 'RC signal hooks should exit normally')
  const completed = await outcome
  assert(!completed.error, 'in-flight request must complete during drain')
  assert.equal(completed.value.status, 200)
  assert.match(completed.value.body, /container-fixture/)
  console.log(
    `PASS in-flight SIGTERM drains and exits normally within ${Date.now() - started}ms`
  )
  const runtimeLogs = await docker('logs', app.name)
  assert.match(runtimeLogs, /phase=draining reason=drain_requested/)
  assert.match(runtimeLogs, /phase=stopped reason=stopped/)
  assert(
    runtimeLogs.indexOf('phase=draining') <
      runtimeLogs.lastIndexOf('request_terminal'),
    'the in-flight request must finish after RC starts draining'
  )
  console.log(runtimeLogs)
  console.log(
    await docker(
      'image',
      'inspect',
      '--format={{.Id}} {{.Os}}/{{.Architecture}} user={{.Config.User}}',
      image
    )
  )
} catch (error) {
  for (const name of owned) {
    try {
      console.error(
        `Logs for owned fixture ${name}:\n${await docker('logs', name)}`
      )
    } catch {
      /* Launch may have failed before container creation. */
    }
  }
  throw error
} finally {
  for (const name of [...owned].reverse()) {
    try {
      await docker('rm', '-f', name)
    } catch (error) {
      console.error(
        `cleanup failed for owned container ${name}: ${error.message}`
      )
      process.exitCode = 1
    }
  }
  if (networkCreated) {
    try {
      await docker('network', 'rm', network)
    } catch (error) {
      console.error(
        `cleanup failed for owned network ${network}: ${error.message}`
      )
      process.exitCode = 1
    }
  }
  await rm(work, { recursive: true, force: true })
  process.off('SIGINT', interrupt)
  process.off('SIGTERM', interrupt)
}
