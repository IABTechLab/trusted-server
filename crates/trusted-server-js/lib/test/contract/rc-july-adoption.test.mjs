import assert from 'node:assert/strict';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { auditRcJulyAdoption } from '../../scripts/check-rc-july-adoption.mjs';

const { test } = process.env.VITEST ? await import('vitest') : await import('node:test');

const testDirectory = path.dirname(fileURLToPath(import.meta.url));
const repositoryRoot = path.resolve(testDirectory, '../../../../..');
const specPath = path.join(
  repositoryRoot,
  'docs/superpowers/specs/2026-08-04-aps-render-fix-and-tsjs-resilience-design.md'
);

test('the pinned rc/july TSJS baseline is completely mapped by the spec ledger', () => {
  const result = auditRcJulyAdoption({ repositoryRoot, specPath });

  assert.equal(result.baseline, '905984e62a0858c53d9f0ff6dd3a1bf190cf311d');
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
