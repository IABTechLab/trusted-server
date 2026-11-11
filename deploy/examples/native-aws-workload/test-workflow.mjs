import assert from 'node:assert/strict'
import fs from 'node:fs'
import test from 'node:test'

const workflow = fs.readFileSync(
  new URL(
    '../../../.github/workflows/native-aws-workload.yml',
    import.meta.url
  ),
  'utf8'
)

test('example checks have no cloud authority or deployment operation', () => {
  assert.match(workflow, /pull_request:/)
  assert.match(workflow, /branches: \[main\]/)
  assert.match(workflow, /permissions:\n  contents: read/)
  assert.doesNotMatch(
    workflow,
    /id-token:|secrets\.|environment:|configure-aws-credentials|pull_request_target:|workflow_run:/
  )
  assert.doesNotMatch(
    workflow,
    /terraform (?:apply|destroy)|run\.mjs|control-plane|bootstrap\/|deploy\/native-demo|docker (?:build|push|run)/
  )
  assert.match(workflow, /AWS_EC2_METADATA_DISABLED: ['"]true['"]/)
  assert.match(workflow, /init -backend=false -input=false -lockfile=readonly/)
  assert.match(
    workflow,
    /node deploy\/examples\/native-aws-workload\/test-draft\.mjs/
  )
  assert.match(
    workflow,
    /git diff --exit-code -- deploy\/examples\/native-aws-workload\/\.terraform\.lock\.hcl/
  )
})

test('only immutable actions and nonpersistent checkout credentials are used', () => {
  for (const match of workflow.matchAll(/\buses:\s*([^\s#]+)/g))
    assert.match(match[1], /^[A-Za-z0-9_.\/-]+@[0-9a-f]{40}$/)
  assert.match(workflow, /persist-credentials: false/)
  assert.match(
    workflow,
    /node-version: \$\{\{ steps\.versions\.outputs\.node \}\}/
  )
  assert.match(
    workflow,
    /terraform_version: \$\{\{ steps\.versions\.outputs\.terraform \}\}/
  )
  assert.match(workflow, /terraform_wrapper: false/)
})
