#!/usr/bin/env node

import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const libDir = path.resolve(scriptDir, '..');
const defaultBaselinePath = path.join(
  libDir,
  'test',
  'fixtures',
  'performance',
  'aps-tsjs-prechange.json'
);
const metricsPath = path.resolve(libDir, '..', 'dist', 'tsjs-build-metrics-v1.json');
const SET_NAMES = ['minimal', 'reference', 'maximal'];
const SIZE_NAMES = ['rawBytes', 'gzipBytes', 'brotliBytes'];
const MAX_GROWTH = 1.05;

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

function validateSets(sets, label) {
  if (!sets || typeof sets !== 'object' || Array.isArray(sets)) {
    fail(`${label} must be an object`);
  }
  for (const setName of SET_NAMES) {
    const set = sets[setName];
    if (!set || typeof set !== 'object' || Array.isArray(set)) {
      fail(`${label}.${setName} must be an object`);
    }
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
    for (const sizeName of SIZE_NAMES) {
      assertPositiveInteger(set[sizeName], `${label}.${setName}.${sizeName}`);
    }
    if (typeof set.sha256 !== 'string' || !/^[0-9a-f]{64}$/.test(set.sha256)) {
      fail(`${label}.${setName}.sha256 must be 64 lowercase hexadecimal characters`);
    }
  }
}

function parseArgs(argv) {
  const options = { baselineOnly: false, baselinePath: defaultBaselinePath };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === '--baseline-only') {
      options.baselineOnly = true;
    } else if (argument === '--baseline') {
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

export function checkBundleBudgets({
  baselineOnly = false,
  baselinePath = defaultBaselinePath,
} = {}) {
  const baseline = readJson(baselinePath, 'baseline');
  const metrics = readJson(metricsPath, 'build metrics');
  if (baseline.schemaVersion !== 1) fail('baseline.schemaVersion must equal 1');
  if (metrics.schemaVersion !== 1) fail('build metrics schemaVersion must equal 1');
  validateSets(baseline.bundles, 'baseline.bundles');
  validateSets(metrics.sets, 'buildMetrics.sets');

  const failures = [];
  for (const setName of SET_NAMES) {
    const expected = baseline.bundles[setName];
    const actual = metrics.sets[setName];
    if (JSON.stringify(actual.files) !== JSON.stringify(expected.files)) {
      failures.push(
        `${setName}.files changed: expected ${JSON.stringify(expected.files)}, got ${JSON.stringify(actual.files)}`
      );
    }
    for (const sizeName of SIZE_NAMES) {
      const limit = baselineOnly ? expected[sizeName] : Math.floor(expected[sizeName] * MAX_GROWTH);
      if (actual[sizeName] > limit) {
        failures.push(
          `${setName}.${sizeName} is ${actual[sizeName]} bytes; limit is ${limit} from baseline ${expected[sizeName]}`
        );
      }
    }
    if (baselineOnly && actual.sha256 !== expected.sha256) {
      failures.push(`${setName}.sha256 differs from the pre-change build`);
    }
  }

  if (failures.length > 0) fail(`budget check failed:\n- ${failures.join('\n- ')}`);

  return {
    baselineOnly,
    baselinePath,
    maxGrowthPercent: baselineOnly ? 0 : 5,
    sets: metrics.sets,
  };
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const result = checkBundleBudgets(parseArgs(process.argv.slice(2)));
    process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
  } catch (error) {
    process.stderr.write(`${error instanceof Error ? error.message : error}\n`);
    process.exitCode = 1;
  }
}
