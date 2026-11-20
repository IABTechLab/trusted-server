#!/usr/bin/env node
// Local synthetic EC smoke, using an already-built native runner or local image.
// Never builds, pulls images, reads operator configuration, or contacts AWS.
// Requires aws-dynamodb-ec + geo-cloudfront and an existing DynamoDB Local.
import assert from 'node:assert/strict'
import { execFile, spawn } from 'node:child_process'
import { createHash, randomUUID } from 'node:crypto'
import {
  chmod,
  mkdir,
  mkdtemp,
  readFile,
  rm,
  writeFile,
} from 'node:fs/promises'
import http from 'node:http'
import net from 'node:net'
import { tmpdir } from 'node:os'
import { isAbsolute, join, resolve } from 'node:path'
import { setTimeout as delay } from 'node:timers/promises'
import { promisify } from 'node:util'

const usage = `Usage:
  node scripts/test-native-ec-image.mjs --binary /absolute/trusted-server-axum --dynamodb http://127.0.0.1:8000
  node scripts/test-native-ec-image.mjs --image LOCAL_IMAGE --dynamodb http://127.0.0.1:8000
Options:
  --image-binary PATH       Image executable, default /usr/local/bin/app
  --expected-version SHA   Assert each document's X-TS-Version
  --output PATH            JSON evidence, default a unique file in the temp directory

Linux image mode uses host networking for loopback-only synthetic services,
--pull=never, a read-only root, UID 10001 and no capabilities. It tests the app
executable directly, not a deployment bootstrap wrapper. Binary mode avoids
image creation and disk use. Supply a disposable DynamoDB Local instance, never
an AWS endpoint. The script creates and deletes only its own uniquely named
table. All config, origins, geography and credentials are synthetic. This is not
real consent transport, AWS IAM, production capacity or image security proof.
`
function options(args) {
  const known = new Set([
    'binary',
    'image',
    'dynamodb',
    'image-binary',
    'expected-version',
    'output',
  ])
  const result = {}
  for (let index = 0; index < args.length; index++) {
    const key = args[index].replace(/^--/, '')
    assert(
      args[index].startsWith('--') && known.has(key),
      `unknown option ${args[index]}`
    )
    assert(!Object.hasOwn(result, key), `duplicate --${key}`)
    const value = args[++index]
    assert(value && !value.startsWith('--'), `missing --${key} value`)
    result[key] = value
  }
  assert(
    Boolean(result.binary) !== Boolean(result.image),
    'supply exactly one of --binary or --image'
  )
  assert(result.dynamodb, 'supply --dynamodb pointing to DynamoDB Local')
  if (result.binary)
    assert(isAbsolute(result.binary), '--binary must be an absolute path')
  if (result.image)
    assert.equal(
      process.platform,
      'linux',
      'image host-network mode requires Linux'
    )
  return result
}
function localEndpoint(value) {
  const url = new URL(value)
  assert.equal(url.protocol, 'http:', 'local endpoint must use HTTP')
  assert.equal(
    url.hostname,
    '127.0.0.1',
    'use literal 127.0.0.1, never an external endpoint or DNS name'
  )
  assert(
    url.port &&
      !url.username &&
      !url.password &&
      !url.search &&
      !url.hash &&
      url.pathname === '/',
    'supply a plain loopback HTTP endpoint with explicit port'
  )
  return url.origin
}
function sorted(value) {
  if (Array.isArray(value)) return value.map(sorted)
  if (value !== null && typeof value === 'object') {
    return Object.fromEntries(
      Object.keys(value)
        .sort()
        .map((key) => [key, sorted(value[key])])
    )
  }
  return value
}
function envelope(data) {
  data = sorted(data)
  return JSON.stringify({
    version: 1,
    generated_at: '2026-10-27T00:00:00Z',
    data,
    sha256: createHash('sha256').update(JSON.stringify(data)).digest('hex'),
  })
}
// Direct HTTP, no proxy environment, redirects, DNS, or credential lookup.
function request(
  endpoint,
  path = '/',
  { method = 'GET', body, headers = {}, timeout = 10000, onHeaders } = {}
) {
  return new Promise((resolve, reject) => {
    const req = http.request(
      `${endpoint}${path}`,
      { method, headers, agent: false },
      (res) => {
        onHeaders?.(res)
        const chunks = []
        let bytes = 0
        res.on('data', (chunk) => {
          bytes += chunk.length
          if (bytes > 2 * 1024 * 1024)
            res.destroy(new Error('fixture response exceeds 2 MiB'))
          else chunks.push(chunk)
        })
        res.on('error', reject)
        res.on('end', () =>
          resolve({
            status: res.statusCode,
            headers: res.headers,
            body: Buffer.concat(chunks),
          })
        )
      }
    )
    const timer = setTimeout(
      () => req.destroy(new Error('fixture HTTP deadline')),
      timeout
    )
    req.on('close', () => clearTimeout(timer))
    req.on('error', reject)
    req.end(body)
  })
}
async function listen(server) {
  await new Promise((resolve, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', resolve)
  })
  return `http://127.0.0.1:${server.address().port}`
}
async function freePort() {
  const server = net.createServer()
  await listen(server)
  const port = server.address().port
  await new Promise((resolve) => server.close(resolve))
  return port
}
function minted(response) {
  return (response.headers['set-cookie'] ?? [])
    .filter((value) => value.startsWith('ts-ec='))
    .map((value) => value.slice('ts-ec='.length).split(';')[0])
    .filter(Boolean)
}

if (process.argv.includes('--help')) {
  console.log(usage)
  process.exit(0)
}
const opt = options(process.argv.slice(2))
const dynamo = localEndpoint(opt.dynamodb)
const id = randomUUID()
const table = `ts-native-ec-fixture-${id}`
const namespace = `fixture-${id}`
const partition = `ec:${namespace}`
const allocatorPartition = `ec-allocator:${namespace}`
const work = await mkdtemp(join(tmpdir(), 'ts-native-ec-smoke-'))
const output = resolve(
  opt.output ?? join(tmpdir(), `ts-native-ec-smoke-${id}.json`)
)
const exec = promisify(execFile)
const docker = async (...args) =>
  (
    await exec('docker', args, { timeout: 20000, maxBuffer: 1024 * 1024 })
  ).stdout.trim()
const proof = {
  outcome: 'running',
  startedAt: new Date().toISOString(),
  table,
  namespace,
  dynamo,
  checks: [],
  events: [],
  artifacts: {},
  limitations: [
    'synthetic geography and local consent policy only',
    'no live AWS IAM or browser acceptance',
    'direct app executable, not deployment wrapper',
    'local harness image or binary smoke does not qualify production release build, linkage or Docker recipe',
  ],
}
let interrupted = false
const interrupt = () => {
  interrupted = true
}
process.on('SIGINT', interrupt)
process.on('SIGTERM', interrupt)
const record = (name) => {
  proof.checks.push(name)
  console.log(`PASS ${name}`)
}
const event = (name, extra = {}) => {
  const entry = {
    sequence: proof.events.length,
    at: new Date().toISOString(),
    name,
    ...extra,
  }
  proof.events.push(entry)
  return entry.sequence
}
async function bounded(operation, milliseconds, message) {
  const deadline = Date.now() + milliseconds
  while (Date.now() < deadline) {
    assert(!interrupted, 'interrupted')
    if (await operation()) return
    await delay(25)
  }
  throw new Error(message)
}
// DynamoDB Local checks the SigV4-shaped authorization header, not AWS keys.
// Fake credentials are generated here and are never read from the environment.
async function ddb(operation, payload) {
  const date = new Date().toISOString().replace(/[:-]|\.\d{3}/g, '')
  const body = JSON.stringify(payload)
  const response = await request(dynamo, '/', {
    method: 'POST',
    body,
    headers: {
      'content-type': 'application/x-amz-json-1.0',
      'content-length': Buffer.byteLength(body),
      'x-amz-target': `DynamoDB_20120810.${operation}`,
      'x-amz-date': date,
      authorization: `AWS4-HMAC-SHA256 Credential=DUMMYLOCAL/${date.slice(0, 8)}/us-east-1/dynamodb/aws4_request, SignedHeaders=host;x-amz-date, Signature=${'0'.repeat(64)}`,
    },
  })
  assert.equal(
    response.status,
    200,
    `DynamoDB Local ${operation} failed: ${response.body.toString('utf8').slice(0, 500)}`
  )
  return JSON.parse(response.body.toString('utf8'))
}
const key = (sk, pk = partition) => ({ pk: { S: pk }, sk: { S: sk } })
const get = async (sk, pk) =>
  (
    await ddb('GetItem', {
      TableName: table,
      Key: key(sk, pk),
      ConsistentRead: true,
    })
  ).Item
const decode = (item) =>
  JSON.parse(Buffer.from(item.body.B, 'base64').toString('utf8'))
let fault = 'normal'
const pending = new Set()
const verifiedPuts = []
const ownedApps = []
let tableCreated = false
let origin, proxy
// Faults alter only this fixture's app-to-local-Dynamo traffic. Administration
// bypasses the proxy so it can prove a commit before returning the SDK's ack.
async function proxyRequest(req, res) {
  try {
    const chunks = []
    let bytes = 0
    for await (const chunk of req) {
      bytes += chunk.length
      assert(bytes <= 2 * 1024 * 1024, 'fixture request limit')
      chunks.push(chunk)
    }
    const body = Buffer.concat(chunks)
    const payload = JSON.parse(body.toString('utf8'))
    const operation = String(req.headers['x-amz-target']).split('.').at(-1)
    assert.equal(payload.TableName, table, 'app attempted a non-fixture table')
    const write = operation === 'PutItem'
    if (write)
      assert.equal(
        payload.Item.pk.S,
        partition,
        'app attempted a non-fixture identity partition'
      )
    if (operation === 'UpdateItem')
      assert.equal(payload.Key.pk.S, allocatorPartition)
    const selectedFault = fault
    event('ddb-request', {
      operation,
      fault: selectedFault,
      key: payload.Item?.sk?.S ?? payload.Key?.sk?.S,
      condition: payload.ConditionExpression,
      expectedGeneration: payload.ExpressionAttributeValues?.[':expected']?.N,
      capturedNowMs: payload.ExpressionAttributeValues?.[':now']?.N,
    })
    if (write && selectedFault === 'deny') {
      res.writeHead(400, {
        'content-type': 'application/x-amz-json-1.0',
        'x-amzn-errortype': 'AccessDeniedException',
      })
      res.end(
        JSON.stringify({
          __type: 'com.amazonaws.dynamodb.v20120810#AccessDeniedException',
          message: 'synthetic fixture denial',
        })
      )
      return
    }
    if (
      write &&
      (selectedFault === 'pause' ||
        (selectedFault === 'pause-cas' &&
          payload.ConditionExpression?.includes(':expected')))
    ) {
      await new Promise((resolve) => {
        const entry = {
          resolve,
          key: payload.Item.sk.S,
          condition: payload.ConditionExpression,
          expectedGeneration:
            payload.ExpressionAttributeValues?.[':expected']?.N,
          capturedNowMs: payload.ExpressionAttributeValues?.[':now']?.N,
        }
        pending.add(entry)
        res.once('close', () => {
          pending.delete(entry)
          resolve()
        })
        entry.resolve = () => {
          pending.delete(entry)
          resolve()
        }
      })
      if (res.destroyed) return
    }
    const response = await request(dynamo, '/', {
      method: 'POST',
      body,
      headers: { ...req.headers, host: new URL(dynamo).host },
      timeout: 5000,
    })
    if (write && response.status === 200) {
      const sk = payload.Item.sk.S
      const row = await get(sk)
      assert(row, 'DynamoDB acknowledged PutItem without a readable row')
      assert.equal(row.generation.N, payload.Item.generation.N)
      assert.equal(row.body.B, payload.Item.body.B)
      verifiedPuts.push({
        key: sk,
        sequence: event('put-verified-before-ack', {
          key: sk,
          generation: row.generation.N,
          fault: selectedFault,
        }),
      })
    }
    if (write && selectedFault === 'uncertain' && response.status === 200) {
      // The row exists, but the caller never receives its successful ack.
      event('commit-ack-lost', { key: payload.Item.sk.S })
      res.destroy()
      return
    }
    res.writeHead(response.status, response.headers)
    res.end(response.body)
  } catch (error) {
    proof.proxyFailure ??= error.message
    if (!res.headersSent) res.writeHead(502, { 'content-type': 'text/plain' })
    res.end('synthetic fixture proxy failure')
  }
}
async function state(app) {
  if (app.child)
    return {
      Running: app.child.exitCode === null && app.child.signalCode === null,
      ExitCode: app.child.exitCode,
      Signal: app.child.signalCode,
    }
  return JSON.parse(
    await docker('inspect', '--format={{json .State}}', app.name)
  )
}
async function logs(app) {
  if (app.child) return app.log
  const result = await exec('docker', ['logs', app.name], {
    timeout: 5000,
    maxBuffer: 1024 * 1024,
  })
  return result.stdout + result.stderr
}
async function signal(app, value) {
  if (app.child) app.child.kill(value)
  else await docker('kill', `--signal=${value}`, app.name)
}
async function stop(app, graceful = true) {
  if (!(await state(app)).Running) return
  await signal(app, graceful ? 'SIGTERM' : 'SIGKILL')
  await bounded(
    async () => !(await state(app)).Running,
    7000,
    'runner shutdown deadline'
  )
  if (graceful)
    assert.equal((await state(app)).ExitCode, 0, 'runner must exit normally')
}
async function launch(label, originEndpoint, proxyEndpoint) {
  const port = await freePort()
  const dir = join(work, label)
  await mkdir(dir, { mode: 0o755 })
  await chmod(dir, 0o755)
  const fixture = {
    ec: {
      passphrase: 'ec_key',
      storage: {
        provider: 'dynamodb',
        region: 'us-east-1',
        table,
        namespace,
        local_endpoint: proxyEndpoint,
      },
      partners: [
        {
          name: 'synthetic fixture',
          source_domain: 'partner.example.com',
          bidstream_enabled: true,
          pull_sync_enabled: false,
        },
      ],
    },
    handlers: [
      { path: '^/_ts/admin', username: 'admin', password: 'admin_key' },
    ],
    publisher: {
      domain: 'publisher.example',
      cookie_domain: '.publisher.example',
      origin_url: originEndpoint,
      proxy_secret: 'proxy_key',
    },
    auction: { enabled: false },
    request_signing: null,
  }
  await writeFile(
    join(dir, 'local-config-trusted_server_config.json'),
    JSON.stringify({ trusted_server_config: envelope(fixture) })
  )
  await writeFile(
    join(dir, 'native-geo.toml'),
    'version = 1\n[geo]\nprovider = "cloudfront"\nalb_peer_cidrs = ["127.0.0.1/32"]\norigin_credential_header_name = "x-origin-synthetic-fixture"\n'
  )
  await chmod(join(dir, 'local-config-trusted_server_config.json'), 0o444)
  await chmod(join(dir, 'native-geo.toml'), 0o444)
  const root = opt.binary ? dir : '/fixture'
  const env = {
    HOME: root,
    PATH: '/usr/local/bin:/usr/bin:/bin',
    AWS_CONFIG_FILE: `${root}/nonexistent-aws-config`,
    AWS_SHARED_CREDENTIALS_FILE: `${root}/nonexistent-aws-credentials`,
    AWS_EC2_METADATA_DISABLED: 'true',
    AWS_REGION: 'us-east-1',
    AWS_DEFAULT_REGION: 'us-east-1',
    TS_NATIVE_GEO_PROFILE_FILE: `${root}/native-geo.toml`,
    EDGEZERO__ADAPTER__CONFIG_DIR: root,
    EDGEZERO__ADAPTER__DATA_DIR: opt.binary ? dir : '/data',
    EDGEZERO__ADAPTER__HOST: '127.0.0.1',
    EDGEZERO__ADAPTER__PORT: String(port),
    EDGEZERO__ADAPTER__SHUTDOWN_GRACE_SECONDS: '3',
    EDGEZERO__ADAPTER__TRUSTED_PROXY_CIDRS: '127.0.0.1/32',
    EDGEZERO__ADAPTER__FORWARDING_HEADER_FAMILY: 'x-forwarded',
    EDGEZERO__SECRETS__EC_KEY: 'synthetic-ec-passphrase-32-bytes-minimum',
    EDGEZERO__SECRETS__PROXY_KEY: 'synthetic-proxy-secret-32-bytes-minimum',
    EDGEZERO__SECRETS__ADMIN_KEY: 'synthetic-admin-password-32-bytes-minimum',
    RUST_LOG: 'info',
  }
  const app = { label, endpoint: `http://127.0.0.1:${port}`, log: '' }
  ownedApps.push(app)
  if (opt.binary) {
    app.child = spawn(opt.binary, [], {
      cwd: dir,
      env,
      stdio: ['ignore', 'pipe', 'pipe'],
    })
    app.child.on('error', (error) => {
      app.spawnError = error.message
    })
    const capture = (chunk) => {
      app.log = (app.log + chunk.toString('utf8')).slice(-256 * 1024)
    }
    app.child.stdout.on('data', capture)
    app.child.stderr.on('data', capture)
  } else {
    app.name = `ts-native-ec-${label}-${id}`
    await docker(
      'run',
      '-d',
      '--pull=never',
      '--name',
      app.name,
      '--network=host',
      '--read-only',
      '--user=10001:10001',
      '--cap-drop=ALL',
      '--security-opt=no-new-privileges',
      '--mount',
      `type=bind,src=${dir},dst=/fixture,readonly`,
      '--tmpfs',
      '/data:rw,noexec,nosuid,size=16m,uid=10001,gid=10001,mode=0700',
      '--tmpfs',
      '/tmp:rw,noexec,nosuid,size=16m,uid=10001,gid=10001,mode=0700',
      ...Object.entries(env).flatMap(([key, value]) => [
        '--env',
        `${key}=${value}`,
      ]),
      '--entrypoint',
      opt['image-binary'] ?? '/usr/local/bin/app',
      opt.image
    )
  }
  await bounded(
    async () => {
      assert(!app.spawnError, `runner spawn failed: ${app.spawnError}`)
      assert(
        (await state(app)).Running,
        `runner exited before readiness: ${await logs(app)}`
      )
      const health = await request(app.endpoint, '/health', {
        timeout: 300,
      }).catch(() => null)
      if (!health) return false
      assert.equal(health.status, 200)
      assert.equal(health.body.toString('utf8'), 'ok')
      assert.equal(minted(health).length, 0)
      return true
    },
    15000,
    'runner readiness deadline; confirm local endpoint seam and required features'
  )
  return app
}
const headers = (country = 'AU', extra = {}) => ({
  host: 'publisher.example',
  accept: 'text/html',
  'user-agent':
    'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/146.0.0.0 Safari/537.36',
  'sec-fetch-dest': 'document',
  'sec-fetch-mode': 'navigate',
  'x-forwarded-host': 'publisher.example',
  'x-forwarded-proto': 'https',
  'x-forwarded-for': '198.51.100.24',
  'cloudfront-viewer-country': country,
  'x-origin-synthetic-fixture': 'synthetic-fixture-only',
  ...extra,
})
async function document(
  app,
  country = 'AU',
  extra = {},
  allowStorageError = false
) {
  let responseSequence
  const result = await request(app.endpoint, '/article', {
    headers: headers(country, extra),
    onHeaders: () => {
      responseSequence = event('document-response-headers', {
        label: app.label,
        country,
      })
    },
  })
  if (allowStorageError)
    assert(
      result.status === 200 || result.status >= 500,
      'storage failure must not become an unrelated redirect or client rejection'
    )
  else
    assert.equal(
      result.status,
      200,
      'synthetic publisher document must succeed'
    )
  if (result.status === 200)
    assert.equal(
      result.headers['x-geo-country'],
      country,
      'synthetic trusted Geo must be admitted'
    )
  if (opt['expected-version'])
    assert.equal(result.headers['x-ts-version'], opt['expected-version'])
  return { ...result, responseSequence }
}
try {
  await chmod(work, 0o755)
  if (opt.binary) {
    proof.artifacts = {
      binary: opt.binary,
      sha256: createHash('sha256')
        .update(await readFile(opt.binary))
        .digest('hex'),
    }
  } else {
    const inspect = JSON.parse(await docker('image', 'inspect', opt.image))[0]
    proof.artifacts = {
      image: opt.image,
      imageId: inspect.Id,
      repoDigests: inspect.RepoDigests,
      executable: opt['image-binary'] ?? '/usr/local/bin/app',
    }
  }
  // No table can pre-exist under this unpredictable UUID. Register ownership
  // before CreateTable so an ambiguous local response still gets cleanup.
  tableCreated = true
  await ddb('CreateTable', {
    TableName: table,
    BillingMode: 'PAY_PER_REQUEST',
    KeySchema: [
      { AttributeName: 'pk', KeyType: 'HASH' },
      { AttributeName: 'sk', KeyType: 'RANGE' },
    ],
    AttributeDefinitions: [
      { AttributeName: 'pk', AttributeType: 'S' },
      { AttributeName: 'sk', AttributeType: 'S' },
    ],
  })
  await bounded(
    async () =>
      (await ddb('DescribeTable', { TableName: table })).Table.TableStatus ===
      'ACTIVE',
    5000,
    'DynamoDB Local table activation'
  )
  await ddb('UpdateTimeToLive', {
    TableName: table,
    TimeToLiveSpecification: { AttributeName: 'ttl_epoch_s', Enabled: true },
  })
  origin = http.createServer((req, res) => {
    if (req.headers['x-origin-synthetic-fixture'] !== undefined) {
      proof.originFailure =
        'protected-origin credential header reached synthetic origin'
      res.writeHead(502)
      res.end('fixture origin credential removal failed')
      return
    }
    res.writeHead(200, { 'content-type': 'text/html' })
    res.end(
      '<!doctype html><html><head><title>synthetic native EC fixture</title></head><body>fixture article</body></html>'
    )
  })
  const originEndpoint = await listen(origin)
  proxy = http.createServer((req, res) => {
    void proxyRequest(req, res)
  })
  const proxyEndpoint = await listen(proxy)
  let app = await launch('initial', originEndpoint, proxyEndpoint)
  const beforeMissing = verifiedPuts.length
  assert.equal(
    minted(
      await document(app, 'US', { 'cloudfront-viewer-country-region': 'TN' })
    ).length,
    0
  )
  assert.equal(
    verifiedPuts.length,
    beforeMissing,
    'US TN missing consent must not create an identity'
  )
  record(
    'US TN without qualifying consent does not mint or persist an identity'
  )
  for (const path of ['/health', '/_ts/admin/ec']) {
    const result = await request(app.endpoint, path, { headers: headers() })
    assert.equal(minted(result).length, 0)
  }
  record('health and rejected admin route do not mint')
  const first = await document(app)
  assert.equal(
    minted(first).length,
    1,
    'eligible synthetic AU navigation must issue exactly one EC'
  )
  const ec = minted(first)[0]
  const create = verifiedPuts.find((put) => put.key === ec)
  assert(
    create && create.sequence < first.responseSequence,
    'real row must be verified before SDK ack and cookie response headers'
  )
  const live = await get(ec)
  assert.equal(decode(live).consent.ok, true)
  assert(Number(live.expires_at_ms.N) > Date.now())
  assert.equal(
    BigInt(live.ttl_epoch_s.N),
    (BigInt(live.expires_at_ms.N) + 999n) / 1000n
  )
  assert.match(first.headers['cache-control'], /private/)
  const allocator = await get('generation', allocatorPartition)
  assert(allocator?.generation?.N)
  assert.equal(allocator.ttl_epoch_s, undefined)
  assert.equal(allocator.expires_at_ms, undefined)
  record(
    'real DynamoDB row and non-expiring allocator precede new cookie headers'
  )
  await stop(app)
  app = await launch('recreated', originEndpoint, proxyEndpoint)
  const returning = await document(app, 'AU', { cookie: `ts-ec=${ec}` })
  assert.equal(
    minted(returning).length,
    0,
    'ordinary returning document must not refresh the EC cookie'
  )
  assert.equal((await get(ec)).generation.N, live.generation.N)
  record(
    'recreated native process reuses the retained identity without cookie refresh'
  )
  // AU grants initial creation without a consent string, but unchanged core
  // policy only treats this GPC signal as explicit EC withdrawal in US TN.
  const withdrawn = await document(app, 'US', {
    'cloudfront-viewer-country-region': 'TN',
    cookie: `ts-ec=${ec}`,
    'sec-gpc': '1',
    'user-agent': 'privacy-client',
  })
  assert.equal(minted(withdrawn).length, 0)
  assert(
    (withdrawn.headers['set-cookie'] ?? []).some(
      (value) => /^ts-ec=;/.test(value) && /max-age=0/i.test(value)
    ),
    'withdrawal must expire the browser cookie'
  )
  const tombstone = await get(ec)
  assert.equal(decode(tombstone).consent.ok, false)
  assert.equal(Object.keys(decode(tombstone).ids ?? {}).length, 0)
  assert(BigInt(tombstone.generation.N) > BigInt(live.generation.N))
  assert(Number(tombstone.expires_at_ms.N) > Date.now())
  assert(Number(tombstone.ttl_epoch_s.N) * 1000 < Date.now() + 2 * 86400000)
  assert.equal(
    minted(await document(app, 'AU', { cookie: `ts-ec=${ec}` })).length,
    0
  )
  assert.equal(
    decode(await get(ec)).consent.ok,
    false,
    'returning cookie must not revive a tombstone'
  )
  record(
    'US TN GPC withdrawal expires cookie, persists a fresh-generation tombstone and resists AU reuse'
  )
  // Hold a real EID CAS after the client captured :now but before DynamoDB
  // evaluates it. Let the physical row expire logically, then withdraw it.
  // Filtering key_exists by logical expiry would skip the protective tombstone
  // and allow the delayed CAS to match the old physical generation afterward.
  const raceCreated = await document(app)
  assert.equal(minted(raceCreated).length, 1)
  const raceEc = minted(raceCreated)[0]
  const raceItem = await get(raceEc)
  const raceExpiry = Date.now() + 500
  raceItem.expires_at_ms = { N: String(raceExpiry) }
  raceItem.ttl_epoch_s = { N: String(Math.ceil(raceExpiry / 1000)) }
  await ddb('PutItem', { TableName: table, Item: raceItem })
  const raceEids = Buffer.from(
    JSON.stringify([
      {
        source: 'partner.example.com',
        uids: [{ id: 'synthetic-late-eid', atype: 1 }],
      },
    ])
  ).toString('base64')
  fault = 'pause-cas'
  const eidUpdate = document(app, 'AU', {
    cookie: `ts-ec=${raceEc}; ts-eids=${raceEids}`,
  }).then(
    (value) => ({ value }),
    (error) => ({ error })
  )
  await bounded(
    () => [...pending].some((entry) => entry.key === raceEc),
    2500,
    'EID update must reach the real conditional PutItem before logical expiry'
  )
  const pausedCas = [...pending].find((entry) => entry.key === raceEc)
  assert(pausedCas.condition.includes(':expected'))
  assert.equal(pausedCas.expectedGeneration, raceItem.generation.N)
  assert(
    Number(pausedCas.capturedNowMs) < raceExpiry,
    'CAS must capture its expiry check while the row is live'
  )
  proof.expiryWithdrawalRace = {
    ec: raceEc,
    expiresAtMs: raceExpiry,
    expectedGeneration: pausedCas.expectedGeneration,
    capturedNowMs: pausedCas.capturedNowMs,
  }
  await bounded(
    () => Date.now() > raceExpiry + 50,
    1500,
    'wait for logical expiry while the physical CAS remains pending'
  )
  const raceWithdrawal = await document(app, 'US', {
    'cloudfront-viewer-country-region': 'TN',
    cookie: `ts-ec=${raceEc}`,
    'sec-gpc': '1',
    'user-agent': 'privacy-client',
  })
  proof.expiryWithdrawalRace.withdrawalExpiredCookie = (
    raceWithdrawal.headers['set-cookie'] ?? []
  ).some((value) => /^ts-ec=;/.test(value) && /max-age=0/i.test(value))
  proof.expiryWithdrawalRace.afterWithdrawalItem = await get(raceEc)
  assert(
    proof.expiryWithdrawalRace.withdrawalExpiredCookie,
    'racing withdrawal must expire the browser cookie'
  )
  pausedCas.resolve()
  const racedUpdate = await eidUpdate
  fault = 'normal'
  assert(
    !racedUpdate.error,
    `delayed EID request failed: ${racedUpdate.error?.message}`
  )
  const afterRace = await get(raceEc)
  proof.expiryWithdrawalRace.finalItem = afterRace
  assert(afterRace, 'racing withdrawal must retain an authoritative tombstone')
  const afterRaceBody = decode(afterRace)
  assert.equal(
    afterRaceBody.consent.ok,
    false,
    'expired physical row must remain withdrawn after a delayed stale-time CAS'
  )
  assert.equal(
    Object.keys(afterRaceBody.ids ?? {}).length,
    0,
    'delayed CAS must not restore partner IDs after withdrawal'
  )
  assert.equal(minted(racedUpdate.value).length, 0)
  record(
    'logical expiry plus delayed EID CAS cannot bypass US TN withdrawal or restore partner IDs'
  )
  fault = 'deny'
  const beforeDenied = verifiedPuts.length
  assert.equal(minted(await document(app, 'AU', {}, true)).length, 0)
  assert.equal(verifiedPuts.length, beforeDenied)
  assert(
    proof.events.some(
      (entry) => entry.operation === 'PutItem' && entry.fault === 'deny'
    )
  )
  record('denied PutItem never emits a phantom cookie')
  fault = 'uncertain'
  const beforeUncertain = verifiedPuts.length
  assert.equal(minted(await document(app, 'AU', {}, true)).length, 0)
  assert(
    verifiedPuts.length > beforeUncertain,
    'uncertain fixture must actually commit before losing acknowledgement'
  )
  assert(proof.events.some((entry) => entry.name === 'commit-ack-lost'))
  record('post-commit acknowledgement loss never emits a phantom cookie')
  fault = 'pause'
  const article = document(app).then(
    (value) => ({ value }),
    (error) => ({ error })
  )
  await bounded(() => pending.size > 0, 3000, 'pending real DynamoDB PutItem')
  const healthStarted = Date.now()
  const health = await request(app.endpoint, '/health', { timeout: 750 })
  assert.equal(health.status, 200)
  assert.equal(minted(health).length, 0)
  proof.healthWhileStoragePausedMs = Date.now() - healthStarted
  const shutdownStarted = Date.now()
  await signal(app, 'SIGTERM')
  await bounded(
    async () => /phase=draining/.test(await logs(app)),
    1000,
    'runner must begin drain while real storage is paused'
  )
  for (const entry of [...pending]) entry.resolve()
  const completed = await article
  assert(
    !completed.error,
    `in-flight request failed during drain: ${completed.error?.message}`
  )
  assert.equal(minted(completed.value).length, 1)
  const delayedEc = minted(completed.value)[0]
  assert(
    verifiedPuts.some(
      (put) =>
        put.key === delayedEc && put.sequence < completed.value.responseSequence
    )
  )
  await bounded(
    async () => !(await state(app)).Running,
    7000,
    'current-thread runner drain deadline'
  )
  assert.equal((await state(app)).ExitCode, 0)
  proof.shutdownMs = Date.now() - shutdownStarted
  const finalLogs = await logs(app)
  assert.match(finalLogs, /phase=stopped/)
  record(
    'actual runner serves health and starts shutdown while storage waits, then drains normally'
  )
  assert(!proof.proxyFailure, `fixture proxy error: ${proof.proxyFailure}`)
  assert(!proof.originFailure, proof.originFailure)
  proof.outcome = 'pass'
} catch (error) {
  proof.outcome = 'fail'
  proof.failure = error.stack ?? error.message
  process.exitCode = 1
  console.error(`FAIL ${error.message}`)
} finally {
  fault = 'normal'
  for (const entry of [...pending]) entry.resolve()
  proof.logs = {}
  for (const app of [...ownedApps].reverse()) {
    try {
      await stop(app)
    } catch (error) {
      proof.cleanupErrors ??= []
      proof.cleanupErrors.push(`stop ${app.label}: ${error.message}`)
      try {
        await signal(app, 'SIGKILL')
      } catch {
        /* Already stopped. */
      }
    }
    try {
      proof.logs[app.label] = (await logs(app)).slice(-256 * 1024)
    } catch {
      /* Failed launch. */
    }
    if (app.name) {
      try {
        await docker('rm', '-f', app.name)
      } catch (error) {
        proof.cleanupErrors ??= []
        proof.cleanupErrors.push(`container ${app.label}: ${error.message}`)
      }
    }
  }
  for (const server of [proxy, origin]) {
    if (server) {
      server.closeAllConnections()
      await new Promise((resolve) => server.close(resolve))
    }
  }
  if (tableCreated) {
    try {
      await ddb('DeleteTable', { TableName: table })
    } catch (error) {
      proof.cleanupErrors ??= []
      proof.cleanupErrors.push(`local table: ${error.message}`)
    }
  }
  if (proof.cleanupErrors?.length) {
    proof.outcome = 'fail'
    process.exitCode = 1
  }
  proof.finishedAt = new Date().toISOString()
  try {
    await writeFile(output, JSON.stringify(proof, null, 2) + '\n', {
      mode: 0o600,
    })
    console.log(`Evidence: ${output}`)
  } catch (error) {
    process.exitCode = 1
    console.error(`Evidence write failed: ${error.code ?? error.message}`)
    console.log(JSON.stringify(proof))
  } finally {
    await rm(work, { recursive: true, force: true })
    process.off('SIGINT', interrupt)
    process.off('SIGTERM', interrupt)
  }
}
