#!/usr/bin/env node

import { spawn, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { constants as fileConstants } from 'node:fs';
import { access, lstat, mkdir, open, readFile, rename, unlink, writeFile } from 'node:fs/promises';
import path from 'node:path';
import process from 'node:process';

const MODEL = 'openai-codex/gpt-5.6-luna';
const THINKING_LEVEL = 'xhigh';
const PROMPT_VERSION = 2;
const COMPLETE_MARKER = '<!-- pi-rust-review:complete -->';
const MAX_STDERR_BYTES = 1024 * 1024;

const SYSTEM_PROMPT = `You are a Rust refactoring analyst, not a bug, security, performance, or correctness reviewer.

You receive exactly one numbered Rust source file. Review only the supplied file and do not make assumptions about unseen code.

Your goal is to identify concrete, behavior-preserving refactorings that improve maintainability and readability or reduce meaningful duplication and boilerplate.

Do not report possible bugs, security concerns, configuration-policy concerns, performance optimizations, documentation gaps, style trivia, or changes to observable behavior.

Return only a focused Markdown report.`;

function reviewPrompt(sourcePath) {
  return `Review the supplied Rust file: ${sourcePath}

Identify only substantial, behavior-preserving refactoring opportunities.

Prioritize:

1. Repeated logic, branches, constructors, conversions, or test setup that can be consolidated.
2. Long functions or modules containing multiple responsibilities that can be separated cleanly.
3. Deeply nested or difficult-to-follow control flow that can be expressed more directly.
4. Repeated match arms or data transformations that can share a helper or clearer abstraction.
5. Boilerplate that can be removed with an existing type, trait, iterator, derive, or data-driven representation.
6. Names, types, or structure that create unnecessary mental overhead.
7. Unnecessary intermediate state or duplicated representations of the same information.

Constraints:

- Preserve observable behavior and existing public APIs.
- Do not report bugs or propose semantic changes.
- Do not suggest performance work unless it also clearly simplifies the code.
- Do not suggest an abstraction merely to reduce line count.
- A new helper or abstraction should remove genuine duplication, isolate a responsibility, or materially simplify control flow.
- Avoid speculative architecture changes requiring unseen files.
- Avoid rustfmt trivia, documentation-only changes, and minor naming preferences.
- Prefer a few high-impact findings over an exhaustive list.
- If the file is already reasonably maintainable, say so rather than manufacturing findings.

Use this structure:

# Refactoring review: ${sourcePath}

## Summary

Briefly describe the file's maintainability and the most valuable refactoring theme.

## Refactoring opportunities

For each opportunity:

### Descriptive title

- Location: symbol and numbered line range
- Current structure: what makes the code harder to maintain
- Proposed refactor: the concrete transformation
- Benefit: duplication, boilerplate, nesting, responsibility, or readability reduced
- Behavior preservation: why the change should not alter behavior
- Estimated scope: Small, Medium, or Large
- Confidence: High, Medium, or Low

Do not include a finding unless you can describe a concrete transformation and a clear maintainability benefit.`;
}

function usage() {
  return `Usage: node scripts/review-rust-files.mjs [options]

Review every tracked Rust file with an isolated Pi process and write Markdown reports.

Options:
  --workers N             Concurrent Pi processes (default: 4)
  --output DIR            Report directory (default: timestamped directory under rust-review-reports/)
  --timeout-minutes N     Per-attempt timeout (default: 45)
  --retries N             Retries after a failed attempt (default: 2)
  --include GLOB          Include matching repository paths; repeatable
  --exclude GLOB          Exclude matching repository paths; repeatable
  --limit N               Review at most N selected files
  --resume                Resume an existing --output directory
  --force                 Re-review files and replace an existing run manifest
  --dry-run               List selected files without starting Pi
  --pi-bin PATH            Pi executable to invoke (default: pi)
  -h, --help              Show this help

Examples:
  node scripts/review-rust-files.mjs --dry-run
  node scripts/review-rust-files.mjs --limit 4 --workers 2
  node scripts/review-rust-files.mjs --workers 6 --output rust-review-reports/full
  node scripts/review-rust-files.mjs --resume --output rust-review-reports/full
`;
}

function timestampId() {
  return new Date()
    .toISOString()
    .replaceAll(':', '-')
    .replace(/\.\d{3}Z$/, 'Z');
}

function parsePositiveInteger(value, optionName, { allowZero = false } = {}) {
  const parsed = Number(value);
  const minimum = allowZero ? 0 : 1;
  if (!Number.isInteger(parsed) || parsed < minimum) {
    throw new Error(`${optionName} must be an integer greater than or equal to ${minimum}`);
  }
  return parsed;
}

function takeOptionValue(argv, index, optionName) {
  const value = argv[index + 1];
  if (value === undefined || value.startsWith('--')) {
    throw new Error(`${optionName} requires a value`);
  }
  return value;
}

function parseArgs(argv) {
  const options = {
    workers: 4,
    output: undefined,
    timeoutMinutes: 45,
    retries: 2,
    includes: [],
    excludes: [],
    limit: undefined,
    resume: false,
    force: false,
    dryRun: false,
    piBin: 'pi',
    outputWasExplicit: false,
  };

  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    switch (argument) {
      case '--workers':
        options.workers = parsePositiveInteger(takeOptionValue(argv, index, argument), argument);
        index += 1;
        break;
      case '--output':
        options.output = takeOptionValue(argv, index, argument);
        options.outputWasExplicit = true;
        index += 1;
        break;
      case '--timeout-minutes':
        options.timeoutMinutes = parsePositiveInteger(
          takeOptionValue(argv, index, argument),
          argument
        );
        index += 1;
        break;
      case '--retries':
        options.retries = parsePositiveInteger(takeOptionValue(argv, index, argument), argument, {
          allowZero: true,
        });
        index += 1;
        break;
      case '--include':
        options.includes.push(takeOptionValue(argv, index, argument));
        index += 1;
        break;
      case '--exclude':
        options.excludes.push(takeOptionValue(argv, index, argument));
        index += 1;
        break;
      case '--limit':
        options.limit = parsePositiveInteger(takeOptionValue(argv, index, argument), argument);
        index += 1;
        break;
      case '--resume':
        options.resume = true;
        break;
      case '--force':
        options.force = true;
        break;
      case '--dry-run':
        options.dryRun = true;
        break;
      case '--pi-bin':
        options.piBin = takeOptionValue(argv, index, argument);
        index += 1;
        break;
      case '-h':
      case '--help':
        options.help = true;
        break;
      default:
        throw new Error(`unknown option: ${argument}`);
    }
  }

  if (options.resume && options.force) {
    throw new Error('--resume and --force cannot be used together');
  }
  if (options.resume && !options.outputWasExplicit) {
    throw new Error('--resume requires an explicit --output directory');
  }

  return options;
}

function globToRegExp(glob) {
  let expression = '^';
  for (let index = 0; index < glob.length; index += 1) {
    const character = glob[index];
    if (character === '*') {
      if (glob[index + 1] === '*') {
        index += 1;
        if (glob[index + 1] === '/') {
          index += 1;
          expression += '(?:.*/)?';
        } else {
          expression += '.*';
        }
      } else {
        expression += '[^/]*';
      }
    } else if (character === '?') {
      expression += '[^/]';
    } else {
      expression += character.replace(/[\\^$.*+?()[\]{}|]/g, '\\$&');
    }
  }
  return new RegExp(`${expression}$`);
}

function matchesAny(filePath, globs) {
  return globs.some((glob) => globToRegExp(glob).test(filePath));
}

function repositoryRoot(cwd) {
  const result = spawnSync('git', ['rev-parse', '--show-toplevel'], {
    cwd,
    encoding: 'utf8',
  });
  if (result.error) {
    throw new Error(`failed to run git: ${result.error.message}`);
  }
  if (result.status !== 0) {
    throw new Error(`failed to find repository root: ${result.stderr.trim()}`);
  }
  return result.stdout.trim();
}

function discoverRustFiles(cwd) {
  const result = spawnSync('git', ['ls-files', '-z', '--', '*.rs'], {
    cwd,
    encoding: 'utf8',
    maxBuffer: 16 * 1024 * 1024,
  });
  if (result.error) {
    throw new Error(`failed to run git: ${result.error.message}`);
  }
  if (result.status !== 0) {
    throw new Error(`git ls-files failed: ${result.stderr.trim()}`);
  }
  return result.stdout.split('\0').filter(Boolean);
}

async function pathExists(filePath) {
  try {
    await access(filePath);
    return true;
  } catch {
    return false;
  }
}

function validateRepositoryPath(filePath, repositoryDirectory) {
  if (typeof filePath !== 'string' || filePath.length === 0 || path.isAbsolute(filePath)) {
    throw new Error(`invalid repository-relative path in file selection: ${String(filePath)}`);
  }
  const resolvedPath = path.resolve(repositoryDirectory, filePath);
  const relativePath = path.relative(repositoryDirectory, resolvedPath);
  if (
    relativePath === '..' ||
    relativePath.startsWith(`..${path.sep}`) ||
    path.isAbsolute(relativePath)
  ) {
    throw new Error(`file selection escapes the repository: ${filePath}`);
  }
  return resolvedPath;
}

async function readRegularFile(filePath) {
  let file;
  try {
    file = await open(filePath, fileConstants.O_RDONLY | fileConstants.O_NOFOLLOW);
    const fileStat = await file.stat();
    if (!fileStat.isFile()) {
      throw new Error('path is not a regular file');
    }
    return await file.readFile('utf8');
  } catch (error) {
    throw new Error(`cannot safely read ${filePath}: ${error.message}`);
  } finally {
    if (file) {
      await file.close().catch(() => {});
    }
  }
}

async function reportIsComplete(filePath, sourceHash) {
  try {
    const contents = await readFile(filePath, 'utf8');
    return (
      contents.includes(`source_sha256: ${sourceHash}\n`) &&
      contents.trimEnd().endsWith(COMPLETE_MARKER)
    );
  } catch {
    return false;
  }
}

async function acquireRunLock(outputDirectory) {
  await mkdir(outputDirectory, { recursive: true });
  const lockPath = path.join(outputDirectory, '.run.lock');
  let lock;
  try {
    lock = await open(lockPath, 'wx');
  } catch (error) {
    if (error.code === 'EEXIST') {
      throw new Error(
        `another review process may be using ${outputDirectory}; remove ${lockPath} if it is stale`
      );
    }
    throw error;
  }
  await lock.writeFile(
    `${JSON.stringify({ pid: process.pid, startedAt: new Date().toISOString() })}\n`
  );
  return async () => {
    await lock.close().catch(() => {});
    await unlink(lockPath).catch(() => {});
  };
}

async function atomicWrite(filePath, contents) {
  await mkdir(path.dirname(filePath), { recursive: true });
  const temporaryPath = `${filePath}.tmp-${process.pid}-${Date.now()}-${Math.random().toString(16).slice(2)}`;
  await writeFile(temporaryPath, contents, 'utf8');
  try {
    await rename(temporaryPath, filePath);
  } catch (error) {
    await unlink(temporaryPath).catch(() => {});
    throw error;
  }
}

function sha256(contents) {
  return createHash('sha256').update(contents).digest('hex');
}

function numberSource(sourcePath, source, sourceHash) {
  const numbered = source
    .split('\n')
    .map((line, index) => `${String(index + 1).padStart(6, '0')} | ${line}`)
    .join('\n');
  return `BEGIN_UNTRUSTED_RUST_SOURCE\npath: ${sourcePath}\nsha256: ${sourceHash}\n${numbered}\nEND_UNTRUSTED_RUST_SOURCE\n`;
}

function extractAssistantText(message) {
  if (!message || message.role !== 'assistant') {
    return undefined;
  }
  if (typeof message.content === 'string') {
    return message.content;
  }
  if (!Array.isArray(message.content)) {
    return undefined;
  }
  const text = message.content
    .filter((part) => part?.type === 'text' && typeof part.text === 'string')
    .map((part) => part.text)
    .join('');
  return text || undefined;
}

function assistantError(message) {
  if (!message || message.role !== 'assistant') {
    return undefined;
  }
  if (message.stopReason !== 'stop') {
    return message.errorMessage || `Pi stopped with reason ${message.stopReason || 'unknown'}`;
  }
  return message.errorMessage;
}

function runPi({ piBin, cwd, sourcePath, sourceInput, timeoutMs, activeChildren }) {
  return new Promise((resolve, reject) => {
    const args = [
      '--mode',
      'json',
      '--no-session',
      '--no-tools',
      '--no-context-files',
      '--no-extensions',
      '--no-skills',
      '--no-prompt-templates',
      '--no-approve',
      '--model',
      MODEL,
      '--thinking',
      THINKING_LEVEL,
      '--system-prompt',
      SYSTEM_PROMPT,
      reviewPrompt(sourcePath),
    ];
    const child = spawn(piBin, args, {
      cwd,
      env: process.env,
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    activeChildren.add(child);

    let stderr = '';
    let stdoutBuffer = '';
    let responseText;
    let usage;
    let eventError;
    let parseError;
    let timedOut = false;
    let killTimer;

    const timeout = setTimeout(() => {
      timedOut = true;
      child.kill('SIGTERM');
      killTimer = setTimeout(() => child.kill('SIGKILL'), 5_000);
      killTimer.unref();
    }, timeoutMs);
    timeout.unref();

    function consumeLine(line) {
      if (!line.trim()) {
        return;
      }
      let event;
      try {
        event = JSON.parse(line);
      } catch (error) {
        parseError ||= `invalid JSON from Pi: ${error.message}`;
        return;
      }

      if (event.type === 'message_end') {
        responseText = extractAssistantText(event.message) || responseText;
        usage = event.message?.usage || usage;
        eventError = assistantError(event.message) || eventError;
      } else if (event.type === 'agent_end' && Array.isArray(event.messages)) {
        for (const message of event.messages) {
          responseText = extractAssistantText(message) || responseText;
          usage = message?.usage || usage;
          eventError = assistantError(message) || eventError;
        }
      }
    }

    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk) => {
      stdoutBuffer += chunk;
      let newlineIndex = stdoutBuffer.indexOf('\n');
      while (newlineIndex !== -1) {
        consumeLine(stdoutBuffer.slice(0, newlineIndex));
        stdoutBuffer = stdoutBuffer.slice(newlineIndex + 1);
        newlineIndex = stdoutBuffer.indexOf('\n');
      }
    });
    child.stderr.on('data', (chunk) => {
      if (stderr.length < MAX_STDERR_BYTES) {
        stderr += chunk.slice(0, MAX_STDERR_BYTES - stderr.length);
      }
    });
    child.on('error', (error) => {
      clearTimeout(timeout);
      if (killTimer) {
        clearTimeout(killTimer);
      }
      activeChildren.delete(child);
      reject(new Error(`failed to start Pi: ${error.message}`));
    });
    child.on('close', (code, signal) => {
      clearTimeout(timeout);
      if (killTimer) {
        clearTimeout(killTimer);
      }
      activeChildren.delete(child);
      if (stdoutBuffer) {
        consumeLine(stdoutBuffer);
      }

      if (timedOut) {
        reject(new Error(`Pi timed out after ${Math.round(timeoutMs / 60_000)} minutes`));
      } else if (code !== 0) {
        reject(
          new Error(
            `Pi exited with ${signal ? `signal ${signal}` : `code ${code}`}${stderr.trim() ? `: ${stderr.trim()}` : ''}`
          )
        );
      } else if (eventError) {
        reject(new Error(eventError));
      } else if (!responseText?.trim()) {
        reject(new Error(parseError || 'Pi completed without a Markdown response'));
      } else {
        resolve({ responseText: responseText.trim(), usage, stderr: stderr.trim() });
      }
    });

    child.stdin.on('error', (error) => {
      if (error.code !== 'EPIPE') {
        stderr += `\nstdin error: ${error.message}`;
      }
    });
    child.stdin.end(sourceInput);
  });
}

function sleep(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

function yamlString(value) {
  return JSON.stringify(value);
}

function buildReport({ sourcePath, sourceHash, responseText, reviewedAt, durationMs, attempts }) {
  return `---
source: ${yamlString(sourcePath)}
source_sha256: ${sourceHash}
model: ${MODEL}
thinking: ${THINKING_LEVEL}
prompt_version: ${PROMPT_VERSION}
reviewed_at: ${reviewedAt}
duration_ms: ${durationMs}
attempts: ${attempts}
---

${responseText}

${COMPLETE_MARKER}
`;
}

function reportRelativePath(sourcePath) {
  return path.join('files', `${sourcePath}.md`);
}

function logRelativePath(sourcePath) {
  return path.join('logs', `${sourcePath}.log`);
}

function formatDuration(milliseconds) {
  const seconds = Math.round(milliseconds / 1000);
  if (seconds < 60) {
    return `${seconds}s`;
  }
  const minutes = Math.floor(seconds / 60);
  return `${minutes}m ${seconds % 60}s`;
}

function escapeMarkdown(value) {
  return value.replaceAll('|', '\\|').replaceAll('[', '\\[').replaceAll(']', '\\]');
}

function buildIndex(manifest) {
  const entries = Object.entries(manifest.files).sort(([left], [right]) =>
    left.localeCompare(right)
  );
  const counts = { completed: 0, failed: 0, pending: 0, running: 0, skipped: 0 };
  for (const [, entry] of entries) {
    counts[entry.status] = (counts[entry.status] || 0) + 1;
  }

  const lines = [
    '# Rust file review',
    '',
    `- Model: \`${manifest.model}\``,
    `- Thinking: \`${manifest.thinking}\``,
    `- Workers: ${manifest.config.workers}`,
    `- Created: ${manifest.createdAt}`,
    `- Updated: ${manifest.updatedAt}`,
    `- Completed: ${counts.completed}`,
    `- Failed: ${counts.failed}`,
    `- Pending or interrupted: ${counts.pending + counts.running}`,
    '',
    '| Source | Status | Duration | Attempts |',
    '| --- | --- | ---: | ---: |',
  ];

  for (const [sourcePath, entry] of entries) {
    const source = escapeMarkdown(sourcePath);
    const sourceCell =
      entry.status === 'completed'
        ? `[${source}](<${entry.report.replaceAll(path.sep, '/')}>)`
        : source;
    lines.push(
      `| ${sourceCell} | ${entry.status} | ${entry.durationMs === undefined ? '' : formatDuration(entry.durationMs)} | ${entry.attempts || 0} |`
    );
  }
  lines.push('');
  return lines.join('\n');
}

async function loadManifest(manifestPath) {
  const contents = await readFile(manifestPath, 'utf8');
  const manifest = JSON.parse(contents);
  if (manifest.version !== 1) {
    throw new Error(`unsupported manifest version: ${manifest.version}`);
  }
  return manifest;
}

async function main() {
  let options;
  try {
    options = parseArgs(process.argv.slice(2));
  } catch (error) {
    console.error(`Error: ${error.message}\n`);
    console.error(usage());
    process.exitCode = 2;
    return;
  }

  if (options.help) {
    console.log(usage());
    return;
  }

  const invocationDirectory = process.cwd();
  const cwd = repositoryRoot(invocationDirectory);
  const output = options.output || path.join('rust-review-reports', timestampId());
  const outputDirectory = path.resolve(cwd, output);
  const manifestPath = path.join(outputDirectory, 'manifest.json');
  const indexPath = path.join(outputDirectory, 'index.md');
  const releaseLock = options.dryRun ? async () => {} : await acquireRunLock(outputDirectory);

  try {
    const manifestExists = await pathExists(manifestPath);
    if (options.resume && !manifestExists) {
      throw new Error(`cannot resume because no manifest exists at ${manifestPath}`);
    }
    if (manifestExists && !options.resume && !options.force) {
      throw new Error(`a review run already exists at ${outputDirectory}; use --resume or --force`);
    }
    if (
      options.resume &&
      (options.includes.length > 0 || options.excludes.length > 0 || options.limit !== undefined)
    ) {
      throw new Error(
        '--resume reuses the original file selection and cannot be combined with --include, --exclude, or --limit'
      );
    }

    const existingManifest = options.resume ? await loadManifest(manifestPath) : undefined;
    if (existingManifest) {
      if (existingManifest.model !== MODEL || existingManifest.thinking !== THINKING_LEVEL) {
        throw new Error('the existing manifest uses a different model or thinking level');
      }
      if (existingManifest.promptVersion !== PROMPT_VERSION) {
        throw new Error('the existing manifest uses a different review prompt version');
      }
      if (!Array.isArray(existingManifest.selectedFiles)) {
        throw new Error('the existing manifest does not contain its original file selection');
      }
    }

    const trackedRustFiles = discoverRustFiles(cwd);
    const trackedRustFileSet = new Set(trackedRustFiles);
    if (existingManifest) {
      for (const filePath of existingManifest.selectedFiles) {
        validateRepositoryPath(filePath, cwd);
        if (!trackedRustFileSet.has(filePath)) {
          throw new Error(
            `resume manifest contains a path that is not a tracked Rust file: ${filePath}`
          );
        }
      }
    }

    let files = existingManifest?.selectedFiles || trackedRustFiles;
    if (!existingManifest && options.includes.length > 0) {
      files = files.filter((filePath) => matchesAny(filePath, options.includes));
    }
    if (!existingManifest && options.excludes.length > 0) {
      files = files.filter((filePath) => !matchesAny(filePath, options.excludes));
    }

    const fileMetadata = await Promise.all(
      files.map(async (filePath) => {
        const absolutePath = validateRepositoryPath(filePath, cwd);
        const fileStat = await lstat(absolutePath);
        if (!fileStat.isFile()) {
          throw new Error(`tracked Rust path is not a regular file: ${filePath}`);
        }
        return { path: filePath, size: fileStat.size };
      })
    );
    fileMetadata.sort(
      (left, right) => right.size - left.size || left.path.localeCompare(right.path)
    );
    if (!existingManifest && options.limit !== undefined) {
      fileMetadata.splice(options.limit);
    }

    if (fileMetadata.length === 0) {
      console.error('No tracked Rust files matched the selection.');
      process.exitCode = 1;
      return;
    }

    console.log(`Selected ${fileMetadata.length} tracked Rust file(s), largest first.`);
    if (options.dryRun) {
      for (const file of fileMetadata) {
        console.log(`${String(file.size).padStart(10)}  ${file.path}`);
      }
      console.log(`\nDry run: would use ${options.workers} worker(s).`);
      return;
    }

    const now = new Date().toISOString();
    const manifest = existingManifest || {
      version: 1,
      createdAt: now,
      updatedAt: now,
      model: MODEL,
      thinking: THINKING_LEVEL,
      promptVersion: PROMPT_VERSION,
      selectedFiles: fileMetadata.map((file) => file.path),
      config: {
        workers: options.workers,
        timeoutMinutes: options.timeoutMinutes,
        retries: options.retries,
        includes: options.includes,
        excludes: options.excludes,
        limit: options.limit,
      },
      files: {},
    };

    if (options.resume) {
      manifest.config.workers = options.workers;
      manifest.config.timeoutMinutes = options.timeoutMinutes;
      manifest.config.retries = options.retries;
    }

    const jobs = [];
    for (const file of fileMetadata) {
      const absolutePath = validateRepositoryPath(file.path, cwd);
      const source = await readRegularFile(absolutePath);
      const sourceHash = sha256(source);
      const report = reportRelativePath(file.path);
      const existing = manifest.files[file.path];
      const completedAndCurrent =
        options.resume &&
        existing?.status === 'completed' &&
        existing.sourceSha256 === sourceHash &&
        (await reportIsComplete(path.join(outputDirectory, report), sourceHash));

      if (completedAndCurrent) {
        existing.status = 'completed';
        existing.skippedOnResume = true;
        continue;
      }

      manifest.files[file.path] = {
        sourceSha256: sourceHash,
        status: 'pending',
        report,
        attempts: 0,
      };
      jobs.push({
        sourcePath: file.path,
        source,
        sourceHash,
        report,
        log: logRelativePath(file.path),
      });
    }

    let persistQueue = Promise.resolve();
    function persist() {
      manifest.updatedAt = new Date().toISOString();
      const manifestSnapshot = `${JSON.stringify(manifest, null, 2)}\n`;
      const indexSnapshot = `${buildIndex(manifest)}\n`;
      persistQueue = persistQueue.then(async () => {
        await atomicWrite(manifestPath, manifestSnapshot);
        await atomicWrite(indexPath, indexSnapshot);
      });
      return persistQueue;
    }

    await persist();

    if (jobs.length === 0) {
      console.log(`All ${fileMetadata.length} selected files are already complete and unchanged.`);
      console.log(`Reports: ${path.relative(cwd, outputDirectory)}`);
      return;
    }

    console.log(
      `Starting ${Math.min(options.workers, jobs.length)} worker(s); ${jobs.length} file(s) need review.`
    );
    console.log(`Reports: ${path.relative(cwd, outputDirectory)}`);

    const activeChildren = new Set();
    let interrupted = false;
    let nextJobIndex = 0;
    let completedJobs = 0;
    let failedJobs = 0;

    function stopChildren(signal) {
      interrupted = true;
      console.error(
        `\nReceived ${signal}; stopping ${activeChildren.size} active Pi process(es)...`
      );
      for (const child of activeChildren) {
        child.kill('SIGTERM');
      }
    }
    process.once('SIGINT', () => stopChildren('SIGINT'));
    process.once('SIGTERM', () => stopChildren('SIGTERM'));

    async function reviewJob(job, workerNumber) {
      const entry = manifest.files[job.sourcePath];
      const startedAt = Date.now();
      const attemptLogs = [];
      let lastError;

      for (let attempt = 1; attempt <= options.retries + 1; attempt += 1) {
        if (interrupted) {
          lastError = new Error('review interrupted');
          break;
        }

        entry.status = 'running';
        entry.attempts = attempt;
        entry.startedAt ||= new Date().toISOString();
        await persist();
        console.log(
          `[worker ${workerNumber}] START ${job.sourcePath} (attempt ${attempt}/${options.retries + 1})`
        );

        try {
          const result = await runPi({
            piBin: options.piBin,
            cwd,
            sourcePath: job.sourcePath,
            sourceInput: numberSource(job.sourcePath, job.source, job.sourceHash),
            timeoutMs: options.timeoutMinutes * 60_000,
            activeChildren,
          });
          const reviewedAt = new Date().toISOString();
          const durationMs = Date.now() - startedAt;
          const reportContents = buildReport({
            sourcePath: job.sourcePath,
            sourceHash: job.sourceHash,
            responseText: result.responseText,
            reviewedAt,
            durationMs,
            attempts: attempt,
          });
          await atomicWrite(path.join(outputDirectory, job.report), reportContents);
          if (result.stderr) {
            attemptLogs.push(`Attempt ${attempt} stderr:\n${result.stderr}`);
          }
          if (attemptLogs.length > 0) {
            await atomicWrite(path.join(outputDirectory, job.log), `${attemptLogs.join('\n\n')}\n`);
            entry.log = job.log;
          }

          entry.status = 'completed';
          entry.reviewedAt = reviewedAt;
          entry.durationMs = durationMs;
          entry.usage = result.usage;
          delete entry.error;
          await persist();
          completedJobs += 1;
          console.log(
            `[worker ${workerNumber}] DONE  ${job.sourcePath} (${formatDuration(durationMs)})`
          );
          return;
        } catch (error) {
          lastError = error;
          attemptLogs.push(`Attempt ${attempt} failed:\n${error.message}`);
          console.error(`[worker ${workerNumber}] FAIL  ${job.sourcePath}: ${error.message}`);
          if (interrupted) {
            break;
          }
          if (attempt <= options.retries) {
            const delayMs = 2 ** (attempt - 1) * 2_000;
            console.error(`[worker ${workerNumber}] RETRY ${job.sourcePath} in ${delayMs / 1000}s`);
            await sleep(delayMs);
          }
        }
      }

      const durationMs = Date.now() - startedAt;
      entry.status = interrupted ? 'pending' : 'failed';
      entry.durationMs = durationMs;
      entry.error = lastError?.message || 'unknown error';
      await atomicWrite(path.join(outputDirectory, job.log), `${attemptLogs.join('\n\n')}\n`);
      entry.log = job.log;
      await persist();
      if (!interrupted) {
        failedJobs += 1;
      }
    }

    async function worker(workerNumber) {
      while (!interrupted) {
        const jobIndex = nextJobIndex;
        nextJobIndex += 1;
        if (jobIndex >= jobs.length) {
          return;
        }
        await reviewJob(jobs[jobIndex], workerNumber);
      }
    }

    const workerCount = Math.min(options.workers, jobs.length);
    await Promise.all(Array.from({ length: workerCount }, (_, index) => worker(index + 1)));
    await persistQueue;

    console.log(`\nCompleted this run: ${completedJobs}`);
    console.log(`Failed this run: ${failedJobs}`);
    if (interrupted) {
      console.log('The run was interrupted. Use --resume with the same --output directory.');
      process.exitCode = 130;
    } else if (failedJobs > 0) {
      console.log('Some reviews failed. Inspect logs and use --resume to retry them.');
      process.exitCode = 1;
    } else {
      console.log(`Review complete: ${path.relative(cwd, indexPath)}`);
    }
  } finally {
    await releaseLock();
  }
}

main().catch((error) => {
  console.error(`Error: ${error.message}`);
  process.exitCode = 1;
});
