// Public TEST-ONLY seeds. Never use these keys for real identities or data.
// Decoded fixture payloads and cases are inputs; signatures and digests are outputs.
import assert from 'node:assert/strict'
import {
  createHash,
  createPrivateKey,
  createPublicKey,
  sign,
} from 'node:crypto'
import { readFileSync, writeFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { fileURLToPath } from 'node:url'

const fixture = new URL(
  '../../docs/superpowers/specs/fixtures/publisher-attestation-v1-vectors.json',
  import.meta.url
)
export const canonical = (value) =>
  JSON.stringify(value, (_key, item) =>
    item && !Array.isArray(item) && typeof item === 'object'
      ? Object.fromEntries(
          Object.keys(item)
            .sort()
            .map((key) => [key, item[key]])
        )
      : item
  )
export const hash = (value) => createHash('sha256').update(value).digest('hex')
const digest = (value) => ({ algorithm: 'sha256', value: hash(value) })
const encode = (value) => Buffer.from(value).toString('base64url')
const seeds = {
  publisher: Buffer.from(Array.from({ length: 32 }, (_, index) => index)),
  publisher_site: Buffer.from(
    Array.from({ length: 32 }, (_, index) => index + 64)
  ),
  verifier: Buffer.from(Array.from({ length: 32 }, (_, index) => index + 32)),
}
const keys = Object.fromEntries(
  Object.entries(seeds).map(([role, seed]) => [
    role,
    createPrivateKey({
      key: Buffer.concat([
        Buffer.from('302e020100300506032b657004220420', 'hex'),
        seed,
      ]),
      type: 'pkcs8',
      format: 'der',
    }),
  ])
)
const thumbprint = (jwk) =>
  encode(createHash('sha256').update(canonical(jwk)).digest())
const replacements = (value, mapping) => {
  if (typeof value === 'string') return mapping.get(value) ?? value
  if (Array.isArray(value))
    return value.map((item) => replacements(item, mapping))
  if (value && typeof value === 'object')
    return Object.fromEntries(
      Object.entries(value).map(([key, item]) => [
        key,
        replacements(item, mapping),
      ])
    )
  return value
}
function signed(
  header,
  payload,
  role = 'publisher',
  rawPayload = canonical(payload)
) {
  const protectedSegment = encode(canonical(header))
  const payloadSegment = encode(rawPayload)
  const signatureSegment = encode(
    sign(null, Buffer.from(`${protectedSegment}.${payloadSegment}`), keys[role])
  )
  const compact = `${protectedSegment}.${payloadSegment}.${signatureSegment}`
  return {
    protected: header,
    payload,
    protected_segment: protectedSegment,
    payload_segment: payloadSegment,
    signature_segment: signatureSegment,
    compact_jws: compact,
    compact_sha256: digest(compact),
  }
}
function withSignature(object, signature) {
  return {
    compact_jws: `${object.protected_segment}.${object.payload_segment}.${encode(signature)}`,
  }
}
const littleInteger = (bytes) =>
  BigInt(`0x${Buffer.from(bytes).reverse().toString('hex')}`)
const littleBytes = (value) =>
  Buffer.from(value.toString(16).padStart(64, '0'), 'hex').reverse()
const order = (1n << 252n) + 27742317777372353535851937790883648493n

export function generate(input) {
  let output = structuredClone(input)
  const map = new Map()
  for (const role of Object.keys(keys)) {
    const jwk = createPublicKey(keys[role]).export({ format: 'jwk' })
    const kid = thumbprint(jwk)
    map.set(output.public_keys[role].kid, kid)
    map.set(output.public_keys[role].jwk.x, jwk.x)
  }
  for (const [name, document] of Object.entries(output.unsigned_objects)) {
    if (document.schema !== 'ts-publisher-policy-v1') continue
    output.unsigned_objects[name] = JSON.parse(
      readFileSync(
        new URL(
          `../../docs/superpowers/specs/policies/${document.id}.json`,
          import.meta.url
        )
      )
    )
  }
  for (const document of Object.values(output.unsigned_objects).filter(
    (item) => item.schema === 'ts-publisher-policy-v1'
  )) {
    for (const object of Object.values(output.signed_objects)) {
      if (
        object.payload.policy?.id === document.id &&
        object.payload.policy.version === document.version
      ) {
        map.set(object.payload.policy.digest.value, hash(canonical(document)))
      }
    }
  }
  output = replacements(output, map)
  const objects = output.signed_objects
  for (const object of Object.values(objects)) {
    const payload = object.payload
    if (!payload.binding) continue
    payload.binding.configuration.digest = digest(
      canonical(payload.binding.configuration.projection)
    )
    const bindingId = hash(canonical(payload.binding))
    map.set(payload.binding_id, bindingId)
    payload.binding_id = bindingId
  }
  output = replacements(output, map)
  const result = output.signed_objects
  for (const [name, object] of Object.entries(result)) {
    if (object.payload.schema !== 'ts-publisher-evidence-v1') continue
    const role = Object.keys(keys).find(
      (role) =>
        output.public_keys[role].kid ===
        object.payload.binding.attestation_key.kid
    )
    result[name] = signed(object.protected, object.payload, role)
  }
  for (const [name, object] of Object.entries(result)) {
    const payload = object.payload
    if (payload.schema !== 'ts-publisher-assessment-v1') continue
    const collected = result[object.evidence_source ?? 'collected_evidence']
    payload.evidence_digest = collected.compact_sha256
    payload.checks.key_possession.evidence_digest = collected.compact_sha256
    payload.checks.configuration.evidence_digest =
      payload.binding.configuration.digest
    result[name] = signed(object.protected, payload, 'verifier')
    if (object.evidence_source)
      result[name].evidence_source = object.evidence_source
  }
  for (const [name, object] of Object.entries(result)) {
    if (object.payload.schema !== 'ts-publisher-status-v1') continue
    result[name] = signed(object.protected, object.payload, 'verifier')
  }
  const evidence = result.evidence
  const negatives = {}
  const statusSignature = Buffer.from(
    result.status.signature_segment,
    'base64url'
  )
  statusSignature[63] ^= 1
  negatives.tampered_status = withSignature(result.status, statusSignature)
  negatives.legacy_algorithm = {
    compact_jws: signed(
      { ...evidence.protected, alg: 'EdDSA' },
      evidence.payload
    ).compact_jws,
  }
  negatives.wrong_message_type = {
    compact_jws: signed(
      { ...evidence.protected, typ: 'ts-publisher-status+jws' },
      evidence.payload
    ).compact_jws,
  }
  negatives.unknown_member = {
    compact_jws: signed(evidence.protected, {
      ...evidence.payload,
      unexpected: true,
    }).compact_jws,
  }
  const signature = Buffer.from(evidence.signature_segment, 'base64url')
  const tampered = Buffer.from(signature)
  tampered[0] ^= 1
  negatives.signature_tamper = withSignature(evidence, tampered)
  negatives.non_canonical_s = withSignature(
    evidence,
    Buffer.concat([
      signature.subarray(0, 32),
      littleBytes(littleInteger(signature.subarray(32)) + order),
    ])
  )
  const identity = Buffer.alloc(32)
  identity[0] = 1
  const expanded = createHash('sha512').update(seeds.publisher).digest()
  expanded[0] &= 248
  expanded[31] &= 63
  expanded[31] |= 64
  const challenge =
    littleInteger(
      createHash('sha512')
        .update(
          Buffer.concat([
            identity,
            Buffer.from(output.public_keys.publisher.jwk.x, 'base64url'),
            Buffer.from(
              `${evidence.protected_segment}.${evidence.payload_segment}`
            ),
          ])
        )
        .digest()
    ) % order
  negatives.small_order_r = withSignature(
    evidence,
    Buffer.concat([
      identity,
      littleBytes(
        (challenge * littleInteger(expanded.subarray(0, 32))) % order
      ),
    ])
  )
  // Noncanonical identity y = p + 1 necessarily overlaps the small-order-R case.
  // Hash its original bytes and set S = k*a so relaxed point comparison accepts.
  const nonCanonicalIdentity = littleBytes((1n << 255n) - 18n)
  const nonCanonicalChallenge =
    littleInteger(
      createHash('sha512')
        .update(
          Buffer.concat([
            nonCanonicalIdentity,
            Buffer.from(output.public_keys.publisher.jwk.x, 'base64url'),
            Buffer.from(
              `${evidence.protected_segment}.${evidence.payload_segment}`
            ),
          ])
        )
        .digest()
    ) % order
  negatives.non_canonical_r = withSignature(
    evidence,
    Buffer.concat([
      nonCanonicalIdentity,
      littleBytes(
        (nonCanonicalChallenge * littleInteger(expanded.subarray(0, 32))) %
          order
      ),
    ])
  )
  // Public-key admission separately isolates encoding from small-order rejection.
  const nonCanonical = littleBytes((1n << 255n) - 16n)
  const fingerprint = structuredClone(
    result.assessment_fingerprint_only.payload
  )
  fingerprint.checks.active_version.outcome = 'not_assessed'
  fingerprint.checks.active_version.method = null
  fingerprint.checks.active_version.observed_at = null
  fingerprint.checks.active_version.evidence_digest = null
  negatives.fingerprint_without_active_version = {
    compact_jws: signed(
      result.assessment_fingerprint_only.protected,
      fingerprint,
      'verifier'
    ).compact_jws,
  }
  const contradiction = structuredClone(evidence.payload)
  contradiction.binding.platform.staging = true
  contradiction.binding_id = hash(canonical(contradiction.binding))
  negatives.contradictory_environment = {
    compact_jws: signed(evidence.protected, contradiction).compact_jws,
  }
  const raw = canonical(evidence.payload)
  for (const [name, text] of [
    [
      'duplicate_member_top_level',
      raw.replace(
        /,"extensions":\{\},"issued_at"/,
        ',"extensions":{},"extensions":{},"issued_at"'
      ),
    ],
    [
      'duplicate_member_nested',
      raw.replace(
        '"host":"news.example.com"',
        '"host":"news.example.com","host":"news.example.com"'
      ),
    ],
  ]) {
    assert.notEqual(raw, text, `${name} must alter raw JSON`)
    negatives[name] = {
      compact_jws: signed(
        evidence.protected,
        evidence.payload,
        'publisher',
        text
      ).compact_jws,
    }
  }
  output.negative_encodings = negatives
  const discovery = structuredClone(output.unsigned_objects.key_discovery)
  discovery.keys[0].jwk.x = encode(nonCanonical)
  discovery.keys[0].kid = thumbprint(discovery.keys[0].jwk)
  output.negative_unsigned_objects.non_canonical_public_key = discovery
  output.unsigned_objects.assessment_list.assessments = [
    result.assessment.compact_jws,
  ]
  output.unsigned_objects.key_history.entries = [
    {
      assessment: result.assessment.compact_jws,
      status: result.status.compact_jws,
    },
  ]
  // TXT values and canonical policy digests are part of the executable fixture contract.
  output.enrollment_txt_value = `ts-publisher-v1=${encode(createHash('sha256').update(canonical(output.unsigned_objects.enrollment_challenge)).digest())}`
  return output
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const current = readFileSync(fixture, 'utf8')
  const output = generate(JSON.parse(current))
  const require = createRequire(
    new URL('../../docs/package.json', import.meta.url)
  )
  const { format } = require('prettier')
  const config = JSON.parse(
    readFileSync(new URL('../../docs/.prettierrc', import.meta.url))
  )
  const bytes = await format(JSON.stringify(output, null, 2), {
    ...config,
    parser: 'json',
  })
  if (process.argv.includes('--check'))
    assert.equal(
      current,
      bytes,
      'run generate.mjs to regenerate exact fixture bytes'
    )
  else writeFileSync(fixture, bytes)
}
