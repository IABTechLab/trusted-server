import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const libDir = path.dirname(fileURLToPath(import.meta.url));
const cratesDir = path.resolve(libDir, '..', '..');

/**
 * The names a module's script uses for the shared browser code, so a script
 * kept in its own crate's `js/` folder imports it without a path into this
 * crate.
 */
export const sharedCodeAliases = {
  '@tsjs/core': path.join(libDir, 'src', 'core'),
  '@tsjs/shared': path.join(libDir, 'src', 'shared'),
};

/** Whether `full` is a folder, false for anything missing, a broken link included. */
function isFolder(full) {
  return fs.statSync(full, { throwIfNoEntry: false })?.isDirectory() === true;
}

/** The folders directly inside `dir`, in name order, or none when it is not a folder. */
function foldersIn(dir) {
  if (!isFolder(dir)) {
    return [];
  }
  return fs
    .readdirSync(dir)
    .sort()
    .map((name) => path.join(dir, name))
    .filter(isFolder);
}

/**
 * The folders that hold modules' sources: this crate's `src/integrations`, then
 * `<type>/<crate>/js/src` for each crate under `cratesDir` that has one.
 */
export function moduleSourceRoots({ lib = libDir, crates = cratesDir } = {}) {
  const crateScriptRoots = foldersIn(crates)
    .flatMap((typeDir) => foldersIn(typeDir))
    .map((crateDir) => path.join(crateDir, 'js', 'src'))
    .filter(isFolder);
  return [path.join(lib, 'src', 'integrations'), ...crateScriptRoots];
}

/**
 * Every module's source folder by module name: `src/integrations/<name>` for
 * this crate's own modules, and `crates/<type>/<crate>/js/src/<name>` for a
 * module whose script lives in the crate that supplies it. A name found twice
 * is an error, because bundles and imports are keyed by name, and so is the
 * name `core`, which is the shared core's bundle.
 */
export function moduleSourceDirs({ lib = libDir, crates = cratesDir } = {}) {
  const dirs = new Map();
  for (const root of moduleSourceRoots({ lib, crates })) {
    for (const dir of foldersIn(root)) {
      const name = path.basename(dir);
      if (name === 'core') {
        throw new Error(`module name core, used by ${dir}, is the shared core's bundle`);
      }
      const earlier = dirs.get(name);
      if (earlier) {
        throw new Error(`module ${name} is in both ${earlier} and ${dir}`);
      }
      dirs.set(name, dir);
    }
  }
  return dirs;
}

/** The entry of the module whose sources are in `dir`, or none when it has no `index.ts`. */
export function moduleEntry(dir) {
  const entryPath = path.join(dir, 'index.ts');
  return fs.statSync(entryPath, { throwIfNoEntry: false })?.isFile() === true ? entryPath : null;
}

/**
 * `@tsjs/module/<name>` for each module, so a script or a test that needs
 * another module's code names the module rather than the folder it lives in.
 */
export function moduleAliases() {
  return Object.fromEntries(
    [...moduleSourceDirs()].map(([name, dir]) => [`@tsjs/module/${name}`, dir])
  );
}

/** Production Vite options for one independently bundled tsjs IIFE. */
export function moduleBuildOptions({ name, entryPath, outDir }) {
  return {
    configFile: false,
    root: libDir,
    resolve: { alias: { ...sharedCodeAliases, ...moduleAliases() } },
    build: {
      emptyOutDir: false,
      outDir,
      assetsDir: '.',
      sourcemap: false,
      minify: 'esbuild',
      rollupOptions: {
        input: entryPath,
        output: {
          format: 'iife',
          dir: outDir,
          entryFileNames: `tsjs-${name}.js`,
          inlineDynamicImports: true,
          extend: false,
          // Use a unique IIFE name per module to avoid conflicts.
          name: name === 'core' ? 'tsjs' : `tsjs_${name}`,
        },
      },
    },
    logLevel: 'warn',
  };
}
