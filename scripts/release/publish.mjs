// Phase 1 release publisher. The release job runs this file from the tagged
// commit; it never checks out or executes build inputs.
import * as fs from 'node:fs'
import { createHash } from 'node:crypto'
import { execFileSync as nodeExecFileSync } from 'node:child_process'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { isDeepStrictEqual } from 'node:util'

export const releaseAssets = [
  'trusted-server-fastly.wasm',
  'ts-aarch64-apple-darwin',
  'ts-x86_64-apple-darwin',
  'ts-x86_64-unknown-linux-gnu',
]

// Strict SemVer 2.0.0 with a `v` prefix. Phase 1 refuses build metadata
// (`+...`): its release-attestation subject URI form is unrehearsed.
const semverTag =
  /^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\.(?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?$/

/**
 * Reports whether `tag` is an accepted Phase 1 release tag.
 */
export const isReleaseTag = (tag) => semverTag.test(tag)

const notesTemplate = fileURLToPath(
  new URL('release-notes.md', import.meta.url)
)

const apiOrigin = 'https://api.github.com' // allow-domain: api.github.com
const uploadOrigin = 'https://uploads.github.com' // allow-domain: uploads.github.com

const hash = (bytes) => createHash('sha256').update(bytes).digest('hex')

/**
 * Renders the release body from the checked-in template.
 */
export function releaseNotes(repo, tag) {
  return fs
    .readFileSync(notesTemplate, 'utf8')
    .replaceAll('{{REPOSITORY}}', repo)
    .replaceAll('{{TAG}}', tag)
    .trimEnd()
}

/**
 * Checks a release attestation statement for this repository and source
 * commit, and returns its asset subjects.
 */
function checkStatement(statement, repo, source) {
  const predicate = statement?.predicate
  if (
    statement?._type !== 'https://in-toto.io/Statement/v1' ||
    statement.predicateType !== 'https://in-toto.io/attestation/release/v0.2' ||
    predicate?.repository !== repo ||
    typeof predicate.tag !== 'string' ||
    !predicate.tag ||
    !/^[1-9][0-9]*$/.test(predicate.databaseId) ||
    !Array.isArray(statement.subject)
  )
    throw Error('Malformed release attestation statement')
  const sources = statement.subject.filter(
    (subject) => subject.uri === `pkg:github/${repo}@${predicate.tag}`
  )
  const subjects = statement.subject.filter(
    (subject) => !sources.includes(subject)
  )
  if (
    sources.length !== 1 ||
    sources[0].digest?.sha1 !== source ||
    Object.keys(sources[0].digest).length !== 1 ||
    !subjects.length ||
    subjects.some(
      (subject) =>
        typeof subject.name !== 'string' ||
        !subject.name ||
        !/^[a-f0-9]{64}$/.test(subject.digest?.sha256) ||
        Object.keys(subject.digest).length !== 1
    ) ||
    new Set(subjects.map((subject) => subject.name)).size !== subjects.length
  )
    throw Error('Invalid release attestation subjects')
  return { statement, subjects }
}

/**
 * Verifies the signing handoff and its bundles, creates one draft, uploads,
 * publishes it as an immutable release and verifies the published result.
 *
 * Every dependency with an outside effect is injected so tests can drive the
 * real program.
 */
export async function publish({
  env,
  root = process.cwd(),
  fetch = globalThis.fetch,
  execFileSync = nodeExecFileSync,
  now = Date.now,
  sleep = (delay) => new Promise((resolve) => setTimeout(resolve, delay)),
  log = console.log,
  warn = console.warn,
}) {
  const {
    GH_REPO: repo,
    RELEASE_TAG: tag,
    GH_TOKEN: token,
    SOURCE_COMMIT: source,
  } = env
  const version = semverTag.exec(tag ?? '')
  if (!/^[\w-]+\/[\w.-]+$/.test(repo ?? '') || !version)
    throw Error('Invalid repository or tag')
  if (!/^[a-f0-9]{40}$/.test(source ?? '')) throw Error('Invalid source commit')
  if (env.RELEASE_PUBLICATION_ENABLED !== 'true')
    throw Error('Release publication is not enabled by maintainers')
  const prerelease = version[4] !== undefined
  const make_latest = prerelease ? 'false' : 'legacy'
  const path = (...parts) => join(root, ...parts)
  const regular = (file) => {
    const stat = fs.lstatSync(file)
    if (!stat.isFile() || stat.size === 0) throw Error(`Invalid file: ${file}`)
    return fs.readFileSync(file)
  }
  const exact = (dir, expected) => {
    if (
      JSON.stringify(fs.readdirSync(dir).sort()) !==
      JSON.stringify([...expected].sort())
    )
      throw Error(`Unexpected artifact contents: ${dir}`)
  }
  fs.mkdirSync(path('release-assets'))
  const files = []
  for (const name of releaseAssets) {
    const build = path('build', name)
    const signed = path('signed', name)
    const inputs = [name, `${name}.spdx.json`]
    const bundles = [
      `${name}.provenance.sigstore.json`,
      `${name}.sbom.sigstore.json`,
    ]
    exact(build, inputs)
    exact(signed, [...bundles, 'SHA256SUMS'])
    const receipt = [
      ...inputs.map((file) => [build, file]),
      ...bundles.map((file) => [signed, file]),
    ]
      .map(([dir, file]) => {
        const bytes = regular(join(dir, file))
        fs.writeFileSync(path('release-assets', file), bytes)
        files.push(file)
        return `${hash(bytes)}  ${file}\n`
      })
      .join('')
    if (regular(join(signed, 'SHA256SUMS')).toString() !== receipt)
      throw Error('Signing handoff digest mismatch')
  }
  fs.writeFileSync(
    path('release-assets', 'SHA256SUMS'),
    files
      .map(
        (file) => `${hash(regular(path('release-assets', file)))}  ${file}\n`
      )
      .join('')
  )
  files.push('SHA256SUMS')
  const asset = (file) => regular(path('release-assets', file))
  // Verify every signing bundle before anything is created: a defective bundle
  // would otherwise become a permanent part of the immutable release. Nothing
  // exists yet, so a failure here is safe to rerun.
  for (const name of releaseAssets)
    for (const [kind, predicate] of [
      ['provenance', 'https://slsa.dev/provenance/v1'],
      ['sbom', 'https://spdx.dev/Document/v2.3'],
    ]) {
      const verified = JSON.parse(
        execFileSync(
          'gh',
          [
            'attestation',
            'verify',
            path('release-assets', name),
            '--repo',
            repo,
            '--bundle',
            path('release-assets', `${name}.${kind}.sigstore.json`),
            '--source-ref',
            `refs/tags/${tag}`,
            '--source-digest',
            source,
            '--signer-digest',
            source,
            '--deny-self-hosted-runners',
            '--predicate-type',
            predicate,
            '--signer-workflow',
            `${repo}/.github/workflows/release-build.yml`,
            '--format',
            'json',
          ],
          // The SBOM result embeds the whole SPDX document, which exceeds
          // Node's 1 MiB default output buffer.
          { timeout: 60000, stdio: 'pipe', maxBuffer: 256 * 1024 * 1024 }
        )
      )
      // The signed SBOM predicate must be the SPDX document shipped beside it.
      if (
        kind === 'sbom' &&
        (!Array.isArray(verified) ||
          verified.length !== 1 ||
          !isDeepStrictEqual(
            verified[0]?.verificationResult?.statement?.predicate,
            JSON.parse(asset(`${name}.spdx.json`))
          ))
      )
        throw Error(`Signed SBOM differs from the shipped SPDX: ${name}`)
    }
  // Close API connections before synchronous gh checks can leave pooled
  // sockets idle. Every read is time-bounded. Mutations are single-shot and
  // rely on the job timeout: a client-side abort cannot cancel a create,
  // upload or publish GitHub already accepted.
  const api = async (endpoint, method = 'GET', body, timeout = 10000) => {
    const response = await fetch(`${apiOrigin}/repos/${repo}/${endpoint}`, {
      method,
      headers: {
        Connection: 'close',
        Authorization: `Bearer ${token}`,
        Accept: 'application/vnd.github+json',
        'Content-Type': 'application/json',
      },
      ...(method === 'GET' ? { signal: AbortSignal.timeout(timeout) } : {}),
      ...(body ? { body: JSON.stringify(body) } : {}),
    })
    if (!response.ok)
      throw Object.assign(
        Error(`GitHub API failed: ${method} ${endpoint} ${response.status}`),
        { status: response.status }
      )
    return response.json()
  }
  // Phase 1 uses the commit digest for both provenance and release membership.
  // Annotated tags have a distinct tag-object digest, so reject them before
  // creating a draft or uploading/publishing any release assets.
  const tagRef = await api(`git/ref/tags/${encodeURIComponent(tag)}`)
  if (tagRef.object?.type !== 'commit' || tagRef.object.sha !== source)
    throw Error('Phase 1 requires a lightweight tag at SOURCE_COMMIT')
  // List releases, including drafts. Never resume, overwrite or delete a
  // matching tag.
  for (let page = 1; ; page++) {
    if (page > 1000) throw Error('Release listing exceeded bound')
    const releases = await api(`releases?per_page=100&page=${page}`)
    if (!Array.isArray(releases)) throw Error('Invalid release listing')
    if (releases.some((release) => release.tag_name === tag))
      throw Error('Release or draft already exists')
    if (releases.length < 100) break
  }
  const created = await api('releases', 'POST', {
    tag_name: tag,
    target_commitish: source,
    name: tag,
    draft: true,
    prerelease,
    make_latest,
    body: releaseNotes(repo, tag),
  })
  // Log the ID before checking it: recovery starts from this line.
  log(`Created release ID: ${created.id}`)
  if (
    !Number.isSafeInteger(created.id) ||
    created.id <= 0 ||
    created.tag_name !== tag ||
    created.draft !== true ||
    created.prerelease !== prerelease
  )
    throw Error('Invalid created release identity')
  const id = created.id
  for (const file of files) {
    const bytes = asset(file)
    const response = await fetch(
      `${uploadOrigin}/repos/${repo}/releases/${id}/assets?name=${encodeURIComponent(file)}`,
      {
        method: 'POST',
        headers: {
          Authorization: `Bearer ${token}`,
          'Content-Type': 'application/octet-stream',
        },
        body: bytes,
      }
    )
    if (!response.ok) throw Error(`Asset upload failed: ${file}`)
    const uploaded = await response.json()
    if (
      uploaded.name !== file ||
      uploaded.state !== 'uploaded' ||
      uploaded.size !== bytes.length ||
      uploaded.digest !== `sha256:${hash(bytes)}`
    )
      throw Error(`Upload mismatch: ${file}`)
  }
  const published = await api(`releases/${id}`, 'PATCH', {
    draft: false,
    prerelease,
    make_latest,
  })
  // Post-publish reads can lag publication. Each read retries any failure at
  // most five times within one shared three-minute window; a returned result
  // that fails a check stops at once.
  const deadline = now() + 180000
  const poll = async (label, limit, read) => {
    let failure
    for (const delay of [0, 2000, 5000, 10000, 20000]) {
      if (now() + delay >= deadline) break
      if (delay) {
        warn(`${label}: ${failure.message}; retry in ${delay}ms`)
        await sleep(delay)
      }
      try {
        return await read(Math.max(1, Math.min(limit, deadline - now())))
      } catch (error) {
        failure = error
      }
    }
    throw Error(`${label} failed; the release is already published`, {
      cause: failure,
    })
  }
  const release = await poll('Published release read', 10000, (timeout) =>
    api(`releases/${id}`, 'GET', undefined, timeout)
  )
  if (
    published.id !== id ||
    release.id !== id ||
    release.tag_name !== tag ||
    release.draft !== false ||
    release.immutable !== true ||
    release.prerelease !== prerelease
  )
    throw Error('Published release identity or immutability mismatch')
  // GitHub creates the release attestation from the published release, so it
  // can appear after the release. Its verified statement must name this
  // release, commit and the exact uploaded asset digests.
  const proof = await poll(
    'Release attestation verification',
    30000,
    (timeout) =>
      JSON.parse(
        execFileSync(
          'gh',
          ['release', 'verify', '--repo', repo, '--format', 'json', '--', tag],
          { timeout, stdio: 'pipe' }
        )
      )
  )
  const { statement, subjects } = checkStatement(
    proof?.verificationResult?.statement,
    repo,
    source
  )
  if (
    statement.predicate.tag !== tag ||
    statement.predicate.databaseId !== String(id) ||
    subjects.length !== files.length ||
    subjects.some(
      (subject) =>
        !files.includes(subject.name) ||
        subject.digest.sha256 !== hash(asset(subject.name))
    )
  )
    throw Error('Current release attestation identity or assets mismatch')
  log('Verified release:', tag, 'signature and attestation statement match')
}

if (import.meta.main)
  publish({ env: process.env }).catch((error) => {
    console.error(error)
    process.exitCode = 1
  })
