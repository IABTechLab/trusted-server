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
  measureReachableFirstDisplayMasks,
  BUNDLE_SEPARATOR,
} from './scripts/bundle-metrics.mjs';
import { computeReleaseId, RELEASE_SENTINEL, stampRelease } from './scripts/release-v1.mjs';

const libDirectory = path.dirname(fileURLToPath(import.meta.url));
const sourceDirectory = path.join(libDirectory, 'src');
const distributionDirectory = path.resolve(libDirectory, '..', 'dist');
const metricsFile = 'tsjs-build-metrics-v1.json';
const releaseFile = 'tsjs-release-v1.json';
const catalogFile = 'tsjs-catalog-v1.json';
const bootstrapFile = 'tsjs-bootstrap.js';

// Closure-private implementation names used only inside the inline artifact.
// Registration, takeover, handoff, and public API protocol keys are excluded.
const bootstrapPrivateProperties =
  /^(?:stateValue|registrations|disposers|authenticated|reject|unwind)$/;

// These properties are closure-private implementation details of the base agent.
// Mangle only that artifact: protocol, registration, handoff, and public agent keys
// intentionally keep their authored names across independently built components.
const firstDisplayBasePrivateProperties =
  /^(?:options|stateValue|agentBatch|slotResults|handoffOwner|mutationObserver|observedMutationRevision|displayWasCommitted|sealed|failed|pending|reasons|actionStarted|disposedDriver|handoffFinalized|committedArtifactsDetached|lastTimingMs|firstActionAtMs|terminalAtMs|paintAtMs|nextTraceSequence|acceptedTrace|recordTerminal|recordFirstAction|scheduleProtectedPaint|readTiming|captureHandoffData|disposeDriver|installNativeMutationIngress|observeDomMutations|isOwnedRuntimeInsertion|closeNativeMutationIngress|disposeNativeMutationIngress|claimTimer|completionTimer|controlRelease|directFrame|documentAccepted|documentAcceptancePending|documentRelease|documentTimer|documentTransferred|insertionTimer|pendingDocumentTerminal|ownerSource|ownerTicket|rendererNonce|phaseValue|registryState|expiresAtInternal|ordinalInternal|controlPort|claim|gam|inserted|ticket|active|cycle|onTerminal|reservationId|timer|attempt|recordFailure)$/;

// APS adds one private render state machine inside its independently built slice.
// These fields never enter registration, message, or handoff objects.
const firstDisplayApsPrivateProperties =
  /^(?:options|stateValue|agentBatch|slotResults|handoffOwner|mutationObserver|observedMutationRevision|displayWasCommitted|sealed|failed|pending|reasons|actionStarted|disposedDriver|handoffFinalized|committedArtifactsDetached|lastTimingMs|firstActionAtMs|terminalAtMs|paintAtMs|nextTraceSequence|acceptedTrace|recordTerminal|recordFirstAction|scheduleProtectedPaint|readTiming|captureHandoffData|disposeDriver|installNativeMutationIngress|observeDomMutations|isOwnedRuntimeInsertion|closeNativeMutationIngress|disposeNativeMutationIngress|claimTimer|completionTimer|controlRelease|directFrame|documentAccepted|documentAcceptancePending|documentRelease|documentTimer|documentTransferred|insertionTimer|pendingDocumentTerminal|ownerSource|ownerTicket|rendererNonce|phaseValue|registryState|expiresAtInternal|ordinalInternal|controlPort|claim|gam|inserted|ticket|active|cycle|onTerminal|reservationId|timer|attempt|recordFailure|hostPositionOwned|previousHostPosition|previousHostPositionPriority|frameAttributes|frameContentWindow|frameSource|frameSourceDocument|bootstrapNavigated|bootstrapPolicy|bootstrapSource|originalCount|isReservationId|isLifecycleTicket|isBootstrapNonce|isRendererNonce|parseDocumentMessage|rendererUrl|sandbox|permanentSandbox|deadlines|insertionMs|documentAcceptanceMs|completionMs|ownerSettlementMs)$/;

// These names are private to the GPT initial IIFE and never cross its protocol
// receipt or handoff boundary. Keep the cross-artifact protocol keys authored.
const firstDisplayGptPrivateProperties =
  /^(?:options|binding|command|cycleMap|createdSlots|diagnosticFacts|diagnosticListeners|targetingObservers|targetingRestorers|publisherCallRestorers|timers|service|started|disposed|ingressClosed|firstAction|commandQueue|commandQueueIndex|createdBinding|renderListener|requestedListener|committedSlotsDetached|detachedSlots|diagnosticFactOverflow|diagnosticFactDrops|targetingWriteDepth|ensureBinding|restoreCreatedBinding|installListeners|removeListener|removePendingCommand|failCycle|failRows|journalPublisherTargeting|observePublisherTargeting|observePublisherCalls|restorePublisherCalls|restoreTargetingObservers|restorePublisherTargeting|notifyNativeMutation|captureDiagnosticFact|clearOwnedTimer|incrementDiagnosticDrops|writeTargeting|invalidateTargeting|sealedTargetingOwnership|targetingWrites|ingressClosing|diagnosticRecords|nextDiagnosticCycleOrdinal|requestTimer|completionTimer|requestOperation|requestInvoked|bindingState|operations|requested|settled|encodedBytes|readPhysicalElementId|retireCycle|snapshotDestroyedCycles|invalidateCyclesForElement|invalidateCyclesForPublisherCall|consumeTargetingWrite|invalidateStaleTargetingObservers|captureRetainedTargeting|timer)$/;

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
const firstDisplayCatalog = catalogModule.FIRST_DISPLAY_CATALOG;
catalogModule.validateReleaseCatalog(releaseCatalog);
if (releaseCatalog.length !== 20) throw new Error('[build-all] Catalog must contain 20 rows');
if (firstDisplayCatalog.length !== 13 || firstDisplayCatalog[0]?.id !== 'first_display') {
  throw new Error('[build-all] First-display catalog must contain its base and twelve slices');
}
const runtimeCatalog = releaseCatalog.map(({ id, phase, trigger, config, consumes, provides }) => ({
  id,
  phase,
  trigger,
  config,
  consumes,
  provides,
}));
fs.rmSync(path.join(distributionDirectory, catalogModuleFile));

const sourceById = Object.freeze({
  render_runtime: 'integrations/render_runtime/transport_marker.ts',
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

const firstDisplaySourceById = Object.freeze({
  first_display: 'first_display/agent.ts',
  aps_initial: 'first_display/slices/aps.ts',
  creative_initial: 'first_display/slices/creative.ts',
  datadome_initial: 'first_display/slices/datadome.ts',
  didomi_initial: 'first_display/slices/didomi.ts',
  google_tag_manager_initial: 'first_display/slices/google_tag_manager.ts',
  gpt_initial: 'first_display/slices/gpt.ts',
  lockr_initial: 'first_display/slices/lockr.ts',
  osano_initial: 'first_display/slices/osano.ts',
  permutive_initial: 'first_display/slices/permutive.ts',
  sourcepoint_initial: 'first_display/slices/sourcepoint.ts',
  prebid_initial: 'first_display/slices/prebid.ts',
  testlight_initial: 'first_display/slices/testlight.ts',
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
if (
  Object.keys(firstDisplaySourceById).length !== firstDisplayCatalog.length ||
  firstDisplayCatalog.some(({ id }) => !(id in firstDisplaySourceById)) ||
  new Set(Object.values(firstDisplaySourceById)).size !== firstDisplayCatalog.length
) {
  throw new Error('[build-all] First-display catalog/source inventory mismatch');
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
    entry: 'core/bootstrap.ts',
  },
  ...firstDisplayCatalog.map((entry, index) => ({
    id: entry.id,
    role: index === 0 ? 'first_display_base' : 'first_display_slice',
    phase: 'first_display',
    trigger: '',
    inputs: [...entry.inputs],
    outputs: [...entry.outputs],
    file: `tsjs-${entry.id}.js`,
    entry: firstDisplaySourceById[entry.id],
    maskBit: index,
  })),
  {
    id: 'core',
    role: 'core',
    phase: '',
    trigger: '',
    inputs: [],
    outputs: ['runtime.v1'],
    file: 'tsjs-core.js',
    entry: 'composition/runtime_transport.ts',
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
    ...(artifact.role === 'bootstrap'
      ? { esbuild: { mangleProps: bootstrapPrivateProperties } }
      : artifact.id === 'first_display'
        ? { esbuild: { mangleProps: firstDisplayBasePrivateProperties } }
        : artifact.id === 'aps_initial'
          ? { esbuild: { mangleProps: firstDisplayApsPrivateProperties } }
          : artifact.id === 'gpt_initial'
            ? { esbuild: { mangleProps: firstDisplayGptPrivateProperties } }
            : {}),
    define: {
      __TSJS_EMBEDDED_RELEASE_ID_V1__: JSON.stringify(RELEASE_SENTINEL),
      __TSJS_EMBEDDED_INTEGRATION_IDS_V1__: JSON.stringify(catalogIds),
      __TSJS_EMBEDDED_RUNTIME_CATALOG_V1__: JSON.stringify(runtimeCatalog),
      __TSJS_EMBEDDED_MAX_MANIFEST_MODULES_V1__: JSON.stringify(releaseCatalog.length),
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
await Promise.all(artifacts.slice(1).map(buildArtifact));

const deferredEntries = new Set(
  artifacts
    .filter(({ phase }) => phase === 'deferred')
    .map(({ entry }) => path.normalize(`src/${entry}`))
);
for (const artifact of artifacts) {
  if (artifact.role !== 'core' && artifact.phase !== 'takeover') continue;
  const reachedDeferred = artifact.moduleIds.find((moduleId) =>
    deferredEntries.has(path.normalize(moduleId))
  );
  if (reachedDeferred) {
    throw new Error(`[build-all] ${artifact.id} reaches deferred source entry ${reachedDeferred}`);
  }
}
const persistentEntryPrefixes = Object.freeze([
  'src/core/',
  'src/kernel/integration_registry.ts',
  'src/kernel/runtime.ts',
  'src/kernel/sessions.ts',
  'src/services/',
  'src/integrations/',
]);
for (const artifact of artifacts.filter(({ phase }) => phase === 'first_display')) {
  const catalogEntry = firstDisplayCatalog.find(({ id }) => id === artifact.id);
  if (!catalogEntry)
    throw new Error(`[build-all] Missing first-display catalog row: ${artifact.id}`);
  const ownEntry = path.normalize(`src/${artifact.entry}`);
  const allowed = new Set(
    catalogEntry.allowedImports.map((moduleId) => path.normalize(`src/${moduleId}.ts`))
  );
  const forbidden = artifact.moduleIds.find((moduleId) =>
    persistentEntryPrefixes.some(
      (prefix) =>
        path.normalize(moduleId).startsWith(path.normalize(prefix)) &&
        !allowed.has(path.normalize(moduleId))
    )
  );
  if (forbidden) {
    throw new Error(`[build-all] ${artifact.id} reaches persistent source ${forbidden}`);
  }
  const undeclared = artifact.moduleIds.find((moduleId) => {
    const normalized = path.normalize(moduleId);
    return normalized !== ownEntry && !allowed.has(normalized);
  });
  if (undeclared) {
    throw new Error(
      `[build-all] ${artifact.id} reaches undeclared first-display source ${undeclared}`
    );
  }
}
for (const artifact of artifacts.filter(
  ({ role, phase }) => role === 'core' || phase === 'takeover' || phase === 'deferred'
)) {
  const forbidden = artifact.moduleIds.find((moduleId) =>
    path.normalize(moduleId).startsWith(path.normalize('src/first_display/'))
  );
  if (forbidden) {
    throw new Error(`[build-all] ${artifact.id} reaches first-display source ${forbidden}`);
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
const bootstrapArtifact = artifacts[0];
const bootstrapBytes = fs.readFileSync(path.join(distributionDirectory, bootstrapFile));
const artifactContents = new Map(
  artifactInventory.map(({ file }) => [
    file,
    fs.readFileSync(path.join(distributionDirectory, file)),
  ])
);
const inventorySetFiles = deriveInventorySetFiles(artifactInventory, releaseCatalog);
const firstDisplayMaskCatalog = firstDisplayCatalog.map(({ id }, maskBit) => ({
  id,
  maskBit,
  file: `tsjs-${id}.js`,
}));
const firstDisplayMasks = await measureReachableFirstDisplayMasks(
  firstDisplayMaskCatalog,
  artifactContents
);
fs.writeFileSync(
  path.join(distributionDirectory, catalogFile),
  `${JSON.stringify({
    version: 1,
    firstDisplay: firstDisplayCatalog.map(
      ({ order, id, include, allowedImports, inputs, outputs, obligation }) => ({
        order,
        id,
        include,
        allowedImports,
        inputs,
        outputs,
        obligation,
      })
    ),
    permittedFirstDisplayMasks: firstDisplayMasks
      .filter(({ permitted }) => permitted)
      .map(({ mask }) => mask),
    modules: releaseCatalog.map(({ id, phase, trigger, include }) => ({
      id,
      phase,
      trigger,
      include,
    })),
  })}\n`
);
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
  firstDisplay: {
    catalog: firstDisplayMaskCatalog,
    components: Object.fromEntries(
      artifacts
        .filter(({ phase }) => phase === 'first_display')
        .map((artifact) => [
          artifact.id,
          {
            file: artifact.file,
            entry: path.normalize(`src/${artifact.entry}`),
            ...measureBytes(fs.readFileSync(path.join(distributionDirectory, artifact.file))),
            sources: artifact.moduleContributions,
          },
        ])
    ),
    masks: firstDisplayMasks,
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
