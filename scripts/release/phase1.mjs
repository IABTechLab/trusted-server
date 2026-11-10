/** Small Phase 1 build-inventory and configuration checks, without release authority. */
import { createHash } from 'node:crypto'
import { execFileSync } from 'node:child_process'
import {
  readFileSync,
  statSync,
  writeFileSync,
  openSync,
  closeSync,
  fsyncSync,
  realpathSync,
  mkdirSync,
} from 'node:fs'
import { join, resolve } from 'node:path'
import { pathToFileURL } from 'node:url'

/** Retains CI-only acquisition failure facts without environment or error text. */
export function unavailableOrdinaryDiagnostic(directory) {
  const facts = Object.fromEntries(
    [
      'canonical_artifact_id',
      'canonical_observation_artifact_id',
      'canonical_sha256',
      'canonical_size',
      'canonical_window',
      'first_difference_offset',
      'reproduction_artifact_id',
      'reproduction_observation_artifact_id',
      'reproduction_sha256',
      'reproduction_size',
      'reproduction_window',
    ].map((key) => [key, 'unavailable'])
  )
  const bytes =
    JSON.stringify({
      code: 'transfer_unavailable',
      facts,
      kind: 'ordinary-comparison-diagnostic',
      outcome: 'unavailable',
      stage: 'transfer',
      version: 1,
    }) + '\n'
  if (Buffer.byteLength(bytes) > 4096)
    throw Error('Ordinary diagnostic exceeds cap')
  mkdirSync(directory, { mode: 0o700 })
  writeFileSync(join(directory, 'ordinary-comparison-diagnostic.json'), bytes, {
    flag: 'wx',
    mode: 0o600,
  })
}

/** Fetches only fixed read-only current-run metadata, retaining strict-parser bytes. */
export async function ordinaryApi(path, token, fetcher = fetch, deadline) {
  if (
    !/^\/repos\/[A-Za-z0-9][A-Za-z0-9_.-]*\/[A-Za-z0-9][A-Za-z0-9_.-]*\/actions\/runs\/[1-9][0-9]*(?:\/attempts\/[1-9][0-9]*\/jobs|\/artifacts)\?per_page=100$/.test(
      path
    ) ||
    !token
  )
    throw Error('Invalid ordinary metadata request')
  const response = await ordinaryFetch(
    // Fixed GitHub Actions metadata service, not an integration destination.
    `https://api.github.com${path}`, // allow-domain: api.github.com
    {
      method: 'GET',
      redirect: 'manual',
      signal: AbortSignal.any([
        AbortSignal.timeout(10_000),
        ...(deadline ? [deadline] : []),
      ]),
      headers: {
        Authorization: `Bearer ${token}`,
        Accept: 'application/vnd.github+json',
        'X-GitHub-Api-Version': '2022-11-28',
      },
    },
    fetcher
  )
  if (response.status !== 200) {
    discardOrdinaryBody(response)
    throw Error(`Ordinary metadata HTTP ${response.status}`)
  }
  const chunks = []
  await ordinaryBody(response, 1024 * 1024, (chunk) =>
    chunks.push(Buffer.from(chunk))
  )
  return Buffer.concat(chunks)
}

/** Downloads one already-bound archive by exact ID, without forwarding credentials. */
export async function ordinaryDownload(request, fetcher = fetch) {
  const { repository, archive, token, directory, deadline } = request
  const limits = {
    'canonical.zip': 768 * 1024 * 1024,
    'canonical-observation.zip': 2 * 1024 * 1024,
    'reproduction.zip': 768 * 1024 * 1024,
    'reproduction-observation.zip': 2 * 1024 * 1024,
  }
  if (
    !/^[A-Za-z0-9][A-Za-z0-9_.-]*\/[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(
      repository
    ) ||
    repository.length > 200 ||
    !token ||
    !archive ||
    !Number.isSafeInteger(archive.id) ||
    archive.id <= 0 ||
    !Number.isSafeInteger(archive.size) ||
    archive.size <= 0 ||
    !Object.hasOwn(limits, archive.file) ||
    archive.size > limits[archive.file] ||
    !/^[a-f0-9]{64}$/.test(archive.sha256) ||
    realpathSync(directory) !== directory
  )
    throw Error('Invalid bound ordinary archive request')
  let descriptor
  try {
    descriptor = openSync(join(directory, archive.file), 'wx', 0o600)
    const signal = AbortSignal.any([
      AbortSignal.timeout(60_000),
      ...(deadline ? [deadline] : []),
    ])
    const response = await ordinaryFetch(
      // Fixed artifact API returns the authenticated download redirect.
      `https://api.github.com/repos/${repository}/actions/artifacts/${archive.id}/zip`, // allow-domain: api.github.com
      {
        method: 'GET',
        redirect: 'manual',
        signal,
        headers: {
          Authorization: `Bearer ${token}`,
          Accept: 'application/vnd.github+json',
          'X-GitHub-Api-Version': '2022-11-28',
        },
      },
      fetcher
    )
    const location = response.headers.get('location')
    discardOrdinaryBody(response)
    if (response.status !== 302)
      throw Error('Ordinary archive API did not return its single redirect')
    const url = new URL(location)
    if (
      url.protocol !== 'https:' ||
      url.username ||
      url.password ||
      url.port ||
      url.hash
    )
      throw Error('Ordinary archive redirect rejected')
    const blob = await ordinaryFetch(
      url.href,
      { method: 'GET', redirect: 'error', signal },
      fetcher
    )
    if (blob.status !== 200) {
      discardOrdinaryBody(blob)
      throw Error('Ordinary archive blob response rejected')
    }
    const hash = createHash('sha256')
    const size = await ordinaryBody(blob, archive.size, (chunk) => {
      hash.update(chunk)
      writeFileSync(descriptor, chunk)
    })
    if (size !== archive.size || hash.digest('hex') !== archive.sha256)
      throw Error('Ordinary archive digest or size mismatch')
    fsyncSync(descriptor)
  } catch {
    throw Error(
      'Ordinary archive acquisition failed; no comparison result is available'
    )
  } finally {
    if (descriptor !== undefined) closeSync(descriptor)
  }
}

/** Acquires four archives only after the Rust current-run binding accepts metadata. */
export async function acquireOrdinaryTransfers(
  request,
  fetcher = fetch,
  planner = ordinaryPlan
) {
  const { producer, input, archives, repository, runId, attempt, token } =
    request
  if (
    !/^[A-Za-z0-9][A-Za-z0-9_.-]*\/[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(
      repository
    ) ||
    repository.length > 200 ||
    !/^[1-9][0-9]*$/.test(runId) ||
    !/^[1-9][0-9]*$/.test(attempt) ||
    !Number.isSafeInteger(+runId) ||
    !Number.isSafeInteger(+attempt) ||
    realpathSync(input) !== input ||
    !token
  )
    throw Error('Invalid ordinary acquisition context')
  const prefix = `/repos/${repository}/actions/runs/${runId}`
  const deadline = AbortSignal.timeout(300_000)
  for (const [file, endpoint] of [
    ['jobs.json', `${prefix}/attempts/${attempt}/jobs?per_page=100`],
    ['artifacts.json', `${prefix}/artifacts?per_page=100`],
  ])
    writeFileSync(
      join(input, file),
      await ordinaryApi(endpoint, token, fetcher, deadline),
      { flag: 'wx', mode: 0o600 }
    )
  const plan = planner(producer, input)
  deadline.throwIfAborted()
  const names = [
    'canonical.zip',
    'canonical-observation.zip',
    'reproduction.zip',
    'reproduction-observation.zip',
  ]
  if (
    plan?.version !== 1 ||
    !Array.isArray(plan.archives) ||
    plan.archives.length !== 4 ||
    !plan.archives.every((archive, index) => archive?.file === names[index]) ||
    new Set(plan.archives.map((archive) => archive.id)).size !== 4
  )
    throw Error('Ordinary bound transfer plan rejected')
  mkdirSync(archives, { mode: 0o700 })
  for (const archive of plan.archives)
    await ordinaryDownload(
      { repository, archive, token, directory: archives, deadline },
      fetcher
    )
}

function ordinaryPlan(producer, input) {
  try {
    return JSON.parse(
      execFileSync(producer, ['ordinary-transfer-plan', '--input', input], {
        encoding: 'utf8',
        timeout: 30_000,
        killSignal: 'SIGKILL',
        maxBuffer: 1024 * 1024,
        env: { PATH: process.env.PATH, LC_ALL: 'C', TZ: 'UTC' },
        stdio: ['ignore', 'pipe', 'pipe'],
      })
    )
  } catch {
    throw Error('Ordinary current-run transfer binding failed')
  }
}

async function ordinaryFetch(url, options, fetcher) {
  try {
    options.signal.throwIfAborted()
    return await fetcher(url, options)
  } catch {
    throw Error('Ordinary read-only transport failed')
  }
}

function discardOrdinaryBody(response) {
  response.body?.cancel().catch(() => {})
}

async function ordinaryBody(response, limit, consume) {
  const reader = response.body?.getReader()
  if (!reader) throw Error('Ordinary response body is missing')
  let size = 0
  try {
    while (true) {
      const { done, value } = await reader.read()
      if (done) return size
      size += value.byteLength
      if (size > limit) throw Error('Ordinary response exceeds its byte bound')
      consume(value)
    }
  } catch {
    throw Error('Ordinary response body failed its bounded read')
  } finally {
    await reader.cancel().catch(() => {})
    reader.releaseLock()
  }
}

/** Check minimal wasm headers and final Linux loader requirements. */
export function checkReleaseFormat(bytes, target, elf = '') {
  if (target === 'wasm32-wasip1') {
    if (!bytes.subarray(0, 8).equals(Buffer.from('0061736d01000000', 'hex')))
      throw Error('Invalid wasm magic or version')
  } else if (target === 'x86_64-unknown-linux-gnu') {
    // 64-bit little-endian ELF whose e_machine is EM_X86_64.
    if (
      bytes.length < 20 ||
      !bytes.subarray(0, 6).equals(Buffer.from('7f454c460201', 'hex')) ||
      bytes.readUInt16LE(18) !== 0x3e
    )
      throw Error('Expected target-matched 64-bit ELF')
    if (/GLIBC_[A-Z]/.test(elf)) throw Error('Unknown GLIBC requirement')
    for (const [, major, minor, patch] of elf.matchAll(
      /GLIBC_(\d+)\.(\d+)(?:\.(\d+))?/g
    ))
      if (
        +major > 2 ||
        (+major === 2 && (+minor > 34 || (+minor === 34 && +patch > 0)))
      )
        throw Error('GLIBC requirement exceeds 2.34')
  } else if (target.endsWith('-apple-darwin')) {
    if (
      bytes.length < 32 ||
      bytes.readUInt32LE(0) !== 0xfeedfacf ||
      bytes.readUInt32LE(4) !==
        (target.startsWith('aarch64-') ? 0x0100000c : 0x01000007)
    )
      throw Error('Expected target-matched 64-bit Mach-O')
    const end = 32 + bytes.readUInt32LE(20),
      count = bytes.readUInt32LE(16)
    if (end > bytes.length || count > Math.floor((end - 32) / 8))
      throw Error('Truncated Mach-O commands')
    let offset = 32,
      minimum
    for (let i = 0; i < count; i++) {
      if (offset + 8 > end) throw Error('Truncated Mach-O command')
      const command = bytes.readUInt32LE(offset),
        size = bytes.readUInt32LE(offset + 4)
      if (size < 8 || offset + size > end)
        throw Error('Invalid Mach-O command size')
      // LC_BUILD_VERSION for PLATFORM_MACOS; a 13.0 deployment target emits
      // no older LC_VERSION_MIN_MACOSX command.
      if (command === 0x32) {
        if (
          minimum !== undefined ||
          size < 24 ||
          bytes.readUInt32LE(offset + 8) !== 1
        )
          throw Error('Invalid macOS deployment command')
        minimum = bytes.readUInt32LE(offset + 12)
      }
      offset += size
    }
    if (offset !== end || minimum === undefined || minimum !== 0x000d0000)
      throw Error('Expected macOS minimum OS 13.0')
    console.log('SUCCESS: Mach-O target and macOS minimum OS 13.0:', target)
  } else throw Error('Unsupported release target')
}

/** Complete converter output with the actual binary's SPDX package. */
export function completeSpdx(document, binary, original) {
  document.creationInfo ??= {}
  if (typeof document.creationInfo.created === 'string')
    document.creationInfo.created = document.creationInfo.created.replace(
      /\.\d+Z$/,
      'Z'
    )
  document.creationInfo.creators = [
    ...new Set([
      ...(document.creationInfo.creators ?? []),
      'Tool: cargo-cyclonedx-0.5.9',
      'Tool: cyclonedx-cli-0.33.1',
      'Tool: trusted-server-phase1',
    ]),
  ]
  for (const item of document.packages)
    for (const key of ['licenseDeclared', 'licenseConcluded'])
      if (typeof item[key] === 'string')
        item[key] = item[key].replace(/\s+/g, ' ').trim()
  const root = original.metadata.component
  document.packages.push({
    SPDXID: 'SPDXRef-Artifact',
    name: root.name,
    versionInfo: root.version,
    downloadLocation: 'NOASSERTION',
    filesAnalyzed: false,
    licenseConcluded: 'NOASSERTION',
    licenseDeclared: 'NOASSERTION',
    copyrightText: 'NOASSERTION',
    externalRefs: root.purl
      ? [
          {
            referenceCategory: 'PACKAGE-MANAGER',
            referenceType: 'purl',
            referenceLocator: root.purl,
          },
        ]
      : [],
    checksums: [
      {
        algorithm: 'SHA256',
        checksumValue: createHash('sha256').update(binary).digest('hex'),
      },
    ],
  })
  document.documentDescribes = ['SPDXRef-Artifact']
  document.relationships = [
    {
      spdxElementId: 'SPDXRef-DOCUMENT',
      relationshipType: 'DESCRIBES',
      relatedSpdxElement: 'SPDXRef-Artifact',
    },
  ]
  return validateSpdx(document, binary, original)
}

/**
 * Rescan the binary and check that every inventory component keeps its name,
 * version and PURL in the SPDX packages.
 */
function validateSpdx(document, binary, original) {
  checkBinary(binary)
  // The signed predicate type names SPDX 2.3.
  if (
    document.spdxVersion !== 'SPDX-2.3' ||
    document.SPDXID !== 'SPDXRef-DOCUMENT'
  )
    throw new Error('Expected an SPDX 2.3 document')
  const packages = document.packages
  const ids = new Set(packages.map((entry) => entry.SPDXID))
  if (ids.size !== packages.length)
    throw new Error('Duplicate SPDX package identity')
  if ([...ids].some((id) => !/^SPDXRef-[A-Za-z0-9.-]+$/.test(id)))
    throw new Error('Invalid SPDX package identifier')
  const flatten = (items) =>
    items.flatMap((item) => [item, ...flatten(item.components ?? [])])
  const used = new Set()
  for (const component of flatten(original.components ?? [])) {
    const match = packages.find(
      (entry) =>
        !used.has(entry.SPDXID) &&
        entry.name === component.name &&
        (!component.version || entry.versionInfo === component.version) &&
        (!component.purl ||
          entry.externalRefs?.some(
            (ref) =>
              ref.referenceType === 'purl' &&
              ref.referenceLocator === component.purl
          ))
    )
    if (!match) throw new Error('SPDX lost component identity or PURL')
    used.add(match.SPDXID)
  }
  return document
}

// Adapted from #1262's native recipe scanner at 10167c6017b725d0a4d5ad0a0c8e6a96fd4aa807.
export const sentinels = [
  'phase1-origin-canary.example.com',
  'phase1-physical-store-canary',
  'phase1-service-account-canary',
  'phase1-secret-canary-32-characters',
  'phase1-permission-policy-canary',
]

export function checkBinary(bytes) {
  if (bytes.length === 0) throw new Error('Release binary is empty')
  for (const marker of sentinels) {
    if (bytes.includes(Buffer.from(marker)))
      throw new Error('Release binary contains planted operator configuration')
  }
}

function namespace(bom, prefix) {
  if (
    bom.bomFormat !== 'CycloneDX' ||
    bom.specVersion !== '1.5' ||
    !bom.metadata?.component?.['bom-ref']
  ) {
    throw new Error('Expected CycloneDX 1.5 with a root component')
  }
  const rename = (value, key) => {
    if (Array.isArray(value)) return value.map((item) => rename(item, key))
    if (value && typeof value === 'object')
      return Object.fromEntries(
        Object.entries(value).map(([k, v]) => [k, rename(v, k)])
      )
    // Hashed references keep valid SPDX identifiers after conversion.
    return typeof value === 'string' && key === 'bom-ref'
      ? `${prefix}-${createHash('sha256').update(value).digest('hex')}`
      : value
  }
  return rename(bom)
}

export function mergeSboms(cargo, npm, binary, name) {
  checkBinary(binary)
  const rust = namespace(cargo, 'cargo')
  const javascript = namespace(npm, 'npm')
  const root = rust.metadata.component
  const npmRoot = javascript.metadata.component
  // Dependency relationships are dropped: SPDX conversion would not keep them.
  return {
    bomFormat: 'CycloneDX',
    specVersion: '1.5',
    version: 1,
    metadata: {
      ...rust.metadata,
      component: {
        ...root,
        // The root's nested components are listed once, below.
        components: undefined,
        name,
        hashes: [
          {
            alg: 'SHA-256',
            content: createHash('sha256').update(binary).digest('hex'),
          },
        ],
      },
      properties: [
        {
          name: 'trusted-server:coverage',
          value:
            'Selected Cargo packages plus the installed npm build tree used for this release build. Conservative inventory: components may be removed by linking, tree-shaking or minification. External Prebid is not bundled; its installed package is a build-tree entry only. The inventory does not record dependency relationships. Native C/assembly, system libraries, Rust standard library and compiler runtimes are not completely inventoried. Complete composition is Phase 2.',
        },
      ],
    },
    components: [
      ...(rust.components ?? []),
      ...(root.components ?? []),
      npmRoot,
      ...(javascript.components ?? []),
    ],
  }
}

/**
 * Checks `ts --version` output against the workspace package version, the
 * release source commit and the build target.
 */
export function checkCliVersion(output, cargoManifest, source, target) {
  const expected =
    'ts package=' +
    cargoManifest.match(
      /\[workspace\.package\][\s\S]*?^version = "([^"]+)"/m
    )[1] +
    ` source=${source} target=${target} variant=default`
  if (output.trim() !== expected) throw Error('Unexpected CLI version')
}

async function smokeRelease(file, target) {
  const linux = target === 'x86_64-unknown-linux-gnu'
  const elf = linux
    ? execFileSync('readelf', ['--version-info', file], {
        encoding: 'utf8',
        maxBuffer: 8388608,
      })
    : ''
  checkReleaseFormat(readFileSync(file), target, elf)
  console.log(
    'SUCCESS: final-byte checks',
    target,
    [...new Set(elf.match(/GLIBC_[\w.]+/g) ?? [])].join(' ')
  )
  if (target === 'wasm32-wasip1') return
  const version = (output) => {
    checkCliVersion(
      output,
      readFileSync('Cargo.toml', 'utf8'),
      process.env.SOURCE_COMMIT,
      target
    )
    console.log('SUCCESS:', output.trim())
  }
  version(
    execFileSync(resolve(file), ['--version'], {
      encoding: 'utf8',
      timeout: 10000,
    })
  )
  if (!linux) return
  const docker = (args, input) =>
    execFileSync('docker', args, {
      input,
      encoding: 'utf8',
      timeout: args[0] === 'pull' ? 180000 : 10000,
      maxBuffer: 8388608,
    })
  for (const reference of [
    'debian:12-slim@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8',
    'ubuntu:22.04@sha256:0a9887931b0e38af2e3487ed39e29436b0535c4f3f52e57745baaf79d1898956',
    'registry.access.redhat.com/ubi9/ubi-minimal:9.6@sha256:34880b64c07f28f64d95737f82f891516de9a3b43583f39970f7bf8e4cfa48b7',
  ]) {
    docker(['pull', reference])
    const image = docker([
      'image',
      'inspect',
      '--format',
      '{{.Id}}',
      reference,
    ]).trim()
    console.log('Runtime image:', reference, image)
    version(
      docker([
        'run',
        '--rm',
        '--network=none',
        '--read-only',
        '--cap-drop=ALL',
        '--security-opt=no-new-privileges',
        '--user=65534:65534',
        '--entrypoint=/release',
        '--mount',
        `type=bind,src=${resolve(file)},dst=/release,readonly`,
        image,
        '--version',
      ])
    )
  }
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
) {
  const [command, ...args] = process.argv.slice(2)
  if (command === 'check' && args.length > 0) {
    for (const file of args) {
      if (!statSync(file).isFile())
        throw new Error('Expected regular release binary')
      checkBinary(readFileSync(file))
    }
  } else if (command === 'smoke' && args.length === 2) {
    await smokeRelease(...args)
  } else if (command === 'ordinary-diagnostic' && args.length === 1) {
    unavailableOrdinaryDiagnostic(args[0])
  } else if (command === 'ordinary-fetch' && args.length === 3) {
    const [producer, input, archives] = args
    await acquireOrdinaryTransfers({
      producer,
      input,
      archives,
      repository: process.env.GITHUB_REPOSITORY,
      runId: process.env.GITHUB_RUN_ID,
      attempt: process.env.GITHUB_RUN_ATTEMPT,
      token: process.env.GH_TOKEN,
    })
  } else if (command === 'merge' && args.length === 4) {
    const [cargo, npm, binary, output] = args
    writeFileSync(
      output,
      JSON.stringify(
        mergeSboms(
          JSON.parse(readFileSync(cargo)),
          JSON.parse(readFileSync(npm)),
          readFileSync(binary),
          binary.split('/').at(-1)
        ),
        null,
        2
      ) + '\n'
    )
  } else if (command === 'validate-spdx' && args.length === 3) {
    const [file, binary, original] = args
    const document = JSON.parse(readFileSync(file))
    const inventory = JSON.parse(readFileSync(original))
    completeSpdx(document, readFileSync(binary), inventory)
    document.creationInfo.creators.push(
      `Tool: npm-${execFileSync('npm', ['--version'], { encoding: 'utf8' }).trim()}`
    )
    const coverage = inventory.metadata.properties?.find(
      (entry) => entry.name === 'trusted-server:coverage'
    )?.value
    if (typeof coverage !== 'string' || coverage.length === 0)
      throw new Error('Missing conservative inventory coverage')
    document.creationInfo.comment = [document.creationInfo.comment, coverage]
      .filter(Boolean)
      .join('\n')
    writeFileSync(file, JSON.stringify(document, null, 2) + '\n')
  } else {
    throw new Error(
      'Expected check <binary...>, merge <cargo> <npm> <binary> <output>, or validate-spdx <spdx> <binary> <merged-cdx>'
    )
  }
}
