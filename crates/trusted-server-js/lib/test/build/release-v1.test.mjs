import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import {
  RELEASE_SENTINEL,
  computeReleaseId,
  stampRelease,
  validateStampedRelease,
} from '../../scripts/release-v1.mjs';

const testDirectory = path.dirname(fileURLToPath(import.meta.url));
const libDirectory = path.resolve(testDirectory, '../..');
const repositoryRoot = path.resolve(libDirectory, '../../..');
const bundle = (id, logical) => ({ id, bytes: Buffer.from(`${logical}${RELEASE_SENTINEL}`) });

test('bundle metrics use the required five-module reference vector', () => {
  const metrics = JSON.parse(
    fs.readFileSync(path.resolve(libDirectory, '../dist/tsjs-build-metrics-v1.json'), 'utf8')
  );

  assert.deepEqual(metrics.sets.reference.files, [
    'tsjs-core.js',
    'tsjs-creative.js',
    'tsjs-gpt.js',
    'tsjs-prebid.js',
    'tsjs-datadome.js',
  ]);
});

test('bundle budgets are exposed through the package and enforced after the CI build', () => {
  const packageJson = JSON.parse(fs.readFileSync(path.join(libDirectory, 'package.json'), 'utf8'));
  const workflow = fs.readFileSync(path.join(repositoryRoot, '.github/workflows/test.yml'), 'utf8');
  const buildStep = workflow.indexOf('run: npm run build');
  const budgetStep = workflow.indexOf('run: npm run check:bundle');

  assert.equal(packageJson.scripts['check:bundle'], 'node scripts/check-bundle-budgets.mjs');
  assert.notEqual(buildStep, -1);
  assert.ok(budgetStep > buildStep, 'bundle budget check must run after the TSJS build');
});

test('release id changes with logical bytes and bundle order', () => {
  const base = [bundle('core', 'a'), bundle('gpt', 'b')];
  assert.notEqual(computeReleaseId(base), computeReleaseId([bundle('core', 'changed'), base[1]]));
  assert.notEqual(computeReleaseId(base), computeReleaseId([base[1], base[0]]));
});

test('sentinel multiplicity and remnants fail closed', () => {
  assert.throws(() => computeReleaseId([bundle('core', RELEASE_SENTINEL)]), /exactly one/);
  assert.throws(
    () => computeReleaseId([{ id: 'core', bytes: Buffer.from('none') }]),
    /exactly one/
  );
  assert.throws(() => stampRelease(`${RELEASE_SENTINEL}${RELEASE_SENTINEL}`, 'a'.repeat(64)));
});

test('wrong release and missing bundle fail validation', () => {
  const release = computeReleaseId([bundle('core', 'a')]);
  const stamped = stampRelease(bundle('core', 'a').bytes, release);
  assert.doesNotThrow(() =>
    validateStampedRelease([{ id: 'core', bytes: stamped }], release, ['core'])
  );
  assert.throws(() =>
    validateStampedRelease([{ id: 'core', bytes: stamped }], 'b'.repeat(64), ['core'])
  );
  assert.throws(() =>
    validateStampedRelease([{ id: 'core', bytes: stamped }], release, ['core', 'gpt'])
  );
});
