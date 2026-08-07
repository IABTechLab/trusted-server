import assert from 'node:assert/strict';
import test from 'node:test';

import {
  RELEASE_SENTINEL,
  computeReleaseId,
  stampRelease,
  validateStampedRelease,
} from '../../scripts/release-v1.mjs';

const bundle = (id, logical) => ({ id, bytes: Buffer.from(`${logical}${RELEASE_SENTINEL}`) });

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
