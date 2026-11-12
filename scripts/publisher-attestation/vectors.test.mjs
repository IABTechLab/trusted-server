import assert from 'node:assert/strict'
import { createHash, createPublicKey, verify } from 'node:crypto'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { generate } from './generate.mjs'

const vectors = JSON.parse(
  readFileSync(
    new URL(
      '../../docs/superpowers/specs/fixtures/publisher-attestation-v1-vectors.json',
      import.meta.url
    )
  )
)
const digest = (value) => createHash('sha256').update(value).digest('hex')
const canonical = (value) =>
  JSON.stringify(value, (_key, item) =>
    item && !Array.isArray(item) && typeof item === 'object'
      ? Object.fromEntries(
          Object.keys(item)
            .sort()
            .map((key) => [key, item[key]])
        )
      : item
  )

for (const [name, object] of Object.entries(vectors.signed_objects)) {
  test(`exact signed bytes and dependent hashes: ${name}`, () => {
    const [header, payload, signature] = object.compact_jws.split('.')
    assert.equal(
      Buffer.from(header, 'base64url').toString(),
      canonical(object.protected),
      `${name}: protected segment should match canonical header bytes`
    )
    assert.equal(
      Buffer.from(payload, 'base64url').toString(),
      canonical(object.payload),
      `${name}: payload segment should match canonical payload bytes`
    )
    assert.equal(
      object.signature_segment,
      signature,
      `${name}: signature_segment matches the compact signature, including S`
    )
    assert.equal(
      object.compact_sha256.value,
      digest(object.compact_jws),
      `${name}: compact_sha256 matches the compact JWS`
    )
    const role =
      object.payload.schema === 'ts-publisher-evidence-v1'
        ? 'publisher'
        : 'verifier'
    const key = createPublicKey({
      key:
        object.payload.schema === 'ts-publisher-evidence-v1'
          ? object.payload.binding.attestation_key.jwk
          : vectors.public_keys[role].jwk,
      format: 'jwk',
    })
    assert.equal(
      verify(
        null,
        Buffer.from(`${header}.${payload}`),
        key,
        Buffer.from(signature, 'base64url')
      ),
      true,
      `${name}: signature should verify over the original compact segments`
    )
    const binding = object.payload.binding
    if (binding) {
      assert.equal(
        object.payload.binding_id,
        digest(canonical(binding)),
        `${name}: binding_id should match the canonical binding digest`
      )
      assert.equal(
        binding.configuration.digest.value,
        digest(canonical(binding.configuration.projection)),
        `${name}: configuration digest should match the canonical projection`
      )
    }
  })
}
test('consumer challenges differ from the verifier collection', () => {
  const {
    evidence,
    collected_evidence: collected,
    assessment,
  } = vectors.signed_objects
  assert.notEqual(
    evidence.payload.nonce,
    collected.payload.nonce,
    'consumer and verifier collection nonces should differ'
  )
  assert.notEqual(
    evidence.compact_sha256.value,
    collected.compact_sha256.value,
    'separately challenged evidence should have distinct compact digests'
  )
  assert.equal(
    evidence.payload.binding_id,
    collected.payload.binding_id,
    'consumer and collection evidence should describe the same binding'
  )
  assert.equal(
    assessment.payload.evidence_digest.value,
    collected.compact_sha256.value,
    'assessment should reference the verifier-collected evidence digest'
  )
})
test('every accepted evidence case answers its configured consumer nonce', () => {
  for (const item of vectors.cases) {
    if (!item.expected.accept || !item.signed_object) continue
    const payload = vectors.signed_objects[item.signed_object].payload
    if (payload.schema === 'ts-publisher-evidence-v1')
      assert.equal(
        payload.nonce,
        item.context_overrides.nonce ?? vectors.external_context.nonce,
        `${item.id}: evidence should echo its configured consumer nonce`
      )
  }
})
test('generator loads normative policies rather than redefining them from fixtures', () => {
  const changed = structuredClone(vectors)
  changed.unsigned_objects.policy_publisher_declaration_v1.max_assessment_lifetime_seconds = 800
  const regenerated = generate(changed)
  assert.deepEqual(
    regenerated.unsigned_objects.policy_publisher_declaration_v1,
    vectors.unsigned_objects.policy_publisher_declaration_v1,
    'normative declaration policy must override altered fixture input'
  )
})
test('immutable assessment identities are unique', () => {
  const objects = Object.values(vectors.signed_objects).filter(
    (item) => item.payload.schema === 'ts-publisher-assessment-v1'
  )
  assert.equal(
    new Set(objects.map((item) => item.payload.assessment_id)).size,
    objects.length,
    'each immutable assessment should have a distinct assessment_id'
  )
})
test('every context override has a declared default', () => {
  const known = Object.keys(vectors.external_context)
  for (const item of vectors.cases)
    for (const key of Object.keys(item.context_overrides))
      assert.ok(known.includes(key), `${item.id}: ${key}`)
  assert.equal(
    'trusted_publisher_kid' in vectors.external_context,
    false,
    'consumer context should not supply an independently trusted publisher key'
  )
  for (const item of vectors.cases)
    assert.equal(
      typeof item.expected.stage,
      'number',
      `${item.id}: expected validation stage should be numeric`
    )
})
test('strict parsing negatives retain exact duplicate-member signed bytes', () => {
  for (const name of [
    'duplicate_member_top_level',
    'duplicate_member_nested',
  ]) {
    const raw = Buffer.from(
      vectors.negative_encodings[name].compact_jws.split('.')[1],
      'base64url'
    ).toString()
    assert.ok(
      name === 'duplicate_member_top_level'
        ? raw.includes('"extensions":{},"extensions":{}')
        : raw.includes('"host":"news.example.com","host":"news.example.com"'),
      `${name}: signed raw JSON should retain its own duplicate-member pattern`
    )
  }
})

test('enrollment TXT derives from the exact canonical challenge', () => {
  assert.equal(
    vectors.enrollment_txt_value,
    `ts-publisher-v1=${Buffer.from(digest(canonical(vectors.unsigned_objects.enrollment_challenge)), 'hex').toString('base64url')}`,
    'enrollment TXT must bind the complete canonical enrollment challenge'
  )
})

test('malformed operator identifier is the only invalid challenge field', () => {
  const negative = structuredClone(
    vectors.negative_unsigned_objects.non_canonical_operator_identifier
  )
  negative.operator_id =
    vectors.unsigned_objects.enrollment_challenge.operator_id
  assert.deepEqual(
    negative,
    vectors.unsigned_objects.enrollment_challenge,
    'repairing only operator_id should recover the valid enrollment challenge'
  )
})
