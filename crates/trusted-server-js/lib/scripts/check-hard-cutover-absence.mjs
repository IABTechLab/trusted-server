import fs from 'node:fs';
import path from 'node:path';

const packageRoot = path.resolve(import.meta.dirname, '..');
const repositoryRoot = path.resolve(packageRoot, '../../..');
const extensions = new Set([
  '.css',
  '.html',
  '.integrity',
  '.js',
  '.json',
  '.log',
  '.map',
  '.md',
  '.mjs',
  '.rs',
  '.sh',
  '.sha256',
  '.sri',
  '.toml',
  '.ts',
  '.tsx',
  '.txt',
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
const productionTsjsFiles = shippedTsjsFiles.filter(
  (file) => !/(?:_test|\.test)\.[cm]?[jt]sx?$/u.test(file)
);
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

const retiredRuntimeFiles = new Set([
  'src/composition/browser.ts',
  'src/composition/critical_transport.ts',
  'src/composition/index.ts',
  'src/core/bootstrap_controller.ts',
]);
const retiredCutoverExpressions = [
  [
    'non-canonical APS renderer route',
    /\/integrations\/aps\/renderer(?!\/v2(?![A-Za-z0-9_./-]))/gu,
  ],
  [
    'rendererUrl in a v4 Prebid response',
    /(?:message\s*:\s*['"]Prebid Response['"]|\[['"]message['"]\]\s*=\s*[^;\n]*prebidResponse)[\s\S]{0,512}(?:rendererVersion\s*:\s*['"]4['"]|\[['"]rendererVersion['"]\]\s*=)[\s\S]{0,512}\brendererUrl\b/gu,
  ],
  ['retired APS owner start payload', /['"]TS APS Start['"]/gu],
  ['raw TSJS integration global', /__tsjs_[A-Za-z0-9_]+/gu],
  ['retired bundle activation attribute', /data-ts-gam-attribution/gu],
  [
    'retired TSJS public surface',
    /\b(?:LegacyTsjsApi|TsjsApiV1|apsPrebidRenderers|renderAllAdUnits|renderAdUnit|renderLog|renderSeq|registerContextProvider|collectContext|installGuards)\b|tsjs:adRendered/gu,
  ],
  [
    'retired TSJS namespace member',
    /\btsjs(?:\.|\?\.)(?:adSlots|bids|getConfig|gptDiagnostics|renders|setConfig)\b/gu,
  ],
  ['retired TSJS public version', /\btsjs(?:\.|\?\.)version\s*=\s*['"]0\.1\.0['"]/gu],
  ['retired creative global', /\b(?:tscreative|tsCreativeConfig)\b/gu],
];

export function findCutoverTextViolations(file, source) {
  const normalized = file
    .replaceAll('\\', '/')
    .replace(/^.*\/crates\/trusted-server-js\/lib\//u, '');
  const found = [];
  if (retiredRuntimeFiles.has(normalized)) found.push('retired second runtime file');
  for (const [label, expression] of retiredCutoverExpressions) {
    expression.lastIndex = 0;
    if (expression.test(source)) found.push(label);
  }
  return found;
}

const forbiddenVendorBasename =
  /^(?:gpt|pubads_impl|prebid-creative|prebid-universal-creative(?:[._-].*)?)\.(?:[cm]?js|html)(?:\.(?:integrity|map|sha256|sri))?$/iu;
const vendorBodyExpressions = [
  /\/[*!]\s*(?:@license\s+)?[^\n]{0,160}\bPrebid Universal Creative\b/giu,
  /\/[*!][^\n]{0,160}@license[^\n]{0,160}\bGoogle Publisher Tag\b/giu,
  /\/[*!][^\n]{0,160}(?:Copyright|@license)[^\n]{0,160}\bAmazon Publisher Services\b/giu,
  /\b(?:apsRunner|gpt|puc)(?:Digest|Integrity|Sha(?:256|384|512)|Sri|Checksum|Version)\b/giu,
  /\b(?:digest|integrity|sha(?:256|384|512)|sri|checksum)\b[^\n]{0,160}\b(?:prebid-creative\.js|Google Publisher Tag|Prebid Universal Creative|Amazon Publisher Services)\b/giu,
  /\b(?:prebid-creative\.js|Google Publisher Tag|Prebid Universal Creative|Amazon Publisher Services)\b[^\n]{0,160}\b(?:digest|integrity|sha(?:256|384|512)|sri|checksum)\b/giu,
];

export function findVendorBoundaryViolations(file, source) {
  const normalized = file.replaceAll('\\', '/');
  const basename = path.posix.basename(normalized);
  const found = [];
  if (forbiddenVendorBasename.test(basename)) found.push('vendor distributable filename');
  for (const expression of vendorBodyExpressions) {
    expression.lastIndex = 0;
    if (expression.test(source)) found.push('stored vendor body, checksum, or version metadata');
  }
  return found;
}

for (const file of uniqueFiles([
  ...productionTsjsFiles,
  ...generatedTsjsFiles,
  ...currentGuideFiles,
  ...productionRustFiles,
])) {
  const source = fs.readFileSync(file, 'utf8');
  const productionSource = file.endsWith('.rs')
    ? (source.split('\n#[cfg(test)]')[0] ?? source)
    : source;
  for (const label of findCutoverTextViolations(relative(file), productionSource)) {
    violations.push(`${relative(file)}:1: ${label}`);
  }
}

const vendorBoundaryFiles = [
  ...productionTsjsFiles,
  ...generatedTsjsFiles,
  ...productionRustFiles,
  ...collect(path.join(repositoryRoot, 'crates/trusted-server-integration-tests/browser/fixtures')),
  ...collect(path.join(repositoryRoot, 'crates/trusted-server-integration-tests/fixtures')),
  ...collect(path.join(repositoryRoot, 'target/aps-tsjs-quality-evidence')),
  ...collect(path.join(repositoryRoot, 'target/aps-tsjs-cutover-evidence')),
  ...collect(
    path.join(repositoryRoot, 'crates/trusted-server-integration-tests/browser/real-gam-evidence')
  ),
  ...collect(
    path.join(repositoryRoot, 'crates/trusted-server-integration-tests/browser/playwright-report')
  ),
  ...collect(
    path.join(repositoryRoot, 'crates/trusted-server-integration-tests/browser/test-results')
  ),
];
for (const file of uniqueFiles(vendorBoundaryFiles)) {
  const source = fs.readFileSync(file, 'utf8');
  const productionSource = file.endsWith('.rs')
    ? (source.split('\n#[cfg(test)]')[0] ?? source)
    : source;
  for (const label of findVendorBoundaryViolations(relative(file), productionSource)) {
    violations.push(`${relative(file)}:1: ${label}`);
  }
}

const oldRuntimePrefix = token('__', 'tsjs', '_');
const oldCreativeGlobal = token('ts', 'creative');
const oldIntegrationConfigTransport = token('_', 'integration', 'Config');
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
forbid(
  [...legacySurfaceFiles, ...productionRustFiles],
  'legacy mutable integration config transport',
  new RegExp(oldIntegrationConfigTransport, 'g')
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
const coreTraceFile = path.join(packageRoot, 'src/core/trace.ts');
forbidSource(
  coreTraceFile,
  fs.readFileSync(coreTraceFile, 'utf8'),
  'core render trace presentation leakage',
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
  /\/integrations\/aps\/renderer(?!\/v2(?![A-Za-z0-9_./-]))/g
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
  /\/integrations\/aps\/renderer(?!\/v2(?![A-Za-z0-9_./-]))/g
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
const browserPackageManifests = [
  'crates/trusted-server-integration-tests/browser/package.json',
  'crates/trusted-server-integration-tests/browser/package-lock.json',
];
for (const manifest of browserPackageManifests) {
  const source = fs.readFileSync(path.join(repositoryRoot, manifest), 'utf8');
  if (source.includes('prebid-universal-creative')) {
    violations.push(`${manifest}:1: PUC package is vendored into the local harness`);
  }
}
const realGamNetworkFile = path.join(
  repositoryRoot,
  'crates/trusted-server-integration-tests/browser/helpers/gam-test-network.ts'
);
const realGamNetworkSource = fs.readFileSync(realGamNetworkFile, 'utf8');
if (!/export const REAL_GAM_PUC_RELEASE = ['"]1\.17\.2['"];/u.test(realGamNetworkSource)) {
  violations.push(
    `${relative(realGamNetworkFile)}:1: protected conformance metadata must pin PUC 1.17.2`
  );
}
forbidSource(
  realGamNetworkFile,
  realGamNetworkSource,
  'PUC conformance metadata must not carry vendor bytes, URLs, or checksums',
  /\bPUC_(?:ASSET|BODY|DIGEST|INTEGRITY|SCRIPT|SRI|URL|VERSION)\b/gu
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
  'crates/trusted-server-integration-tests/src/bin/generate-tsjs-prospective-fixture.rs',
  'crates/trusted-server-js/lib/src/composition/browser.ts',
  'crates/trusted-server-js/lib/src/composition/critical_transport.ts',
  'crates/trusted-server-js/lib/src/composition/index.ts',
  'crates/trusted-server-js/lib/src/core/bootstrap_controller.ts',
  'crates/trusted-server-js/lib/src/core/context.ts',
  'crates/trusted-server-js/lib/src/core/request.ts',
  'crates/trusted-server-js/lib/src/first_display/contracts.ts',
  'crates/trusted-server-js/lib/src/first_display/handoff.ts',
  'crates/trusted-server-js/lib/src/first_display/registration.ts',
  'crates/trusted-server-js/lib/src/first_display/transaction.ts',
  'crates/trusted-server-js/lib/src/integrations/gpt/bootstrap_fallback.ts',
  'crates/trusted-server-js/lib/test/core/context.test.ts',
  'crates/trusted-server-js/lib/test/core/bootstrap_controller.test.ts',
  'crates/trusted-server-js/lib/test/core/trace.test.ts',
];
for (const file of forbiddenFiles) {
  if (fs.existsSync(path.join(repositoryRoot, file))) {
    violations.push(`${file}:1: unreachable legacy file remains`);
  }
}

const requiredReplacements = [
  ['crates/trusted-server-core/src/tsjs.rs', 'IntegrationConfigsV1'],
  ['crates/trusted-server-js/lib/src/core/index.ts', '_claimBootSnapshot'],
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

const executedAsMain =
  process.argv[1] !== undefined &&
  path.resolve(process.argv[1]) === path.resolve(import.meta.filename);
if (violations.length > 0) {
  console.error(`Hard-cutover absence check failed (${violations.length} violations):`);
  for (const violation of violations.sort()) console.error(`- ${violation}`);
  process.exitCode = 1;
} else if (executedAsMain) {
  console.log('Hard-cutover absence check passed.');
}
