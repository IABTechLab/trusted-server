import assert from 'node:assert/strict'
import fs from 'node:fs'
import test from 'node:test'

const source = ['main.tf', 'outputs.tf', 'terraform.tf', 'variables.tf']
  .map((name) => fs.readFileSync(new URL(name, import.meta.url), 'utf8'))
  .join('\n')

test('bootstrap owns containers, not secret values, IAM policies or issuance waiters', () => {
  assert.doesNotMatch(
    source,
    /aws_secretsmanager_secret_version|secret_string|secret_binary|aws_iam_|aws_acm_certificate_validation|terraform_remote_state/
  )
  assert.equal(
    [...source.matchAll(/^resource "aws_route53_record" /gm)].length,
    1,
    'same-account regional and viewer certificates share one validation CNAME'
  )
  assert.match(source, /backend "local" \{\}/)
  assert.match(source, /alias\s*= "us_east_1"/)
  assert.match(source, /region\s*= "us-east-1"/)
  assert.equal(
    [...source.matchAll(/allowed_account_ids\s*= \[var\.aws_account_id\]/g)]
      .length,
    2
  )
})

test('mock checks are plan-only and the private backend override is ignored', () => {
  const fixture = fs.readFileSync(
    new URL('tests/bootstrap.tftest.hcl', import.meta.url),
    'utf8'
  )
  assert.doesNotMatch(fixture, /command\s*=\s*apply/)
  assert.equal([...fixture.matchAll(/command\s*=\s*plan/g)].length, 2)
  assert.match(
    fs.readFileSync(new URL('.gitignore', import.meta.url), 'utf8'),
    /^backend_override\.tf$/m
  )
})
