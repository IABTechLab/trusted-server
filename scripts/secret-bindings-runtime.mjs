#!/usr/bin/env node
// Native S3 regression: Fastly shipping routes, CF/Spin exporter contract fixture.
import assert from 'node:assert/strict'
import { createHash, createHmac } from 'node:crypto'
import { spawn, spawnSync } from 'node:child_process'
import { once } from 'node:events'
import { createServer as httpsServer } from 'node:https'
import { request } from 'node:http'
import { createServer, connect } from 'node:net'
import { mkdtemp, readFile, writeFile, mkdir } from 'node:fs/promises'
import { openSync, closeSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { setTimeout as delay } from 'node:timers/promises'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const hash = (value) => createHash('sha256').update(value).digest('hex')
const hmac = (key, value) => createHmac('sha256', key).update(value).digest()
const quote = JSON.stringify // JSON strings are also TOML basic strings.
const sentinel = 'FICTIONAL_S3_ASSET'

export function verifyS3(event, bindings) {
  const value = (role) =>
    bindings.find((binding) => binding.descriptor_id === role)?.value
  const { headers, method, url } = event
  const match =
    /^AWS4-HMAC-SHA256 Credential=([^/]+)\/(\d{8})\/([^/]+)\/s3\/aws4_request, SignedHeaders=([^,]+), Signature=([a-f0-9]{64})$/.exec(
      headers.authorization ?? ''
    )
  assert.ok(match, 'missing S3 SigV4 authorization')
  const [, access, date, region, signed, signature] = match
  assert.equal(access, value('s3.access-id'))
  assert.equal(region, 'us-east-1')
  assert.equal(headers['x-amz-security-token'], value('s3.session'))
  assert.equal(headers['x-amz-content-sha256'], 'UNSIGNED-PAYLOAD')
  const names = signed.split(';')
  assert.ok(names.includes('host') && names.includes('x-amz-security-token'))
  const canonicalHeaders = names
    .map((name) => `${name}:${headers[name].trim().replace(/\s+/g, ' ')}\n`)
    .join('')
  const target = new URL(url, `https://${headers.host}`)
  assert.equal(target.search, '', 'fixture paths must not contain queries')
  const canonical = [
    method,
    target.pathname,
    '',
    canonicalHeaders,
    signed,
    headers['x-amz-content-sha256'],
  ].join('\n')
  const scope = `${date}/${region}/s3/aws4_request`
  const toSign = [
    'AWS4-HMAC-SHA256',
    headers['x-amz-date'],
    scope,
    hash(canonical),
  ].join('\n')
  const key = hmac(
    hmac(hmac(hmac(`AWS4${value('s3.secret')}`, date), region), 's3'),
    'aws4_request'
  )
  assert.equal(
    createHmac('sha256', key).update(toSign).digest('hex'),
    signature,
    `${method} signature does not match received method/Host/path/session`
  )
}

function run(program, args, options = {}) {
  const result = spawnSync(program, args, {
    cwd: root,
    encoding: 'utf8',
    ...options,
  })
  assert.equal(
    result.status,
    0,
    `${program} ${args.join(' ')}\n${result.stderr}\n${result.error ?? ''}`
  )
  return result.stdout
}

async function port() {
  const server = createServer()
  server.listen(0, '127.0.0.1')
  await once(server, 'listening')
  const result = server.address().port
  await new Promise((resolve) => server.close(resolve))
  return result
}

function get(port, path, method = 'GET') {
  return new Promise((resolve, reject) => {
    const req = request(
      {
        hostname: '127.0.0.1',
        port,
        path,
        method,
        headers: { host: `127.0.0.1:${port}` },
      },
      (response) => {
        const chunks = []
        response.on('data', (chunk) => chunks.push(chunk))
        response.on('end', () =>
          resolve({
            status: response.statusCode,
            headers: response.headers,
            body: Buffer.concat(chunks).toString(),
          })
        )
        response.on('error', reject)
      }
    )
    req.setTimeout(15000, () => req.destroy(new Error('app response timeout')))
    req.on('error', reject).end()
  })
}

async function ready(child, port) {
  for (let attempt = 0; attempt < 200; attempt++) {
    if (child.startError) throw child.startError
    assert.equal(
      child.exitCode,
      null,
      'runtime exited during startup; inspect runtime.log'
    )
    const up = await new Promise((resolve) => {
      const socket = connect({ host: '127.0.0.1', port })
      socket.once('connect', () => {
        socket.destroy()
        resolve(true)
      })
      socket.once('error', () => resolve(false))
    })
    if (up) return
    await delay(100)
  }
  throw new Error('runtime did not listen; inspect runtime.log')
}

async function stop(child) {
  if (!child?.pid) return
  const exited = once(child, 'exit')
  try {
    process.kill(-child.pid, 'SIGTERM')
  } catch (error) {
    if (error.code !== 'ESRCH') throw error
  }
  await Promise.race([exited, delay(3000)])
  // Wrangler launches workerd children. Kill the isolated group even if its parent exited.
  try {
    process.kill(-child.pid, 'SIGKILL')
  } catch (error) {
    if (error.code !== 'ESRCH') throw error
  }
}

async function certificates(workspace) {
  for (const name of ['ca', 'untrusted']) {
    run('openssl', [
      'req',
      '-x509',
      '-newkey',
      'rsa:2048',
      '-nodes',
      '-days',
      '1',
      '-subj',
      `/CN=Fictional ${name}`,
      '-keyout',
      join(workspace, `${name}.key`),
      '-out',
      join(workspace, `${name}.pem`),
    ])
  }
  await writeFile(
    join(workspace, 'extensions'),
    'subjectAltName=DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n'
  )
  run('openssl', [
    'req',
    '-new',
    '-newkey',
    'rsa:2048',
    '-nodes',
    '-subj',
    '/CN=localhost',
    '-keyout',
    join(workspace, 'server.key'),
    '-out',
    join(workspace, 'server.csr'),
  ])
  for (const [name, ca] of [
    ['server', 'ca'],
    ['bad', 'untrusted'],
  ]) {
    run('openssl', [
      'x509',
      '-req',
      '-in',
      join(workspace, 'server.csr'),
      '-CA',
      join(workspace, `${ca}.pem`),
      '-CAkey',
      join(workspace, `${ca}.key`),
      '-CAcreateserial',
      '-days',
      '1',
      '-extfile',
      join(workspace, 'extensions'),
      '-out',
      join(workspace, `${name}.pem`),
    ])
  }
}

async function runtime(
  adapter,
  directory,
  artifact,
  envelope,
  fixture,
  ca,
  appPort
) {
  await mkdir(directory, { recursive: true })
  const bindings = fixture.bindings
  let program, args
  if (adapter === 'fastly') {
    const backends = fixture.backends
      .map(
        (backend) =>
          `[local_server.backends.${quote(backend.name)}]\nurl = ${quote(backend.url)}\ncert_host = "localhost"\nuse_sni = true\nca_certificate.file = ${quote(ca)}\n`
      )
      .join('\n')
    const secrets = bindings
      .map(
        (binding) =>
          `[[local_server.secret_stores.trusted_server_secrets]]\nkey = ${quote(binding.native.native_name)}\ndata = ${quote(binding.value)}\n`
      )
      .join('\n')
    await writeFile(
      join(directory, 'fastly.toml'),
      `manifest_version = 3\nname = "secret-bindings-local"\n${backends}\n${secrets}\n[local_server.config_stores.trusted_server_config]\nformat = "inline-toml"\n[local_server.config_stores.trusted_server_config.contents]\ntrusted_server_config = ${quote(envelope)}\n`
    )
    program = process.env.VICEROY_BIN ?? 'viceroy'
    args = [
      '--config',
      join(directory, 'fastly.toml'),
      '--addr',
      `127.0.0.1:${appPort}`,
      artifact,
    ]
  } else if (adapter === 'cloudflare') {
    await writeFile(
      join(directory, 'wrangler.toml'),
      `name = "secret-bindings-local"\nmain = ${quote(artifact)}\ncompatibility_date = "2024-09-23"\ncompatibility_flags = ["nodejs_compat", "cache_option_enabled"]\n[vars]\nTRUSTED_SERVER_CONFIG = ${quote(JSON.stringify({ app_config: envelope }))}\n${bindings.map((binding) => `${binding.native.native_name} = ${quote(binding.value)}`).join('\n')}\n`
    )
    program = process.env.WRANGLER_BIN ?? 'wrangler'
    args = [
      'dev',
      '--local',
      '--config',
      join(directory, 'wrangler.toml'),
      '--ip',
      '127.0.0.1',
      '--port',
      String(appPort),
      '--persist-to',
      join(directory, 'state'),
    ]
  } else {
    const declarations = bindings
      .flatMap((binding) => binding.native.declarations)
      .join('\n')
    // Repeated component tables are legal only once. Render the variable mappings together.
    const appDeclarations = bindings
      .map((binding) => binding.native.declarations[0])
      .join('\n')
    const component = bindings
      .map((binding) =>
        binding.native.declarations[1].replace(
          '[component.trusted-server.variables]\n',
          ''
        )
      )
      .join('\n')
    assert.ok(declarations.includes('secret = true'))
    await writeFile(
      join(directory, 'spin.toml'),
      `spin_manifest_version = 2\n[application]\nname = "secret-bindings-local"\nversion = "0.1.0"\n${appDeclarations}\n[[trigger.http]]\nroute = "/..."\ncomponent = "trusted-server"\n[component.trusted-server]\nsource = ${quote(artifact)}\nallowed_outbound_hosts = ["https://127.0.0.1:*", "https://localhost:*"]\nkey_value_stores = ["default"]\n[component.trusted-server.variables]\n${component}\n`
    )
    program = process.env.SPIN_BIN ?? 'spin'
    args = [
      'up',
      '--from',
      join(directory, 'spin.toml'),
      '--listen',
      `127.0.0.1:${appPort}`,
      '--state-dir',
      join(directory, 'state'),
      '--log-dir',
      join(directory, 'component-logs'),
      '--key-value',
      `trusted_server_config=${envelope}`,
    ]
    for (const binding of bindings)
      args.push('--variable', `${binding.native.native_name}=${binding.value}`)
  }
  const env = Object.fromEntries(
    Object.entries(process.env).filter(
      ([name]) =>
        !/^(TRUSTED_SERVER__|EDGEZERO__|NODE_TLS_REJECT_UNAUTHORIZED$|SPIN_VARIABLE_)/.test(
          name
        )
    )
  )
  // These extend child-process trust only. No system trust or verification bypass.
  Object.assign(env, {
    NODE_EXTRA_CA_CERTS: ca,
    SSL_CERT_FILE: ca,
    WRANGLER_SEND_METRICS: 'false',
    CI: 'true',
  })
  const log = openSync(join(directory, 'runtime.log'), 'w')
  const child = spawn(program, args, {
    cwd: directory,
    env,
    stdio: ['ignore', log, log],
    detached: true,
  })
  closeSync(log)
  child.on('error', (error) => {
    child.startError = error
  })
  return child
}

export async function main(adapter) {
  assert.ok(
    ['fastly', 'cloudflare', 'spin'].includes(adapter),
    'usage: scripts/secret-bindings-runtime.sh {fastly|cloudflare|spin}'
  )
  const workspace = await mkdtemp(join(tmpdir(), `secret-bindings-${adapter}-`))
  console.log(`Evidence: ${workspace}`)
  const events = [],
    results = [],
    servers = []
  let child
  const cleanup = async () => {
    await stop(child)
    for (const server of servers) {
      server.closeAllConnections()
      await new Promise((resolve) => server.close(resolve))
    }
  }
  const interrupted = () => {
    cleanup().finally(() => process.exit(130))
  }
  process.once('SIGINT', interrupted)
  process.once('SIGTERM', interrupted)
  try {
    await certificates(workspace)
    let otherPort,
      phase = 'startup'
    const start = async (name, cert) => {
      const options = {
        key: await readFile(join(workspace, 'server.key')),
        cert: await readFile(join(workspace, cert)),
      }
      const handler = (req, res) => {
        events.push({
          phase,
          server: name,
          method: req.method,
          url: req.url,
          headers: req.headers,
        })
        const redirect = /redirect-(301|302|303|307|308)-(same|other)/.exec(
          req.url
        )
        if (redirect) {
          res.writeHead(Number(redirect[1]), {
            location: `https://localhost:${redirect[2] === 'same' ? server.address().port : otherPort}/sink.svg`,
          })
          res.end()
        } else {
          res.writeHead(200, {
            'content-type': 'image/svg+xml',
            'content-length': Buffer.byteLength(sentinel),
            'cache-control': 'no-store',
          })
          res.end(req.method === 'HEAD' ? undefined : sentinel)
        }
      }
      const server = httpsServer(options, handler)
      server.connectionsSeen = 0
      server.on('connection', () => server.connectionsSeen++)
      server.on('tlsClientError', () => {})
      server.listen(0, '127.0.0.1')
      await once(server, 'listening')
      servers.push(server)
      // localhost resolution differs across native runtimes. Listen on both
      // loopback addresses without opening a wildcard/network interface.
      const ipv6 = httpsServer(options, handler)
      ipv6.on('connection', () => server.connectionsSeen++)
      ipv6.on('tlsClientError', () => {})
      ipv6.listen(server.address().port, '::1')
      await once(ipv6, 'listening')
      servers.push(ipv6)
      return server
    }
    const trusted = await start('trusted', 'server.pem')
    const bad = await start('bad-cert', 'bad.pem')
    const sink = await start('other-origin', 'server.pem')
    otherPort = sink.address().port
    const fixtureBin =
      process.env.SECRET_BINDINGS_FIXTURE_BIN ??
      join(root, 'target/debug/secret-bindings-fixture')
    const fixture = JSON.parse(
      run(fixtureBin, [
        adapter,
        String(trusted.address().port),
        String(bad.address().port),
        String(otherPort),
      ])
    )
    await writeFile(
      join(workspace, 'fixture.json'),
      JSON.stringify(fixture, null, 2)
    )
    const artifact = resolve(
      process.env.SECRET_BINDINGS_ARTIFACT ??
        (adapter === 'cloudflare'
          ? join(
              root,
              'crates/trusted-server-adapter-cloudflare/build/index.js'
            )
          : join(
              root,
              'target/wasm32-wasip1/release',
              adapter === 'fastly'
                ? 'trusted-server-adapter-fastly.wasm'
                : 'trusted_server_adapter_spin.wasm'
            ))
    )
    for (const [name, envelope] of Object.entries(fixture.cases)) {
      const directory = join(workspace, name),
        appPort = await port()
      // Fastly exercises shipping app routing. CF/Spin lack asset app routes;
      // their test-utils integration forwards to the same production exporter.
      const appGet = (path, method) =>
        get(
          appPort,
          `${adapter === 'fastly' ? '' : '/integrations/secret-binding-runtime'}${path}`,
          method
        )
      phase = 'startup'
      child = await runtime(
        adapter,
        directory,
        artifact,
        envelope,
        fixture,
        join(workspace, 'ca.pem'),
        appPort
      )
      await ready(child, appPort)
      // Viceroy probes all static backends before opening its listener. Keep
      // those unsigned startup probes in evidence, outside app-send assertions.
      phase = name
      const before = events.length
      for (const event of events.filter((event) => event.phase === 'startup')) {
        assert.equal(
          event.method,
          'GET',
          'runtime startup probes must be plain GET'
        )
        assert.equal(
          event.url,
          '/',
          'runtime startup probes must target only root'
        )
        assert.equal(
          event.headers.authorization,
          undefined,
          'runtime startup probes must never include credentials'
        )
      }
      if (name === 'original') {
        for (const [prefix, server, expectedPort] of [
          ['assets', 'trusted', trusted.address().port],
          ['sink-control', 'other-origin', otherPort],
        ])
          for (const method of ['GET', 'HEAD']) {
            const prior = events.length
            const response = await appGet(
              `/${prefix}/success-${method}.svg`,
              method
            )
            assert.equal(
              response.status,
              200,
              `${adapter} trusted HTTPS ${method}`
            )
            assert.equal(response.body, method === 'GET' ? sentinel : '')
            assert.equal(
              events.length,
              prior + 1,
              'positive request must reach S3 exactly once'
            )
            const event = events.at(-1)
            assert.equal(event.server, server)
            assert.equal(event.method, method)
            assert.equal(
              event.headers.host,
              `localhost:${expectedPort}`,
              'preserve signed Host authority including port'
            )
            verifyS3(event, fixture.bindings)
            results.push({
              case: `trusted-https-${prefix}`,
              method,
              status: response.status,
            })
          }
        for (const status of [301, 302, 303, 307, 308])
          for (const target of ['same', 'other'])
            for (const method of ['GET', 'HEAD']) {
              const prior = events.length
              const response = await appGet(
                `/assets/redirect-${status}-${target}-${method}.svg`,
                method
              )
              assert.equal(
                response.status,
                status,
                'return redirect without following it'
              )
              assert.equal(
                events.length,
                prior + 1,
                'redirect must not contact either sink'
              )
              verifyS3(events.at(-1), fixture.bindings)
              results.push({
                case: `redirect-${status}-${target}`,
                method,
                status: response.status,
              })
            }
        for (const method of ['GET', 'HEAD']) {
          const prior = events.length,
            connections = bad.connectionsSeen
          const response = await appGet(
            `/bad-cert/reject-${method}.svg`,
            method
          )
          assert.ok(response.status >= 500, 'untrusted TLS must fail')
          assert.equal(
            events.length,
            prior,
            'bad certificate must prevent every HTTP send'
          )
          assert.ok(
            bad.connectionsSeen > connections,
            'bad-cert check must reach the TLS listener'
          )
          results.push({
            case: 'bad-certificate-proxy-check-false',
            method,
            status: response.status,
          })
        }
      } else {
        for (const method of ['GET', 'HEAD']) {
          const response = await appGet(`/assets/mutated-${method}.svg`, method)
          assert.ok(
            response.status >= 500,
            `${name} must reject during native config loading`
          )
          results.push({ case: name, method, status: response.status })
        }
        assert.equal(
          events.length,
          before,
          `${name} must reject before any outbound HTTP send`
        )
        await stop(child)
        child = undefined
        const log = await readFile(
          join(
            directory,
            adapter === 'spin'
              ? 'component-logs/trusted-server_stderr.txt'
              : 'runtime.log'
          ),
          'utf8'
        )
        assert.match(
          log,
          name === 'cross-field'
            ? /removed credential reference/
            : name === 'ec-allowlist'
              ? /invalid unresolved configuration/
              : /native credential unavailable/,
          'failure must identify config/native binding rejection, not transport failure'
        )
      }
      await stop(child)
      child = undefined
    }
    assert.equal(
      events.filter(
        (event) => event.phase !== 'startup' && event.server === 'other-origin'
      ).length,
      2,
      'other-origin sink must receive only its GET/HEAD authorized controls'
    )
    assert.ok(
      !events.some((event) => event.url === '/sink.svg'),
      'no redirect sink may receive requests'
    )
    console.log(
      `${adapter}: ${results.length} ${adapter === 'fastly' ? 'shipping S3 route' : 'native core/adapter S3 contract'} security assertions passed`
    )
  } finally {
    await cleanup()
    process.removeListener('SIGINT', interrupted)
    process.removeListener('SIGTERM', interrupted)
    await writeFile(
      join(workspace, 'events.json'),
      JSON.stringify(events, null, 2)
    )
    await writeFile(
      join(workspace, 'results.json'),
      JSON.stringify(results, null, 2)
    )
  }
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  main(process.argv[2]).catch((error) => {
    console.error(error)
    process.exitCode = 1
  })
}
