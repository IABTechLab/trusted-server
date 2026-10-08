#!/usr/bin/env node
// Render Compose and exercise smoke command wiring without starting containers.
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { delimiter, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const example = fileURLToPath(new URL('../', import.meta.url))
const env = Object.fromEntries(
  ['PATH', 'HOME']
    .filter((key) => process.env[key] !== undefined)
    .map((key) => [key, process.env[key]])
)

// Inherit stderr so a failed Compose command explains why it failed.
function run(command, args, environment = env) {
  return execFileSync(command, args, {
    env: environment,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'inherit'],
  })
}

function renderProduction() {
  const compose = join(example, 'runtime/compose.yaml')
  // Older Compose releases stat env_file even with --no-env-resolution.
  // Check the deployment default separately and render with dummy values.
  assert(
    readFileSync(compose, 'utf8').includes(
      '${PBS_SECRET_ENV_FILE:-/run/pbs/secrets/examplebidder.env}'
    )
  )
  const service = JSON.parse(
    run('docker', ['compose', '-f', compose, 'config', '--format', 'json'], {
      ...env,
      PBS_SECRET_ENV_FILE: join(example, 'runtime/examples/pbs-secrets.env'),
    })
  ).services.pbs
  const port = service.ports[0]
  assert.equal(port.host_ip, '0.0.0.0')
  assert.equal(port.published, '8000')
  assert.equal(port.target, 8000)
}

function checkSmoke() {
  const directory = mkdtempSync(join(tmpdir(), 'pbs-smoke-wiring-'))
  try {
    writeFileSync(
      join(directory, 'docker'),
      `#!/usr/bin/env node
const assert = require('node:assert/strict')
const { execFileSync } = require('node:child_process')
const { appendFileSync, writeFileSync } = require('node:fs')
const args = process.argv.slice(2)
assert.equal(args[0], 'compose')
const operation = args[7]
assert(['up', 'logs', 'down'].includes(operation))
appendFileSync(process.env.CALLS, operation + '\\n')
if (operation === 'up') {
  const rendered = execFileSync('docker', [...args.slice(0, 7), 'config', '--format', 'json'], {
    env: { ...process.env, PATH: process.env.REAL_PATH },
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'inherit'],
  })
  writeFileSync(process.env.RENDERED, rendered)
}
`,
      { mode: 0o700 }
    )
    writeFileSync(
      join(directory, 'curl'),
      `#!/usr/bin/env node
const assert = require('node:assert/strict')
assert.equal(process.argv.at(-1), 'http://127.0.0.1:18081/status')
console.log('ok')
`,
      { mode: 0o700 }
    )
    run(join(example, 'scripts/smoke-runtime.sh'), [], {
      ...env,
      PATH: `${directory}${delimiter}${env.PATH}`,
      REAL_PATH: env.PATH,
      CALLS: join(directory, 'calls'),
      RENDERED: join(directory, 'rendered.json'),
      PBS_SMOKE_PORT: '18081',
      PBS_HOST_PORT: '19000',
      PBS_BIND_ADDRESS: '0.0.0.0',
      PBS_CONFIG_FILE: '/nonexistent/inherited-pbs.yaml',
      PBS_SECRET_ENV_FILE: '/nonexistent/inherited-secrets.env',
    })
    assert.deepEqual(
      readFileSync(join(directory, 'calls'), 'utf8').trim().split('\n'),
      ['up', 'logs', 'down']
    )
    const service = JSON.parse(
      readFileSync(join(directory, 'rendered.json'), 'utf8')
    ).services.pbs
    const port = service.ports[0]
    assert.equal(port.host_ip, '127.0.0.1')
    assert.equal(port.published, '18081')
    assert.equal(port.target, 8000)
    assert.equal(service.volumes[0].source, join(example, 'runtime/pbs.yaml'))
    assert.equal(
      service.environment.PBS_ADAPTERS_EXAMPLEBIDDER_API_KEY,
      'example-only-api-key'
    )
    assert.equal(
      service.environment.PBS_ADAPTERS_EXAMPLEBIDDER_OPTIONAL_TOKEN,
      'example-only-optional-token'
    )
  } finally {
    rmSync(directory, { recursive: true, force: true })
  }
}

JSON.parse(
  readFileSync(join(example, 'runtime/secret-bindings.example.json'), 'utf8')
)
renderProduction()
checkSmoke()
console.log(
  'Runtime JSON, Compose production/smoke bindings and dummy selectors passed; no containers started.'
)
