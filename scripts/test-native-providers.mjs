#!/usr/bin/env node
// Usage: node scripts/test-native-providers.mjs /absolute/path/to/trusted-server-axum
// Requires a prebuilt production binary with aws-appconfig-agent,aws-secrets-manager.
// No builds, external AWS, DynamoDB, or CLI-export acceptance are performed here.
// The ASCII fixture uses the existing BlobEnvelope v1 sorted compact JSON hash.
// CLI export/publication input acceptance remains a separate check.
import { spawn, execFile } from 'node:child_process'
import { createHash, randomUUID } from 'node:crypto'
import { access, mkdtemp, mkdir, writeFile, rename, rm } from 'node:fs/promises'
import http from 'node:http'
import net from 'node:net'
import { tmpdir } from 'node:os'
import { isAbsolute, join } from 'node:path'
import { setTimeout as delay } from 'node:timers/promises'
import { pathToFileURL } from 'node:url'
import { promisify } from 'node:util'

const exec = promisify(execFile)
export const images = {
  moto: 'ghcr.io/getmoto/motoserver@sha256:d8ae5edc2bf080e7e4c13f9bd4b29b53ac3b4427e92956318db3dbe23ec43eb7',
  agent:
    'public.ecr.aws/aws-appconfig/aws-appconfig-agent@sha256:cd604064652c56d9d6dc8435aa0d4bf3e90aa348830fa3b8a7b8650e3500ec3e',
}
const configId = 'trusted_server_config'
const secretId = 'trusted_server_secrets'
const token = 'synthetic-agent-token'
const keys = ['ec_key', 'proxy_key', 'admin_key']
const secretValues = (phase) => ({
  ec_key: `synthetic-ec-passphrase-32-bytes-minimum-${phase}`,
  proxy_key: `synthetic-proxy-secret-32-bytes-minimum-${phase}`,
  // A whole JSON document, including whitespace, must be the password.
  admin_key: ` { "password": "synthetic-admin-${phase}" }\n`,
})
// Only these application-owned assertions may reach diagnostics. Never print
// arbitrary subprocess, environment, SDK or HTTP error messages.
class FixtureAssertion extends Error {}
function requireCheck(condition, stage) {
  if (!condition) throw new FixtureAssertion(stage)
}
export function publishedEndpoint(address) {
  const match = /^127\.0\.0\.1:([0-9]+)$/.exec(address.trim())
  requireCheck(
    match && Number(match[1]) > 0 && Number(match[1]) <= 65535,
    'invalid published port'
  )
  return `http://127.0.0.1:${Number(match[1])}`
}
export function sorted(value) {
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
export function envelope(data) {
  const canonical = sorted(data)
  return JSON.stringify({
    data: canonical,
    sha256: createHash('sha256')
      .update(JSON.stringify(canonical))
      .digest('hex'),
    version: 1,
    generated_at: '2026-10-20T00:00:00Z',
  })
}
export function fixtureEnvelope(phase) {
  return envelope({
    ec: { passphrase: 'ec_key' },
    handlers: [
      { path: '^/_ts/admin', username: 'admin', password: 'admin_key' },
      { path: '^/health$', username: 'admin', password: 'admin_key' },
    ],
    publisher: {
      domain: 'publisher.example.com',
      cookie_domain: '.publisher.example.com',
      origin_url: 'https://origin.publisher.example.com',
      proxy_secret: 'proxy_key',
    },
    request_signing: null,
    response_headers: { 'x-provider-snapshot': phase },
  })
}
export function isolatedEnvironment(work, port, endpoints, bindings, phase) {
  const env = {
    HOME: work,
    AWS_CONFIG_FILE: join(work, 'aws-config'),
    AWS_SHARED_CREDENTIALS_FILE: join(work, 'aws-credentials'),
    AWS_EC2_METADATA_DISABLED: 'true',
    AWS_ACCESS_KEY_ID: 'DUMMYLOCALACCESS123',
    AWS_SECRET_ACCESS_KEY: 'DUMMYLOCALSECRET456',
    AWS_REGION: 'us-east-1',
    AWS_DEFAULT_REGION: 'us-east-1',
    AWS_ENDPOINT_URL: endpoints.moto,
    AWS_ENDPOINT_URL_SECRETS_MANAGER: endpoints.moto,
    AWS_IGNORE_CONFIGURED_ENDPOINT_URLS: 'false',
    APPCONFIG_AGENT_TOKEN: token,
    EDGEZERO__STORE_BINDINGS_FILE: bindings,
    EDGEZERO__ADAPTER__HOST: '127.0.0.1',
    EDGEZERO__ADAPTER__PORT: String(port),
    EDGEZERO__ADAPTER__CONFIG_DIR: work,
    EDGEZERO__ADAPTER__DATA_DIR: work,
    EDGEZERO__ADAPTER__SHUTDOWN_GRACE_SECONDS: '1',
    EDGEZERO__LOGGING__LEVEL: 'off',
    RUST_LOG: 'off',
    TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG:
      fixtureEnvelope(phase),
    TRUSTED_SERVER__EC__PASSPHRASE: 'poison',
    TRUSTED_SERVER__PUBLISHER__ORIGIN_URL: 'https://poison.example.com',
  }
  for (const [key, value] of Object.entries(secretValues(phase))) {
    // Valid local and legacy values deliberately cannot rescue selected providers.
    env[`EDGEZERO__SECRETS__${key.toUpperCase()}`] = value
    env[`TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_${key.toUpperCase()}`] =
      value
  }
  return env
}

// Direct HTTP only: no environment proxy lookup, redirects, or credential lookup.
async function request(
  endpoint,
  path,
  { body, headers = {}, method = 'GET' } = {}
) {
  return new Promise((resolve, reject) => {
    const req = http.request(
      `${endpoint}${path}`,
      { method, headers },
      (res) => {
        const chunks = []
        let size = 0
        res.on('data', (chunk) => {
          size += chunk.length
          if (size > 1024 * 1024) req.destroy(new Error('response limit'))
          else chunks.push(chunk)
        })
        res.on('error', () => reject(new Error('response read')))
        res.on('end', () =>
          resolve({
            status: res.statusCode,
            headers: res.headers,
            body: Buffer.concat(chunks).toString('utf8'),
          })
        )
      }
    )
    const timer = setTimeout(
      () => req.destroy(new Error('request deadline')),
      2500
    )
    req.on('close', () => clearTimeout(timer))
    req.on('error', () => reject(new Error('local request')))
    req.end(body)
  })
}
async function secrets(endpoint, operation, payload = {}) {
  const body = JSON.stringify(payload)
  const date = new Date().toISOString().replace(/[:-]|\.\d{3}/g, '')
  const response = await request(endpoint, '/', {
    method: 'POST',
    body,
    headers: {
      'Content-Type': 'application/x-amz-json-1.1',
      'Content-Length': Buffer.byteLength(body),
      'X-Amz-Target': `secretsmanager.${operation}`,
      'X-Amz-Date': date,
      Authorization: `AWS4-HMAC-SHA256 Credential=DUMMYLOCAL/${date.slice(0, 8)}/us-east-1/secretsmanager/aws4_request, SignedHeaders=host;x-amz-date, Signature=${'0'.repeat(64)}`,
    },
  })
  requireCheck(response.status === 200, 'fixture administration')
  try {
    return JSON.parse(response.body)
  } catch {
    throw new Error('fixture administration response')
  }
}
async function freePort() {
  const server = net.createServer()
  await new Promise((resolve, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', resolve)
  })
  const port = server.address().port
  await new Promise((resolve) => server.close(resolve))
  return port
}
async function listening(port) {
  return new Promise((resolve) => {
    const socket = net.connect({ host: '127.0.0.1', port })
    const finish = (value) => {
      socket.destroy()
      resolve(value)
    }
    socket.setTimeout(100, () => finish(false))
    socket.once('connect', () => finish(true))
    socket.once('error', () => finish(false))
  })
}
export async function stopChild(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return
  child.kill('SIGTERM')
  const deadline = Date.now() + 2000
  while (
    child.exitCode === null &&
    child.signalCode === null &&
    Date.now() < deadline
  )
    await delay(25)
  if (child.exitCode === null && child.signalCode === null)
    child.kill('SIGKILL')
  const killed = Date.now() + 2000
  while (
    child.exitCode === null &&
    child.signalCode === null &&
    Date.now() < killed
  )
    await delay(25)
  requireCheck(
    child.exitCode !== null || child.signalCode !== null,
    'child teardown deadline'
  )
}
function bindingsText(
  mode,
  endpoints,
  profile,
  secretArns,
  missingSecret = false
) {
  let text = 'version = 1\n'
  if (mode !== 'secrets')
    text += `[config.${configId}]\nprovider = "aws-appconfig-agent"\ndefault_key = "${configId}"\nendpoint = "${endpoints.agent}"\naccess_token_env = "APPCONFIG_AGENT_TOKEN"\n[config.${configId}.documents.${configId}]\napplication = "ts"\nenvironment = "test"\nprofile = "${profile}"\n`
  if (mode !== 'config') {
    text += `[secrets.${secretId}]\nprovider = "aws-secrets-manager"\nregion = "us-east-1"\n`
    for (const key of keys) {
      const arn = secretArns[key]
      requireCheck(
        typeof arn === 'string' &&
          arn.startsWith('arn:aws:secretsmanager:us-east-1:'),
        'synthetic secret ARN mapping'
      )
      const target =
        missingSecret && key === 'admin_key'
          ? arn.replace(/:secret:.+$/, ':secret:missing-A1b2c3')
          : arn
      text += `[secrets.${secretId}.entries.${key}]\nsecret_id = "${target}"\n`
    }
  }
  return text
}
async function publish(path, contents) {
  await writeFile(`${path}.next`, contents)
  await rename(`${path}.next`, path)
}

export async function main(binary) {
  requireCheck(
    typeof binary === 'string' && isAbsolute(binary),
    'supply absolute prebuilt binary path'
  )
  await access(binary)
  const work = await mkdtemp(join(tmpdir(), 'ts-native-providers-'))
  const prefix = `ts-providers-${randomUUID()}`
  const names = { moto: `${prefix}-moto`, agent: `${prefix}-agent` }
  const configs = join(work, 'configs')
  const children = new Set()
  const commands = new Set()
  const created = new Set()
  let interrupted = false
  let stage = 'setup'
  let failed = false
  let cleaning = false
  let interruptTimer
  const interrupt = () => {
    if (interrupted) return
    interrupted = true
    if (!cleaning) for (const command of commands) command.abort()
    for (const child of children) child.kill('SIGTERM')
    interruptTimer = setTimeout(() => {
      for (const child of children) {
        if (child.exitCode === null && child.signalCode === null)
          child.kill('SIGKILL')
      }
    }, 2000)
  }
  process.on('SIGINT', interrupt)
  process.on('SIGTERM', interrupt)
  const check = () => requireCheck(!interrupted, 'interrupted')
  // Do not inherit AWS, proxies, Docker config credentials, or arbitrary endpoints.
  const dockerEnv = { PATH: process.env.PATH, HOME: work }
  const docker = async (args, timeout = 30_000) => {
    const controller = new AbortController()
    commands.add(controller)
    try {
      const { stdout } = await exec('docker', args, {
        env: dockerEnv,
        timeout,
        signal: controller.signal,
        killSignal: 'SIGKILL',
        maxBuffer: 1024 * 1024,
      })
      return stdout.trim()
    } catch {
      throw new Error('container command')
    } finally {
      commands.delete(controller)
    }
  }
  const launch = async (directory, endpoints, bindingPath, phase) => {
    check()
    const port = await freePort()
    const child = spawn(binary, [], {
      cwd: directory,
      env: isolatedEnvironment(directory, port, endpoints, bindingPath, phase),
      stdio: 'ignore',
    })
    let spawnFailed = false
    child.on('error', () => {
      spawnFailed = true
    })
    children.add(child)
    return {
      child,
      port,
      endpoint: `http://127.0.0.1:${port}`,
      spawnFailed: () => spawnFailed,
    }
  }
  const stop = async (app) => {
    await stopChild(app.child)
    children.delete(app.child)
  }
  const health = (app, password) =>
    request(app.endpoint, '/health', {
      headers: {
        Authorization: `Basic ${Buffer.from(`admin:${password}`).toString('base64')}`,
      },
    })
  const ready = async (app, phase) => {
    const until = Date.now() + 20_000
    while (Date.now() < until) {
      check()
      requireCheck(
        !app.spawnFailed() &&
          app.child.exitCode === null &&
          app.child.signalCode === null,
        'startup exited'
      )
      let response
      try {
        response = await health(app, secretValues(phase).admin_key)
      } catch {
        /* Retry connection refusal, not a wrong HTTP response. */
      }
      if (response) {
        requireCheck(
          response.status === 200 &&
            response.body === 'ok' &&
            response.headers['x-ts-version'] &&
            response.headers['x-provider-snapshot'] === phase,
          'health snapshot readiness'
        )
        return
      }
      await delay(100)
    }
    throw new Error('startup deadline')
  }
  const snapshot = async (app, configPhase, secretPhase, rejectedPhase) => {
    const response = await health(app, secretValues(secretPhase).admin_key)
    requireCheck(
      response.status === 200 &&
        response.body === 'ok' &&
        response.headers['x-provider-snapshot'] === configPhase,
      'snapshot observation'
    )
    requireCheck(
      (await health(app, secretValues(rejectedPhase).admin_key)).status === 401,
      'old or future secret rejection'
    )
    requireCheck(
      (await health(app, `synthetic-admin-${secretPhase}`)).status === 401,
      'whole secret semantics'
    )
    const js = await request(app.endpoint, '/static/tsjs=0000000000000000')
    requireCheck(
      js.status === 404 &&
        js.headers['x-geo-info-available'] === 'false' &&
        js.headers['x-provider-snapshot'] === configPhase,
      'TSJS route observation'
    )
  }
  const rejected = async (app) => {
    const until = Date.now() + 20_000
    while (Date.now() < until) {
      check()
      requireCheck(!(await listening(app.port)), 'invalid startup listened')
      requireCheck(!app.spawnFailed(), 'binary launch')
      if (app.child.exitCode !== null || app.child.signalCode !== null) {
        requireCheck(
          app.child.exitCode !== 0 && app.child.signalCode === null,
          'invalid startup exit'
        )
        return
      }
      await delay(25)
    }
    throw new Error('invalid startup deadline')
  }
  try {
    await mkdir(configs)
    await writeFile(join(work, 'aws-config'), '')
    await writeFile(join(work, 'aws-credentials'), '')
    await docker(['info'], 20_000)
    for (const image of Object.values(images)) {
      check()
      await docker(['pull', image], 180_000)
    }
    for (const service of ['moto', 'agent']) {
      check()
      created.add(names[service])
      const args = [
        'run',
        '-d',
        '--name',
        names[service],
        '--publish',
        `127.0.0.1::${service === 'moto' ? 5000 : 2772}`,
      ]
      if (service === 'agent')
        args.push(
          '--mount',
          `type=bind,source=${configs},target=/configs,readonly`,
          '--env',
          'LOCAL_DEVELOPMENT_DIRECTORY=/configs',
          '--env',
          'SERVICE_REGION=us-east-1',
          '--env',
          'AWS_ACCESS_KEY_ID=DUMMYLOCAL',
          '--env',
          'AWS_SECRET_ACCESS_KEY=DUMMYLOCAL',
          '--env',
          'AWS_EC2_METADATA_DISABLED=true',
          '--env',
          `ACCESS_TOKEN=${token}`,
          '--env',
          'HTTP_HOST=0.0.0.0',
          '--env',
          'POLL_INTERVAL=1s'
        )
      args.push(images[service])
      await docker(args)
    }
    const endpoints = {}
    for (const [service, port] of [
      ['moto', 5000],
      ['agent', 2772],
    ])
      endpoints[service] = publishedEndpoint(
        await docker(['port', names[service], `${port}/tcp`])
      )
    stage = 'service readiness'
    await publish(join(configs, 'probe:test:ready'), 'agent-ready\n')
    let servicesReady = false
    const deadline = Date.now() + 60_000
    while (Date.now() < deadline) {
      check()
      try {
        await secrets(endpoints.moto, 'ListSecrets')
        const probe = await request(
          endpoints.agent,
          '/applications/probe/environments/test/configurations/ready',
          { headers: { Authorization: `Bearer ${token}` } }
        )
        servicesReady = probe.status === 200 && probe.body === 'agent-ready\n'
        if (servicesReady) break
      } catch {
        /* Bounded service startup retry. */
      }
      await delay(200)
    }
    requireCheck(servicesReady, 'service readiness deadline')
    const secretArns = {}
    for (const [key, value] of Object.entries(secretValues('A'))) {
      const createdSecret = await secrets(endpoints.moto, 'CreateSecret', {
        Name: key,
        SecretString: value,
      })
      secretArns[key] = createdSecret.ARN
    }
    for (const mode of ['config', 'secrets', 'combined']) {
      stage = `${mode} snapshots`
      check()
      const directory = join(work, mode)
      await mkdir(directory)
      await writeFile(join(directory, 'aws-config'), '')
      await writeFile(join(directory, 'aws-credentials'), '')
      const bindingPath = join(directory, 'bindings.toml')
      const localPath = join(directory, `local-config-${configId}.json`)
      const document = join(configs, `ts:test:${mode}`)
      const publishPhase = async (phase) => {
        await publish(document, fixtureEnvelope(phase))
        await publish(
          localPath,
          JSON.stringify({ [configId]: fixtureEnvelope(phase) })
        )
        for (const [key, value] of Object.entries(secretValues(phase))) {
          await secrets(endpoints.moto, 'PutSecretValue', {
            SecretId: key,
            SecretString: value,
            ClientRequestToken: randomUUID(),
          })
          const actual = await secrets(endpoints.moto, 'GetSecretValue', {
            SecretId: key,
          })
          requireCheck(
            actual.SecretString === value,
            'independent secret readback'
          )
        }
      }
      await publishPhase('A')
      await writeFile(
        bindingPath,
        bindingsText(mode, endpoints, mode, secretArns)
      )
      const app = await launch(directory, endpoints, bindingPath, 'A')
      await ready(app, 'A')
      await snapshot(app, 'A', 'A', 'B')
      await publishPhase('B')
      // Wait for the independent Agent cache, not only the mounted file write.
      const until = Date.now() + 15_000
      let adopted = false
      while (Date.now() < until) {
        check()
        const response = await request(
          endpoints.agent,
          `/applications/ts/environments/test/configurations/${mode}`,
          { headers: { Authorization: `Bearer ${token}` } }
        )
        adopted =
          response.status === 200 && response.body === fixtureEnvelope('B')
        if (adopted) break
        await delay(200)
      }
      requireCheck(adopted, 'independent config readback')
      await snapshot(app, 'A', 'A', 'B')
      await stop(app)
      const restarted = await launch(directory, endpoints, bindingPath, 'B')
      await ready(restarted, 'B')
      await snapshot(restarted, 'B', 'B', 'A')
      await stop(restarted)
      console.log(
        `PASS ${mode}: real TS health/auth/TSJS; immutable snapshots; restart adoption`
      )
      if (mode !== 'secrets') {
        stage = `${mode} config failure`
        await writeFile(
          bindingPath,
          bindingsText(mode, endpoints, 'missing', secretArns)
        )
        const invalid = await launch(directory, endpoints, bindingPath, 'B')
        await rejected(invalid)
        await stop(invalid)
        console.log(
          `PASS ${mode}: selected config failure before listen, no local/legacy fallback`
        )
      }
      if (mode !== 'config') {
        stage = `${mode} secret failure`
        await writeFile(
          bindingPath,
          bindingsText(mode, endpoints, mode, secretArns, true)
        )
        const invalid = await launch(directory, endpoints, bindingPath, 'B')
        await rejected(invalid)
        await stop(invalid)
        console.log(
          `PASS ${mode}: selected secret failure before listen, no local/legacy fallback`
        )
      }
    }
  } catch (error) {
    // stage and assertion messages are application-owned bounded labels.
    const reason =
      error instanceof FixtureAssertion ? error.message : 'operation failed'
    console.error(`FAIL provider acceptance stage: ${stage}; ${reason}`)
    failed = true
  } finally {
    cleaning = true
    for (const child of children) {
      try {
        await stopChild(child)
      } catch {
        failed = true
      }
    }
    for (const name of created) {
      try {
        await docker(['rm', '-f', name], 10_000)
      } catch {
        failed = true
      }
    }
    try {
      await rm(work, { recursive: true, force: true })
    } catch {
      failed = true
    }
    clearTimeout(interruptTimer)
    process.off('SIGINT', interrupt)
    process.off('SIGTERM', interrupt)
  }
  // Never print child output, arbitrary caught errors, endpoint bytes, or values.
  requireCheck(
    !failed && !interrupted,
    `provider acceptance failed during ${stage}; child/provider diagnostics suppressed`
  )
  console.log(
    'PASS provider acceptance; CLI-export acceptance and DynamoDB are not covered'
  )
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  main(process.argv[2]).catch(() => {
    console.error(
      'Native provider acceptance failed; check binary/features, Docker availability, and bounded stage output. Raw diagnostics suppressed.'
    )
    process.exitCode = 1
  })
}
