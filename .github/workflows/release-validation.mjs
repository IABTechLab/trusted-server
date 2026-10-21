import { readFile } from 'node:fs/promises'
import { pathToFileURL } from 'node:url'

const shaPattern = /^[0-9a-f]{40}$/
const numericIdentifier = '(?:0|[1-9][0-9]*)'
const prereleaseIdentifier = `(?:${numericIdentifier}|[0-9]*[A-Za-z-][0-9A-Za-z-]*)`
const tagPattern = new RegExp(
  `^v${numericIdentifier}\\.${numericIdentifier}\\.${numericIdentifier}(?:-${prereleaseIdentifier}(?:\\.${prereleaseIdentifier})*)?(?:\\+[0-9A-Za-z-]+(?:\\.[0-9A-Za-z-]+)*)?$`
)
const requireValue = (condition, message) => {
  if (!condition) throw new Error(message)
}

export function validateContext(env, policy) {
  const source = env.SOURCE_SHA
  requireValue(
    shaPattern.test(source ?? '') &&
      source === env.GITHUB_SHA &&
      source === env.WORKFLOW_SHA,
    'Source and workflow must be the same full commit'
  )
  requireValue(
    policy.cohort === 'phase1-fastly-cli' &&
      env.GITHUB_REPOSITORY === policy.repository &&
      env.GITHUB_REPOSITORY_ID === policy.repository_id &&
      env.GITHUB_REPOSITORY_OWNER_ID === policy.repository_owner_id,
    'Official repository identity required'
  )
  requireValue(
    env.GITHUB_EVENT_NAME === 'push' && policy.identity.event === 'push',
    'Tag push event required'
  )
  const ref = env.GITHUB_REF
  requireValue(
    typeof ref === 'string' &&
      ref.startsWith('refs/tags/') &&
      tagPattern.test(ref.slice(10)),
    'Strict SemVer full tag ref required'
  )
  requireValue(
    Buffer.byteLength(ref.slice(10), 'utf8') <= 128,
    'Tag exceeds 128 byte maximum'
  )
  requireValue(
    env.WORKFLOW_REF ===
      `${policy.repository}/${policy.identity.caller_path}@${ref}` &&
      policy.identity.caller_path === '.github/workflows/release.yml',
    'Exact same-tag caller workflow required'
  )
  requireValue(
    /^[1-9][0-9]*$/.test(env.GITHUB_RUN_ID ?? '') &&
      /^[1-9][0-9]*$/.test(env.GITHUB_RUN_ATTEMPT ?? ''),
    'Current run and attempt required'
  )
  return {
    source,
    ref,
    tag: ref.slice(10),
    repository: policy.repository,
    repositoryId: policy.repository_id,
    ownerId: policy.repository_owner_id,
    runId: env.GITHUB_RUN_ID,
    attempt: env.GITHUB_RUN_ATTEMPT,
  }
}

export const governanceGap =
  'Production source eligibility unavailable: missing independently approved governance, ruleset, and bypass approval profile'

export async function verifySource(context, api) {
  const base = `/repos/${context.repository}`
  const tag = await api(
    `${base}/git/ref/tags/${encodeURIComponent(context.tag)}`
  )
  requireValue(
    tag.ref === context.ref &&
      tag.object?.type === 'commit' &&
      tag.object.sha === context.source,
    'Remote lightweight tag must target exact source commit'
  )
  const main = await api(`${base}/git/ref/heads/main`)
  requireValue(
    main.ref === 'refs/heads/main' &&
      main.object?.type === 'commit' &&
      shaPattern.test(main.object.sha ?? ''),
    'Remote main commit required'
  )
  const comparison = await api(
    `${base}/compare/${context.source}...${main.object.sha}`
  )
  requireValue(
    ['ahead', 'identical'].includes(comparison.status) &&
      comparison.merge_base_commit?.sha === context.source,
    'Source must be an ancestor of remote main'
  )
}

export async function admit(context, api) {
  await verifySource(context, api)
  // Ancestry observations and validation gates do not establish review authority.
  return { source_eligibility: false, diagnostic: governanceGap }
}

export const requiredJobNames = Object.freeze([
  'Release validation admission',
  'Release validation Fastly',
  'Release validation native and parity',
  'Release validation Cloudflare and Spin',
  'Release validation release tool Linux',
  'Release validation release tool macOS',
  'Release validation JavaScript',
  'Release validation docs',
])

export function inspectJobs(context, body, requiredNames = requiredJobNames) {
  requireValue(
    Number.isInteger(body.total_count) &&
      body.total_count <= 100 &&
      Array.isArray(body.jobs) &&
      body.jobs.length === body.total_count,
    'Jobs response is truncated or invalid'
  )
  const observed = new Map()
  for (const job of body.jobs) {
    const name = job.name?.split(' / ').at(-1)
    if (!requiredNames.includes(name)) continue
    requireValue(!observed.has(name), 'Duplicate required job')
    requireValue(
      String(job.run_id) === context.runId &&
        (job.run_attempt === undefined ||
          String(job.run_attempt) === context.attempt) &&
        job.head_sha === context.source,
      'Job must belong to current source, run, and attempt'
    )
    requireValue(
      [
        'queued',
        'in_progress',
        'waiting',
        'pending',
        'requested',
        'completed',
      ].includes(job.status),
      'Unknown required job status'
    )
    if (job.status === 'completed')
      requireValue(
        job.conclusion === 'success',
        'Required job does not succeed. Rerun all jobs within the current validation window; no previous-attempt results count'
      )
    else
      requireValue(
        job.conclusion === null,
        'Pending required job has a conclusion'
      )
    observed.set(name, job.status === 'completed')
  }
  return requiredNames.every((name) => observed.get(name) === true)
}

function validateRun(context, run) {
  requireValue(
    String(run.id) === context.runId &&
      String(run.run_attempt) === context.attempt &&
      run.head_sha === context.source &&
      run.head_branch === context.tag &&
      run.event === 'push' &&
      [
        '.github/workflows/release.yml',
        `.github/workflows/release.yml@${context.ref}`,
      ].includes(run.path),
    'Run must match current tag, source, caller, event, and attempt'
  )
  requireValue(
    run.repository?.full_name === context.repository &&
      String(run.repository.id) === context.repositoryId &&
      String(run.repository.owner?.id) === context.ownerId,
    'Run repository identity mismatch'
  )
  requireValue(
    Number.isFinite(Date.parse(run.created_at)),
    'Run creation timestamp required'
  )
}

const deadlineMessage =
  '90 minute validation deadline exceeded (includes queue time). Reruns cannot reset this deadline; a fresh trusted invocation is required'

export async function wait(
  context,
  {
    api,
    requiredNames = requiredJobNames,
    now = Date.now,
    monotonic = () => performance.now(),
    sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
  }
) {
  const start = monotonic()
  const wallStart = now()
  const base = `/repos/${context.repository}/actions/runs/${context.runId}/attempts/${context.attempt}`
  const run = await api(base, 90 * 60 * 1000)
  validateRun(context, run)
  const budget = Date.parse(run.created_at) + 90 * 60 * 1000 - wallStart
  const remaining = () => budget - (monotonic() - start)
  const boundedApi = (path) => {
    requireValue(remaining() > 0, deadlineMessage)
    return api(path, remaining())
  }
  requireValue(
    budget <= 90 * 60 * 1000,
    'Run creation timestamp is in the future'
  )
  while (true) {
    requireValue(remaining() > 0, deadlineMessage)
    const current = await boundedApi(base)
    validateRun(context, current)
    requireValue(
      current.created_at === run.created_at,
      'Run creation timestamp changed'
    )
    const body = await boundedApi(`${base}/jobs?per_page=100`)
    requireValue(remaining() > 0, deadlineMessage)
    if (inspectJobs(context, body, requiredNames)) {
      await verifySource(context, boundedApi)
      requireValue(remaining() > 0, deadlineMessage)
      throw new Error(governanceGap)
    }
    await sleep(Math.min(15000, remaining()))
  }
}

// The fixed official REST origin receives the explicit read-only token.
export const apiOrigin = 'https://api.github.com' // allow-domain: api.github.com

export function createApi(token, fetchImpl = fetch) {
  requireValue(
    typeof token === 'string' && token.length > 0 && !/[\r\n]/.test(token),
    'Explicit read token required'
  )
  return async (path, remainingBudget = 15000) => {
    requireValue(
      Number.isFinite(remainingBudget) && remainingBudget > 0,
      deadlineMessage
    )
    requireValue(
      typeof path === 'string' &&
        /^\/repos\/[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+\/(?:git\/ref\/heads\/main|git\/ref\/tags\/v[A-Za-z0-9.%+-]+|compare\/[a-f0-9]{40}\.\.\.[a-f0-9]{40}|actions\/runs\/[1-9][0-9]*\/attempts\/[1-9][0-9]*(?:\/jobs\?per_page=100)?)$/.test(
          path
        ),
      'Fixed repository API path required'
    )
    const controller = new AbortController()
    const timer = setTimeout(
      () => controller.abort(),
      Math.min(15000, remainingBudget)
    )
    let reader
    try {
      const response = await fetchImpl(`${apiOrigin}${path}`, {
        method: 'GET',
        redirect: 'error',
        signal: controller.signal,
        headers: {
          Authorization: `Bearer ${token}`,
          Accept: 'application/vnd.github+json',
          'X-GitHub-Api-Version': '2022-11-28',
        },
      })
      requireValue(
        response.ok && !response.redirected,
        'GitHub API request unsuccessful'
      )
      requireValue(
        !response.headers.get('link'),
        'Paginated GitHub API responses are unavailable'
      )
      const limit = 2 * 1024 * 1024
      const length = response.headers.get('content-length')
      requireValue(
        length === null || (/^[0-9]+$/.test(length) && Number(length) <= limit),
        'GitHub API body exceeds bound'
      )
      requireValue(response.body, 'GitHub API body required')
      reader = response.body.getReader()
      const chunks = []
      let size = 0
      while (true) {
        const { done, value } = await reader.read()
        if (done) break
        size += value.byteLength
        requireValue(size <= limit, 'GitHub API body exceeds bound')
        chunks.push(value)
      }
      return JSON.parse(Buffer.concat(chunks, size).toString('utf8'))
    } catch {
      // Network and response errors may contain credentials or untrusted content.
      throw new Error(
        'Bounded GitHub API read failed. Rerun all jobs within the current validation window; an expired window requires a fresh trusted invocation'
      )
    } finally {
      clearTimeout(timer)
      if (reader) await reader.cancel().catch(() => {})
    }
  }
}

// Importing this module for pure tests never invokes the CI entry point.
if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  try {
    requireValue(
      process.argv.length === 3 && ['admit', 'wait'].includes(process.argv[2]),
      'Expected admit or wait command'
    )
    const policy = JSON.parse(
      await readFile(
        new URL(
          '../../crates/trusted-server-cli/release-policy.json',
          import.meta.url
        ),
        'utf8'
      )
    )
    const context = validateContext(process.env, policy)
    const api = createApi(process.env.GH_TOKEN)
    if (process.argv[2] === 'admit') {
      const result = await admit(context, api)
      process.stdout.write(`${JSON.stringify(result)}\n`)
    } else {
      await wait(context, { api })
    }
  } catch (error) {
    process.stderr.write(`${error.message}\n`)
    process.exitCode = 1
  }
}
