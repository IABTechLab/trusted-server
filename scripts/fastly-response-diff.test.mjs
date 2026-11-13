import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { mkdtempSync, rmSync, symlinkSync } from 'node:fs'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { after, test } from 'node:test'
import { fileURLToPath } from 'node:url'

import {
  InputError,
  classifyDifferences,
  compare,
  normalize,
  parseExpected,
} from './fastly-response-diff.mjs'

const SCRIPT = fileURLToPath(
  new URL('./fastly-response-diff.mjs', import.meta.url)
)

const scratch = mkdtempSync(path.join(tmpdir(), 'fastly-response-diff-'))
after(() => rmSync(scratch, { recursive: true, force: true }))

let links = 0

/** Runs the script through a symlink to it, the way a symlinked checkout or macOS /tmp would. */
function runThroughSymlink(args) {
  links += 1
  const link = path.join(scratch, `linked-${links}.mjs`)
  symlinkSync(SCRIPT, link)
  return spawnSync(process.execPath, [link, ...args], { encoding: 'utf8' })
}

test('the script runs as a command through a symlinked path', () => {
  const result = runThroughSymlink(['--help'])

  assert.equal(result.status, 0, result.stderr)
  assert.match(
    result.stdout,
    /^Usage:/,
    'should print the usage instead of skipping main'
  )
})

test('the script rejects bad arguments through a symlinked path', () => {
  const result = runThroughSymlink(['http://127.0.0.1:1'])

  assert.equal(result.status, 2, 'should exit 2 on bad input, not 0')
  assert.match(
    result.stderr,
    /fastly-response-diff: Usage:/,
    'should report the bad input'
  )
})

const REQUESTS = [{ id: 'trace-method' }, { id: 'head-root' }]

/** One request's differences, as `compare` builds them. */
function differences(entries) {
  return new Map(entries)
}

test('parseExpected accepts a bare item and a pinned head value', () => {
  const parsed = parseExpected(
    {
      'trace-method': { status: { item: 5, head: 405 } },
      'head-root': { 'header content-length': 7 },
    },
    REQUESTS,
    'expected.json'
  )

  assert.deepEqual(parsed, {
    'trace-method': { status: { item: 5, pinned: true, head: 405 } },
    'head-root': {
      'header content-length': { item: 7, pinned: false, head: undefined },
    },
  })
})

test('parseExpected rejects a pinned entry without exactly item and head', () => {
  for (const entry of [{ item: 5 }, { item: 5, head: 405, note: 'x' }]) {
    assert.throws(
      () =>
        parseExpected(
          { 'trace-method': { status: entry } },
          REQUESTS,
          'expected.json'
        ),
      (error) =>
        error instanceof InputError &&
        error.message.includes('must hold exactly "item" and "head"')
    )
  }
})

test('parseExpected rejects a pinned entry with no item number', () => {
  assert.throws(
    () =>
      parseExpected(
        { 'trace-method': { status: { item: 0, head: 405 } } },
        REQUESTS,
        'expected.json'
      ),
    (error) =>
      error instanceof InputError &&
      error.message.includes('must name a Fastly differences item number')
  )
})

test('classifyDifferences matches a key-only entry on the key alone', () => {
  const listed = parseExpected(
    { 'head-root': { 'header content-length': 7 } },
    REQUESTS,
    'expected.json'
  )['head-root']

  const results = classifyDifferences(
    differences([['header content-length', { base: ['12'], head: [] }]]),
    listed
  )

  assert.deepEqual(
    results.map(({ kind, key }) => [kind, key]),
    [['expected', 'header content-length']]
  )
})

test('classifyDifferences accepts a pinned head value that matches', () => {
  const listed = parseExpected(
    { 'trace-method': { status: { item: 5, head: 405 } } },
    REQUESTS,
    'expected.json'
  )['trace-method']

  const results = classifyDifferences(
    differences([['status', { base: 501, head: 405 }]]),
    listed
  )

  assert.deepEqual(
    results.map(({ kind, item }) => [kind, item]),
    [['expected', 5]]
  )
})

test('classifyDifferences flags a pinned head value that differs', () => {
  const listed = parseExpected(
    { 'trace-method': { status: { item: 5, head: 405 } } },
    REQUESTS,
    'expected.json'
  )['trace-method']

  const results = classifyDifferences(
    differences([['status', { base: 405, head: 500 }]]),
    listed
  )

  assert.deepEqual(
    results.map(({ kind, head }) => [kind, head]),
    [['mismatched', 500]]
  )
})

test('classifyDifferences reports unlisted and missing differences', () => {
  const listed = parseExpected(
    { 'head-root': { 'header content-length': 7 } },
    REQUESTS,
    'expected.json'
  )['head-root']

  const results = classifyDifferences(
    differences([['header etag', { base: ['"a"'], head: ['"b"'] }]]),
    listed
  )

  assert.deepEqual(
    results.map(({ kind, key }) => [kind, key]),
    [
      ['unexpected', 'header etag'],
      ['missing', 'header content-length'],
    ]
  )
})

const UUID_A = '123e4567-e89b-12d3-a456-426614174000'
const UUID_B = '00000000-1111-2222-3333-444444444444'
const EC_ID_A = `${'a'.repeat(64)}.Ab12Cd`
const EC_ID_B = `${'b'.repeat(64)}.Xy34Zw`

/** A response as `send` returns it. */
function response(headers, body = '', status = 200) {
  return { status, headers, body: Buffer.from(body) }
}

/** The differences between two responses after normalization. */
function diff(base, head, staging = false) {
  return compare(normalize(base, staging), normalize(head, staging))
}

test('normalize scrubs UUIDs and EC IDs in headers and bodies', () => {
  const base = response(
    [
      ['x-request-id', UUID_A],
      ['set-cookie', `ts-ec=${EC_ID_A}; Path=/`],
    ],
    `<p>${UUID_A} ${EC_ID_A}</p>`
  )
  const head = response(
    [
      ['x-request-id', UUID_B],
      ['set-cookie', `ts-ec=${EC_ID_B}; Path=/`],
    ],
    `<p>${UUID_B} ${EC_ID_B}</p>`
  )

  assert.deepEqual(
    [...normalize(base, false).headers],
    [
      ['x-request-id', ['<uuid>']],
      ['set-cookie', ['ts-ec=<ec-id>; Path=/']],
    ],
    'should replace each generated ID with its placeholder'
  )
  assert.equal(
    diff(base, head).size,
    0,
    'should find no difference between responses that differ only in generated IDs'
  )
})

test('a header value that changes beyond its IDs still differs', () => {
  const base = response([['set-cookie', `ts-ec=${EC_ID_A}; Max-Age=100`]])
  const head = response([['set-cookie', `ts-ec=${EC_ID_B}; Max-Age=200`]])

  assert.deepEqual(
    Object.fromEntries(diff(base, head)),
    {
      'header set-cookie': {
        base: ['ts-ec=<ec-id>; Max-Age=100'],
        head: ['ts-ec=<ec-id>; Max-Age=200'],
      },
    },
    'should report the EC cookie whose attributes changed'
  )
})

test('a header on one side only still differs after normalization', () => {
  const base = response([['set-cookie', `ts-ec=${EC_ID_A}; Path=/`]])
  const head = response([])

  assert.deepEqual(
    Object.fromEntries(diff(base, head)),
    {
      'header set-cookie': { base: ['ts-ec=<ec-id>; Path=/'], head: [] },
    },
    'should report the header head dropped'
  )
})

test('volatile headers keep their presence', () => {
  const base = response([
    ['date', 'Mon, 02 Nov 2026 10:00:00 GMT'],
    ['x-ts-version', 'base-build'],
  ])
  const sameNames = response([
    ['date', 'Mon, 02 Nov 2026 10:00:07 GMT'],
    ['x-ts-version', 'head-build'],
  ])
  const dropped = response([['x-ts-version', 'head-build']])

  assert.equal(
    diff(base, sameNames).size,
    0,
    'should ignore the values of volatile headers'
  )
  assert.deepEqual(
    Object.fromEntries(diff(base, dropped)),
    { 'header date': { base: ['<normalized>'], head: [] } },
    'should still report a volatile header head dropped'
  )
})

test('staging-only headers are left alone without --staging', () => {
  const base = response([
    ['x-geo-country', 'US'],
    ['x-ts-env', 'staging'],
  ])
  const head = response([
    ['x-geo-country', 'CA'],
    ['x-ts-env', 'production'],
  ])

  assert.deepEqual(
    [...diff(base, head).keys()],
    ['header x-geo-country', 'header x-ts-env'],
    'should compare staging-only headers without --staging'
  )
  assert.equal(
    diff(base, head, true).size,
    0,
    'should normalize staging-only headers with --staging'
  )
})
