import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { completeSpdx, validateSpdx } from './phase1.mjs'
import { test } from 'node:test'
import {
  readFileSync,
  mkdtempSync,
  mkdirSync,
  writeFileSync,
  rmSync,
} from 'node:fs'
import { join } from 'node:path'
import { tmpdir } from 'node:os'
import * as fs from 'node:fs'
import vm from 'node:vm'
import { checkBinary, checkSource, mergeSboms, sentinels } from './phase1.mjs'
test('rejects every planted operator value, including inside binary data', () => {
  for (const marker of sentinels) {
    assert.throws(
      () =>
        checkBinary(
          Buffer.concat([
            Buffer.from([0]),
            Buffer.from(marker),
            Buffer.from([255]),
          ])
        ),
      /operator configuration/
    )
  }
  assert.doesNotThrow(() =>
    checkBinary(Buffer.from('fictional clean executable'))
  )
  assert.throws(() => checkBinary(Buffer.alloc(0)), /empty/)
})
test('rejects deployment compile inputs while permitting product inputs', () => {
  for (const source of [
    'env!("TRUSTED_SERVER__EC__PASSPHRASE")',
    'option_env!("EDGEZERO__STORES__CONFIG__NAME")',
    'include_str!("../../trusted-server.toml")',
    'include_bytes!(".env.production")',
  ]) {
    assert.throws(() => checkSource('src/lib.rs', source), /compile-time/)
  }
  assert.throws(
    () => checkSource('build.rs', 'fs::read("trusted-server.toml")'),
    /compile-time/
  )
  assert.doesNotThrow(() =>
    checkSource(
      'src/lib.rs',
      'option_env!("TRUSTED_SERVER__GIT_VERSION"); include_str!("trusted-server.example.toml")'
    )
  )
})
test('merges independent graphs without colliding references and binds binary bytes', () => {
  const bom = (name) => ({
    bomFormat: 'CycloneDX',
    specVersion: '1.5',
    version: 1,
    metadata: { component: { type: 'application', name, 'bom-ref': 'root' } },
    components: [
      { type: 'library', name: `${name}-dependency`, 'bom-ref': 'dependency' },
    ],
    dependencies: [
      { ref: 'root', dependsOn: ['dependency'] },
      { ref: 'dependency', dependsOn: [] },
    ],
  })
  const output = mergeSboms(
    bom('cargo'),
    bom('npm'),
    Buffer.from('binary'),
    'example.wasm'
  )
  assert.equal(output.metadata.component.name, 'example.wasm')
  assert.equal(output.metadata.component.hashes[0].alg, 'SHA-256')
  assert.equal(output.metadata.component.hashes[0].content.length, 64)
  assert.deepEqual(
    output.dependencies.find(
      (d) => d.ref === output.metadata.component['bom-ref']
    ).dependsOn,
    [output.components[0]['bom-ref'], output.components[1]['bom-ref']]
  )
  assert.equal(
    new Set(output.components.map((c) => c['bom-ref'])).size,
    output.components.length
  )
  assert.ok(
    output.components.every((entry) =>
      /^[A-Za-z0-9.-]+$/.test(entry['bom-ref'])
    )
  )
  assert.throws(
    () => mergeSboms({}, bom('npm'), Buffer.from('binary'), 'example'),
    /CycloneDX/
  )
})
test('workflow signs only tags, separates privileged jobs, and never checks out in signing', () => {
  const workflow = readFileSync(
    new URL('../../.github/workflows/release-build.yml', import.meta.url),
    'utf8'
  )
  const sign = workflow.split('\n  sign:')[1]?.split('\n  release:')[0]
  assert.ok(sign, 'should define the isolated signing job')
  assert.match(sign, /github.event_name == 'push'/)
  assert.doesNotMatch(
    sign,
    /checkout@|cargo |npm |contents: write|artifact-metadata:/
  )
  assert.equal((sign.match(/uses: actions\/attest@/g) ?? []).length, 2)
  assert.match(
    readFileSync('.github/workflows/release.yml', 'utf8'),
    /cancel-in-progress:.*pull_request/
  )
  assert.doesNotMatch(workflow, /workflow_dispatch|--clobber/)
  for (const line of workflow
    .split('\n')
    .filter((line) => /uses: .*@/.test(line))) {
    assert.match(line, /@[a-f0-9]{40} # v/)
  }
})
const assets = [
  'trusted-server-fastly.wasm',
  'ts-aarch64-apple-darwin',
  'ts-x86_64-apple-darwin',
  'ts-x86_64-unknown-linux-gnu',
]
const hash = (bytes) => createHash('sha256').update(bytes).digest('hex')
function publicationScript() {
  const workflow = readFileSync('.github/workflows/release.yml', 'utf8')
  const match = workflow.match(
    /          \/\/ BEGIN PUBLICATION\n([\s\S]*?)          \/\/ END PUBLICATION/
  )
  assert.ok(
    match,
    'should expose the actual inline publication program for failure tests'
  )
  return match[1]
    .split('\n')
    .map((line) => line.slice(10))
    .join('\n')
}
test('publication binds exact signing handoffs and refuses drafts, corruption and partial uploads', async () => {
  const script = publicationScript()
  for (const mode of [
    'success',
    'idle-socket',
    'post-read-error',
    'draft',
    'api-error',
    'missing',
    'extra',
    'changed',
    'empty-bundle',
    'upload-error',
    'mutable',
    'wrong-id',
  ]) {
    const root = mkdtempSync(join(tmpdir(), 'phase1-publish-'))
    const calls = []
    const uploaded = []
    try {
      for (const asset of assets) {
        const build = join(root, 'build', asset)
        const signed = join(root, 'signed', asset)
        mkdirSync(build, { recursive: true })
        mkdirSync(signed, { recursive: true })
        const entries = []
        for (const suffix of [
          '',
          '.spdx.json',
          '.provenance.sigstore.json',
          '.sbom.sigstore.json',
        ]) {
          const bytes = Buffer.from(`fixture ${asset}${suffix}`)
          writeFileSync(
            join(suffix.includes('sigstore') ? signed : build, asset + suffix),
            bytes
          )
          entries.push(`${hash(bytes)}  ${asset}${suffix}\n`)
        }
        writeFileSync(join(signed, 'SHA256SUMS'), entries.join(''))
      }
      if (mode === 'missing')
        fs.unlinkSync(join(root, 'build', assets[0], assets[0]))
      if (mode === 'extra')
        writeFileSync(join(root, 'build', assets[0], 'unexpected'), 'extra')
      if (mode === 'changed')
        writeFileSync(join(root, 'build', assets[0], assets[0]), 'different')
      if (mode === 'empty-bundle')
        writeFileSync(
          join(root, 'signed', assets[0], assets[0] + '.sbom.sigstore.json'),
          ''
        )
      const release = (draft) => ({
        id: 42,
        tag_name: 'v0.0.0-example',
        draft,
        immutable: mode !== 'mutable',
        assets: uploaded,
      })
      const fetch = async (url, options = {}) => {
        const u = new URL(url)
        const method = options.method ?? 'GET'
        if (
          calls.some(([kind]) => kind === 'gh') &&
          (mode === 'post-read-error' ||
            (mode === 'idle-socket' && options.headers.Connection !== 'close'))
        )
          throw new TypeError('other side closed')
        calls.push([method, u.pathname])
        const response = (body, status = 200) =>
          new Response(
            typeof body === 'string' || Buffer.isBuffer(body)
              ? body
              : JSON.stringify(body),
            { status }
          )
        if (u.pathname.endsWith('/releases') && method === 'GET')
          return mode === 'api-error'
            ? response('denied', 503)
            : response(mode === 'draft' ? [release(true)] : [])
        if (u.pathname.endsWith('/releases') && method === 'POST')
          return response(release(true), 201)
        if (u.pathname.endsWith('/assets') && method === 'POST') {
          if (mode === 'upload-error') return response('upload failed', 500)
          const bytes = Buffer.from(options.body)
          const name = u.searchParams.get('name')
          const item = {
            id: 100 + uploaded.length,
            name,
            size: bytes.length,
            state: 'uploaded',
            digest: `sha256:${hash(bytes)}`,
            bytes,
          }
          uploaded.push(item)
          return response(item, 201)
        }
        if (u.pathname.includes('/releases/tags/'))
          return response({
            ...release(false),
            id: mode === 'wrong-id' ? 43 : 42,
          })
        if (u.pathname.endsWith('/releases/42'))
          return response(release(method !== 'PATCH' && uploaded.length === 0))
        throw Error(`Unexpected ${method} ${url}`)
      }
      const context = {
        Buffer,
        AbortSignal,
        URL,
        console,
        fetch,
        process: {
          env: {
            GH_REPO: 'example/project',
            RELEASE_TAG: 'v0.0.0-example',
            SOURCE_COMMIT: 'a'.repeat(40),
          },
        },
        require(name) {
          if (name === 'node:fs')
            return {
              ...fs,
              readdirSync: (path, ...rest) =>
                fs.readdirSync(join(root, path), ...rest),
              lstatSync: (path) => fs.lstatSync(join(root, path)),
              readFileSync: (path, ...rest) =>
                fs.readFileSync(join(root, path), ...rest),
              mkdirSync: (path, ...rest) =>
                fs.mkdirSync(join(root, path), ...rest),
              writeFileSync: (path, ...rest) =>
                fs.writeFileSync(join(root, path), ...rest),
              copyFileSync: (a, b) =>
                fs.copyFileSync(join(root, a), join(root, b)),
            }
          if (name === 'node:crypto') return { createHash }
          if (name === 'node:child_process')
            return { execFileSync: (...args) => calls.push(['gh', args]) }
          throw Error(name)
        },
      }
      const execute = () =>
        vm.runInNewContext(`(async()=>{${script}\n})()`, context)
      if (['success', 'idle-socket'].includes(mode)) await execute()
      else await assert.rejects(execute, undefined, mode)
      if (
        [
          'draft',
          'api-error',
          'missing',
          'extra',
          'changed',
          'empty-bundle',
        ].includes(mode)
      )
        assert.ok(!calls.some(([method]) => method === 'POST'), mode)
      if (mode === 'api-error' || mode === 'upload-error')
        assert.equal(calls.length, mode === 'api-error' ? 1 : 3)
      if (['upload-error'].includes(mode))
        assert.ok(!calls.some(([method]) => method === 'PATCH'), mode)
      if (['success', 'idle-socket'].includes(mode)) {
        assert.equal(uploaded.length, 17)
        assert.equal(calls.filter(([method]) => method === 'gh').length, 18)
      }
    } finally {
      rmSync(root, { recursive: true, force: true })
    }
  }
})
test('acquires the locked workspace graph before offline SBOM resolution', () => {
  const script = readFileSync(
    new URL('./phase1-build.sh', import.meta.url),
    'utf8'
  )
  const acquisition = script.indexOf(
    'cargo metadata --locked --format-version 1 --filter-platform'
  )
  assert.ok(
    acquisition >= 0,
    'should populate the locked workspace dependency sources'
  )
  assert.ok(
    acquisition < script.indexOf('CARGO_NET_OFFLINE=true'),
    'should acquire before offline generation'
  )
})
// SPDX conversion retains the binary root and conservative package inventory.
const binary = Buffer.from('fictional binary')
function spdxFixture() {
  const root = {
    SPDXID: 'SPDXRef-Root',
    name: 'example.wasm',
    checksums: [
      {
        algorithm: 'SHA256',
        checksumValue: createHash('sha256').update(binary).digest('hex'),
      },
    ],
  }
  const component = {
    name: 'example-package',
    version: '1.0.0',
    purl: 'pkg:npm/example-package@1.0.0',
  }
  const original = {
    metadata: { component: { name: root.name } },
    components: [component],
  }
  const document = {
    spdxVersion: 'SPDX-2.3',
    SPDXID: 'SPDXRef-DOCUMENT',
    documentDescribes: [root.SPDXID],
    packages: [
      root,
      {
        SPDXID: 'SPDXRef-Package',
        name: component.name,
        versionInfo: component.version,
        externalRefs: [
          { referenceType: 'purl', referenceLocator: component.purl },
        ],
      },
    ],
    relationships: [
      {
        spdxElementId: 'SPDXRef-DOCUMENT',
        relationshipType: 'DESCRIBES',
        relatedSpdxElement: root.SPDXID,
      },
    ],
  }
  return { original, document }
}
test('SPDX preserves a binary-bound root and package versions/PURLs', () => {
  const { original, document } = spdxFixture()
  assert.equal(validateSpdx(document, binary, original), document)
  delete document.documentDescribes
  assert.doesNotThrow(() => validateSpdx(document, binary, original))
})
test('SPDX rejects root/hash ambiguity and lost package identities', () => {
  for (const mutate of [
    (d) => {
      d.spdxVersion = 'SPDX-2.2'
    },
    (d) => {
      d.documentDescribes.push('SPDXRef-Package')
    },
    (d) => {
      d.packages[0].checksums[0].checksumValue = '0'.repeat(64)
    },
    (d) => {
      d.packages[0].checksums.push(d.packages[0].checksums[0])
    },
    (d) => {
      d.packages.push(d.packages[0])
    },
    (d) => {
      d.packages.pop()
    },
    (d) => {
      d.packages[1].versionInfo = '2.0.0'
    },
    (d) => {
      d.packages[1].externalRefs = []
    },
  ]) {
    const { document, original } = spdxFixture()
    mutate(document)
    assert.throws(() => validateSpdx(document, binary, original))
  }
})
test('completes real converter package output with a binary-bound root', () => {
  const { original, document } = spdxFixture()
  document.packages.shift()
  delete document.documentDescribes
  delete document.relationships
  completeSpdx(document, binary, original)
  assert.deepEqual(document.documentDescribes, ['SPDXRef-Artifact'])
  assert.equal(document.packages.at(-1).filesAnalyzed, false)
  assert.throws(() => completeSpdx(document, binary, original), /Unexpected/)
})
test('post-publish retries distinguish transient failures and cap Retry-After', async () => {
  const code = publicationScript()
    .split('// BEGIN READ RETRIES')[1]
    ?.split('// END READ RETRIES')[0]
  assert.ok(code, 'should exercise the actual post-publish retry helper')
  for (const [error, failures, expected] of [
    [{ cause: { code: 'UND_ERR_SOCKET' } }, 1, [2000]],
    [{ status: 503 }, 9, [2000, 5000]],
    [{ status: 503, elapsed: 180000 }, 9, []],
    [{ status: 429, retryAfter: '30' }, 1, [10000]],
    [{ status: 429, retryAfter: '3' }, 1, [3000]],
    [{ status: 403 }, 9, []],
    [{ name: 'TimeoutError' }, 1, [2000]],
    [{ status: 429, retryAfter: 'Thu, 01 Jan 1970 00:00:03 GMT' }, 1, [3000]],
    [new Error('Published asset digest mismatch'), 9, []],
    [{ stderr: Buffer.from('HTTP 503: Service Unavailable') }, 1, [2000]],
    [{ stderr: Buffer.from('verification failed: HTTP 503') }, 9, []],
  ]) {
    const waits = [],
      logs = []
    let attempts = 0,
      now = 0
    const retry = vm.runInNewContext(`${code}; retryRead`, {
      Date: { now: () => now, parse: Date.parse },
      setTimeout: (resolve, ms) => {
        waits.push(ms)
        now += ms
        resolve()
      },
      console: { warn: (line) => logs.push(line) },
    })
    const operation = () => {
      now += error.elapsed ?? 0
      if (++attempts <= failures) throw error
      return 'verified'
    }
    if (failures === 1) assert.equal(await retry(operation), 'verified')
    else await assert.rejects(() => retry(operation))
    assert.deepEqual(waits, expected)
    assert.equal(attempts, expected.length + 1)
    assert.equal(logs.length, expected.length)
    assert.ok(logs.every((line) => /attempt [23]\/3/.test(line)))
  }
})
