import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { execFileSync } from 'node:child_process'
import { completeSpdx, validateSpdx, checkReleaseFormat } from './phase1.mjs'
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
import { isDeepStrictEqual } from 'node:util'
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
  for (const line of [
    workflow,
    readFileSync('.github/workflows/release.yml', 'utf8'),
  ]
    .join('\n')
    .split('\n')
    .filter((line) => /uses: .*@/.test(line))) {
    assert.match(line, /@[a-f0-9]{40} # v/)
  }
})
const assets = [
  'trusted-server-axum-x86_64-unknown-linux-gnu',
  'trusted-server-axum-x86_64-unknown-linux-musl',
  'trusted-server-axum-aarch64-unknown-linux-gnu',

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
    'stable',
    'annotated-tag',
    'wrong-tag-commit',
    'missing-tag',
    'malformed-tag',
    'download-corrupt',
    'download-missing',
    'download-extra',
    'download-manifest',
    'download-transient',
    'download-never',
    'publication-disabled',
    'prerelease-mismatch',
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
    'attestation-404-then-present',
    'attestation-after-two-waits',
    'attestation-404-never',
    'attestation-non-200',
    'attestation-201',
    'attestation-auth-error',
    'attestation-malformed',
    'attestation-wrong-tag-shape',
    'attestation-wrong-id',
    'attestation-wrong-repository',
    'attestation-wrong-source',
    'attestation-wrong-assets',
    'attestation-missing-asset',
    'attestation-extra-asset',
    'attestation-duplicate',
    'attestation-other-tag',
    'attestation-other-tag-before-current',
    'attestation-other-malformed',
    'attestation-other-unverifiable',
    'attestation-substituted-bundle',
    'attestation-verification-failure',
  ]) {
    const root = mkdtempSync(join(tmpdir(), 'phase1-publish-'))
    const calls = []
    const uploaded = []
    const stable = ['stable', 'attestation-other-tag'].includes(mode)
    const waits = [],
      ignored = []
    let downloads = 0,
      now = 0
    let polls = 0,
      observed = []
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
        tag_name: stable ? 'v1.0.0' : 'v0.0.0-example',
        prerelease: !stable && mode !== 'prerelease-mismatch',
        draft,
        immutable: mode !== 'mutable',
        assets: uploaded,
      })
      const attestation = (tag = release(false).tag_name) => {
        const statement = {
          _type: 'https://in-toto.io/Statement/v1',
          predicateType: 'https://in-toto.io/attestation/release/v0.2',
          predicate: { databaseId: '42', repository: 'example/project', tag },
          subject: [
            {
              uri: `pkg:github/example/project@${tag}`,
              digest: { sha1: 'a'.repeat(40) },
            },
            ...uploaded.map((a) => ({
              name: a.name,
              digest: { sha256: a.digest.slice(7) },
            })),
          ],
        }
        if (mode === 'attestation-wrong-id')
          statement.predicate.databaseId = '43'
        if (mode === 'attestation-wrong-repository')
          statement.predicate.repository = 'example/other'
        if (mode === 'attestation-wrong-source')
          statement.subject[0].digest.sha1 = 'b'.repeat(40)
        if (mode === 'attestation-wrong-assets')
          statement.subject[1].digest.sha256 = '0'.repeat(64)
        if (mode === 'attestation-missing-asset') statement.subject.pop()
        if (mode === 'attestation-extra-asset')
          statement.subject.push({
            name: 'unexpected',
            digest: { sha256: '0'.repeat(64) },
          })
        if (mode === 'attestation-wrong-tag-shape') statement.predicate.tag = 42
        return {
          initiator: 'github',
          bundle: {
            mediaType: 'application/vnd.dev.sigstore.bundle.v0.3+json',
            verificationMaterial: {},
            dsseEnvelope: {
              payloadType: 'application/vnd.in-toto+json',
              payload: Buffer.from(JSON.stringify(statement)).toString(
                'base64'
              ),
              signatures: [{ sig: 'fixture' }],
            },
          },
        }
      }
      const fetch = async (url, options = {}) => {
        const u = new URL(url)
        const method = options.method ?? 'GET'
        assert.equal(
          Boolean(options.signal),
          method === 'GET',
          `should time-bound every read and only reads: ${method} ${u.pathname}`
        )
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
        if (u.pathname.includes('/git/ref/tags/')) {
          assert.equal(method, 'GET')
          assert.equal(u.pathname.split('/').at(-1), release(false).tag_name)
          if (mode === 'missing-tag') return response({}, 404)
          if (mode === 'malformed-tag') return response({})
          return response({
            object: {
              type: mode === 'annotated-tag' ? 'tag' : 'commit',
              sha: ['annotated-tag', 'wrong-tag-commit'].includes(mode)
                ? 'b'.repeat(40)
                : 'a'.repeat(40),
            },
          })
        }
        if (u.pathname.includes('/attestations/')) {
          polls++
          assert.equal(method, 'GET')
          assert.equal(u.searchParams.get('predicate_type'), 'release')
          if (mode === 'attestation-non-200') return response({}, 503)
          if (mode === 'attestation-201') return response({}, 201)
          if (mode === 'attestation-auth-error') return response({}, 403)
          if (mode === 'attestation-malformed')
            return response({ attestations: null })
          const other = attestation('v1.0.0-rc.1')
          if (mode === 'attestation-other-malformed')
            other.bundle.dsseEnvelope.payload = '!invalid!'
          const delay =
            mode === 'attestation-404-then-present'
              ? 1
              : [
                    'attestation-after-two-waits',
                    'attestation-other-tag-before-current',
                  ].includes(mode)
                ? 2
                : mode === 'attestation-404-never'
                  ? 5
                  : 0
          if (polls <= delay && mode !== 'attestation-other-tag-before-current')
            return response({ message: 'Not Found' }, 404)
          observed = polls <= delay ? [] : [attestation()]
          if (mode === 'attestation-duplicate') observed.push(attestation())
          if (mode.startsWith('attestation-other-')) observed.unshift(other)
          return response({ attestations: observed })
        }
        if (u.pathname.endsWith('/releases') && method === 'GET')
          return mode === 'api-error'
            ? response('denied', 503)
            : response(mode === 'draft' ? [release(true)] : [])
        if (u.pathname.endsWith('/releases') && method === 'POST') {
          const creation = JSON.parse(options.body)
          assert.equal(creation.prerelease, !stable)
          assert.equal(creation.make_latest, stable ? 'legacy' : 'false')
          const body = creation.body
          assert.match(body, /--signer-workflow.*release-build\.yml/)
          assert.match(body, /--cert-identity.*release-build\.yml@refs\/tags/)
          assert.ok(body.includes('COMMIT=' + 'a'.repeat(40)))
          for (const asset of assets) assert.ok(body.includes(asset))
          const normalize = (code) =>
            code.replace(/^(REPO|TAG|COMMIT|ASSETS)=.*$/gm, '$1=<pin>').trim()
          const guide = readFileSync(
            'docs/guide/release-verification.md',
            'utf8'
          ).match(
            /<!-- release-verification:start -->\n\n```bash\n([\s\S]*?)\n```\n\n<!-- release-verification:end -->/
          )
          assert.ok(guide, 'should mark the canonical verification block')
          assert.equal(
            normalize(body.match(/```bash\n([\s\S]*?)\n```/)[1]),
            normalize(guide[1]),
            'release commands should match the guide'
          )
          execFileSync('bash', ['-n'], {
            input: body.match(/```bash\n([\s\S]*?)\n```/)[1],
          })
          return response(release(true), 201)
        }
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
        Date: { now: () => now, parse: Date.parse },
        JSON,
        AbortSignal,
        URL,
        console: {
          log: (...args) => {
            if (args[0] === 'Ignored release attestation for other tag:')
              ignored.push(args[1])
          },
          warn: () => {},
        },
        setTimeout: (resolve, delay) => {
          waits.push(delay)
          now += delay
          resolve()
        },
        fetch,
        process: {
          env: {
            GH_REPO: 'example/project',
            RELEASE_TAG: stable ? 'v1.0.0' : 'v0.0.0-example',
            RELEASE_PUBLICATION_ENABLED:
              mode === 'publication-disabled' ? 'false' : 'true',
            SOURCE_COMMIT: 'a'.repeat(40),
          },
        },
        require(name) {
          if (name === 'node:fs')
            return {
              ...fs,
              mkdtempSync: (prefix) =>
                fs.mkdtempSync(join(root, prefix)).slice(root.length + 1),
              rmSync: (path, options) => fs.rmSync(join(root, path), options),
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
          if (name === 'node:util') return { isDeepStrictEqual }
          if (name === 'node:child_process')
            return {
              execFileSync: (...args) => {
                calls.push(['gh', args])
                if (args[1][1] === 'download') {
                  downloads++
                  assert.ok(
                    args[2].timeout > 10000 && args[2].timeout <= 60000,
                    'should allow bulk transfers within the shared deadline'
                  )
                  const dir = args[1][args[1].indexOf('--dir') + 1]
                  if (
                    mode === 'download-never' ||
                    (mode === 'download-transient' && downloads === 1)
                  )
                    throw Object.assign(Error('timeout'), { code: 'ETIMEDOUT' })
                  for (const item of uploaded)
                    fs.writeFileSync(join(root, dir, item.name), item.bytes)
                  if (mode === 'download-corrupt')
                    fs.writeFileSync(join(root, dir, assets[0]), 'corrupt')
                  if (mode === 'download-manifest')
                    fs.writeFileSync(join(root, dir, 'SHA256SUMS'), 'corrupt')
                  if (mode === 'download-missing')
                    fs.unlinkSync(join(root, dir, assets[0]))
                  if (mode === 'download-extra')
                    fs.writeFileSync(join(root, dir, 'extra'), 'extra')
                  return Buffer.from('downloaded')
                }
                if (args[1][1] !== 'verify')
                  return Buffer.from('verified asset')
                const tag = args[1].at(-1)
                if (
                  mode === 'attestation-verification-failure' ||
                  (mode === 'attestation-other-unverifiable' &&
                    tag === 'v1.0.0-rc.1')
                )
                  throw Error('Signature verification failed')
                assert.ok(
                  polls > 0,
                  'should read release-attestation availability before verification'
                )
                const record = observed.find(
                  (r) =>
                    JSON.parse(
                      Buffer.from(r.bundle.dsseEnvelope.payload, 'base64')
                    ).predicate.tag === tag
                )
                const bundle = structuredClone(record.bundle)
                if (mode === 'attestation-substituted-bundle')
                  bundle.dsseEnvelope.signatures[0].sig = 'different'
                return Buffer.from(JSON.stringify({ attestation: { bundle } }))
              },
            }
          throw Error(name)
        },
      }
      const execute = () =>
        vm.runInNewContext(`(async()=>{${script}\n})()`, context)
      const successful = [
        'success',
        'idle-socket',
        'stable',
        'download-transient',
        'attestation-404-then-present',
        'attestation-after-two-waits',
        'attestation-other-tag',
        'attestation-other-tag-before-current',
        'attestation-other-unverifiable',
      ].includes(mode)
      if (successful) await execute()
      else
        await assert.rejects(
          execute,
          mode === 'attestation-404-never'
            ? /unavailable after five checks/
            : mode.startsWith('download-') && mode !== 'download-never'
              ? /mismatch|Unexpected artifact contents|ENOENT/
              : undefined,
          mode
        )
      if (
        [
          'draft',
          'annotated-tag',
          'wrong-tag-commit',
          'missing-tag',
          'malformed-tag',
          'publication-disabled',
          'api-error',
          'missing',
          'extra',
          'changed',
          'empty-bundle',
        ].includes(mode)
      )
        assert.ok(
          !calls.some(([method]) => ['POST', 'PATCH'].includes(method)),
          mode
        )
      if (mode === 'api-error' || mode === 'upload-error')
        assert.equal(calls.length, mode === 'api-error' ? 2 : 4)
      if (['upload-error'].includes(mode))
        assert.ok(!calls.some(([method]) => method === 'PATCH'), mode)
      if (successful) {
        assert.equal(uploaded.length, 29)
        assert.equal(
          calls.filter(
            ([method, args]) => method === 'gh' && args[1][1] !== 'download'
          ).length,
          30
        )
        assert.ok(
          !calls.some(
            ([method, args]) =>
              method === 'gh' &&
              args[1][1] === 'verify' &&
              args[1].at(-1) === 'v1.0.0-rc.1'
          ),
          'should not verify another tag release attestation'
        )
      }
      assert.equal(
        fs
          .readdirSync(root)
          .some((name) => name.startsWith('published-download-')),
        false,
        'should clean every download attempt'
      )
      if (mode.startsWith('download-')) {
        assert.equal(
          downloads,
          mode === 'download-transient' ? 2 : mode === 'download-never' ? 3 : 1
        )
        assert.deepEqual(
          waits,
          mode === 'download-transient'
            ? [2000]
            : mode === 'download-never'
              ? [2000, 5000]
              : []
        )
      }
      if (mode.startsWith('attestation-')) {
        const expectedWaits =
          mode === 'attestation-404-never'
            ? [2000, 5000, 10000, 20000]
            : mode === 'attestation-404-then-present'
              ? [2000]
              : [
                    'attestation-after-two-waits',
                    'attestation-other-tag-before-current',
                  ].includes(mode)
                ? [2000, 5000]
                : []
        assert.deepEqual(waits, expectedWaits, mode)
        assert.equal(polls, expectedWaits.length + 1, mode)
        assert.equal(
          calls.filter(
            ([method, path]) => method === 'POST' && path.endsWith('/releases')
          ).length,
          1,
          mode
        )
        assert.equal(
          calls.filter(
            ([method, path]) => method === 'POST' && path.endsWith('/assets')
          ).length,
          29,
          mode
        )
        assert.equal(
          calls.filter(([method]) => method === 'PATCH').length,
          1,
          mode
        )
        assert.deepEqual(
          ignored,
          ['attestation-other-tag', 'attestation-other-unverifiable'].includes(
            mode
          )
            ? ['"v1.0.0-rc.1"']
            : mode === 'attestation-other-tag-before-current'
              ? Array(3).fill('"v1.0.0-rc.1"')
              : [],
          mode
        )
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

test('rejects invalid wasm, GNU symbols above 2.34 and dynamic musl', () => {
  const wasm = Buffer.from('0061736d01000000', 'hex')
  checkReleaseFormat(wasm, 'wasm32-wasip1')
  for (const bytes of [
    wasm.subarray(0, 7),
    Buffer.alloc(8),
    Buffer.from('0061736d02000000', 'hex'),
  ])
    assert.throws(() => checkReleaseFormat(bytes, 'wasm32-wasip1'), /wasm/)
  const elf = Buffer.from('7f454c46', 'hex')
  const gnu = 'x86_64-unknown-linux-gnu',
    musl = 'x86_64-unknown-linux-musl'
  checkReleaseFormat(elf, gnu, 'GLIBC_2.2.5 GLIBC_2.34')
  for (const versions of [
    'GLIBC_2.35',
    'GLIBC_3.0',
    'GLIBC_2.34.1',
    'GLIBC_ABI_DT_RELR',
  ])
    assert.throws(() => checkReleaseFormat(elf, gnu, versions), /GLIBC/)
  checkReleaseFormat(elf, musl, 'There is no dynamic section')
  for (const dynamic of ['INTERP', '(NEEDED) libgcc_s.so.1'])
    assert.throws(() => checkReleaseFormat(elf, musl, dynamic), /static/)
  assert.throws(() => checkReleaseFormat(wasm, gnu, 'GLIBC_2.34'), /ELF/)
})
test('audit resolves raw and nested concatenated deployment inputs', () => {
  for (const source of [
    'include_str!(concat!("trusted-", "server.toml"))',
    'include_bytes!(concat!(".en", r#"v.production"#))',
    'env!(r#"TRUSTED_SERVER__EC__PASSPHRASE"#)',
    'option_env!(concat!("EDGEZERO__", concat!("STORE", "S")))',
  ])
    assert.throws(() => checkSource('src/lib.rs', source), /compile-time/)
  for (const source of [
    'std::env::var("TRUSTED_SERVER__EC__PASSPHRASE")',
    'std::env::var_os(concat!("EDGEZERO__", "STORE"))',
    'const SECRET: &str = r#"TRUSTED_SERVER__SECRET"#; env::var(SECRET)',
  ])
    assert.throws(() => checkSource('build.rs', source), /compile-time/)
  for (const source of [
    'include_str!(concat!(env!("OUT_DIR"), "/generated.rs"))',
    'include_str!("trusted-server.example.toml")',
    '// env!("EDGEZERO__SECRET")\nlet product = "literal";',
    '/* include_str!("trusted-server.toml") */',
  ])
    assert.doesNotThrow(() => checkSource('src/lib.rs', source))
  assert.doesNotThrow(() =>
    checkSource('build.rs', 'env::var("TRUSTED_SERVER__GIT_VERSION")')
  )
  assert.doesNotThrow(() =>
    checkSource('src/lib.rs', 'std::env::var("EDGEZERO__ADAPTER__HOST")')
  )
})
test('rejects non-Mach-O, wrong CPU and newer or absent macOS floor', () => {
  const macho = (cpu = 0x0100000c, minimum = 0x000d0000) => {
    const b = Buffer.alloc(56)
    b.writeUInt32LE(0xfeedfacf, 0)
    b.writeUInt32LE(cpu, 4)
    b.writeUInt32LE(1, 16)
    b.writeUInt32LE(24, 20)
    b.writeUInt32LE(0x32, 32)
    b.writeUInt32LE(24, 36)
    b.writeUInt32LE(1, 40)
    b.writeUInt32LE(minimum, 44)
    return b
  }
  assert.doesNotThrow(() => checkReleaseFormat(macho(), 'aarch64-apple-darwin'))
  assert.doesNotThrow(() =>
    checkReleaseFormat(macho(0x01000007), 'x86_64-apple-darwin')
  )
  for (const b of [
    Buffer.alloc(8),
    macho(0x01000007),
    macho(0x0100000c, 0x000e0000),
    macho().subarray(0, 40),
  ])
    assert.throws(
      () => checkReleaseFormat(b, 'aarch64-apple-darwin'),
      /Mach-O|macOS/
    )
})
test('workflows select exactly the descriptive cohort and restrict actual build jobs', () => {
  const cohort = JSON.parse(readFileSync('scripts/release/release-cohort.json'))
  const names = cohort.artifacts.map((a) => a.name).sort()
  const build = readFileSync('.github/workflows/release-build.yml', 'utf8')
  const release = readFileSync('.github/workflows/release.yml', 'utf8')
  assert.deepEqual(
    [...build.matchAll(/^            asset: (\S+)/gm)].map((m) => m[1]).sort(),
    names
  )
  const sign = build.split('\n  sign:')[1]
  for (const name of names) assert.equal(sign.split(name).length - 1, 1)
  const published = publicationScript().match(/const names = (\[[^;]+\]);/)[1]
  assert.deepEqual(Array.from(vm.runInNewContext(published)).sort(), names)
  for (const prefix of ['release-', 'signed-'])
    assert.deepEqual(
      [
        ...release.matchAll(
          new RegExp('          name: ' + prefix + '(\\S+)', 'g')
        ),
      ]
        .map((m) => m[1])
        .sort(),
      names
    )
  const actualBuild = build.split('\n  build:')[1].split('\n  sign:')[0]
  assert.doesNotMatch(actualBuild, /id-token|attestations:/)
  // Signing and publication need a push of a `v*` tag and the enablement variable.
  // Anchor to the job-level indentation so a matching step-level `if:` cannot satisfy it.
  const gate =
    "\n    if: github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v') && vars.RELEASE_PUBLICATION_ENABLED == 'true'\n"
  assert.ok(
    release.split('\n  release:')[1]?.includes(gate),
    'should gate publication on a v* tag push and enablement'
  )
  assert.ok(
    build.split('\n  sign:')[1]?.split('\n  release:')[0].includes(gate),
    'should gate signing on a v* tag push and enablement'
  )
})
test('release preflight rejects inherited controls in an isolated script fixture', () => {
  const root = mkdtempSync(join(tmpdir(), 'phase1-preflight-'))
  const script = join(root, 'scripts/release/phase1-build.sh')
  const bin = join(root, 'bin'),
    marker = join(root, 'tools-ran')
  const source = 'a'.repeat(40)
  const args = [
    'trusted-server-cli',
    'x86_64-unknown-linux-gnu',
    'ts-x86_64-unknown-linux-gnu',
  ]
  try {
    mkdirSync(join(root, 'scripts/release'), { recursive: true })
    mkdirSync(bin)
    fs.copyFileSync('scripts/release/phase1-build.sh', script)
    const stub = (name, body) =>
      writeFileSync(join(bin, name), '#!/bin/sh\n' + body + '\n', {
        mode: 0o755,
      })
    stub(
      'git',
      `test "$*" = "rev-parse HEAD" || exit 96; printf '%s\\n' "$PREFLIGHT_FIXTURE_COMMIT"`
    )
    stub('printenv', 'exec /usr/bin/printenv "$@"')
    for (const tool of [
      'node',
      'npm',
      'docker',
      'cargo',
      'rustup',
      'curl',
      'sudo',
    ])
      stub(tool, `printf '%s\\n' "$0" >> "$PREFLIGHT_TOOL_LOG"; exit 97`)
    const execute = (extra) => {
      try {
        execFileSync('/bin/bash', [script, ...args], {
          cwd: root,
          stdio: 'pipe',
          env: {
            PATH: bin,
            RUNNER_TEMP: root,
            SOURCE_COMMIT: source,
            PREFLIGHT_FIXTURE_COMMIT: source,
            PREFLIGHT_TOOL_LOG: marker,
            ...extra,
          },
        })
        assert.fail('should stop before any real tool executes')
      } catch (error) {
        return error
      }
    }
    for (const variable of [
      'TSJS_SKIP_BUILD',
      'TSJS_PREBUILT_DIR',
      'TSJS_TEST',
    ]) {
      const error = execute({ [variable]: '' })
      assert.equal(error.status, 1)
      assert.match(error.stderr.toString(), new RegExp('Refusing ' + variable))
      assert.equal(fs.existsSync(marker), false)
    }
    for (const invalid of ['0'.repeat(40), 'A'.repeat(40), 'invalid'])
      assert.equal(execute({ SOURCE_COMMIT: invalid }).status, 1)
    assert.equal(fs.existsSync(marker), false)
    assert.equal(
      execute({}).status,
      97,
      'should reach the safe node stub when preflight passes'
    )
    assert.equal(readFileSync(marker, 'utf8').trim(), join(bin, 'node'))
    assert.equal(fs.existsSync(join(root, 'trusted-server.toml')), false)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})
test('SPDX identifies all producing tools and normalizes license spacing', () => {
  const { document, original } = spdxFixture()
  document.documentDescribes = []
  document.relationships = []
  document.packages = document.packages.filter(
    (p) => p.SPDXID !== 'SPDXRef-Root'
  )
  document.packages[0].licenseDeclared = 'Apache-2.0  OR  MIT'
  original.metadata.component.version = 'v1.2.3-example'
  const output = completeSpdx(document, binary, original)
  assert.equal(
    output.packages.find((p) => p.SPDXID === 'SPDXRef-Artifact').versionInfo,
    'v1.2.3-example'
  )
  assert.equal(output.packages[0].licenseDeclared, 'Apache-2.0 OR MIT')
  assert.ok(output.creationInfo.creators.includes('Tool: cyclonedx-cli-0.33.1'))
  assert.ok(
    output.creationInfo.creators.includes('Tool: trusted-server-phase1')
  )
})
test('audit handles Rust escapes and every macro delimiter', () => {
  for (const source of [
    String.raw`env!("\x54RUSTED_SERVER__SECRET")`,
    String.raw`env!("\u{54}RUSTED_SERVER__SECRET")`,
    String.raw`include_str!("\x2eenv.production")`,
    'env![concat!["TRUSTED_SERVER__", "SECRET"]]',
    'env!{concat!{"TRUSTED_SERVER__", "SECRET"}}',
  ])
    assert.throws(() => checkSource('src/lib.rs', source), /compile-time/)
})
test('audit handles ordinary char literals and generated include prefixes', () => {
  assert.doesNotThrow(() =>
    checkSource('src/lib.rs', `const QUOTE: char = '"';`)
  )
  assert.throws(
    () =>
      checkSource(
        'src/lib.rs',
        'include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/trusted-server.toml"))'
      ),
    /compile-time/
  )
  assert.doesNotThrow(() =>
    checkSource(
      'src/lib.rs',
      'include_str!(concat!(env!("OUT_DIR"), "/generated.rs"))'
    )
  )
})

test('post-publish reads cannot succeed after the shared deadline', async () => {
  const code = publicationScript()
    .split('// BEGIN READ RETRIES')[1]
    .split('// END READ RETRIES')[0]
  let now = 0
  const retry = vm.runInNewContext(`${code}; retryRead`, {
    Date: { now: () => now, parse: Date.parse },
    console: {
      warn: () => assert.fail('should not retry deadline exhaustion'),
    },
  })
  await assert.rejects(
    () =>
      retry(() => {
        now = 180000
        return 'late success'
      }),
    /window exhausted/
  )
})

test('SPDX creation timestamps use whole-second UTC without changing the instant second', () => {
  for (const created of [
    '2026-01-01T12:34:56.9290415Z',
    '2026-01-01T12:34:56.000Z',
    '2026-01-01T12:34:56Z',
  ]) {
    const { document, original } = spdxFixture()
    document.documentDescribes = []
    document.relationships = []
    document.packages = document.packages.filter(
      (p) => p.SPDXID !== 'SPDXRef-Root'
    )
    document.creationInfo = { created, creators: ['Tool: example-converter'] }
    const output = completeSpdx(document, binary, original)
    assert.equal(output.creationInfo.created, '2026-01-01T12:34:56Z')
    assert.ok(output.creationInfo.creators.includes('Tool: example-converter'))
  }
})

test('release build cleans owned config and inventory on success and failure', () => {
  for (const mode of [
    'success',
    'config-write-failure',
    'build-failure',
    'generator-failure',
    'converter-failure',
    'existing-config',
    'existing-inventory',
    'inventory-symlink',
    'config-symlink',
  ]) {
    const root = mkdtempSync(join(tmpdir(), 'phase1-cleanup-'))
    const bin = join(root, 'bin')
    const inventory = join(root, 'crates/trusted-server-cli/phase1.json')
    const config = join(root, 'trusted-server.toml')
    try {
      mkdirSync(bin)
      mkdirSync(join(root, 'crates/trusted-server-cli'), { recursive: true })
      const script = join(root, 'build.sh')
      fs.copyFileSync('scripts/release/phase1-build.sh', script)
      const stub = (name, body) =>
        writeFileSync(join(bin, name), '#!/bin/bash\nset -eu\n' + body + '\n', {
          mode: 0o755,
        })
      stub(
        'git',
        'if [[ "$1" == rev-parse ]]; then printf "%s\\n" "$SOURCE_COMMIT"; fi'
      )
      stub('node', 'exit 0')
      stub('npm', 'echo "{}"')
      stub('shasum', 'exit 0')
      stub('tar', 'exit 0')
      stub(
        'cat',
        'if [[ "$FIXTURE_MODE" == config-write-failure ]]; then printf partial; exit 97; fi; exec /bin/cat "$@"'
      )
      stub(
        'cargo',
        `case "$1" in
        build)
          [[ "$FIXTURE_MODE" != build-failure ]] || exit 97
          mkdir -p "$CARGO_TARGET_DIR/aarch64-apple-darwin/release"
          printf binary > "$CARGO_TARGET_DIR/aarch64-apple-darwin/release/ts" ;;
        install)
          mkdir -p "$RUNNER_TEMP/phase1-cyclonedx/install/bin"
          printf '%s\\n' '#!/bin/bash' 'printf "{}" > crates/trusted-server-cli/phase1.json' 'if [[ "$FIXTURE_MODE" == generator-failure ]]; then exit 98; fi' > "$RUNNER_TEMP/phase1-cyclonedx/install/bin/cargo-cyclonedx"
          chmod +x "$RUNNER_TEMP/phase1-cyclonedx/install/bin/cargo-cyclonedx" ;;
        metadata) echo '{}' ;;
        *) exit 96 ;;
      esac`
      )
      stub(
        'curl',
        `while [[ "$1" != --output ]]; do shift; done
        printf '%s\\n' '#!/bin/bash' 'if [[ "$FIXTURE_MODE" == converter-failure ]]; then exit 99; fi' > "$2"`
      )
      if (mode === 'existing-config') writeFileSync(config, 'operator config')
      if (mode === 'config-symlink')
        fs.symlinkSync('missing-user-config', config)
      if (mode === 'existing-inventory')
        writeFileSync(inventory, 'operator inventory')
      if (mode === 'inventory-symlink')
        fs.symlinkSync('missing-user-file', inventory)
      let error
      try {
        execFileSync(
          '/bin/bash',
          [
            script,
            'trusted-server-cli',
            'aarch64-apple-darwin',
            'ts-aarch64-apple-darwin',
          ],
          {
            cwd: root,
            stdio: 'pipe',
            env: {
              PATH: `${bin}:/usr/bin:/bin`,
              RUNNER_TEMP: root,
              SOURCE_COMMIT: 'a'.repeat(40),
              FIXTURE_MODE: mode,
            },
          }
        )
      } catch (failure) {
        error = failure
      }
      const expectedStatus =
        {
          success: undefined,
          'config-write-failure': 97,
          'build-failure': 97,
          'generator-failure': 98,
          'converter-failure': 99,
        }[mode] ?? (mode === 'success' ? undefined : 1)
      assert.equal(error?.status, expectedStatus, mode)
      if (mode === 'config-symlink')
        assert.equal(fs.readlinkSync(config), 'missing-user-config')
      assert.equal(
        fs.existsSync(config),
        mode === 'existing-config',
        `${mode}: config cleanup`
      )
      if (mode === 'existing-config')
        assert.equal(readFileSync(config, 'utf8'), 'operator config')
      if (mode === 'existing-inventory')
        assert.equal(readFileSync(inventory, 'utf8'), 'operator inventory')
      else if (mode === 'inventory-symlink')
        assert.equal(fs.readlinkSync(inventory), 'missing-user-file')
      else
        assert.equal(
          fs.existsSync(inventory),
          false,
          `${mode}: inventory cleanup`
        )
    } finally {
      rmSync(root, { recursive: true, force: true })
    }
  }
})
