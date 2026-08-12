/** Build the phase-aware, content-addressed TSJS release from its canonical catalog. */

import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { transform } from 'esbuild';
import { build } from 'vite';

import {
  deriveInventorySetFiles,
  measureBundleSet,
  measureBytes,
  BUNDLE_SEPARATOR,
} from './scripts/bundle-metrics.mjs';
import { computeReleaseId, RELEASE_SENTINEL, stampRelease } from './scripts/release-v1.mjs';

const libDirectory = path.dirname(fileURLToPath(import.meta.url));
const sourceDirectory = path.join(libDirectory, 'src');
const distributionDirectory = path.resolve(libDirectory, '..', 'dist');
const metricsFile = 'tsjs-build-metrics-v1.json';
const releaseFile = 'tsjs-release-v1.json';
const catalogFile = 'tsjs-catalog-v1.json';
const bootstrapFile = 'gpt-bootstrap-fallback.js';

fs.rmSync(distributionDirectory, { recursive: true, force: true });
fs.mkdirSync(distributionDirectory, { recursive: true });

// Build the TypeScript catalog itself as a temporary Node module. This keeps the
// browser, release builder, and generated Rust metadata on one authored authority.
const catalogModuleFile = '.release-catalog-v1.mjs';
const catalogSource = fs.readFileSync(
  path.join(sourceDirectory, 'kernel', 'release_catalog.ts'),
  'utf8'
);
const transformedCatalog = await transform(catalogSource, {
  format: 'esm',
  loader: 'ts',
  target: 'es2020',
});
fs.writeFileSync(path.join(distributionDirectory, catalogModuleFile), transformedCatalog.code);
const catalogModule = await import(
  `${pathToFileURL(path.join(distributionDirectory, catalogModuleFile)).href}?build=${Date.now()}`
);
const releaseCatalog = catalogModule.RELEASE_CATALOG;
catalogModule.validateReleaseCatalog(releaseCatalog);
if (releaseCatalog.length !== 20) throw new Error('[build-all] Catalog must contain 20 rows');
const runtimeCatalog = releaseCatalog.map(({ id, phase, trigger, consumes, provides }) => ({
  id,
  phase,
  trigger,
  consumes,
  provides,
}));
fs.rmSync(path.join(distributionDirectory, catalogModuleFile));

const sourceById = Object.freeze({
  render_runtime: 'integrations/render_runtime/index.ts',
  aps: 'integrations/aps/index.ts',
  creative: 'integrations/creative/index.ts',
  datadome: 'integrations/datadome/index.ts',
  didomi: 'integrations/didomi/index.ts',
  google_tag_manager: 'integrations/google_tag_manager/index.ts',
  gpt: 'integrations/gpt/index.ts',
  gpt_diagnostics: 'integrations/gpt_diagnostics/index.ts',
  lockr: 'integrations/lockr/index.ts',
  osano_consent: 'integrations/osano/consent.ts',
  permutive_context: 'integrations/permutive/context.ts',
  sourcepoint_consent: 'integrations/sourcepoint/consent.ts',
  prebid: 'integrations/prebid/index.ts',
  testlight: 'integrations/testlight/index.ts',
  diagnostics_presentation: 'integrations/gpt_diagnostics/presentation.ts',
  gpt_later: 'integrations/gpt/later.ts',
  osano_lifecycle: 'integrations/osano/lifecycle.ts',
  permutive_lifecycle: 'integrations/permutive/lifecycle.ts',
  prebid_later: 'integrations/prebid/later.ts',
  sourcepoint_lifecycle: 'integrations/sourcepoint/lifecycle.ts',
});

const catalogIds = releaseCatalog.map(({ id }) => id);
if (
  Object.keys(sourceById).length !== releaseCatalog.length ||
  catalogIds.some((id) => !(id in sourceById))
) {
  throw new Error('[build-all] Catalog/source inventory mismatch');
}
if (new Set(Object.values(sourceById)).size !== releaseCatalog.length) {
  throw new Error('[build-all] Every catalog artifact must have one distinct source entry');
}

const artifacts = [
  {
    id: 'bootstrap',
    role: 'bootstrap',
    phase: '',
    trigger: '',
    inputs: [],
    outputs: [],
    file: bootstrapFile,
    entry: 'integrations/gpt/bootstrap_fallback.ts',
  },
  {
    id: 'core',
    role: 'core',
    phase: '',
    trigger: '',
    inputs: [],
    outputs: ['runtime.v1'],
    file: 'tsjs-core.js',
    entry: 'composition/index.ts',
  },
  ...releaseCatalog.map((entry) => ({
    id: entry.id,
    role: 'integration',
    phase: entry.phase,
    trigger: entry.trigger ?? '',
    inputs: [...entry.consumes],
    outputs: [...entry.provides],
    file: `tsjs-${entry.id}.js`,
    entry: sourceById[entry.id],
  })),
];

const ids = new Set();
const files = new Set();
for (const artifact of artifacts) {
  if (ids.has(artifact.id) || files.has(artifact.file)) {
    throw new Error(`[build-all] Duplicate artifact: ${artifact.id}`);
  }
  if (/(?:^|\/)(?:test|fixtures?|fakes?|no-?op)(?:\/|$)/iu.test(artifact.entry)) {
    throw new Error(`[build-all] Test/fake/no-op artifact source: ${artifact.entry}`);
  }
  ids.add(artifact.id);
  files.add(artifact.file);
}

async function buildArtifact(artifact) {
  const entryPath = path.join(sourceDirectory, artifact.entry);
  if (!fs.existsSync(entryPath)) throw new Error(`[build-all] Missing source: ${artifact.entry}`);
  console.log(`[build-all] Building ${artifact.file} from ${artifact.entry}`);
  const result = await build({
    configFile: false,
    root: libDirectory,
    define: {
      __TSJS_EMBEDDED_RELEASE_ID_V1__: JSON.stringify(RELEASE_SENTINEL),
      __TSJS_EMBEDDED_INTEGRATION_IDS_V1__: JSON.stringify(catalogIds),
      __TSJS_EMBEDDED_RUNTIME_CATALOG_V1__: JSON.stringify(runtimeCatalog),
    },
    build: {
      emptyOutDir: false,
      outDir: distributionDirectory,
      assetsDir: '.',
      sourcemap: false,
      minify: 'esbuild',
      rollupOptions: {
        input: entryPath,
        output: {
          format: 'iife',
          dir: distributionDirectory,
          entryFileNames: artifact.file,
          extend: false,
          name: `tsjs_${artifact.id}`,
        },
      },
    },
    logLevel: 'warn',
  });

  const outputs = (Array.isArray(result) ? result : [result]).flatMap((item) => item.output);
  const chunk = outputs.find((item) => item.type === 'chunk' && item.fileName === artifact.file);
  if (!chunk || chunk.type !== 'chunk') {
    throw new Error(`[build-all] Missing generated chunk metadata: ${artifact.file}`);
  }
  artifact.moduleIds = Object.freeze(
    Object.keys(chunk.modules).map((moduleId) => path.relative(libDirectory, moduleId))
  );
  artifact.moduleContributions = Object.freeze(
    Object.entries(chunk.modules).map(([moduleId, contribution]) => ({
      file: path.relative(libDirectory, moduleId),
      renderedBytes: contribution.renderedLength,
    }))
  );

  const filePath = path.join(distributionDirectory, artifact.file);
  const source = fs.readFileSync(filePath, 'utf8');
  const sentinelCount = source.split(RELEASE_SENTINEL).length - 1;
  if (sentinelCount > 1) {
    throw new Error(`[build-all] Multiple release sentinels before stamping: ${artifact.file}`);
  }
  if (sentinelCount === 0) fs.writeFileSync(filePath, `${source}\n;void"${RELEASE_SENTINEL}";\n`);
}

await buildArtifact(artifacts[0]);
await buildArtifact(artifacts[1]);
await Promise.all(artifacts.slice(2).map(buildArtifact));

const deferredEntries = new Set(
  artifacts
    .filter(({ phase }) => phase === 'deferred')
    .map(({ entry }) => path.normalize(`src/${entry}`))
);
for (const artifact of artifacts) {
  if (artifact.role !== 'core' && artifact.phase !== 'critical') continue;
  const reachedDeferred = artifact.moduleIds.find((moduleId) =>
    deferredEntries.has(path.normalize(moduleId))
  );
  if (reachedDeferred) {
    throw new Error(`[build-all] ${artifact.id} reaches deferred source entry ${reachedDeferred}`);
  }
}
const generatedJavaScript = fs
  .readdirSync(distributionDirectory)
  .filter((file) => file.endsWith('.js'));
const expectedJavaScript = artifacts.map(({ file }) => file);
if (
  generatedJavaScript.length !== expectedJavaScript.length ||
  expectedJavaScript.some((file) => !generatedJavaScript.includes(file))
) {
  throw new Error('[build-all] Missing or unknown production JavaScript artifact');
}

const releaseId = computeReleaseId(
  artifacts.map((artifact) => ({
    id: artifact.id,
    role: artifact.role,
    phase: artifact.phase,
    trigger: artifact.trigger,
    bytes: fs.readFileSync(path.join(distributionDirectory, artifact.file)),
  }))
);

for (const artifact of artifacts) {
  const filePath = path.join(distributionDirectory, artifact.file);
  fs.writeFileSync(filePath, stampRelease(fs.readFileSync(filePath), releaseId));
}

const artifactInventory = artifacts.map((artifact) => {
  const bytes = fs.readFileSync(path.join(distributionDirectory, artifact.file));
  return {
    id: artifact.id,
    role: artifact.role,
    phase: artifact.phase || null,
    trigger: artifact.trigger || null,
    inputs: artifact.inputs,
    outputs: artifact.outputs,
    file: artifact.file,
    bytes: bytes.byteLength,
    hash: createHash('sha256').update(bytes).digest('hex'),
  };
});
fs.writeFileSync(
  path.join(distributionDirectory, releaseFile),
  `${JSON.stringify({ version: 1, releaseId, artifacts: artifactInventory })}\n`
);
fs.writeFileSync(
  path.join(distributionDirectory, catalogFile),
  `${JSON.stringify({
    version: 1,
    modules: releaseCatalog.map(({ id, phase, trigger, include }) => ({
      id,
      phase,
      trigger,
      include,
    })),
  })}\n`
);

const bootstrapArtifact = artifacts[0];
const bootstrapBytes = fs.readFileSync(path.join(distributionDirectory, bootstrapFile));
const artifactContents = new Map(
  artifactInventory.map(({ file }) => [
    file,
    fs.readFileSync(path.join(distributionDirectory, file)),
  ])
);
const inventorySetFiles = deriveInventorySetFiles(artifactInventory, releaseCatalog);
const metrics = {
  schemaVersion: 1,
  compression: {
    concatenationSeparator: BUNDLE_SEPARATOR.toString('utf8'),
    gzipLevel: 9,
    gzipMtime: 0,
    brotliMode: 'text',
    brotliQuality: 11,
  },
  modules: artifacts.slice(1).map((artifact) => {
    const bytes = fs.readFileSync(path.join(distributionDirectory, artifact.file));
    return {
      file: artifact.file,
      entry: path.normalize(`src/${artifact.entry}`),
      rawBytes: bytes.byteLength,
      sha256: createHash('sha256').update(bytes).digest('hex'),
      sources: artifact.moduleContributions,
    };
  }),
  bootstrap: {
    file: bootstrapFile,
    entry: path.normalize(`src/${bootstrapArtifact.entry}`),
    ...measureBytes(bootstrapBytes),
    sources: bootstrapArtifact.moduleContributions,
  },
  sets: Object.fromEntries(
    Object.entries(inventorySetFiles).map(([setName, setFiles]) => [
      setName,
      measureBundleSet(setFiles, artifactContents),
    ])
  ),
};
fs.writeFileSync(
  path.join(distributionDirectory, metricsFile),
  `${JSON.stringify(metrics, null, 2)}\n`
);

console.log(`[build-all] Built ${artifacts.length} canonical artifacts`);
console.log(`[build-all] Wrote ${releaseFile} for release ${releaseId}`);
