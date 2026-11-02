// Phase 1 release publisher. The release job runs this file from the tagged
// commit; it never checks out or executes build inputs.
import * as fs from 'node:fs'
import { createHash } from 'node:crypto'
import { execFileSync as nodeExecFileSync } from 'node:child_process'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { isDeepStrictEqual } from 'node:util'

export const releaseAssets = [
  'trusted-server-axum-x86_64-unknown-linux-gnu',
  'trusted-server-axum-x86_64-unknown-linux-musl',
  'trusted-server-axum-aarch64-unknown-linux-gnu',
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
    .replace('{{ASSETS}}', releaseAssets.join(' '))
    .trimEnd()
}

/**
 * Decodes and checks a release attestation's DSSE envelope.
 */
function decodeStatement(bundle, repo, source) {
  const envelope = bundle?.dsseEnvelope
  if (
    envelope?.payloadType !== 'application/vnd.in-toto+json' ||
    typeof envelope.payload !== 'string' ||
    !envelope.payload ||
    !Array.isArray(envelope.signatures) ||
    !envelope.signatures.length
  )
    throw Error('Malformed release attestation bundle')
  const bytes = Buffer.from(envelope.payload, 'base64')
  if (bytes.toString('base64') !== envelope.payload)
    throw Error('Malformed release attestation payload')
  return checkStatement(JSON.parse(bytes.toString()), repo, source)
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
 * Returns a reader that retries transient post-publish read failures (at most
 * three attempts) and never returns a result after `deadline`.
 */
export function readRetrier({
  deadline,
  now = Date.now,
  sleep = (delay) => new Promise((resolve) => setTimeout(resolve, delay)),
  warn = console.warn,
}) {
  return async (operation) => {
    for (let attempt = 1; ; attempt++)
      try {
        if (now() >= deadline)
          throw Error('Post-publish verification window exhausted')
        const result = await operation()
        if (now() >= deadline)
          throw Error('Post-publish verification window exhausted')
        return result
      } catch (error) {
        const stderr = error.stderr?.toString().trim() ?? ''
        const transient =
          !error.stdout?.length &&
          (error.name === 'TimeoutError' ||
            error.status === 429 ||
            (error.status >= 500 && error.status <= 599) ||
            /^(ECONNRESET|ETIMEDOUT|EAI_AGAIN|UND_ERR_SOCKET|UND_ERR_CONNECT_TIMEOUT|UND_ERR_HEADERS_TIMEOUT|UND_ERR_BODY_TIMEOUT)$/.test(
              error.cause?.code ?? error.code
            ) ||
            /^HTTP (429|5\d\d): [^\r\n]+$/.test(stderr))
        if (!transient || attempt === 3) throw error
        const after =
          Number(error.retryAfter) * 1000 ||
          Date.parse(error.retryAfter) - now()
        const delay = Math.min(
          10000,
          Math.max(
            [2000, 5000][attempt - 1],
            Number.isFinite(after) ? after : 0
          )
        )
        if (now() + delay >= deadline)
          throw Error('Post-publish retry window exhausted')
        warn(
          `Post-publish read retry: attempt ${attempt + 1}/3, transient ${error.status ?? 'transport/CLI'} failure, wait ${delay}ms`
        )
        await sleep(delay)
      }
  }
}

/**
 * Verifies the signing handoff, creates one draft, uploads, publishes it as an
 * immutable release and verifies the published result.
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
    if (
      !response.ok ||
      (endpoint.startsWith('attestations/') && response.status !== 200)
    )
      throw Object.assign(
        Error(`GitHub API failed: ${method} ${endpoint} ${response.status}`),
        {
          status: response.status,
          retryAfter: response.headers.get('retry-after'),
        }
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
  if (
    !Number.isSafeInteger(created.id) ||
    created.id <= 0 ||
    created.tag_name !== tag ||
    created.draft !== true ||
    created.prerelease !== prerelease
  )
    throw Error('Invalid created release identity')
  const id = created.id
  log(`Created release ID: ${id}`)
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
  const retryDeadline = now() + 180000
  const remaining = (limit) =>
    Math.max(1, Math.min(limit, retryDeadline - now()))
  const retryRead = readRetrier({ deadline: retryDeadline, now, sleep, warn })
  const verifyIdentity = async () => {
    const release = await retryRead(() =>
      api(`releases/${id}`, 'GET', undefined, remaining(10000))
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
    if (
      !Array.isArray(release.assets) ||
      release.assets.length !== files.length ||
      new Set(release.assets.map((item) => item.name)).size !== files.length
    )
      throw Error('Published asset set mismatch')
    for (const item of release.assets) {
      if (!files.includes(item.name)) throw Error('Unexpected published asset')
      const bytes = asset(item.name)
      if (item.size !== bytes.length || item.digest !== `sha256:${hash(bytes)}`)
        throw Error('Published asset digest mismatch')
    }
    const byTag = await retryRead(() =>
      api(
        `releases/tags/${encodeURIComponent(tag)}`,
        'GET',
        undefined,
        remaining(10000)
      )
    )
    if (byTag.id !== id) throw Error('Published tag identity mismatch')
  }
  await verifyIdentity()
  // The current release's attestation must name this release, commit and the
  // exact uploaded asset digests.
  const checkCurrent = ({ statement, subjects }) => {
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
  }
  // `gh release verify` fetches the bundle itself (inline or via bundle_url)
  // and returns it with the statement it verified. Check both, and compare
  // with the inline record when the API supplied one.
  const fetchProof = () =>
    retryRead(() =>
      JSON.parse(
        execFileSync(
          'gh',
          ['release', 'verify', '--repo', repo, '--format', 'json', '--', tag],
          { timeout: remaining(10000), stdio: 'pipe' }
        )
      )
    )
  const checkProof = (proof, inline) => {
    const bundle = proof?.attestation?.bundle
    const decoded = decodeStatement(bundle, repo, source)
    if (
      !isDeepStrictEqual(decoded.statement, proof.verificationResult?.statement)
    )
      throw Error('Verified release attestation statement mismatch')
    checkCurrent(decoded)
    if (inline && !isDeepStrictEqual(bundle, inline.bundle))
      throw Error('Verified release attestation bundle mismatch')
  }
  // The release attestation can be created after the immutable release
  // becomes visible.
  const availabilityDeadline = Math.min(retryDeadline, now() + 60000)
  // GitHub CLI reports a bundle-fetch, trusted-root or network failure the
  // same way as a record that is not yet listed, so any failed proof fetch is
  // retried until the availability checks run out. A fetched proof that fails
  // a check is final.
  let pending
  const exhausted = () =>
    pending ?? Error('Release attestation availability window exhausted')
  for (let attempt = 1; ; attempt++) {
    if (now() >= availabilityDeadline) throw exhausted()
    // Only this endpoint maps HTTP 404 to an unavailable record. Other
    // statuses fail once transient retries are exhausted.
    let page,
      missing = false
    try {
      page = await retryRead(() =>
        api(
          `attestations/sha1:${source}?per_page=100&predicate_type=release`,
          'GET',
          undefined,
          Math.max(1, Math.min(4000, availabilityDeadline - now()))
        )
      )
    } catch (error) {
      if (error.status !== 404) throw error
      page = { attestations: [] }
      missing = true
    }
    if (!Array.isArray(page.attestations) || page.attestations.length >= 100)
      throw Error('Invalid or truncated release attestation response')
    const records = []
    let opaque = 0
    for (const record of page.attestations) {
      if (!record || typeof record.initiator !== 'string')
        throw Error('Malformed release attestation record')
      if (record.initiator !== 'github') continue
      // The API may omit the inline bundle and supply only `bundle_url`.
      if (record.bundle == null) {
        if (typeof record.bundle_url !== 'string' || !record.bundle_url)
          throw Error('Malformed release attestation bundle')
        opaque++
        continue
      }
      records.push({
        ...record,
        ...decodeStatement(record.bundle, repo, source),
      })
    }
    const current = records.filter(
      (record) => record.statement.predicate.tag === tag
    )
    if (current.length > 1)
      throw Error('Multiple release attestations for current tag')
    if (current.length) checkCurrent(current[0])
    // Another tag's release (for example an earlier prerelease on this commit)
    // can be ignored by this availability check. GitHub CLI may still fetch
    // its bundle before filtering by tag during the cryptographic checks.
    for (const record of records.filter(
      (record) => record.statement.predicate.tag !== tag
    ))
      log(
        'Ignored release attestation for other tag:',
        JSON.stringify(record.statement.predicate.tag)
      )
    // Without an inline bundle the record's tag is unknown until GitHub CLI
    // fetches it.
    pending = undefined
    if (current.length || opaque) {
      let proof
      try {
        proof = await fetchProof()
      } catch (error) {
        pending = error
      }
      if (proof) {
        checkProof(proof, current[0])
        log(
          attempt === 1
            ? 'Release attestation available on first check'
            : `Release attestation available on check ${attempt}/5`
        )
        log(
          'Verified release:',
          tag,
          'signature and attestation statement match'
        )
        break
      }
    }
    if (attempt === 5) {
      if (pending) throw pending
      throw Error(
        'Release already published and immutable; release attestation unavailable after five checks'
      )
    }
    const delay = [2000, 5000, 10000, 20000][attempt - 1]
    if (now() + delay >= availabilityDeadline) throw exhausted()
    warn(
      `Release attestation availability: attempt ${attempt + 1}/5, ${missing ? 'HTTP 404' : pending ? 'not yet verifiable' : 'HTTP 200, no current-tag record'}, wait ${delay}ms`
    )
    await sleep(delay)
  }
  for (const file of files)
    log(
      'Verified asset:',
      file,
      (
        await retryRead(() =>
          execFileSync(
            'gh',
            [
              'release',
              'verify-asset',
              tag,
              path('release-assets', file),
              '--repo',
              repo,
            ],
            { timeout: remaining(10000) }
          )
        )
      ).toString()
    )
  // Re-download into a fresh directory on every transport retry; verify bytes
  // outside retries.
  let download
  try {
    await retryRead(() => {
      if (download) fs.rmSync(download, { recursive: true, force: true })
      download = fs.mkdtempSync(path('published-download-'))
      return execFileSync(
        'gh',
        ['release', 'download', tag, '--repo', repo, '--dir', download],
        { timeout: remaining(60000), stdio: 'pipe' }
      )
    })
    exact(download, files)
    for (const file of files)
      if (!regular(join(download, file)).equals(asset(file)))
        throw Error(`Downloaded asset mismatch: ${file}`)
    log(
      'Verified post-publish re-download: all',
      files.length,
      'assets byte-identical to signed checksums and handoff'
    )
  } finally {
    if (download) fs.rmSync(download, { recursive: true, force: true })
  }
  await verifyIdentity()
}

if (import.meta.main)
  publish({ env: process.env }).catch((error) => {
    console.error(error)
    process.exitCode = 1
  })
