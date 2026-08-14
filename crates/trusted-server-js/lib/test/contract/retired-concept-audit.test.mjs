import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  assertRetiredConceptAudit,
  auditRetiredPlanCommands,
  auditRetiredConceptAudit,
  countExecutableShellFences,
} from '../../scripts/check-retired-concept-audit.mjs';

const { test } = process.env.VITEST ? await import('vitest') : await import('node:test');

const testDirectory = path.dirname(fileURLToPath(import.meta.url));
const repositoryRoot = path.resolve(testDirectory, '../../../../..');
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
const mainAuditSha = 'f6a2fb85ce623bf8a574e3941e1ee349acc3412d';
const auditedClassifications = JSON.parse(readFileSync(auditFixturePath, 'utf8'));
const validClassifications = auditedClassifications.map((row) => ({
  ...row,
  classification: 'main-owned',
  ownerPaths: ['crates/trusted-server-js/lib/src/core/index.ts'],
  testPath: 'crates/trusted-server-js/lib/test/core/index.test.ts',
  command: 'npm --prefix crates/trusted-server-js/lib test -- --run test/core/index.test.ts',
  result: 'pass',
}));
const pendingClassifications = validClassifications.map((row, index) =>
  index === 0 ? { ...row, classification: 'proof-pending', result: 'proof-pending' } : row
);
const manifestFence = /```json retired-rcjuly-tsjs-concept-manifest-v1\n([\s\S]*?)\n```/;
const specSource = readFileSync(specPath, 'utf8');
const manifestSource = manifestFence.exec(specSource)?.[1];
assert.equal(typeof manifestSource, 'string');
const validManifest = JSON.parse(manifestSource);
const ledgerIds = [
  ...new Set([...specSource.matchAll(/\| `(RCJ-[A-Z]+-[0-9]+)`/g)].map((match) => match[1])),
];

function inventoryDigest(inventory) {
  return createHash('sha256')
    .update(inventory.map((entry) => `${entry}\n`).join(''))
    .digest('hex');
}

function sourceFor(manifest, ids = ledgerIds) {
  const ledger = ids.map((id) => `| \`${id}\` | retained | preserve |`).join('\n');
  return `${ledger}\n\n\`\`\`json retired-rcjuly-tsjs-concept-manifest-v1\n${JSON.stringify(manifest, null, 2)}\n\`\`\`\n`;
}

function auditSource(source) {
  const directory = mkdtempSync(path.join(tmpdir(), 'retired-concept-audit-'));
  const temporarySpecPath = path.join(directory, 'spec.md');
  writeFileSync(temporarySpecPath, source);
  try {
    return auditRetiredConceptAudit({ repositoryRoot, specPath: temporarySpecPath });
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

function assertSourceRejected(source, expected) {
  assert.throws(() => assertRetiredConceptAudit(auditSource(source)), expected);
}

function assertFixtureRejected(rows, expected) {
  const directory = mkdtempSync(path.join(tmpdir(), 'current-main-concept-audit-'));
  const temporaryFixturePath = path.join(directory, 'audit.json');
  writeFileSync(temporaryFixturePath, JSON.stringify(rows));
  try {
    assert.throws(
      () =>
        assertRetiredConceptAudit(
          auditRetiredConceptAudit({
            repositoryRoot,
            specPath,
            auditFixturePath: temporaryFixturePath,
            mainAuditSha,
          })
        ),
      expected
    );
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

test('the in-spec retired concept inventory is complete without a historical Git object', () => {
  const result = auditRetiredConceptAudit({ repositoryRoot, specPath });

  assert.equal(result.authority, 'concept-audit-only');
  assert.equal(result.retiredSnapshot, '905984e62a0858c53d9f0ff6dd3a1bf190cf311d');
  assert.equal(
    result.inventorySha256,
    'b1e28c8b30f0b8d95e38c0f8f57394df4ad43f760ae7abf5631e2054228aef08'
  );
  assert.equal(result.fileCount, 144);
  assert.equal(result.mappingCount, 38);
  assert.equal(result.manifestIdCount, 23);
  assert.equal(result.ledgerIdCount, 23);
  assert.deepEqual(result.unmappedFiles, []);
  assert.deepEqual(result.qualityOnlySourceFiles, []);
  assert.deepEqual(result.deadMappings, []);
  assert.deepEqual(result.manifestOnlyIds, []);
  assert.deepEqual(result.ledgerOnlyIds, []);
});

test('the retired concept manifest has one exact authoritative fence', () => {
  assertSourceRejected(`${sourceFor(validManifest)}\n${sourceFor(validManifest)}`, /exactly one/);
  assertSourceRejected(
    sourceFor({ ...validManifest, authority: 'behavior-authority' }),
    /concept-audit-only/
  );
  assertSourceRejected(
    sourceFor({ ...validManifest, retiredSnapshot: '0'.repeat(40) }),
    /recorded retired snapshot/
  );
});

test('the embedded inventory is exactly counted, sorted, unique, and digested', () => {
  assertSourceRejected(
    sourceFor({ ...validManifest, inventoryCount: validManifest.inventoryCount - 1 }),
    /inventoryCount/
  );
  assertSourceRejected(
    sourceFor({ ...validManifest, inventory: [...validManifest.inventory].reverse() }),
    /sorted/
  );

  const duplicateInventory = [...validManifest.inventory];
  duplicateInventory[1] = duplicateInventory[0];
  assertSourceRejected(
    sourceFor({
      ...validManifest,
      inventory: duplicateInventory,
      inventoryCount: duplicateInventory.length,
      inventorySha256: inventoryDigest(duplicateInventory),
    }),
    /unique/
  );
  assertSourceRejected(sourceFor({ ...validManifest, inventorySha256: '0'.repeat(64) }), /SHA-256/);

  const substitutedInventory = ['!retired-concept-audit-path', ...validManifest.inventory.slice(1)];
  assertSourceRejected(
    sourceFor({
      ...validManifest,
      inventory: substitutedInventory,
      inventorySha256: inventoryDigest(substitutedInventory),
    }),
    /recorded inventory SHA-256/
  );
});

test('every inventory path has a live behavioral mapping', () => {
  assertSourceRejected(sourceFor({ ...validManifest, mappings: [] }), /unmappedFiles/);

  const deadMappings = [
    ...validManifest.mappings,
    { exact: ['crates/trusted-server-js/lib/src/not-present.ts'], ids: ['RCJ-CORE-01'] },
  ];
  assertSourceRejected(sourceFor({ ...validManifest, mappings: deadMappings }), /deadMappings/);

  const qualityOnlyMappings = validManifest.mappings.map((mapping) =>
    mapping.prefix === 'crates/trusted-server-js/lib/src/core/'
      ? { ...mapping, ids: ['RCJ-QUAL-01'] }
      : mapping
  );
  assertSourceRejected(
    sourceFor({ ...validManifest, mappings: qualityOnlyMappings }),
    /qualityOnlySourceFiles/
  );
});

test('the manifest and retained-concept ledger expose the same 23 IDs', () => {
  assertSourceRejected(sourceFor(validManifest, ledgerIds.slice(1)), /manifestOnlyIds/);
});

test('classification rows reject missing, duplicate, pending, mismatched, or historical proof', () => {
  assertFixtureRejected(
    [...validClassifications, { ...validClassifications[0], id: null }],
    /exactly 23 classification rows/
  );
  assertFixtureRejected(
    validClassifications.map((row, index) => (index === 0 ? { ...row, id: null } : row)),
    /valid RCJ id/
  );
  assertFixtureRejected(validClassifications.slice(1), /missingClassificationIds/);
  assertFixtureRejected(
    [validClassifications[0], ...validClassifications],
    /duplicateClassificationIds/
  );
  assertFixtureRejected(pendingClassifications, /proof-pending/);
  assertFixtureRejected(
    validClassifications.map((row, index) =>
      index === 0 ? { ...row, mainSha: '0'.repeat(40) } : row
    ),
    /mainSha/
  );
  assertFixtureRejected(
    validClassifications.map((row, index) =>
      index === 0 ? { ...row, ownerPaths: [], testPath: '', command: '' } : row
    ),
    /ownerPaths/
  );
  assertFixtureRejected(
    validClassifications.map((row, index) => (index === 0 ? { ...row, result: 'fail' } : row)),
    /main-owned.*pass/
  );
  assertFixtureRejected(
    validClassifications.map((row, index) =>
      index === 0 ? { ...row, classification: 'implementation-gap' } : row
    ),
    /implementation-gap.*fail/
  );
  assertFixtureRejected(
    validClassifications.map((row, index) =>
      index === 0 ? { ...row, command: 'git show rc/july:historical.ts' } : row
    ),
    /historical source/
  );
  assert.throws(
    () =>
      auditRetiredConceptAudit({
        repositoryRoot,
        specPath,
        auditFixturePath,
        mainAuditSha: '0'.repeat(40),
      }),
    /recorded current-main SHA/
  );
});

test('every retained concept has a final current-main classification and reproducible proof', () => {
  const result = auditRetiredConceptAudit({
    specPath,
    auditFixturePath,
    mainAuditSha,
  });
  assertRetiredConceptAudit(result);

  assert.equal(result.classifications.length, 23);
  assert.deepEqual(result.classifications.map(({ id }) => id).sort(), [...result.ledgerIds].sort());
  assert.ok(
    result.classifications.every(({ classification }) =>
      ['main-owned', 'implementation-gap'].includes(classification)
    )
  );
});

test('the default current-main authority cannot be rewritten through the fixture', () => {
  const directory = mkdtempSync(path.join(tmpdir(), 'current-main-authority-'));
  const temporaryFixturePath = path.join(directory, 'audit.json');
  const rewrittenRows = auditedClassifications.map((row) => ({ ...row, mainSha: '0'.repeat(40) }));
  writeFileSync(temporaryFixturePath, JSON.stringify(rewrittenRows));
  try {
    assert.throws(
      () =>
        assertRetiredConceptAudit(
          auditRetiredConceptAudit({
            repositoryRoot,
            specPath,
            auditFixturePath: temporaryFixturePath,
          })
        ),
      new RegExp(`mainSha must equal ${mainAuditSha}`)
    );
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test('plan integrity scans executable shell fences while allowing prose and rename sources', () => {
  const safe = `
Never build rc/july or ${validManifest.retiredSnapshot}.

\`\`\`json
{"historical":"rc/july"}
\`\`\`

\`\`\`bash
echo 'rc/july is retired'
echo 'Do not build rc/july'
printf '%s\\n' 'Do not compare rc/july'
git mv scripts/check-rc-july-adoption.mjs scripts/check-retired-concept-audit.mjs
\`\`\`
`;
  assert.deepEqual(auditRetiredPlanCommands(safe), []);

  for (const command of [
    'git fetch origin RC/July',
    'git -C repository fetch origin rc/july',
    'git -c protocol.version=2 diff main...rc/july',
    'echo "$(git fetch origin rc/july)"',
    "printf '%s\\n' \"$(git -C repository fetch origin rc/july)\"",
    'echo "`git fetch origin rc/july`"',
    'git merge rc/july',
    'git rebase rc/july',
    'git cherry-pick 905984e62a0858c53d9f0ff6dd3a1bf190cf311d',
    'git checkout rc/july',
    'git worktree add /tmp/retired rc/july',
    'git archive rc/july',
    'git diff main...rc/july',
    'npm run build -- rc/july',
    'hyperfine "build main" "build rc/july"',
  ]) {
    const violations = auditRetiredPlanCommands(`\`\`\`sh\n${command}\n\`\`\``);
    assert.equal(violations.length, 1, command);
  }

  assert.equal(countExecutableShellFences('  ```bash\necho safe\n  ```'), 1);
  assert.equal(auditRetiredPlanCommands('  ```bash\ngit fetch origin rc/july\n  ```').length, 1);
});

test('the implementation plan contains no executable retired-source command', () => {
  const planSource = readFileSync(planPath, 'utf8');
  assert.equal(countExecutableShellFences(planSource), 103);
  assert.deepEqual(auditRetiredPlanCommands(planSource), []);

  const mutatedPlanSource = planSource.replace(
    '  ```bash\n',
    '  ```bash\ngit fetch origin rc/july\n'
  );
  assert.equal(auditRetiredPlanCommands(mutatedPlanSource).length, 1);
});

test('the historical performance fixture is immutable report-only evidence', () => {
  const result = auditRetiredConceptAudit({
    specPath,
    historicalPerformanceFixturePath,
  });
  assert.deepEqual(result.historicalPerformanceEvidence, {
    authority: 'report-only',
    sha256: 'fe5d7f52dc47dc9608ca6b92b036c6b971845e6424b157768dc9403a62d2d6b4',
  });

  const directory = mkdtempSync(path.join(tmpdir(), 'historical-performance-evidence-'));
  const modifiedFixturePath = path.join(directory, 'modified.json');
  writeFileSync(modifiedFixturePath, `${readFileSync(historicalPerformanceFixturePath, 'utf8')} `);
  try {
    assert.throws(
      () =>
        assertRetiredConceptAudit(
          auditRetiredConceptAudit({
            specPath,
            historicalPerformanceFixturePath: modifiedFixturePath,
          })
        ),
      /historicalPerformanceEvidence/
    );
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
