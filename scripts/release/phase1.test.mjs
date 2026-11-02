import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
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
import { fileURLToPath } from 'node:url'
import {
  audit,
  checkBinary,
  checkSource,
  mergeSboms,
  sentinels,
} from './phase1.mjs'
import {
  publish,
  readRetrier,
  releaseAssets,
  releaseNotes,
} from './publish.mjs'
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
  const asset = 'example-asset'
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
      const env = { PATH: process.env.PATH, ASSET: asset }
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
const hash = (bytes) => createHash('sha256').update(bytes).digest('hex')
const canonicalBlock = (markdown) => {
  const block = markdown.match(
    /<!-- release-verification:start -->\n\n```bash\n([\s\S]*?)\n```\n\n<!-- release-verification:end -->/
  )
  assert.ok(block, 'should mark the canonical verification block')
  return block[1]
}
const normalizePins = (code) =>
  code.replace(/^(REPO|TAG|COMMIT|ASSETS)=.*$/gm, '$1=<pin>').trim()
test('release notes match the guide and never pre-fill reviewed pins', () => {
  const body = releaseNotes('example/project', 'v1.2.3-rc.1')
  const code = body.match(/```bash\n([\s\S]*?)\n```/)[1]
  assert.equal(
    normalizePins(code),
    normalizePins(
      canonicalBlock(readFileSync('docs/guide/release-verification.md', 'utf8'))
    ),
    'release commands should match the guide'
  )
  assert.match(code, /^REPO=$/m, 'should leave REPO for independent review')
  assert.match(code, /^TAG=$/m, 'should leave TAG for independent review')
  assert.match(code, /^COMMIT=$/m, 'should leave COMMIT for independent review')
  assert.match(code, new RegExp(`^ASSETS=\\(${assets.join(' ')}\\)$`, 'm'))
  assert.match(body, /stay editable after publication/)
  assert.match(
    body,
    /blob\/v1\.2\.3-rc\.1\/docs\/guide\/release-verification\.md/
  )
  assert.doesNotMatch(body, /\{\{/)
  execFileSync('bash', ['-n'], { input: code })
  // The unset pins must stop the block before any verification runs.
  const bin = mkdtempSync(join(tmpdir(), 'phase1-notes-'))
  try {
    writeFileSync(join(bin, 'gh'), '#!/bin/sh\necho ran >> "$0.log"\n', {
      mode: 0o755,
    })
    // macOS ships bash 3.2, where a failing `[[ ]]` alone does not trigger
    // `set -e`; check it wherever it is installed.
    for (const shell of ['bash', '/bin/bash'].filter(
      (shell) => shell === 'bash' || fs.existsSync(shell)
    ))
      for (const input of [
        code,
        code
          .replace(/^REPO=$/m, 'REPO=example/project')
          .replace(/^TAG=$/m, 'TAG=v1.2.3'),
      ]) {
        assert.throws(() =>
          execFileSync(shell, ['-c', input], {
            cwd: bin,
            env: { PATH: `${bin}:${process.env.PATH}` },
            stdio: 'pipe',
          })
        )
        assert.equal(fs.existsSync(join(bin, 'gh.log')), false, shell)
      }
  } finally {
    rmSync(bin, { recursive: true, force: true })
  }
})
test('publication binds exact signing handoffs and refuses drafts, corruption and partial uploads', async () => {
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
    'draft-page-2',
    'api-error',
    'missing',
    'extra',
    'changed',
    'empty-bundle',
    'symlink-input',
    'upload-error',
    'upload-digest',
    'upload-size',
    'mutable',
    'wrong-id',
    'published-asset-digest',
    'published-asset-extra',
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
    const waits = [],
      ignored = []
    let downloads = 0,
      verifies = 0,
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
        if (
          calls.some(([kind]) => kind === 'gh') &&
          (mode === 'post-read-error' ||
            (mode === 'idle-socket' && options.headers.Connection !== 'close'))
        )
          throw new TypeError('other side closed')
        calls.push([method, u.pathname + u.search])
        const response = (body, status = 200) =>
          new Response(
            typeof body === 'string' || Buffer.isBuffer(body)
              ? body
              : JSON.stringify(body),
            { status }
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
                  : mode === 'attestation-verify-past-deadline'
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
          const creation = JSON.parse(options.body)
          assert.equal(creation.prerelease, !stable)
          assert.equal(creation.make_latest, stable ? 'legacy' : 'false')
          assert.equal(creation.body, releaseNotes('example/project', tag))
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
          return response(
            mode === 'upload-digest'
              ? { ...item, digest: `sha256:${'0'.repeat(64)}` }
              : mode === 'upload-size'
                ? { ...item, size: item.size + 1 }
                : item,
            201
          )
        }
        if (u.pathname.includes('/releases/tags/'))
          return response({
            ...release(false),
            id: mode === 'wrong-id' ? 43 : 42,
          })
        if (u.pathname.endsWith('/releases/42')) {
          const current = release(method !== 'PATCH' && uploaded.length === 0)
          if (mode === 'published-asset-extra')
            current.assets.push({ name: 'unexpected', size: 1 })
          return response(current)
        }
        throw Error(`Unexpected ${method} ${url}`)
      }
      const execFileSync = (...args) => {
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
            fs.writeFileSync(join(dir, item.name), item.bytes)
          if (mode === 'download-corrupt')
            fs.writeFileSync(join(dir, assets[0]), 'corrupt')
          if (mode === 'download-manifest')
            fs.writeFileSync(join(dir, 'SHA256SUMS'), 'corrupt')
          if (mode === 'download-missing') fs.unlinkSync(join(dir, assets[0]))
          if (mode === 'download-extra')
            fs.writeFileSync(join(dir, 'extra'), 'extra')
          return Buffer.from('downloaded')
        }
        if (args[1][1] !== 'verify') return Buffer.from('verified asset')
        verifies++
        const verifiedTag = args[1].at(-1)
        assert.equal(verifiedTag, tag, 'should verify only the current tag')
        assert.ok(
          now + args[2].timeout <= 60000,
          'should bound proof verification by the availability deadline'
        )
        if (mode === 'attestation-verify-transient' && verifies === 1)
          throw Object.assign(Error('gh failed'), {
            stderr: Buffer.from('HTTP 503: Service Unavailable'),
          })
        // Each proof fetch runs until its command timeout.
        if (mode === 'attestation-verify-past-deadline' && verifies <= 2) {
          now += args[2].timeout
          throw Object.assign(Error('timeout'), { code: 'ETIMEDOUT' })
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
        'download-transient',
        'attestation-404-then-present',
        'attestation-after-two-waits',
        'attestation-other-tag',
        'attestation-other-tag-before-current',
        'attestation-other-unverifiable',
        'attestation-verify-transient',
        'attestation-list-transient',
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
        'attestation-truncated': /truncated/,
        'attestation-duplicate-subject': /Invalid release attestation subjects/,
        'attestation-payload-type': /Malformed release attestation bundle/,
        'draft-page-2': /already exists/,
        'symlink-input': /Invalid file/,
        'upload-digest': /Upload mismatch/,
        'published-asset-digest': /Published asset digest mismatch/,
        'published-asset-extra': /Published asset set mismatch/,
        'attestation-404-never': /unavailable after five checks/,
        'attestation-verify-past-deadline': /retry window exhausted/,
        'attestation-statement-mismatch': /statement mismatch/,
        'attestation-substituted-bundle': /bundle mismatch/,
        'attestation-null-bundle-never': /Signature verification failed/,
        'attestation-null-bundle-no-url':
          /Malformed release attestation bundle/,
        'attestation-null-bundle-wrong-assets': /identity or assets mismatch/,
      }[mode]
      if (successful) await execute()
      else
        await assert.rejects(
          execute,
          expectedError ??
            (mode.startsWith('download-') && mode !== 'download-never'
              ? /mismatch|Unexpected artifact contents|ENOENT/
              : undefined),
          mode
        )
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
          'symlink-input',
        ].includes(mode)
      )
        assert.ok(
          !calls.some(([method]) => ['POST', 'PATCH'].includes(method)),
          mode
        )
      if (mode === 'draft-page-2')
        assert.ok(
          calls.some(([, path]) =>
            /\/releases\?per_page=100&page=2$/.test(path)
          ),
          'should read the second release page'
        )
      if (mode === 'api-error' || mode === 'upload-error')
        assert.equal(calls.length, mode === 'api-error' ? 2 : 4)
      if (['upload-error', 'upload-digest', 'upload-size'].includes(mode))
        assert.ok(!calls.some(([method]) => method === 'PATCH'), mode)
      if (successful) {
        assert.equal(uploaded.length, 29)
        assert.equal(
          calls.filter(
            ([method, args]) => method === 'gh' && args[1][1] === 'verify-asset'
          ).length,
          29
        )
        assert.equal(
          verifies,
          [
            'attestation-verify-transient',
            'attestation-null-bundle-late',
          ].includes(mode)
            ? 2
            : 1,
          mode
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
          'attestation-non-200': [[2000, 5000], 3],
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
})
test('release build keeps locked builds, pinned generators and final integrity checks', () => {
  const script = readFileSync('scripts/release/phase1-build.sh', 'utf8')
  const commands = script.split('\n').filter((line) => !/^\s*#/.test(line))
  const builds = commands.filter((line) => /\bcargo build\b/.test(line))
  assert.equal(
    builds.length,
    2,
    'should build in the GNU container and natively'
  )
  for (const line of builds) assert.match(line, /--locked/)
  assert.match(script, /cargo install \\\n[^\n]*--locked/)
  assert.match(script, /cargo metadata --locked/)
  // Generator crate and every converter binary are checksum-pinned before use.
  assert.match(
    script,
    /printf '%s {2}%s\\n' [a-f0-9]{64} "\$tool_root\/generator\.crate" \| shasum -a 256 --check\n/
  )
  assert.equal((script.match(/converter_sha=[a-f0-9]{64} ;;/g) ?? []).length, 4)
  assert.match(
    script,
    /printf '%s {2}%s\\n' "\$converter_sha" "\$tool_root\/converter" \| shasum -a 256 --check\n/
  )
  // The released bytes must be unchanged since checks ran, and the build must
  // not have rewritten the lockfiles or manifest.
  const tail = commands.filter(Boolean).slice(-2)
  assert.deepEqual(tail, [
    'shasum -a 256 --check "$RUNNER_TEMP/binary.sha256"',
    'git diff --exit-code -- Cargo.lock crates/trusted-server-js/lib/package-lock.json edgezero.toml',
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

test('audit tokenizes byte and C raw strings and astral char literals', () => {
  const planted = 'env!("TRUSTED_SERVER__EC__PASSPHRASE")'
  for (const source of [
    `const A: &[u8] = br#"a"#; ${planted}; const B: &[u8] = br#"b"#;`,
    `const A: &core::ffi::CStr = cr#"a"#; ${planted}; const B: &core::ffi::CStr = cr#"b"#;`,
    `const A: [char; 2] = ['😀','"']; ${planted}; const B: char = '"';`,
  ])
    assert.throws(
      () => checkSource('src/lib.rs', source),
      /compile-time/,
      source
    )
  assert.doesNotThrow(() =>
    checkSource(
      'src/lib.rs',
      'const P: &[u8] = br"C:\\"; const Q: char = \'😀\';'
    )
  )
})
test('build-script rule covers modules build scripts load and build helper directories', () => {
  const root = mkdtempSync(join(tmpdir(), 'phase1-audit-'))
  const write = (file, source) => {
    mkdirSync(join(root, file, '..'), { recursive: true })
    writeFileSync(join(root, file), source)
  }
  const leak = 'const NAME: &str = "TRUSTED_SERVER__PUBLISHER__ORIGIN_URL";'
  try {
    write(
      'a/build.rs',
      '#[path = "../b/helper/x.rs"]\nmod x;\nmod y;\nfn main() {}'
    )
    write('b/helper/x.rs', 'mod nested;')
    write('b/helper/nested.rs', '')
    write('a/y.rs', '')
    write('c/build_support/z.rs', '')
    write('c/build/w.rs', '')
    write('c/src/lib.rs', leak)
    audit(root)
    for (const file of [
      'b/helper/x.rs',
      'b/helper/nested.rs',
      'a/y.rs',
      'c/build_support/z.rs',
      'c/build/w.rs',
    ]) {
      write(file, leak)
      assert.throws(
        () => audit(root),
        new RegExp(
          `${file.replaceAll('/', '[/\\\\]')}: operator compile-time build input`
        ),
        file
      )
      write(file, file === 'b/helper/x.rs' ? 'mod nested;' : '')
    }
    // Messages that name a variable are not variable names.
    write(
      'c/build/w.rs',
      'const M: &str = "TRUSTED_SERVER__RELEASE_BUILD must be exactly 1";'
    )
    audit(root)
    // A padded or templated deployment name is still a deployment name.
    for (const name of [
      'TRUSTED_SERVER__EC__PASSPHRASE ',
      'TRUSTED_SERVER__EC__PASSPHRASE\\n',
      'TRUSTED_SERVER__{} must be set',
    ]) {
      write('c/build/w.rs', `const M: &str = "${name}";`)
      assert.throws(
        () => audit(root),
        /operator compile-time build input/,
        name
      )
    }
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
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
