/** Small Phase 1 build-inventory and configuration checks, without release authority. */
import { createHash } from 'node:crypto'
import { readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { pathToFileURL } from 'node:url'

/** Complete converter output with the actual binary's SPDX package. */
export function completeSpdx(document, binary, original) {
  if (
    document.documentDescribes?.length ||
    document.relationships?.length ||
    document.packages.some((entry) => entry.SPDXID === 'SPDXRef-Artifact')
  )
    throw new Error('Unexpected converter root or relationships')
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

export function checkSource(file, source) {
  const macros = source.matchAll(/\b(?:option_env|env)!\s*\(\s*"([^"]+)"/g)
  for (const [, variable] of macros) {
    if (
      /^(?:TRUSTED_SERVER__|EDGEZERO__)/.test(variable) &&
      variable !== 'TRUSTED_SERVER__GIT_VERSION'
    ) {
      throw new Error(`${file}: deployment compile-time environment input`)
    }
  }
  if (
    /\binclude_(?:str|bytes)!\s*\([^)]*"[^"\n]*(?:trusted-server\.toml|\.env(?:[."]))/s.test(
      source
    ) ||
    (file.endsWith('build.rs') &&
      /["'][^"'\n]*trusted-server\.toml["']/.test(source))
  ) {
    throw new Error(`${file}: operator compile-time file input`)
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

export function mergeSboms(cargo, npm, binary, name) {
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
