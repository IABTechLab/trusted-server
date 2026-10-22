import assert from 'node:assert/strict'
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
import { spawnSync } from 'node:child_process'
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
    output.dependencies.find((d) => d.ref === 'cargo:root').dependsOn,
    ['cargo:dependency', 'npm:root']
  )
  assert.equal(
    new Set(output.components.map((c) => c['bom-ref'])).size,
    output.components.length
  )
  assert.throws(
    () => mergeSboms({}, bom('npm'), Buffer.from('binary'), 'example'),
    /CycloneDX/
  )
})

test('workflow signs only tags, separates privileged jobs, and never checks out in signing', () => {
  const workflow = readFileSync(
    new URL('../../.github/workflows/release.yml', import.meta.url),
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
  assert.match(workflow, /cancel-in-progress: false/)
  assert.doesNotMatch(workflow, /workflow_dispatch|--clobber/)
  for (const line of workflow
    .split('\n')
    .filter((line) => /uses: .*@/.test(line))) {
    assert.match(line, /@[a-f0-9]{40} # v/)
  }
})

test('release publication refuses existing releases and API errors, and does not publish after upload failure', () => {
  const workflow = readFileSync(
    new URL('../../.github/workflows/release.yml', import.meta.url),
    'utf8'
  )
  const script = workflow
    .slice(workflow.lastIndexOf('        run: |\n') + '        run: |\n'.length)
    .split('\n')
    .map((line) => line.slice(10))
    .join('\n')
  const root = mkdtempSync(join(tmpdir(), 'phase1-publication-'))
  try {
    const bin = join(root, 'bin')
    mkdirSync(bin)
    writeFileSync(
      join(bin, 'gh'),
      `#!/usr/bin/env node
const fs = require('node:fs');
const args = process.argv.slice(2);
fs.appendFileSync(process.env.GH_LOG, JSON.stringify(args) + '\\n');
const mode = process.env.FIXTURE_MODE;
if (args[0] === 'api' && args.length === 2) {
  if (mode === 'existing') { process.stdout.write('{}'); process.exit(0); }
  process.stderr.write(mode === 'api-error' ? 'HTTP 403' : 'HTTP 404'); process.exit(1);
}
if (args[0] === 'api') process.stdout.write(mode === 'mutable' ? 'false' : 'true');
if (args[1] === 'upload' && mode === 'upload-error') process.exit(1);
`,
      { mode: 0o755 }
    )
    writeFileSync(
      join(bin, 'sha256sum'),
      '#!/bin/sh\nexec shasum -a 256 "$@"\n',
      { mode: 0o755 }
    )
    for (const mode of [
      'existing',
      'api-error',
      'upload-error',
      'mutable',
      'success',
    ]) {
      const working = join(root, mode)
      mkdirSync(join(working, 'release-assets'), { recursive: true })
      for (const asset of [
        'trusted-server-fastly.wasm',
        'ts-x86_64-unknown-linux-gnu',
        'ts-aarch64-apple-darwin',
        'ts-x86_64-apple-darwin',
      ]) {
        for (const suffix of ['', '.cdx.json'])
          writeFileSync(
            join(working, 'release-assets', asset + suffix),
            'fictional fixture'
          )
      }
      const log = join(working, 'gh.log')
      const result = spawnSync('bash', ['-c', script], {
        cwd: working,
        encoding: 'utf8',
        env: {
          ...process.env,
          PATH: `${bin}:${process.env.PATH}`,
          GH_REPO: 'example/project',
          RELEASE_TAG: 'v0.0.0-example',
          GH_LOG: log,
          FIXTURE_MODE: mode,
        },
      })
      const calls = readFileSync(log, 'utf8').trim().split('\n').map(JSON.parse)
      assert.equal(
        result.status,
        mode === 'success' ? 0 : 1,
        `${mode}: ${result.stderr}`
      )
      if (['existing', 'api-error'].includes(mode))
        assert.equal(calls.length, 1)
      if (mode === 'upload-error')
        assert.ok(!calls.some((args) => args[1] === 'edit'))
      if (mode === 'mutable') assert.match(result.stdout, /NOT immutable/)
      if (mode === 'success')
        assert.equal(
          calls.filter((args) => args[1] === 'verify-asset').length,
          9
        )
    }
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
