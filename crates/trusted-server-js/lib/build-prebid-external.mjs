#!/usr/bin/env node

/**
 * Build a publisher-specific external Prebid bundle.
 *
 * Unlike build-all.mjs, this script is intended to run outside the Cargo build.
 * It produces an immutable bundle and manifest that can be hosted on an asset
 * CDN, then referenced by integrations.prebid.external_bundle_url.
 */

import crypto from 'node:crypto';
import fs from 'node:fs';
import { createRequire } from 'node:module';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);
const srcDir = path.resolve(__dirname, 'src');
const integrationsDir = path.join(srcDir, 'integrations');
const prebidDir = path.join(integrationsDir, 'prebid');

const MODULES_GENERATED_SPECIFIER = './_modules.generated';
const USER_ID_REGISTRY_FILE = path.join(prebidDir, 'user_id_modules.json');
const PREBID_LOCK_FILE = path.join(__dirname, 'package-lock.json');
const PREBID_PACKAGE_DIR = path.join(__dirname, 'node_modules', 'prebid.js');
const PREBID_PACKAGE_JSON = path.join(PREBID_PACKAGE_DIR, 'package.json');
const PREBID_METADATA_DIR = path.join(PREBID_PACKAGE_DIR, 'metadata', 'modules');
const LIVE_INTENT_SHIM_ALIAS = 'prebid.js/modules/liveIntentIdSystem.js';
const PREBID_LIVE_INTENT_STANDARD = path.join(
  PREBID_PACKAGE_DIR,
  'dist',
  'src',
  'libraries',
  'liveIntentId',
  'idSystem.js'
);
const PREBID_GLOBAL_MODULE = path.join(PREBID_PACKAGE_DIR, 'dist', 'src', 'src', 'prebidGlobal.js');
const LIVE_INTENT_SHIM = path.join(prebidDir, 'prebid_modules', 'liveIntentIdSystem.ts');
const SHIM_WATCHDOG_DELAY_MS = 5000;
const MODULE_STEM_PATTERN = /^[A-Za-z0-9_-]+$/;
const MODULE_KIND_DEFINITIONS = Object.freeze([
  {
    requestKey: 'bidder',
    tomlKey: 'bidder',
    fieldPath: 'integrations.prebid.bundle.modules.bidder',
    metadataType: 'bidder',
  },
  {
    requestKey: 'userId',
    tomlKey: 'user_id',
    fieldPath: 'integrations.prebid.bundle.modules.user_id',
    metadataType: 'userId',
  },
  {
    requestKey: 'analytics',
    tomlKey: 'analytics',
    fieldPath: 'integrations.prebid.bundle.modules.analytics',
    metadataType: 'analytics',
  },
]);
const MODULE_KIND_BY_REQUEST_KEY = new Map(
  MODULE_KIND_DEFINITIONS.map((definition) => [definition.requestKey, definition])
);

function buildError(message) {
  return new Error(`[build-prebid-external] ${message}`);
}

function readJsonFile(filePath, description) {
  let contents;
  try {
    contents = fs.readFileSync(filePath, 'utf8');
  } catch (error) {
    throw buildError(`could not read ${description} ${filePath}: ${error.message}`);
  }

  try {
    return JSON.parse(contents);
  } catch (error) {
    throw buildError(`could not parse ${description} ${filePath}: ${error.message}`);
  }
}

function isRecord(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function moduleListError(definition, message) {
  throw buildError(`${definition.fieldPath} ${message}`);
}

function validateModuleList(value, definition, { required = false } = {}) {
  if (value === undefined) {
    if (required) {
      moduleListError(definition, 'is required and must contain at least one module stem');
    }
    return undefined;
  }
  if (!Array.isArray(value)) {
    moduleListError(definition, 'must be an array of module stems');
  }
  if (required && value.length === 0) {
    moduleListError(definition, 'must contain at least one module stem');
  }

  const seen = new Set();
  return value.map((stem) => {
    if (typeof stem !== 'string' || !MODULE_STEM_PATTERN.test(stem)) {
      moduleListError(
        definition,
        `contains invalid module stem ${JSON.stringify(stem)}; use the exact upstream filename without .js`
      );
    }
    if (seen.has(stem)) {
      moduleListError(definition, `contains duplicate module stem "${stem}"`);
    }
    seen.add(stem);
    return stem;
  });
}

function validateCrossKindDuplicates(selection) {
  const ownerByStem = new Map();
  for (const definition of MODULE_KIND_DEFINITIONS) {
    for (const stem of selection[definition.requestKey] ?? []) {
      const previous = ownerByStem.get(stem);
      if (previous) {
        throw buildError(
          `${definition.fieldPath} repeats module stem "${stem}" already selected by ${previous.fieldPath}`
        );
      }
      ownerByStem.set(stem, definition);
    }
  }
}

export function parseModuleRequest(rawJson) {
  let request;
  try {
    request = JSON.parse(rawJson);
  } catch (error) {
    throw buildError(`--modules-json must contain valid JSON: ${error.message}`);
  }

  if (!isRecord(request)) {
    throw buildError('--modules-json must contain a JSON object');
  }

  const supportedKeys = new Set(MODULE_KIND_DEFINITIONS.map(({ requestKey }) => requestKey));
  const unknownKeys = Object.keys(request).filter((key) => !supportedKeys.has(key));
  if (unknownKeys.length > 0) {
    throw buildError(`--modules-json contains unsupported field "${unknownKeys[0]}"`);
  }

  const selection = {
    bidder: validateModuleList(request.bidder, MODULE_KIND_BY_REQUEST_KEY.get('bidder'), {
      required: true,
    }),
    userId: validateModuleList(request.userId, MODULE_KIND_BY_REQUEST_KEY.get('userId')),
    analytics: validateModuleList(request.analytics, MODULE_KIND_BY_REQUEST_KEY.get('analytics')),
  };
  validateCrossKindDuplicates(selection);
  return selection;
}

export function normalizeModuleRequest(request, defaultUserIdModules) {
  const normalized = {
    bidder: [...request.bidder],
    userId: [...(request.userId ?? defaultUserIdModules)],
    analytics: [...(request.analytics ?? [])],
  };

  for (const definition of MODULE_KIND_DEFINITIONS) {
    validateModuleList(normalized[definition.requestKey], definition, {
      required: definition.requestKey === 'bidder',
    });
  }
  validateCrossKindDuplicates(normalized);
  return normalized;
}

export function parseArgs(argv) {
  const options = new Map();
  const supportedOptions = new Set(['modules-json', 'out']);

  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (!argument.startsWith('--')) {
      throw buildError(`Unexpected positional argument: ${argument}`);
    }

    const equalsIndex = argument.indexOf('=');
    const key = equalsIndex === -1 ? argument.slice(2) : argument.slice(2, equalsIndex);
    if (!supportedOptions.has(key)) {
      throw buildError(`Unknown option --${key}`);
    }
    if (options.has(key)) {
      throw buildError(`Option --${key} may only be specified once`);
    }

    const inlineValue = equalsIndex === -1 ? undefined : argument.slice(equalsIndex + 1);
    const value = inlineValue ?? argv[index + 1];
    if (!value || (inlineValue === undefined && value.startsWith('--'))) {
      throw buildError(`Missing value for --${key}`);
    }
    if (inlineValue === undefined) {
      index += 1;
    }
    options.set(key, value);
  }

  const rawModules = options.get('modules-json');
  if (rawModules === undefined) {
    throw buildError('Missing required --modules-json');
  }

  return {
    moduleRequest: parseModuleRequest(rawModules),
    outDir: path.resolve(process.cwd(), options.get('out') ?? path.join('..', 'dist', 'prebid')),
  };
}

export function verifyPrebidPackageVersion({
  lockFile = PREBID_LOCK_FILE,
  packageJsonFile = PREBID_PACKAGE_JSON,
} = {}) {
  const lock = readJsonFile(lockFile, 'npm lockfile');
  const installedPackage = readJsonFile(packageJsonFile, 'installed Prebid package manifest');
  const lockedVersion = lock?.packages?.['node_modules/prebid.js']?.version;
  const installedVersion = installedPackage?.version;

  if (typeof lockedVersion !== 'string' || lockedVersion.length === 0) {
    throw buildError(
      `${lockFile} does not declare packages["node_modules/prebid.js"].version; run \`npm ci\` in crates/trusted-server-js/lib and retry`
    );
  }
  if (typeof installedVersion !== 'string' || installedVersion.length === 0) {
    throw buildError(
      `${packageJsonFile} does not declare a Prebid version; run \`npm ci\` in crates/trusted-server-js/lib and retry`
    );
  }
  if (lockedVersion !== installedVersion) {
    throw buildError(
      `installed prebid.js version ${installedVersion} does not match package-lock.json version ${lockedVersion}; run \`npm ci\` in crates/trusted-server-js/lib and retry`
    );
  }

  return installedVersion;
}

function readUserIdRegistry(registryFile = USER_ID_REGISTRY_FILE) {
  const registry = readJsonFile(registryFile, 'Trusted Server User ID registry');
  if (!Array.isArray(registry?.defaultPreset) || !Array.isArray(registry?.modules)) {
    throw buildError(`${registryFile} must contain defaultPreset and modules arrays`);
  }
  return registry;
}

function isContainedPath(root, candidate) {
  const relative = path.relative(root, candidate);
  return (
    relative === '' ||
    (!path.isAbsolute(relative) && relative !== '..' && !relative.startsWith(`..${path.sep}`))
  );
}

function requireRegularContainedFile(filePath, rootPath, description) {
  let canonicalRoot;
  let canonicalFile;
  try {
    if (!fs.lstatSync(filePath).isFile()) {
      throw new Error('path is not a regular file');
    }
    canonicalRoot = fs.realpathSync(rootPath);
    canonicalFile = fs.realpathSync(filePath);
  } catch (error) {
    throw buildError(`${description} could not be resolved at ${filePath}: ${error.message}`);
  }

  if (!isContainedPath(canonicalRoot, canonicalFile)) {
    throw buildError(`${description} resolves outside ${canonicalRoot}: ${canonicalFile}`);
  }
  if (!fs.statSync(canonicalFile).isFile()) {
    throw buildError(`${description} is not a regular file: ${canonicalFile}`);
  }
  return canonicalFile;
}

function unsupportedModuleError(definition, stem, prebidVersion, cause) {
  const expectedPath = `modules/${stem}.js`;
  const suffix = cause ? ` (${cause})` : '';
  const category = definition.requestKey === 'userId' ? 'User ID' : definition.tomlKey;
  const article = definition.requestKey === 'analytics' ? 'an' : 'a';
  return buildError(
    `${definition.fieldPath} requested "${stem}", but prebid.js ${prebidVersion} does not provide ${expectedPath}${suffix}. Choose ${article} ${category} module shipped by the pinned prebid.js package; local paths and URLs are unsupported.`
  );
}

function resolveSpecifierFromInstalledPackage(specifier) {
  return require.resolve(specifier, { paths: [__dirname] });
}

function resolveOneModule(stem, definition, context) {
  const metadataFilename = `${stem}.json`;
  const metadataPath = path.join(context.metadataDir, metadataFilename);
  const metadataEntries = fs.readdirSync(context.metadataDir);
  if (!metadataEntries.includes(metadataFilename)) {
    throw unsupportedModuleError(definition, stem, context.prebidVersion);
  }

  let canonicalMetadataRoot;
  let canonicalMetadataPath;
  try {
    canonicalMetadataRoot = fs.realpathSync(context.metadataDir);
    canonicalMetadataPath = fs.realpathSync(metadataPath);
  } catch (error) {
    throw unsupportedModuleError(definition, stem, context.prebidVersion, error.message);
  }
  if (path.dirname(canonicalMetadataPath) !== canonicalMetadataRoot) {
    throw buildError(
      `${definition.fieldPath} requested "${stem}", but metadata ${metadataPath} resolves outside the pinned metadata directory`
    );
  }
  if (!fs.statSync(canonicalMetadataPath).isFile()) {
    throw buildError(
      `${definition.fieldPath} requested "${stem}", but metadata ${metadataPath} is not a regular file`
    );
  }

  const metadata = readJsonFile(
    canonicalMetadataPath,
    `Prebid metadata for ${definition.fieldPath} stem "${stem}"`
  );
  if (!Array.isArray(metadata?.components)) {
    throw buildError(
      `${definition.fieldPath} requested "${stem}", but metadata ${canonicalMetadataPath} has no components array`
    );
  }

  const matchingComponents = metadata.components.filter(
    (component) => isRecord(component) && component.componentType === definition.metadataType
  );
  if (matchingComponents.length === 0) {
    const declaredTypes = [
      ...new Set(
        metadata.components
          .map((component) => (isRecord(component) ? component.componentType : undefined))
          .filter((componentType) => typeof componentType === 'string')
      ),
    ].sort();
    const declaration =
      declaredTypes.length === 0 ? 'no recognized component type' : declaredTypes.join(', ');
    throw buildError(
      `${definition.fieldPath} requested "${stem}", but metadata ${canonicalMetadataPath} declares ${declaration} rather than ${definition.metadataType}`
    );
  }

  const runtimeCodes = [];
  for (const component of matchingComponents) {
    if (typeof component.componentName !== 'string' || component.componentName.length === 0) {
      throw buildError(
        `${definition.fieldPath} requested "${stem}", but metadata ${canonicalMetadataPath} has a ${definition.metadataType} component without a non-empty componentName`
      );
    }
    runtimeCodes.push(component.componentName);
  }

  const specifier = `prebid.js/modules/${stem}.js`;
  let resolvedTarget;
  try {
    resolvedTarget = context.resolveSpecifier(specifier);
  } catch (error) {
    throw unsupportedModuleError(definition, stem, context.prebidVersion, error.message);
  }

  let canonicalTarget;
  try {
    canonicalTarget = fs.realpathSync(resolvedTarget);
  } catch (error) {
    throw unsupportedModuleError(definition, stem, context.prebidVersion, error.message);
  }
  if (!isContainedPath(context.canonicalPackageDir, canonicalTarget)) {
    throw buildError(
      `${definition.fieldPath} requested "${stem}", but ${specifier} resolves outside the pinned prebid.js package: ${canonicalTarget}`
    );
  }
  if (!fs.statSync(canonicalTarget).isFile()) {
    throw buildError(
      `${definition.fieldPath} requested "${stem}", but ${specifier} does not resolve to a regular file: ${canonicalTarget}`
    );
  }

  return {
    stem,
    specifier,
    runtimeCodes: [...new Set(runtimeCodes)].sort(),
  };
}

function validateLiveIntentTargets({
  packageDir,
  liveIntentShim,
  liveIntentStandard,
  prebidGlobal,
}) {
  requireRegularContainedFile(liveIntentShim, prebidDir, 'LiveIntent ESM shim');
  requireRegularContainedFile(
    liveIntentStandard,
    packageDir,
    'Prebid LiveIntent standard ESM module'
  );
  requireRegularContainedFile(prebidGlobal, packageDir, 'Prebid global module');
}

export function resolveBundleModules(
  selection,
  {
    prebidVersion,
    registry,
    metadataDir = PREBID_METADATA_DIR,
    packageDir = PREBID_PACKAGE_DIR,
    resolveSpecifier = resolveSpecifierFromInstalledPackage,
    liveIntentShim = LIVE_INTENT_SHIM,
    liveIntentStandard = PREBID_LIVE_INTENT_STANDARD,
    prebidGlobal = PREBID_GLOBAL_MODULE,
  }
) {
  let canonicalPackageDir;
  let canonicalMetadataDir;
  try {
    canonicalPackageDir = fs.realpathSync(packageDir);
    canonicalMetadataDir = fs.realpathSync(metadataDir);
  } catch (error) {
    throw buildError(
      `could not resolve the pinned prebid.js package and metadata directories; run \`npm ci\` in crates/trusted-server-js/lib and retry: ${error.message}`
    );
  }
  if (!fs.statSync(canonicalPackageDir).isDirectory()) {
    throw buildError(`pinned prebid.js package root is not a directory: ${canonicalPackageDir}`);
  }
  if (
    !isContainedPath(canonicalPackageDir, canonicalMetadataDir) ||
    !fs.statSync(canonicalMetadataDir).isDirectory()
  ) {
    throw buildError(
      `Prebid metadata directory ${metadataDir} must be a directory contained by the pinned package root ${canonicalPackageDir}; run \`npm ci\` in crates/trusted-server-js/lib and retry`
    );
  }

  const userIdModules = new Set(registry.modules.map((entry) => entry.moduleName));
  const resolved = { bidder: [], userId: [], analytics: [] };

  for (const definition of MODULE_KIND_DEFINITIONS) {
    for (const stem of selection[definition.requestKey]) {
      if (definition.requestKey === 'userId' && !userIdModules.has(stem)) {
        throw buildError(
          `${definition.fieldPath} requested "${stem}", but Trusted Server has no User ID registry entry for it`
        );
      }
      resolved[definition.requestKey].push(
        resolveOneModule(stem, definition, {
          prebidVersion,
          metadataDir: canonicalMetadataDir,
          resolveSpecifier,
          canonicalPackageDir,
        })
      );
    }
  }

  if (selection.userId.includes('liveIntentIdSystem')) {
    validateLiveIntentTargets({
      packageDir,
      liveIntentShim,
      liveIntentStandard,
      prebidGlobal,
    });
  }

  return resolved;
}

export function createSelectionManifest(resolvedModules) {
  const runtimeCodes = (kind) =>
    [...new Set(resolvedModules[kind].flatMap((module) => module.runtimeCodes))].sort();

  return {
    schemaVersion: 1,
    modules: {
      bidder: resolvedModules.bidder.map(({ stem }) => stem),
      userId: resolvedModules.userId.map(({ stem }) => stem),
      analytics: resolvedModules.analytics.map(({ stem }) => stem),
    },
    runtimeCodes: {
      bidder: runtimeCodes('bidder'),
      analytics: runtimeCodes('analytics'),
    },
  };
}

export function renderGeneratedModules(resolvedModules, selectionManifest) {
  const imports = MODULE_KIND_DEFINITIONS.flatMap(({ requestKey }) =>
    resolvedModules[requestKey].map(({ specifier }) => `import '${specifier}';`)
  );

  return [
    '// Auto-generated by build-prebid-external.mjs.',
    '// Selected module imports are validated against the pinned Prebid package.',
    '',
    ...imports,
    '',
    `export const PREBID_BUNDLE_SELECTION = ${JSON.stringify(selectionManifest)} as const;`,
    '',
  ].join('\n');
}

export function renderExternalEntry({ includeUserIdModules }) {
  return [
    '// Auto-generated by build-prebid-external.mjs.',
    '//',
    '// Pure Prebid.js external bundle. The Trusted Server Prebid shim installs',
    '// the trustedServer adapter and owns queue processing. This bundle only',
    '// drains the queue through the watchdog when that shim does not install.',
    "import 'prebid.js';",
    "import 'prebid.js/modules/consentManagementTcf.js';",
    "import 'prebid.js/modules/consentManagementGpp.js';",
    "import 'prebid.js/modules/consentManagementUsp.js';",
    ...(includeUserIdModules ? ["import 'prebid.js/modules/userId.js';"] : []),
    `import { PREBID_BUNDLE_SELECTION } from '${MODULES_GENERATED_SPECIFIER}';`,
    '',
    'const bundleWindow = window as unknown as {',
    '  __tsjs_prebid_bundle?: unknown;',
    '  __tsjsPrebidShimInstalled?: boolean;',
    '  pbjs?: { processQueue?: () => void };',
    '};',
    'bundleWindow.__tsjs_prebid_bundle = Object.freeze(PREBID_BUNDLE_SELECTION);',
    '',
    '// The shim can fail independently because of CSP, blocking, or a separate',
    '// asset error. Drain the publisher queue after the grace period in that case.',
    'setTimeout(() => {',
    '  if (!bundleWindow.__tsjsPrebidShimInstalled) {',
    '    bundleWindow.pbjs?.processQueue?.();',
    '  }',
    `}, ${SHIM_WATCHDOG_DELAY_MS});`,
    '',
  ].join('\n');
}

function createTemporaryModulePaths() {
  const temporaryDir = fs.mkdtempSync(path.join(prebidDir, '.external-generated-'));
  return {
    temporaryDir,
    modulesFile: path.join(temporaryDir, '_modules.generated.ts'),
    entryFile: path.join(temporaryDir, '_external_entry.generated.ts'),
  };
}

export function deriveBundleMetadata(bundleBytes) {
  const sha256 = crypto.createHash('sha256').update(bundleBytes).digest('hex');
  const sri = `sha384-${crypto.createHash('sha384').update(bundleBytes).digest('base64')}`;
  const filename = `trusted-prebid-${sha256}.js`;

  return { filename, sha256, sri };
}

async function buildExternalBundle(outDir, generatedModules) {
  fs.mkdirSync(outDir, { recursive: true });

  const temporaryFile = `trusted-prebid-${process.pid}-${crypto.randomUUID()}.tmp.js`;
  const temporaryPath = path.join(outDir, temporaryFile);
  let renamedTemporaryFile = false;

  try {
    const { build } = await import('vite');

    await build({
      configFile: false,
      root: __dirname,
      resolve: {
        alias: [
          { find: LIVE_INTENT_SHIM_ALIAS, replacement: LIVE_INTENT_SHIM },
          { find: 'prebid.js/modules/liveIntentIdSystem', replacement: LIVE_INTENT_SHIM },
          {
            find: 'tsjs-prebid/liveIntentIdSystemStandard',
            replacement: PREBID_LIVE_INTENT_STANDARD,
          },
          { find: 'tsjs-prebid/prebidGlobal', replacement: PREBID_GLOBAL_MODULE },
          {
            find: 'prebid.js/src/adapterManager.js',
            replacement: path.resolve(
              __dirname,
              'node_modules/prebid.js/dist/src/src/adapterManager.js'
            ),
          },
          {
            find: 'prebid.js/src/adRendering.js',
            replacement: path.resolve(
              __dirname,
              'node_modules/prebid.js/dist/src/src/adRendering.js'
            ),
          },
        ],
      },
      build: {
        emptyOutDir: false,
        outDir,
        assetsDir: '.',
        sourcemap: false,
        minify: 'esbuild',
        rollupOptions: {
          input: generatedModules.entryFile,
          output: {
            format: 'iife',
            dir: outDir,
            entryFileNames: temporaryFile,
            inlineDynamicImports: true,
            extend: false,
            name: 'tsjs_prebid_external',
          },
        },
      },
      logLevel: 'warn',
    });

    const bundleBytes = fs.readFileSync(temporaryPath);
    const metadata = deriveBundleMetadata(bundleBytes);
    const finalPath = path.join(outDir, metadata.filename);

    fs.rmSync(finalPath, { force: true });
    fs.renameSync(temporaryPath, finalPath);
    renamedTemporaryFile = true;

    return metadata;
  } finally {
    if (!renamedTemporaryFile) {
      fs.rmSync(temporaryPath, { force: true });
    }
  }
}

export async function main(
  argv = process.argv.slice(2),
  {
    lockFile = PREBID_LOCK_FILE,
    packageJsonFile = PREBID_PACKAGE_JSON,
    registryFile = USER_ID_REGISTRY_FILE,
    metadataDir = PREBID_METADATA_DIR,
    packageDir = PREBID_PACKAGE_DIR,
    resolveSpecifier = resolveSpecifierFromInstalledPackage,
    liveIntentShim = LIVE_INTENT_SHIM,
    liveIntentStandard = PREBID_LIVE_INTENT_STANDARD,
    prebidGlobal = PREBID_GLOBAL_MODULE,
    createGeneratedPaths = createTemporaryModulePaths,
    buildBundle = buildExternalBundle,
  } = {}
) {
  const args = parseArgs(argv);
  const prebidVersion = verifyPrebidPackageVersion({ lockFile, packageJsonFile });
  const registry = readUserIdRegistry(registryFile);
  const selection = normalizeModuleRequest(args.moduleRequest, registry.defaultPreset);
  const resolvedModules = resolveBundleModules(selection, {
    prebidVersion,
    registry,
    metadataDir,
    packageDir,
    resolveSpecifier,
    liveIntentShim,
    liveIntentStandard,
    prebidGlobal,
  });
  const selectionManifest = createSelectionManifest(resolvedModules);
  const generatedModules = createGeneratedPaths();

  try {
    fs.writeFileSync(
      generatedModules.modulesFile,
      renderGeneratedModules(resolvedModules, selectionManifest)
    );
    fs.writeFileSync(
      generatedModules.entryFile,
      renderExternalEntry({ includeUserIdModules: selection.userId.length > 0 })
    );

    const bundle = await buildBundle(args.outDir, generatedModules);
    const manifest = {
      schemaVersion: 1,
      prebidVersion,
      modules: selectionManifest.modules,
      runtimeCodes: selectionManifest.runtimeCodes,
      sha256: bundle.sha256,
      sri: bundle.sri,
      filename: bundle.filename,
    };

    fs.mkdirSync(args.outDir, { recursive: true });
    fs.writeFileSync(
      path.join(args.outDir, 'manifest.json'),
      `${JSON.stringify(manifest, null, 2)}\n`
    );

    console.log('[build-prebid-external] Built external Prebid bundle:', bundle.filename);
    console.log('[build-prebid-external] SHA-256:', bundle.sha256);
    console.log('[build-prebid-external] SRI:', bundle.sri);
    console.log('[build-prebid-external] Manifest:', path.join(args.outDir, 'manifest.json'));
  } finally {
    fs.rmSync(generatedModules.temporaryDir, { recursive: true, force: true });
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  await main();
}
