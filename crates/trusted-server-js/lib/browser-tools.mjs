import { spawn } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { assertBrowserBuildLease } from '../../../scripts/browser-build-lease.mjs';

await assertBrowserBuildLease();
const library = path.dirname(fileURLToPath(import.meta.url));
const crates = path.resolve(library, '../..');
const [tool, ...extra] = process.argv.slice(2);
const roots = ['trusted-server-js/lib', 'trusted-server-integrations-js/lib'];
const sources = roots.map((root) => `${root}/{src,test}/**/*.{ts,tsx,js,mjs}`);
const config = (file) => path.join(library, file);
let entry;
let args;
let cwd = library;
switch (tool) {
  case 'typecheck':
    entry = 'typescript/bin/tsc';
    args = ['--project', config('tsconfig.production.json')];
    break;
  case 'test':
  case 'test:watch':
    entry = 'vitest/vitest.mjs';
    args = tool === 'test' ? ['run'] : [];
    break;
  case 'lint':
  case 'lint:fix':
    entry = 'eslint/bin/eslint.js';
    cwd = crates;
    args = [
      '--config',
      config('eslint.config.js'),
      '--no-error-on-unmatched-pattern',
      '--max-warnings=0',
      'trusted-server-js/lib/*.{ts,js,mjs}',
      ...sources,
    ];
    if (tool === 'lint:fix') args.push('--fix');
    break;
  case 'format':
  case 'format:write':
    entry = 'prettier/bin/prettier.cjs';
    cwd = crates;
    args = [
      '--config',
      config('.prettierrc.json'),
      '--ignore-path',
      config('.prettierignore'),
      '--no-error-on-unmatched-pattern',
      tool === 'format' ? '--check' : '--write',
      'trusted-server-js/lib/**/*.{ts,tsx,js,mjs,json,css,md}',
      'trusted-server-integrations-js/lib/{src,test}/**/*.{ts,tsx,js,mjs,json,css,md}',
      // Exact legacy bootstrap exception only. Shared rules remain unchanged.
      '!trusted-server-integrations-js/lib/src/integrations/gpt/gpt_bootstrap.js',
    ];
    break;
  case 'dev':
    entry = 'vite/bin/vite.js';
    args = ['build', '--watch'];
    break;
  default:
    throw new Error(`Unknown browser tool ${tool}`);
}
const child = spawn(process.execPath, [config(`node_modules/${entry}`), ...args, ...extra], {
  cwd,
  stdio: 'inherit',
  env: process.env,
});
child.on('error', (error) => {
  throw error;
});
process.exitCode = await new Promise((resolve) =>
  child.on('exit', (code, signal) => {
    if (signal) process.kill(process.pid, signal);
    resolve(code ?? 1);
  })
);
