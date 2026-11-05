/**
 * Produce conservative post-Rollup contributions and final post-esbuild hashes.
 * Later minification can eliminate recorded code; rendered_length is not final
 * byte attribution. This producer does not prove authenticated acquisition,
 * lifecycle execution, host qualification or A/B equality. Preparation
 * observations and execution boundary qualification remain separate requirements.
 */
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { execFileSync } from 'node:child_process'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { buildAll } from '../../crates/trusted-server-js/lib/build-all.mjs'
import {
  actualToolFiles,
  installedOrigins,
  installedMaterials,
  validatePreparation,
  releaseBuild,
  readBounded,
  strictJson,
  sha256,
} from '../../crates/trusted-server-js/lib/release-composition.mjs'

const here = path.dirname(fileURLToPath(import.meta.url))
const defaultRoot = path.resolve(here, '../..')
const MAX_BUNDLE = 32 * 1024 * 1024

/** Discover modules from the selected source identity. */
export function expectedModuleIds(repoRoot, sourceCommit, sourceContextPath) {
  if (!/^[a-f0-9]{40}$/.test(sourceCommit ?? ''))
    throw new Error('Expected full source commit')
  if (sourceContextPath) {
    const file = fs.realpathSync(sourceContextPath)
    if (file === repoRoot || file.startsWith(`${repoRoot}${path.sep}`))
      throw new Error('Source context must be outside the writable checkout')
    const context = strictJson(readBounded(file))
    if (
      !context ||
      typeof context !== 'object' ||
      Array.isArray(context) ||
      Object.keys(context).sort().join(',') !==
        'module_ids,schema_version,source_commit' ||
      context.schema_version !== 1 ||
      context.source_commit !== sourceCommit ||
      !Array.isArray(context.module_ids) ||
      !context.module_ids.includes('core') ||
      context.module_ids.some(
        (id) => typeof id !== 'string' || !/^[a-z][a-z0-9_-]*$/.test(id)
      ) ||
      context.module_ids.some(
        (id, index) => index > 0 && context.module_ids[index - 1] >= id
      )
    )
      throw new Error('Invalid parent-derived source context')
    // The parent derives this list from authenticated objects and mounts it
    // read-only. Parsing it alone does not establish source/build completion.
    return context.module_ids
  }
  const gitEnvironment = {
    PATH: process.env.PATH,
    TMPDIR: os.tmpdir(),
    LANG: 'C',
    LC_ALL: 'C',
    GIT_CONFIG_NOSYSTEM: '1',
    GIT_CONFIG_GLOBAL: '/dev/null',
    GIT_NO_REPLACE_OBJECTS: '1',
    GIT_OPTIONAL_LOCKS: '0',
  }
  const raw = execFileSync(
    'git',
    ['ls-tree', '-rz', '--name-only', sourceCommit],
    {
      cwd: repoRoot,
      env: gitEnvironment,
      maxBuffer: 1024 * 1024,
    }
  )
  const tracked = new TextDecoder('utf-8', { fatal: true })
    .decode(raw)
    .split('\0')
    .filter(
      (file) =>
        file === 'crates/trusted-server-js/lib/src/core/index.ts' ||
        /^crates\/trusted-server-js\/lib\/src\/integrations\/[^/]+\/index\.ts$/.test(
          file
        )
    )
  const expected = tracked
    .map((file) =>
      file.endsWith('/core/index.ts') ? 'core' : file.split('/').at(-2)
    )
    .sort()
  if (
    !expected.includes('core') ||
    expected.some((id) => !/^[a-z][a-z0-9_-]*$/.test(id)) ||
    new Set(expected).size !== expected.length
  )
    throw new Error('Invalid tracked module identities')
  return expected
}

/** Build release bundles using checked observations from independent preparation. */
export async function collectJs({
  repoRoot = defaultRoot,
  outDir,
  inventoryPath,
  preparationPath,
  sourceContextPath,
}) {
  if (!preparationPath)
    throw new Error('Checked preparation records are required')
  if (!releaseBuild())
    throw new Error('Collector requires the existing release build control')
  repoRoot = fs.realpathSync(repoRoot)
  if (repoRoot !== fs.realpathSync(defaultRoot))
    throw new Error('Collector must execute from its own checkout')
  const sourceCommit = process.env.TRUSTED_SERVER__GIT_VERSION
  const libDir = path.join(repoRoot, 'crates/trusted-server-js/lib')
  const lockFile = path.join(libDir, 'package-lock.json')
  const owners = installedOrigins({ repoRoot, lockFile })
  const packages = installedMaterials(owners)
  const strictPreparation = strictJson(readBounded(preparationPath))
  const prepared = validatePreparation(strictPreparation, {
    installRoot: libDir,
    packages,
  })
  for (const actual of actualToolFiles({ installRoot: libDir })) {
    const tool = prepared.tools.find(
      (record) => record.name === actual.name && record.role === actual.role
    )
    const material = prepared.materials.find(
      (record) => record.id === tool?.material_id
    )
    const distribution = strictPreparation.distributions.find(
      (record) => record.material.id === tool?.material_id
    )
    if (
      !tool ||
      tool.version !== actual.version ||
      material.sha256 !== actual.sha256 ||
      fs.realpathSync(distribution.path) !== fs.realpathSync(actual.path)
    )
      throw new Error(
        `Actual build tool digest or identity mismatch: ${actual.name}`
      )
  }
  fs.mkdirSync(path.resolve(outDir), { recursive: true })
  outDir = fs.realpathSync(path.resolve(outDir))
  inventoryPath = path.join(
    fs.realpathSync(path.dirname(path.resolve(inventoryPath))),
    path.basename(inventoryPath)
  )
  if (
    inventoryPath === outDir ||
    inventoryPath.startsWith(`${outDir}${path.sep}`)
  )
    throw new Error('Inventory must be outside the bundle directory')
  const expected = expectedModuleIds(repoRoot, sourceCommit, sourceContextPath)
  const records = await buildAll({
    outDir,
    sourceDir: path.join(libDir, 'src'),
    composition: { repoRoot, lockFile, owners },
  })
  if (
    JSON.stringify(records.map((record) => record.id)) !==
    JSON.stringify(expected)
  )
    throw new Error('Discovered module set differs from tracked source')
  const actualFiles = fs.readdirSync(outDir).sort()
  if (
    JSON.stringify(actualFiles) !==
    JSON.stringify(expected.map((id) => `tsjs-${id}.js`).sort())
  )
    throw new Error('Unexpected bundle files')
  const modules = records.map((record) => {
    const bytes = readBounded(path.join(outDir, record.filename), MAX_BUNDLE)
    if (!bytes.length) throw new Error('Empty release bundle')
    new TextDecoder('utf-8', { fatal: true }).decode(bytes)
    return {
      id: record.id,
      filename: record.filename,
      size: bytes.length,
      sha256: sha256(bytes),
      contributions: record.contributions,
    }
  })
  const sources = [
    'package.json',
    'package-lock.json',
    'build-all.mjs',
    'build-module-options.mjs',
    'release-composition.mjs',
    'vitest.config.ts',
    'tsconfig.json',
  ].map((file) => `crates/trusted-server-js/lib/${file}`)
  sources.push('scripts/release/collect-js.mjs')
  sources.push('scripts/release/observe-npm.mjs')
  sources.push(
    'scripts/release/build-js.sh',
    'scripts/release/bootstrap.sh',
    'crates/trusted-server-release/Cargo.toml',
    'crates/trusted-server-release/src/main.rs'
  )
  const materials = [
    ...prepared.materials,
    ...sources.map((file) => ({
      kind: 'source',
      id: `source:${file}`,
      path: file,
      sha256: sha256(readBounded(path.join(repoRoot, file))),
    })),
  ].sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0))
  if (
    new Set(materials.map((material) => material.id)).size !== materials.length
  )
    throw new Error('Duplicate combined material identity')
  const inventory = {
    schema_version: 1,
    source_commit: sourceCommit,
    modules,
    tools: prepared.tools,
    materials,
  }
  const bytes = Buffer.from(`${JSON.stringify(inventory)}\n`)
  if (bytes.length > 1024 * 1024) throw new Error('Inventory exceeds 1 MiB')
  fs.writeFileSync(inventoryPath, bytes, { flag: 'wx', mode: 0o600 })
  return inventory
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(fs.realpathSync(process.argv[1])).href
) {
  const args = process.argv.slice(2)
  if (
    ![6, 8].includes(args.length) ||
    args[0] !== '--out-dir' ||
    args[2] !== '--inventory' ||
    args[4] !== '--preparation' ||
    (args.length === 8 && args[6] !== '--source-context')
  )
    throw new Error(
      'Usage: collect-js.mjs --out-dir <dir> --inventory <file> --preparation <file> [--source-context <file>]'
    )
  await collectJs({
    outDir: args[1],
    inventoryPath: args[3],
    preparationPath: args[5],
    sourceContextPath: args[7],
  })
}
