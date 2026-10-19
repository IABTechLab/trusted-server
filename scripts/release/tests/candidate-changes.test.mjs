import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

const script = fileURLToPath(new URL('./candidate-changes.sh', import.meta.url))

function fixture(t, initial = 'README.md') {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'candidate-changes-'))
  t.after(() => fs.rmSync(root, { recursive: true, force: true }))
  const env = {
    PATH: process.env.PATH,
    HOME: root,
    LC_ALL: 'C',
    GIT_CONFIG_NOSYSTEM: '1',
    GIT_CONFIG_GLOBAL: '/dev/null',
  }
  const git = (...args) =>
    execFileSync(
      'git',
      [
        '-c',
        'maintenance.auto=false',
        '-c',
        'gc.auto=0',
        '-c',
        'commit.gpgsign=false',
        '-c',
        'user.name=Example',
        '-c',
        'user.email=example@example.com',
        ...args,
      ],
      { cwd: root, env, encoding: 'utf8' }
    ).trim()
  git('init', '-q')
  fs.mkdirSync(path.dirname(path.join(root, initial)), { recursive: true })
  fs.writeFileSync(path.join(root, initial), 'Example\n')
  git('add', '.')
  git('commit', '-q', '-m', 'Initial fixture')
  const base = git('rev-parse', 'HEAD')
  return {
    change(name) {
      fs.mkdirSync(path.dirname(path.join(root, name)), { recursive: true })
      fs.writeFileSync(path.join(root, name), 'Changed example\n')
      git('add', '.')
      git('commit', '-q', '-m', 'Change fixture')
      return `${base}...${git('rev-parse', 'HEAD')}`
    },
    rename(from, to) {
      fs.mkdirSync(path.dirname(path.join(root, to)), { recursive: true })
      fs.renameSync(path.join(root, from), path.join(root, to))
      git('add', '-A')
      git('commit', '-q', '-m', 'Move fixture')
      return `${base}...${git('rev-parse', 'HEAD')}`
    },
    selected(range) {
      return execFileSync('bash', [script, range], {
        cwd: root,
        env,
        encoding: 'utf8',
        stdio: ['ignore', 'pipe', 'pipe'],
      }).trim()
    },
    base,
  }
}

for (const name of [
  'Cargo.toml',
  '.cargo/config.toml',
  'edgezero.toml',
  'crates/trusted-server-core/src/lib.rs',
  'crates/trusted-server-openrtb/Cargo.toml',
  'crates/trusted-server-cli/src/run.rs',
  'unknown-build-input.json',
  'crates/trusted-server-core/src/odd\n"name.rs',
]) {
  test(`selects candidate jobs for ${JSON.stringify(name)}`, (t) => {
    const repo = fixture(t)
    assert.equal(repo.selected(repo.change(name)), 'true')
  })
}

test('skips known documentation-only changes and an empty diff', (t) => {
  const repo = fixture(t)
  assert.equal(repo.selected(`${repo.base}..${repo.base}`), 'false')
  assert.equal(repo.selected(repo.change('docs/guide/example.md')), 'false')
})

test('selects candidate jobs when Git cannot resolve the comparison', (t) => {
  const repo = fixture(t)
  assert.equal(
    repo.selected('missing-example-base...missing-example-head'),
    'true'
  )
  assert.equal(repo.selected(`${'a'.repeat(40)}...${'b'.repeat(40)}`), 'true')
})

test('selects candidate jobs when source is renamed into documentation', (t) => {
  const source = 'crates/trusted-server-core/src/example.rs'
  const repo = fixture(t, source)
  assert.equal(
    repo.selected(repo.rename(source, 'docs/retired-source.md')),
    'true'
  )
})
