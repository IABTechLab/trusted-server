import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { once } from 'node:events'
import { test } from 'node:test'
import {
  envelope,
  fixtureEnvelope,
  images,
  isolatedEnvironment,
  publishedEndpoint,
  sorted,
  stopChild,
} from '../test-native-providers.mjs'

test('published ports must be exclusively literal loopback and errors omit input', () => {
  assert.equal(publishedEndpoint('127.0.0.1:12345\n'), 'http://127.0.0.1:12345')
  for (const input of [
    '127.0.0.1:0',
    '127.0.0.1:65536',
    '0.0.0.0:12345',
    '[::]:12345',
    'https://secret.example.com',
    '127.0.0.1:80\n[::]:80',
  ]) {
    assert.throws(() => publishedEndpoint(input), {
      message: 'invalid published port',
    })
  }
})

test("existing TS startup fixture's BlobEnvelope hash matches Node serialization", () => {
  const data = {
    ec: { passphrase: 'ec_key' },
    handlers: [
      { password: 'admin_key', path: '^/_ts/admin', username: 'admin' },
    ],
    publisher: {
      cookie_domain: '.publisher.example.com',
      domain: 'publisher.example.com',
      origin_url: 'https://origin.publisher.example.com',
      proxy_secret: 'proxy_key',
    },
    request_signing: null,
  }
  // Exact golden vector from crates/trusted-server-adapter-axum/tests/startup.rs.
  assert.equal(
    JSON.parse(envelope(data)).sha256,
    'f4dc03017deb24bfd589efde7dce0f9f8a20d952c28ebcbdb6141b08661cd137'
  )
  assert.deepEqual(sorted({ b: [{ z: 1, a: 2 }], a: null }), {
    a: null,
    b: [{ a: 2, z: 1 }],
  })
  const fixture = JSON.parse(fixtureEnvelope('A'))
  assert.equal(fixture.version, 1)
  assert.equal(fixture.data.ec.passphrase, 'ec_key')
  assert.equal(fixture.data.handlers[1].password, 'admin_key')
  assert.equal(fixture.data.publisher.proxy_secret, 'proxy_key')
  assert.equal(fixture.data.response_headers['x-provider-snapshot'], 'A')
})

test('binary environment never inherits real credentials, proxy settings, or endpoints', () => {
  const previous = process.env.AWS_SESSION_TOKEN
  process.env.AWS_SESSION_TOKEN = 'sentinel-not-to-be-inherited'
  try {
    const env = isolatedEnvironment(
      '/tmp/synthetic',
      12345,
      { moto: 'http://127.0.0.1:12346' },
      '/tmp/synthetic/bindings.toml',
      'A'
    )
    assert.equal(env.AWS_SESSION_TOKEN, undefined)
    assert.equal(env.HTTP_PROXY, undefined)
    assert.equal(env.PATH, undefined)
    assert.equal(env.AWS_EC2_METADATA_DISABLED, 'true')
    assert.equal(env.AWS_ACCESS_KEY_ID, 'DUMMYLOCALACCESS123')
    assert.equal(env.AWS_ENDPOINT_URL_SECRETS_MANAGER, 'http://127.0.0.1:12346')
    assert.equal(env.HOME, '/tmp/synthetic')
    assert.equal(
      env.AWS_SHARED_CREDENTIALS_FILE,
      '/tmp/synthetic/aws-credentials'
    )
    assert.ok(env.EDGEZERO__SECRETS__EC_KEY.length >= 32)
    assert.ok(env.EDGEZERO__SECRETS__ADMIN_KEY.startsWith(' { "password":'))
    assert.ok(!JSON.stringify(env).includes('sentinel-not-to-be-inherited'))
  } finally {
    if (previous === undefined) delete process.env.AWS_SESSION_TOKEN
    else process.env.AWS_SESSION_TOKEN = previous
  }
})

test('only the two independently pinned provider services are selected', () => {
  assert.deepEqual(Object.keys(images).sort(), ['agent', 'moto'])
  for (const image of Object.values(images))
    assert.match(image, /@sha256:[a-f0-9]{64}$/)
})

test(
  'child teardown is idempotent and escalates for an ignored SIGTERM',
  { timeout: 7000 },
  async () => {
    const child = spawn(
      process.execPath,
      [
        '-e',
        'process.on("SIGTERM", () => {}); console.log("ready"); setInterval(() => {}, 1000);',
      ],
      { env: {}, stdio: ['ignore', 'pipe', 'ignore'] }
    )
    try {
      await once(child.stdout, 'data')
      await stopChild(child)
      assert.equal(child.signalCode, 'SIGKILL')
      await stopChild(child)
    } finally {
      if (child.exitCode === null && child.signalCode === null)
        child.kill('SIGKILL')
    }
  }
)
