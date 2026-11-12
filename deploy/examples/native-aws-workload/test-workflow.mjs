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

test('both Terraform roots and provider locks are checked', () => {
  for (const root of [
    'deploy/examples/native-aws-workload',
    'deploy/examples/native-aws-workload/retained',
  ]) {
    assert.ok(
      workflow.includes(
        `terraform -chdir=${root} init -backend=false -input=false -lockfile=readonly`
      )
    )
    assert.ok(workflow.includes(`terraform -chdir=${root} validate`))
  }
  assert.match(workflow, /fmt -check -recursive/)
  assert.match(
    workflow,
    /git diff --exit-code -- deploy\/examples\/native-aws-workload\/\.terraform\.lock\.hcl deploy\/examples\/native-aws-workload\/retained\/\.terraform\.lock\.hcl/
  )
})

test('AWS envelope verification runs and lints in native Rust CI', () => {
  for (const name of ['test.yml', 'format.yml']) {
    const rustWorkflow = fs.readFileSync(
      new URL(`../../../.github/workflows/${name}`, import.meta.url),
      'utf8'
    )
    assert.match(
      rustWorkflow,
      /--test template_build_digest --test aws_example_config/
    )
  }
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
