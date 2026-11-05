import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { gzipSync, gunzipSync } from 'node:zlib'
import { execFileSync, spawnSync } from 'node:child_process'
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
import { fileURLToPath } from 'node:url'
import {
  checkBinary,
  checkCliVersion,
  mergeSboms,
  sentinels,
} from './phase1.mjs'
import {
  publish,
  readRetrier,
  releaseAssets,
  releaseNotes,
} from './publish.mjs'
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
test('merges independent graphs without colliding references and binds binary bytes', () => {
  const bom = (name) => ({
    bomFormat: 'CycloneDX',
    specVersion: '1.5',
    version: 1,
    metadata: {
      component: {
        type: 'application',
        name,
        version: '1.2.3',
        'bom-ref': 'root',
        components: [
          { type: 'library', name: `${name}-nested`, 'bom-ref': 'nested' },
        ],
      },
    },
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
  // The release tag stays external; the root keeps Cargo's package version,
  // matching its purl and `ts --version`.
  assert.equal(output.metadata.component.version, '1.2.3')
  // The root's nested components are listed once, at the top level.
  assert.equal(output.metadata.component.components, undefined)
  assert.equal(output.metadata.component.hashes[0].alg, 'SHA-256')
  assert.equal(output.metadata.component.hashes[0].content.length, 64)
  assert.deepEqual(
    output.dependencies.find(
      (d) => d.ref === output.metadata.component['bom-ref']
    ).dependsOn,
    [output.components[0]['bom-ref'], output.components[2]['bom-ref']]
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
  const release = readFileSync('.github/workflows/release.yml', 'utf8')
  const triggers = release.split('\npermissions:')[0]
  assert.doesNotMatch(
    triggers,
    /pull_request/,
    'should not run release builds or privileged callers on pull requests'
  )
  assert.match(triggers, /branches: \[main\]/)
  assert.match(triggers, /tags: \['v\*'\]/)
  assert.doesNotMatch(workflow, /workflow_dispatch|--clobber/)
  // The publisher job checks out only its own script, template and tool pins.
  const publisher = release.split('\n  release:')[1]
  assert.equal((publisher.match(/actions\/checkout@/g) ?? []).length, 1)
  assert.match(publisher, /persist-credentials: false/)
  assert.deepEqual(
    publisher
      .match(/sparse-checkout: \|\n((?: {12}\S.*\n)+)/)[1]
      .trim()
      .split(/\s+/),
    [
      '.tool-versions',
      'scripts/release/publish.mjs',
      'scripts/release/release-notes.md',
    ]
  )
  assert.match(publisher, /sparse-checkout-cone-mode: false/)
  assert.doesNotMatch(publisher, /cargo |npm |phase1-build/)
  assert.match(publisher, /GH_SHA256: [a-f0-9]{64}\n/)
  assert.match(publisher, /sha256sum --check/)
  assert.match(publisher, /node publisher\/scripts\/release\/publish\.mjs\n/)
  // Match the repository convention: actions are referenced by major tag.
  for (const line of [workflow, release]
    .join('\n')
    .split('\n')
    .filter((line) => /uses: .*@/.test(line))) {
    assert.match(line, /uses: actions\/[\w-]+@v\d+$/)
  }
  // Every job that runs Node first installs the pinned version.
  for (const text of [workflow, release])
    for (const job of text.split(/\n {2}(?=[\w-]+:\n)/).slice(1))
      if (/^\s*(?:- )?(?:run: )?node /m.test(job))
        assert.ok(
          job.indexOf('actions/setup-node@') >= 0 &&
            job.indexOf('actions/setup-node@') <
              job.search(/^\s*(?:- )?(?:run: )?node /m),
          `should set up pinned Node before running it: ${job.split(':')[0]}`
        )
})
const runBlock = (workflow, step) => {
  const lines = workflow.split('\n')
  const start = lines.findIndex((line) => line.trim() === `- name: ${step}`)
  assert.ok(start >= 0, `should define step ${step}`)
  const run = lines.findIndex(
    (line, index) => index > start && line.trim() === 'run: |'
  )
  const indent = lines[run + 1].match(/^ */)[0]
  const body = []
  for (const line of lines.slice(run + 1)) {
    if (line && !line.startsWith(indent)) break
    body.push(line.slice(indent.length))
  }
  return body.join('\n')
}
test('signing job shell refuses symlinked, extra, empty and changed inputs', () => {
  const workflow = readFileSync('.github/workflows/release-build.yml', 'utf8')
  const validate = runBlock(
    workflow,
    'Validate signing inputs and retain their digests'
  )
  const retain = runBlock(
    workflow,
    "Retain only this run's real bundles and signed-input digests"
  )
  // Use a cohort asset without a companion; the validator rejects unknown names.
  const asset = 'ts-x86_64-unknown-linux-gnu'
  for (const mode of [
    'success',
    'symlink-binary',
    'symlink-spdx',
    'directory-binary',
    'extra',
    'missing',
    'empty',
    'changed',
    'empty-bundle',
    'empty-provenance',
  ]) {
    const root = mkdtempSync(join(tmpdir(), 'phase1-sign-'))
    try {
      const inputs = join(root, 'release-assets')
      mkdirSync(inputs)
      writeFileSync(join(root, 'outside'), 'outside')
      for (const file of [asset, `${asset}.spdx.json`])
        writeFileSync(join(inputs, file), `fixture ${file}`)
      if (mode.startsWith('symlink-')) {
        const file = join(
          inputs,
          mode === 'symlink-binary' ? asset : `${asset}.spdx.json`
        )
        rmSync(file)
        fs.symlinkSync(join(root, 'outside'), file)
      }
      if (mode === 'directory-binary') {
        rmSync(join(inputs, asset))
        mkdirSync(join(inputs, asset))
        writeFileSync(join(inputs, asset, 'nested'), 'nested')
      }
      if (mode === 'extra') writeFileSync(join(inputs, 'extra'), 'extra')
      if (mode === 'missing') rmSync(join(inputs, `${asset}.spdx.json`))
      if (mode === 'empty') writeFileSync(join(inputs, asset), '')
      const env = {
        PATH: process.env.PATH,
        ASSET: asset,
        SOURCE_COMMIT: 'a'.repeat(40),
        GITHUB_RUN_ID: '17',
        GITHUB_RUN_ATTEMPT: '2',
        GITHUB_OUTPUT: join(root, 'github-output'),
      }
      const run = (script, extra = {}) =>
        spawnSync('bash', ['-c', script], {
          cwd: root,
          env: { ...env, ...extra },
          encoding: 'utf8',
        })
      const validated = run(validate)
      const validFixture = [
        'success',
        'changed',
        'empty-bundle',
        'empty-provenance',
      ].includes(mode)
      assert.equal(validated.status === 0, validFixture, mode)
      if (!validFixture) continue
      // Simulate the two attest steps between the shell steps.
      const provenance = join(root, 'provenance.json')
      const sbom = join(root, 'sbom.json')
      writeFileSync(
        provenance,
        mode === 'empty-provenance' ? '' : 'provenance bundle'
      )
      writeFileSync(sbom, mode === 'empty-bundle' ? '' : 'sbom bundle')
      if (mode === 'changed') writeFileSync(join(inputs, asset), 'changed')
      const retained = run(retain, { PROVENANCE: provenance, SBOM: sbom })
      assert.equal(retained.status === 0, mode === 'success', mode)
      if (mode !== 'success') continue
      const signed = join(root, 'signed-assets')
      assert.deepEqual(fs.readdirSync(signed).sort(), [
        'SHA256SUMS',
        `${asset}.provenance.sigstore.json`,
        `${asset}.sbom.sigstore.json`,
      ])
      const sums = readFileSync(join(signed, 'SHA256SUMS'), 'utf8')
      for (const [file, dir] of [
        [asset, inputs],
        [`${asset}.spdx.json`, inputs],
        [`${asset}.provenance.sigstore.json`, signed],
        [`${asset}.sbom.sigstore.json`, signed],
      ])
        assert.ok(
          sums.includes(`${hash(readFileSync(join(dir, file)))}  ${file}\n`),
          `should retain the digest of ${file}`
        )
    } finally {
      rmSync(root, { recursive: true, force: true })
    }
  }
})
const assets = releaseAssets
// The publisher fixture shadows `execFileSync`; this runs a real child process.
const realExecFileSync = execFileSync
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

const canonicalBlock = (markdown) => {
  const block = markdown.match(
    /<!-- release-verification:start -->\n\n```bash\n([\s\S]*?)\n```\n\n<!-- release-verification:end -->/
  )
  assert.ok(block, 'should mark the canonical verification block')
  return block[1]
}
test('release notes link to the guide at the tag and carry no commands', () => {
  const body = releaseNotes('example/project', 'v1.2.3-rc.1')
  assert.ok(
    body.includes(
      '(https://github.com/example/project/blob/v1.2.3-rc.1/docs/guide/release-verification.md)'
    ),
    'should link to the guide at the tag'
  )
  assert.match(body, /stay editable after publication/)
  assert.doesNotMatch(body, /\{\{|```/)
  // The guide's block is the only copy of the verification commands.
  execFileSync('bash', ['-n'], {
    input: canonicalBlock(
      readFileSync('docs/guide/release-verification.md', 'utf8')
    ),
  })
})
test('publication binds exact signing handoffs and refuses drafts, corruption and partial uploads', async (t) => {
  // Record each read's command timeout so availability reads can be bounded.
  const timeouts = new WeakMap()
  const timeout = AbortSignal.timeout.bind(AbortSignal)
  t.mock.method(AbortSignal, 'timeout', (delay) => {
    const signal = timeout(delay)
    timeouts.set(signal, delay)
    return signal
  })
  for (const mode of [
    'success',
    'idle-socket',
    'stable',
    'build-metadata-tag',
    'leading-zero-tag',
    'non-semver-tag',
    'annotated-tag',
    'wrong-tag-commit',
    'missing-tag',
    'malformed-tag',
    'bundle-verification-failure',
    'bundle-sbom-mismatch',
    'identity-late',
    'large-sbom-output',
    'publication-disabled',
    'prerelease-mismatch',
    'draft',
    'draft-page-2',
    'api-error',
    'missing',
    'extra',
    'changed',
    'empty-bundle',
    'missing-companion',
    'changed-companion',
    'extra-companion',
    'unsigned-companion',
    'symlink-input',
    'signed-extra',
    'created-wrong-tag',
    'created-not-draft',
    'created-not-prerelease',
    'upload-error',
    'upload-digest',
    'upload-size',
    'mutable',
    'wrong-id',
    'published-asset-digest',
    'published-asset-extra',
    'published-asset-renamed',
    'attestation-404-then-present',
    'attestation-after-two-waits',
    'attestation-404-never',
    'attestation-non-200',
    'attestation-list-transient',
    'attestation-user-initiated',
    'attestation-truncated',
    'attestation-duplicate-subject',
    'attestation-payload-type',
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
    'attestation-statement-mismatch',
    'attestation-verification-failure',
    'attestation-verify-transient',
    'attestation-verify-past-deadline',
    'attestation-proof-late-failure',
    'attestation-late-reads',
    'attestation-list-past-deadline',
    'attestation-null-bundle',
    'attestation-null-bundle-late',
    'attestation-null-bundle-never',
    'attestation-null-bundle-no-url',
    'attestation-null-bundle-wrong-assets',
  ]) {
    const root = mkdtempSync(join(tmpdir(), 'phase1-publish-'))
    const calls = []
    const uploaded = []
    const stable = mode.startsWith('stable') || mode === 'attestation-other-tag'
    const tag =
      mode === 'build-metadata-tag'
        ? 'v1.0.0+build-1'
        : mode === 'leading-zero-tag'
          ? 'v01.0.0'
          : mode === 'non-semver-tag'
            ? 'v1.0'
            : stable
              ? 'v1.0.0'
              : 'v0.0.0-example'
    const opaque = mode.startsWith('attestation-null-bundle')
    // Real SBOM results embed the whole SPDX document and exceed 1 MiB.
    const spdxDocument = (asset) =>
      mode === 'large-sbom-output'
        ? { fixture: asset, padding: 'x'.repeat(2 * 1024 * 1024) }
        : { fixture: asset }
    const waits = [],
      ignored = []
    // Whether a gh command ran since the last API call.
    let ghSinceFetch = false
    let verifies = 0,
      now = 0,
      // The availability window starts when the identity reads finish.
      availabilityEnd = 60000
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
          const bytes = Buffer.from(
            suffix === '.spdx.json'
              ? JSON.stringify(spdxDocument(asset))
              : `fixture ${asset}${suffix}`
          )
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
      if (mode === 'signed-extra')
        writeFileSync(join(root, 'signed', assets[0], 'unexpected'), 'extra')
      if (mode === 'symlink-input') {
        const input = join(root, 'build', assets[0], assets[0])
        fs.renameSync(input, join(root, 'elsewhere'))
        fs.symlinkSync(join(root, 'elsewhere'), input)
      }
      const release = (draft) => ({
        id: 42,
        tag_name: tag,
        prerelease: !stable && mode !== 'prerelease-mismatch',
        draft,
        immutable: mode !== 'mutable',
        assets: uploaded.map((item, index) => ({
          ...item,
          digest:
            mode === 'published-asset-digest' && index === 0
              ? `sha256:${'0'.repeat(64)}`
              : item.digest,
        })),
      })
      const attestation = (attested = tag) => {
        const statement = {
          _type: 'https://in-toto.io/Statement/v1',
          predicateType: 'https://in-toto.io/attestation/release/v0.2',
          predicate: {
            databaseId: '42',
            repository: 'example/project',
            tag: attested,
          },
          subject: [
            {
              uri: `pkg:github/example/project@${attested}`,
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
        if (
          mode === 'attestation-wrong-assets' ||
          mode === 'attestation-null-bundle-wrong-assets'
        )
          statement.subject[1].digest.sha256 = '0'.repeat(64)
        if (mode === 'attestation-missing-asset') statement.subject.pop()
        if (mode === 'attestation-extra-asset')
          statement.subject.push({
            name: 'unexpected',
            digest: { sha256: '0'.repeat(64) },
          })
        if (mode === 'attestation-wrong-tag-shape') statement.predicate.tag = 42
        if (mode === 'attestation-duplicate-subject')
          statement.subject.push(statement.subject[1])
        return {
          initiator: 'github',
          bundle: {
            mediaType: 'application/vnd.dev.sigstore.bundle.v0.3+json',
            verificationMaterial: {},
            dsseEnvelope: {
              payloadType:
                mode === 'attestation-payload-type'
                  ? 'application/json'
                  : 'application/vnd.in-toto+json',
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
        // A synchronous gh check between API calls leaves pooled sockets
        // idle, and the server may close them meanwhile.
        if (
          mode === 'idle-socket' &&
          ghSinceFetch &&
          options.headers.Connection !== 'close'
        )
          throw new TypeError('other side closed')
        ghSinceFetch = false
        calls.push([method, u.pathname + u.search])
        const response = (body, status = 200, headers = {}) =>
          new Response(
            typeof body === 'string' || Buffer.isBuffer(body)
              ? body
              : JSON.stringify(body),
            { status, headers }
          )
        if (u.pathname.includes('/git/ref/tags/')) {
          assert.equal(method, 'GET')
          assert.equal(decodeURIComponent(u.pathname.split('/').at(-1)), tag)
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
          assert.ok(
            now < availabilityEnd,
            'should not start a read after the deadline'
          )
          assert.equal(
            timeouts.get(options.signal),
            Math.min(4000, availabilityEnd - now),
            'should bound every availability read by the availability deadline'
          )
          // Two transient failures push the third read close to the deadline.
          if (mode === 'attestation-late-reads' && [5, 6].includes(polls))
            return response({}, 503, { 'retry-after': '10' })
          // The second failure runs until its timeout, so its retry would end
          // after the availability deadline.
          if (mode === 'attestation-list-past-deadline' && polls >= 5) {
            if (polls === 6) now += timeouts.get(options.signal)
            return response({}, 503, { 'retry-after': '10' })
          }
          if (
            mode === 'attestation-non-200' ||
            (mode === 'attestation-list-transient' && polls === 1)
          )
            return response({}, 503)
          if (mode === 'attestation-201') return response({}, 201)
          if (mode === 'attestation-auth-error') return response({}, 403)
          if (mode === 'attestation-malformed')
            return response({ attestations: null })
          if (opaque)
            return response({
              attestations: [
                {
                  initiator: 'github',
                  bundle: null,
                  ...(mode === 'attestation-null-bundle-no-url'
                    ? {}
                    : { bundle_url: 'https://example.com/bundle' }),
                },
              ],
            })
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
                  : [
                        'attestation-verify-past-deadline',
                        'attestation-late-reads',
                        'attestation-list-past-deadline',
                      ].includes(mode)
                    ? 4
                    : 0
          if (polls <= delay && mode !== 'attestation-other-tag-before-current')
            return response({ message: 'Not Found' }, 404)
          observed = polls <= delay ? [] : [attestation()]
          if (mode === 'attestation-duplicate') observed.push(attestation())
          if (mode === 'attestation-user-initiated')
            observed.unshift({ initiator: 'user', bundle: 'not a bundle' })
          if (mode === 'attestation-truncated')
            observed = Array(100).fill(observed[0])
          if (mode.startsWith('attestation-other-')) observed.unshift(other)
          return response({ attestations: observed })
        }
        if (u.pathname.endsWith('/releases') && method === 'GET') {
          if (mode === 'api-error') return response('denied', 503)
          const page = Number(u.searchParams.get('page'))
          if (mode === 'draft-page-2')
            return response(
              page === 1
                ? Array.from({ length: 100 }, (_, index) => ({
                    tag_name: `v0.0.${index}`,
                  }))
                : [release(true)]
            )
          assert.equal(page, 1, 'should stop after a short page')
          return response(mode === 'draft' ? [release(true)] : [])
        }
        if (u.pathname.endsWith('/releases') && method === 'POST') {
          assert.deepEqual(
            JSON.parse(options.body),
            {
              tag_name: tag,
              target_commitish: 'a'.repeat(40),
              name: tag,
              draft: true,
              prerelease: !stable,
              make_latest: stable ? 'legacy' : 'false',
              body: releaseNotes('example/project', tag),
            },
            'should create exactly one draft for the tag'
          )
          return response(
            mode === 'created-wrong-tag'
              ? { ...release(true), tag_name: 'v9.9.9' }
              : mode === 'created-not-prerelease'
                ? { ...release(true), prerelease: false }
                : release(mode !== 'created-not-draft'),
            201
          )
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
          return response(
            mode === 'upload-digest'
              ? { ...item, digest: `sha256:${'0'.repeat(64)}` }
              : mode === 'upload-size'
                ? { ...item, size: item.size + 1 }
                : item,
            201
          )
        }
        if (method === 'GET' && /\/releases\/(?:42|tags\/)/.test(u.pathname))
          assert.equal(
            timeouts.get(options.signal),
            Math.min(10000, 180000 - now),
            'should bound every identity read by the post-publish window'
          )
        if (u.pathname.includes('/releases/tags/')) {
          availabilityEnd = Math.min(180000, now + 60000)
          return response({
            ...release(false),
            id: mode === 'wrong-id' ? 43 : 42,
          })
        }
        if (u.pathname.endsWith('/releases/42')) {
          if (method === 'PATCH')
            assert.deepEqual(
              JSON.parse(options.body),
              {
                draft: false,
                prerelease: !stable,
                make_latest: stable ? 'legacy' : 'false',
              },
              'should publish with the release type and Latest policy'
            )
          // The first identity read ends late in the post-publish window.
          else if (mode === 'identity-late') now = 175000
          const current = release(method !== 'PATCH' && uploaded.length === 0)
          if (mode === 'published-asset-extra')
            current.assets.push({ name: 'unexpected', size: 1 })
          // Same count and unique names, but one name is not in the set.
          if (mode === 'published-asset-renamed' && current.assets.length)
            current.assets[0] = { ...current.assets[0], name: 'unexpected' }
          return response(current)
        }
        throw Error(`Unexpected ${method} ${url}`)
      }
      const execFileSync = (...args) => {
        calls.push(['gh', args])
        ghSinceFetch = true
        if (args[1][0] === 'attestation') {
          const index =
            calls.filter(
              ([kind, call]) => kind === 'gh' && call[1][0] === 'attestation'
            ).length - 1
          // Two predicates per binary; the Fastly provenance also names its
          // rebuild check.
          const [name, subject, kind] = assets.flatMap((asset) => [
            [asset, asset, 'provenance'],
            [asset, asset, 'sbom'],
            ...(asset === 'trusted-server-fastly.wasm'
              ? [[asset, `${asset}.rebuild-check.json`, 'provenance']]
              : []),
          ])[index]
          assert.deepEqual(
            args,
            [
              'gh',
              [
                'attestation',
                'verify',
                join(root, 'release-assets', subject),
                '--repo',
                'example/project',
                '--bundle',
                join(root, 'release-assets', `${name}.${kind}.sigstore.json`),
                '--source-ref',
                `refs/tags/${tag}`,
                '--source-digest',
                'a'.repeat(40),
                '--signer-digest',
                'a'.repeat(40),
                '--deny-self-hosted-runners',
                '--predicate-type',
                kind === 'sbom'
                  ? 'https://spdx.dev/Document/v2.3'
                  : 'https://slsa.dev/provenance/v1',
                '--signer-workflow',
                'example/project/.github/workflows/release-build.yml',
                '--format',
                'json',
              ],
              {
                timeout: 60000,
                stdio: 'pipe',
                maxBuffer: 256 * 1024 * 1024,
              },
            ],
            'should verify each signing bundle for its asset and predicate'
          )
          assert.ok(
            !calls.some(([method]) => method !== 'gh'),
            'should verify signing bundles before any API call'
          )
          if (mode === 'bundle-verification-failure' && index === 3)
            throw Error('Signature verification failed')
          const output = JSON.stringify([
            {
              verificationResult: {
                statement: {
                  predicate:
                    mode === 'bundle-sbom-mismatch'
                      ? { fixture: 'other' }
                      : spdxDocument(name),
                },
              },
            },
          ])
          if (mode !== 'large-sbom-output') return Buffer.from(output)
          // Pass the large result through a real child process with the
          // publisher's own options, so Node's output buffer limit applies.
          const file = join(root, 'gh-output.json')
          writeFileSync(file, output)
          return realExecFileSync(
            process.execPath,
            [
              '-e',
              "process.stdout.write(require('node:fs').readFileSync(process.argv[1]))",
              file,
            ],
            args[2]
          )
        }
        verifies++
        const verifiedTag = args[1].at(-1)
        assert.equal(verifiedTag, tag, 'should verify only the current tag')
        assert.equal(
          args[2].timeout,
          Math.min(10000, availabilityEnd - now),
          'should bound proof verification by the availability deadline'
        )
        // The first proof fails, so the next availability check reads the
        // API again after a gh command.
        if (mode === 'idle-socket' && verifies === 1)
          throw Error('Signature verification failed')
        if (mode === 'attestation-verify-transient' && verifies === 1)
          throw Object.assign(Error('gh failed'), {
            stderr: Buffer.from('HTTP 503: Service Unavailable'),
          })
        // Each proof fetch runs until its command timeout.
        if (mode === 'attestation-verify-past-deadline' && verifies <= 2) {
          now += args[2].timeout
          throw Object.assign(Error('timeout'), { code: 'ETIMEDOUT' })
        }
        // Every failed proof fetch runs until its command timeout, so the
        // check before the next wait finds the deadline too close.
        if (mode === 'attestation-proof-late-failure') {
          now += args[2].timeout
          throw Error('Signature verification failed')
        }
        if (
          mode === 'attestation-verification-failure' ||
          mode === 'attestation-null-bundle-never' ||
          (mode === 'attestation-null-bundle-late' && verifies === 1)
        )
          throw Error('Signature verification failed')
        assert.ok(
          polls > 0,
          'should read release-attestation availability before verification'
        )
        const bundle = opaque
          ? attestation().bundle
          : structuredClone(
              observed.find(
                (r) =>
                  r.initiator === 'github' &&
                  JSON.parse(
                    Buffer.from(r.bundle.dsseEnvelope.payload, 'base64')
                  ).predicate.tag === verifiedTag
              ).bundle
            )
        if (mode === 'attestation-substituted-bundle')
          bundle.dsseEnvelope.signatures[0].sig = 'different'
        const statement = JSON.parse(
          Buffer.from(bundle.dsseEnvelope.payload, 'base64')
        )
        if (mode === 'attestation-statement-mismatch')
          statement.predicate.databaseId = '99'
        return Buffer.from(
          JSON.stringify({
            attestation: { bundle },
            verificationResult: { statement },
          })
        )
      }
      const execute = () =>
        publish({
          env: {
            GH_REPO: 'example/project',
            GH_TOKEN: 'fixture-token',
            RELEASE_TAG: tag,
            RELEASE_PUBLICATION_ENABLED:
              mode === 'publication-disabled' ? 'false' : 'true',
            SOURCE_COMMIT: 'a'.repeat(40),
          },
          root,
          fetch,
          execFileSync,
          now: () => now,
          sleep: async (delay) => {
            waits.push(delay)
            now += delay
          },
          log: (...args) => {
            if (args[0] === 'Ignored release attestation for other tag:')
              ignored.push(args[1])
          },
          warn: () => {},
        })
      const successful = [
        'success',
        'idle-socket',
        'stable',
        'identity-late',
        'large-sbom-output',
        'attestation-404-then-present',
        'attestation-after-two-waits',
        'attestation-other-tag',
        'attestation-other-tag-before-current',
        'attestation-other-unverifiable',
        'attestation-verify-transient',
        'attestation-list-transient',
        'attestation-late-reads',
        'attestation-user-initiated',
        'attestation-null-bundle',
        'attestation-null-bundle-late',
      ].includes(mode)
      const expectedError = {
        'non-semver-tag': /Invalid repository or tag/,
        'build-metadata-tag': /Invalid repository or tag/,
        'leading-zero-tag': /Invalid repository or tag/,
        'upload-size': /Upload mismatch/,
        'attestation-verification-failure': /Signature verification failed/,
        'attestation-proof-late-failure': /Signature verification failed/,
        'bundle-verification-failure': /Signature verification failed/,
        'bundle-sbom-mismatch': /Signed SBOM differs from the shipped SPDX/,
        'attestation-truncated': /truncated/,
        'attestation-duplicate-subject': /Invalid release attestation subjects/,
        'attestation-payload-type': /Malformed release attestation bundle/,
        'draft-page-2': /already exists/,
        'symlink-input': /Invalid file/,
        'upload-digest': /Upload mismatch/,
        'published-asset-digest': /Published asset digest mismatch/,
        'published-asset-extra': /Published asset set mismatch/,
        'published-asset-renamed': /Unexpected published asset/,
        'attestation-404-never': /unavailable after five checks/,
        // The deadline names its window and keeps the failure that caused the
        // retry.
        'attestation-verify-past-deadline': (error) =>
          error.message ===
            'Release attestation availability retry window exhausted' &&
          error.cause?.code === 'ETIMEDOUT',
        'attestation-list-past-deadline': (error) =>
          error.message ===
            'Release attestation availability retry window exhausted' &&
          error.cause?.status === 503,
        'signed-extra': /Unexpected artifact contents/,
        'created-wrong-tag': /Invalid created release identity/,
        'created-not-draft': /Invalid created release identity/,
        'created-not-prerelease': /Invalid created release identity/,
        'attestation-statement-mismatch': /statement mismatch/,
        'attestation-substituted-bundle': /bundle mismatch/,
        'attestation-null-bundle-never': /Signature verification failed/,
        'attestation-null-bundle-no-url':
          /Malformed release attestation bundle/,
        'attestation-null-bundle-wrong-assets': /identity or assets mismatch/,
      }[mode]
      if (successful) await execute()
      else await assert.rejects(execute, expectedError, mode)
      if (
        [
          'draft',
          'draft-page-2',
          'non-semver-tag',
          'build-metadata-tag',
          'leading-zero-tag',
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
          'missing-companion',
          'changed-companion',
          'extra-companion',
          'unsigned-companion',
          'symlink-input',
          'signed-extra',
          'bundle-verification-failure',
          'bundle-sbom-mismatch',
        ].includes(mode)
      )
        assert.ok(
          !calls.some(([method]) => ['POST', 'PATCH'].includes(method)),
          mode
        )
      if (mode.startsWith('bundle-'))
        assert.ok(
          !calls.some(([method]) => method !== 'gh'),
          'should call no API after a signing bundle fails'
        )
      if (mode === 'draft-page-2')
        assert.ok(
          calls.some(([, path]) =>
            /\/releases\?per_page=100&page=2$/.test(path)
          ),
          'should read the second release page'
        )
      // Two predicates per binary, plus the Fastly rebuild check.
      const bundleChecks = releaseAssets.length * 2 + 1
      if (mode === 'api-error' || mode === 'upload-error')
        assert.equal(
          calls.length,
          bundleChecks + (mode === 'api-error' ? 2 : 4)
        )
      if (mode.startsWith('created-'))
        assert.ok(
          !calls.some(([method, path]) =>
            String(path).split('?')[0].endsWith('/assets')
          ),
          'should upload nothing to a release with the wrong identity'
        )
      // Immutability is verified before any availability read.
      if (mode === 'mutable') assert.equal(polls, 0, mode)
      if (['upload-error', 'upload-digest', 'upload-size'].includes(mode))
        assert.ok(!calls.some(([method]) => method === 'PATCH'), mode)
      if (successful) {
        // Four files per binary, the Fastly rebuild check and SHA256SUMS.
        assert.equal(uploaded.length, assets.length * 4 + 2)
        // Two-space lines are what `shasum -a 256 --check` accepts.
        assert.equal(
          uploaded.find((item) => item.name === 'SHA256SUMS').bytes.toString(),
          uploaded
            .filter((item) => item.name !== 'SHA256SUMS')
            .map((item) => `${hash(item.bytes)}  ${item.name}\n`)
            .join(''),
          'should publish checksums in the checker format'
        )
        assert.equal(
          calls.filter(
            ([kind, args]) => kind === 'gh' && args[1][0] === 'attestation'
          ).length,
          bundleChecks
        )
        assert.equal(
          verifies,
          [
            'idle-socket',
            'attestation-verify-transient',
            'attestation-null-bundle-late',
          ].includes(mode)
            ? 2
            : 1,
          mode
        )
      }
      if (mode.startsWith('attestation-')) {
        const expectedWaits = [
          'attestation-404-never',
          'attestation-null-bundle-never',
          'attestation-verification-failure',
        ].includes(mode)
          ? [2000, 5000, 10000, 20000]
          : [
                'attestation-404-then-present',
                'attestation-null-bundle-late',
              ].includes(mode)
            ? [2000]
            : [
                  'attestation-after-two-waits',
                  'attestation-other-tag-before-current',
                ].includes(mode)
              ? [2000, 5000]
              : []
        // Transient read retries wait inside one availability check.
        const retried = {
          'attestation-verify-transient': [[2000], 1],
          'attestation-verify-past-deadline': [
            [2000, 5000, 10000, 20000, 2000],
            5,
          ],
          'attestation-list-transient': [[2000], 2],
          'attestation-late-reads': [
            [2000, 5000, 10000, 20000, 10000, 10000],
            7,
          ],
          'attestation-list-past-deadline': [
            [2000, 5000, 10000, 20000, 10000],
            6,
          ],
          'attestation-non-200': [[2000, 5000], 3],
          'attestation-proof-late-failure': [[2000, 5000, 10000], 4],
        }[mode]
        assert.deepEqual(waits, retried?.[0] ?? expectedWaits, mode)
        assert.equal(polls, retried?.[1] ?? expectedWaits.length + 1, mode)
        assert.equal(
          calls.filter(
            ([method, path]) => method === 'POST' && path.endsWith('/releases')
          ).length,
          1,
          mode
        )
        assert.equal(
          calls.filter(
            ([method, path]) =>
              method === 'POST' && path.split('?')[0].endsWith('/assets')
          ).length,
          assets.length * 4 + 2,
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
test('publisher entry point exits non-zero on failure', () => {
  const root = mkdtempSync(join(tmpdir(), 'phase1-entry-'))
  try {
    const result = spawnSync(
      process.execPath,
      [fileURLToPath(new URL('./publish.mjs', import.meta.url))],
      {
        cwd: root,
        env: {
          PATH: process.env.PATH,
          GH_REPO: 'example/project',
          RELEASE_TAG: 'v1.0.0',
          SOURCE_COMMIT: 'a'.repeat(40),
          RELEASE_PUBLICATION_ENABLED: 'false',
        },
        encoding: 'utf8',
      }
    )
    assert.equal(result.status, 1, 'should fail the publication step')
    assert.match(result.stderr, /not enabled by maintainers/)
    assert.deepEqual(fs.readdirSync(root), [], 'should not stage any asset')
  } finally {
    rmSync(root, { recursive: true, force: true })
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
  // The Fastly ordinary capture reads the locked graph this step writes.
  const metadata = script.match(/^ {4}ORDINARY_METADATA="([^"]+)" \\$/m)?.[1]
  assert.ok(metadata, 'should pass the metadata file to the ordinary capture')
  assert.ok(
    script.slice(acquisition).split('\n')[0].endsWith(` > "${metadata}"`),
    'should write the metadata the ordinary capture reads'
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
  // A converter package named like the root, listed before it, must not stand
  // in for the described root.
  const { document, original } = spdxFixture()
  document.packages.unshift({
    SPDXID: 'SPDXRef-Impostor',
    name: document.packages[0].name,
  })
  assert.throws(
    () => validateSpdx(document, binary, original),
    /SPDX describes a different inventory root/
  )
})
test('SPDX validation rescans the final binary for planted operator values', () => {
  const planted = Buffer.concat([binary, Buffer.from(sentinels[0])])
  const { original, document } = spdxFixture()
  document.packages[0].checksums[0].checksumValue = createHash('sha256')
    .update(planted)
    .digest('hex')
  assert.throws(
    () => validateSpdx(document, planted, original),
    /operator configuration/
  )
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
test('completed SPDX is validated before it is returned', () => {
  const { original, document } = spdxFixture()
  document.packages.shift()
  delete document.documentDescribes
  delete document.relationships
  document.packages[0].externalRefs = []
  assert.throws(
    () => completeSpdx(document, binary, original),
    /identity or PURL/
  )
})
test('release command line checks binaries, final bytes and SPDX output', () => {
  const root = mkdtempSync(join(tmpdir(), 'phase1-cli-'))
  const run = (...args) =>
    spawnSync(
      process.execPath,
      [fileURLToPath(new URL('./phase1.mjs', import.meta.url)), ...args],
      { cwd: root, encoding: 'utf8' }
    )
  const write = (name, bytes) => {
    writeFileSync(join(root, name), bytes)
    return join(root, name)
  }
  try {
    const clean = write('clean', binary)
    const planted = write(
      'planted',
      Buffer.concat([binary, Buffer.from(sentinels.at(-1))])
    )
    assert.equal(run('check', clean).status, 0, 'should accept a clean binary')
    const refused = run('check', clean, planted)
    assert.equal(refused.status, 1, 'should refuse a planted binary')
    assert.match(refused.stderr, /planted operator configuration/)

    const wasm = write('valid.wasm', Buffer.from('0061736d01000000', 'hex'))
    const accepted = run('smoke', wasm, 'wasm32-wasip1')
    assert.equal(accepted.status, 0, accepted.stderr)
    assert.match(accepted.stdout, /SUCCESS: final-byte checks wasm32-wasip1/)
    const invalid = run('smoke', clean, 'wasm32-wasip1')
    assert.equal(invalid.status, 1, 'should refuse non-wasm final bytes')
    assert.match(invalid.stderr, /wasm/)

    const { original, document } = spdxFixture()
    document.packages.shift()
    delete document.documentDescribes
    delete document.relationships
    original.metadata.properties = [
      { name: 'trusted-server:coverage', value: 'Fixture coverage.' },
    ]
    const spdx = write('sbom.spdx.json', JSON.stringify(document))
    const inventory = write('combined.cdx.json', JSON.stringify(original))
    const completed = run('validate-spdx', spdx, clean, inventory)
    assert.equal(completed.status, 0, completed.stderr)
    const output = JSON.parse(readFileSync(spdx, 'utf8'))
    assert.deepEqual(output.documentDescribes, ['SPDXRef-Artifact'])
    assert.match(output.creationInfo.comment, /Fixture coverage\./)
    write('sbom.spdx.json', JSON.stringify(document))
    const rescanned = run('validate-spdx', spdx, planted, inventory)
    assert.equal(rescanned.status, 1, 'should rescan the final binary')
    assert.match(rescanned.stderr, /planted operator configuration/)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})
test('every canary the release build plants is a scanned sentinel', () => {
  const script = readFileSync('scripts/release/phase1-build.sh', 'utf8')
  const canary = 'phase1-[a-z0-9-]*canary[a-z0-9.-]*'
  const exported = [
    ...script.matchAll(new RegExp(`^export (\\w+)=\\S*?(${canary})$`, 'gm')),
  ].map(([, name, value]) => `${name}=${value}`)
  const config = script.slice(
    script.indexOf("cat > trusted-server.toml <<'CONFIG'\n"),
    script.indexOf('\nCONFIG\n')
  )
  const configured = [
    ...config.matchAll(new RegExp(`^(\\w+) = "\\S*?(${canary})"$`, 'gm')),
  ].map(([, key, value]) => `${key}=${value}`)
  assert.deepEqual(
    exported,
    [
      'TRUSTED_SERVER__PUBLISHER__ORIGIN_URL=phase1-origin-canary.example.com',
      'TRUSTED_SERVER__PUBLISHER__PROXY_SECRET=phase1-secret-canary-32-characters',
      'EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME=phase1-physical-store-canary',
      'FASTLY_SERVICE_ID=phase1-service-account-canary',
    ],
    'should plant every deployment variable canary'
  )
  assert.deepEqual(
    configured,
    [
      'origin_url=phase1-origin-canary.example.com',
      'proxy_secret=phase1-secret-canary-32-characters',
      'policy=phase1-permission-policy-canary',
    ],
    'should plant every config file canary'
  )
  assert.deepEqual(
    [
      ...new Set(
        [...exported, ...configured].map((entry) => entry.split('=')[1])
      ),
    ].sort(),
    [...sentinels].sort(),
    'should scan for exactly the planted canaries'
  )
  // The GNU builds run in a container that sees only the variables passed in.
  for (const name of [
    'TRUSTED_SERVER__RELEASE_BUILD',
    'TRUSTED_SERVER__GIT_VERSION',
    ...exported.map((entry) => entry.split('=')[0]),
  ])
    assert.match(
      script,
      new RegExp(`--env ${name} `),
      `should pass ${name} into the GNU builder`
    )
})
test('post-publish retries distinguish transient failures and cap Retry-After', async () => {
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
    const retry = readRetrier({
      deadline: 180000,
      now: () => now,
      sleep: async (ms) => {
        waits.push(ms)
        now += ms
      },
      warn: (line) => logs.push(line),
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

test('rejects invalid wasm, foreign ELF and GNU symbols above 2.34', () => {
  const wasm = Buffer.from('0061736d01000000', 'hex')
  checkReleaseFormat(wasm, 'wasm32-wasip1')
  for (const bytes of [
    wasm.subarray(0, 7),
    Buffer.alloc(8),
    Buffer.from('0061736d02000000', 'hex'),
  ])
    assert.throws(() => checkReleaseFormat(bytes, 'wasm32-wasip1'), /wasm/)
  // A 64-bit little-endian ELF header with the given e_machine.
  const elf = (machine = 0x3e, elfClass = 2) => {
    const bytes = Buffer.alloc(64)
    Buffer.from('7f454c46', 'hex').copy(bytes)
    bytes[4] = elfClass
    bytes[5] = 1
    bytes.writeUInt16LE(machine, 18)
    return bytes
  }
  const gnu = 'x86_64-unknown-linux-gnu'
  checkReleaseFormat(elf(), gnu, 'GLIBC_2.2.5 GLIBC_2.34')
  for (const versions of [
    'GLIBC_2.35',
    'GLIBC_3.0',
    'GLIBC_2.34.1',
    'GLIBC_ABI_DT_RELR',
  ])
    assert.throws(() => checkReleaseFormat(elf(), gnu, versions), /GLIBC/)
  // aarch64 (EM_AARCH64) and 32-bit ELF are refused for the x86_64 target.
  for (const bytes of [elf(0xb7), elf(0x3e, 1), wasm])
    assert.throws(() => checkReleaseFormat(bytes, gnu, 'GLIBC_2.34'), /ELF/)
  assert.throws(
    () => checkReleaseFormat(elf(), 'x86_64-unknown-linux-musl'),
    /Unsupported release target/
  )
})
test('smoke reads glibc version needs from the final Linux bytes', () => {
  const root = mkdtempSync(join(tmpdir(), 'phase1-smoke-'))
  try {
    const bin = join(root, 'bin')
    mkdirSync(bin)
    // Report a glibc 2.35 need only when version information is requested.
    writeFileSync(
      join(bin, 'readelf'),
      '#!/bin/sh\ncase " $* " in *" --version-info "*) echo "Name: GLIBC_2.35" ;; esac\n',
      { mode: 0o755 }
    )
    const header = Buffer.alloc(64)
    Buffer.from('7f454c460201', 'hex').copy(header)
    header.writeUInt16LE(0x3e, 18)
    const asset = join(root, 'ts-x86_64-unknown-linux-gnu')
    writeFileSync(asset, header)
    const result = spawnSync(
      process.execPath,
      [
        fileURLToPath(new URL('./phase1.mjs', import.meta.url)),
        'smoke',
        asset,
        'x86_64-unknown-linux-gnu',
      ],
      {
        cwd: root,
        encoding: 'utf8',
        env: { PATH: `${bin}:${process.env.PATH}` },
      }
    )
    assert.equal(result.status, 1, 'should refuse a glibc 2.35 need')
    assert.match(result.stderr, /GLIBC requirement exceeds 2\.34/)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
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
test('workflows build, sign and publish exactly the release assets and restrict their jobs', () => {
  const names = [...releaseAssets].sort()
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
  assert.deepEqual([...releaseAssets].sort(), names)
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
  // Tag checks run before any build; only tag builds upload signing inputs.
  const checks = build.split('\n  checks:')[1].split('\n  build:')[0]
  for (const step of [
    'Refuse non-SemVer release tags',
    'Refuse annotated release tags',
    'Refuse tags on commits outside every branch',
  ])
    assert.ok(checks.includes(`\n      - name: ${step}\n`), step)
  // The branch check needs every branch's history.
  assert.match(
    checks,
    /- uses: actions\/checkout@v\d+\n {8}with:\n {10}persist-credentials: false\n {10}fetch-depth: 0\n/
  )
  assert.ok(actualBuild.startsWith('\n    needs: checks\n'))
  assert.match(
    actualBuild,
    /- uses: actions\/upload-artifact@v\d+\n {8}if: startsWith\(github\.ref, 'refs\/tags\/v'\)\n/
  )
  // Each job keeps exactly its own permissions.
  const permissions = (job) =>
    job
      .match(/\n {4}permissions:\n((?: {6}\S.*\n)+)/)[1]
      .trim()
      .split(/\s+/)
  assert.deepEqual(permissions(checks), ['contents:', 'read'])
  // A queued run must never cancel a publication in progress.
  assert.match(release, /\n  cancel-in-progress: false\n/)
  assert.deepEqual(permissions(release.split('\n  release:')[1]), [
    'contents:',
    'write',
  ])
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
  assert.match(comparison, /ref: \$\{\{ github\.sha \}\}/)
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
  assert.match(job('sign'), /needs: \[checks, build, fastly-comparison\]/)
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

test('release build keeps locked builds, pinned generators and final integrity checks', () => {
  const script = readFileSync('scripts/release/phase1-build.sh', 'utf8')
  const commands = script.split('\n').filter((line) => !/^\s*#/.test(line))
  const builds = commands.filter((line) =>
    /\bcargo (?:--config "\$ordinary_rustflags" )?build\b/.test(line)
  )
  assert.equal(
    builds.length,
    3,
    'should build in the GNU container, the ordinary Fastly path and natively'
  )
  for (const line of builds) assert.match(line, /--locked/)
  // Native binaries drop line-level debug info in both build paths.
  assert.match(
    script,
    /if \[\[ "\$build_target" != wasm32-wasip1 \]\]; then\n {2}export CARGO_PROFILE_RELEASE_STRIP=debuginfo\nfi\n/
  )
  assert.match(script, /--env CARGO_PROFILE_RELEASE_STRIP /)
  assert.ok(
    script.indexOf('export CARGO_PROFILE_RELEASE_STRIP=debuginfo') <
      script.search(/\bcargo build\b/),
    'should set the strip before either build'
  )
  // The copied release bytes are scanned and smoke-checked before any
  // inventory is generated for them.
  const step = (command) => {
    const index = commands.indexOf(command)
    assert.ok(index >= 0, `should run: ${command}`)
    return index
  }
  const copied = step(
    'cp "$CARGO_TARGET_DIR/$build_target/release/$executable" "release-assets/$release_asset"'
  )
  const checked = step(
    'node scripts/release/phase1.mjs check "release-assets/$release_asset"'
  )
  const smoked = step(
    'node scripts/release/phase1.mjs smoke "release-assets/$release_asset" "$build_target"'
  )
  assert.ok(
    copied < checked &&
      checked < smoked &&
      smoked < commands.findIndex((line) => line.startsWith('tool_root=')),
    'should check and smoke the copied binary before generating inventories'
  )
  // The published SPDX gains its binary-bound root and is validated only by
  // the completion step after conversion.
  const merged = step('node scripts/release/phase1.mjs merge \\')
  const converted = commands.findIndex((line) =>
    line.startsWith('"$tool_root/converter" convert ')
  )
  const completed = step('node scripts/release/phase1.mjs validate-spdx \\')
  assert.ok(
    smoked < merged && merged < converted && converted < completed,
    'should merge, convert, then complete and validate the SPDX'
  )
  assert.match(script, /cargo install \\\n[^\n]*--locked/)
  assert.match(script, /cargo metadata --locked/)
  // Generator crate and every converter binary are checksum-pinned before use.
  assert.match(
    script,
    /printf '%s {2}%s\\n' [a-f0-9]{64} "\$tool_root\/generator\.crate" \| shasum -a 256 --check\n/
  )
  assert.equal((script.match(/converter_sha=[a-f0-9]{64} ;;/g) ?? []).length, 3)
  assert.match(
    script,
    /printf '%s {2}%s\\n' "\$converter_sha" "\$tool_root\/converter" \| shasum -a 256 --check\n/
  )
  // The released bytes must be unchanged since checks ran, and the build must
  // not have rewritten the lockfiles or manifest.
  const integrity = step(
    'git diff --exit-code -- Cargo.lock crates/trusted-server-js/lib/package-lock.json edgezero.toml'
  )
  assert.equal(
    commands[integrity - 1],
    'shasum -a 256 --check "$RUNNER_TEMP/binary.sha256"'
  )
  // The ordinary Fastly observation re-checks the bytes after measuring them.
  const tail = commands
    .filter(Boolean)
    .slice(-2)
    .map((line) => line.trim())
  assert.deepEqual(tail, [
    'shasum -a 256 --check "$RUNNER_TEMP/binary.sha256"',
    'fi',
  ])
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
    // The cleanup trap removes the partly written config.
    stub('rm', 'exec /bin/rm "$@"')
    for (const tool of [
      'cat',
      'node',
      'npm',
      'docker',
      'cargo',
      'rustup',
      'curl',
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
      'should reach the first safe tool stub when preflight passes'
    )
    assert.equal(readFileSync(marker, 'utf8').trim(), join(bin, 'cat'))
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
test('post-publish reads cannot succeed after the shared deadline', async () => {
  let now = 0
  const retry = readRetrier({
    deadline: 180000,
    now: () => now,
    warn: () => assert.fail('should not retry deadline exhaustion'),
  })
  await assert.rejects(
    () =>
      retry(() => {
        now = 180000
        return 'late success'
      }),
    /window exhausted/
  )
  await assert.rejects(
    () =>
      retry(() => assert.fail('should not start a read after the deadline')),
    /^Error: Post-publish verification window exhausted$/
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
    'member-inventory',
    'existing-target',
    'nonempty-assets',
  ]) {
    const root = mkdtempSync(join(tmpdir(), 'phase1-cleanup-'))
    const bin = join(root, 'bin')
    const inventory = join(root, 'crates/trusted-server-cli/phase1.json')
    // The generator also writes beside every other workspace member.
    const member = join(root, 'crates/trusted-server-core/phase1.json')
    const config = join(root, 'trusted-server.toml')
    try {
      mkdirSync(bin)
      mkdirSync(join(root, 'crates/trusted-server-cli'), { recursive: true })
      mkdirSync(join(root, 'crates/trusted-server-core'), { recursive: true })
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
          printf '%s\\n' '#!/bin/bash' 'printf "{}" > crates/trusted-server-cli/phase1.json' 'printf "{}" > crates/trusted-server-core/phase1.json' 'if [[ "$FIXTURE_MODE" == generator-failure ]]; then exit 98; fi' > "$RUNNER_TEMP/phase1-cyclonedx/install/bin/cargo-cyclonedx"
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
      if (mode === 'member-inventory')
        fs.symlinkSync('missing-user-file', member)
      if (mode === 'existing-target') mkdirSync(join(root, 'phase1-target'))
      if (mode === 'nonempty-assets') {
        mkdirSync(join(root, 'release-assets'))
        writeFileSync(join(root, 'release-assets/stale'), 'stale')
      }
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
      if (mode === 'member-inventory')
        assert.equal(fs.readlinkSync(member), 'missing-user-file')
      else
        assert.equal(
          fs.existsSync(member),
          false,
          `${mode}: member inventory cleanup`
        )
    } finally {
      rmSync(root, { recursive: true, force: true })
    }
  }
})
test('smoke accepts only the release build facts in ts --version', () => {
  const manifest = '[workspace.package]\nversion = "1.2.3"\n'
  const source = 'a'.repeat(40)
  const target = 'x86_64-unknown-linux-gnu'
  const facts = `ts package=1.2.3 source=${source} target=${target} variant=default`
  checkCliVersion(`${facts}\n`, manifest, source, target)
  for (const output of [
    `ts package=1.2.3 source=unknown target=${target} variant=default`,
    `ts package=1.2.4 source=${source} target=${target} variant=default`,
    `ts package=1.2.3 source=${source} target=aarch64-apple-darwin variant=default`,
    `${facts} extra`,
    'ts 1.2.3',
  ])
    assert.throws(
      () => checkCliVersion(output, manifest, source, target),
      /Unexpected CLI version/,
      output
    )
})
// Runs a workflow step's shell with stub tools, as GitHub's `bash -eo pipefail`.
const runStep = (workflow, step, stubs, env) => {
  const bin = mkdtempSync(join(tmpdir(), 'phase1-step-'))
  try {
    for (const [name, body] of Object.entries(stubs))
      writeFileSync(join(bin, name), `#!/bin/sh\n${body}\n`, { mode: 0o755 })
    return spawnSync(
      'bash',
      [
        '--noprofile',
        '--norc',
        '-eo',
        'pipefail',
        '-c',
        runBlock(workflow, step),
      ],
      {
        cwd: bin,
        encoding: 'utf8',
        env: { PATH: `${bin}:${process.env.PATH}`, ...env },
      }
    )
  } finally {
    rmSync(bin, { recursive: true, force: true })
  }
}
test('build checks refuse a tag reference that is not a lightweight tag at the commit', () => {
  const workflow = readFileSync('.github/workflows/release-build.yml', 'utf8')
  const commit = 'a'.repeat(40)
  for (const [reference, status] of [
    [`commit ${commit}`, 0],
    [`tag ${commit}`, 1],
    [`commit ${'b'.repeat(40)}`, 1],
    [null, 1],
  ]) {
    const result = runStep(
      workflow,
      'Refuse annotated release tags',
      {
        gh:
          reference === null
            ? 'echo "HTTP 404: Not Found" >&2; exit 1'
            : `printf '%s\\n' '${reference}'`,
      },
      {
        GITHUB_REPOSITORY: 'example/project',
        GITHUB_SHA: commit,
        RELEASE_TAG: 'v1.2.3',
      }
    )
    assert.equal(result.status, status, `${reference}: ${result.stderr}`)
    if (reference?.startsWith('tag'))
      assert.match(result.stdout, /::error::Release tags must be lightweight/)
  }
})
test('build checks refuse a tagged commit that is on no branch', () => {
  const workflow = readFileSync('.github/workflows/release-build.yml', 'utf8')
  const commit = 'a'.repeat(40)
  for (const [branches, status] of [
    ['  origin/main', 0],
    ['  origin/main\n  origin/release/v1', 0],
    ['', 1],
    [null, 1],
  ]) {
    const result = runStep(
      workflow,
      'Refuse tags on commits outside every branch',
      {
        // Answer only the exact containment query for the tagged commit.
        git:
          branches === null
            ? 'echo "fatal: malformed object name" >&2; exit 129'
            : `test "$*" = "branch -r --contains ${commit}" || exit 97; printf '%s' '${branches}'`,
      },
      { GITHUB_SHA: commit }
    )
    // A failing git query stops the step with git's own status.
    if (branches === null)
      assert.notEqual(result.status, 0, 'should stop when git fails')
    else assert.equal(result.status, status, `${branches}: ${result.stderr}`)
    if (branches === '')
      assert.match(
        result.stdout,
        new RegExp(`::error::Push ${commit} to a branch before tagging it`)
      )
  }
})
test('publisher step refuses a Node other than the pinned version before publishing', () => {
  const release = readFileSync('.github/workflows/release.yml', 'utf8')
  const step = 'Verify handoff, create once, publish by release ID and verify'
  assert.match(
    release.split(`- name: ${step}`)[1],
    /\n {10}NODE_VERSION: \$\{\{ steps\.tools\.outputs\.node \}\}\n/,
    'should compare against the Node version the job installed'
  )
  for (const [installed, status] of [
    ['v24.20.0', 0],
    ['v20.20.2', 1],
  ]) {
    const result = runStep(
      release,
      step,
      {
        gh: 'echo "gh version 2.102.0 (2026-01-01)"',
        node: `if [ "$1" = --version ]; then echo ${installed}; else echo published; fi`,
      },
      { NODE_VERSION: '24.20.0' }
    )
    assert.equal(result.status, status, `${installed}: ${result.stderr}`)
    assert.equal(
      result.stdout.includes('published'),
      status === 0,
      `${installed}: should run the publisher only on the pinned Node`
    )
  }
})
