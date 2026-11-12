// Test-only point admission for synthetic interoperability fixtures.
import assert from 'node:assert/strict'

const prime = (1n << 255n) - 19n
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
const integer = (bytes) =>
  BigInt(`0x${Buffer.from(bytes).reverse().toString('hex')}`)
const inverse = (value) => power(value, prime - 2n)
const curve = mod(-121665n * inverse(121666n))
function point(bytes) {
  const encoded = integer(bytes)
  const y = encoded & ((1n << 255n) - 1n)
  assert.ok(y < prime, 'point encoding must be canonical')
  const square = mod((y * y - 1n) * inverse(curve * y * y + 1n))
  let x = power(square, (prime + 3n) / 8n)
  if (mod(x * x) !== square) x = mod(x * power(2n, (prime - 1n) / 4n))
  assert.equal(mod(x * x), square, 'point must lie on the curve')
  const sign = encoded >> 255n
  assert.ok(x !== 0n || sign === 0n, 'zero x cannot carry the sign bit')
  if ((x & 1n) !== sign) x = prime - x
  return [x, y]
}
export function strictPoint(bytes) {
  assert.equal(bytes.length, 32, 'point must contain exactly 32 bytes')
  let [x, y] = point(bytes)
  for (let i = 0; i < 3; i++) {
    const product = mod(curve * x * x * y * y)
    const nextX = mod(2n * x * y * inverse(1n + product))
    y = mod((y * y + x * x) * inverse(1n - product))
    x = nextX
  }
  assert.ok(x !== 0n || y !== 1n, 'point must not have small order')
}
