#!/usr/bin/env node
// Fake AWS process for PBS CLI tests. Never contacts AWS.
const assert = require('node:assert/strict')
const {
  appendFileSync,
  readFileSync,
  statSync,
  writeFileSync,
} = require('node:fs')
const { join } = require('node:path')

const args = process.argv.slice(2)
const root = process.env.PBS_FAKE_ROOT
appendFileSync(join(root, 'calls'), `${JSON.stringify(args)}\n`)
assert(args.includes('--profile'))
assert.equal(args[args.indexOf('--profile') + 1], 'pbs-sandbox')
if (args.includes('configure')) {
  if (process.env.PBS_FAKE_HISTORY === 'unset') process.exit(1)
  console.log(
    process.env.PBS_FAKE_HISTORY === 'enabled' ? 'enabled' : 'disabled'
  )
  process.exit(0)
}
assert(args.includes('--region'))
assert.equal(args[args.indexOf('--region') + 1], 'us-east-1')
assert.equal(process.env.AWS_IGNORE_CONFIGURED_ENDPOINT_URLS, 'true')
assert(args.includes('--cli-input-json'))
const input = args[args.indexOf('--cli-input-json') + 1]
assert(input.startsWith('file://'))
const path = input.slice('file://'.length)
assert.equal(statSync(path).mode & 0o7777, 0o600)
appendFileSync(join(root, 'payload_paths'), `${path}\n`)
const request = JSON.parse(readFileSync(path, 'utf8'))
const arn =
  'arn:aws:secretsmanager:us-east-1:123456789012:secret:pbs/example-AbCdEf'
if (args.includes('get-caller-identity')) {
  console.log(
    JSON.stringify({ Account: process.env.PBS_FAKE_ACCOUNT ?? '123456789012' })
  )
} else if (args.includes('describe-secret')) {
  console.log(JSON.stringify({ ARN: arn }))
} else if (args.includes('put-secret-value')) {
  const failure = process.env.PBS_FAKE_FAILURE
  if (failure === 'collision' || failure === 'transport') {
    const error =
      failure === 'collision'
        ? 'ResourceExistsException'
        : 'Connection reset by peer'
    console.error(`${error}: ${request.SecretString}`)
    process.exit(1)
  }
  writeFileSync(join(root, 'captured_request.json'), JSON.stringify(request))
  if (failure === 'invalid-json') {
    console.log(`invalid response ${request.SecretString}`)
  } else if (failure === 'unverified') {
    console.log(
      JSON.stringify({
        ARN: arn,
        VersionId: 'unexpected',
        SecretString: request.SecretString,
      })
    )
  } else {
    console.log(
      JSON.stringify({ ARN: arn, VersionId: request.ClientRequestToken })
    )
  }
} else if (args.includes('describe-instance-status')) {
  console.log(JSON.stringify({ InstanceStatuses: [] }))
} else {
  process.exit(2)
}
