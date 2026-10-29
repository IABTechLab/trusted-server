import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { gzipSync, gunzipSync } from 'node:zlib'
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
import {
  ordinaryApi,
  ordinaryDownload,
  acquireOrdinaryTransfers,
} from './phase1.mjs'

test('ordinary transport preserves raw metadata and uses only the fixed authenticated GET', async () => {
  const calls = []
  const bytes = '{"total_count":0,"jobs":[],"same":1,"same":2}'
  const fetched = await ordinaryApi(
    '/repos/example/release/actions/runs/17/attempts/2/jobs?per_page=100',
    'fictional-token',
    async (url, options) => {
      calls.push({ url, options })
      return new Response(bytes)
    }
  )
  assert.equal(fetched.toString(), bytes)
  assert.equal(calls.length, 1)
  assert.equal(calls[0].options.method, 'GET')
  assert.equal(calls[0].options.redirect, 'manual')
  assert.equal(calls[0].options.headers.Authorization, 'Bearer fictional-token')
  // Assert the fixed metadata origin without permitting arbitrary destinations.
  assert.equal(new URL(calls[0].url).origin, 'https://api.github.com') // allow-domain: api.github.com
})

test('ordinary transport rejects metadata failures and oversized responses without leaking details', async () => {
  let discarded = 0
  await assert.rejects(
    ordinaryApi(
      '/repos/example/release/actions/runs/17/artifacts?per_page=100',
      'fictional-token',
      async () =>
        new Response(
          new ReadableStream({
            cancel() {
              discarded++
            },
          }),
          { status: 403 }
        )
    )
  )
  assert.equal(discarded, 1)
  for (const response of [
    new Response('fictional-sensitive-body', { status: 403 }),
    new Response('fictional-sensitive-body', {
      status: 302,
      headers: { location: 'https://example.com/fictional-secret' },
    }),
    new Response(Buffer.alloc(1024 * 1024 + 1)),
  ])
    await assert.rejects(
      ordinaryApi(
        '/repos/example/release/actions/runs/17/artifacts?per_page=100',
        'fictional-token',
        async () => response
      ),
      (error) =>
        !/fictional-sensitive|fictional-secret|fictional-token/.test(
          error.message
        )
    )
  let called = false
  await assert.rejects(
    ordinaryApi(
      '/repos/example/release/releases',
      'fictional-token',
      async () => {
        called = true
      }
    )
  )
  assert.equal(called, false)
  for (const path of [
    '/repos/../release/actions/runs/17/artifacts?per_page=100',
    '/repos/example/../actions/runs/17/artifacts?per_page=100',
  ]) {
    await assert.rejects(
      ordinaryApi(path, 'fictional-token', async () => {
        called = true
      })
    )
    assert.equal(called, false)
  }
  await assert.rejects(
    ordinaryApi(
      '/repos/example/release/actions/runs/17/artifacts?per_page=100',
      'fictional-token',
      async () => {
        throw Error('https://example.com/fictional-secret fictional-token')
      }
    ),
    (error) => !/fictional-secret|fictional-token/.test(error.message)
  )
})

test('ordinary transport downloads exact archive bytes without forwarding the API token', async () => {
  const directory = fs.realpathSync(
    mkdtempSync(join(tmpdir(), 'ordinary-transport-'))
  )
  const bytes = Buffer.from('fictional archive bytes')
  const archive = {
    id: 41,
    size: bytes.length,
    sha256: createHash('sha256').update(bytes).digest('hex'),
    file: 'canonical.zip',
  }
  const calls = []
  try {
    await ordinaryDownload(
      {
        repository: 'example/release',
        archive,
        token: 'fictional-token',
        directory,
      },
      async (url, options) => {
        calls.push({ url, options })
        return calls.length === 1
          ? new Response(null, {
              status: 302,
              headers: {
                location: 'https://example.com/archive?fictional-secret',
              },
            })
          : new Response(bytes)
      }
    )
    assert.deepEqual(readFileSync(join(directory, archive.file)), bytes)
    assert.equal(calls.length, 2)
    assert.match(calls[0].url, /\/actions\/artifacts\/41\/zip$/)
    assert.equal(
      calls[0].options.headers.Authorization,
      'Bearer fictional-token'
    )
    assert.equal(calls[1].options.headers?.Authorization, undefined)
    assert.equal(calls[1].options.redirect, 'error')
  } finally {
    rmSync(directory, { recursive: true, force: true })
  }
})

test('ordinary transport rejects wrong archive hashes and reused output paths', async () => {
  const directory = fs.realpathSync(
    mkdtempSync(join(tmpdir(), 'ordinary-transport-'))
  )
  const archive = {
    id: 41,
    size: 3,
    sha256: 'a'.repeat(64),
    file: 'canonical-observation.zip',
  }
  let calls = 0
  const fetcher = async () =>
    ++calls % 2 === 1
      ? new Response(null, {
          status: 302,
          headers: { location: 'https://example.com/archive?fictional-secret' },
        })
      : new Response('bad')
  try {
    await assert.rejects(
      ordinaryDownload(
        {
          repository: 'example/release',
          archive,
          token: 'fictional-token',
          directory,
        },
        fetcher
      ),
      /archive/
    )
    writeFileSync(join(directory, archive.file), 'retained')
    await assert.rejects(
      ordinaryDownload(
        {
          repository: 'example/release',
          archive,
          token: 'fictional-token',
          directory,
        },
        fetcher
      )
    )
    assert.equal(
      readFileSync(join(directory, archive.file), 'utf8'),
      'retained'
    )
  } finally {
    rmSync(directory, { recursive: true, force: true })
  }
})

test('ordinary transport stops before ZIP requests when current-run binding fails', async () => {
  const root = fs.realpathSync(
    mkdtempSync(join(tmpdir(), 'ordinary-transport-'))
  )
  const input = join(root, 'input')
  mkdirSync(input)
  const calls = []
  try {
    await assert.rejects(
      acquireOrdinaryTransfers(
        {
          producer: '/fictional/producer',
          input,
          archives: join(root, 'archives'),
          repository: 'example/release',
          runId: '17',
          attempt: '2',
          token: 'fictional-token',
        },
        async (url) => {
          calls.push(url)
          return new Response('{"total_count":0}')
        },
        () => {
          assert.equal(
            readFileSync(join(input, 'jobs.json'), 'utf8'),
            '{"total_count":0}'
          )
          assert.equal(
            readFileSync(join(input, 'artifacts.json'), 'utf8'),
            '{"total_count":0}'
          )
          throw Error('rejected current-run metadata')
        }
      ),
      /current-run/
    )
    assert.equal(calls.length, 2)
    assert.ok(calls.every((url) => !url.endsWith('/zip')))
    assert.equal(fs.existsSync(join(root, 'archives')), false)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('ordinary transport rejects unsafe redirects before any blob request', async () => {
  for (const location of [
    'http://example.com/archive',
    'https://user:fictional-secret@example.com/archive',
    'https://example.com:444/archive',
    'not-a-url',
  ]) {
    const root = fs.realpathSync(
      mkdtempSync(join(tmpdir(), 'ordinary-transport-'))
    )
    let calls = 0
    try {
      await assert.rejects(
        ordinaryDownload(
          {
            repository: 'example/release',
            token: 'fictional-token',
            directory: root,
            archive: {
              id: 41,
              file: 'canonical.zip',
              size: 1,
              sha256: 'a'.repeat(64),
            },
          },
          async () => {
            calls++
            return new Response(null, { status: 302, headers: { location } })
          }
        ),
        (error) => !error.message.includes('fictional-secret')
      )
      assert.equal(calls, 1)
    } finally {
      rmSync(root, { recursive: true, force: true })
    }
  }
})

test('ordinary transport enforces observation and body bounds and aborts before fetching', async () => {
  let oversizedCalled = false
  await assert.rejects(
    ordinaryDownload(
      {
        repository: 'example/release',
        token: 'fictional-token',
        directory: '/unused',
        archive: {
          id: 41,
          file: 'canonical-observation.zip',
          size: 2 * 1024 * 1024 + 1,
          sha256: 'a'.repeat(64),
        },
      },
      async () => {
        oversizedCalled = true
      }
    )
  )
  assert.equal(oversizedCalled, false)
  for (const body of ['short', 'longer-than-the-bound']) {
    const root = fs.realpathSync(
      mkdtempSync(join(tmpdir(), 'ordinary-transport-'))
    )
    let calls = 0
    try {
      await assert.rejects(
        ordinaryDownload(
          {
            repository: 'example/release',
            token: 'fictional-token',
            directory: root,
            archive: {
              id: 41,
              file: 'canonical-observation.zip',
              size: 8,
              sha256: 'a'.repeat(64),
            },
          },
          async () =>
            ++calls === 1
              ? new Response(null, {
                  status: 302,
                  headers: { location: 'https://example.com/archive' },
                })
              : new Response(body)
        )
      )
      assert.equal(calls, 2)
      assert.ok(fs.statSync(join(root, 'canonical-observation.zip')).size <= 8)
    } finally {
      rmSync(root, { recursive: true, force: true })
    }
  }
  let called = false
  await assert.rejects(
    ordinaryApi(
      '/repos/example/release/actions/runs/17/artifacts?per_page=100',
      'fictional-token',
      async () => {
        called = true
      },
      AbortSignal.abort()
    )
  )
  assert.equal(called, false)
})

test('ordinary transport acquires exactly the four Rust-bound archives in fixed order', async () => {
  const root = fs.realpathSync(
    mkdtempSync(join(tmpdir(), 'ordinary-transport-'))
  )
  const input = join(root, 'input'),
    archives = join(root, 'archives')
  mkdirSync(input)
  const bytes = Buffer.from('fictional bound ZIP bytes')
  const names = [
    'canonical.zip',
    'canonical-observation.zip',
    'reproduction.zip',
    'reproduction-observation.zip',
  ]
  let planned = false,
    calls = 0
  try {
    await acquireOrdinaryTransfers(
      {
        producer: '/fictional/producer',
        input,
        archives,
        repository: 'example/release',
        runId: '17',
        attempt: '2',
        token: 'fictional-token',
      },
      async (url, options) => {
        calls++
        assert.equal(options.method, 'GET')
        if (url.endsWith('per_page=100'))
          return new Response('{"total_count":0}')
        assert.equal(planned, true)
        // The mock distinguishes the fixed authenticated API from blob reads.
        const apiOrigin = 'https://api.github.com/' // allow-domain: api.github.com
        if (url.startsWith(apiOrigin))
          return new Response(null, {
            status: 302,
            headers: { location: 'https://example.com/archive' },
          })
        assert.equal(options.headers?.Authorization, undefined)
        return new Response(bytes)
      },
      (producer, metadataRoot) => {
        assert.equal(producer, '/fictional/producer')
        assert.equal(metadataRoot, input)
        assert.equal(fs.existsSync(join(input, 'artifacts.json')), true)
        planned = true
        return {
          version: 1,
          archives: names.map((file, index) => ({
            file,
            id: index + 41,
            size: bytes.length,
            sha256: createHash('sha256').update(bytes).digest('hex'),
          })),
        }
      }
    )
    assert.equal(calls, 10)
    assert.deepEqual(fs.readdirSync(archives).sort(), [...names].sort())
    for (const name of names)
      assert.deepEqual(readFileSync(join(archives, name)), bytes)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('checked JS inventory metadata is confined to its exact build script', () => {
  const source = 'option_env!("TRUSTED_SERVER__TSJS_PREBUILT_INVENTORY")'
  assert.doesNotThrow(() =>
    checkSource('crates/trusted-server-js/build.rs', source)
  )
  for (const file of [
    'crates/example/build.rs',
    'crates/trusted-server-js/src/lib.rs',
    'crates/trusted-server-js-other/build.rs',
    'other/crates/trusted-server-js/build.rs',
  ])
    assert.throws(() => checkSource(file, source), /compile-time/)
})
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

test('isolated signing accepts only bound 3/2 input sets and retains exact provenance subjects', () => {
  const workflow = readFileSync('.github/workflows/release-build.yml', 'utf8')
  const match = workflow.match(
    /          \/\/ BEGIN SIGNING INPUTS\n([\s\S]*?)          \/\/ END SIGNING INPUTS/
  )
  assert.ok(match, 'should exercise the actual inline signer validation')
  const script = match[1]
    .split('\n')
    .map((line) => line.slice(10))
    .join('\n')
  const canonical = (value) =>
    Array.isArray(value)
      ? value.map(canonical)
      : value && typeof value === 'object'
        ? Object.fromEntries(
            Object.keys(value)
              .sort()
              .map((key) => [key, canonical(value[key])])
          )
        : value
  for (const asset of assets)
    for (const mode of asset === 'trusted-server-fastly.wasm'
      ? [
          'success',
          'missing',
          'extra',
          'source',
          'run',
          'attempt',
          'failed',
          'profile',
          'qualified',
          'binary',
          'spdx',
          'duplicate-key',
        ]
      : ['success', 'extra']) {
      const root = mkdtempSync(join(tmpdir(), 'ordinary-sign-'))
      try {
        mkdirSync(join(root, 'release-assets'))
        const binary = Buffer.from(`fixture ${asset}`),
          spdx = Buffer.from('{}')
        writeFileSync(join(root, 'release-assets', asset), binary)
        writeFileSync(join(root, 'release-assets', `${asset}.spdx.json`), spdx)
        if (asset === 'trusted-server-fastly.wasm') {
          mkdirSync(join(root, 'comparison'))
          const receipt = {
            version: 1,
            profile: 'ordinary-release-fastly-byte-equality-v1',
            release_qualification: false,
            source_commit: 'a'.repeat(40),
            asset,
            target: 'wasm32-wasip1',
            variant: 'default',
            outcome: 'matched',
            canonical_sha256: hash(binary),
            reproduction_sha256: hash(binary),
            canonical: {
              run_id: 17,
              run_attempt: 2,
              job_id: 101,
              size: binary.length,
              sha256: hash(binary),
              spdx_sha256: hash(spdx),
            },
            reproduction: {
              run_id: 17,
              run_attempt: 2,
              job_id: 102,
              size: binary.length,
              sha256: hash(binary),
            },
          }
          if (mode === 'source') receipt.source_commit = 'b'.repeat(40)
          if (mode === 'run') receipt.canonical.run_id = 18
          if (mode === 'attempt') receipt.reproduction.run_attempt = 1
          if (mode === 'failed') receipt.outcome = 'mismatched'
          if (mode === 'profile') receipt.profile = 'controlled-recipe'
          if (mode === 'qualified') receipt.release_qualification = true
          if (mode === 'binary') receipt.canonical_sha256 = '0'.repeat(64)
          if (mode === 'spdx') receipt.canonical.spdx_sha256 = '0'.repeat(64)
          let text = JSON.stringify(canonical(receipt)) + '\n'
          if (mode === 'duplicate-key') {
            assert.ok(text.startsWith('{'))
            text = '{"version":0,' + text.slice(1)
          }
          if (mode !== 'missing')
            writeFileSync(
              join(root, 'comparison', `${asset}.rebuild-check.json`),
              text
            )
          if (mode === 'extra')
            writeFileSync(join(root, 'comparison', 'unexpected'), 'extra')
        } else if (mode === 'extra')
          writeFileSync(join(root, 'release-assets', 'unexpected'), 'extra')
        const local = (path) => join(root, path)
        const context = {
          Buffer,
          process: {
            env: {
              ASSET: asset,
              SOURCE_COMMIT: 'a'.repeat(40),
              GITHUB_RUN_ID: '17',
              GITHUB_RUN_ATTEMPT: '2',
              GITHUB_OUTPUT: 'outputs',
            },
          },
          require(name) {
            if (name === 'node:crypto') return { createHash }
            if (name === 'node:fs')
              return {
                readdirSync: (path) => fs.readdirSync(local(path)),
                lstatSync: (path) => fs.lstatSync(local(path)),
                readFileSync: (path) => fs.readFileSync(local(path)),
                writeFileSync: (path, ...args) =>
                  fs.writeFileSync(local(path), ...args),
                appendFileSync: (path, ...args) =>
                  fs.appendFileSync(local(path), ...args),
              }
            throw Error(`Unexpected signing module: ${name}`)
          },
        }
        const execute = () => vm.runInNewContext(script, context)
        if (mode !== 'success') {
          assert.throws(execute, undefined, `${asset}: ${mode}`)
          assert.equal(
            fs.existsSync(join(root, 'inputs.sha256')),
            false,
            'should not seal failed inputs'
          )
          continue
        }
        execute()
        const files = [
          asset,
          `${asset}.spdx.json`,
          ...(asset === 'trusted-server-fastly.wasm'
            ? [`${asset}.rebuild-check.json`]
            : []),
        ]
        assert.equal(
          readFileSync(join(root, 'inputs.sha256'), 'utf8'),
          files
            .map(
              (file) =>
                `${hash(readFileSync(join(root, 'release-assets', file)))}  ${file}\n`
            )
            .join('')
        )
        const output = readFileSync(join(root, 'outputs'), 'utf8')
        for (const file of files)
          assert.ok(
            output.includes(`release-assets/${file}\n`),
            'should attest every sealed input'
          )
        assert.equal(
          readFileSync(join(root, 'inputs.sha256'), 'utf8').trim().split('\n')
            .length,
          files.length
        )
      } finally {
        rmSync(root, { recursive: true, force: true })
      }
    }
})

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

test('release verification commands parse in Bash (CI only)', () => {
  const guide = readFileSync(
    'docs/guide/release-verification.md',
    'utf8'
  ).match(/<!-- release-verification:start -->\s*```bash\n([\s\S]*?)\n```/)
  assert.ok(
    guide,
    'should syntax-check the commands matched by the publisher test'
  )
  execFileSync('bash', ['-n'], { input: guide[1] })
})

test('publication binds exact signing handoffs and refuses drafts, corruption and partial uploads', async () => {
  const script = publicationScript()
  for (const mode of [
    'success',
    'idle-socket',
    'stable',
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
    'missing-companion',
    'changed-companion',
    'extra-companion',
    'unsigned-companion',
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
        if (asset === 'trusted-server-fastly.wasm') {
          const companion = `${asset}.rebuild-check.json`,
            bytes = Buffer.from('fixture signed ordinary rebuild check')
          writeFileSync(join(signed, companion), bytes)
          if (mode !== 'unsigned-companion')
            entries.splice(2, 0, `${hash(bytes)}  ${companion}\n`)
          if (mode === 'changed-companion')
            writeFileSync(join(signed, companion), 'changed')
          if (mode === 'missing-companion')
            fs.unlinkSync(join(signed, companion))
          if (mode === 'extra-companion')
            writeFileSync(join(signed, 'unexpected'), 'extra')
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
          'publication-disabled',
          'api-error',
          'missing',
          'extra',
          'changed',
          'empty-bundle',
          'missing-companion',
          'changed-companion',
          'extra-companion',
          'unsigned-companion',
        ].includes(mode)
      )
        assert.ok(!calls.some(([method]) => method === 'POST'), mode)
      if (mode === 'api-error' || mode === 'upload-error')
        assert.equal(calls.length, mode === 'api-error' ? 1 : 3)
      if (['upload-error'].includes(mode))
        assert.ok(!calls.some(([method]) => method === 'PATCH'), mode)
      if (successful) {
        assert.equal(uploaded.length, 30)
        assert.equal(
          calls.filter(
            ([method, args]) => method === 'gh' && args[1][1] !== 'download'
          ).length,
          31
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
          30,
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
  const canonical = build
    .split('\n  fastly-canonical:')[1]
    ?.split(/\n  [a-z][\w-]*:\n/)[0]
  const fastly = canonical?.match(/^          RELEASE_ASSET: (\S+)/m)?.[1]
  assert.deepEqual(
    [...build.matchAll(/^            asset: (\S+)/gm)]
      .map((m) => m[1])
      .concat(fastly ? [fastly] : [])
      .sort(),
    names
  )
  const sign = build.split('\n  sign:')[1]
  const matrix = sign
    .split('\n    steps:')[0]
    .match(/\n        asset:\s*\[([\s\S]*?)\]/)[1]
  assert.deepEqual(
    matrix
      .split(',')
      .map((name) => name.trim())
      .filter(Boolean)
      .sort(),
    names,
    'should sign precisely the cohort'
  )
  const allowed = sign.match(/const names = (\[[^;]+\]);/)[1]
  assert.deepEqual(
    Array.from(vm.runInNewContext(allowed)).sort(),
    names,
    'should validate the same exact input cohort'
  )
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

test('ordinary Fastly gate binds dedicated A and B uploads before isolated signing', () => {
  const build = readFileSync('scripts/release/phase1-build.sh', 'utf8')
  assert.match(build, /ordinary-rustflags/)
  assert.match(build, /cargo --config "\$ordinary_rustflags" build/)
  assert.ok(
    build.indexOf('ordinary-rustflags') < build.indexOf('npm --prefix'),
    'should seal arguments before dependency preparation and compilation'
  )
  assert.match(build, /rustflags: env\.ORDINARY_RUSTFLAGS/)
  const workflow = readFileSync('.github/workflows/release-build.yml', 'utf8')
  const caller = readFileSync('.github/workflows/release.yml', 'utf8')
  const job = (name) =>
    workflow.split(`\n  ${name}:`)[1]?.split(/\n  [a-z][\w-]*:\n/)[0]
  const canonical = job('fastly-canonical'),
    reproduction = job('fastly-reproduction'),
    comparison = job('fastly-comparison')
  assert.ok(
    canonical && reproduction && comparison,
    'should run two dedicated builds and a comparison'
  )
  assert.match(canonical, /name: Build trusted-server-fastly\.wasm/)
  assert.match(reproduction, /name: Reproduce trusted-server-fastly\.wasm/)
  assert.match(reproduction, /needs:.*fastly-canonical/)
  for (const [role, body] of [
    ['canonical', canonical],
    ['reproduction', reproduction],
  ]) {
    assert.doesNotMatch(body, /id-token|attestations:|contents: write/)
    assert.match(body, new RegExp(`path: ordinary-${role}-source`))
    assert.match(body, new RegExp(`Upload ${role} release artifact`))
    assert.match(body, new RegExp(`Upload ${role} observation`))
    for (const output of [
      'artifact-id',
      'artifact-digest',
      'observation-id',
      'observation-digest',
    ])
      assert.match(body, new RegExp(`      ${output}:`))
  }
  assert.match(comparison, /needs: \[fastly-canonical, fastly-reproduction\]/)
  assert.match(comparison, /actions: read/)
  assert.match(
    comparison,
    /ref: \$\{\{ github\.event\.pull_request\.head\.sha \|\| github\.sha \}\}/
  )
  assert.match(
    comparison,
    /test "\$\(git rev-parse HEAD\)" = "\$SOURCE_COMMIT"/
  )
  assert.doesNotMatch(comparison, /id-token|attestations:|contents: write/)
  assert.match(comparison, /ordinary-fetch/)
  assert.match(comparison, /ordinary-compare/)
  assert.match(comparison, /id: comparison/)
  assert.match(
    comparison,
    /if:.*success\(\).*steps\.comparison\.outcome == 'success'/
  )
  assert.match(comparison, /if ! env -i/)
  assert.doesNotMatch(
    comparison,
    /(?:test -[ef]|existsSync).*rebuild-check\.json/
  )
  for (const role of ['canonical', 'reproduction'])
    for (const output of [
      'artifact-id',
      'artifact-digest',
      'observation-id',
      'observation-digest',
    ])
      assert.ok(
        comparison.includes(`needs.fastly-${role}.outputs.${output}`),
        'should carry every direct upload output'
      )
  assert.match(job('sign'), /needs: \[build, fastly-comparison\]/)
  assert.match(caller.split('\n  release:')[0], /actions: read/)
})

test('canonical Fastly route verifies the pinned tool offline without credentials or deployment', () => {
  const workflow = readFileSync('.github/workflows/release-build.yml', 'utf8')
  const canonical = workflow
    .split('\n  fastly-canonical:')[1]
    .split('\n  fastly-reproduction:')[0]
  const route = canonical
    .split('- name: Verify the pinned manual Fastly route offline')[1]
    ?.split('      - name: Upload canonical release artifact')[0]
  assert.ok(route, 'should check the actual canonical binary before upload')
  assert.match(route, /\.tool-versions/)
  assert.match(route, /test "\$fastly_version" = 16\.0\.0/)
  assert.match(
    route,
    /b1b89112f1f1291ff275393779ffdacba8724ac64091bda0fa6000ae06308459/
  )
  assert.match(route, /sha256sum --check/)
  assert.ok(
    route.indexOf('sha256sum --check') < route.indexOf('unshare --net'),
    'should authenticate the downloaded CLI before any execution'
  )
  assert.match(route, /unshare --net -- setpriv/)
  assert.match(route, /--bounding-set=-all --no-new-privs/)
  assert.match(route, /env -i/)
  assert.match(route, /guest_network=\$\(readlink \/proc\/self\/ns\/net\)/)
  assert.match(route, /active_interfaces=\$\(ip -o link show up\)/)
  assert.doesNotMatch(route, /test[^\n]*\$\((?:ip|readlink) /)
  assert.match(route, /XDG_CONFIG_HOME="\$route\/config"/)
  assert.doesNotMatch(
    route,
    /(?:^|\s)HOME=|--token|FASTLY_API_TOKEN|fastly compute deploy|contents: write|id-token/m
  )
  for (const command of [
    'fastly compute pack --wasm-binary ./trusted-server-fastly.wasm',
    'fastly compute validate --package ./pkg/example-app.tar.gz',
    'tar -tzf ./pkg/example-app.tar.gz',
    'cmp trusted-server-fastly.wasm packaged.wasm',
    'cmp fastly.toml packaged.toml',
    'inspect-fastly-package',
  ])
    assert.ok(route.includes(command), `should execute ${command}`)
  assert.match(route, /set -euxo pipefail/)
  assert.match(route, /SUCCESS: pinned Fastly route/)
  assert.doesNotMatch(
    canonical.split('    steps:')[0],
    /write|id-token|attestations:/
  )
})

test('Fastly route diagnostics preserve literal directory headers and reject malformed size fields', () => {
  const workflow = readFileSync('.github/workflows/release-build.yml', 'utf8')
  const match = workflow.match(
    /          timeout 15 "\$6" --input-type=commonjs <<'HEADERS'\n([\s\S]*?)          HEADERS/
  )
  assert.ok(match, 'should inspect the actual bounded diagnostic program')
  const script = match[1]
    .split('\n')
    .map((line) => line.slice(10))
    .join('\n')
  const bytes = Buffer.alloc(3 * 512)
  for (const [index, path] of ['example-app', 'example-app/bin'].entries()) {
    const header = bytes.subarray(index * 512, (index + 1) * 512)
    header.write(path)
    header.write('00000000000\0', 124)
    header[156] = 53
  }
  const records = []
  let compressed = gzipSync(bytes)
  const context = {
    console: {
      log: (label, data) => {
        assert.equal(label, 'Fastly raw TAR header:')
        records.push(JSON.parse(data))
      },
    },
    require(name) {
      if (name === 'node:fs')
        return {
          lstatSync: () => ({ isFile: () => true, size: compressed.length }),
          readFileSync: () => compressed,
        }
      if (name === 'node:zlib') return { gunzipSync }
      throw Error(`Unexpected diagnostic module: ${name}`)
    },
  }
  vm.runInNewContext(script, { ...context })
  assert.deepEqual(
    records,
    ['example-app', 'example-app/bin'].map((path) => ({
      path,
      type: '5',
      size: 0,
      prefix: '',
      link: '',
      nonzero_padding: false,
    }))
  )
  bytes[64] = 1
  compressed = gzipSync(bytes)
  vm.runInNewContext(script, { ...context })
  assert.equal(
    records.at(-2).nonzero_padding,
    true,
    'should expose hidden nonzero name padding'
  )
  bytes[64] = 0
  bytes.write('invalid-size', 124)
  compressed = gzipSync(bytes)
  assert.throws(
    () => vm.runInNewContext(script, { ...context }),
    /Unsupported diagnostic size encoding/
  )
  assert.ok(
    workflow.indexOf('Fastly raw TAR header:') <
      workflow.indexOf('timeout 30 "$2" inspect-fastly-package'),
    'should log literal headers before the unchanged strict inspector'
  )
})

test('negative comparator proof keeps canaries read-only and audits both skipped stages', () => {
  const workflow = readFileSync('.github/workflows/test.yml', 'utf8')
  const job = (name) =>
    workflow.split(`\n  ${name}:`)[1]?.split(/\n  [a-z][\w-]*:\n/)[0]
  const native = job('test-axum')
  assert.match(native, /id: producer-tests/)
  assert.match(native, /comparison-succeeded=false/)
  assert.match(
    native,
    /ordinary_comparison_failure_prevents_signing_and_publication_sentinels -- --exact/
  )
  assert.match(native, /1 passed; 0 failed; 0 ignored;/)
  const fixture = readFileSync(
    'crates/trusted-server-release/tests/commands.rs',
    'utf8'
  )
  assert.match(
    fixture,
    /fn ordinary_comparison_failure_prevents_signing_and_publication_sentinels\(/
  )
  assert.match(
    fixture,
    /stderr\(&result\)\.contains\("ordinary final-byte mismatch"\)/
  )
  for (const name of ['ordinary-negative-sign', 'ordinary-negative-publish']) {
    const canary = job(name)
    assert.match(canary, /needs: test-axum/)
    assert.match(canary, /if:.*outputs\.comparison-succeeded == 'true'/)
    assert.match(canary, /contents: read/)
    assert.match(canary, /exit 1/)
    assert.doesNotMatch(canary, /uses:|write|gh |cargo |npm |secrets/)
  }
  const audit = job('ordinary-negative-audit')
  assert.match(audit, /if: always\(\)/)
  assert.match(audit, /test "\$NATIVE_RESULT" = success/)
  assert.match(audit, /test "\$SIGN_RESULT" = skipped/)
  assert.match(audit, /test "\$PUBLISH_RESULT" = skipped/)
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

test('unavailable ordinary diagnostic has fixed bounded canonical facts and cannot overwrite evidence', async () => {
  const helper = await import('./phase1.mjs')
  assert.equal(typeof helper.unavailableOrdinaryDiagnostic, 'function')
  const root = mkdtempSync(join(tmpdir(), 'ordinary-diagnostic-'))
  try {
    const directory = join(root, 'ordinary-diagnostics')
    helper.unavailableOrdinaryDiagnostic(directory)
    const file = join(directory, 'ordinary-comparison-diagnostic.json')
    const bytes = readFileSync(file)
    const value = JSON.parse(bytes)
    assert.ok(bytes.length <= 4096)
    assert.equal(value.outcome, 'unavailable')
    assert.equal(value.stage, 'transfer')
    assert.equal(value.code, 'transfer_unavailable')
    assert.ok(
      Object.values(value.facts).every((fact) => fact === 'unavailable')
    )
    assert.equal(value.profile, undefined)
    assert.equal(bytes.toString(), JSON.stringify(value) + '\n')
    assert.deepEqual(Object.keys(value), Object.keys(value).sort())
    assert.deepEqual(Object.keys(value.facts), Object.keys(value.facts).sort())
    assert.throws(() => helper.unavailableOrdinaryDiagnostic(directory))
    assert.deepEqual(readFileSync(file), bytes)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('ordinary diagnostic remains a failure-only CI artifact outside signing and publication', () => {
  const workflow = readFileSync('.github/workflows/release-build.yml', 'utf8')
  const comparison = workflow
    .split('\n  fastly-comparison:')[1]
    .split('\n  sign:')[0]
  const sign = workflow.split('\n  sign:')[1]
  const release = readFileSync('.github/workflows/release.yml', 'utf8')
  assert.match(
    comparison,
    /Retain bounded nonpublishable comparison diagnostics\n\s+if: failure\(\)/
  )
  assert.match(
    comparison,
    /ordinary-diagnostics\/ordinary-comparison-diagnostic\.json/
  )
  assert.match(
    comparison,
    /ordinary-diagnostic "\$RUNNER_TEMP\/ordinary-diagnostics"/
  )
  assert.match(comparison, /outcome=failed: ordinary diagnostic writer failed/)
  assert.match(comparison, /head -c 16384/)
  assert.doesNotMatch(
    sign,
    /ordinary-diagnostics|ordinary-comparison-diagnostic/
  )
  assert.doesNotMatch(
    release,
    /ordinary-diagnostics|ordinary-comparison-diagnostic/
  )
  const fixture = readFileSync(
    'crates/trusted-server-release/tests/commands.rs',
    'utf8'
  )
  assert.match(fixture, /diagnostic\["outcome"\],\s*"mismatched"/)
  assert.match(fixture, /diagnostic\["stage"\],\s*"bytes"/)
})
