// Publishes a published GitHub Release's binaries as GHCR OCI artifacts.
//
// The packages job runs this file from the tagged commit after the release
// job. It downloads the release by tag, checks every file against the
// release's SHA256SUMS, and pushes each binary with its evidence files as one
// artifact. It never builds, signs or changes the release.
import * as fs from 'node:fs'
import { createHash } from 'node:crypto'
import { execFileSync } from 'node:child_process'
import { join } from 'node:path'
import { isReleaseTag, releaseAssets } from './publish.mjs'

const registry = 'ghcr.io'
const sigstoreBundle = 'application/vnd.dev.sigstore.bundle.v0.3+json'

const hash = (bytes) => createHash('sha256').update(bytes).digest('hex')

/**
 * Returns the GHCR package and tag for one release binary.
 */
export function packageReference(owner, tag, asset) {
  const namespace = owner.toLowerCase()
  if (asset === 'trusted-server-fastly.wasm')
    return `${registry}/${namespace}/trusted-server-fastly:${tag}`
  const target = asset.match(/^ts-([a-z0-9_]+(?:-[a-z0-9_]+)+)$/)?.[1]
  if (!target) throw Error(`No package for release asset ${asset}`)
  return `${registry}/${namespace}/trusted-server-cli:${tag}-${target}`
}

/**
 * Lists the files one binary's artifact carries, with their media types.
 */
export function packageFiles(asset) {
  const files = [
    {
      name: asset,
      mediaType: asset.endsWith('.wasm')
        ? 'application/wasm'
        : 'application/octet-stream',
    },
    { name: `${asset}.spdx.json`, mediaType: 'application/spdx+json' },
    { name: `${asset}.provenance.sigstore.json`, mediaType: sigstoreBundle },
    { name: `${asset}.sbom.sigstore.json`, mediaType: sigstoreBundle },
  ]
  if (asset === 'trusted-server-fastly.wasm')
    files.push({
      name: `${asset}.rebuild-check.json`,
      mediaType: 'application/json',
    })
  return files
}

/**
 * Plans one artifact per release binary for `repo` at `tag`.
 */
export function packagePlan(repo, tag, commit) {
  if (!isReleaseTag(tag)) throw Error(`Not a release tag: ${tag}`)
  if (!/^[0-9a-f]{40}$/.test(commit)) throw Error('Invalid source commit')
  const [owner] = repo.split('/')
  const source = `https://github.com/${repo}`
  return releaseAssets.map((asset) => ({
    asset,
    reference: packageReference(owner, tag, asset),
    artifactType: asset.endsWith('.wasm')
      ? 'application/vnd.trusted-server.fastly.wasm.v1'
      : 'application/vnd.trusted-server.cli.v1',
    files: packageFiles(asset),
    annotations: {
      'org.opencontainers.image.source': source,
      'org.opencontainers.image.revision': commit,
      'org.opencontainers.image.version': tag,
      'org.opencontainers.image.title': asset,
      'org.opencontainers.image.description': `${asset} from Trusted Server ${tag}, with its SPDX SBOM and Sigstore attestation bundles`,
    },
  }))
}

/**
 * Checks a downloaded release directory against its SHA256SUMS.
 *
 * The directory must hold exactly the files SHA256SUMS lists plus SHA256SUMS
 * itself, and every planned file must be listed. Returns name → sha256.
 */
export function checkDownload(directory, plan) {
  const lines = fs
    .readFileSync(join(directory, 'SHA256SUMS'), 'utf8')
    .split('\n')
    .filter(Boolean)
  const sums = new Map()
  for (const line of lines) {
    const match = line.match(/^([0-9a-f]{64}) {2}([A-Za-z0-9._-]+)$/)
    if (!match || sums.has(match[2]))
      throw Error(`Malformed SHA256SUMS line: ${line}`)
    sums.set(match[2], match[1])
  }
  const present = fs.readdirSync(directory).sort()
  const expected = [...sums.keys(), 'SHA256SUMS'].sort()
  if (JSON.stringify(present) !== JSON.stringify(expected))
    throw Error('Downloaded release files differ from SHA256SUMS')
  for (const [name, digest] of sums) {
    const stat = fs.lstatSync(join(directory, name))
    if (!stat.isFile()) throw Error(`Not a regular file: ${name}`)
    if (hash(fs.readFileSync(join(directory, name))) !== digest)
      throw Error(`Digest mismatch: ${name}`)
  }
  for (const { files } of plan)
    for (const { name } of files)
      if (!sums.has(name)) throw Error(`Release lacks ${name}`)
  return sums
}

/**
 * Checks a pushed manifest carries exactly the planned files and digests.
 */
export function checkManifest(manifest, entry, sums) {
  const layers = manifest?.layers
  if (
    manifest?.artifactType !== entry.artifactType ||
    !Array.isArray(layers) ||
    layers.length !== entry.files.length
  )
    throw Error(`Unexpected manifest for ${entry.reference}`)
  entry.files.forEach(({ name, mediaType }, index) => {
    const layer = layers[index]
    if (
      layer.mediaType !== mediaType ||
      layer.digest !== `sha256:${sums.get(name)}` ||
      layer.annotations?.['org.opencontainers.image.title'] !== name
    )
      throw Error(`Layer ${name} differs in ${entry.reference}`)
  })
  for (const [key, value] of Object.entries(entry.annotations))
    if (manifest.annotations?.[key] !== value)
      throw Error(`Annotation ${key} differs in ${entry.reference}`)
}

/**
 * Reports whether a reference already resolves, keeping tags write-once.
 */
function exists(run, reference) {
  try {
    run('oras', ['manifest', 'fetch', '--descriptor', reference])
    return true
  } catch {
    return false
  }
}

/**
 * Downloads the published release, checks it and pushes every artifact.
 */
export function publishPackages({ env, run = defaultRun, log = console.log }) {
  const repo = env.GH_REPO
  const tag = env.RELEASE_TAG
  const commit = env.SOURCE_COMMIT
  if (env.PACKAGE_PUBLICATION_ENABLED !== 'true')
    throw Error('Package publication is not enabled')
  const plan = packagePlan(repo, tag, commit)
  const release = JSON.parse(
    run('gh', [
      'release',
      'view',
      tag,
      '--repo',
      repo,
      '--json',
      'tagName,isDraft',
    ])
  )
  if (release.tagName !== tag || release.isDraft !== false)
    throw Error(`Release ${tag} is not published`)
  const directory = fs.mkdtempSync(join(env.RUNNER_TEMP ?? '/tmp', 'release-'))
  run('gh', ['release', 'download', tag, '--repo', repo, '--dir', directory])
  const sums = checkDownload(directory, plan)
  for (const entry of plan)
    if (exists(run, entry.reference))
      throw Error(`Refusing to overwrite ${entry.reference}`)
  for (const entry of plan) {
    run(
      'oras',
      [
        'push',
        entry.reference,
        '--artifact-type',
        entry.artifactType,
        ...Object.entries(entry.annotations).flatMap(([key, value]) => [
          '--annotation',
          `${key}=${value}`,
        ]),
        ...entry.files.map(({ name, mediaType }) => `${name}:${mediaType}`),
      ],
      { cwd: directory }
    )
    const manifest = JSON.parse(
      run('oras', ['manifest', 'fetch', entry.reference])
    )
    checkManifest(manifest, entry, sums)
    log('Published package:', entry.reference)
  }
}

function defaultRun(command, args, options = {}) {
  return execFileSync(command, args, {
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'inherit'],
    ...options,
  })
}

if (import.meta.main)
  try {
    publishPackages({ env: process.env })
  } catch (error) {
    console.error(error)
    process.exitCode = 1
  }
