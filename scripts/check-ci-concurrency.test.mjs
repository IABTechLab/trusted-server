import assert from 'node:assert/strict'
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { spawnSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import test from 'node:test'

const repositoryRoot = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  '..'
)
const guardPath = path.join(repositoryRoot, 'scripts/check-ci-concurrency.sh')
const workflowPrefix = '$' + '{{ github.workflow }}'
const workflowGroup =
  workflowPrefix +
  '-' +
  '$' +
  '{{ github.event.pull_request.number || github.run_id }}'
const authorGroupSuffix =
  '$' +
  "{{ github.event_name == 'pull_request' && github.event.pull_request.user.type != 'Bot' && github.event.pull_request.user.login || github.run_id }}"

function authorGroup(job, matrix = '') {
  return [workflowPrefix, job, matrix, authorGroupSuffix]
    .filter(Boolean)
    .join('-')
}

function workflow(job, group, options = {}) {
  const lines = [
    'name: ' + job,
    'on:',
    '  pull_request:',
    'concurrency:',
    '  group: ' + workflowGroup,
    '  cancel-in-progress: true',
    'jobs:',
    '  ' + job + ':',
    '    runs-on: ubuntu-latest',
  ]
  if (options.timeout !== false) lines.push('    timeout-minutes: 5')
  if (options.matrix) {
    lines.push('    strategy:', '      matrix:', '        os: [ubuntu-latest]')
  }
  lines.push('    concurrency:', '      group: ' + group)
  if (options.queue !== false) lines.push('      queue: max')
  lines.push('    steps:', '      - run: echo test')
  return lines.join('\n') + '\n'
}

function fixtureRoot() {
  const root = mkdtempSync(path.join(os.tmpdir(), 'check-ci-concurrency-'))
  const workflowsPath = path.join(root, '.github/workflows')
  mkdirSync(workflowsPath, { recursive: true })
  writeFileSync(
    path.join(workflowsPath, 'test.yml'),
    workflow('test-rust', authorGroup('test-rust'))
  )
  writeFileSync(
    path.join(workflowsPath, 'format.yml'),
    workflow('format-rust', authorGroup('format-rust'))
  )
  writeFileSync(
    path.join(workflowsPath, 'push-only.yml'),
    [
      'name: Push only',
      'on:',
      '  push:',
      'jobs:',
      '  build:',
      '    runs-on: ubuntu-latest',
      '    steps:',
      '      - run: echo test',
      '',
    ].join('\n')
  )
  writeFileSync(
    path.join(workflowsPath, 'reusable.yml'),
    [
      'name: Reusable caller',
      'on:',
      '  pull_request:',
      'concurrency:',
      '  group: ' + workflowGroup,
      '  cancel-in-progress: true',
      'jobs:',
      '  call:',
      '    uses: example/reusable/.github/workflows/test.yml@main',
      '',
    ].join('\n')
  )
  return { root, workflowsPath }
}

function runGuard(root) {
  return spawnSync('bash', [guardPath, '--root', root], { encoding: 'utf8' })
}

function withFixture(callback) {
  const fixture = fixtureRoot()
  try {
    callback(fixture)
  } finally {
    rmSync(fixture.root, { recursive: true, force: true })
  }
}

function assertRejected(root, expectedMessage) {
  const result = runGuard(root)
  assert.equal(
    result.error,
    undefined,
    'should start the guard: ' + result.error
  )
  assert.notEqual(
    result.status,
    0,
    'should reject the policy:\n' + result.stdout + '\n' + result.stderr
  )
  assert.match(result.stdout + result.stderr, expectedMessage)
}

test('accepts per-PR cancellation and distinct per-author job queues', () => {
  withFixture(({ root }) => {
    const result = runGuard(root)
    assert.equal(
      result.error,
      undefined,
      'should start the guard: ' + result.error
    )
    assert.equal(result.status, 0, result.stdout + '\n' + result.stderr)
  })
})

test('rejects author groups that use the actor or queue bot PRs', () => {
  withFixture(({ root, workflowsPath }) => {
    const formatPath = path.join(workflowsPath, 'format.yml')
    const source = workflow('format-rust', authorGroup('format-rust'))
    writeFileSync(
      formatPath,
      source.replace('pull_request.user.login', 'github.actor')
    )
    assertRejected(root, /concurrency group/i)
    writeFileSync(formatPath, source.replace("user.type != 'Bot' && ", ''))
    assertRejected(root, /concurrency group/i)
  })
})

test('rejects a per-author group without the non-PR run-ID fallback', () => {
  withFixture(({ root, workflowsPath }) => {
    const formatPath = path.join(workflowsPath, 'format.yml')
    const group = authorGroup('format-rust').replace(' || github.run_id', '')
    writeFileSync(formatPath, workflow('format-rust', group))
    assertRejected(root, /concurrency group/i)
  })
})

test('rejects copied job groups and groups without the workflow prefix', () => {
  withFixture(({ root, workflowsPath }) => {
    const formatPath = path.join(workflowsPath, 'format.yml')
    writeFileSync(formatPath, workflow('format-rust', authorGroup('test-rust')))
    assertRejected(root, /concurrency group/i)
    writeFileSync(
      formatPath,
      workflow(
        'format-rust',
        authorGroup('format-rust').replace(workflowPrefix + '-', '')
      )
    )
    assertRejected(root, /concurrency group/i)
  })
})

test('requires matrix jobs to separate their legs in the author queue', () => {
  withFixture(({ root, workflowsPath }) => {
    const formatPath = path.join(workflowsPath, 'format.yml')
    writeFileSync(
      formatPath,
      workflow(
        'format-rust',
        authorGroup('format-rust', '$' + '{{ matrix.os }}'),
        {
          matrix: true,
        }
      )
    )
    const passing = runGuard(root)
    assert.equal(
      passing.error,
      undefined,
      'should start the guard: ' + passing.error
    )
    assert.equal(passing.status, 0, passing.stdout + '\n' + passing.stderr)

    writeFileSync(
      formatPath,
      workflow('format-rust', authorGroup('format-rust'), { matrix: true })
    )
    assertRejected(root, /matrix job/i)
  })
})

test('requires every non-reusable PR job to set a timeout and queue max', () => {
  withFixture(({ root, workflowsPath }) => {
    const formatPath = path.join(workflowsPath, 'format.yml')
    writeFileSync(
      formatPath,
      workflow('format-rust', authorGroup('format-rust'), { timeout: false })
    )
    assertRejected(root, /timeout-minutes/)
    writeFileSync(
      formatPath,
      workflow('format-rust', authorGroup('format-rust'), { queue: false })
    )
    assertRejected(root, /queue: max/)
  })
})

test('requires the exact per-PR cancellation group on every PR workflow', () => {
  withFixture(({ root, workflowsPath }) => {
    const formatPath = path.join(workflowsPath, 'format.yml')
    const wrongGroup = workflow(
      'format-rust',
      authorGroup('format-rust')
    ).replace(
      'github.event.pull_request.number || github.run_id',
      'github.actor'
    )
    writeFileSync(formatPath, wrongGroup)
    assertRejected(root, /workflow concurrency group/)
  })
})
