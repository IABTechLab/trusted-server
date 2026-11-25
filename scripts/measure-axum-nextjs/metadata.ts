import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { availableParallelism, platform, release, arch } from 'node:os';
import { extname, relative, resolve } from 'node:path';

const [output, repository] = process.argv.slice(2);
if (!output || !repository) throw new Error('expected output directory and repository');
const execute = (command: string, args: string[]) =>
  execFileSync(command, args, { encoding: 'utf8' }).trim();
const hash = (path: string) => createHash('sha256').update(readFileSync(path)).digest('hex');
function inputs(directory: string): string[] {
  return readdirSync(directory).flatMap((name) => {
    const path = resolve(directory, name);
    return statSync(path).isDirectory() ? inputs(path) : [path];
  });
}
const edgezero = readFileSync(resolve(repository, 'Cargo.lock'), 'utf8')
  .split('[[package]]')
  .find((block) => /^name = "edgezero-adapter-axum"$/m.test(block));
const edgezeroSource = edgezero?.match(/^source = "([^"]+)"$/m)?.[1];
if (!edgezeroSource) throw new Error('missing resolved EdgeZero source');
const metadata = {
  recorder_tcp_nodelay: true,
  edgezero_source: edgezeroSource,
  fixture_inputs_sha256: Object.fromEntries(
    inputs(resolve(repository, 'scripts/measure-axum-nextjs'))
      .filter((path) => ['.ts', '.tsx', '.sh'].includes(extname(path)))
      .map((path) => [relative(repository, path), hash(path)])
  ),
  next_version: JSON.parse(
    readFileSync(resolve(output, 'origin-app/node_modules/next/package.json'), 'utf8')
  ).version,
  runtime_revision: execute('git', ['-C', repository, 'rev-parse', 'HEAD']),
  runtime_source_diff: execute('git', [
    '-C',
    repository,
    'diff',
    'HEAD',
    '--',
    'crates',
    'Cargo.toml',
    'Cargo.lock',
  ]),
  rust: execute('rustc', ['--version']),
  node: process.version,
  platform: `${platform()} ${release()} ${arch()}`,
  available_parallelism: availableParallelism(),
  build_environment: Object.fromEntries(
    ['RUSTFLAGS', 'CARGO_PROFILE_DEV_DEBUG', 'CARGO_INCREMENTAL'].map((name) => [
      name,
      process.env[name] ?? null,
    ])
  ),
  axum_binary_sha256: hash(process.env.MEASURE_BIN!),
  github_run_id: process.env.GITHUB_RUN_ID ?? null,
  limits:
    "Local HTTP/1; allowed proxy origin; sequential targets; timings are descriptive, not pooling's causal benefit",
};
writeFileSync(resolve(output, 'metadata.json'), JSON.stringify(metadata, null, 2) + '\n');
