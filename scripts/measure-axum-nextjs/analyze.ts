import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync, realpathSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export interface Receipt {
  connection: number;
  method: string;
  path: string;
  bytes: number;
  sha256: string;
  cookie: string | null;
  action: string | null;
  chunked: boolean;
}

/** Parse complete HTTP/1 requests, rejecting truncated evidence. */
export function parseStream(connection: number, data: Buffer): Receipt[] {
  const rows: Receipt[] = [];
  let position = 0;
  while (position < data.length) {
    const end = data.indexOf('\r\n\r\n', position);
    if (end < 0) throw new Error('incomplete request head');
    const lines = data.subarray(position, end).toString('latin1').split('\r\n');
    const [method, path, version] = lines.shift()!.split(' ');
    if (!method || !path || version !== 'HTTP/1.1') throw new Error('invalid request line');
    const headers = new Map<string, string>();
    for (const line of lines) {
      const colon = line.indexOf(':');
      if (colon < 1) throw new Error('invalid request header');
      headers.set(line.slice(0, colon).toLowerCase(), line.slice(colon + 1).trim());
    }
    position = end + 4;
    const chunked = headers.get('transfer-encoding') === 'chunked';
    let body: Buffer;
    if (chunked) {
      const chunks: Buffer[] = [];
      while (true) {
        const chunkEnd = data.indexOf('\r\n', position);
        if (chunkEnd < 0) throw new Error('incomplete chunk size');
        const sizeText = data.subarray(position, chunkEnd).toString().split(';')[0];
        if (!/^[0-9a-f]+$/i.test(sizeText)) throw new Error('invalid chunk size');
        const size = Number.parseInt(sizeText, 16);
        position = chunkEnd + 2;
        if (!Number.isSafeInteger(size) || position + size + 2 > data.length)
          throw new Error('incomplete chunk body');
        chunks.push(data.subarray(position, position + size));
        position += size;
        if (data.subarray(position, position + 2).toString() !== '\r\n')
          throw new Error('invalid chunk terminator');
        position += 2;
        if (size === 0) break;
      }
      body = Buffer.concat(chunks);
    } else {
      const sizeText = headers.get('content-length') ?? '0';
      if (!/^\d+$/.test(sizeText)) throw new Error('invalid content length');
      const size = Number(sizeText);
      if (!Number.isSafeInteger(size) || position + size > data.length)
        throw new Error('incomplete request body');
      body = data.subarray(position, position + size);
      position += size;
    }
    rows.push({
      connection,
      method,
      path,
      bytes: body.length,
      sha256: createHash('sha256').update(body).digest('hex'),
      cookie: headers.get('cookie') ?? null,
      action: headers.get('next-action') ?? null,
      chunked,
    });
  }
  return rows;
}

/** Require exact historical bodies and reuse across distinct requests. */
export function validateHistory(rows: Receipt[], expectedCount = 30): Receipt[] {
  const history = rows.filter((row) => row.action === 'test-action-id');
  if (history.length !== expectedCount) throw new Error('missing historical POST receipts');
  const expectedHash = createHash('sha256').update('[]').digest('hex');
  if (
    history.some(
      (row) =>
        row.method !== 'POST' || row.path !== '/' || row.bytes !== 2 || row.sha256 !== expectedHash
    )
  )
    throw new Error('historical POST body changed');
  const counts = new Map<number, number>();
  for (const row of history) counts.set(row.connection, (counts.get(row.connection) ?? 0) + 1);
  if (Math.max(0, ...counts.values()) < 2)
    throw new Error('historical requests do not demonstrate upstream reuse');
  return history;
}

/** Return descriptive milliseconds without a performance pass threshold. */
export function timing(input: number[]) {
  if (input.length === 0 || input.some((value) => !Number.isFinite(value)))
    throw new Error('missing timing samples');
  const values = [...input].sort((a, b) => a - b);
  const middle = Math.floor(values.length / 2);
  const round = (value: number) => Math.round(value * 1000) / 1000;
  return {
    n: values.length,
    median_ms: round(
      values.length % 2 ? values[middle] : (values[middle - 1] + values[middle]) / 2
    ),
    p95_ms: round(values[Math.min(values.length - 1, Math.floor(values.length * 0.95))]),
    max_ms: round(values.at(-1)!),
  };
}

export function analyze(root: string) {
  const streams = new Map<number, Buffer[]>();
  for (const line of readFileSync(resolve(root, 'connections.jsonl'), 'utf8').trim().split('\n')) {
    const event = JSON.parse(line);
    if (event.event === 'request-bytes') {
      const bytes = Buffer.from(event.data, 'base64');
      if (bytes.toString('base64') !== event.data) throw new Error('invalid base64 receipt');
      if (!streams.has(event.id)) streams.set(event.id, []);
      streams.get(event.id)!.push(bytes);
    }
  }
  const rows = [...streams].flatMap(([connection, chunks]) =>
    parseStream(connection, Buffer.concat(chunks))
  );
  const history = validateHistory(rows);
  const result = JSON.parse(readFileSync(resolve(root, 'results.json'), 'utf8'));
  if (result.success !== true) throw new Error('runtime assertions did not pass');
  const counts: Record<string, number> = {};
  for (const row of rows) counts[row.connection] = (counts[row.connection] ?? 0) + 1;
  const actual = rows.filter((row) => row.action && row.action !== 'test-action-id');
  const unique = (receipts: Receipt[]) =>
    [...new Set(receipts.map((row) => row.connection))].sort((a, b) => a - b);
  const timings: Record<string, Record<string, ReturnType<typeof timing>>> = {};
  for (const port of [18980, 18982]) {
    const requests = result.requests.filter((row: { port: number }) => row.port === port);
    timings[port] = {
      historical_post: timing(
        requests
          .filter((row: { path: string }) => row.path === '/')
          .map((row: { ms: number }) => row.ms)
      ),
      api_get: timing(
        requests
          .filter((row: { path: string }) => row.path === '/api/hello')
          .map((row: { ms: number }) => row.ms)
      ),
      concurrent_echo: timing(
        requests
          .filter((row: { path: string }) =>
            row.path.startsWith('/api/echo?probe=probe-concurrent')
          )
          .map((row: { ms: number }) => row.ms)
      ),
      browser_cycles: timing(
        result.browser
          .filter((row: { port: number }) => row.port === port)
          .flatMap((row: { cycles: { ms: number }[] }) => row.cycles.map((cycle) => cycle.ms))
      ),
    };
  }
  const summary = {
    success: true,
    upstream_requests: rows.length,
    used_connections: Object.keys(counts).length,
    requests_per_connection: counts,
    historical_posts: history.length,
    historical_connections: unique(history),
    valid_fetch_actions: actual.length,
    valid_fetch_action_connections: unique(actual),
    timings,
  };
  for (const [name, value] of [
    ['parsed-requests.json', rows],
    ['summary.json', summary],
  ] as const)
    writeFileSync(resolve(root, name), JSON.stringify(value, null, 2) + '\n');
  const metadata = JSON.parse(readFileSync(resolve(root, 'metadata.json'), 'utf8'));
  const lines = [
    '# Axum / Next.js measurement',
    '',
    `Runtime revision: \`${metadata.runtime_revision}\`.`,
    '',
    `Correctness checks passed. ${rows.length} upstream requests used ${Object.keys(counts).length} TCP connections; ${history.length} historical POST receipts were complete and demonstrated reuse.`,
    '',
    '| Workload | Direct median / p95 (ms) | Managed median / p95 (ms) |',
    '| --- | ---: | ---: |',
  ];
  for (const name of Object.keys(timings[18980])) {
    const direct = timings[18980][name],
      managed = timings[18982][name];
    lines.push(
      `| ${name} | ${direct.median_ms} / ${direct.p95_ms} | ${managed.median_ms} / ${managed.p95_ms} |`
    );
  }
  lines.push(
    '',
    'Local HTTP/1, explicitly allowed proxy origin. Timings include fixture pacing and queueing; browser cycles include loading and returning navigation. These are complete-path observations, not a pooled-versus-fresh comparison. No timing threshold is enforced.',
    '',
    'Artifacts include source/tool/binary identity, individual observations, parsed receipts, raw synthetic TCP requests and logs.'
  );
  writeFileSync(resolve(root, 'summary.md'), lines.join('\n') + '\n');
  return summary;
}

if (process.argv[1] && realpathSync(process.argv[1]) === fileURLToPath(import.meta.url))
  console.log(JSON.stringify(analyze(dirname(fileURLToPath(import.meta.url))), null, 2));
