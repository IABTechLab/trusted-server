import assert from 'node:assert/strict'
import {
  mkdtempSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
  copyFileSync,
  rmSync,
} from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { spawnSync } from 'node:child_process'
import test from 'node:test'

const root = resolve(import.meta.dirname, '..')

function fixture(t, version = '4.83.0', pin = 'wrangler 4.83.0\n') {
  const directory = mkdtempSync(join(tmpdir(), 'cloudflare-pin-test-'))
  t.after(() => rmSync(directory, { recursive: true, force: true }))
  const repository = join(directory, 'repository')
  const bin = join(directory, 'bin')
  mkdirSync(join(repository, 'scripts'), { recursive: true })
  mkdirSync(
    join(repository, 'crates/trusted-server-adapter-cloudflare/build'),
    { recursive: true }
  )
  mkdirSync(bin)
  for (const file of ['smoke-cloudflare.sh', 'smoke-common.sh']) {
    copyFileSync(join(root, 'scripts', file), join(repository, 'scripts', file))
  }
  writeFileSync(join(repository, '.tool-versions'), pin)
  writeFileSync(join(repository, 'edgezero.toml'), '')
  for (const file of ['build/index.js', 'wrangler.toml', 'wrangler.ci.toml']) {
    writeFileSync(
      join(repository, 'crates/trusted-server-adapter-cloudflare', file),
      ''
    )
  }
  // Avoid listeners/builds; retain the real CLI and KV handoff in the entry point.
  writeFileSync(
    join(repository, 'scripts/smoke-common.sh'),
    readFileSync(join(repository, 'scripts/smoke-common.sh'), 'utf8') +
      '\nsmoke_start_origin() { SMOKE_ORIGIN_PID=""; }\n'
  )
  const executable = (name, source) =>
    writeFileSync(join(bin, name), '#!/usr/bin/env bash\nset -eu\n' + source, {
      mode: 0o755,
    })
  for (const name of ['cargo', 'curl', 'jq', 'pgrep'])
    executable(name, 'exit 99\n')
  // smoke_require_python runs `python3 -c` for its 3.11 floor; pass only that probe.
  executable('python3', 'if [ "$1" = -c ]; then exit 0; fi\nexit 99\n')
  executable(
    'wrangler',
    `
candidate="$PWD"
while [ ! -f "$candidate/.tool-versions" ] && [ "$candidate" != / ]; do candidate=$(dirname "$candidate"); done
test -f "$candidate/.tool-versions"
if [ "$1" = --version ]; then
  ${version === 'fails' ? 'exit 7' : `echo '${version}'`}
  exit 0
fi
candidate="$PWD"
while [ ! -f "$candidate/.tool-versions" ] && [ "$candidate" != / ]; do candidate=$(dirname "$candidate"); done
test -f "$candidate/.tool-versions"
test "$(awk '$1 == "wrangler" {print $2}' "$candidate/.tool-versions")" = 4.83.0
test "$(awk '$1 == "nodejs" {print $2}' "$candidate/.tool-versions")" = 24.20.0
if [ "$1" = kv ]; then echo reached-pinned-kv >&2; exit 42; fi
`
  )
  executable('ts', 'if [ "$1 $2" = "config push" ]; then wrangler check; fi\n')
  return () =>
    spawnSync('bash', [join(repository, 'scripts/smoke-cloudflare.sh')], {
      cwd: directory,
      encoding: 'utf8',
      env: {
        ...process.env,
        PATH: `${bin}:${process.env.PATH}`,
        TS_BIN: join(bin, 'ts'),
        // The smoke builds the Worker unless it is handed a prebuilt bundle.
        CLOUDFLARE_BUILD_PATH: join(
          repository,
          'crates/trusted-server-adapter-cloudflare/build'
        ),
        TMPDIR: directory,
      },
    })
}

for (const [name, version, pin, diagnostic] of [
  ['missing pin', '4.83.0', '', 'missing Wrangler pin'],
  ['wrong version', '4.82.0', 'wrangler 4.83.0\n', 'found 4.82.0'],
  ['malformed version', 'unknown', 'wrangler 4.83.0\n', 'found unknown'],
  [
    'failed executable',
    'fails',
    'wrangler 4.83.0\n',
    'could not run: wrangler --version',
  ],
]) {
  test(name, (t) => {
    const result = fixture(t, version, pin)()
    assert.equal(result.status, 1)
    assert.ok(result.stderr.includes(diagnostic), result.stderr)
  })
}

test('external invocation preserves tool selection through CLI and KV working directories', (t) => {
  const run = fixture(t, '4.83.0', 'wrangler 4.83.0\nnodejs 24.20.0\n')
  const result = run()
  assert.equal(result.status, 42, result.stderr)
  assert.match(result.stderr, /reached-pinned-kv/)
})

test('every CI Wrangler install reads the workspace pin after changing directory', (t) => {
  const directory = mkdtempSync(join(tmpdir(), 'cloudflare-ci-pin-test-'))
  t.after(() => rmSync(directory, { recursive: true, force: true }))
  const bin = join(directory, 'bin')
  mkdirSync(bin)
  writeFileSync(
    join(bin, 'npm'),
    '#!/usr/bin/env bash\nprintf "%s\\n" "$*"\n',
    { mode: 0o755 }
  )
  let installations = 0
  for (const workflow of [
    'integration-tests.yml',
    'worker-auction-deadlines.yml',
  ]) {
    const source = readFileSync(
      join(root, '.github/workflows', workflow),
      'utf8'
    )
    const blocks =
      source.match(
        / *wrangler_version=\$\(awk[^\n]+\n *test -n "\$wrangler_version"\n *npm install --global "wrangler@\$wrangler_version"/g
      ) ?? []
    for (const block of blocks) {
      installations++
      for (const pin of ['4.83.0', '4.84.0', '']) {
        writeFileSync(
          join(directory, '.tool-versions'),
          pin ? `wrangler ${pin}\n` : ''
        )
        const result = spawnSync('bash', ['-eu', '-c', block], {
          cwd: bin,
          encoding: 'utf8',
          env: {
            ...process.env,
            PATH: `${bin}:${process.env.PATH}`,
            GITHUB_WORKSPACE: directory,
          },
        })
        assert.equal(result.status, pin ? 0 : 1, result.stderr)
        assert.equal(
          result.stdout.trim(),
          pin ? `install --global wrangler@${pin}` : ''
        )
      }
    }
  }
  assert.equal(installations, 3)
})
