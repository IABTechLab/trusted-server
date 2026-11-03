#!/usr/bin/env node
import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import {
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  closeSync,
  readFileSync,
  readdirSync,
  writeFileSync,
} from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { parseArgs } from 'node:util'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const browser = 'crates/trusted-server-integration-tests/browser'
export const BASELINE_REVISION = '51730bdaf841e4d7bb96c2258e5e98169bdff9a7'
const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex')
const readJson = (file) => JSON.parse(readFileSync(file, 'utf8'))

/** Conditions are declared before measurement, never selected from favorable results. */
export const conditions = [
  {
    name: 'normal-cold',
    delay: 0,
    cpu: 1,
    latency: 0,
    download: 0,
    upload: 0,
    cache: 'cold',
  },
  {
    name: 'normal-warm',
    delay: 0,
    cpu: 1,
    latency: 0,
    download: 0,
    upload: 0,
    cache: 'warm',
  },
  {
    name: 'mobile-cold',
    delay: 0,
    cpu: 4,
    latency: 100,
    download: 1600,
    upload: 750,
    cache: 'cold',
  },
  {
    name: 'mobile-warm',
    delay: 0,
    cpu: 4,
    latency: 100,
    download: 1600,
    upload: 750,
    cache: 'warm',
  },
  {
    name: 'delayed-cold',
    delay: 750,
    cpu: 1,
    latency: 0,
    download: 0,
    upload: 0,
    cache: 'cold',
  },
]

export function measurementArgs(options, condition) {
  return [
    `${browser}/cwv/run.cjs`,
    '--baseline',
    options.baseline,
    '--loading',
    options.loading,
    '--combined',
    options.combined,
    '--runs',
    String(options.runs),
    '--runtime-delay-ms',
    String(condition.delay),
    '--cpu-rate',
    String(condition.cpu),
    '--network-latency-ms',
    String(condition.latency),
    '--download-kbps',
    String(condition.download),
    '--upload-kbps',
    String(condition.upload),
    '--cache',
    condition.cache,
    '--out',
    resolve(options.out, condition.name),
  ]
}

export function validateMeasurement(report, condition, runs) {
  assert.equal(report.schemaVersion, 1)
  assert.equal(
    report.status,
    'passed',
    `${condition.name}: incomplete or failed measurement`
  )
  assert.equal(report.scope, 'measurement-and-correctness')
  assert.deepEqual(
    report.conditions,
    {
      runtimeDelayMs: condition.delay,
      cpuRate: condition.cpu,
      cache: condition.cache,
      network: {
        offline: false,
        latency: condition.latency,
        downloadThroughput:
          condition.download === 0 ? -1 : (condition.download * 1000) / 8,
        uploadThroughput:
          condition.upload === 0 ? -1 : (condition.upload * 1000) / 8,
      },
    },
    `${condition.name}: actual conditions differ from declared conditions`
  )
  assert.equal(
    report.samples,
    8 * runs + 24,
    'Every repeated measurement and correctness sample must complete'
  )
  for (const name of ['parser-blocking', 'reservation-removed']) {
    assert.equal(
      report.negativeControls?.[name]?.status,
      'passed',
      `${condition.name}: ${name} control did not pass`
    )
  }
}

/** Missing inventory is an incomplete acceptance run, never an offline-only success. */
export function conclusion(steps) {
  if (steps.some((step) => step.status === 'failed')) return 'failed'
  const required = [
    'units',
    'offline-registration',
    'legacy-automatic-controls',
    'framework',
    ...conditions.map(({ name }) => name),
    'real-gpt',
  ]
  if (
    required.some(
      (name) =>
        !steps.some((step) => step.name === name && step.status === 'passed')
    )
  )
    return 'blocked'
  return steps.every((step) => step.status === 'passed') ? 'passed' : 'blocked'
}

/** Snapshot every supplied artifact so later commands cannot consume a replacement release. */
export function snapshotInputs(options) {
  const directory = mkdtempSync(resolve(options.out, 'inputs-'))
  const selected = { ...options }
  for (const name of ['baseline', 'loading', 'combined', 'dist', 'prebid']) {
    selected[name] = resolve(directory, name)
    cpSync(options[name], selected[name], {
      recursive: true,
      dereference: true,
    })
  }
  for (const name of ['wasm', 'baseline-wasm', 'inventory']) {
    if (!options[name]) continue
    selected[name] = resolve(
      directory,
      name === 'inventory' ? 'inventory.json' : `${name}.wasm`
    )
    cpSync(options[name], selected[name], { dereference: true })
    const suffix =
      name === 'wasm'
        ? '.publisher-registration.json'
        : '.publisher-registration-baseline.json'
    if (name !== 'inventory' && existsSync(options[name] + suffix))
      cpSync(options[name] + suffix, selected[name] + suffix)
  }
  if (options['baseline-dist']) {
    selected['baseline-dist'] = resolve(directory, 'baseline-dist')
    cpSync(options['baseline-dist'], selected['baseline-dist'], {
      recursive: true,
      dereference: true,
    })
  }
  return { directory, options: selected, hashes: treeHashes(directory) }
}

function treeHashes(directory, prefix = '') {
  return Object.fromEntries(
    readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
      const name = prefix + entry.name
      const file = resolve(directory, entry.name)
      return entry.isDirectory()
        ? Object.entries(treeHashes(file, name + '/'))
        : [[name, sha256(readFileSync(file))]]
    })
  )
}

function git(args) {
  const result = spawnSync('git', args, { cwd: root, encoding: 'utf8' })
  assert.equal(result.status, 0, result.stderr)
  return result.stdout.trim()
}

export function trustedBaselineSources() {
  return Object.fromEntries(
    [
      'crates/trusted-server-core/src/html_processor.rs',
      'crates/trusted-server-core/src/tsjs.rs',
      'crates/trusted-server-core/src/integrations/gpt_bootstrap.js',
    ].map((file) => {
      const result = spawnSync(
        'git',
        ['show', `${BASELINE_REVISION}:${file}`],
        { cwd: root }
      )
      assert.equal(
        result.status,
        0,
        `Pinned original-main object is required: ${BASELINE_REVISION}:${file}`
      )
      return [file, sha256(result.stdout)]
    })
  )
}

export function validateExportSources(inputs, source, originalSources) {
  assert.equal(
    inputs.baseline.manifest.sourceRevision,
    BASELINE_REVISION,
    'Baseline must be the pinned original-main revision'
  )
  assert(
    originalSources && Object.keys(originalSources).length,
    'Trusted original-main source fingerprints are required'
  )
  const hashes = (manifest) =>
    Object.fromEntries(
      manifest.sourceFingerprints
        .split('\n')
        .filter(Boolean)
        .map((line) => {
          const match = /^([a-f0-9]{64})  (.+)$/.exec(line)
          assert(match, 'Export must contain complete source fingerprints')
          return [match[2], match[1]]
        })
    )
  const baseline = hashes(inputs.baseline.manifest)
  for (const [file, hash] of Object.entries(originalSources)) {
    assert.equal(
      baseline[file],
      hash,
      `Baseline production source differs from pinned main: ${file}`
    )
  }
  const loading = hashes(inputs.loading.manifest)
  const combined = hashes(inputs.combined.manifest)
  assert.deepEqual(
    loading,
    combined,
    'Loading and combined exports must come from the same source inputs'
  )
  for (const [file, hash] of Object.entries(loading))
    assert.equal(source.files[file], hash, `Candidate export is stale: ${file}`)
  for (const file of ['Cargo.lock', 'rust-toolchain.toml', '.tool-versions']) {
    assert(
      baseline[file] && loading[file],
      `Missing baseline toolchain/dependency fingerprint: ${file}`
    )
    assert.equal(
      baseline[file],
      loading[file],
      `Baseline and candidate must use matching ${file}`
    )
  }
}

function sourceState() {
  const files = git([
    'ls-files',
    '--cached',
    '--others',
    '--exclude-standard',
    '--',
    'Cargo.toml',
    'Cargo.lock',
    '.cargo',
    'rust-toolchain.toml',
    '.tool-versions',
    'crates/trusted-server-core',
    'crates/trusted-server-adapter-fastly',
    'crates/trusted-server-openrtb',
    'crates/trusted-server-js',
    'crates/trusted-server-integration-tests',
    'scripts/prepare-client-cwv-baseline.mjs',
    'scripts/prepare-client-cwv-baseline.test.mjs',
    'scripts/test-client-cwv-acceptance.mjs',
    'scripts/test-client-cwv-acceptance.test.mjs',
    'scripts/test-publisher-registration.sh',
    'scripts/test-publisher-registration-real-gpt*',
  ])
    .split('\n')
    .filter(Boolean)
  return {
    revision: git(['rev-parse', 'HEAD']),
    status: git(['status', '--short']),
    files: Object.fromEntries(
      files.map((file) => [
        file,
        existsSync(resolve(root, file))
          ? sha256(readFileSync(resolve(root, file)))
          : null,
      ])
    ),
  }
}

function unitTests() {
  return ['cwv', 'publisher-registration']
    .flatMap((directory) =>
      readdirSync(resolve(root, browser, directory))
        .filter((name) => name.endsWith('.test.cjs'))
        .map((name) => `${browser}/${directory}/${name}`)
    )
    .concat([
      'scripts/prepare-client-cwv-baseline.test.mjs',
      'scripts/test-client-cwv-acceptance.test.mjs',
      'scripts/test-publisher-registration-real-gpt.test.mjs',
    ])
}

export function runAcceptance(requestedOptions) {
  let options = requestedOptions
  mkdirSync(options.out, { recursive: true })
  const state = sourceState()
  const report = {
    schemaVersion: 1,
    status: 'running',
    startedAt: new Date().toISOString(),
    source: state,
    conditions,
    steps: [],
  }
  const save = () =>
    writeFileSync(
      resolve(options.out, 'acceptance.json'),
      JSON.stringify(report, null, 2) + '\n'
    )
  save()
  const step = (name, command, args, verify) => {
    const log = resolve(options.out, `${name}.log`)
    const record = {
      name,
      command: [command, ...args],
      status: 'running',
      log,
      startedAt: new Date().toISOString(),
    }
    report.steps.push(record)
    save()
    const fd = openSync(log, 'w')
    try {
      const result = spawnSync(command, args, {
        cwd: root,
        stdio: ['ignore', fd, fd],
      })
      record.exitCode = result.status
      assert.equal(
        result.status,
        0,
        `${name} failed; see ${log}: ${result.error ?? ''}`
      )
      verify?.()
      record.status = 'passed'
      console.log(`PASS ${name}`)
    } catch (error) {
      record.status = 'failed'
      record.error = error.message
      console.error(record.error)
    } finally {
      closeSync(fd)
      record.finishedAt = new Date().toISOString()
      save()
    }
  }
  try {
    const snapshot = snapshotInputs(options)
    options = snapshot.options
    report.requestedInputs = requestedOptions
    report.snapshot = snapshot
    // Record exact exports and immutable bundle inputs before starting any browser.
    report.inputs = {}
    for (const name of ['baseline', 'loading', 'combined']) {
      const manifest = resolve(options[name], 'manifest.json')
      report.inputs[name] = {
        path: manifest,
        sha256: sha256(readFileSync(manifest)),
        manifest: readJson(manifest),
      }
    }
    validateExportSources(report.inputs, state, trustedBaselineSources())
    const wasm = readFileSync(options.wasm)
    report.inputs.wasm = { path: options.wasm, sha256: sha256(wasm) }
    for (const name of readdirSync(options.dist).filter((file) =>
      /^tsjs-.*\.js$/.test(file)
    )) {
      assert(
        wasm.includes(readFileSync(resolve(options.dist, name))),
        `Selected WASM does not embed selected module ${name}`
      )
    }
    report.inputs.prebid = {
      path: options.prebid,
      manifest: readJson(resolve(options.prebid, 'manifest.json')),
    }
    report.inputs.dist = Object.fromEntries(
      readdirSync(options.dist)
        .filter((name) => /^tsjs-.*\.js$/.test(name))
        .map((name) => [
          name,
          sha256(readFileSync(resolve(options.dist, name))),
        ])
    )
    save()
    step('units', process.execPath, ['--test', ...unitTests()])
    step('offline-registration', process.execPath, [
      `${browser}/publisher-registration/run.cjs`,
      '--implementation',
      'all',
      '--dist',
      options.dist,
      '--prebid',
      options.prebid,
      '--out',
      resolve(options.out, 'offline-registration'),
    ])
    step('legacy-automatic-controls', process.execPath, [
      `${browser}/publisher-registration/run.cjs`,
      '--baseline',
      '--implementation',
      'all',
      '--dist',
      options.dist,
      '--prebid',
      options.prebid,
      '--out',
      resolve(options.out, 'legacy-automatic-controls'),
    ])
    const framework = [
      '--dist',
      options.dist,
      '--prebid',
      options.prebid,
      '--wasm',
      options.wasm,
      '--out',
      resolve(options.out, 'framework'),
    ]
    if (options.image) framework.push('--image', options.image)
    step('framework', 'bash', [
      'scripts/test-publisher-registration.sh',
      ...framework,
    ])
    for (const condition of conditions) {
      step(
        condition.name,
        process.execPath,
        measurementArgs(options, condition),
        () =>
          validateMeasurement(
            readJson(resolve(options.out, condition.name, 'acceptance.json')),
            condition,
            options.runs
          )
      )
    }
    if (options.inventory) {
      report.inputs.inventory = {
        path: options.inventory,
        sha256: sha256(readFileSync(options.inventory)),
      }
      const live = [
        'scripts/test-publisher-registration-real-gpt.mjs',
        '--inventory',
        options.inventory,
        '--dist',
        options.dist,
        '--prebid',
        options.prebid,
        '--wasm',
        options.wasm,
        '--out',
        resolve(options.out, 'real-gpt'),
      ]
      if (options.image) live.push('--image', options.image)
      for (const name of ['baseline-wasm', 'baseline-dist'])
        if (options[name]) live.push(`--${name}`, options[name])
      step('real-gpt', process.execPath, live, () => {
        const result = readJson(
          resolve(options.out, 'real-gpt', 'outcome.json')
        )
        assert.equal(result.status, 'passed')
        assert.equal(result.liveAcceptancePassed, true)
        assert.equal(result.originalFailureObserved, true)
        assert.equal(result.candidate.status, 'passed')
        assert.equal(result.baseline.status, 'passed')
        assert.equal(result.counts.failed, 0)
        assert.equal(result.counts.skipped, 0)
        assert(
          result.counts.passed > 0 &&
            result.counts.passed === result.counts.total
        )
      })
    } else {
      report.steps.push({
        name: 'real-gpt',
        status: 'blocked',
        reason:
          'Dedicated GAM inventory and test creatives were not supplied. No Google traffic attempted.',
      })
    }
    const final = sourceState()
    assert.deepEqual(
      final,
      state,
      'Relevant inputs changed during acceptance; results cannot establish the final state'
    )
    assert.deepEqual(
      treeHashes(snapshot.directory),
      snapshot.hashes,
      'Snapshotted artifacts changed during acceptance'
    )
  } catch (error) {
    report.steps.push({
      name: 'acceptance-inputs',
      status: 'failed',
      error: error.message,
    })
  } finally {
    report.status = conclusion(report.steps)
    report.finishedAt = new Date().toISOString()
    report.functionalGatesPassed = report.status === 'passed'
    report.requiresPerformanceReview = true
    save()
  }
  console.log(
    `Issue 854 functional acceptance: ${report.status}; timing results require performance review; report ${resolve(options.out, 'acceptance.json')}`
  )
  return report
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  const { values } = parseArgs({
    options: Object.fromEntries(
      [
        'baseline',
        'loading',
        'combined',
        'dist',
        'prebid',
        'wasm',
        'baseline-wasm',
        'baseline-dist',
        'inventory',
        'out',
        'image',
        'runs',
      ].map((name) => [name, { type: 'string' }])
    ),
  })
  for (const name of [
    'baseline',
    'loading',
    'combined',
    'dist',
    'prebid',
    'wasm',
    'out',
  ])
    assert(values[name], `--${name} is required`)
  const options = Object.fromEntries(
    Object.entries(values).map(([name, value]) => [
      name,
      ['runs', 'image'].includes(name) ? value : resolve(value),
    ])
  )
  options.runs = Number(options.runs ?? 5)
  assert(
    Number.isInteger(options.runs) && options.runs > 0,
    '--runs must be a positive integer'
  )
  process.exitCode = runAcceptance(options).status === 'passed' ? 0 : 1
}
