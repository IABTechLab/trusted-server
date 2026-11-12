// Fixture admission probes, not a production verifier.
import assert from 'node:assert/strict'
import { createHash, createPublicKey, verify } from 'node:crypto'
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
  assert.equal(integer(bytes), prime + 3n, 'should equal for integer(bytes)')
  assert.throws(
    () => strictPoint(bytes),
    /canonical/,
    'should throws for () => strictPoint(bytes)'
  )
  const reduced = Buffer.alloc(32)
  reduced[0] = 3
  assert.doesNotThrow(
    () => strictPoint(reduced),
    'should doesNotThrow for () => strictPoint(reduced)'
  )
})
test('small-order discovery key fails admission', () => {
  const key =
    vectors.negative_unsigned_objects.small_order_key_discovery.keys[0]
  assert.throws(
    () => strictPoint(Buffer.from(key.jwk.x, 'base64url')),
    /small order/,
    "should throws for () => strictPoint(Buffer.from(key.jwk.x, 'base64url'))"
  )
})
test('R fixtures exercise overlapping canonicality and small-order rejection', () => {
  const signature = (name) =>
    Buffer.from(
      vectors.negative_encodings[name].compact_jws.split('.')[2],
      'base64url'
    )
  assert.throws(
    () => strictPoint(signature('non_canonical_r').subarray(0, 32)),
    /canonical/,
    "should throws for () => strictPoint(signature('non_canonical_r').subarray(0, 32))"
  )
  assert.throws(
    () => strictPoint(signature('small_order_r').subarray(0, 32)),
    /small order/,
    "should throws for () => strictPoint(signature('small_order_r').subarray(0, 32))"
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
      true,
      "should equal for verify( null, Buffer.from(`${header}.${payload}`), key, Buffer.from(signature, 'base64url') )"
    )
    const raw = Buffer.from(payload, 'base64url').toString()
    // A permissive JSON parser accepts these. Admission must inspect original bytes.
    assert.doesNotThrow(
      () => JSON.parse(raw),
      'should doesNotThrow for () => JSON.parse(raw)'
    )
    assert.notEqual(
      JSON.stringify(JSON.parse(raw)),
      raw,
      'should notEqual for JSON.stringify(JSON.parse(raw))'
    )
  }
})
test('point admission requires exactly 32 bytes', () => {
  const valid = Buffer.from(vectors.public_keys.publisher.jwk.x, 'base64url')
  for (const bytes of [
    Buffer.alloc(0),
    valid.subarray(0, 31),
    Buffer.concat([valid, Buffer.alloc(1)]),
    Buffer.concat([valid, Buffer.alloc(3)]),
  ])
    assert.throws(
      () => strictPoint(bytes),
      /32 bytes/,
      `${bytes.length}-byte point must reject before arithmetic`
    )
})

// Deliberately relaxed affine equation, used only to distinguish these test vectors
// from arbitrary signature tampering. This is not a production verifier.
const mod = (value) => ((value % prime) + prime) % prime
const power = (value, exponent) => {
  let result = 1n
  while (exponent) {
    if (exponent & 1n) result = mod(result * value)
    value = mod(value * value)
    exponent >>= 1n
  }
  return result
}
const inverse = (value) => power(value, prime - 2n)
const curve = mod(-121665n * inverse(121666n))
function relaxedPoint(bytes) {
  const encoded = integer(bytes)
  const y = mod(encoded & ((1n << 255n) - 1n))
  const square = mod((y * y - 1n) * inverse(curve * y * y + 1n))
  let x = power(square, (prime + 3n) / 8n)
  if (mod(x * x) !== square) x = mod(x * power(2n, (prime - 1n) / 4n))
  assert.equal(mod(x * x), square, 'relaxed test point must lie on the curve')
  if ((x & 1n) !== encoded >> 255n) x = mod(-x)
  return [x, y]
}
function add([x, y], [u, v]) {
  const product = mod(curve * x * u * y * v)
  return [
    mod((x * v + y * u) * inverse(1n + product)),
    mod((y * v + x * u) * inverse(1n - product)),
  ]
}
function multiply(point, scalar) {
  let result = [0n, 1n]
  while (scalar) {
    if (scalar & 1n) result = add(result, point)
    point = add(point, point)
    scalar >>= 1n
  }
  return result
}
test('identity R negatives satisfy the relaxed cofactorless equation', () => {
  const base = relaxedPoint(
    Buffer.from(
      '5866666666666666666666666666666666666666666666666666666666666666',
      'hex'
    )
  )
  const key = Buffer.from(vectors.public_keys.publisher.jwk.x, 'base64url')
  for (const name of ['small_order_r', 'non_canonical_r']) {
    const [header, payload, encoded] =
      vectors.negative_encodings[name].compact_jws.split('.')
    const signature = Buffer.from(encoded, 'base64url')
    const r = signature.subarray(0, 32)
    const challenge =
      integer(
        createHash('sha512')
          .update(Buffer.concat([r, key, Buffer.from(`${header}.${payload}`)]))
          .digest()
      ) %
      ((1n << 252n) + 27742317777372353535851937790883648493n)
    assert.deepEqual(
      multiply(base, integer(signature.subarray(32))),
      add(relaxedPoint(r), multiply(relaxedPoint(key), challenge)),
      `${name}: relaxed point comparison must accept, so this is not ordinary signature tampering`
    )
  }
})
