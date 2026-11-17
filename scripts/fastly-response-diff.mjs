#!/usr/bin/env node
// Diff Trusted Server responses between a base and a head server.
//
// Usage:
//   node scripts/fastly-response-diff.mjs [--staging] [--origin <url>] \
//     <base_url> <head_url> <requests.json> <expected.json>
//   node scripts/fastly-response-diff.mjs --serve-origin <port>
//
// The first form sends every request in requests.json to both servers, base
// first, and compares the status, the sorted response headers, and the
// SHA-256 of the response body. Before comparing, it replaces values that
// differ between two builds of the same revision with fixed placeholders: the
// x-ts-version, x-ts-sandbox-*, date, and age headers, and generated IDs
// (UUIDs and EC IDs) in any header or body. --staging also replaces the geo
// headers (x-geo-*), x-ts-fastly-version, and x-ts-env, which differ between
// two hosted services.
//
// expected.json maps a request id to the differences that request is meant
// to show. Each difference names the "Fastly differences" item of the
// EdgeZero PR 275 cutover spec that explains it:
//
//   { "head-root": { "header content-length": 7 } }
//
// A difference is "status", "body", or "header <lower-case name>". An entry
// can also pin the value head must show, as { "item": <n>, "head": <value> }:
// a number for "status", the sorted list of normalized values for a header
// ([] when head drops it), and "sha256 <hex> (<n> bytes)" for "body":
//
//   { "trace-method": { "status": { "item": 5, "head": 405 } } }
//
// The diff exits 1 when a difference is not listed, when head shows another
// value than a pinned one, and also when a listed difference does not occur.
// It exits 2 on bad input or when a server cannot be reached.
//
// Each entry in requests.json has an "id", a "method", optional "headers",
// and an optional "body" string or "json" value. It names either a "path" or
// a "sign" target. A sign target is signed once through base's
// /first-party/sign, and the returned /first-party/proxy URL is then
// requested from both servers. In any string, "{self}" stands for the URL of
// the server being asked and "{origin}" for --origin. An optional
// "base_status" must match base's status, so a fixture that stops producing
// the case it is named for, such as a 304, fails instead of diffing the
// wrong case.
//
// The second form serves the loopback publisher origin that the checked-in
// requests expect. scripts/fastly-response-diff.sh starts it.

import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import http from 'node:http'
import https from 'node:https'
import { parseArgs } from 'node:util'

const USAGE = `Usage:
  node scripts/fastly-response-diff.mjs [--staging] [--origin <url>] <base_url> <head_url> <requests.json> <expected.json>
  node scripts/fastly-response-diff.mjs --serve-origin <port>`

const REQUEST_TIMEOUT_MS = 30_000
const NORMALIZED = '<normalized>'

// Headers whose values differ between two builds of the same revision.
const VOLATILE_HEADERS = new Set(['x-ts-version', 'date', 'age'])
const VOLATILE_HEADER_PREFIXES = ['x-ts-sandbox-']

// Headers whose values also differ between two hosted services.
const STAGING_HEADERS = new Set(['x-ts-fastly-version', 'x-ts-env'])
const STAGING_HEADER_PREFIXES = ['x-geo-']

const GENERATED_IDS = [
  [/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/gi, '<uuid>'],
  [/\b[0-9a-f]{64}\.[0-9A-Za-z]{6}\b/g, '<ec-id>'],
]

const DIFFERENCE_KEY = /^(?:status|body|header [!#$%&'*+.^_`|~0-9a-z-]+)$/

// Pages the loopback origin serves. Each route can carry an ETag, which makes
// a matching If-None-Match answer 304.
const ORIGIN_ROUTES = new Map([
  [
    '/',
    {
      type: 'text/html; charset=utf-8',
      etag: '"fastly-response-diff-page-1"',
      body:
        '<!doctype html>\n<html><head><title>Example publisher</title></head>\n' +
        '<body><h1>Example publisher</h1><p>Fixture page for the Fastly response diff.</p></body></html>\n',
    },
  ],
  [
    '/assets/site.css',
    {
      type: 'text/css; charset=utf-8',
      etag: '"fastly-response-diff-css-1"',
      body: 'body { margin: 0; }\n',
    },
  ],
  [
    '/proxied/widget.js',
    {
      type: 'application/javascript; charset=utf-8',
      body: 'window.exampleWidget = true;\n',
    },
  ],
])

export class InputError extends Error {}

function fail(message) {
  throw new InputError(message)
}

function serveOrigin(port) {
  const server = http.createServer((request, response) => {
    // Drain any request body so keep-alive connections stay usable.
    request.resume()
    request.on('end', () => {
      const path = new URL(request.url, 'http://127.0.0.1').pathname
      const route = ORIGIN_ROUTES.get(path)
      console.log(`${request.method} ${request.url}`)
      if (!route) {
        response.writeHead(404, { 'content-type': 'text/plain; charset=utf-8' })
        response.end('not found\n')
        return
      }
      const headers = {
        'cache-control': 'max-age=60',
        'content-type': route.type,
      }
      if (route.etag) {
        headers.etag = route.etag
        if (request.headers['if-none-match'] === route.etag) {
          response.writeHead(304, headers)
          response.end()
          return
        }
      }
      headers['content-length'] = Buffer.byteLength(route.body)
      response.writeHead(200, headers)
      response.end(request.method === 'HEAD' ? undefined : route.body)
    })
  })
  server.listen(port, '127.0.0.1', () => {
    console.log(`origin listening on 127.0.0.1:${port}`)
  })
  for (const signal of ['SIGINT', 'SIGTERM']) {
    process.on(signal, () => server.close(() => process.exit(0)))
  }
}

function readJson(path) {
  let text
  try {
    text = readFileSync(path, 'utf8')
  } catch (error) {
    fail(`cannot read ${path}: ${error.message}`)
  }
  try {
    return JSON.parse(text)
  } catch (error) {
    fail(`${path} is not valid JSON: ${error.message}`)
  }
}

function isStringMap(value) {
  return (
    value !== null &&
    typeof value === 'object' &&
    !Array.isArray(value) &&
    Object.values(value).every((entry) => typeof entry === 'string')
  )
}

function loadRequests(path) {
  const requests = readJson(path)
  if (!Array.isArray(requests) || requests.length === 0) {
    fail(`${path} must hold a non-empty array of requests`)
  }
  const ids = new Set()
  for (const request of requests) {
    const label = `${path} request ${JSON.stringify(request?.id)}`
    if (typeof request?.id !== 'string' || request.id === '') {
      fail(`${label} needs a string id`)
    }
    if (ids.has(request.id)) {
      fail(`${label} repeats an id`)
    }
    ids.add(request.id)
    if (typeof request.method !== 'string' || request.method === '') {
      fail(`${label} needs a method`)
    }
    if (
      (typeof request.path === 'string') ===
      (typeof request.sign === 'string')
    ) {
      fail(`${label} needs exactly one of path and sign`)
    }
    if (request.headers !== undefined && !isStringMap(request.headers)) {
      fail(`${label} headers must map names to strings`)
    }
    if (request.body !== undefined && typeof request.body !== 'string') {
      fail(`${label} body must be a string`)
    }
    if (request.body !== undefined && request.json !== undefined) {
      fail(`${label} cannot carry both body and json`)
    }
    if (
      request.base_status !== undefined &&
      !Number.isInteger(request.base_status)
    ) {
      fail(`${label} base_status must be an integer`)
    }
  }
  return requests
}

function isPlainObject(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}

function loadExpected(path, requests) {
  return parseExpected(readJson(path), requests, path)
}

// Validates expected.json's `expected` against `requests` and returns it with
// every difference as { item, pinned, head }, where `pinned` says whether the
// entry fixed head's value.
export function parseExpected(expected, requests, label) {
  if (!isPlainObject(expected)) {
    fail(`${label} must hold an object`)
  }
  const ids = new Set(requests.map((request) => request.id))
  const parsed = {}
  for (const [id, differences] of Object.entries(expected)) {
    if (!ids.has(id)) {
      fail(`${label} names unknown request ${JSON.stringify(id)}`)
    }
    if (!isPlainObject(differences)) {
      fail(`${label} entry ${JSON.stringify(id)} must map differences to items`)
    }
    parsed[id] = {}
    for (const [key, entry] of Object.entries(differences)) {
      const where = `${label} entry ${JSON.stringify(id)} difference ${JSON.stringify(key)}`
      if (!DIFFERENCE_KEY.test(key)) {
        fail(
          `${label} entry ${JSON.stringify(id)} has malformed difference ${JSON.stringify(key)}`
        )
      }
      const pinned = isPlainObject(entry)
      const item = pinned ? entry.item : entry
      if (!Number.isInteger(item) || item < 1) {
        fail(`${where} must name a Fastly differences item number`)
      }
      if (pinned) {
        const fields = Object.keys(entry).sort()
        if (JSON.stringify(fields) !== JSON.stringify(['head', 'item'])) {
          fail(`${where} must hold exactly "item" and "head"`)
        }
      }
      parsed[id][key] = { item, pinned, head: pinned ? entry.head : undefined }
    }
  }
  return parsed
}

// Sorts one request's `differences` against its `listed` expectations.
//
// Returns one { kind, key, item, base, head } per difference and per listed
// difference that did not occur. `kind` is "expected", "unexpected",
// "mismatched" (listed, but head shows another value than the pinned one),
// or "missing".
export function classifyDifferences(differences, listed) {
  const results = []
  for (const [key, { base, head }] of differences) {
    const expectation = listed[key]
    let kind = 'expected'
    if (expectation === undefined) {
      kind = 'unexpected'
    } else if (
      expectation.pinned &&
      JSON.stringify(expectation.head) !== JSON.stringify(head)
    ) {
      kind = 'mismatched'
    }
    results.push({ kind, key, item: expectation?.item, base, head })
  }
  for (const [key, expectation] of Object.entries(listed)) {
    if (!differences.has(key)) {
      results.push({ kind: 'missing', key, item: expectation.item })
    }
  }
  return results
}

function substitute(text, placeholders) {
  return text.replace(/\{(self|origin)\}/g, (match, name) => {
    const value = placeholders[name]
    if (value === undefined) {
      fail(`${match} needs a value; pass --origin`)
    }
    return value
  })
}

function send(serverUrl, method, path, headers, body) {
  const url = new URL(path, serverUrl)
  const transport = url.protocol === 'https:' ? https : http
  const requestHeaders = { ...headers }
  if (body !== undefined) {
    requestHeaders['content-length'] = String(Buffer.byteLength(body))
  }
  return new Promise((resolve, reject) => {
    const request = transport.request(
      url,
      { method, headers: requestHeaders, timeout: REQUEST_TIMEOUT_MS },
      (response) => {
        const chunks = []
        response.on('data', (chunk) => chunks.push(chunk))
        response.on('error', reject)
        response.on('end', () => {
          const pairs = []
          for (let index = 0; index < response.rawHeaders.length; index += 2) {
            pairs.push([
              response.rawHeaders[index].toLowerCase(),
              response.rawHeaders[index + 1],
            ])
          }
          resolve({
            status: response.statusCode,
            headers: pairs,
            body: Buffer.concat(chunks),
          })
        })
      }
    )
    request.on('timeout', () =>
      request.destroy(new Error(`timed out after ${REQUEST_TIMEOUT_MS} ms`))
    )
    request.on('error', reject)
    request.end(body)
  })
}

async function sendOrFail(serverUrl, method, path, headers, body) {
  try {
    return await send(serverUrl, method, path, headers, body)
  } catch (error) {
    fail(`${method} ${path} to ${serverUrl} failed: ${error.message}`)
  }
}

function placeholdersFor(serverUrl, placeholders) {
  return { ...placeholders, self: new URL(serverUrl).origin }
}

// Signs a request's target once, through base, so both servers receive the
// same /first-party/proxy URL.
async function signedPath(request, baseUrl, placeholders) {
  const target = substitute(
    request.sign,
    placeholdersFor(baseUrl, placeholders)
  )
  const signPath = `/first-party/sign?url=${encodeURIComponent(target)}`
  const response = await sendOrFail(baseUrl, 'GET', signPath, {}, undefined)
  if (response.status !== 200) {
    fail(
      `request ${request.id}: signing ${target} on base returned ${response.status}`
    )
  }
  let href
  try {
    href = JSON.parse(response.body.toString('utf8')).href
  } catch {
    fail(`request ${request.id}: base signing response is not JSON`)
  }
  if (typeof href !== 'string' || href === '') {
    fail(`request ${request.id}: base signing response has no href`)
  }
  const signed = new URL(href, baseUrl)
  return `${signed.pathname}${signed.search}`
}

function scrubGeneratedIds(text) {
  return GENERATED_IDS.reduce(
    (scrubbed, [pattern, placeholder]) =>
      scrubbed.replace(pattern, placeholder),
    text
  )
}

function isNormalizedHeader(name, staging) {
  if (
    VOLATILE_HEADERS.has(name) ||
    VOLATILE_HEADER_PREFIXES.some((prefix) => name.startsWith(prefix))
  ) {
    return true
  }
  return (
    staging &&
    (STAGING_HEADERS.has(name) ||
      STAGING_HEADER_PREFIXES.some((prefix) => name.startsWith(prefix)))
  )
}

function normalize(response, staging) {
  const headers = new Map()
  for (const [name, value] of response.headers) {
    const normalized = isNormalizedHeader(name, staging)
      ? NORMALIZED
      : scrubGeneratedIds(value)
    headers.set(name, [...(headers.get(name) ?? []), normalized])
  }
  for (const values of headers.values()) {
    values.sort()
  }
  // Latin-1 maps every byte to one code unit, so the scrub cannot alter
  // bytes outside the IDs it replaces.
  const body = Buffer.from(
    scrubGeneratedIds(response.body.toString('latin1')),
    'latin1'
  )
  return {
    status: response.status,
    headers,
    bodyHash: createHash('sha256').update(body).digest('hex'),
    bodyLength: body.length,
  }
}

function compare(base, head) {
  const differences = new Map()
  if (base.status !== head.status) {
    differences.set('status', { base: base.status, head: head.status })
  }
  const names = [...new Set([...base.headers.keys(), ...head.headers.keys()])]
  for (const name of names.sort()) {
    const baseValues = base.headers.get(name) ?? []
    const headValues = head.headers.get(name) ?? []
    if (JSON.stringify(baseValues) !== JSON.stringify(headValues)) {
      differences.set(`header ${name}`, { base: baseValues, head: headValues })
    }
  }
  if (base.bodyHash !== head.bodyHash) {
    differences.set('body', {
      base: `sha256 ${base.bodyHash} (${base.bodyLength} bytes)`,
      head: `sha256 ${head.bodyHash} (${head.bodyLength} bytes)`,
    })
  }
  return differences
}

function describe(value) {
  return Array.isArray(value)
    ? value.length === 0
      ? '(absent)'
      : value.map((entry) => JSON.stringify(entry)).join(', ')
    : String(value)
}

async function runDiff(values, positionals) {
  if (positionals.length !== 4) {
    fail(USAGE)
  }
  const [baseUrl, headUrl, requestsPath, expectedPath] = positionals
  for (const url of [baseUrl, headUrl]) {
    if (!URL.canParse(url)) {
      fail(`${url} is not a URL`)
    }
  }
  const requests = loadRequests(requestsPath)
  const expected = loadExpected(expectedPath, requests)
  const placeholders =
    values.origin === undefined ? {} : { origin: values.origin }
  const staging = values.staging === true

  let unexpected = 0
  let missing = 0
  for (const request of requests) {
    const signed =
      request.sign === undefined
        ? undefined
        : await signedPath(request, baseUrl, placeholders)
    const results = []
    for (const serverUrl of [baseUrl, headUrl]) {
      const local = placeholdersFor(serverUrl, placeholders)
      const path = signed ?? substitute(request.path, local)
      const headers = Object.fromEntries(
        Object.entries(request.headers ?? {}).map(([name, value]) => [
          name,
          substitute(value, local),
        ])
      )
      const body =
        request.json !== undefined
          ? JSON.stringify(request.json)
          : request.body === undefined
            ? undefined
            : substitute(request.body, local)
      const response = await sendOrFail(
        serverUrl,
        request.method,
        path,
        headers,
        body
      )
      results.push(normalize(response, staging))
    }
    const [base, head] = results
    if (
      request.base_status !== undefined &&
      base.status !== request.base_status
    ) {
      fail(
        `request ${request.id}: base answered ${base.status}, but the fixture expects ${request.base_status}`
      )
    }

    const differences = compare(base, head)
    const listed = expected[request.id] ?? {}
    if (differences.size === 0 && Object.keys(listed).length === 0) {
      console.log(`same        ${request.id}`)
    }
    for (const result of classifyDifferences(differences, listed)) {
      const { kind, key, item } = result
      if (kind === 'missing') {
        missing += 1
        console.log(
          `missing     ${request.id}: ${key} (item ${item}) did not differ`
        )
        continue
      }
      if (kind === 'expected') {
        console.log(`expected    ${request.id}: ${key} (item ${item})`)
      } else if (kind === 'mismatched') {
        unexpected += 1
        console.log(
          `mismatched  ${request.id}: ${key} (item ${item}) head differs from the pinned ${describe(listed[key].head)}`
        )
      } else {
        unexpected += 1
        console.log(`unexpected  ${request.id}: ${key}`)
      }
      console.log(`              base: ${describe(result.base)}`)
      console.log(`              head: ${describe(result.head)}`)
    }
  }

  console.log(
    `${requests.length} requests, ${unexpected} unexpected differences, ${missing} expected differences missing`
  )
  return unexpected === 0 && missing === 0 ? 0 : 1
}

async function main() {
  let parsed
  try {
    parsed = parseArgs({
      allowPositionals: true,
      options: {
        help: { type: 'boolean' },
        origin: { type: 'string' },
        'serve-origin': { type: 'string' },
        staging: { type: 'boolean' },
      },
    })
  } catch (error) {
    fail(`${error.message}\n${USAGE}`)
  }
  const { values, positionals } = parsed
  if (values.help) {
    console.log(USAGE)
    return 0
  }
  if (values['serve-origin'] !== undefined) {
    const port = Number(values['serve-origin'])
    if (!Number.isInteger(port) || port < 1 || port > 65_535) {
      fail(`--serve-origin needs a port, got ${values['serve-origin']}`)
    }
    serveOrigin(port)
    return undefined
  }
  return runDiff(values, positionals)
}

// Run only as a command, so the test file can import the compare logic.
// `import.meta.main` holds through a symlinked path, where comparing the
// module URL with `process.argv[1]` would skip `main` and exit 0 silently.
if (import.meta.main) {
  try {
    const code = await main()
    if (code !== undefined) {
      process.exitCode = code
    }
  } catch (error) {
    if (!(error instanceof InputError)) {
      throw error
    }
    console.error(`fastly-response-diff: ${error.message}`)
    process.exitCode = 2
  }
}
