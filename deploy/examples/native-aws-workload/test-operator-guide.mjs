import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'

for (const name of ['DEPLOYMENT.md', 'bootstrap/README.md']) {
  const guide = fs.readFileSync(new URL(name, import.meta.url), 'utf8')

  test(`${name}: shell examples parse without executing deployment commands`, () => {
    const blocks = [
      ...guide.matchAll(/^([ \t]*)```(?:bash|sh)\n([\s\S]*?)^\1```/gm),
    ]
    assert.ok(blocks.length > 0)
    for (const [index, block] of blocks.entries()) {
      const input = block[2]
        .split('\n')
        .map((line) =>
          line.startsWith(block[1]) ? line.slice(block[1].length) : line
        )
        .join('\n')
      const result = spawnSync('bash', ['-n'], {
        input,
        encoding: 'utf8',
      })
      assert.equal(
        result.status,
        0,
        `shell block ${index + 1}: ${result.stderr}`
      )
    }
  })

  test(`${name}: inline Node examples parse without reading private inputs`, () => {
    const blocks = [...guide.matchAll(/<<'JS'\n([\s\S]*?)\nJS/g)]
    assert.ok(blocks.length > 0)
    for (const [index, block] of blocks.entries()) {
      const result = spawnSync(
        process.execPath,
        ['--check', '--input-type=module'],
        {
          input: block[1],
          encoding: 'utf8',
        }
      )
      assert.equal(
        result.status,
        0,
        `Node block ${index + 1}: ${result.stderr}`
      )
    }
  })
}

test('walkthrough scripts preserve backend, secret-version and envelope handoffs', () => {
  const guide = fs.readFileSync(
    new URL('DEPLOYMENT.md', import.meta.url),
    'utf8'
  )
  const snippets = [...guide.matchAll(/<<'JS'\n([\s\S]*?)\nJS/g)].map(
    (match) => match[1]
  )
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'ts-operator-guide-'))
  const run = (marker, args) => {
    const script = snippets.find((snippet) => snippet.includes(marker))
    assert.ok(script, `documented script containing ${marker} must exist`)
    return spawnSync(process.execPath, ['--input-type=module', '-', ...args], {
      cwd: temporary,
      input: script,
      encoding: 'utf8',
    })
  }
  const references = [
    'handler_password',
    'publisher_proxy_secret',
    'ec_passphrase',
    'browser_forwarder_secret',
  ]
  try {
    const application_secret_arns = Object.fromEntries(
      references.map((name) => [
        name,
        `arn:aws:secretsmanager:eu-west-2:000000000000:secret:example-${name}-AbCdEf`,
      ])
    )
    const agent_secret_arn =
      'arn:aws:secretsmanager:eu-west-2:000000000000:secret:example-agent-AbCdEf'
    const backend_configs = Object.fromEntries(
      ['bootstrap', 'retained', 'workload'].map((root) => [
        root,
        {
          bucket: 'example-state',
          key: `example/${root}.tfstate`,
          region: 'eu-west-2',
          encrypt: true,
          use_lockfile: true,
        },
      ])
    )
    const output = Object.fromEntries(
      Object.entries({
        application_secret_arns,
        agent_secret_arn,
        backend_configs,
        route53_zone_id: 'Z0000000000000',
        regional_certificate_arn:
          'arn:aws:acm:eu-west-2:000000000000:certificate/00000000-0000-0000-0000-000000000000',
      }).map(([key, value]) => [key, { value }])
    )
    const outputFile = path.join(temporary, 'bootstrap-output.json')
    fs.writeFileSync(outputFile, JSON.stringify(output))
    assert.equal(run('bootstrap-workload.tfvars.json', [temporary]).status, 0)
    for (const root of Object.keys(backend_configs)) {
      const config = fs.readFileSync(
        path.join(temporary, `${root}.backend.hcl`),
        'utf8'
      )
      assert.ok(config.includes(`key = "example/${root}.tfstate"`))
      assert.ok(config.includes('use_lockfile = true'))
    }
    assert.equal(run('randomBytes', [temporary]).status, 0)
    assert.notEqual(
      run('randomBytes', [temporary]).status,
      0,
      'first-installation generation must not overwrite stable secret files'
    )
    const values = [...references, 'agent_token'].map((name) =>
      fs.readFileSync(path.join(temporary, `${name}.txt`), 'utf8')
    )
    assert.equal(new Set(values).size, 5)
    for (const value of values) assert.match(value, /^[A-Za-z0-9_-]{43}$/)
    for (const [index, reference] of [...references, 'agent_token'].entries()) {
      const arn =
        reference === 'agent_token'
          ? agent_secret_arn
          : application_secret_arns[reference]
      assert.equal(
        run("reference === 'agent_token'", [outputFile, reference]).stdout,
        arn
      )
      fs.writeFileSync(
        path.join(temporary, `${reference}.version.json`),
        JSON.stringify({ arn, version_id: String(index + 1).repeat(32) })
      )
    }
    assert.equal(run('const application_secrets', [temporary]).status, 0)
    const versions = JSON.parse(
      fs.readFileSync(path.join(temporary, 'secrets.tfvars.json'), 'utf8')
    )
    assert.deepEqual(Object.keys(versions.application_secrets), references)
    assert.equal(versions.agent_token.arn, agent_secret_arn)
    assert.equal(
      versions.application_secrets.browser_forwarder_secret.version_id,
      '4'.repeat(32)
    )
    assert.equal(
      fs.readFileSync(path.join(temporary, 'browser-auth.txt'), 'utf8'),
      `reviewer:${values[0]}`
    )
    assert.equal(
      fs.readFileSync(
        path.join(temporary, 'browser-forwarder-token.txt'),
        'utf8'
      ),
      values[3]
    )
    assert.equal(
      fs.statSync(path.join(temporary, 'secrets.tfvars.json')).mode & 0o777,
      0o600
    )

    fs.mkdirSync(path.join(temporary, '.edgezero'))
    const envelope =
      '{"data":{"ratio":0.0},"sha256":"fictional-transport-only"}'
    fs.writeFileSync(
      path.join(temporary, '.edgezero/local-config-trusted_server_config.json'),
      JSON.stringify({ trusted_server_config: envelope })
    )
    const envelopeFile = path.join(temporary, 'appconfig-envelope.json')
    assert.equal(
      run('Missing configuration envelope', [envelopeFile]).status,
      0
    )
    assert.equal(
      fs.readFileSync(envelopeFile, 'utf8'),
      envelope,
      'envelope extraction must preserve inner bytes'
    )
  } finally {
    fs.rmSync(temporary, { recursive: true, force: true })
  }
})

test('walkthrough output references exist in the bootstrap contract', () => {
  const guide = fs.readFileSync(
    new URL('DEPLOYMENT.md', import.meta.url),
    'utf8'
  )
  const outputs = fs.readFileSync(
    new URL('bootstrap/outputs.tf', import.meta.url),
    'utf8'
  )
  const names = new Set(
    [...outputs.matchAll(/^output "([^"]+)"/gm)].map((match) => match[1])
  )
  for (const [, name] of guide.matchAll(/output\.([a-z_]+)\.value/g)) {
    assert.ok(names.has(name), `bootstrap output ${name} must exist`)
  }
})
