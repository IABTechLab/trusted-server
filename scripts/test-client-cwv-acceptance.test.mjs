import assert from 'node:assert/strict'
import test from 'node:test'
import {
  mkdtempSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { spawnSync } from 'node:child_process'
import {
  BASELINE_REVISION,
  conditions,
  conclusion,
  measurementArgs,
  snapshotInputs,
  validateExportSources,
  validateExportAssets,
  validateMeasurement,
} from './test-client-cwv-acceptance.mjs'

const required = [
  'units',
  'offline-registration',
  'legacy-automatic-controls',
  'framework',
  ...conditions.map(({ name }) => name),
  'real-gpt',
]
const passed = () => required.map((name) => ({ name, status: 'passed' }))

test('functional acceptance requires every offline, framework, measurement and real-GPT row', () => {
  assert.equal(conclusion(passed()), 'passed')
  assert.equal(conclusion([]), 'blocked')
  for (const omitted of required)
    assert.equal(
      conclusion(passed().filter(({ name }) => name !== omitted)),
      'blocked',
      omitted
    )
  assert.equal(
    conclusion([
      ...passed().filter(({ name }) => name !== 'real-gpt'),
      { name: 'real-gpt', status: 'blocked' },
    ]),
    'blocked'
  )
  assert.equal(
    conclusion([...passed(), { name: 'input-change', status: 'failed' }]),
    'failed'
  )
})

test('performance commands preserve source inputs and declare normal, warm, mobile and delayed conditions', () => {
  const options = {
    baseline: '/baseline',
    loading: '/loading',
    combined: '/combined',
    out: '/evidence',
    runs: 5,
  }
  assert(
    conditions.some(
      (condition) => condition.delay === 0 && condition.cache === 'cold'
    )
  )
  assert(
    conditions.some(
      (condition) => condition.delay === 0 && condition.cache === 'warm'
    )
  )
  assert(
    conditions.some((condition) => condition.cpu > 1 && condition.latency > 0)
  )
  assert(conditions.some((condition) => condition.delay === 750))
  for (const condition of conditions) {
    const args = measurementArgs(options, condition)
    for (const [flag, value] of [
      ['--baseline', '/baseline'],
      ['--loading', '/loading'],
      ['--combined', '/combined'],
      ['--runtime-delay-ms', String(condition.delay)],
      ['--cache', condition.cache],
    ]) {
      assert.equal(args[args.indexOf(flag) + 1], value)
    }
    assert.equal(args[args.indexOf('--out') + 1], `/evidence/${condition.name}`)
  }
})

test('a successful command without completed negative controls is insufficient evidence', () => {
  const condition = conditions[0]
  const report = {
    schemaVersion: 1,
    status: 'passed',
    scope: 'measurement-and-correctness',
    conditions: {
      runtimeDelayMs: 0,
      cpuRate: 1,
      cache: 'cold',
      network: {
        offline: false,
        latency: 0,
        downloadThroughput: -1,
        uploadThroughput: -1,
      },
    },
    samples: 64,
    negativeControls: {
      'runtime-deferred': { status: 'passed' },
      'reservation-removed': { status: 'passed' },
    },
  }
  validateMeasurement(report, condition, 5)
  for (const status of ['running', 'failed', 'blocked'])
    assert.throws(() =>
      validateMeasurement({ ...report, status }, condition, 5)
    )
  assert.throws(() =>
    validateMeasurement({ ...report, scope: 'regressions' }, condition, 5)
  )
  assert.throws(() =>
    validateMeasurement({ ...report, negativeControls: {} }, condition, 5)
  )
  assert.throws(() =>
    validateMeasurement({ ...report, samples: 63 }, condition, 5)
  )
  assert.throws(() =>
    validateMeasurement(
      { ...report, conditions: { ...report.conditions, cache: 'warm' } },
      condition,
      5
    )
  )
})

test('candidate exports must match current source and share baseline toolchain/dependency pins', () => {
  const files = Object.fromEntries(
    [
      'Cargo.lock',
      'rust-toolchain.toml',
      '.tool-versions',
      'crates/trusted-server-core/src/tsjs.rs',
      'crates/trusted-server-core/src/html_processor.rs',
      'crates/trusted-server-core/src/integrations/gpt_bootstrap.js',
      'crates/trusted-server-js/lib/package-lock.json',
      'crates/trusted-server-js/lib/src/core/index.ts',
      'crates/trusted-server-js/lib/src/integrations/gpt/publisher_registration.ts',
    ].map((file) => [file, 'a'.repeat(64)])
  )
  const originalSources = Object.fromEntries(
    Object.entries(files).filter(([file]) =>
      file.startsWith('crates/trusted-server-core/')
    )
  )
  const manifest = {
    sourceRevision: BASELINE_REVISION,
    sourceFingerprints: Object.entries(files)
      .map(([file, hash]) => `${hash}  ${file}`)
      .join('\n'),
  }
  const inputs = Object.fromEntries(
    ['baseline', 'loading', 'combined'].map((name) => [
      name,
      { manifest: { ...manifest } },
    ])
  )
  validateExportSources(inputs, { files }, originalSources)
  const jsSource =
    'crates/trusted-server-js/lib/src/integrations/gpt/publisher_registration.ts'
  const withoutJs = {
    ...manifest,
    sourceFingerprints: manifest.sourceFingerprints
      .split('\n')
      .filter((line) => !line.includes('crates/trusted-server-js/lib/src/'))
      .join('\n'),
  }
  assert.throws(
    () =>
      validateExportSources(
        {
          ...inputs,
          loading: { manifest: withoutJs },
          combined: { manifest: withoutJs },
        },
        { files },
        originalSources
      ),
    /stale or incomplete/
  )
  assert.throws(
    () =>
      validateExportSources(
        inputs,
        { files: { ...files, [jsSource]: 'b'.repeat(64) } },
        originalSources
      ),
    /stale/
  )
  assert.throws(
    () =>
      validateExportSources(
        inputs,
        {
          files: {
            ...files,
            'crates/trusted-server-js/lib/src/new.ts': 'b'.repeat(64),
          },
        },
        originalSources
      ),
    /stale or incomplete/
  )
  const deletedSource = 'crates/trusted-server-js/lib/src/deleted.ts'
  validateExportSources(
    inputs,
    { files: { ...files, [deletedSource]: null } },
    originalSources
  )
  assert.throws(
    () =>
      validateExportSources(
        inputs,
        {
          files: {
            ...files,
            'crates/trusted-server-core/src/tsjs.rs': 'b'.repeat(64),
          },
        },
        originalSources
      ),
    /stale/
  )
  inputs.baseline.manifest.sourceFingerprints =
    manifest.sourceFingerprints.replace('a'.repeat(64), 'b'.repeat(64))
  assert.throws(
    () => validateExportSources(inputs, { files }, originalSources),
    /matching Cargo.lock/
  )
  inputs.baseline.manifest.sourceFingerprints = manifest.sourceFingerprints
  inputs.baseline.manifest.sourceRevision = 'candidate-or-other-main'
  assert.throws(
    () => validateExportSources(inputs, { files }, originalSources),
    /pinned original-main revision/
  )
  inputs.baseline.manifest.sourceRevision = BASELINE_REVISION
  inputs.baseline.manifest.sourceFingerprints =
    manifest.sourceFingerprints.replace(
      `${'a'.repeat(64)}  crates/trusted-server-core/src/tsjs.rs`,
      `${'b'.repeat(64)}  crates/trusted-server-core/src/tsjs.rs`
    )
  assert.throws(
    () => validateExportSources(inputs, { files }, originalSources),
    /differs from pinned main/
  )
  assert.throws(
    () => validateExportSources(inputs, { files }),
    /Trusted original-main/
  )
})

test('exported runtime bytes must match the selected distribution, not only current source metadata', () => {
  const directory = mkdtempSync(join(tmpdir(), 'cwv-acceptance-assets-'))
  try {
    const dist = join(directory, 'dist')
    mkdirSync(dist)
    for (const id of ['core', 'creative', 'gpt', 'prebid'])
      writeFileSync(join(dist, `tsjs-${id}.js`), `// current ${id}\n`)
    const inputs = {}
    for (const name of ['loading', 'combined']) {
      const out = join(directory, name)
      mkdirSync(out)
      writeFileSync(
        join(out, 'runtime.js'),
        '// current core\n;\n// current creative\n;\n// current gpt\n'
      )
      writeFileSync(join(out, 'prebid.js'), '// current prebid\n')
      inputs[name] = {
        path: join(out, 'manifest.json'),
        manifest: {
          jsModules: { immediate: ['creative', 'gpt'], deferred: ['prebid'] },
          assets: {
            '/static/tsjs=tsjs-unified.min.js?v=fixture': 'runtime.js',
            '/static/tsjs=tsjs-prebid.min.js?v=fixture': 'prebid.js',
          },
        },
      }
    }
    validateExportAssets(inputs, dist)
    writeFileSync(
      join(directory, 'loading', 'runtime.js'),
      '// stale prebuilt runtime'
    )
    assert.throws(
      () => validateExportAssets(inputs, dist),
      /differs from selected distribution/
    )
    writeFileSync(
      join(directory, 'loading', 'runtime.js'),
      readFileSync(join(directory, 'combined', 'runtime.js'))
    )
    writeFileSync(
      join(dist, 'tsjs-prebid.js'),
      '// replacement deferred runtime'
    )
    assert.throws(
      () => validateExportAssets(inputs, dist),
      /differs from selected distribution/
    )
    writeFileSync(join(dist, 'tsjs-prebid.js'), '// current prebid\n')
    const moduleIds = inputs.loading.manifest.jsModules
    delete inputs.loading.manifest.jsModules
    assert.throws(
      () => validateExportAssets(inputs, dist),
      /lacks runtime module identities/
    )
    inputs.loading.manifest.jsModules = moduleIds
    delete inputs.loading.manifest.assets[
      '/static/tsjs=tsjs-prebid.min.js?v=fixture'
    ]
    assert.throws(
      () => validateExportAssets(inputs, dist),
      /missing runtime asset/
    )
  } finally {
    rmSync(directory, { recursive: true, force: true })
  }
})

test('source-capture failure replaces a previous passing acceptance journal', () => {
  const directory = mkdtempSync(join(tmpdir(), 'cwv-acceptance-journal-'))
  try {
    const file = join(directory, 'acceptance.json')
    writeFileSync(
      file,
      JSON.stringify({ status: 'passed', functionalGatesPassed: true })
    )
    const controller = new URL(
      './test-client-cwv-acceptance.mjs',
      import.meta.url
    ).href
    const result = spawnSync(
      process.execPath,
      [
        '--input-type=module',
        '-e',
        `import { runAcceptance } from ${JSON.stringify(controller)};
         const result = runAcceptance({ out: process.argv[1] });
         if (result.status !== 'failed') process.exitCode = 1;`,
        directory,
      ],
      {
        encoding: 'utf8',
        env: { ...process.env, PATH: join(directory, 'missing-tools') },
      }
    )
    assert.equal(result.status, 0, result.stderr)
    const report = JSON.parse(readFileSync(file, 'utf8'))
    assert.equal(report.status, 'failed')
    assert.equal(report.functionalGatesPassed, false)
    assert.equal(report.source, null)
    assert.equal(report.steps[0].name, 'acceptance-inputs')
    assert.equal(report.steps[0].status, 'failed')
  } finally {
    rmSync(directory, { recursive: true, force: true })
  }
})

test('every external input is frozen, including HTML, runtime, WASM stamps and inventory', () => {
  const directory = mkdtempSync(join(tmpdir(), 'cwv-acceptance-inputs-'))
  try {
    const options = { out: join(directory, 'out') }
    mkdirSync(options.out)
    for (const name of [
      'baseline',
      'loading',
      'combined',
      'dist',
      'prebid',
      'baseline-dist',
    ]) {
      options[name] = join(directory, name)
      mkdirSync(options[name])
      writeFileSync(join(options[name], 'asset.js'), `original ${name}`)
    }
    for (const name of ['wasm', 'baseline-wasm', 'inventory']) {
      options[name] = join(directory, name)
      writeFileSync(options[name], `original ${name}`)
    }
    writeFileSync(
      options.wasm + '.publisher-registration.json',
      'candidate stamp'
    )
    writeFileSync(
      options['baseline-wasm'] + '.publisher-registration-baseline.json',
      'baseline stamp'
    )
    const snapshot = snapshotInputs(options)
    writeFileSync(join(options.dist, 'asset.js'), 'replacement runtime')
    writeFileSync(options.wasm, 'replacement wasm')
    writeFileSync(options.inventory, 'replacement inventory')
    assert.equal(
      readFileSync(join(snapshot.options.dist, 'asset.js'), 'utf8'),
      'original dist'
    )
    assert.equal(readFileSync(snapshot.options.wasm, 'utf8'), 'original wasm')
    assert.equal(
      readFileSync(snapshot.options.inventory, 'utf8'),
      'original inventory'
    )
    assert.equal(
      readFileSync(
        snapshot.options.wasm + '.publisher-registration.json',
        'utf8'
      ),
      'candidate stamp'
    )
    assert.equal(
      readFileSync(
        snapshot.options['baseline-wasm'] +
          '.publisher-registration-baseline.json',
        'utf8'
      ),
      'baseline stamp'
    )
    assert(snapshot.hashes['baseline/asset.js'])
    assert(snapshot.hashes['combined/asset.js'])
  } finally {
    rmSync(directory, { recursive: true, force: true })
  }
})
