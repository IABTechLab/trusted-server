import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const project = 'crates/trusted-server-js/lib';
const wrapper = /\bnode\s+[^\n]*browser-build-runner\.mjs["']?\s+--\s+/;
const tool = /(?:^|\s)(?:\S*\/)?(?:npm|npx|vite|vitest|tsc|eslint|prettier)["']?(?:\s|$)|\bnode\s+[^\n]*(?:(?:build-all|build-prebid-external|build-trace-assets|browser-tools)\.mjs|node_modules\/(?:vite|vitest|typescript|eslint|prettier)\/)/;
const isProject = (value) => /(?:^|\/)trusted-server-js\/lib(?:[\s/"']|$)/.test(value);

/** Check shell commands while tracking their production-browser cwd. */
export function checkCommands(source, initialCwd = '.', file = '<commands>') {
  let cwd = initialCwd;
  const variables = new Map();
  const findings = [];
  const lines = source.replace(/\\\r?\n\s*/g, ' ').split('\n');
  for (let index = 0; index < lines.length; index++) {
    const line = lines[index].trim();
    if (!line || line.startsWith('#')) continue;
    const assignment = line.match(/^([A-Z_]+)=["']?([^"']+)/);
    if (assignment) variables.set(assignment[1], assignment[2]);
    for (const part of line.split(/&&|\|\||;|\|/)) {
      let command = part.trim().replace(/^\(\s*/, '');
      for (const [name, value] of variables) command = command.replaceAll(`$${name}`, value).replaceAll(`\${${name}}`, value);
      if (/^cd\s/.test(command)) {
        cwd = command.slice(3).replace(/["']/g, '').trim();
        continue;
      }
      const production = isProject(cwd) || isProject(command);
      if (production && tool.test(command) && !wrapper.test(command)) {
        findings.push(`${file}:${index + 1}: unwrapped production-browser command: ${part.trim()}`);
      }
    }
  }
  return findings;
}

/** Inspect workflow run steps without assigning docs/Playwright the browser lease. */
export function checkWorkflow(source, file = '<workflow>') {
  let defaultCwd = '.';
  let steps = false;
  const findings = [];
  const lines = source.split('\n');
  for (let index = 0; index < lines.length; index++) {
    const line = lines[index];
    if (/^  [\w-]+:/.test(line)) { defaultCwd = '.'; steps = false; }
    if (/^    steps:/.test(line)) steps = true;
    const directory = line.match(/^\s+working-directory:\s*(.+)/);
    if (directory && !steps) defaultCwd = directory[1];
    if (!steps || !/^      - /.test(line)) continue;
    const step = [line];
    while (index + 1 < lines.length && !/^      - |^  [\w-]+:/.test(lines[index + 1])) step.push(lines[++index]);
    const cwdLine = step.find((item) => /^(?:        |      - )working-directory:/.test(item));
    const stepCwd = cwdLine ? cwdLine.replace(/^(?:        |      - )working-directory:\s*/, '') : defaultCwd;
    const runIndex = step.findIndex((item) => /^(?:        |      - )run:/.test(item));
    if (runIndex < 0) continue;
    const run = step[runIndex].replace(/^(?:        |      - )run:\s*/, '');
    let command = run;
    if (/^[|>][-+]?$/.test(run)) {
      const block = [];
      for (let next = runIndex + 1; next < step.length && (/^          /.test(step[next]) || !step[next].trim()); next++) block.push(step[next].trimStart());
      command = block.join(run.startsWith('>') ? ' ' : '\n');
    }
    findings.push(...checkCommands(command, stepCwd, file));
  }
  return findings;
}

function discover(directory, extension) {
  return fs.readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const name = path.join(directory, entry.name);
    return entry.isDirectory() ? discover(name, extension) : name.endsWith(extension) ? [name] : [];
  });
}

/** Return violations in the repository's canonical callers and package scripts. */
export function checkRepository(root) {
  const findings = [];
  for (const file of discover(path.join(root, '.github/workflows'), '.yml')) {
    findings.push(...checkWorkflow(fs.readFileSync(file, 'utf8'), path.relative(root, file)));
  }
  for (const file of discover(path.join(root, 'scripts'), '.sh')) {
    findings.push(...checkCommands(fs.readFileSync(file, 'utf8'), '.', path.relative(root, file)));
  }
  const library = path.join(root, project);
  const scripts = JSON.parse(fs.readFileSync(path.join(library, 'package.json'), 'utf8')).scripts;
  for (const [name, command] of Object.entries(scripts)) {
    findings.push(...checkCommands(command, project, `package.json scripts.${name}`));
  }
  const requiredGuards = new Set(['build-all.mjs', 'build-prebid-external.mjs', 'build-trace.mjs', 'build-trace-assets.mjs', 'browser-tools.mjs', 'check-two-root-typecheck.mjs']);
  for (const file of fs.readdirSync(library).filter((name) => name.endsWith('.mjs'))) {
    const source = fs.readFileSync(path.join(library, file), 'utf8');
    const importsTool = /(?:from\s*|import\()['"](?:vite|vitest|typescript|eslint|prettier)(?:\/|['"])/.test(source);
    if ((requiredGuards.has(file) || importsTool) && !/await assertBrowserBuildLease\(/.test(source)) {
      findings.push(`${project}/${file}: missing live lease assertion`);
    }
  }
  const build = fs.readFileSync(path.join(root, 'crates/trusted-server-js/build.rs'), 'utf8');
  if (!build.includes('browser-build-runner.mjs') || !build.includes('assert-browser-build-lease.mjs') || build.includes('.tsjs-npm-ci.lock')) {
    findings.push('crates/trusted-server-js/build.rs: expected complete self-reexec lease, no npm-ci-only lock');
  }
  const cli = fs.readFileSync(path.join(root, 'crates/trusted-server-cli/src/prebid_bundle.rs'), 'utf8');
  if (!cli.includes('browser-build-runner.mjs')) findings.push('CLI Prebid generator is missing its runner hook');
  return findings;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
  const findings = checkRepository(root);
  if (findings.length) {
    console.error(findings.join('\n'));
    process.exitCode = 1;
  } else console.log('Production-browser invocation boundaries passed.');
}
