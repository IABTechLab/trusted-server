#!/usr/bin/env node
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { spawnSync } from 'node:child_process'
import {
  closeSync,
  existsSync,
  mkdirSync,
  openSync,
  readFileSync,
  writeFileSync,
} from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { parseArgs } from 'node:util'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const exporterPath =
  'crates/trusted-server-integration-tests/examples/cwv_html.rs'
const publisherPath = 'crates/trusted-server-core/src/publisher.rs'
const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex')

function replaceOnce(source, before, after) {
  assert.equal(
    source.split(before).length,
    2,
    `Expected one fixture adaptation: ${before}`
  )
  return source.replace(before, after)
}

/** Adapt only the copied test exporter, leaving baseline production behavior intact. */
export function baselineExporter(source) {
  source = replaceOnce(
    source,
    '    AssemblyMode, CreativeOpportunityInitialization,',
    '    AssemblyMode,'
  )
  source = replaceOnce(
    source,
    '        ("registered", "/news/article", false, false, false, 250),\n',
    ''
  )
  source = replaceOnce(
    source,
    `        if name == "registered" {
            page_settings
                .creative_opportunities
                .as_mut()
                .expect("should configure slots")
                .slot[0]
                .initialization = CreativeOpportunityInitialization::PublisherRegistered;
        }
`,
    ''
  )
  source = replaceOnce(source, '        .with_request_path(path)\n', '')
  source = replaceOnce(
    source,
    '        .with_enforced_csp(name == "csp")\n',
    ''
  )
  return source
}

/** Extract the test-only bridge; no loading, registration or reservation fix is backported. */
export function fixtureBridge(source) {
  const start = source.indexOf(
    '/// Generate production slot and bid snippets for isolated browser fixtures.'
  )
  const end = source.indexOf(
    '/// Build the empty-bids `<script>` tag used when no bids were returned.',
    start
  )
  assert(
    start >= 0 && end > start,
    'Expected the delimited test-only publisher bridge'
  )
  const bridge = source.slice(start, end)
  assert(bridge.includes('#[cfg(feature = "test-utils")]'))
  return bridge
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: root,
    encoding: 'utf8',
    ...options,
  })
  assert.equal(
    result.status,
    0,
    `${command} ${args.join(' ')} failed: ${result.stderr ?? result.error ?? ''}`
  )
  return result.stdout?.trim()
}

export function prepare(revision, out) {
  const oid = run('git', ['rev-parse', '--verify', `${revision}^{commit}`])
  assert(!existsSync(out), `Output already exists: ${out}`)
  mkdirSync(out, { recursive: true })
  const archive = resolve(out, 'source.tar')
  const fd = openSync(archive, 'w')
  try {
    run('git', ['archive', oid], { stdio: ['ignore', fd, 'pipe'] })
  } finally {
    closeSync(fd)
  }
  const tree = resolve(out, 'source')
  mkdirSync(tree)
  run('tar', ['-xf', archive, '-C', tree])
  const originalPublisher = readFileSync(resolve(tree, publisherPath), 'utf8')
  assert(
    !originalPublisher.includes('pub fn fixture_ad_scripts('),
    'Baseline already has this PR fixture bridge; select a pre-PR revision'
  )
  const bridge = fixtureBridge(
    readFileSync(resolve(root, publisherPath), 'utf8')
  )
  const originalExporter = readFileSync(resolve(root, exporterPath), 'utf8')
  const exporter = baselineExporter(originalExporter)
  writeFileSync(resolve(tree, publisherPath), originalPublisher + '\n' + bridge)
  mkdirSync(dirname(resolve(tree, exporterPath)), { recursive: true })
  writeFileSync(resolve(tree, exporterPath), exporter)
  const fingerprints = {}
  for (const path of [
    'Cargo.lock',
    'rust-toolchain.toml',
    '.tool-versions',
    'crates/trusted-server-js/lib/package-lock.json',
  ]) {
    const baseline = sha256(readFileSync(resolve(tree, path)))
    const candidate = sha256(readFileSync(resolve(root, path)))
    fingerprints[path] = { baseline, candidate, equal: baseline === candidate }
  }
  const manifest = {
    schemaVersion: 1,
    revision: oid,
    requestedRevision: revision,
    tree,
    candidateRevision: run('git', ['rev-parse', 'HEAD']),
    candidateStatus: run('git', ['status', '--short']),
    archiveSha256: sha256(readFileSync(archive)),
    fingerprints,
    adaptations: {
      publisher: {
        originalSha256: sha256(originalPublisher),
        bridge,
        patchedSha256: sha256(originalPublisher + '\n' + bridge),
      },
      exporter: {
        originalSha256: sha256(originalExporter),
        source: exporter,
        sha256: sha256(exporter),
      },
    },
    boundary:
      'Only a cfg(test-utils) bridge and copied exporter are added. Baseline production loading, slot initialization and CSS remain unchanged. Build with this tree’s own JS and lockfiles.',
  }
  writeFileSync(
    resolve(out, 'baseline-source.json'),
    JSON.stringify(manifest, null, 2) + '\n'
  )
  return manifest
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  const { values } = parseArgs({
    options: { revision: { type: 'string' }, out: { type: 'string' } },
  })
  assert(
    values.revision && values.out,
    'Usage: prepare-client-cwv-baseline.mjs --revision EXACT_PRE_PR_REVISION --out NEW_DIRECTORY'
  )
  const manifest = prepare(values.revision, resolve(values.out))
  console.log(`Prepared ${manifest.revision}: ${manifest.tree}`)
  console.log(
    'Build/export from that tree; set CWV_SOURCE_REVISION and CWV_SOURCE_STATUS to the recorded revision and fixture-only adaptations.'
  )
}
