// @vitest-environment node

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { afterEach, describe, expect, it } from 'vitest';

import { moduleEntry, moduleSourceDirs, moduleSourceRoots } from '../build-module-options.mjs';

const libDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const trees = [];

/** A scratch tree with a lib and a crates folder holding the given files. */
function tree(files) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'tsjs-module-discovery-'));
  trees.push(root);
  for (const file of files) {
    const full = path.join(root, file);
    fs.mkdirSync(path.dirname(full), { recursive: true });
    fs.writeFileSync(full, '');
  }
  return { root, lib: path.join(root, 'lib'), crates: path.join(root, 'crates') };
}

afterEach(() => {
  for (const root of trees.splice(0)) {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

describe('moduleSourceDirs', () => {
  it('finds modules here and in the crate that supplies them, and nothing else', () => {
    const { lib, crates } = tree([
      'lib/src/integrations/zeta/index.ts',
      'crates/kind/vendor/js/src/alpha/index.ts',
      'crates/kind/vendor/js/src/helpers/shared.ts',
      'crates/kind/carried/js/probe.js',
      'crates/kind/plain/src/lib.rs',
    ]);

    const dirs = moduleSourceDirs({ lib, crates });

    expect([...dirs.keys()]).toEqual(['zeta', 'alpha', 'helpers']);
    expect(moduleEntry(dirs.get('alpha'))).toBe(path.join(dirs.get('alpha'), 'index.ts'));
    expect(moduleEntry(dirs.get('helpers'))).toBeNull();
  });

  it('refuses a module name two folders supply', () => {
    const { lib, crates } = tree([
      'lib/src/integrations/alpha/index.ts',
      'crates/kind/vendor/js/src/alpha/index.ts',
    ]);

    expect(() => moduleSourceDirs({ lib, crates })).toThrow(/module alpha is in both/);
  });

  it('refuses a module named core, the shared core bundle', () => {
    const { lib, crates } = tree(['crates/kind/vendor/js/src/core/index.ts']);

    expect(() => moduleSourceDirs({ lib, crates })).toThrow(/module name core/);
  });

  it('passes over a link whose target is gone', () => {
    const { root, lib, crates } = tree(['crates/kind/vendor/js/src/alpha/index.ts']);
    const gone = path.join(root, 'gone');
    fs.mkdirSync(gone);
    fs.symlinkSync(gone, path.join(crates, 'kind', 'dangling'), 'junction');
    fs.rmSync(gone, { recursive: true });

    expect([...moduleSourceDirs({ lib, crates }).keys()]).toEqual(['alpha']);
  });

  it('takes a folder named index.ts for no entry', () => {
    const { crates } = tree(['crates/kind/vendor/js/src/alpha/index.ts/placeholder']);

    expect(moduleEntry(path.join(crates, 'kind', 'vendor', 'js', 'src', 'alpha'))).toBeNull();
  });
});

describe('the settings editors find for crate scripts', () => {
  it('format crate scripts with the same Prettier options as this crate', () => {
    const own = JSON.parse(fs.readFileSync(path.join(libDir, '.prettierrc.json'), 'utf8'));
    const crates = JSON.parse(
      fs.readFileSync(path.join(libDir, '..', '..', '.prettierrc.json'), 'utf8')
    );

    expect(crates.overrides).toEqual([
      {
        files: ['*/*/js/{src,test}/**', '.prettierrc.json', 'eslint.config.js', 'tsconfig.json'],
        options: own,
      },
    ]);
  });

  it('extend this crate TypeScript settings', () => {
    const crates = JSON.parse(
      fs.readFileSync(path.join(libDir, '..', '..', 'tsconfig.json'), 'utf8')
    );

    expect(crates.extends).toBe('./trusted-server-js/lib/tsconfig.json');
  });
});

describe('tsconfig.json', () => {
  it('lists every folder of module sources for @tsjs/module, in discovery order', () => {
    const tsconfig = JSON.parse(fs.readFileSync(path.join(libDir, 'tsconfig.json'), 'utf8'));
    const expected = moduleSourceRoots().map((root) => {
      const relative = path.relative(libDir, root).split(path.sep).join('/');
      return `${relative.startsWith('.') ? relative : `./${relative}`}/*`;
    });

    expect(tsconfig.compilerOptions.paths['@tsjs/module/*']).toEqual(expected);
  });
});
