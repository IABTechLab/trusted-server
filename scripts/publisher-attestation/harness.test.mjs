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
  try {
    const parsed = JSON.parse(raw)
    const tokens =
      raw.match(
        /"(?:[^"\\]|\\.)*"|[{}[\]:,]|true|false|null|-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?/g
      ) ?? []
    let index = 0
    function value(depth = 0) {
      const token = tokens[index++]
      if (token !== '{' && token !== '[') return
      if (depth >= 16) reject(1, 'malformed')
      const keys = new Set()
      const close = token === '{' ? '}' : ']'
      while (tokens[index] !== close) {
        if (token === '{') {
          const key = JSON.parse(tokens[index++])
          if (keys.has(key)) reject(1, 'malformed')
          keys.add(key)
          index++
        }
        value(depth + 1)
        if (tokens[index] !== ',') break
        index++
      }
      index++
    }
    value()
    return parsed
  } catch (error) {
    if (error?.accept === false) throw error
    reject(1, 'malformed')
  }
}
function admittedKey(key, stage = 4, reason = 'malformed') {
  try {
    if (
      key.kid !==
      Buffer.from(hash(canonical(key.jwk)), 'hex').toString('base64url')
    )
      reject(stage, reason)
    strictPoint(Buffer.from(key.jwk.x, 'base64url'))
  } catch {
    reject(stage, reason)
  }
}
function decoded(compact, context, expectedType) {
  const limits = {
    'ts-publisher-evidence+jws': 16384,
    'ts-publisher-assessment+jws': 32768,
    'ts-publisher-status+jws': 8192,
  }
  if (!limits[expectedType]) reject(2, 'signature_invalid')
  const segments = compact.split('.')
  if (
    segments.length !== 3 ||
    Buffer.byteLength(compact) > limits[expectedType]
  )
    reject(1, 'malformed')
  const [headerSegment, payloadSegment, signatureSegment] = segments
  for (const segment of segments)
    if (
      !segment ||
      Buffer.from(segment, 'base64url').toString('base64url') !== segment
    )
      reject(1, 'malformed')
  const json = (segment) => {
    try {
      return parse(
        new TextDecoder('utf-8', { fatal: true }).decode(
          Buffer.from(segment, 'base64url')
        )
      )
    } catch (error) {
      if (error?.accept === false) throw error
      reject(1, 'malformed')
    }
  }
  const header = json(headerSegment)
  const payload = json(payloadSegment)
  const evidence = expectedType === 'ts-publisher-evidence+jws'
  if (!ajv.validate('attestation', header) || header.typ !== expectedType)
    reject(2, 'signature_invalid')
  if (!evidence && payload.issuer !== context.trusted_verifier.issuer)
    reject(2, 'policy_untrusted')
  const key = evidence
    ? payload.binding?.attestation_key
    : context.trusted_verifier
  if (
    !key?.jwk ||
    header.kid !== key.kid ||
    key.kid !==
      Buffer.from(hash(canonical(key.jwk)), 'hex').toString('base64url') ||
    (!evidence &&
      !(key.not_before <= context.now && context.now < key.not_after))
  )
    reject(2, 'signature_invalid')
  admittedKey(key, 3, 'signature_invalid')
  const signature = Buffer.from(signatureSegment, 'base64url')
  if (signature.length !== 64) reject(3, 'signature_invalid')
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
  try {
    if (
      !verify(
        null,
        Buffer.from(`${headerSegment}.${payloadSegment}`),
        createPublicKey({ key: key.jwk, format: 'jwk' }),
        signature
      )
    )
      reject(3, 'signature_invalid')
  } catch {
    reject(3, 'signature_invalid')
  }
  const platform = payload.binding?.platform
  if (
    platform?.provider === 'fastly' &&
    ((platform.staging === true &&
      payload.binding.environment === 'production') ||
      (platform.staging === false && payload.binding.environment === 'staging'))
  )
    reject(4, 'environment_mismatch')
  const payloadSchemas = {
    'ts-publisher-evidence+jws': 'ts-publisher-evidence-v1',
    'ts-publisher-assessment+jws': 'ts-publisher-assessment-v1',
    'ts-publisher-status+jws': 'ts-publisher-status-v1',
  }
  if (
    payload.schema !== payloadSchemas[expectedType] ||
    !ajv.validate('attestation', payload)
  )
    reject(4, 'malformed')
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
  if (payload.schema === 'ts-publisher-assessment-v1') {
    const sameDigest = (left, right) => canonical(left) === canonical(right)
    if (
      !sameDigest(
        payload.checks.key_possession.evidence_digest,
        payload.evidence_digest
      ) ||
      !sameDigest(
        payload.checks.configuration.evidence_digest,
        payload.binding.configuration.digest
      ) ||
      !(payload.observed_from <= payload.observed_until) ||
      !(payload.observed_until <= payload.issued_at) ||
      payload.issued_at > payload.observed_until + 60 ||
      payload.expires_at > payload.observed_from + 900
    )
      reject(4, 'malformed')
  } else if (payload.expires_at > payload.issued_at + 60) reject(4, 'malformed')
  if (evidence) {
    const window = payload.binding.attestation_key
    if (
      !(window.not_before <= payload.issued_at) ||
      !(payload.expires_at <= window.not_after)
    )
      reject(4, 'key_unauthorized')
  }
  return payload
}
function freshTime(payload, context) {
  if (payload.nonce !== context.nonce) reject(6, 'nonce_mismatch')
  if (payload.issued_at > context.now + 30 || context.now >= payload.expires_at)
    reject(6, 'stale')
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
  const signedInput = (name) =>
    (vectors.signed_objects[name] ?? vectors.negative_encodings[name])
      .compact_jws
  if (item.pipeline === 'verifier_issuance') {
    let payload
    try {
      payload = decoded(compact, context, item.expected_type)
    } catch (error) {
      if (error?.accept === false) reject(1, error.reason_code)
      throw error
    }
    issuance(payload, context)
    return { accept: true, stage: 3 }
  }
  const assessment = item.expected_type === 'ts-publisher-assessment+jws'
  // Each stage applies to the fresh evidence before the candidate assessment.
  const fresh = assessment
    ? decoded(
        signedInput(context.fresh_evidence),
        context,
        'ts-publisher-evidence+jws'
      )
    : null
  const payload = decoded(compact, context, item.expected_type)
  return consumer(payload, fresh, context)
}
function issuance(payload, context) {
  const publisher = context.publisher_authorization
  if (
    payload.binding &&
    (!publisher?.authorization_record_active ||
      publisher.proof_verified !== true ||
      !Number.isSafeInteger(publisher.not_before) ||
      !Number.isSafeInteger(publisher.not_after) ||
      !(publisher.not_before < publisher.not_after) ||
      typeof context.authenticated_account !== 'string' ||
      !Array.isArray(publisher.authorized_sites) ||
      publisher.publisher_id !== payload.binding.publisher_id ||
      publisher.authenticated_account !== context.authenticated_account ||
      !publisher.authorized_sites.includes(payload.binding.host) ||
      context.now < publisher.not_before ||
      context.now >= publisher.not_after ||
      payload.expires_at > publisher.not_after)
  )
    reject(2, 'publisher_unauthorized')
  const authority = context.operator_authorization
  if (
    payload.binding?.operator_id &&
    (!authority?.authorization_record_active ||
      authority.operator_id !== payload.binding.operator_id ||
      authority.authenticated_account !== context.authenticated_account ||
      authority.publisher_id !== payload.binding.publisher_id ||
      authority.deployment_id !== payload.binding.deployment_id ||
      !Number.isSafeInteger(authority.not_before) ||
      !Number.isSafeInteger(authority.not_after) ||
      !(authority.not_before < authority.not_after) ||
      context.now < authority.not_before ||
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
}
function consumer(payload, fresh, context) {
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
        check.outcome === 'pass' &&
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
  if (fresh) freshTime(fresh, context)
  if (payload.schema === 'ts-publisher-evidence-v1') freshTime(payload, context)
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
      if (
        fresh.binding_id !== payload.binding_id ||
        canonical(fresh.binding) !== canonical(payload.binding)
      )
        reject(7, 'binding_mismatch')
    }
  }
  if (assessment) {
    if (payload.result !== 'satisfied') reject(8, 'policy_untrusted')
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
  if (context.known_key_compromise) reject(10, 'key_compromised')
  if (context.known_withdrawal) reject(10, 'withdrawn')
  let status
  if (context.require_status || payload.schema === 'ts-publisher-status-v1') {
    if (!context.status_available) reject(10, 'status_unavailable')
    try {
      status =
        payload.schema === 'ts-publisher-status-v1'
          ? payload
          : decoded(
              (
                vectors.signed_objects[context.status_object] ??
                vectors.negative_encodings[context.status_object]
              ).compact_jws,
              context,
              'ts-publisher-status+jws'
            )
    } catch {
      reject(10, 'status_unavailable')
    }
    const target = assessment
      ? payload
      : decoded(
          vectors.signed_objects[context.target_assessment].compact_jws,
          context,
          'ts-publisher-assessment+jws'
        )
    if (
      status.issuer !== target.issuer ||
      status.enrollment_id !== target.enrollment_id ||
      status.assessment_id !== target.assessment_id ||
      status.binding_id !== target.binding_id ||
      status.publisher_key_kid !== target.binding.attestation_key.kid ||
      status.sequence < context.highest_status_sequence ||
      status.issued_at > context.now + 30 ||
      context.now >=
        Math.min(
          status.expires_at,
          status.issued_at + context.configured_status_max_age
        )
    )
      reject(10, 'status_unavailable')
    if (status.publisher_key_status === 'compromised')
      reject(10, 'key_compromised')
    if (status.publisher_key_status === 'retired')
      reject(10, 'key_unauthorized')
    if (
      status.assessment_status !== 'current' ||
      status.enrollment_status !== 'active' ||
      status.publisher_key_status !== 'active'
    )
      reject(10, 'withdrawn')
  }
  if (
    context.cache_action === 'reuse' &&
    context.cached_consumer_result?.required_status_dependencies.some(
      (dependency) => dependency.sequence < context.highest_status_sequence
    )
  )
    reject(10, 'status_unavailable')
  if (context.distinct_issuer_threshold > 1) reject(11, 'policy_untrusted')
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
test('strict JSON parser enforces depth, duplicate names, syntax and trailing-data bounds', () => {
  assert.doesNotThrow(
    () => parse('['.repeat(16) + '0' + ']'.repeat(16)),
    '16 containers should be allowed'
  )
  for (const raw of [
    '['.repeat(17) + '0' + ']'.repeat(17),
    '{"x":1,"x":2}',
    '{"x":{"y":1,"y":2}}',
    '{} trailing',
    '{"x":}',
    '',
  ])
    assert.throws(
      () => parse(raw),
      (error) => error.stage === 1 && error.reason_code === 'malformed',
      `invalid JSON should reject at stage 1: ${raw}`
    )
})
test('compact envelope bounds follow the independently expected message type', () => {
  // Valid base64url/JSON up to stage 1; the empty header then fails stage 2.
  const sized = (length) => {
    for (let filler = Math.floor((length * 3) / 4); filler > 0; filler--) {
      const head = `e30.${Buffer.from(JSON.stringify({ filler: 'x'.repeat(filler) })).toString('base64url')}.`
      const remaining = length - Buffer.byteLength(head)
      if (remaining >= 2 && remaining <= 4) return head + 'A'.repeat(remaining)
    }
    throw new Error(`cannot build a ${length}-byte envelope`)
  }
  for (const [type, limit] of [
    ['ts-publisher-evidence+jws', 16384],
    ['ts-publisher-status+jws', 8192],
    ['ts-publisher-assessment+jws', 32768],
  ]) {
    assert.throws(
      () => decoded(sized(limit), vectors.external_context, type),
      (error) => error.stage === 2,
      `${type} should admit an envelope at its limit`
    )
    assert.throws(
      () => decoded(sized(limit + 1), vectors.external_context, type),
      (error) => error.stage === 1,
      `${type} should reject an envelope above its limit`
    )
  }
  for (const [compact, message] of [
    [
      'e30.Iv8i.AA',
      'invalid UTF-8 inside a JSON string should reject at stage 1',
    ],
    ['e30=.e30.AA', 'padded base64url should reject at stage 1'],
  ])
    assert.throws(
      () =>
        decoded(compact, vectors.external_context, 'ts-publisher-evidence+jws'),
      (error) => error.stage === 1,
      message
    )
})
