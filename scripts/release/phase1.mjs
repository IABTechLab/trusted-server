/** Small Phase 1 build-inventory and configuration checks, without release authority. */
import { createHash, randomUUID } from 'node:crypto'
import { execFileSync } from 'node:child_process'
import { createServer } from 'node:net'
import { once } from 'node:events'
import { readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { pathToFileURL } from 'node:url'

/** Check minimal wasm headers and final Linux loader requirements. */
export function checkReleaseFormat(bytes, target, elf = '') {
  if (target === 'wasm32-wasip1') {
    if (!bytes.subarray(0, 8).equals(Buffer.from('0061736d01000000', 'hex')))
      throw Error('Invalid wasm magic or version')
  } else if (target.includes('-linux-')) {
    if (!bytes.subarray(0, 4).equals(Buffer.from('7f454c46', 'hex')))
      throw Error('Expected ELF binary')
    if (target.endsWith('-musl')) {
      if (/INTERP|\(NEEDED\)/.test(elf))
        throw Error('Expected fully static musl binary')
    } else {
      if (/GLIBC_[A-Z]/.test(elf)) throw Error('Unknown GLIBC requirement')
      for (const [, major, minor, patch] of elf.matchAll(
        /GLIBC_(\d+)\.(\d+)(?:\.(\d+))?/g
      ))
        if (
          +major > 2 ||
          (+major === 2 && (+minor > 34 || (+minor === 34 && +patch > 0)))
        )
          throw Error('GLIBC requirement exceeds 2.34')
    }
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
      if (command === 0x32 || command === 0x24) {
        if (
          minimum !== undefined ||
          size < (command === 0x32 ? 24 : 16) ||
          (command === 0x32 && bytes.readUInt32LE(offset + 8) !== 1)
        )
          throw Error('Invalid macOS deployment command')
        minimum = bytes.readUInt32LE(offset + (command === 0x32 ? 12 : 8))
      }
      offset += size
    }
    if (offset !== end || minimum === undefined || minimum !== 0x000d0000)
      throw Error('Expected macOS minimum OS 13.0')
    console.log('SUCCESS: Mach-O target and macOS minimum OS 13.0:', target)
  }
}

/** Complete converter output with the actual binary's SPDX package. */
export function completeSpdx(document, binary, original) {
  if (
    document.documentDescribes?.length ||
    document.relationships?.length ||
    document.packages.some((entry) => entry.SPDXID === 'SPDXRef-Artifact')
  )
    throw new Error('Unexpected converter root or relationships')
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

/** Check the frozen SPDX root/hash join and preserved package identities. */
export function validateSpdx(document, binary, original) {
  checkBinary(binary)
  if (
    document.spdxVersion !== 'SPDX-2.3' ||
    document.SPDXID !== 'SPDXRef-DOCUMENT'
  )
    throw new Error('Expected an SPDX 2.3 document')
  const packages = document.packages ?? []
  const ids = new Set(packages.map((entry) => entry.SPDXID))
  if (ids.size !== packages.length)
    throw new Error('Duplicate SPDX package identity')
  if ([...ids].some((id) => !/^SPDXRef-[A-Za-z0-9.-]+$/.test(id)))
    throw new Error('Invalid SPDX package identifier')
  const described = new Set(document.documentDescribes ?? [])
  for (const edge of document.relationships ?? []) {
    if (
      edge.spdxElementId === document.SPDXID &&
      edge.relationshipType === 'DESCRIBES'
    )
      described.add(edge.relatedSpdxElement)
  }
  const root = packages.find((entry) => entry.SPDXID === [...described][0])
  if (described.size !== 1 || !root)
    throw new Error('SPDX must describe exactly one root package')
  const hashes = (root.checksums ?? []).filter(
    (hash) => hash.algorithm === 'SHA256'
  )
  if (
    hashes.length !== 1 ||
    hashes[0].checksumValue !==
      createHash('sha256').update(binary).digest('hex')
  )
    throw new Error('SPDX root SHA256 differs from binary')
  const flatten = (items) =>
    items.flatMap((item) => [item, ...flatten(item.components ?? [])])
  const components = [
    original.metadata.component,
    ...flatten(original.components ?? []),
  ]
  const used = new Set()
  for (const component of components) {
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
    if (component === original.metadata.component && match !== root)
      throw new Error('SPDX describes a different inventory root')
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

// Resolve literal macro arguments, including nested concat!, without Rust dataflow claims.
function rustTokens(source) {
  const tokens = []
  for (let i = 0; i < source.length; ) {
    if (/\s/.test(source[i])) {
      i++
      continue
    }
    if (source.startsWith('//', i)) {
      i = source.indexOf('\n', i)
      if (i < 0) break
      continue
    }
    if (source.startsWith('/*', i)) {
      let depth = 1
      i += 2
      while (i < source.length && depth) {
        if (source.startsWith('/*', i)) {
          depth++
          i += 2
        } else if (source.startsWith('*/', i)) {
          depth--
          i += 2
        } else i++
      }
      continue
    }
    const raw = source.slice(i).match(/^r(#{0,255})"/)
    if (raw) {
      const start = i + raw[0].length,
        end = source.indexOf('"' + raw[1], start)
      if (end < 0) throw Error('Unterminated Rust raw string')
      tokens.push({ string: source.slice(start, end) })
      i = end + 1 + raw[1].length
      continue
    }
    const character = source
      .slice(i)
      .match(/^'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\r\n])'/)
    if (character) {
      i += character[0].length
      continue
    }
    if (source[i] === '"') {
      let end = i + 1
      while (end < source.length && source[end] !== '"') {
        if (source[end] === '\\') end++
        end++
      }
      if (end === source.length) throw Error('Unterminated Rust string')
      const value = source
        .slice(i + 1, end)
        .replace(
          /\\(?:x([0-9a-fA-F]{2})|u\{([0-9a-fA-F_]+)\}|(\r?\n)\s*|([nrt0\\"']))/g,
          (_, hex, unicode, newline, simple) => {
            if (hex) return String.fromCodePoint(parseInt(hex, 16))
            if (unicode)
              return String.fromCodePoint(
                parseInt(unicode.replaceAll('_', ''), 16)
              )
            if (newline) return ''
            return { n: '\n', r: '\r', t: '\t', 0: '\0' }[simple] ?? simple
          }
        )
      tokens.push({ string: value })
      i = end + 1
      continue
    }
    const word = source.slice(i).match(/^[A-Za-z_][A-Za-z_0-9]*/)
    if (word) {
      tokens.push(word[0])
      i += word[0].length
    } else tokens.push(source[i++])
  }
  return tokens
}

export function checkSource(file, source) {
  const tokens = rustTokens(source)
  const deployment = (value) => /^(?:TRUSTED_SERVER__|EDGEZERO__)/.test(value)
  // Only release control and Cargo-owned target facts are additional build-time inputs.
  const allowedBuild = new Set([
    'TRUSTED_SERVER__GIT_VERSION',
    'TRUSTED_SERVER__RELEASE_BUILD',
    'TRUSTED_SERVER__BUILD_TARGET',
  ])
  const expression = (start) => {
    if (
      ['env', 'option_env'].includes(tokens[start]) &&
      tokens[start + 1] === '!' &&
      ['(', '[', '{'].includes(tokens[start + 2])
    ) {
      const close = { '(': ')', '[': ']', '{': '}' }[tokens[start + 2]]
      const argument = expression(start + 3)
      if (argument && tokens[argument.end] === close)
        return { value: '__BUILD_ENV__', end: argument.end + 1 }
      return null
    }
    if (tokens[start]?.string !== undefined)
      return { value: tokens[start].string, end: start + 1 }
    if (
      tokens[start] !== 'concat' ||
      tokens[start + 1] !== '!' ||
      !['(', '[', '{'].includes(tokens[start + 2])
    )
      return null
    const close = { '(': ')', '[': ']', '{': '}' }[tokens[start + 2]]
    let cursor = start + 3,
      value = ''
    while (tokens[cursor] !== close) {
      const part = expression(cursor)
      if (!part) return null
      value += part.value
      cursor = part.end
      if (tokens[cursor] === ',') cursor++
      else if (tokens[cursor] !== close) return null
    }
    return { value, end: cursor + 1 }
  }
  const operatorPath = (value) =>
    /(?:^|[/\\])(?:trusted-server\.toml|\.env(?:[./\\]|$))/.test(value)
  for (let i = 0; i < tokens.length; i++) {
    if (
      ['env', 'option_env', 'include_str', 'include_bytes'].includes(
        tokens[i]
      ) &&
      tokens[i + 1] === '!'
    ) {
      const argument = expression(i + 3)
      if (!argument) continue
      const value = argument.value
      if (
        (['env', 'option_env'].includes(tokens[i]) &&
          deployment(value) &&
          !allowedBuild.has(value)) ||
        (tokens[i].startsWith('include_') && operatorPath(value))
      )
        throw Error(`${file}: deployment compile-time input`)
    }
    if (file.endsWith('build.rs')) {
      const value = expression(i)?.value
      if (
        value !== undefined &&
        ((deployment(value) && !allowedBuild.has(value)) || operatorPath(value))
      )
        throw Error(`${file}: operator compile-time build input`)
    }
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
    return typeof value === 'string' &&
      ['bom-ref', 'ref', 'dependsOn', 'provides'].includes(key)
      ? `${prefix}-${createHash('sha256').update(value).digest('hex')}`
      : value
  }
  return rename(bom)
}

export function mergeSboms(cargo, npm, binary, name, version) {
  checkBinary(binary)
  const rust = namespace(cargo, 'cargo')
  const javascript = namespace(npm, 'npm')
  const root = rust.metadata.component
  const npmRoot = javascript.metadata.component
  const dependencies = [
    ...(rust.dependencies ?? []),
    ...(javascript.dependencies ?? []),
  ]
  let edge = dependencies.find((entry) => entry.ref === root['bom-ref'])
  if (!edge) dependencies.push((edge = { ref: root['bom-ref'], dependsOn: [] }))
  edge.dependsOn = [...(edge.dependsOn ?? []), npmRoot['bom-ref']]
  return {
    bomFormat: 'CycloneDX',
    specVersion: '1.5',
    version: 1,
    metadata: {
      ...rust.metadata,
      component: {
        ...root,
        name,
        version: version ?? root.version,
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
            'Selected Cargo packages plus the installed npm build tree used for this release build. Conservative inventory: components may be removed by linking, tree-shaking or minification. External Prebid is not bundled; its installed package is a build-tree entry only. SPDX conversion does not retain dependency relationships. Native C/assembly, system libraries, Rust standard library and compiler runtimes are not completely inventoried. Complete composition is Phase 2.',
        },
      ],
    },
    components: [
      ...(rust.components ?? []),
      ...(root.components ?? []),
      npmRoot,
      ...(javascript.components ?? []),
    ],
    dependencies,
  }
}

function audit(root) {
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    if (['node_modules', 'target', 'dist', '.git'].includes(entry.name))
      continue
    const file = join(root, entry.name)
    if (entry.isDirectory()) audit(file)
    else if (entry.isFile() && file.endsWith('.rs'))
      checkSource(file, readFileSync(file, 'utf8'))
  }
}

async function smokeRelease(file, target) {
  const elf = target.includes('-linux-')
    ? execFileSync(
        'readelf',
        ['--program-headers', '--dynamic', '--version-info', file],
        { encoding: 'utf8', maxBuffer: 8388608 }
      )
    : ''
  checkReleaseFormat(readFileSync(file), target, elf)
  console.log(
    'SUCCESS: final-byte checks',
    target,
    [...new Set(elf.match(/GLIBC_[\w.]+/g) ?? [])].join(' ')
  )
  if (target === 'wasm32-wasip1') return
  const cli = file.split('/').at(-1).startsWith('ts-')
  const expected =
    'ts package=' +
    readFileSync('Cargo.toml', 'utf8').match(
      /\[workspace\.package\][\s\S]*?^version = "([^"]+)"/m
    )[1] +
    ` source=${process.env.SOURCE_COMMIT} target=${target} variant=default`
  const version = (output) => {
    if (output.trim() !== expected) throw Error('Unexpected CLI version')
    console.log('SUCCESS:', output.trim())
  }
  if (cli)
    version(
      execFileSync(resolve(file), ['--version'], {
        encoding: 'utf8',
        timeout: 10000,
      })
    )
  if (!target.includes('-linux-')) return
  const docker = (args, input) =>
    execFileSync('docker', args, {
      input,
      encoding: 'utf8',
      timeout: args[0] === 'pull' ? 180000 : 10000,
      maxBuffer: 8388608,
    })
  const images = target.endsWith('-musl')
    ? [
        'alpine:3.22.2@sha256:4b7ce07002c69e8f3d704a9c5d6fd3053be500b7f1c69fc0d80990c2ad8dd412',
        'scratch',
      ]
    : target.startsWith('aarch64-')
      ? [
          'debian:12-slim@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8',
        ]
      : [
          'debian:12-slim@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8',
          'ubuntu:22.04@sha256:0a9887931b0e38af2e3487ed39e29436b0535c4f3f52e57745baaf79d1898956',
          'registry.access.redhat.com/ubi9/ubi-minimal:9.6@sha256:34880b64c07f28f64d95737f82f891516de9a3b43583f39970f7bf8e4cfa48b7',
        ]
  for (const reference of images) {
    let image
    if (reference === 'scratch')
      image = docker(['import', '-'], Buffer.alloc(1024)).trim()
    else {
      docker(['pull', reference])
      image = docker([
        'image',
        'inspect',
        '--format',
        '{{.Id}}',
        reference,
      ]).trim()
    }
    console.log('Runtime image:', reference, image)
    const restrictions = [
      '--read-only',
      '--cap-drop=ALL',
      '--security-opt=no-new-privileges',
      '--user=65534:65534',
      '--entrypoint=/release',
      '--mount',
      `type=bind,src=${resolve(file)},dst=/release,readonly`,
    ]
    if (!cli)
      restrictions.push(
        '--mount',
        'type=bind,src=/etc/ssl/certs/ca-certificates.crt,dst=/ca.pem,readonly',
        '--env=SSL_CERT_FILE=/ca.pem'
      )
    const container = 'phase1-smoke-' + randomUUID()
    let failed
    try {
      if (cli) {
        version(
          docker([
            'run',
            '--name',
            container,
            '--network=none',
            ...restrictions,
            image,
            '--version',
          ])
        )
        continue
      }
      for (const setting of ['EDGEZERO__ADAPTER__PORT', 'PORT']) {
        let rejected = false
        try {
          docker([
            'run',
            '--rm',
            '--network=none',
            ...restrictions,
            '--env',
            setting + '=0',
            image,
          ])
        } catch (error) {
          if (
            error.status !== 1 ||
            !(String(error.stdout ?? '') + String(error.stderr ?? '')).includes(
              '1–65535'
            )
          )
            throw error
          rejected = true
        }
        if (!rejected) throw Error('Axum accepted port zero: ' + setting)
        console.log(
          'SUCCESS: Axum rejects ' +
            setting +
            '=0; exit 1, ports must be 1–65535:',
          image
        )
      }
      const reservation = createServer().listen(0, '127.0.0.1')
      await once(reservation, 'listening')
      const port = reservation.address().port
      await new Promise((resolve) => reservation.close(resolve))
      // Fixed ASCII fixture keys are sorted for the envelope's canonical hash.
      const data = {
        ec: { passphrase: 'ec_passphrase' },
        handlers: [
          {
            password: 'handler_password',
            path: '^/_ts/admin',
            username: 'smoke',
          },
        ],
        publisher: {
          cookie_domain: '127.0.0.1',
          domain: '127.0.0.1',
          origin_url: 'http://127.0.0.1:9',
          proxy_secret: 'publisher_proxy_secret',
        },
      }
      const envelope = {
        data,
        generated_at: '2026-01-01T00:00:00Z',
        sha256: createHash('sha256').update(JSON.stringify(data)).digest('hex'),
        version: 1,
      }
      const environment = [
        'EDGEZERO__ADAPTER__PORT=8080',
        'EDGEZERO__ADAPTER__HOST=0.0.0.0',
        `TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG=${JSON.stringify(envelope)}`,
        'TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_EC_PASSPHRASE=smoke-edge-cookie-passphrase-32-bytes',
        'TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_HANDLER_PASSWORD=smoke-admin-password-32-bytes-ok',
        'TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_PUBLISHER_PROXY_SECRET=smoke-publisher-proxy-secret-32-bytes',
      ]
      docker([
        'run',
        '--name',
        container,
        '-d',
        '--publish',
        `127.0.0.1:${port}:8080`,
        ...restrictions,
        ...environment.flatMap((value) => ['--env', value]),
        image,
      ])
      let healthy = false
      for (let attempt = 0; attempt < 30; attempt++) {
        if (
          docker([
            'inspect',
            '--format',
            '{{.State.Running}}',
            container,
          ]).trim() !== 'true'
        )
          throw Error('Axum exited before readiness')
        let response
        try {
          response = await fetch(`http://127.0.0.1:${port}/health`, {
            signal: AbortSignal.timeout(1000),
          })
        } catch {
          await new Promise((resolve) => setTimeout(resolve, 100))
          continue
        }
        const body = await response.text()
        if (response.status !== 200 || body !== 'ok')
          throw Error(
            `Axum health mismatch: HTTP ${response.status}: ${body.slice(0, 500)}`
          )
        healthy = true
        break
      }
      if (!healthy) throw Error('Axum startup deadline exhausted')
      console.log(
        'SUCCESS: Axum bound to 0.0.0.0:8080; host reached published container port, HTTP 200 ok:',
        reference,
        image
      )
    } catch (error) {
      failed = error
      try {
        console.error(docker(['logs', container]).slice(-4000))
      } catch {}
      throw error
    } finally {
      try {
        docker(['rm', '-f', container])
      } catch (error) {
        if (
          !error.stderr?.toString().includes(`No such container: ${container}`)
        ) {
          if (!failed) throw error
          console.error('Smoke container cleanup failed:', error.message)
        }
      }
    }
  }
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
) {
  const [command, ...args] = process.argv.slice(2)
  if (command === 'audit' && args.length === 0) {
    audit('crates')
  } else if (command === 'check' && args.length > 0) {
    for (const file of args) {
      if (!statSync(file).isFile())
        throw new Error('Expected regular release binary')
      checkBinary(readFileSync(file))
    }
  } else if (command === 'smoke' && args.length === 2) {
    await smokeRelease(...args)
  } else if (command === 'merge' && args.length === 4) {
    const [cargo, npm, binary, output] = args
    writeFileSync(
      output,
      JSON.stringify(
        mergeSboms(
          JSON.parse(readFileSync(cargo)),
          JSON.parse(readFileSync(npm)),
          readFileSync(binary),
          binary.split('/').at(-1),
          process.env.RELEASE_VERSION
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
      'Expected audit, check <binary...>, merge <cargo> <npm> <binary> <output>, or validate-spdx <spdx> <binary> <merged-cdx>'
    )
  }
}
