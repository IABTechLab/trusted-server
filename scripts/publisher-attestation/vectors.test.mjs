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
      canonical(object.protected)
    )
    assert.equal(
      Buffer.from(payload, 'base64url').toString(),
      canonical(object.payload)
    )
    assert.equal(object.compact_sha256.value, digest(object.compact_jws))
    const role =
      object.payload.schema === 'ts-publisher-evidence-v1'
        ? 'publisher'
        : 'verifier'
    const key = createPublicKey({
      key: vectors.public_keys[role].jwk,
      format: 'jwk',
    })
    assert.equal(
      verify(
        null,
        Buffer.from(`${header}.${payload}`),
        key,
        Buffer.from(signature, 'base64url')
      ),
      true
    )
    const binding = object.payload.binding
    if (binding) {
      assert.equal(object.payload.binding_id, digest(canonical(binding)))
      assert.equal(
        binding.configuration.digest.value,
        digest(canonical(binding.configuration.projection))
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
  assert.notEqual(evidence.payload.nonce, collected.payload.nonce)
  assert.notEqual(evidence.compact_sha256.value, collected.compact_sha256.value)
  assert.equal(evidence.payload.binding_id, collected.payload.binding_id)
  assert.equal(
    assessment.payload.evidence_digest.value,
    collected.compact_sha256.value
  )
})
test('every accepted evidence case answers its configured consumer nonce', () => {
  for (const item of vectors.cases) {
    if (!item.expected.accept || !item.signed_object) continue
    const payload = vectors.signed_objects[item.signed_object].payload
    if (payload.schema === 'ts-publisher-evidence-v1')
      assert.equal(
        payload.nonce,
        item.context_overrides.nonce ?? vectors.external_context.nonce
      )
  }
})
test('generator propagates edited policy digests through assessments and trust pins', () => {
  const changed = structuredClone(vectors)
  changed.unsigned_objects.policy_publisher_declaration_v1.max_assessment_lifetime_seconds = 800
  const regenerated = generate(changed)
  const expected = digest(
    canonical(regenerated.unsigned_objects.policy_publisher_declaration_v1)
  )
  assert.equal(
    regenerated.signed_objects.assessment.payload.policy.digest.value,
    expected
  )
  assert.equal(
    regenerated.external_context.trusted_verifier.pinned_policy_digests[
      'publisher-declaration-v1'
    ],
    expected
  )
})
test('immutable assessment identities are unique', () => {
  const objects = Object.values(vectors.signed_objects).filter(
    (item) => item.payload.schema === 'ts-publisher-assessment-v1'
  )
  assert.equal(
    new Set(objects.map((item) => item.payload.assessment_id)).size,
    objects.length
  )
})
test('every context override has a declared default', () => {
  const known = Object.keys(vectors.external_context)
  for (const item of vectors.cases)
    for (const key of Object.keys(item.context_overrides))
      assert.ok(known.includes(key), `${item.id}: ${key}`)
  assert.equal('trusted_publisher_kid' in vectors.external_context, false)
  for (const item of vectors.cases)
    assert.equal(typeof item.expected.stage, 'number')
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
      raw.includes('"extensions":{},"extensions":{}') ||
        raw.includes('"host":"news.example.com","host":"news.example.com"')
    )
  }
})
