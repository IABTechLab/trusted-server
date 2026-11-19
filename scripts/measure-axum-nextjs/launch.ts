import { spawn } from 'node:child_process';
import type { ChildProcess } from 'node:child_process';
import { openSync, closeSync, readFileSync } from 'node:fs';
import { createConnection, createServer } from 'node:net';
import { dirname, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';

const root = dirname(fileURLToPath(import.meta.url));
const abort = new AbortController();
const owned: { child: ChildProcess; finished: Promise<number | null> }[] = [];
process.once('SIGTERM', () => abort.abort(new Error('SIGTERM')));
process.once('SIGINT', () => abort.abort(new Error('SIGINT')));

function groupSignal(child: ChildProcess, signal: NodeJS.Signals) {
  if (!child.pid) return;
  try {
    process.kill(-child.pid, signal);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'ESRCH') throw error;
  }
}

async function interruptible<T>(promise: Promise<T>, signal: AbortSignal): Promise<T> {
  signal.throwIfAborted();
  let listener: () => void;
  const interrupted = new Promise<never>((_resolve, reject) => {
    listener = () => reject(signal.reason);
    signal.addEventListener('abort', listener, { once: true });
  });
  try {
    return await Promise.race([promise, interrupted]);
  } finally {
    signal.removeEventListener('abort', listener!);
  }
}

function launch(
  command: string,
  args: string[],
  cwd: string,
  extra: NodeJS.ProcessEnv = {},
  name?: string
) {
  const descriptor = name ? openSync(resolve(root, `${name}.log`), 'w') : undefined;
  let child: ChildProcess;
  try {
    child = spawn(command, args, {
      cwd,
      env: { ...process.env, ...extra },
      detached: true,
      stdio: descriptor === undefined ? 'inherit' : ['ignore', descriptor, descriptor],
    });
  } finally {
    if (descriptor !== undefined) closeSync(descriptor);
  }
  const finished = new Promise<number | null>((resolve, reject) => {
    child.once('error', reject);
    child.once('close', resolve);
  });
  // Readiness may still be pending when a child fails to spawn.
  void finished.catch(() => {});
  const processRecord = { child, finished };
  owned.push(processRecord);
  return processRecord;
}

function freePort(port: number): Promise<void> {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.once('error', reject);
    server.listen(port, '127.0.0.1', () =>
      server.close((error) => (error ? reject(error) : resolve()))
    );
  });
}

async function listening(port: number) {
  const signal = AbortSignal.any([abort.signal, AbortSignal.timeout(20000)]);
  while (true) {
    signal.throwIfAborted();
    const connected = await new Promise<boolean>((resolve) => {
      const socket = createConnection({ host: '127.0.0.1', port });
      socket.once('connect', () => {
        socket.destroy();
        resolve(true);
      });
      socket.once('error', () => {
        socket.destroy();
        resolve(false);
      });
      socket.setTimeout(250, () => {
        socket.destroy();
        resolve(false);
      });
    });
    if (connected) return;
    await delay(100, undefined, { signal });
  }
}

async function start(
  command: string,
  args: string[],
  cwd: string,
  name: string,
  port: number,
  extra: NodeJS.ProcessEnv = {}
) {
  const service = launch(command, args, cwd, extra, name);
  await Promise.race([
    listening(port),
    service.finished.then(() => {
      throw new Error(`${name} exited before readiness; see its log`);
    }),
  ]);
}

try {
  for (const port of [18980, 18981, 18982]) await freePort(port);
  await start(
    process.execPath,
    ['node_modules/next/dist/bin/next', 'start', '-H', '127.0.0.1', '-p', '18980'],
    resolve(root, 'origin-app'),
    'origin',
    18980,
    { NEXT_TELEMETRY_DISABLED: '1' }
  );
  await start(process.execPath, [resolve(root, 'relay.ts')], root, 'relay', 18981);
  const envelope = JSON.parse(
    readFileSync(process.env.MEASURE_ENVELOPE!, 'utf8')
  ).trusted_server_config;
  await start(process.env.MEASURE_BIN!, [], process.env.MEASURE_REPO!, 'axum', 18982, {
    EDGEZERO__ADAPTER__HOST: '127.0.0.1',
    EDGEZERO__ADAPTER__PORT: '18982',
    EDGEZERO__LOGGING__LEVEL: 'warn',
    TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG: envelope,
    TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_HANDLER_PASSWORD:
      'fixture-admin-password-32-bytes-ok',
    TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_PUBLISHER_PROXY_SECRET:
      'fixture-proxy-secret-32-bytes-test',
    TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_EC_PASSPHRASE:
      'fixture-ec-passphrase-32-bytes-test',
  });
  const probe = launch(process.execPath, [resolve(root, 'exercise.ts')], root);
  const status = await interruptible(
    probe.finished,
    AbortSignal.any([abort.signal, AbortSignal.timeout(300000)])
  );
  if (status !== 0) throw new Error('measurement assertions failed; see results.json');
} catch (error) {
  console.error(error);
  process.exitCode = abort.signal.aborted ? 143 : 1;
} finally {
  abort.abort();
  for (const { child, finished } of owned.reverse()) {
    groupSignal(child, 'SIGTERM');
    await Promise.race([finished.catch(() => {}), delay(10000, undefined, { ref: false })]);
    // A leader exiting does not prove that all browser descendants exited.
    groupSignal(child, 'SIGKILL');
    await finished.catch(() => {});
  }
}
