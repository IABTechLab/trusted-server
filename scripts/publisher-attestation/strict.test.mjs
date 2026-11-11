// Fixture admission probes, not a production verifier.
import assert from 'node:assert/strict'
import { createPublicKey, verify } from 'node:crypto'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const vectors = JSON.parse(
  readFileSync(
    new URL(
      '../../docs/superpowers/specs/fixtures/publisher-attestation-v1-vectors.json',
      import.meta.url
    )
  )
)
import { strictPoint } from './point-admission.mjs'
const prime = (1n << 255n) - 19n
const integer = (bytes) =>
  BigInt(`0x${Buffer.from(bytes).reverse().toString('hex')}`)

test('ordinary fixture keys pass canonical point and small-order checks', () => {
  for (const key of Object.values(vectors.public_keys))
    strictPoint(Buffer.from(key.jwk.x, 'base64url'))
})
test('noncanonical public-key fixture isolates encoding from small-order admission', () => {
  const key = vectors.negative_unsigned_objects.non_canonical_public_key.keys[0]
  const bytes = Buffer.from(key.jwk.x, 'base64url')
  assert.equal(integer(bytes), prime + 3n)
  assert.throws(() => strictPoint(bytes), /canonical/)
  const reduced = Buffer.alloc(32)
  reduced[0] = 3
  assert.doesNotThrow(() => strictPoint(reduced))
})
test('small-order discovery key fails admission', () => {
  const key =
    vectors.negative_unsigned_objects.small_order_key_discovery.keys[0]
  assert.throws(
    () => strictPoint(Buffer.from(key.jwk.x, 'base64url')),
    /small order/
  )
})
test('R fixtures exercise canonicality and small-order rejection separately', () => {
  const signature = (name) =>
    Buffer.from(
      vectors.negative_encodings[name].compact_jws.split('.')[2],
      'base64url'
    )
  assert.throws(
    () => strictPoint(signature('non_canonical_r').subarray(0, 32)),
    /canonical/
  )
  assert.throws(
    () => strictPoint(signature('small_order_r').subarray(0, 32)),
    /small order/
  )
  for (const object of Object.values(vectors.signed_objects))
    strictPoint(
      Buffer.from(object.signature_segment, 'base64url').subarray(0, 32)
    )
})
test('duplicate-member examples carry genuine signatures over the raw invalid JSON', () => {
  const key = createPublicKey({
    key: vectors.public_keys.publisher.jwk,
    format: 'jwk',
  })
  for (const name of [
    'duplicate_member_top_level',
    'duplicate_member_nested',
  ]) {
    const [header, payload, signature] =
      vectors.negative_encodings[name].compact_jws.split('.')
    assert.equal(
      verify(
        null,
        Buffer.from(`${header}.${payload}`),
        key,
        Buffer.from(signature, 'base64url')
      ),
      true
    )
    const raw = Buffer.from(payload, 'base64url').toString()
    // A permissive JSON parser accepts these. Admission must inspect original bytes.
    assert.doesNotThrow(() => JSON.parse(raw))
    assert.notEqual(JSON.stringify(JSON.parse(raw)), raw)
  }
})
