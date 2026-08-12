import fs from 'node:fs';
import path from 'node:path';

const packageRoot = path.resolve(import.meta.dirname, '..');
const repositoryRoot = path.resolve(packageRoot, '../../..');
const extensions = new Set([
  '.css',
  '.js',
  '.json',
  '.md',
  '.mjs',
  '.rs',
  '.sh',
  '.toml',
  '.ts',
  '.tsx',
  '.yaml',
  '.yml',
]);
const ignoredDirectories = new Set(['.git', 'coverage', 'dist', 'node_modules', 'target']);

function relative(file) {
  return path.relative(repositoryRoot, file).replaceAll(path.sep, '/');
}

function collect(directory, files = []) {
  if (!fs.existsSync(directory)) return files;
  for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
    if (entry.isDirectory() && ignoredDirectories.has(entry.name)) continue;
    const target = path.join(directory, entry.name);
    if (entry.isDirectory()) collect(target, files);
    else if (extensions.has(path.extname(entry.name))) files.push(target);
  }
  return files;
}

function lineNumber(source, offset) {
  let line = 1;
  for (let index = 0; index < offset; index += 1) {
    if (source.charCodeAt(index) === 10) line += 1;
  }
  return line;
}

const jsPackageFiles = collect(packageRoot);
const shippedTsjsFiles = collect(path.join(packageRoot, 'src'));
const generatedTsjsFiles = collect(path.join(packageRoot, 'dist'));
const currentGuideFiles = collect(path.join(repositoryRoot, 'docs/guide'));
const browserTestFiles = collect(
  path.join(repositoryRoot, 'crates/trusted-server-integration-tests/browser')
);
const productionRustFiles = [
  'crates/trusted-server-core/src',
  'crates/trusted-server-adapter-fastly/src',
  'crates/trusted-server-adapter-axum/src',
  'crates/trusted-server-adapter-cloudflare/src',
  'crates/trusted-server-adapter-spin/src',
].flatMap((directory) => collect(path.join(repositoryRoot, directory)));
const thisScript = path.resolve(import.meta.filename);
const auxiliaryFiles = [
  ...jsPackageFiles,
  ...collect(path.join(repositoryRoot, 'crates/trusted-server-integration-tests')),
  ...collect(path.join(repositoryRoot, 'scripts')),
  ...collect(path.join(repositoryRoot, '.github/workflows')),
].filter((file) => path.resolve(file) !== thisScript);
const legacySurfaceFiles = [...shippedTsjsFiles, ...generatedTsjsFiles, ...currentGuideFiles];
const uniqueFiles = (files) => [...new Set(files)];
const violations = [];

function forbidSource(file, source, label, expression) {
  expression.lastIndex = 0;
  for (let match = expression.exec(source); match; match = expression.exec(source)) {
    violations.push(`${relative(file)}:${lineNumber(source, match.index)}: ${label}`);
    if (match[0].length === 0) expression.lastIndex += 1;
  }
}

function forbid(files, label, expression) {
  for (const file of uniqueFiles(files)) {
    const source = fs.readFileSync(file, 'utf8');
    forbidSource(file, source, label, expression);
  }
}

function token(...parts) {
  return parts.join('');
}

const oldRuntimePrefix = token('__', 'tsjs', '_');
const oldCreativeGlobal = token('ts', 'creative');
const legacyPublicTokens = [
  token('Legacy', 'TsjsApi'),
  token('TsjsApi', 'V1'),
  token('apsPrebid', 'Renderers'),
  token('render', 'AllAdUnits'),
  token('render', 'AdUnit'),
  token('render', 'Log'),
  token('render', 'Seq'),
  token('tsjs:', 'adRendered'),
  token('__tsRender', 'Generation'),
  token('__tsRender', 'Bid'),
  token('registerContext', 'Provider'),
  token('collect', 'Context'),
  token('install', 'Guards'),
];

forbid(legacySurfaceFiles, 'legacy window runtime flag', new RegExp(oldRuntimePrefix, 'g'));
forbid(
  legacySurfaceFiles,
  'legacy creative global',
  new RegExp(`(?:globalThis\\.)?${oldCreativeGlobal}|tsCreativeConfig`, 'g')
);
for (const name of legacyPublicTokens) {
  forbid(
    legacySurfaceFiles,
    `legacy TSJS surface ${name}`,
    new RegExp(name.replaceAll(/[.*+?^${}()|[\]\\]/g, '\\$&'), 'g')
  );
}
forbid(
  legacySurfaceFiles,
  'legacy public GPT diagnostics surface',
  /\b(?:window\.)?tsjs(?:\?\.|\.)gptDiagnostics\b/g
);

forbid(
  shippedTsjsFiles,
  'legacy mutable core configuration API',
  /export\s+function\s+(?:setConfig|getConfig)\b/g
);
forbid(
  shippedTsjsFiles,
  'temporary architecture lint allowlist',
  /LEGACY_(?:ADTECH_GLOBAL|RESTRICTED_IMPORT)_ALLOWLIST/g
);
forbid(
  shippedTsjsFiles,
  'integration-owned function sentinel',
  /__(?:tsInitialLoadConfigHooked|tsInitialLoadHooked|tsPushed|tsSlotHandoffPatched|tsApsBidResponseListenerInstalled|tsRefreshWrapped|tsRemoveAdUnitWrapped|tsRenderTraceInstalled|tsjsPrebidShimInstalled)\b/g
);
forbid(
  shippedTsjsFiles,
  'empty catch in migrated TSJS source',
  /catch\s*(?:\([^)]*\))?\s*\{\s*\}/g
);
const criticalTraceFile = path.join(packageRoot, 'src/core/trace.ts');
forbidSource(
  criticalTraceFile,
  fs.readFileSync(criticalTraceFile, 'utf8'),
  'critical render trace presentation leakage',
  /\b(?:Document|HTMLElement|MutationObserver)\b|createElement|getElementById|querySelector|clipboard|data-ts-/g
);
forbid(
  [...currentGuideFiles, ...browserTestFiles],
  'legacy mutable TSJS configuration call',
  /\btsjs(?:\?\.)?\.setConfig\s*\(/g
);
forbid(
  browserTestFiles,
  'legacy TSJS render function invocation',
  /\.tsjs(?:\?\.)?\.(?:renderAdUnit|renderAllAdUnits)\s*\(/g
);
forbid(
  productionRustFiles,
  'legacy window runtime flag in production documentation',
  /\/\/\/.*window\.__tsjs_.*/g
);

const routeAndConfigFiles = [...shippedTsjsFiles, ...currentGuideFiles];
forbid(routeAndConfigFiles, 'deprecated page-bids route', /\/__ts\/page-bids/g);
forbid(
  routeAndConfigFiles,
  'non-canonical APS renderer route',
  /\/integrations\/aps\/renderer(?!\/v1(?![A-Za-z0-9_./-]))/g
);
const apsIntegrationFile = path.join(
  repositoryRoot,
  'crates/trusted-server-core/src/integrations/aps.rs'
);
const apsIntegrationSource = fs.readFileSync(apsIntegrationFile, 'utf8');
forbidSource(
  apsIntegrationFile,
  apsIntegrationSource.split('\n#[cfg(test)]')[0] ?? apsIntegrationSource,
  'non-canonical APS renderer route',
  /\/integrations\/aps\/renderer(?!\/v1(?![A-Za-z0-9_./-]))/g
);
if (
  !apsIntegrationSource.includes(
    'pub const APS_RUNNER_ROUTE: &str = "/integrations/aps/runner.js";'
  )
) {
  violations.push(`${relative(apsIntegrationFile)}:1: missing canonical APS runner route`);
}
if (
  /APS_RUNNER_ROUTE:\s*&str\s*=\s*"\/integrations\/aps\/runner\/v1\.js"/.test(apsIntegrationSource)
) {
  violations.push(`${relative(apsIntegrationFile)}:1: versioned APS runner route is served`);
}
forbid(currentGuideFiles, 'APS pub_id compatibility alias', /\bpub_id\b/g);
forbidSource(
  apsIntegrationFile,
  apsIntegrationSource.split('\n#[cfg(test)]')[0] ?? apsIntegrationSource,
  'APS pub_id compatibility alias',
  /\bpub_id\b/g
);
forbid(
  [...shippedTsjsFiles, ...productionRustFiles, ...currentGuideFiles],
  'vendored or pinned APS runner asset',
  /include_(?:bytes|str)!?[^\n]*runner|APS_RUNNER_(?:ASSET|DIGEST|SRI|VERSION)|runner[_-]cache|offline[_ -]runner|prebid-creative\.js[^\n]*(?:digest|integrity|version)/gi
);
forbid(
  auxiliaryFiles,
  'APS runner downloader, updater, or pinned artifact metadata',
  /APS_RUNNER_(?:ASSET|DIGEST|SRI|VERSION)|runner[_-]cache|offline[_ -]runner|prebid-creative\.js[^\n]*(?:digest|integrity|version)|(?:download|update)[^\n]*prebid-creative\.js/gi
);

for (const manifest of [
  'crates/trusted-server-adapter-fastly/Cargo.toml',
  'crates/trusted-server-adapter-axum/Cargo.toml',
  'crates/trusted-server-adapter-cloudflare/Cargo.toml',
  'crates/trusted-server-adapter-spin/Cargo.toml',
]) {
  const source = fs.readFileSync(path.join(repositoryRoot, manifest), 'utf8');
  const defaultFeatures = source.match(/^default\s*=\s*\[([^\]]*)\]/m)?.[1] ?? '';
  if (defaultFeatures.includes('aps-runner-proxy-integration-test')) {
    violations.push(`${manifest}:1: APS proxy test hook is enabled in a production feature set`);
  }
}

const forbiddenFiles = [
  'crates/trusted-server-js/lib/src/core/context.ts',
  'crates/trusted-server-js/lib/src/core/request.ts',
  'crates/trusted-server-js/lib/test/core/context.test.ts',
  'crates/trusted-server-js/lib/test/core/trace.test.ts',
];
for (const file of forbiddenFiles) {
  if (fs.existsSync(path.join(repositoryRoot, file))) {
    violations.push(`${file}:1: unreachable legacy file remains`);
  }
}

const requiredReplacements = [
  ['crates/trusted-server-core/src/integrations/mod.rs', '_integrationConfig'],
  ['crates/trusted-server-js/lib/src/core/index.ts', '_integrationConfig'],
  ['crates/trusted-server-js/lib/src/integrations/didomi/module.ts', 'proxyPath'],
  ['crates/trusted-server-js/lib/src/integrations/prebid/module.ts', 'clientSideBidders'],
  ['crates/trusted-server-js/lib/src/integrations/sourcepoint/module.ts', 'rewriteSdk'],
  [
    'crates/trusted-server-js/lib/build-prebid-external.mjs',
    'assertNoLegacyRuntimeFlags(finalBundle)',
  ],
];
for (const [file, required] of requiredReplacements) {
  const source = fs.readFileSync(path.join(repositoryRoot, file), 'utf8');
  if (!source.includes(required)) {
    violations.push(`${file}:1: missing immutable boot replacement ${required}`);
  }
}

if (violations.length > 0) {
  console.error(`Hard-cutover absence check failed (${violations.length} violations):`);
  for (const violation of violations.sort()) console.error(`- ${violation}`);
  process.exitCode = 1;
} else {
  console.log('Hard-cutover absence check passed.');
}
