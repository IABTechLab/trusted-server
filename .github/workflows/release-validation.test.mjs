import assert from 'node:assert/strict'
import test from 'node:test'
import * as validation from './release-validation.mjs'

const sha = 'a'.repeat(40)
const policy = {
  repository: 'example-org/example-repo',
  repository_id: '99999',
  repository_owner_id: '99998',
  cohort: 'phase1-fastly-cli',
  identity: { event: 'push', caller_path: '.github/workflows/release.yml' },
}
const env = {
  SOURCE_SHA: sha,
  GITHUB_SHA: sha,
  GITHUB_REF: 'refs/tags/v1.2.3',
  GITHUB_EVENT_NAME: 'push',
  GITHUB_REPOSITORY: policy.repository,
  GITHUB_REPOSITORY_ID: '99999',
  GITHUB_REPOSITORY_OWNER_ID: '99998',
  GITHUB_RUN_ID: '123',
  GITHUB_RUN_ATTEMPT: '2',
  WORKFLOW_SHA: sha,
  WORKFLOW_REF:
    'example-org/example-repo/.github/workflows/release.yml@refs/tags/v1.2.3',
}

test('context requires same source, official identity, event, and strict full SemVer tag', async (t) => {
  assert.equal(typeof validation.validateContext, 'function')
  assert.equal(validation.validateContext(env, policy).source, sha)
  for (const change of [
    { SOURCE_SHA: 'b'.repeat(40) },
    { WORKFLOW_SHA: 'b'.repeat(40) },
    { GITHUB_EVENT_NAME: 'workflow_dispatch' },
    { GITHUB_REPOSITORY_ID: '99997' },
    { GITHUB_REF: 'refs/heads/main' },
    { GITHUB_REF: 'refs/tags/v01.2.3' },
    { GITHUB_REF: 'refs/tags/v1.2.3-01' },
    { GITHUB_REF: 'refs/tags/v1.2.3/' },
    { GITHUB_RUN_ATTEMPT: '0' },
  ]) {
    await t.test(`rejects ${JSON.stringify(change)}`, () =>
      assert.throws(() =>
        validation.validateContext(
          {
            ...env,
            ...change,
            ...(change.GITHUB_REF
              ? {
                  WORKFLOW_REF: `${policy.repository}/.github/workflows/release.yml@${change.GITHUB_REF}`,
                }
              : {}),
          },
          policy
        )
      )
    )
  }
  for (const tag of ['v1.2.3+example.01', 'v1.2.3-rc.1']) {
    await t.test(`parses ${tag} without eligibility authority`, () =>
      assert.equal(
        validation.validateContext(
          {
            ...env,
            GITHUB_REF: `refs/tags/${tag}`,
            WORKFLOW_REF: `example-org/example-repo/.github/workflows/release.yml@refs/tags/${tag}`,
          },
          policy
        ).tag,
        tag
      )
    )
  }
})

const context = () => validation.validateContext(env, policy)
const sourceApi =
  (change = {}) =>
  async (path) => {
    if (path.includes('/git/ref/tags/'))
      return {
        ref: env.GITHUB_REF,
        object: { type: 'commit', sha },
        ...change.tag,
      }
    if (path.endsWith('/git/ref/heads/main'))
      return {
        ref: 'refs/heads/main',
        object: { type: 'commit', sha: 'b'.repeat(40) },
        ...change.main,
      }
    if (path.includes('/compare/'))
      return { status: 'ahead', merge_base_commit: { sha }, ...change.compare }
    throw new Error('Unexpected fixture endpoint')
  }
test('remote lightweight tag and main ancestry provide observations without eligibility authority', async (t) => {
  assert.equal(typeof validation.admit, 'function')
  assert.deepEqual(await validation.admit(context(), sourceApi()), {
    source_eligibility: false,
    diagnostic: validation.governanceGap,
  })
  for (const change of [
    { tag: { object: { type: 'tag', sha } } },
    { tag: { object: { type: 'commit', sha: 'b'.repeat(40) } } },
    { compare: { status: 'behind' } },
    { compare: { merge_base_commit: { sha: 'b'.repeat(40) } } },
  ]) {
    await t.test(`rejects remote ${JSON.stringify(change)}`, async () =>
      assert.rejects(validation.admit(context(), sourceApi(change)))
    )
  }
  const rc = validation.validateContext(
    {
      ...env,
      GITHUB_REF: 'refs/tags/v1.2.3-rc.1',
      WORKFLOW_REF:
        'example-org/example-repo/.github/workflows/release.yml@refs/tags/v1.2.3-rc.1',
      CANDIDATE_ALLOWLIST: sha,
      LOCAL_REF: 'refs/heads/main',
    },
    policy
  )
  const rcApi = async (path) =>
    path.includes('/git/ref/tags/')
      ? { ref: rc.ref, object: { type: 'commit', sha } }
      : sourceApi()(path)
  assert.equal((await validation.admit(rc, rcApi)).source_eligibility, false)
})

const requiredNames = [
  'Release validation admission',
  'Release validation Fastly',
]
const run = () => ({
  id: 123,
  run_attempt: 2,
  head_sha: sha,
  head_branch: 'v1.2.3',
  event: 'push',
  path: '.github/workflows/release.yml',
  created_at: '2026-01-01T00:00:00Z',
  repository: { full_name: policy.repository, id: 99999, owner: { id: 99998 } },
})
const jobs = () => ({
  total_count: 2,
  jobs: requiredNames.map((name, index) => ({
    id: index + 1,
    name: `caller / ${name}`,
    run_id: 123,
    run_attempt: 2,
    head_sha: sha,
    status: 'completed',
    conclusion: 'success',
  })),
})
test('waiter binds exact gates to own run attempt and fails closed on every terminal outcome', async (t) => {
  assert.equal(typeof validation.inspectJobs, 'function')
  assert.equal(validation.inspectJobs(context(), jobs(), requiredNames), true)
  for (const change of [
    { run_id: 124 },
    { run_attempt: 1 },
    { head_sha: 'b'.repeat(40) },
    { conclusion: 'skipped' },
    { conclusion: 'cancelled' },
    { conclusion: 'timed_out' },
    { conclusion: 'failure' },
    { name: 'unrelated success' },
  ]) {
    await t.test(`gate rejects or waits for ${JSON.stringify(change)}`, () => {
      const body = jobs()
      Object.assign(body.jobs[0], change)
      if (change.name)
        assert.equal(
          validation.inspectJobs(context(), body, requiredNames),
          false
        )
      else
        assert.throws(() =>
          validation.inspectJobs(context(), body, requiredNames)
        )
    })
  }
  const duplicate = jobs()
  duplicate.jobs[1].name = duplicate.jobs[0].name
  assert.throws(
    () => validation.inspectJobs(context(), duplicate, requiredNames),
    /Duplicate/
  )
  assert.throws(
    () =>
      validation.inspectJobs(
        context(),
        { ...jobs(), total_count: 101 },
        requiredNames
      ),
    /truncated/
  )
  const pending = jobs()
  pending.jobs[0].status = 'queued'
  pending.jobs[0].conclusion = null
  assert.equal(validation.inspectJobs(context(), pending, requiredNames), false)
})
test('waiter deadline includes queue time and successful gates never approve governance', async (t) => {
  assert.equal(typeof validation.wait, 'function')
  const api = async (path) =>
    path.endsWith('/jobs?per_page=100')
      ? jobs()
      : path.includes('/actions/runs/')
        ? run()
        : sourceApi()(path)
  const options = {
    api,
    requiredNames,
    now: () => Date.parse('2026-01-01T00:01:00Z'),
    monotonic: () => 100,
    sleep: async () => {},
  }
  await assert.rejects(
    validation.wait(context(), options),
    /missing independently approved governance/
  )
  await assert.rejects(
    validation.wait(context(), {
      ...options,
      now: () => Date.parse('2026-01-01T01:30:00Z'),
    }),
    /90 minute/
  )
  for (const change of [
    { run_attempt: 1 },
    { head_sha: 'b'.repeat(40) },
    { event: 'workflow_dispatch' },
    { head_branch: 'main' },
    {
      repository: {
        full_name: policy.repository,
        id: 99997,
        owner: { id: 99998 },
      },
    },
  ]) {
    await t.test(`rejects run ${JSON.stringify(change)}`, async () =>
      assert.rejects(
        validation.wait(context(), {
          ...options,
          api: async (path) =>
            path.includes('/actions/runs/') && !path.includes('/jobs?')
              ? { ...run(), ...change }
              : api(path),
        }),
        change.repository
          ? /Run repository identity mismatch/
          : /Run must match/
      )
    )
  }
  let elapsed = 0
  const queued = jobs()
  queued.jobs[0].status = 'queued'
  queued.jobs[0].conclusion = null
  await assert.rejects(
    validation.wait(context(), {
      ...options,
      api: async (path) =>
        path.endsWith('/jobs?per_page=100') ? queued : api(path),
      now: () => Date.parse('2026-01-01T01:29:59Z'),
      monotonic: () => elapsed,
      sleep: async (ms) => {
        elapsed += ms
      },
    }),
    /90 minute/
  )
})

test('transport fixes origin, blocks redirects and pagination, bounds body, and keeps credentials out of errors', async () => {
  assert.equal(typeof validation.createApi, 'function')
  let seen
  const api = validation.createApi('example-token', async (url, options) => {
    seen = { url, options }
    return new Response('{"value":1}')
  })
  assert.deepEqual(
    await api('/repos/example-org/example-repo/git/ref/heads/main'),
    { value: 1 }
  )
  assert.equal(
    seen.url,
    `${validation.apiOrigin}/repos/example-org/example-repo/git/ref/heads/main`
  )
  assert.equal(seen.options.redirect, 'error')
  assert.equal(seen.options.headers.Authorization, 'Bearer example-token')
  assert.ok(seen.options.signal instanceof AbortSignal)
  assert.throws(() => validation.createApi(''))
  await assert.rejects(api('https://example.com/'), /path/)
  for (const response of [
    new Response('{}', { status: 302 }),
    new Response('{}', {
      headers: { link: '<https://example.com/>; rel="next"' },
    }),
    new Response('x'.repeat(2 * 1024 * 1024 + 1)),
    new Response('{}', { headers: { 'content-length': '9999999' } }),
  ]) {
    await assert.rejects(
      validation.createApi(
        'example-token',
        async () => response
      )('/repos/example-org/example-repo/git/ref/heads/main')
    )
  }
  await assert.rejects(
    validation.createApi('example-token', async () => {
      throw new Error('example-token')
    })('/repos/example-org/example-repo/git/ref/heads/main'),
    (error) => !error.message.includes('example-token')
  )
})

test('tag byte length matches the consumer maximum', () => {
  const tag = `v1.2.3+${'a'.repeat(122)}`
  assert.throws(
    () =>
      validation.validateContext(
        {
          ...env,
          GITHUB_REF: `refs/tags/${tag}`,
          WORKFLOW_REF: `${policy.repository}/.github/workflows/release.yml@refs/tags/${tag}`,
        },
        policy
      ),
    /128/
  )
})

test('attempt endpoint supports documented jobs without a job attempt field', async () => {
  const body = jobs()
  for (const job of body.jobs) delete job.run_attempt
  assert.equal(validation.inspectJobs(context(), body, requiredNames), true)
})

for (const conclusion of ['skipped', 'cancelled', 'timed_out']) {
  test(`${conclusion} gate tells operators to rerun all jobs`, () => {
    const body = jobs()
    body.jobs[0].conclusion = conclusion
    assert.throws(
      () => validation.inspectJobs(context(), body, requiredNames),
      /Rerun all jobs/
    )
  })
}

test('all waiter reads receive the remaining global deadline including source reads', async () => {
  let elapsed = 0
  const calls = []
  const api = async (path, remaining) => {
    calls.push({ path, remaining })
    assert.ok(remaining > 0 && remaining <= 5400000)
    elapsed += 1
    return path.endsWith('/jobs?per_page=100')
      ? jobs()
      : path.includes('/actions/runs/')
        ? run()
        : sourceApi()(path)
  }
  await assert.rejects(
    validation.wait(context(), {
      api,
      requiredNames,
      now: () => Date.parse('2026-01-01T01:29:59Z'),
      monotonic: () => elapsed,
    }),
    /missing independently approved governance/
  )
  assert.equal(calls.length, 6)
  assert.ok(calls.slice(1).every(({ remaining }) => remaining <= 1000))
})

test('API request abort is clipped to the remaining global budget', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] })
  let signal
  const api = validation.createApi('example-token', async (_url, options) => {
    signal = options.signal
    return new Promise((_resolve, reject) =>
      signal.addEventListener('abort', () => reject(new Error('aborted')), {
        once: true,
      })
    )
  })
  const pending = api('/repos/example-org/example-repo/git/ref/heads/main', 10)
  t.mock.timers.tick(10)
  assert.equal(signal.aborted, true)
  await assert.rejects(pending, /rerun|Rerun/)
})

test('run path accepts only exact bare caller or caller at the current tag', async () => {
  const api = async (path) =>
    path.endsWith('/jobs?per_page=100')
      ? jobs()
      : path.includes('/actions/runs/')
        ? { ...run(), path: `.github/workflows/release.yml@${env.GITHUB_REF}` }
        : sourceApi()(path)
  await assert.rejects(
    validation.wait(context(), {
      api,
      requiredNames,
      now: () => Date.parse('2026-01-01T00:01:00Z'),
    }),
    /missing independently approved governance/
  )
})

test('retry does not reset the queue inclusive deadline and explains fresh invocation requirement', async () => {
  const api = async () => ({ ...run(), run_started_at: '2026-01-01T02:00:00Z' })
  await assert.rejects(
    validation.wait(context(), {
      api,
      now: () => Date.parse('2026-01-01T02:00:00Z'),
    }),
    /fresh trusted invocation/
  )
})

for (const extra of [
  { CANDIDATE_ALLOWLIST: sha },
  { LOCAL_REF: 'refs/heads/main' },
]) {
  test(`unlisted RC remains ineligible despite ${Object.keys(extra)[0]}`, async () => {
    const tag = 'v1.2.3-rc.1'
    const current = validation.validateContext(
      {
        ...env,
        ...extra,
        GITHUB_REF: `refs/tags/${tag}`,
        WORKFLOW_REF: `${policy.repository}/.github/workflows/release.yml@refs/tags/${tag}`,
      },
      policy
    )
    const api = async (path) =>
      path.includes('/git/ref/tags/')
        ? { ref: current.ref, object: { type: 'commit', sha } }
        : sourceApi()(path)
    assert.equal(
      (await validation.admit(current, api)).source_eligibility,
      false
    )
  })
}

test('admission composes with bounded transport through the remote compare endpoint', async () => {
  const paths = []
  const api = validation.createApi('example-token', async (url) => {
    const path = new URL(url).pathname
    paths.push(path)
    return Response.json(await sourceApi()(path))
  })
  assert.deepEqual(await validation.admit(context(), api), {
    source_eligibility: false,
    diagnostic: validation.governanceGap,
  })
  assert.equal(paths.length, 3)
  assert.ok(paths[2].endsWith(`/compare/${sha}...${'b'.repeat(40)}`))
})
