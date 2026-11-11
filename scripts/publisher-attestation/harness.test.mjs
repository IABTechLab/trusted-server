// Bounded synthetic case runner. This is not the production consumer or collector.
import assert from 'node:assert/strict'
import { createPublicKey, verify } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import test from 'node:test'
import { canonical, hash } from './generate.mjs'
import { strictPoint } from './point-admission.mjs'

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
const ajv = new Ajv({ allErrors: true })
ajv.addSchema(schema, 'attestation')
const reject = (stage, reason_code) => {
  throw { accept: false, stage, reason_code }
}
function merge(target, patch) {
  if (!patch || typeof patch !== 'object' || Array.isArray(patch))
    return structuredClone(patch)
  const result =
    target && typeof target === 'object' && !Array.isArray(target)
      ? structuredClone(target)
      : {}
  for (const [key, value] of Object.entries(patch)) {
    if (value === null) delete result[key]
    else result[key] = merge(result[key], value)
  }
  return result
}
function parse(raw) {
  // Walk tokens before JSON.parse so duplicate members survive long enough to reject.
  const tokens =
    raw.match(
      /"(?:[^"\\]|\\.)*"|[{}\[\]:,]|true|false|null|-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?/g
    ) ?? []
  let index = 0
  function value(depth = 0) {
    if (depth > 16) reject(1, 'malformed')
    const token = tokens[index++]
    if (token === '{') {
      const keys = new Set()
      while (tokens[index] !== '}') {
        const key = JSON.parse(tokens[index++])
        if (keys.has(key)) reject(1, 'malformed')
        keys.add(key)
        assert.equal(tokens[index++], ':')
        value(depth + 1)
        if (tokens[index] !== ',') break
        index++
      }
      assert.equal(tokens[index++], '}')
    } else if (token === '[') {
      while (tokens[index] !== ']') {
        value(depth + 1)
        if (tokens[index] !== ',') break
        index++
      }
      assert.equal(tokens[index++], ']')
    }
  }
  value()
  return JSON.parse(raw)
}
function admittedKey(key, stage = 4) {
  try {
    strictPoint(Buffer.from(key.jwk.x, 'base64url'))
  } catch {
    reject(stage, 'malformed')
  }
  assert.equal(
    key.kid,
    Buffer.from(hash(canonical(key.jwk)), 'hex').toString('base64url')
  )
}
function decoded(compact, context, expectedType) {
  const segments = compact.split('.')
  if (segments.length !== 3 || compact.length > 32768) reject(1, 'malformed')
  const [headerSegment, payloadSegment, signatureSegment] = segments
  for (const segment of segments)
    if (Buffer.from(segment, 'base64url').toString('base64url') !== segment)
      reject(1, 'malformed')
  const header = parse(Buffer.from(headerSegment, 'base64url').toString())
  const payload = parse(Buffer.from(payloadSegment, 'base64url').toString())
  const evidence = payload.schema === 'ts-publisher-evidence-v1'
  const type =
    expectedType ??
    (evidence
      ? 'ts-publisher-evidence+jws'
      : payload.schema === 'ts-publisher-status-v1'
        ? 'ts-publisher-status+jws'
        : 'ts-publisher-assessment+jws')
  if (!ajv.validate('attestation', header) || header.typ !== type)
    reject(2, 'signature_invalid')
  if (!evidence && payload.issuer !== context.trusted_verifier.issuer)
    reject(2, 'policy_untrusted')
  const key = evidence
    ? payload.binding.attestation_key
    : vectors.public_keys.verifier
  admittedKey(key)
  if (header.kid !== key.kid) reject(2, 'signature_invalid')
  const signature = Buffer.from(signatureSegment, 'base64url')
  try {
    strictPoint(signature.subarray(0, 32))
  } catch {
    reject(3, 'signature_invalid')
  }
  const scalar = BigInt(
    `0x${Buffer.from(signature.subarray(32)).reverse().toString('hex')}`
  )
  if (scalar >= (1n << 252n) + 27742317777372353535851937790883648493n)
    reject(3, 'signature_invalid')
  if (
    !verify(
      null,
      Buffer.from(`${headerSegment}.${payloadSegment}`),
      createPublicKey({ key: key.jwk, format: 'jwk' }),
      signature
    )
  )
    reject(3, 'signature_invalid')
  const platform = payload.binding?.platform
  if (
    platform?.provider === 'fastly' &&
    ((platform.staging === true &&
      payload.binding.environment === 'production') ||
      (platform.staging === false && payload.binding.environment === 'staging'))
  )
    reject(4, 'environment_mismatch')
  if (!ajv.validate('attestation', payload)) reject(4, 'malformed')
  if (payload.binding) {
    if (payload.binding_id !== hash(canonical(payload.binding)))
      reject(4, 'malformed')
    if (
      payload.binding.configuration.digest.value !==
      hash(canonical(payload.binding.configuration.projection))
    )
      reject(4, 'malformed')
  }
  if (!(payload.issued_at < payload.expires_at)) reject(4, 'malformed')
  return payload
}
function evaluate(item) {
  const context = merge(vectors.external_context, item.context_overrides)
  if (item.unsigned_object) {
    const object =
      vectors.unsigned_objects[item.unsigned_object] ??
      vectors.negative_unsigned_objects[item.unsigned_object]
    if (!ajv.validate('attestation', object))
      reject(item.pipeline === 'verifier_issuance' ? 1 : 4, 'malformed')
    for (const key of object.keys ?? []) admittedKey(key)
    return { accept: true, stage: 4 }
  }
  const compact = item.signed_object
    ? vectors.signed_objects[item.signed_object].compact_jws
    : vectors.negative_encodings[item.negative_encoding].compact_jws
  const payload = decoded(compact, context, item.expected_type)
  if (item.pipeline === 'verifier_issuance') {
    const authority = context.operator_authorization
    if (
      payload.binding?.operator_id &&
      (!authority?.authorization_record_active ||
        !authority.operator_dns_account_proof_verified ||
        context.now >= authority.not_after ||
        payload.expires_at > authority.not_after)
    )
      reject(2, 'operator_unauthorized')
    if (
      !context.durable_authorization_history_available ||
      !context.signer_fence_current
    )
      reject(3, 'state_recovery_required')
    return { accept: true, stage: 3 }
  }
  const assessment = payload.schema === 'ts-publisher-assessment-v1'
  let policy
  if (assessment) {
    policy = context.trusted_verifier.policy_documents
      .map((name) => vectors.unsigned_objects[name])
      .find(
        (document) =>
          document.id === payload.policy.id &&
          document.version === payload.policy.version
      )
    if (
      !policy ||
      !context.trusted_verifier.allowed_policies.includes(policy.id) ||
      hash(canonical(policy)) !== payload.policy.digest.value ||
      context.trusted_verifier.pinned_policy_digests[policy.id] !==
        payload.policy.digest.value
    )
      reject(5, 'policy_untrusted')
    for (const [name, check] of Object.entries(payload.checks)) {
      if (
        check.method &&
        (!context.trusted_verifier.allowed_methods.includes(check.method) ||
          (policy.checks[name].methods !== null &&
            !policy.checks[name].methods.includes(check.method)))
      )
        reject(5, 'policy_untrusted')
    }
    if (
      !context.trusted_verifier.allowed_methods.includes(payload.build.method)
    )
      reject(5, 'policy_untrusted')
    const methodField = {
      declared: 'declared_methods',
      fingerprint_only: 'fingerprint_methods',
      artifact_bound: 'artifact_methods',
    }[payload.build.assurance]
    if (
      policy.build[methodField] !== null &&
      !policy.build[methodField].includes(payload.build.method)
    )
      reject(5, 'policy_untrusted')
  }
  if (payload.nonce && payload.nonce !== context.nonce)
    reject(6, 'nonce_mismatch')
  if (
    payload.issued_at > context.now + 30 ||
    context.now >= payload.expires_at ||
    (assessment &&
      context.now >=
        payload.observed_from + context.maximum_observation_age_seconds)
  )
    reject(6, 'stale')
  if (payload.binding) {
    if (payload.binding.host !== context.expected_host)
      reject(7, 'host_mismatch')
    if (payload.binding.environment !== context.expected_environment)
      reject(7, 'environment_mismatch')
    if (assessment) {
      const fresh = decoded(
        vectors.signed_objects[context.fresh_evidence].compact_jws,
        context
      )
      if (fresh.nonce !== context.nonce) reject(6, 'nonce_mismatch')
      if (
        fresh.binding_id !== payload.binding_id ||
        canonical(fresh.binding) !== canonical(payload.binding)
      )
        reject(7, 'binding_mismatch')
    }
  }
  if (assessment) {
    for (const [name, requirement] of Object.entries(policy.checks)) {
      const outcome = payload.checks[name].outcome
      if (
        outcome !== 'pass' &&
        !(
          requirement.requirement === 'pass_or_not_assessed' &&
          outcome === 'not_assessed'
        )
      )
        reject(8, 'malformed')
    }
    if (
      payload.result !== 'satisfied' ||
      !policy.build.assurance.includes(payload.build.assurance) ||
      payload.expires_at >
        payload.observed_from + policy.max_assessment_lifetime_seconds
    )
      reject(8, 'malformed')
    if (
      payload.build.assurance === 'artifact_bound' &&
      !context.production_provider_qualification
    )
      reject(9, 'artifact_binding_unproven')
  }
  const status =
    payload.schema === 'ts-publisher-status-v1'
      ? payload
      : vectors.signed_objects.status.payload
  if (context.require_status || payload.schema === 'ts-publisher-status-v1') {
    if (
      !context.status_available ||
      status.sequence < context.highest_status_sequence ||
      context.now >=
        Math.min(
          status.expires_at,
          status.issued_at + context.configured_status_max_age
        )
    )
      reject(10, 'status_unavailable')
  }
  if (
    context.cache_action === 'reuse' &&
    context.cached_consumer_result?.required_status_dependencies.some(
      (dependency) => dependency.sequence < context.highest_status_sequence
    )
  )
    reject(10, 'status_unavailable')
  if (!assessment) return { accept: true, stage: payload.nonce ? 7 : 11 }
  const deadlines = [
    payload.expires_at,
    context.trusted_verifier.not_after,
    payload.observed_from + context.maximum_observation_age_seconds,
  ]
  if (context.require_status)
    deadlines.push(
      status.expires_at,
      status.issued_at + context.configured_status_max_age
    )
  if (typeof context.maximum_cache_age_seconds === 'number')
    deadlines.push(context.now + context.maximum_cache_age_seconds)
  if (typeof context.trusted_key_accept_until === 'number')
    deadlines.push(context.trusted_key_accept_until)
  return { accept: true, stage: 11, valid_until: Math.min(...deadlines) }
}
for (const item of vectors.cases) {
  test(`declared semantic case: ${item.id}`, () => {
    let actual
    try {
      actual = evaluate(item)
    } catch (error) {
      if (!('accept' in error)) throw error
      actual = error
    }
    for (const key of ['accept', 'stage', 'reason_code', 'valid_until'])
      if (key in item.expected)
        assert.equal(actual[key], item.expected[key], `${item.id}: ${key}`)
  })
}
test('merge patch preserves nested policy pins and removes null members', () => {
  assert.deepEqual(
    merge(
      { policy: { pin: 1, method: 2 }, cache: 3 },
      { policy: { method: 4 }, cache: null }
    ),
    { policy: { pin: 1, method: 4 } }
  )
})
