#!/usr/bin/env node

import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import vm from 'node:vm';
import { fileURLToPath } from 'node:url';
import ts from 'typescript';

import {
  BUNDLE_SEPARATOR,
  BUNDLE_SET_NAMES,
  BUNDLE_SIZE_NAMES,
  FIRST_DISPLAY_AGENT_SIZE_CEILING,
  deriveInventorySetFiles,
  deriveSemanticBundleSetIds,
  enumerateReachableFirstDisplayMasks,
  firstDisplayMaskIsPermitted,
  measureBundleSet,
  measureBytes,
} from './bundle-metrics.mjs';
import { computeReleaseId, RELEASE_SENTINEL } from './release-v1.mjs';

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const libDir = path.resolve(scriptDir, '..');
const repositoryRoot = path.resolve(libDir, '../../..');
const defaultBaselinePath = path.join(
  libDir,
  'test',
  'fixtures',
  'performance',
  'aps-tsjs-prechange.json'
);
const metricsPath = path.resolve(libDir, '..', 'dist', 'tsjs-build-metrics-v1.json');
const catalogPath = path.resolve(libDir, '..', 'dist', 'tsjs-catalog-v1.json');
const releasePath = path.resolve(libDir, '..', 'dist', 'tsjs-release-v1.json');
const releaseCatalogSourcePath = path.join(libDir, 'src', 'kernel', 'release_catalog.ts');
const repositoryReleaseCatalogPath = 'crates/trusted-server-js/lib/src/kernel/release_catalog.ts';
const SET_NAMES = BUNDLE_SET_NAMES;
const SIZE_NAMES = BUNDLE_SIZE_NAMES;
const TRANSFER_SET_NAMES = Object.freeze(['bootstrap', ...SET_NAMES]);
const HISTORICAL_EVIDENCE_SHA256 =
  '53f762603ad49239f1756171440be422e190cc231efafc56cf37a11e1a38ddf4';
const ROLE_CORRECT_CAPTURE_SHA256 =
  'f1d73d517e4888ef4dc3a84b34166e9aeb6a2bde99dec1c835f151f4e070f64a';
const REVIEW_REMEDIATION_CAPTURE_SHA256 =
  '25ca3892167bac91ae18f4927fce47b49dcdb107814985e354924cec8392572f';
const BOOTSTRAP_BASELINE = Object.freeze({
  rawBytes: 19_101,
  gzipBytes: 5_468,
  brotliBytes: 4_632,
});
export const CANDIDATE_ARCHITECTURE_SIZE_CEILINGS = Object.freeze({
  bootstrap: Object.freeze({ rawBytes: 48_000, gzipBytes: 16_000, brotliBytes: 14_000 }),
  firstDisplayAgent: Object.freeze({
    ...FIRST_DISPLAY_AGENT_SIZE_CEILING,
  }),
  referencePersistent: Object.freeze({
    rawBytes: 524_288,
    gzipBytes: 163_840,
    brotliBytes: 131_072,
  }),
  maximalTotal: Object.freeze({
    rawBytes: 1_048_576,
    gzipBytes: 327_680,
    brotliBytes: 262_144,
  }),
});
const PRODUCTION_SEAM_PATTERN =
  /(?:^|\/)(?:tests?|fixtures?|fakes?|no-?op)(?:\/|$)|(?:^|[/_.-])(?:test|fake|no-?op)(?=[/_.-]|$)|ForTest/u;
const CAPTURE_PACKAGE_LOCK_PATH = 'crates/trusted-server-js/lib/package-lock.json';
const CAPTURE_TOOL_VERSIONS_PATH = '.tool-versions';
const CAPTURE_PERFORMANCE_WORKFLOW_PATH = '.github/workflows/tsjs-performance-gate.yml';
const CAPTURE_BUILD_INPUTS = Object.freeze([
  CAPTURE_TOOL_VERSIONS_PATH,
  CAPTURE_PERFORMANCE_WORKFLOW_PATH,
  'crates/trusted-server-js/lib/src',
  'crates/trusted-server-js/lib/build-all.mjs',
  'crates/trusted-server-js/lib/package-lock.json',
  'crates/trusted-server-js/lib/package.json',
  'crates/trusted-server-js/lib/tsconfig.json',
  'crates/trusted-server-js/lib/vite.config.ts',
]);
const ROLE_CORRECT_CAPTURE_BUILD_INPUTS = Object.freeze(
  CAPTURE_BUILD_INPUTS.filter((input) => input !== CAPTURE_PERFORMANCE_WORKFLOW_PATH)
);
const CAPTURE_TOOL_PACKAGES = Object.freeze({
  typescript: 'typescript',
  vite: 'vite',
  esbuild: 'esbuild',
});
const CURRENT_PROVIDER_SOURCE_OWNERS = Object.freeze({
  'src/kernel/runtime.ts': 'core',
  'src/integrations/render_runtime/module.ts': 'core',
  'src/services/render.ts': 'core',
  'src/integrations/gpt/module.ts': 'gpt',
  'src/integrations/gpt/startup.ts': 'gpt',
  'src/integrations/gpt_diagnostics/store.ts': 'gpt_diagnostics',
  'src/integrations/osano/consent.ts': 'osano_consent',
  'src/adapters/prebid.ts': 'prebid',
  'src/integrations/prebid/module.ts': 'prebid',
  'src/integrations/prebid/startup.ts': 'prebid',
  'src/integrations/sourcepoint/consent.ts': 'sourcepoint_consent',
});
const CURRENT_EXACT_SOURCE_OWNERS = Object.freeze({
  'src/core/adapters/gam_attribution.ts': 'bootstrap',
  'src/core/bootstrap.ts': 'bootstrap',
  'src/core/contracts/boot.ts': 'bootstrap',
  'src/core/contracts/integration_configs.ts': 'bootstrap',
  'src/core/types.ts': 'bootstrap',
  'src/adapters/googletag.ts': 'gpt',
  'src/adapters/messaging.ts': 'core',
  'src/composition/runtime_transport.ts': 'core',
  'src/services/auction_batch.ts': 'core',
  'src/services/context.ts': 'core',
  'src/services/projections.ts': 'core',
  'src/services/puc_bridge.ts': 'gpt',
  'src/services/reservations.ts': 'core',
  'src/services/slots.ts': 'gpt',
  'src/services/targeting.ts': 'gpt',
  'src/integrations/aps/index.ts': 'aps',
  'src/integrations/aps/documents.ts': 'aps',
  'src/integrations/aps/module.ts': 'aps',
  'src/integrations/aps/render.ts': 'aps',
  'src/integrations/creative/click.ts': 'creative',
  'src/integrations/creative/dynamic_src_guard.ts': 'creative',
  'src/integrations/creative/iframe.ts': 'creative',
  'src/integrations/creative/image.ts': 'creative',
  'src/integrations/creative/index.ts': 'creative',
  'src/integrations/creative/module.ts': 'creative',
  'src/integrations/creative/proxy_sign.ts': 'creative',
  'src/integrations/creative/startup.ts': 'creative',
  'src/integrations/datadome/index.ts': 'datadome',
  'src/integrations/datadome/module.ts': 'datadome',
  'src/integrations/datadome/script_guard.ts': 'datadome',
  'src/integrations/didomi/index.ts': 'didomi',
  'src/integrations/didomi/module.ts': 'didomi',
  'src/integrations/google_tag_manager/index.ts': 'google_tag_manager',
  'src/integrations/google_tag_manager/module.ts': 'google_tag_manager',
  'src/integrations/google_tag_manager/script_guard.ts': 'google_tag_manager',
  'src/integrations/gpt/diagnostics_facts.ts': 'gpt',
  'src/integrations/gpt/index.ts': 'gpt',
  'src/integrations/gpt/later.ts': 'gpt_later',
  'src/integrations/gpt/script_guard.ts': 'gpt',
  'src/integrations/gpt_diagnostics/badges.ts': 'diagnostics_presentation',
  'src/integrations/gpt_diagnostics/binding.ts': 'diagnostics_presentation',
  'src/integrations/gpt_diagnostics/data_api.ts': 'gpt_diagnostics',
  'src/integrations/gpt_diagnostics/exhaustive.ts': 'diagnostics_presentation',
  'src/integrations/gpt_diagnostics/index.ts': 'gpt_diagnostics',
  'src/integrations/gpt_diagnostics/module.ts': 'gpt_diagnostics',
  'src/integrations/gpt_diagnostics/observer.ts': 'gpt_diagnostics',
  'src/integrations/gpt_diagnostics/overlay.ts': 'diagnostics_presentation',
  'src/integrations/gpt_diagnostics/presentation.ts': 'diagnostics_presentation',
  'src/integrations/gpt_diagnostics/presentation_helpers.ts': 'diagnostics_presentation',
  'src/integrations/lockr/index.ts': 'lockr',
  'src/integrations/lockr/module.ts': 'lockr',
  'src/integrations/lockr/script_guard.ts': 'lockr',
  'src/integrations/osano/consent_mirror.ts': 'osano_consent',
  'src/integrations/osano/lifecycle.ts': 'osano_lifecycle',
  'src/integrations/osano/module.ts': 'osano_consent',
  'src/integrations/permutive/context.ts': 'permutive_context',
  'src/integrations/permutive/lifecycle.ts': 'permutive_lifecycle',
  'src/integrations/permutive/module.ts': 'permutive_context',
  'src/integrations/permutive/script_guard.ts': 'permutive_context',
  'src/integrations/permutive/segments.ts': 'permutive_context',
  'src/integrations/prebid/index.ts': 'prebid',
  'src/integrations/prebid/later.ts': 'prebid_later',
  'src/integrations/prebid/refresh.ts': 'prebid_later',
  'src/integrations/render_runtime/transport_marker.ts': 'render_runtime',
  'src/integrations/sourcepoint/consent_mirror.ts': 'sourcepoint_consent',
  'src/integrations/sourcepoint/lifecycle.ts': 'sourcepoint_lifecycle',
  'src/integrations/sourcepoint/module.ts': 'sourcepoint_consent',
  'src/integrations/sourcepoint/script_guard.ts': 'sourcepoint_consent',
  'src/integrations/testlight/index.ts': 'testlight',
  'src/integrations/testlight/module.ts': 'testlight',
});
const CURRENT_SHARED_SOURCE_OWNER_POLICIES = Object.freeze({
  'src/core/auction.ts': Object.freeze(['core']),
  'src/core/config.ts': Object.freeze(['bootstrap', 'core']),
  'src/core/contracts/aps_renderer.ts': Object.freeze(['bootstrap', 'core', 'aps']),
  'src/core/contracts/auction_projection.ts': Object.freeze([
    'bootstrap',
    'core',
    'prebid',
    'prebid_later',
  ]),
  'src/core/contracts/generated/renderer_validator_v1.ts': Object.freeze([
    'bootstrap',
    'core',
    'aps',
  ]),
  'src/core/contracts/request_ads.ts': Object.freeze(['bootstrap', 'core']),
  'src/core/index.ts': Object.freeze(['core']),
  'src/core/log.ts': Object.freeze([
    'bootstrap',
    'core',
    'creative',
    'datadome',
    'didomi',
    'google_tag_manager',
    'gpt',
    'gpt_diagnostics',
    'lockr',
    'osano_consent',
    'permutive_context',
    'sourcepoint_consent',
    'testlight',
    'diagnostics_presentation',
  ]),
  'src/core/puc_shell.ts': Object.freeze(['gpt']),
  'src/core/queue.ts': Object.freeze(['bootstrap', 'core']),
  'src/core/release_id.ts': Object.freeze([
    'bootstrap',
    'core',
    'aps',
    'creative',
    'datadome',
    'didomi',
    'google_tag_manager',
    'gpt',
    'gpt_diagnostics',
    'lockr',
    'osano_consent',
    'permutive_context',
    'sourcepoint_consent',
    'prebid',
    'testlight',
    'diagnostics_presentation',
    'gpt_later',
    'osano_lifecycle',
    'permutive_lifecycle',
    'prebid_later',
    'sourcepoint_lifecycle',
  ]),
  'src/core/registry.ts': Object.freeze(['bootstrap', 'core']),
  'src/core/release.ts': Object.freeze([
    'bootstrap',
    'core',
    'aps',
    'creative',
    'datadome',
    'didomi',
    'google_tag_manager',
    'gpt',
    'gpt_diagnostics',
    'lockr',
    'osano_consent',
    'permutive_context',
    'sourcepoint_consent',
    'prebid',
    'testlight',
    'diagnostics_presentation',
    'gpt_later',
    'osano_lifecycle',
    'permutive_lifecycle',
    'prebid_later',
    'sourcepoint_lifecycle',
  ]),
  'src/core/render.ts': Object.freeze(['core']),
  'src/core/styles/normalize.css?inline': Object.freeze(['core']),
  'src/core/templates/iframe.html?raw': Object.freeze(['core']),
  'src/core/trace.ts': Object.freeze(['core', 'gpt_diagnostics']),
  'src/kernel/contracts/message_protocol.ts': Object.freeze(['core', 'gpt']),
  'src/kernel/contracts/puc_dynamic_owner.ts': Object.freeze(['aps_initial', 'gpt']),
  'src/kernel/diagnostics.ts': Object.freeze(['core']),
  'src/kernel/disposable.ts': Object.freeze(['core', 'gpt']),
  'src/kernel/fallback.ts': Object.freeze(['bootstrap', 'core']),
  'src/kernel/fallback_surface.ts': Object.freeze(['bootstrap', 'core']),
  'src/kernel/identity.ts': Object.freeze(['first_display', 'core']),
  'src/kernel/integration_registry.ts': Object.freeze(['core']),
  'src/kernel/lifecycle_module.ts': Object.freeze([
    'datadome',
    'didomi',
    'google_tag_manager',
    'lockr',
    'testlight',
  ]),
  'src/kernel/phase_loader.ts': Object.freeze(['core']),
  'src/kernel/release_catalog.ts': Object.freeze([
    'bootstrap',
    'core',
    'datadome',
    'didomi',
    'google_tag_manager',
    'lockr',
    'testlight',
  ]),
  'src/kernel/sessions.ts': Object.freeze(['core']),
  'src/shared/async.ts': Object.freeze(['creative', 'datadome']),
  'src/shared/first_display_contracts.ts': Object.freeze(['bootstrap', 'core']),
  'src/shared/first_display_handoff.ts': Object.freeze(['bootstrap', 'first_display']),
  'src/shared/first_display_registration.ts': Object.freeze(['bootstrap', 'first_display']),
  'src/shared/first_display_transaction.ts': Object.freeze(['bootstrap']),
  'src/shared/integration_config_validators.ts': Object.freeze([
    'first_display',
    'aps',
    'datadome',
    'google_tag_manager',
    'gpt',
    'lockr',
    'osano_consent',
    'permutive_context',
    'sourcepoint_consent',
    'prebid',
    'testlight',
    'gpt_later',
    'osano_lifecycle',
    'permutive_lifecycle',
    'prebid_later',
    'sourcepoint_lifecycle',
  ]),
  'src/shared/beacon_guard.ts': Object.freeze(['google_tag_manager']),
  'src/shared/dom_insertion_dispatcher.ts': Object.freeze([
    'datadome',
    'google_tag_manager',
    'gpt',
    'lockr',
    'permutive_context',
    'sourcepoint_consent',
  ]),
  'src/shared/globals.ts': Object.freeze(['creative']),
  'src/shared/origin.ts': Object.freeze(['core', 'creative']),
  'src/shared/realm.ts': Object.freeze(['gpt_diagnostics', 'diagnostics_presentation']),
  'src/shared/scheduler.ts': Object.freeze(['creative']),
  'src/shared/script_guard.ts': Object.freeze([
    'datadome',
    'google_tag_manager',
    'gpt',
    'lockr',
    'permutive_context',
    'sourcepoint_consent',
  ]),
  'src/shared/takeover.ts': Object.freeze([
    'bootstrap',
    'first_display',
    'core',
    'aps',
    'creative',
    'datadome',
    'didomi',
    'google_tag_manager',
    'gpt',
    'lockr',
    'osano_consent',
    'permutive_context',
    'sourcepoint_consent',
    'prebid',
    'testlight',
  ]),
});
const CAPABILITY_PATTERN = /^[a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)*\.v[1-9][0-9]*$/u;
const CAPABILITY_PREDICATE_PATTERN = /^[a-z][a-z0-9_]{0,63}$/u;

function fail(message) {
  throw new Error(`[bundle-budgets] ${message}`);
}

function readJson(file, label) {
  if (!fs.existsSync(file)) fail(`${label} does not exist: ${file}`);
  try {
    return JSON.parse(fs.readFileSync(file, 'utf8'));
  } catch (error) {
    fail(`${label} is not valid JSON (${file}): ${error instanceof Error ? error.message : error}`);
  }
}

function assertPositiveInteger(value, label) {
  if (!Number.isSafeInteger(value) || value <= 0) fail(`${label} must be a positive integer`);
}

function canonicalJson(value) {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;
  if (value !== null && typeof value === 'object') {
    return `{${Object.keys(value)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`)
      .join(',')}}`;
  }
  return JSON.stringify(value);
}

/** Hash JSON with recursive key ordering while preserving array order and values. */
export function canonicalJsonSha256(value) {
  return createHash('sha256').update(canonicalJson(value)).digest('hex');
}

/** Evaluate the authored catalog source without importing production TS at runtime. */
function loadAuthoredCatalogModule(source) {
  const transpiled = ts.transpileModule(source, {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
    fileName: releaseCatalogSourcePath,
  }).outputText;
  const module = { exports: {} };
  vm.runInNewContext(
    transpiled,
    { module, exports: module.exports, Object, Set, TypeError },
    {
      filename: releaseCatalogSourcePath,
    }
  );
  return module.exports;
}

export function loadAuthoredReleaseCatalog(
  source = fs.readFileSync(releaseCatalogSourcePath, 'utf8')
) {
  const authored = loadAuthoredCatalogModule(source);
  const catalog = authored.RELEASE_CATALOG;
  const validateCatalog = authored.validateReleaseCatalog;
  if (!Array.isArray(catalog) || typeof validateCatalog !== 'function') {
    fail('authored release catalog did not export its catalog and validator');
  }
  try {
    validateCatalog(catalog);
  } catch (error) {
    fail(
      `authored release catalog failed self-validation: ${error instanceof Error ? error.message : error}`
    );
  }
  return JSON.parse(JSON.stringify(catalog));
}

/** Load the complete authored first-display catalog from the same source authority. */
export function loadAuthoredFirstDisplayCatalog(
  source = fs.readFileSync(releaseCatalogSourcePath, 'utf8')
) {
  const catalog = loadAuthoredCatalogModule(source).FIRST_DISPLAY_CATALOG;
  if (!Array.isArray(catalog)) {
    fail('authored release catalog did not export its first-display catalog');
  }
  return JSON.parse(JSON.stringify(catalog));
}

function loadHistoricalReleaseCatalog(capture, label) {
  const sha = capture?.source?.sha;
  let source;
  try {
    source = execFileSync('git', ['show', `${sha}:${repositoryReleaseCatalogPath}`], {
      cwd: repositoryRoot,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
    });
  } catch {
    fail(`${label} authored release catalog is missing at its captured source SHA`);
  }
  return loadAuthoredReleaseCatalog(source).map((entry) => ({
    ...entry,
    phase: entry.phase === 'critical' ? 'takeover' : entry.phase,
  }));
}

/** Authenticate the immutable inputs and tool versions recorded by a historical capture. */
export function validateCaptureSourceProvenance(
  capture,
  { buildInputs = CAPTURE_BUILD_INPUTS, head = 'HEAD', npmAuthorityCapture = capture } = {}
) {
  const sha = capture?.source?.sha;
  if (typeof sha !== 'string' || !/^[0-9a-f]{40}$/u.test(sha)) {
    fail('capture source SHA is invalid');
  }

  try {
    if (
      execFileSync('git', ['cat-file', '-t', sha], {
        cwd: repositoryRoot,
        encoding: 'utf8',
        stdio: ['ignore', 'pipe', 'ignore'],
      }).trim() !== 'commit'
    ) {
      fail('capture source SHA does not identify a commit');
    }
  } catch (error) {
    if (error instanceof Error && error.message.startsWith('[bundle-budgets]')) throw error;
    fail('capture source SHA does not identify a commit');
  }

  try {
    execFileSync('git', ['merge-base', '--is-ancestor', sha, head], {
      cwd: repositoryRoot,
      stdio: 'ignore',
    });
  } catch {
    fail('capture source SHA is not an ancestor of HEAD');
  }

  for (const input of buildInputs) {
    try {
      execFileSync('git', ['cat-file', '-e', `${sha}:${input}`], {
        cwd: repositoryRoot,
        stdio: 'ignore',
      });
    } catch {
      fail(`captured build input does not exist at source SHA: ${input}`);
    }
  }

  let packageLockBytes;
  try {
    packageLockBytes = execFileSync('git', ['show', `${sha}:${CAPTURE_PACKAGE_LOCK_PATH}`], {
      cwd: repositoryRoot,
    });
  } catch {
    fail('captured package-lock does not exist at source SHA');
  }

  const packageLockSha256 = createHash('sha256').update(packageLockBytes).digest('hex');
  if (capture?.tools?.packageLockSha256 !== packageLockSha256) {
    fail('captured packageLockSha256 does not match package-lock bytes at source SHA');
  }

  let packageLock;
  try {
    packageLock = JSON.parse(packageLockBytes.toString('utf8'));
  } catch {
    fail('captured package-lock is not valid JSON');
  }

  for (const [tool, packageName] of Object.entries(CAPTURE_TOOL_PACKAGES)) {
    const resolvedVersion = packageLock.packages?.[`node_modules/${packageName}`]?.version;
    if (capture?.tools?.[tool] !== resolvedVersion) {
      fail(`captured ${tool} version does not match the resolved package-lock version`);
    }
  }

  let toolVersions;
  try {
    toolVersions = execFileSync('git', ['show', `${sha}:${CAPTURE_TOOL_VERSIONS_PATH}`], {
      cwd: repositoryRoot,
      encoding: 'utf8',
    });
  } catch {
    fail('captured .tool-versions does not exist at source SHA');
  }

  const nodejsEntries = toolVersions
    .split(/\r?\n/u)
    .filter((line) => /^\s*nodejs(?:\s|$)/u.test(line));
  const nodejsMatch = nodejsEntries[0]?.match(/^\s*nodejs\s+(\S+)\s*$/u);
  if (
    nodejsEntries.length !== 1 ||
    nodejsMatch === null ||
    capture?.tools?.node !== `v${nodejsMatch[1]}`
  ) {
    fail('captured node version does not match the exact .tool-versions nodejs pin');
  }

  if (capture?.tools?.npm !== npmAuthorityCapture?.tools?.npm) {
    fail('captured npm version does not match its authenticated npm authority');
  }
  const npmAuthoritySha = npmAuthorityCapture?.source?.sha;
  if (typeof npmAuthoritySha !== 'string' || !/^[0-9a-f]{40}$/u.test(npmAuthoritySha)) {
    fail('captured npm authority source SHA is invalid');
  }
  let performanceWorkflow;
  try {
    performanceWorkflow = execFileSync(
      'git',
      ['show', `${npmAuthoritySha}:${CAPTURE_PERFORMANCE_WORKFLOW_PATH}`],
      {
        cwd: repositoryRoot,
        encoding: 'utf8',
      }
    );
  } catch {
    fail('captured TSJS performance workflow does not exist at source SHA');
  }

  const npmVersionAssertions = performanceWorkflow
    .split(/\r?\n/u)
    .filter((line) => line.includes('npm --version'));
  const npmVersionMatch = npmVersionAssertions[0]?.match(
    /^\s*test "\$\(npm --version\)" = "([^"\s]+)"\s*$/u
  );
  if (
    npmVersionAssertions.length !== 1 ||
    npmVersionMatch === null ||
    capture?.tools?.npm !== npmVersionMatch[1]
  ) {
    fail('captured npm version does not match the exact single workflow assertion');
  }
}

/** Authenticate both linked frozen transfer captures at their recorded source commits. */
export function validateFrozenCaptureProvenance(intermediate, capture) {
  validateCaptureSourceProvenance(intermediate, {
    buildInputs: ROLE_CORRECT_CAPTURE_BUILD_INPUTS,
    npmAuthorityCapture: capture,
  });
  validateCaptureSourceProvenance(capture);
}

function validateBudgetSets(sets, label) {
  if (!sets || typeof sets !== 'object' || Array.isArray(sets)) {
    fail(`${label} must be an object`);
  }
  for (const setName of SET_NAMES) {
    const set = sets[setName];
    if (!set || typeof set !== 'object' || Array.isArray(set)) {
      fail(`${label}.${setName} must be an object`);
    }
    for (const sizeName of SIZE_NAMES) {
      assertPositiveInteger(set[sizeName], `${label}.${setName}.${sizeName}`);
    }
  }
}

function validateMetricSets(sets, label) {
  validateBudgetSets(sets, label);
  for (const setName of SET_NAMES) {
    const set = sets[setName];
    if (!Array.isArray(set.files) || set.files.length === 0) {
      fail(`${label}.${setName}.files must be a non-empty array`);
    }
    for (const [index, file] of set.files.entries()) {
      if (typeof file !== 'string' || !/^tsjs-[a-z0-9_]+\.js$/.test(file)) {
        fail(`${label}.${setName}.files[${index}] is not a canonical TSJS bundle filename`);
      }
    }
    if (new Set(set.files).size !== set.files.length) {
      fail(`${label}.${setName}.files contains a duplicate`);
    }
    if (typeof set.sha256 !== 'string' || !/^[0-9a-f]{64}$/.test(set.sha256)) {
      fail(`${label}.${setName}.sha256 must be 64 lowercase hexadecimal characters`);
    }
  }
}

/** Validate semantic set membership against exact release artifact ownership. */
export function validateSemanticBundleSets(metrics, release, catalog) {
  if (catalog?.version !== 1) fail('catalog.version must equal 1');
  if (!Array.isArray(catalog.firstDisplay) || catalog.firstDisplay.length !== 13) {
    fail('catalog.firstDisplay must contain the closed thirteen-row inventory');
  }
  if (release?.version !== 1 || !Array.isArray(release.artifacts)) {
    fail('release inventory is invalid');
  }
  if (!metrics?.sets) fail('build metrics sets are missing');
  validateMetricSets(metrics.sets, 'buildMetrics.sets');
  if (
    !metrics.bootstrap ||
    metrics.bootstrap.file !== 'tsjs-bootstrap.js' ||
    typeof metrics.bootstrap.sha256 !== 'string' ||
    !/^[0-9a-f]{64}$/.test(metrics.bootstrap.sha256)
  ) {
    fail('buildMetrics.bootstrap must identify the generated bootstrap exactly once');
  }
  for (const sizeName of SIZE_NAMES) {
    assertPositiveInteger(metrics.bootstrap[sizeName], `buildMetrics.bootstrap.${sizeName}`);
  }
  const expected = deriveSemanticBundleSetIds(catalog.modules);
  const files = new Map();
  const ids = new Set();
  for (const artifact of release.artifacts) {
    if (
      !artifact ||
      typeof artifact !== 'object' ||
      typeof artifact.id !== 'string' ||
      typeof artifact.file !== 'string' ||
      !['bootstrap', 'first_display_base', 'first_display_slice', 'core', 'integration'].includes(
        artifact.role
      ) ||
      ids.has(artifact.id) ||
      files.has(artifact.file)
    ) {
      fail('release inventory contains an invalid or duplicate artifact');
    }
    ids.add(artifact.id);
    files.set(artifact.file, artifact);
  }
  const expectedArtifacts = [
    { id: 'bootstrap', role: 'bootstrap', file: 'tsjs-bootstrap.js' },
    ...catalog.firstDisplay.map(({ id }, index) => ({
      id,
      role: index === 0 ? 'first_display_base' : 'first_display_slice',
      file: `tsjs-${id}.js`,
    })),
    { id: 'core', role: 'core', file: 'tsjs-core.js' },
    ...catalog.modules.map(({ id }) => ({
      id,
      role: 'integration',
      file: `tsjs-${id}.js`,
    })),
  ];
  if (release.artifacts.length !== expectedArtifacts.length) {
    fail('release inventory does not contain the exact catalog artifact count');
  }
  for (const [index, expectedArtifact] of expectedArtifacts.entries()) {
    const artifact = release.artifacts[index];
    if (
      artifact.id !== expectedArtifact.id ||
      artifact.role !== expectedArtifact.role ||
      artifact.file !== expectedArtifact.file
    ) {
      fail(
        `release inventory artifact ${index} must be ${expectedArtifact.role}/${expectedArtifact.id}/${expectedArtifact.file}`
      );
    }
  }

  const actual = {};
  for (const setName of SET_NAMES) {
    const setIds = metrics.sets[setName].files.map((file) => {
      const artifact = files.get(file);
      if (!artifact || !['core', 'integration'].includes(artifact.role)) {
        fail(`buildMetrics.sets.${setName} contains an unknown or non-persistent artifact`);
      }
      return artifact.id;
    });
    if (new Set(setIds).size !== setIds.length) {
      fail(`buildMetrics.sets.${setName} contains a multiply counted artifact`);
    }
    if (JSON.stringify(setIds) !== JSON.stringify(expected[setName])) {
      fail(
        `buildMetrics.sets.${setName} has semantic ids ${JSON.stringify(setIds)}; expected ${JSON.stringify(expected[setName])}`
      );
    }
    actual[setName] = setIds;
  }
  return actual;
}

function canonicalSourcePath(file) {
  return typeof file === 'string' ? file.replaceAll('\\', '/') : '';
}

function currentExactSourceOwner(source) {
  if (Object.hasOwn(CURRENT_PROVIDER_SOURCE_OWNERS, source)) {
    return { kind: 'provider', owner: CURRENT_PROVIDER_SOURCE_OWNERS[source] };
  }
  if (Object.hasOwn(CURRENT_EXACT_SOURCE_OWNERS, source)) {
    return { kind: 'phase', owner: CURRENT_EXACT_SOURCE_OWNERS[source] };
  }
  if (source.startsWith('src/integrations/gpt_diagnostics/presentation/')) {
    return { kind: 'phase', owner: 'diagnostics_presentation' };
  }
  if (source.startsWith('src/integrations/gpt/later/')) {
    return { kind: 'phase', owner: 'gpt_later' };
  }
  if (source.startsWith('src/integrations/prebid/later/')) {
    return { kind: 'phase', owner: 'prebid_later' };
  }
  return undefined;
}

function readCurrentSourceGraph(metrics, release) {
  if (!Array.isArray(metrics?.modules) || !Array.isArray(release?.artifacts)) {
    fail('build metrics module graph or release inventory is missing');
  }
  const productionArtifacts = release.artifacts.filter(({ role }) => role !== 'bootstrap');
  if (metrics.modules.length !== productionArtifacts.length) {
    fail('build metrics module graph does not classify every production artifact exactly once');
  }
  const rawEntries = [
    {
      artifact: release.artifacts.find(({ role }) => role === 'bootstrap'),
      module: metrics.bootstrap,
    },
    ...metrics.modules.map((module, index) => ({
      artifact: productionArtifacts[index],
      module,
    })),
  ];
  const graphEntries = [];
  const ownersBySource = new Map();
  const contributions = [];
  for (const [index, { artifact, module }] of rawEntries.entries()) {
    if (
      !artifact ||
      !module ||
      typeof module !== 'object' ||
      module.file !== artifact.file ||
      typeof module.entry !== 'string' ||
      !Array.isArray(module.sources)
    ) {
      fail(`build metrics module graph entry ${index} is invalid or out of release order`);
    }
    const entry = canonicalSourcePath(module.entry);
    const sources = new Set();
    for (const [sourceIndex, source] of module.sources.entries()) {
      const sourceFile = canonicalSourcePath(source?.file);
      if (
        !sourceFile.startsWith('src/') ||
        !Number.isSafeInteger(source?.renderedBytes) ||
        source.renderedBytes < 0 ||
        sources.has(sourceFile)
      ) {
        fail(`build metrics module graph ${module.file}.sources[${sourceIndex}] is invalid`);
      }
      sources.add(sourceFile);
      const owners = ownersBySource.get(sourceFile) ?? [];
      if (owners.includes(artifact.id)) {
        fail(`build metrics source ${sourceFile} repeats owner ${artifact.id}`);
      }
      owners.push(artifact.id);
      ownersBySource.set(sourceFile, owners);
      contributions.push({
        artifact: artifact.id,
        source: sourceFile,
        renderedBytes: source.renderedBytes,
      });
    }
    if (sources.size === 0 || !sources.has(entry)) {
      fail(`build metrics module graph ${module.file} does not contain its entry source`);
    }
    graphEntries.push({ artifact, module, entry, sources });
  }
  return {
    graphEntries,
    sourceOwners: Object.fromEntries(ownersBySource),
    contributions,
  };
}

/** Build the frozen attribution report used to review production bundle growth. */
export function buildProductionGraphReport(metrics, release) {
  const currentGraph = readCurrentSourceGraph(metrics, release);
  const largestContributions = [...currentGraph.contributions]
    .sort(
      (left, right) =>
        right.renderedBytes - left.renderedBytes ||
        left.source.localeCompare(right.source) ||
        left.artifact.localeCompare(right.artifact)
    )
    .slice(0, 20);
  const repeatedAttributions = Object.entries(currentGraph.sourceOwners)
    .filter(([, owners]) => owners.length > 1)
    .map(([source, owners]) => ({ source, owners }))
    .sort((left, right) => left.source.localeCompare(right.source));
  return { largestContributions, repeatedAttributions };
}

function findTakeoverDeferredViolations(currentGraph, release) {
  const artifactsById = new Map(release.artifacts.map((artifact) => [artifact.id, artifact]));
  const violations = [];
  for (const [source, owners] of Object.entries(currentGraph.sourceOwners)) {
    const policy = currentExactSourceOwner(source);
    if (policy && artifactsById.get(policy.owner)?.phase === 'deferred') {
      for (const owner of owners) {
        const artifact = artifactsById.get(owner);
        if (artifact?.role === 'core' || artifact?.phase === 'takeover') {
          violations.push(`${owner} reaches deferred-owned source ${source}`);
        }
      }
    }
  }
  return violations;
}

function validateCurrentSourceOwnership(currentGraph, release) {
  const artifactsById = new Map(release.artifacts.map((artifact) => [artifact.id, artifact]));
  const artifactIds = new Set(artifactsById.keys());
  const violations = findTakeoverDeferredViolations(currentGraph, release);
  for (const source of Object.keys(CURRENT_PROVIDER_SOURCE_OWNERS)) {
    if (!Object.hasOwn(currentGraph.sourceOwners, source)) {
      violations.push(`current provider source is missing: ${source}`);
    }
  }
  for (const [source, owners] of Object.entries(currentGraph.sourceOwners)) {
    const firstDisplaySource = source.startsWith('src/first_display/');
    const firstDisplayOwners = owners.filter((owner) =>
      ['first_display_base', 'first_display_slice'].includes(artifactsById.get(owner)?.role)
    );
    if (firstDisplaySource) {
      for (const owner of owners) {
        if (!firstDisplayOwners.includes(owner)) {
          violations.push(`${owner} reaches first-display source ${source}`);
        }
      }
      continue;
    }
    const sharedOwners = CURRENT_SHARED_SOURCE_OWNER_POLICIES[source];
    for (const owner of firstDisplayOwners) {
      if (!sharedOwners?.includes(owner)) {
        violations.push(`${owner} reaches persistent source ${source}`);
      }
    }
    const policy = currentExactSourceOwner(source);
    if (policy) {
      if (!artifactIds.has(policy.owner)) {
        violations.push(`${source} requires missing current owner ${policy.owner}`);
        continue;
      }
      for (const owner of owners) {
        if (owner !== policy.owner) {
          if (policy.kind === 'provider') {
            violations.push(`${owner} inlines provider ${policy.owner} implementation ${source}`);
          } else {
            violations.push(
              `${source} must have exact current owner ${policy.owner}, not ${owner}`
            );
          }
        }
      }
      if (!owners.includes(policy.owner)) {
        violations.push(`${source} must have exact current owner ${policy.owner}`);
      }
    } else if (Object.hasOwn(CURRENT_SHARED_SOURCE_OWNER_POLICIES, source)) {
      const allowedOwners = CURRENT_SHARED_SOURCE_OWNER_POLICIES[source];
      for (const owner of owners) {
        if (!allowedOwners.includes(owner)) {
          violations.push(`${owner} reaches forbidden shared source ${source}`);
        }
      }
    } else {
      violations.push(`unclassified current production source ${source}`);
    }
  }
  return violations;
}

/** Return takeover artifacts that transitively bundle explicit deferred-phase source. */
export function findTakeoverDeferredSourceViolations(metrics, release) {
  return findTakeoverDeferredViolations(readCurrentSourceGraph(metrics, release), release);
}

/** Return all current production graph ownership and seam violations. */
export function findProductionGraphViolations(metrics, release) {
  const currentGraph = readCurrentSourceGraph(metrics, release);
  const violations = validateCurrentSourceOwnership(currentGraph, release);
  for (const { artifact, sources } of currentGraph.graphEntries) {
    for (const source of sources) {
      if (PRODUCTION_SEAM_PATTERN.test(source)) {
        violations.push(`${artifact.id} reaches production test/fake/no-op seam ${source}`);
      }
    }
  }
  return violations;
}

function validateArtifactContents(release, contents) {
  if (!(contents instanceof Map)) fail('current artifact contents must be a Map');
  const releaseArtifacts = [];
  for (const artifact of release.artifacts) {
    const bytes = contents.get(artifact.file);
    if (!(bytes instanceof Uint8Array))
      fail(`current artifact bytes are missing: ${artifact.file}`);
    const digest = createHash('sha256').update(bytes).digest('hex');
    if (bytes.byteLength !== artifact.bytes || digest !== artifact.hash) {
      fail(`current artifact bytes do not match release inventory: ${artifact.file}`);
    }
    const source = Buffer.from(bytes.buffer, bytes.byteOffset, bytes.byteLength).toString('utf8');
    if (source.includes(RELEASE_SENTINEL) || source.split(release.releaseId).length - 1 !== 1) {
      fail(`current artifact bytes do not contain exactly one release id: ${artifact.file}`);
    }
    releaseArtifacts.push({
      id: artifact.id,
      role: artifact.role,
      phase: artifact.phase ?? '',
      trigger: artifact.trigger ?? '',
      bytes: Buffer.from(source.replace(release.releaseId, RELEASE_SENTINEL)),
    });
  }
  if (computeReleaseId(releaseArtifacts) !== release.releaseId) {
    fail('current artifact bytes do not reproduce release id');
  }
}

function firstDisplayCompositionBytes(files, contents) {
  const parts = files.flatMap((file, index) => {
    const bytes = contents.get(file);
    if (!(bytes instanceof Uint8Array)) fail(`current artifact bytes are missing: ${file}`);
    return index === files.length - 1 ? [bytes] : [bytes, BUNDLE_SEPARATOR];
  });
  return Buffer.concat(parts);
}

function validateFirstDisplayMaskMeasurements(metrics, contents) {
  const catalog = metrics?.firstDisplay?.catalog;
  const masks = metrics?.firstDisplay?.masks;
  const expected = enumerateReachableFirstDisplayMasks(catalog);
  if (!Array.isArray(masks) || masks.length !== expected.length) {
    fail(`build metrics must contain all ${expected.length} reachable first-display masks`);
  }
  const hashes = new Set();
  for (const [index, canonical] of expected.entries()) {
    const measured = masks[index];
    if (
      !hasExactKeys(measured, [
        'mask',
        'ids',
        'files',
        'rawBytes',
        'gzipBytes',
        'brotliBytes',
        'sha256',
        'permitted',
      ]) ||
      measured.mask !== canonical.mask ||
      canonicalJson(measured.ids) !== canonicalJson(canonical.ids) ||
      canonicalJson(measured.files) !== canonicalJson(canonical.files)
    ) {
      fail(`build metrics first-display mask ${index} is not canonical`);
    }
    for (const sizeName of SIZE_NAMES) {
      assertPositiveInteger(measured[sizeName], `firstDisplay.masks.${index}.${sizeName}`);
    }
    const bytes = firstDisplayCompositionBytes(canonical.files, contents);
    const sha256 = createHash('sha256').update(bytes).digest('hex');
    if (
      measured.rawBytes !== bytes.byteLength ||
      measured.sha256 !== sha256 ||
      measured.permitted !== firstDisplayMaskIsPermitted(measured) ||
      hashes.has(sha256)
    ) {
      fail(`build metrics first-display mask ${canonical.mask} bytes or digest are invalid`);
    }
    hashes.add(sha256);
  }
  return masks;
}

function largestMask(masks, sizeName) {
  return masks.reduce((largest, candidate) =>
    candidate[sizeName] > largest[sizeName] ? candidate : largest
  );
}

function namedFirstDisplayMask(masks, ids, name) {
  const key = JSON.stringify(ids);
  const matches = masks.filter((mask) => JSON.stringify(mask.ids) === key);
  if (matches.length !== 1) fail(`first-display ${name} mask is unavailable or ambiguous`);
  return matches[0];
}

/** Build the four independent absolute-size measurements from one canonical release. */
export function buildCandidateArchitectureSizeReport({
  metrics,
  release,
  catalog,
  currentArtifactContents,
}) {
  validateSemanticBundleSets(metrics, release, catalog);
  if (!(currentArtifactContents instanceof Map)) fail('current artifact contents must be a Map');
  const masks = validateFirstDisplayMaskMeasurements(metrics, currentArtifactContents);
  const permittedMasks = masks.filter(({ permitted }) => permitted === true);
  if (permittedMasks.length === 0) fail('release has no permitted first-display masks');
  const generatedPermittedMasks = catalog?.permittedFirstDisplayMasks;
  if (
    !Array.isArray(generatedPermittedMasks) ||
    canonicalJson(generatedPermittedMasks) !== canonicalJson(permittedMasks.map(({ mask }) => mask))
  ) {
    fail('generated permitted first-display masks do not match measured size admission');
  }
  const largestRaw = largestMask(permittedMasks, 'rawBytes');
  const largestGzip = largestMask(permittedMasks, 'gzipBytes');
  const largestBrotli = largestMask(permittedMasks, 'brotliBytes');
  const maximalFiles = release.artifacts.map(({ file }) => file);
  const maximalTotal = measureBundleSet(maximalFiles, currentArtifactContents);
  const named = {
    minimal: namedFirstDisplayMask(masks, ['first_display'], 'minimal'),
    reference: namedFirstDisplayMask(
      masks,
      ['first_display', 'creative_initial', 'datadome_initial', 'gpt_initial', 'prebid_initial'],
      'reference'
    ),
    aps: namedFirstDisplayMask(
      masks,
      ['first_display', 'aps_initial', 'creative_initial', 'gpt_initial'],
      'APS'
    ),
    largestRaw,
    largestGzip,
    largestBrotli,
  };
  for (const name of ['minimal', 'reference', 'aps']) {
    if (!named[name].permitted) fail(`required first-display ${name} mask exceeds its ceiling`);
  }
  return {
    ceilings: CANDIDATE_ARCHITECTURE_SIZE_CEILINGS,
    bootstrap: Object.fromEntries(
      SIZE_NAMES.map((sizeName) => [sizeName, metrics.bootstrap[sizeName]])
    ),
    firstDisplayAgent: {
      rawBytes: largestRaw.rawBytes,
      gzipBytes: largestGzip.gzipBytes,
      brotliBytes: largestBrotli.brotliBytes,
    },
    referencePersistent: Object.fromEntries(
      SIZE_NAMES.map((sizeName) => [sizeName, metrics.sets.reference[sizeName]])
    ),
    maximalTotal: Object.fromEntries(
      SIZE_NAMES.map((sizeName) => [sizeName, maximalTotal[sizeName]])
    ),
    firstDisplay: { masks, permittedMasks, named },
  };
}

/** Reject any semantic set that exceeds one reviewed independent ceiling. */
export function enforceCandidateArchitectureSizeCeilings(report) {
  for (const [semanticSet, limits] of Object.entries(CANDIDATE_ARCHITECTURE_SIZE_CEILINGS)) {
    const measured = report?.[semanticSet];
    for (const sizeName of SIZE_NAMES) {
      if (!Number.isSafeInteger(measured?.[sizeName]) || measured[sizeName] < 1) {
        fail(`${semanticSet}.${sizeName} is not a positive integer measurement`);
      }
      if (measured[sizeName] > limits[sizeName]) {
        fail(`${semanticSet}.${sizeName} exceeds ${limits[sizeName]} bytes: ${measured[sizeName]}`);
      }
    }
  }
  return report;
}

function validateCurrentMeasurements(metrics, release, catalog, contents) {
  const expectedSets = deriveInventorySetFiles(release.artifacts, catalog.modules);
  for (const setName of SET_NAMES) {
    const measured = measureBundleSet(expectedSets[setName], contents);
    if (canonicalJson(measured) !== canonicalJson(metrics.sets[setName])) {
      fail(`build metrics do not match current artifact bytes: ${setName}`);
    }
  }
  const bootstrapBytes = contents.get('tsjs-bootstrap.js');
  if (!(bootstrapBytes instanceof Uint8Array)) {
    fail('current artifact bytes are missing: tsjs-bootstrap.js');
  }
  const measuredBootstrap = measureBytes(bootstrapBytes);
  for (const key of [...SIZE_NAMES, 'sha256']) {
    if (metrics.bootstrap[key] !== measuredBootstrap[key]) {
      fail('build metrics do not match current artifact bytes: bootstrap');
    }
  }
  for (const [index, module] of metrics.modules.entries()) {
    const artifact = release.artifacts.filter(({ role }) => role !== 'bootstrap')[index];
    if (module.rawBytes !== artifact.bytes || module.sha256 !== artifact.hash) {
      fail(`build metrics do not match current artifact bytes: ${artifact.id}`);
    }
  }
}

function hasExactKeys(value, keys) {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) return false;
  const actual = Object.keys(value);
  const expected = new Set(keys);
  return actual.length === expected.size && actual.every((key) => expected.has(key));
}

function validateReleaseInventoryShape(release, label, allowHistoricalPhase = false) {
  if (!hasExactKeys(release, ['version', 'releaseId', 'artifacts'])) {
    fail(`${label} release inventory must have exact keys version,releaseId,artifacts`);
  }
  if (
    release.version !== 1 ||
    !/^[0-9a-f]{64}$/u.test(release.releaseId) ||
    !Array.isArray(release.artifacts)
  ) {
    fail(`${label} release inventory has invalid version, releaseId, or artifacts`);
  }
  const artifactKeys = [
    'id',
    'role',
    'phase',
    'trigger',
    'inputs',
    'outputs',
    'file',
    'bytes',
    'hash',
  ];
  const ids = new Set();
  const files = new Set();
  for (const [index, artifact] of release.artifacts.entries()) {
    if (!hasExactKeys(artifact, artifactKeys)) {
      fail(`${label} release artifact ${index} must have exact keys ${artifactKeys.join(',')}`);
    }
    const stringArray = (value) =>
      Array.isArray(value) &&
      value.every((entry) => typeof entry === 'string') &&
      new Set(value).size === value.length;
    const phaseAndTriggerAreValid =
      artifact.role === 'bootstrap' || artifact.role === 'core'
        ? artifact.phase === null && artifact.trigger === null
        : artifact.role === 'first_display_base' || artifact.role === 'first_display_slice'
          ? artifact.phase === 'first_display' && artifact.trigger === null
          : artifact.role === 'integration' &&
            (artifact.phase === 'takeover' ||
            (allowHistoricalPhase && artifact.phase === 'critical')
              ? artifact.trigger === null
              : artifact.phase === 'deferred' && artifact.trigger === 'first_display_or_idle');
    if (
      !/^[a-z0-9][a-z0-9_-]{0,63}$/u.test(artifact.id) ||
      ids.has(artifact.id) ||
      !phaseAndTriggerAreValid ||
      !stringArray(artifact.inputs) ||
      !stringArray(artifact.outputs) ||
      typeof artifact.file !== 'string' ||
      !(
        /^tsjs-[a-z0-9_]+\.js$/u.test(artifact.file) ||
        (allowHistoricalPhase && artifact.file === 'gpt-bootstrap-fallback.js')
      ) ||
      files.has(artifact.file) ||
      !Number.isSafeInteger(artifact.bytes) ||
      artifact.bytes <= 0 ||
      !/^[0-9a-f]{64}$/u.test(artifact.hash)
    ) {
      fail(`${label} release artifact ${index} has an invalid shape`);
    }
    ids.add(artifact.id);
    files.add(artifact.file);
  }
}

function validateCurrentReleaseSemantics(
  current,
  generatedCatalog,
  authoredCatalog,
  authoredFirstDisplayCatalog
) {
  if (
    generatedCatalog?.version !== 1 ||
    !Array.isArray(generatedCatalog.modules) ||
    !Array.isArray(generatedCatalog.firstDisplay) ||
    !Array.isArray(authoredCatalog) ||
    !Array.isArray(authoredFirstDisplayCatalog) ||
    generatedCatalog.modules.length !== authoredCatalog.length ||
    canonicalJson(generatedCatalog.firstDisplay) !== canonicalJson(authoredFirstDisplayCatalog)
  ) {
    fail('current generated/authored catalog inventory is invalid');
  }
  const expectedArtifacts = [
    {
      id: 'bootstrap',
      role: 'bootstrap',
      phase: null,
      trigger: null,
      inputs: [],
      outputs: [],
      file: 'tsjs-bootstrap.js',
    },
    ...authoredFirstDisplayCatalog.map((entry, index) => ({
      id: entry.id,
      role: index === 0 ? 'first_display_base' : 'first_display_slice',
      phase: 'first_display',
      trigger: null,
      inputs: entry.inputs,
      outputs: entry.outputs,
      file: `tsjs-${entry.id}.js`,
    })),
    {
      id: 'core',
      role: 'core',
      phase: null,
      trigger: null,
      inputs: [],
      outputs: ['runtime.v1'],
      file: 'tsjs-core.js',
    },
    ...authoredCatalog.map((entry, index) => {
      const generated = generatedCatalog.modules[index];
      if (
        !generated ||
        generated.id !== entry.id ||
        generated.phase !== entry.phase ||
        generated.trigger !== entry.trigger ||
        generated.include !== entry.include
      ) {
        fail(`generated catalog entry ${index} differs from current authored catalog`);
      }
      return {
        id: entry.id,
        role: 'integration',
        phase: entry.phase,
        trigger: entry.trigger,
        inputs: entry.consumes,
        outputs: entry.provides,
        file: `tsjs-${entry.id}.js`,
      };
    }),
  ];
  if (current.artifacts.length !== expectedArtifacts.length) {
    fail('current release does not match the live catalog artifact count');
  }

  const coreIndex = current.artifacts.findIndex(({ role }) => role === 'core');
  const providers = new Map([['runtime.v1', coreIndex]]);
  for (const [index, artifact] of current.artifacts.entries()) {
    if (artifact.role !== 'integration') continue;
    for (const capability of artifact.outputs) {
      if (!CAPABILITY_PATTERN.test(capability)) {
        fail(`current release has invalid output capability: ${capability}`);
      }
      if (providers.has(capability)) {
        fail(`current capability has multiple providers: ${capability}`);
      }
      providers.set(capability, index);
    }
  }
  let sawDeferred = false;
  for (const [index, artifact] of current.artifacts.entries()) {
    const expected = expectedArtifacts[index];
    for (const field of ['id', 'role', 'phase', 'trigger', 'file']) {
      if (artifact[field] !== expected[field]) {
        fail(`current release artifact ${index} ${field} differs from the live catalog`);
      }
    }
    if (
      artifact.role === 'bootstrap' &&
      (artifact.inputs.length !== 0 || artifact.outputs.length !== 0)
    ) {
      fail('current bootstrap invariant requires no inputs or outputs');
    }
    if (
      artifact.role === 'core' &&
      (artifact.inputs.length !== 0 || canonicalJson(artifact.outputs) !== '["runtime.v1"]')
    ) {
      fail('current core invariant requires only the runtime.v1 output');
    }
    if (artifact.role !== 'integration') continue;
    if (!artifact.inputs.includes('runtime.v1')) {
      fail(`current integration ${artifact.id} must consume runtime.v1`);
    }
    if (artifact.phase === 'deferred') {
      sawDeferred = true;
      if (artifact.outputs.length !== 0) {
        fail(`current deferred integration cannot provide capabilities: ${artifact.id}`);
      }
    } else if (sawDeferred) {
      fail('current takeover integration cannot follow a deferred integration');
    }
    for (const edge of artifact.inputs) {
      const parts = edge.split('?');
      if (
        parts.length > 2 ||
        !CAPABILITY_PATTERN.test(parts[0]) ||
        (parts.length === 2 && !CAPABILITY_PREDICATE_PATTERN.test(parts[1]))
      ) {
        fail(`current release has invalid input capability: ${edge}`);
      }
      const providerIndex = providers.get(parts[0]);
      if (providerIndex === undefined) fail(`current release has unknown capability: ${parts[0]}`);
      if (providerIndex >= index) {
        fail(`current capability provider must precede consumer: ${parts[0]}`);
      }
    }
    if (
      canonicalJson(artifact.inputs) !== canonicalJson(expected.inputs) ||
      canonicalJson(artifact.outputs) !== canonicalJson(expected.outputs)
    ) {
      fail(`current release artifact ${index} capabilities differ from authored catalog`);
    }
  }
}

function validateCapturedMembership(capture, historicalCatalog, label = 'capturedTransfer') {
  const artifactsById = new Map(
    capture.release.artifacts.map((artifact) => [artifact.id, artifact])
  );
  const idsByFile = new Map(capture.release.artifacts.map(({ id, file }) => [file, id]));
  const expectedFiles = deriveInventorySetFiles(capture.release.artifacts, historicalCatalog);
  const bootstrapFile = artifactsById.get('bootstrap')?.file;
  if (typeof bootstrapFile !== 'string') {
    fail(`${label} bootstrap release artifact is missing`);
  }
  const expected = {
    bootstrap: { artifactIds: ['bootstrap'], files: [bootstrapFile] },
    ...Object.fromEntries(
      SET_NAMES.map((setName) => [
        setName,
        {
          artifactIds: expectedFiles[setName].map((file) => idsByFile.get(file)),
          files: expectedFiles[setName],
        },
      ])
    ),
  };
  for (const setName of TRANSFER_SET_NAMES) {
    const set = capture.sets?.[setName];
    if (
      !set ||
      !Array.isArray(set.artifactIds) ||
      !Array.isArray(set.files) ||
      set.artifactIds.length !== set.files.length ||
      set.artifactIds.length === 0 ||
      new Set(set.artifactIds).size !== set.artifactIds.length ||
      new Set(set.files).size !== set.files.length ||
      canonicalJson(set.artifactIds) !== canonicalJson(expected[setName].artifactIds) ||
      canonicalJson(set.files) !== canonicalJson(expected[setName].files)
    ) {
      fail(`${label} ${setName} semantic membership is invalid`);
    }
    for (const [index, artifactId] of set.artifactIds.entries()) {
      const artifact = artifactsById.get(artifactId);
      if (!artifact || artifact.file !== set.files[index]) {
        fail(`${label} ${setName} release/set membership pair is invalid`);
      }
      if (setName === 'bootstrap' ? artifact.role !== 'bootstrap' : artifact.role === 'bootstrap') {
        fail(`${label} ${setName} release/set role membership is invalid`);
      }
    }
    if (
      setName === 'bootstrap' &&
      (set.artifactIds[0] !== 'bootstrap' || set.files[0] !== bootstrapFile)
    ) {
      fail(`${label} bootstrap semantic membership is invalid`);
    }
    for (const sizeName of SIZE_NAMES) {
      assertPositiveInteger(set[sizeName], `${label}.sets.${setName}.${sizeName}`);
    }
    if (!/^[0-9a-f]{64}$/.test(set.sha256)) {
      fail(`${label}.sets.${setName}.sha256 is invalid`);
    }
  }
}

function validateReductionCheckpoint(capture, intermediate) {
  const minimal = capture.sets.minimal;
  if (minimal.rawBytes > 220_000) {
    fail(`review remediation minimal.rawBytes exceeds 220000: ${minimal.rawBytes}`);
  }
  if (minimal.gzipBytes > 59_000) {
    fail(`review remediation minimal.gzipBytes exceeds 59000: ${minimal.gzipBytes}`);
  }
  if (minimal.brotliBytes >= intermediate.sets.minimal.brotliBytes) {
    fail('review remediation minimal.brotliBytes must improve on the intermediate capture');
  }
  for (const sizeName of SIZE_NAMES) {
    if (capture.sets.reference[sizeName] >= intermediate.sets.reference[sizeName]) {
      fail(`review remediation reference.${sizeName} must improve on the intermediate capture`);
    }
    if (capture.sets.maximal[sizeName] > intermediate.sets.maximal[sizeName]) {
      fail(`review remediation maximal.${sizeName} must not grow from the intermediate capture`);
    }
  }
}

/** Report current transfer sizes against an authenticated historical capture. */
export function buildTransferCaptureReport(captured, current) {
  const reports = {};
  for (const setName of TRANSFER_SET_NAMES) {
    reports[setName] = {};
    for (const sizeName of SIZE_NAMES) {
      const capturedBytes = captured?.[setName]?.[sizeName];
      const currentBytes = current?.[setName]?.[sizeName];
      assertPositiveInteger(capturedBytes, `captured.${setName}.${sizeName}`);
      assertPositiveInteger(currentBytes, `current.${setName}.${sizeName}`);
      reports[setName][sizeName] = {
        capturedBytes,
        currentBytes,
        deltaBytes: currentBytes - capturedBytes,
      };
    }
  }
  return reports;
}

/** Authenticate frozen evidence, then independently validate and report the current release. */
export function validateRoleCorrectTransfer({
  baseline,
  metrics,
  catalog,
  release,
  currentArtifactContents,
  verifyGitProvenance = false,
  authoredCatalog = loadAuthoredReleaseCatalog(),
  authoredFirstDisplayCatalog = loadAuthoredFirstDisplayCatalog(),
}) {
  const intermediate = baseline?.roleCorrectTransfer;
  if (!intermediate || typeof intermediate !== 'object') fail('role-correct capture is missing');
  const capture = baseline?.reviewRemediationTransfer;
  if (!capture || typeof capture !== 'object') fail('review-remediation capture is missing');
  const historical = Object.fromEntries(
    Object.entries(baseline).filter(
      ([key]) => key !== 'roleCorrectTransfer' && key !== 'reviewRemediationTransfer'
    )
  );
  const historicalDigest = canonicalJsonSha256(historical);
  if (historicalDigest !== HISTORICAL_EVIDENCE_SHA256) {
    fail('historical evidence digest does not match the immutable original top-level fields');
  }
  if (canonicalJsonSha256(intermediate) !== ROLE_CORRECT_CAPTURE_SHA256) {
    fail('role-correct capture digest does not match the immutable capture');
  }
  if (canonicalJsonSha256(capture) !== REVIEW_REMEDIATION_CAPTURE_SHA256) {
    fail('review-remediation capture digest does not match the immutable capture');
  }
  if (
    capture.originalTopLevelSha256 !== HISTORICAL_EVIDENCE_SHA256 ||
    intermediate.originalTopLevelSha256 !== HISTORICAL_EVIDENCE_SHA256
  ) {
    fail('historical evidence digest linkage is invalid');
  }
  if (capture.roleCorrectTransferSha256 !== ROLE_CORRECT_CAPTURE_SHA256) {
    fail('review-remediation linkage to the immutable intermediate capture is invalid');
  }
  if (verifyGitProvenance) {
    validateFrozenCaptureProvenance(intermediate, capture);
  }

  validateReleaseInventoryShape(intermediate.release, 'role-correct intermediate', true);
  validateReleaseInventoryShape(capture.release, 'captured', true);
  validateReleaseInventoryShape(release, 'current');
  validateCapturedMembership(
    intermediate,
    loadHistoricalReleaseCatalog(intermediate, 'roleCorrectTransfer'),
    'roleCorrectTransfer'
  );
  validateCapturedMembership(
    capture,
    loadHistoricalReleaseCatalog(capture, 'reviewRemediationTransfer'),
    'reviewRemediationTransfer'
  );
  validateReductionCheckpoint(capture, intermediate);
  validateCurrentReleaseSemantics(release, catalog, authoredCatalog, authoredFirstDisplayCatalog);
  validateSemanticBundleSets(metrics, release, catalog);
  const graphViolations = findProductionGraphViolations(metrics, release);
  if (graphViolations.length > 0)
    fail(`production graph failed:\n- ${graphViolations.join('\n- ')}`);
  if (currentArtifactContents !== undefined) {
    validateArtifactContents(release, currentArtifactContents);
    validateCurrentMeasurements(metrics, release, catalog, currentArtifactContents);
  }
  const currentSets = { bootstrap: metrics.bootstrap, ...metrics.sets };
  return {
    captureReports: {
      roleCorrectTransfer: buildTransferCaptureReport(intermediate.sets, currentSets),
      reviewRemediationTransfer: buildTransferCaptureReport(capture.sets, currentSets),
    },
    capture,
  };
}

function parseArgs(argv) {
  const options = { baselinePath: defaultBaselinePath };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === '--baseline') {
      const value = argv[index + 1];
      if (!value) fail('--baseline requires a path');
      options.baselinePath = path.resolve(value);
      index += 1;
    } else {
      fail(`unknown argument: ${argument}`);
    }
  }
  return options;
}

export function checkBundleBudgets({ baselinePath = defaultBaselinePath } = {}) {
  const baseline = readJson(baselinePath, 'baseline');
  const metrics = readJson(metricsPath, 'build metrics');
  const catalog = readJson(catalogPath, 'release catalog');
  const release = readJson(releasePath, 'release inventory');
  if (baseline.schemaVersion !== 1) fail('baseline.schemaVersion must equal 1');
  if (metrics.schemaVersion !== 1) fail('build metrics schemaVersion must equal 1');
  validateBudgetSets(baseline.bundles, 'baseline.bundles');
  validateSemanticBundleSets(metrics, release, catalog);

  const historicalDeltas = {};
  const historicalSets = {
    bootstrap: BOOTSTRAP_BASELINE,
    ...Object.fromEntries(SET_NAMES.map((setName) => [setName, baseline.bundles[setName]])),
  };
  const currentSets = { bootstrap: metrics.bootstrap, ...metrics.sets };
  for (const [setName, historical] of Object.entries(historicalSets)) {
    const current = currentSets[setName];
    historicalDeltas[setName] = Object.fromEntries(
      SIZE_NAMES.map((sizeName) => [
        sizeName,
        {
          historicalBytes: historical[sizeName],
          currentBytes: current[sizeName],
          deltaBytes: current[sizeName] - historical[sizeName],
        },
      ])
    );
  }

  const currentArtifactContents = new Map(
    release.artifacts.map(({ file }) => [
      file,
      fs.readFileSync(path.join(path.dirname(releasePath), file)),
    ])
  );
  const roleCorrect = validateRoleCorrectTransfer({
    baseline,
    metrics,
    catalog,
    release,
    currentArtifactContents,
    verifyGitProvenance: true,
  });
  const candidateArchitecture = buildCandidateArchitectureSizeReport({
    metrics,
    release,
    catalog,
    currentArtifactContents,
  });
  enforceCandidateArchitectureSizeCeilings(candidateArchitecture);

  return {
    baselinePath,
    roleCorrectStatus: 'immutable-intermediate',
    reviewRemediationStatus: 'immutable-report-only',
    transferCapturesEnforced: false,
    historicalDeltas,
    frozenTransferReports: roleCorrect.captureReports,
    candidateArchitecture,
    productionGraphReport: buildProductionGraphReport(metrics, release),
    sets: metrics.sets,
  };
}

/** Keep the blocking CLI report reviewable while full mask evidence stays in build metrics. */
export function summarizeBundleBudgetCommandReport(result) {
  const architecture = result?.candidateArchitecture;
  const firstDisplay = architecture?.firstDisplay;
  if (
    !architecture ||
    !firstDisplay ||
    !Array.isArray(firstDisplay.masks) ||
    !Array.isArray(firstDisplay.permittedMasks)
  ) {
    fail('candidate architecture report is missing first-display mask evidence');
  }
  return {
    ...result,
    candidateArchitecture: {
      ...architecture,
      firstDisplay: {
        reachableMaskCount: firstDisplay.masks.length,
        permittedMaskCount: firstDisplay.permittedMasks.length,
        named: firstDisplay.named,
      },
    },
  };
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const result = checkBundleBudgets(parseArgs(process.argv.slice(2)));
    process.stdout.write(
      `${JSON.stringify(summarizeBundleBudgetCommandReport(result), null, 2)}\n`
    );
  } catch (error) {
    process.stderr.write(`${error instanceof Error ? error.message : error}\n`);
    process.exitCode = 1;
  }
}
