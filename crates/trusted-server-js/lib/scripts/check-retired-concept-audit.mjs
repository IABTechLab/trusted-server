import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';

const MANIFEST_FENCE = /```json retired-rcjuly-tsjs-concept-manifest-v1\n([\s\S]*?)\n```/g;
const LEDGER_ID = /\| `(RCJ-[A-Z]+-[0-9]+)`/g;
const QUALITY_ID = 'RCJ-QUAL-01';
const SOURCE_ROOT = 'crates/trusted-server-js/lib/src/';
const SHA_40 = /^[0-9a-f]{40}$/;
const SHA_256 = /^[0-9a-f]{64}$/;
const RECORDED_RETIRED_SNAPSHOT = '905984e62a0858c53d9f0ff6dd3a1bf190cf311d';
const RECORDED_INVENTORY_SHA256 =
  'b1e28c8b30f0b8d95e38c0f8f57394df4ad43f760ae7abf5631e2054228aef08';
const RECORDED_MAIN_AUDIT_SHA = 'f6a2fb85ce623bf8a574e3941e1ee349acc3412d';
const HISTORICAL_PERFORMANCE_SHA256 =
  'fe5d7f52dc47dc9608ca6b92b036c6b971845e6424b157768dc9403a62d2d6b4';
const CLASSIFICATIONS = new Set(['main-owned', 'implementation-gap']);
const RESULTS = new Set(['pass', 'fail']);
const DISPOSITIONS = new Set(['preserve', 'rebuild', 'supersede', 'exclude']);
const CLASSIFICATION_KEYS = [
  'classification',
  'command',
  'disposition',
  'id',
  'mainSha',
  'ownerPaths',
  'result',
  'testPath',
];
const RETIRED_SOURCE_REFERENCE = /(?:rc[/-]july|905984e62a0858c53d9f0ff6dd3a1bf190cf311d)/i;
const EXECUTABLE_SHELL_FENCE =
  /^ {0,3}```(?:bash|sh|shell)(?:[ \t][^\n]*)?\r?\n([\s\S]*?)^ {0,3}```[ \t]*$/gim;
const RETIRED_OPERATION =
  /(?:\bgit\b[^\r\n]*\b(?:fetch|merge|rebase|cherry-pick|checkout|worktree|archive|diff)\b|\b(?:build|bench(?:mark)?|compare|hyperfine|perf(?:ormance)?|time)\b)/i;
const RETIRED_RENAME_SOURCE = /(?:check-rc-july-adoption\.mjs|rc-july-adoption\.test\.mjs)/gi;
const INFORMATIONAL_SHELL_COMMAND = /^(?:echo|printf)\b/i;

function codePointCompare(left, right) {
  if (left < right) return -1;
  if (left > right) return 1;
  return 0;
}

function sorted(values) {
  return [...values].sort(codePointCompare);
}

function inventorySha256(inventory) {
  const source = inventory.map((entry) => `${entry}\n`).join('');
  return createHash('sha256').update(source, 'utf8').digest('hex');
}

function isAllowedRetiredRename(command) {
  if (!/^\s*git\s+mv\b/i.test(command)) return false;
  const argumentsAfterMove = command.replace(/^\s*git\s+mv\s+(?:--\s+)?/i, '');
  if (
    !/^(?:['"])?\S*(?:check-rc-july-adoption\.mjs|rc-july-adoption\.test\.mjs)(?:['"])?\s+/i.test(
      argumentsAfterMove
    )
  ) {
    return false;
  }
  return !RETIRED_SOURCE_REFERENCE.test(command.replace(RETIRED_RENAME_SOURCE, ''));
}

function isNonExecutingInformationalCommand(command) {
  return INFORMATIONAL_SHELL_COMMAND.test(command) && !/\$\(|`/.test(command);
}

/** Return executable shell commands that attempt to resolve the retired source. */
export function auditRetiredPlanCommands(planSource) {
  const violations = [];
  for (const [fenceIndex, match] of [...planSource.matchAll(EXECUTABLE_SHELL_FENCE)].entries()) {
    const executable = match[1]
      .split('\n')
      .filter((line) => !/^\s*#/.test(line))
      .join('\n')
      .replace(/\\\r?\n/g, ' ');
    for (const command of executable.split(/\r?\n|&&|\|\||;/)) {
      const trimmed = command.trim();
      if (
        trimmed.length === 0 ||
        isNonExecutingInformationalCommand(trimmed) ||
        !RETIRED_SOURCE_REFERENCE.test(trimmed) ||
        !RETIRED_OPERATION.test(trimmed) ||
        isAllowedRetiredRename(trimmed)
      ) {
        continue;
      }
      violations.push({ fence: fenceIndex + 1, command: trimmed });
    }
  }
  return violations;
}

/** Return the number of executable shell fences examined by the plan-integrity scanner. */
export function countExecutableShellFences(planSource) {
  return [...planSource.matchAll(EXECUTABLE_SHELL_FENCE)].length;
}

function extractManifest(specSource) {
  const matches = [...specSource.matchAll(MANIFEST_FENCE)];
  if (matches.length !== 1 || typeof matches[0]?.[1] !== 'string') {
    throw new Error(
      `expected exactly one retired-rcjuly-tsjs-concept-manifest-v1 block, found ${matches.length}`
    );
  }

  const manifest = JSON.parse(matches[0][1]);
  if (manifest === null || typeof manifest !== 'object' || manifest.version !== 1) {
    throw new Error('retired concept manifest must be a version 1 object');
  }
  if (manifest.authority !== 'concept-audit-only') {
    throw new Error('retired concept manifest authority must be concept-audit-only');
  }
  if (!SHA_40.test(manifest.retiredSnapshot ?? '')) {
    throw new Error(
      'retired concept manifest retiredSnapshot must be a lowercase 40-character SHA'
    );
  }
  if (manifest.retiredSnapshot !== RECORDED_RETIRED_SNAPSHOT) {
    throw new Error(`retired concept manifest must name recorded retired snapshot`);
  }
  if (!Number.isInteger(manifest.inventoryCount) || !Array.isArray(manifest.inventory)) {
    throw new Error('retired concept manifest inventoryCount/inventory has an invalid shape');
  }
  if (!SHA_256.test(manifest.inventorySha256 ?? '')) {
    throw new Error('retired concept manifest inventorySha256 must be a lowercase SHA-256');
  }
  if (!Array.isArray(manifest.mappings)) {
    throw new Error('retired concept manifest mappings must be an array');
  }

  const inventory = manifest.inventory;
  if (inventory.some((entry) => typeof entry !== 'string' || entry.length === 0)) {
    throw new Error('retired concept inventory entries must be non-empty strings');
  }
  if (manifest.inventoryCount !== inventory.length) {
    throw new Error(
      `inventoryCount ${manifest.inventoryCount} does not match ${inventory.length} inventory paths`
    );
  }
  if (inventory.length !== 144) {
    throw new Error(
      `retired concept inventory must contain exactly 144 paths, found ${inventory.length}`
    );
  }
  const uniqueInventory = new Set(inventory);
  if (uniqueInventory.size !== inventory.length) {
    throw new Error('retired concept inventory paths must be unique');
  }
  if (inventory.some((entry, index) => entry !== sorted(inventory)[index])) {
    throw new Error('retired concept inventory paths must be code-point sorted');
  }
  const computedInventorySha256 = inventorySha256(inventory);
  if (computedInventorySha256 !== manifest.inventorySha256) {
    throw new Error(
      `retired concept inventory SHA-256 mismatch: expected ${manifest.inventorySha256}, computed ${computedInventorySha256}`
    );
  }
  if (manifest.inventorySha256 !== RECORDED_INVENTORY_SHA256) {
    throw new Error('retired concept manifest must preserve the recorded inventory SHA-256');
  }

  for (const [index, mapping] of manifest.mappings.entries()) {
    if (mapping === null || typeof mapping !== 'object') {
      throw new Error(`mapping ${index} must be an object`);
    }
    if (!Array.isArray(mapping.ids) || mapping.ids.length === 0) {
      throw new Error(`mapping ${index} must declare at least one id`);
    }
    if (mapping.ids.some((id) => typeof id !== 'string' || !/^RCJ-[A-Z]+-[0-9]+$/.test(id))) {
      throw new Error(`mapping ${index} contains an invalid id`);
    }
    if (
      mapping.exact !== undefined &&
      (!Array.isArray(mapping.exact) ||
        mapping.exact.length === 0 ||
        mapping.exact.some((entry) => typeof entry !== 'string' || entry.length === 0))
    ) {
      throw new Error(`mapping ${index} has invalid exact paths`);
    }
    if (
      mapping.prefix !== undefined &&
      (typeof mapping.prefix !== 'string' || mapping.prefix.length === 0)
    ) {
      throw new Error(`mapping ${index} has an invalid prefix`);
    }
    if (
      mapping.prefixes !== undefined &&
      (!Array.isArray(mapping.prefixes) ||
        mapping.prefixes.length === 0 ||
        mapping.prefixes.some((entry) => typeof entry !== 'string' || entry.length === 0))
    ) {
      throw new Error(`mapping ${index} has invalid prefixes`);
    }
    if (
      mapping.exact === undefined &&
      mapping.prefix === undefined &&
      mapping.prefixes === undefined
    ) {
      throw new Error(`mapping ${index} must declare exact, prefix, or prefixes`);
    }
  }

  return manifest;
}

function mappingMatches(file, mapping) {
  return (
    (Array.isArray(mapping.exact) && mapping.exact.includes(file)) ||
    (typeof mapping.prefix === 'string' && file.startsWith(mapping.prefix)) ||
    (Array.isArray(mapping.prefixes) && mapping.prefixes.some((prefix) => file.startsWith(prefix)))
  );
}

function mappingIdsForFile(file, mappings) {
  const ids = new Set();
  for (const mapping of mappings) {
    if (!mappingMatches(file, mapping)) continue;
    for (const id of mapping.ids) ids.add(id);
  }
  return ids;
}

function auditClassifications({ auditFixturePath, ledgerIds, mainAuditSha }) {
  if (mainAuditSha !== RECORDED_MAIN_AUDIT_SHA) {
    throw new Error(`MAIN_AUDIT_SHA must equal the recorded current-main SHA`);
  }
  if (!SHA_40.test(mainAuditSha ?? '')) {
    throw new Error('MAIN_AUDIT_SHA must be the recorded lowercase 40-character current-main SHA');
  }
  const classifications = JSON.parse(fs.readFileSync(auditFixturePath, 'utf8'));
  if (!Array.isArray(classifications)) {
    throw new Error('current-main concept audit fixture must be an array');
  }

  const failures = [];
  if (classifications.length !== 23) {
    failures.push(`expected exactly 23 classification rows, found ${classifications.length}`);
  }
  const ids = classifications.map((row) => row?.id).filter((id) => typeof id === 'string');
  const seen = new Set();
  const duplicateClassificationIds = sorted([
    ...new Set(ids.filter((id) => (seen.has(id) ? true : !seen.add(id)))),
  ]);
  const fixtureIds = new Set(ids);
  const missingClassificationIds = sorted([...ledgerIds].filter((id) => !fixtureIds.has(id)));
  const extraClassificationIds = sorted([...fixtureIds].filter((id) => !ledgerIds.has(id)));
  if (duplicateClassificationIds.length > 0) {
    failures.push(`duplicateClassificationIds: ${JSON.stringify(duplicateClassificationIds)}`);
  }
  if (missingClassificationIds.length > 0) {
    failures.push(`missingClassificationIds: ${JSON.stringify(missingClassificationIds)}`);
  }
  if (extraClassificationIds.length > 0) {
    failures.push(`extraClassificationIds: ${JSON.stringify(extraClassificationIds)}`);
  }

  for (const [index, row] of classifications.entries()) {
    if (row === null || typeof row !== 'object' || Array.isArray(row)) {
      failures.push(`classification row ${index} must be an object`);
      continue;
    }
    const keys = Object.keys(row).sort(codePointCompare);
    if (
      keys.length !== CLASSIFICATION_KEYS.length ||
      keys.some((key, keyIndex) => key !== CLASSIFICATION_KEYS[keyIndex])
    ) {
      failures.push(`classification row ${index} must contain the exact required fields`);
    }
    const label = typeof row.id === 'string' ? row.id : `row ${index}`;
    if (typeof row.id !== 'string' || !/^RCJ-[A-Z]+-[0-9]+$/.test(row.id)) {
      failures.push(`classification row ${index} must contain a valid RCJ id`);
    }
    if (row.mainSha !== mainAuditSha) {
      failures.push(`${label} mainSha must equal ${mainAuditSha}`);
    }
    if (!CLASSIFICATIONS.has(row.classification)) {
      failures.push(
        `${label} classification must be main-owned or implementation-gap, not ${JSON.stringify(row.classification)}`
      );
    }
    if (!Array.isArray(row.ownerPaths) || row.ownerPaths.length === 0) {
      failures.push(`${label} ownerPaths must be a non-empty array`);
    } else if (
      row.ownerPaths.some(
        (ownerPath) =>
          typeof ownerPath !== 'string' ||
          ownerPath.length === 0 ||
          path.isAbsolute(ownerPath) ||
          RETIRED_SOURCE_REFERENCE.test(ownerPath)
      )
    ) {
      failures.push(
        `${label} ownerPaths must name exact current-main paths without historical source`
      );
    }
    if (
      typeof row.testPath !== 'string' ||
      row.testPath.length === 0 ||
      path.isAbsolute(row.testPath) ||
      RETIRED_SOURCE_REFERENCE.test(row.testPath)
    ) {
      failures.push(`${label} testPath must name an exact focused current-main test path`);
    }
    if (
      typeof row.command !== 'string' ||
      row.command.trim().length === 0 ||
      RETIRED_SOURCE_REFERENCE.test(row.command)
    ) {
      failures.push(`${label} command must be reproducible and must not resolve historical source`);
    }
    if (!RESULTS.has(row.result)) {
      failures.push(`${label} result must be pass or fail, not ${JSON.stringify(row.result)}`);
    }
    if (!DISPOSITIONS.has(row.disposition)) {
      failures.push(`${label} disposition is invalid`);
    }
    if (row.classification === 'main-owned' && row.result !== 'pass') {
      failures.push(`${label} main-owned classification must record result pass`);
    }
    if (row.classification === 'implementation-gap' && row.result !== 'fail') {
      failures.push(`${label} implementation-gap classification must record result fail`);
    }
    if (
      row.classification === 'proof-pending' ||
      row.classification === 'coverage-gap' ||
      row.result === 'proof-pending' ||
      row.result === 'coverage-gap'
    ) {
      failures.push(`${label} proof-pending and coverage-gap are not final classifications`);
    }
  }

  return { classifications, classificationFailures: failures };
}

export function auditRetiredConceptAudit({
  specPath,
  auditFixturePath,
  historicalPerformanceFixturePath,
  mainAuditSha = RECORDED_MAIN_AUDIT_SHA,
  planPath,
}) {
  const specSource = fs.readFileSync(specPath, 'utf8');
  const manifest = extractManifest(specSource);
  const orderedFiles = [...manifest.inventory];
  const unmappedFiles = orderedFiles.filter(
    (file) => !manifest.mappings.some((mapping) => mappingMatches(file, mapping))
  );
  const qualityOnlySourceFiles = orderedFiles.filter((file) => {
    if (!file.startsWith(SOURCE_ROOT)) return false;
    const ids = mappingIdsForFile(file, manifest.mappings);
    return ![...ids].some((id) => id !== QUALITY_ID);
  });
  const deadMappings = manifest.mappings
    .map((mapping, index) => ({ index, mapping }))
    .filter(({ mapping }) => !orderedFiles.some((file) => mappingMatches(file, mapping)))
    .map(({ index }) => index);

  const manifestIds = new Set(manifest.mappings.flatMap((mapping) => mapping.ids));
  const ledgerIds = new Set([...specSource.matchAll(LEDGER_ID)].map((match) => match[1]));
  const manifestOnlyIds = sorted([...manifestIds].filter((id) => !ledgerIds.has(id)));
  const ledgerOnlyIds = sorted([...ledgerIds].filter((id) => !manifestIds.has(id)));

  const result = {
    authority: manifest.authority,
    retiredSnapshot: manifest.retiredSnapshot,
    inventorySha256: manifest.inventorySha256,
    fileCount: orderedFiles.length,
    mappingCount: manifest.mappings.length,
    manifestIdCount: manifestIds.size,
    ledgerIdCount: ledgerIds.size,
    ledgerIds: sorted(ledgerIds),
    unmappedFiles,
    qualityOnlySourceFiles,
    deadMappings,
    manifestOnlyIds,
    ledgerOnlyIds,
  };
  if (auditFixturePath !== undefined) {
    Object.assign(result, auditClassifications({ auditFixturePath, ledgerIds, mainAuditSha }));
  }
  if (planPath !== undefined) {
    result.retiredPlanCommandViolations = auditRetiredPlanCommands(
      fs.readFileSync(planPath, 'utf8')
    );
  }
  if (historicalPerformanceFixturePath !== undefined) {
    result.historicalPerformanceEvidence = {
      authority: 'report-only',
      sha256: createHash('sha256')
        .update(fs.readFileSync(historicalPerformanceFixturePath))
        .digest('hex'),
    };
  }
  return result;
}

export function assertRetiredConceptAudit(result) {
  const failures = [];
  if (result.fileCount !== 144) failures.push(`expected 144 files, found ${result.fileCount}`);
  if (result.mappingCount !== 38) {
    failures.push(`expected 38 mappings, found ${result.mappingCount}`);
  }
  if (result.manifestIdCount !== 23 || result.ledgerIdCount !== 23) {
    failures.push(
      `expected 23 manifest/ledger ids, found ${result.manifestIdCount}/${result.ledgerIdCount}`
    );
  }
  for (const key of [
    'unmappedFiles',
    'qualityOnlySourceFiles',
    'deadMappings',
    'manifestOnlyIds',
    'ledgerOnlyIds',
  ]) {
    if (result[key].length > 0) failures.push(`${key}: ${JSON.stringify(result[key])}`);
  }
  failures.push(...(result.classificationFailures ?? []));
  if ((result.retiredPlanCommandViolations ?? []).length > 0) {
    failures.push(
      `retiredPlanCommandViolations: ${JSON.stringify(result.retiredPlanCommandViolations)}`
    );
  }
  if (
    result.historicalPerformanceEvidence !== undefined &&
    (result.historicalPerformanceEvidence.authority !== 'report-only' ||
      result.historicalPerformanceEvidence.sha256 !== HISTORICAL_PERFORMANCE_SHA256)
  ) {
    failures.push(
      `historicalPerformanceEvidence must remain report-only at ${HISTORICAL_PERFORMANCE_SHA256}`
    );
  }
  if (failures.length > 0) throw new Error(failures.join('\n'));
}

const scriptPath = fileURLToPath(import.meta.url);
if (process.argv[1] && path.resolve(process.argv[1]) === scriptPath) {
  const repositoryRoot = path.resolve(path.dirname(scriptPath), '../../../..');
  const specPath = path.join(
    repositoryRoot,
    'docs/superpowers/specs/2026-08-04-aps-render-fix-and-tsjs-resilience-design.md'
  );
  const auditFixturePath = path.join(
    repositoryRoot,
    'crates/trusted-server-js/lib/test/fixtures/contracts/current-main-concept-audit.json'
  );
  const planPath = path.join(
    repositoryRoot,
    'docs/superpowers/plans/2026-08-04-aps-tsjs-resilience-implementation.md'
  );
  const historicalPerformanceFixturePath = path.join(
    repositoryRoot,
    'crates/trusted-server-js/lib/test/fixtures/performance/aps-tsjs-prechange.json'
  );
  const configuredMainAuditSha = process.env.MAIN_AUDIT_SHA ?? RECORDED_MAIN_AUDIT_SHA;
  const result = auditRetiredConceptAudit({
    specPath,
    auditFixturePath,
    historicalPerformanceFixturePath,
    mainAuditSha: configuredMainAuditSha,
    planPath,
  });
  assertRetiredConceptAudit(result);
  process.stdout.write(`${JSON.stringify(result)}\n`);
}
