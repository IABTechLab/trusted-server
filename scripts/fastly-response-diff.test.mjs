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
