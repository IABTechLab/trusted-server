import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import test from 'node:test'

const require = createRequire(
  new URL('../../docs/package.json', import.meta.url)
)
const Ajv = require('ajv')
const schema = JSON.parse(
  readFileSync(
    new URL(
      '../../docs/superpowers/specs/schemas/publisher-attestation-v1.schema.json',
      import.meta.url
    )
  )
)
const vectors = JSON.parse(
  readFileSync(
    new URL(
      '../../docs/superpowers/specs/fixtures/publisher-attestation-v1-vectors.json',
      import.meta.url
    )
  )
)
const ajv = new Ajv({ allErrors: true, schemaId: 'auto' })
ajv.addSchema(schema, 'attestation')
const validate = (name, value) =>
  ajv.validate({ $ref: `attestation#/definitions/${name}` }, value)

for (const [name, object] of Object.entries(vectors.signed_objects)) {
  test(`signed fixture schema: ${name}`, () => {
    assert.ok(
      ajv.validate('attestation', object.protected),
      JSON.stringify(ajv.errors)
    )
    assert.ok(
      ajv.validate('attestation', object.payload),
      JSON.stringify(ajv.errors)
    )
  })
}
for (const [name, object] of Object.entries(vectors.unsigned_objects)) {
  test(`unsigned fixture schema: ${name}`, () =>
    assert.ok(ajv.validate('attestation', object), JSON.stringify(ajv.errors)))
}

for (const host of [
  '127.0.0.1',
  '0x7f.0.0.1',
  '0x7f000001',
  '1.2.3.0x4',
  'example.123',
  'example.0x',
  '1.2.3.4.5',
]) {
  test(`reject numeric host ${host}`, () => {
    assert.equal(validate('host', host), false)
    assert.equal(validate('httpsIdentifier', `https://${host}/path`), false)
  })
}
for (const host of [
  'news.example.com',
  'xn--nxasmq6b.example',
  `${'a'.repeat(63)}.example`,
]) {
  test(`preserve DNS host ${host}`, () => {
    assert.equal(validate('host', host), true)
    assert.equal(validate('httpsIdentifier', `https://${host}/path`), true)
  })
}
test('reject identifier dot segments', () => {
  for (const path of ['/./x', '/x/..', '/../x'])
    assert.equal(
      validate('httpsIdentifier', `https://example.com${path}`),
      false
    )
})
test('fingerprint requires a known staging flag', () => {
  const assessment = structuredClone(
    vectors.signed_objects.assessment_fingerprint_only.payload
  )
  assessment.binding.platform.staging = null
  assert.equal(validate('assessment', assessment), false)
})
for (const [staging, environment, valid] of [
  [true, 'production', false],
  [false, 'staging', false],
  [true, 'staging', true],
  [false, 'production', true],
  [true, 'development', true],
  [false, 'development', true],
]) {
  test(`Fastly staging=${staging}, environment=${environment}`, () => {
    const binding = structuredClone(
      vectors.signed_objects.assessment_fingerprint_only.payload.binding
    )
    binding.platform.staging = staging
    binding.environment = environment
    assert.equal(validate('binding', binding), valid)
  })
}
test('Axum is development only', () => {
  const binding = structuredClone(
    vectors.signed_objects.evidence.payload.binding
  )
  binding.platform = {
    provider: 'axum',
    service_id: null,
    service_version: null,
    staging: null,
  }
  binding.environment = 'production'
  assert.equal(validate('binding', binding), false)
  binding.environment = 'development'
  assert.equal(validate('binding', binding), true)
})
