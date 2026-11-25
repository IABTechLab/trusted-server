import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { test } from 'node:test';
import { spawnSync } from 'node:child_process';
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseStream, validateHistory, timing } from './analyze.ts';

const post = (body = '[]') =>
  Buffer.from(`POST / HTTP/1.1\r\nNext-Action: test-action-id\r\nContent-Length: 2\r\n\r\n${body}`);
test('two requests on one stream retain exact bodies', () => {
  const rows = parseStream(
    7,
    Buffer.concat([post(), Buffer.from('GET /api/hello HTTP/1.1\r\n\r\n')])
  );
  assert.deepEqual(
    rows.map((row) => row.connection),
    [7, 7]
  );
  assert.equal(rows[0].sha256, createHash('sha256').update('[]').digest('hex'));
  assert.equal(rows[1].bytes, 0);
});
test('truncated content length is rejected', () =>
  assert.throws(() => parseStream(1, post('[')), /incomplete request body/));
test('truncated chunk is rejected', () =>
  assert.throws(
    () =>
      parseStream(1, Buffer.from('POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n[')),
    /incomplete chunk body/
  ));
test('chunked input preserves body hash', () => {
  const rows = parseStream(
    1,
    Buffer.from(
      'POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n1\r\n[\r\n1\r\n]\r\n0\r\n\r\n'
    )
  );
  assert.equal(rows[0].bytes, 2);
  assert.equal(rows[0].sha256, createHash('sha256').update('[]').digest('hex'));
});
test('wrong historical body is rejected', () =>
  assert.throws(() => validateHistory(parseStream(1, post('{}')), 1), /body changed/));
test('a fresh upstream connection for every POST does not demonstrate reuse', () =>
  assert.throws(
    () => validateHistory([...parseStream(1, post()), ...parseStream(2, post())], 2),
    /do not demonstrate upstream reuse/
  ));
test('descriptive timings handle unsorted even sample counts', () =>
  assert.deepEqual(timing([4, 1, 3, 2]), { n: 4, median_ms: 2.5, p95_ms: 4, max_ms: 4 }));

test('CLI entry writes evidence when invoked through a directory symlink', () => {
  const root = mkdtempSync(join(tmpdir(), 'axum-nextjs-entry-'));
  try {
    const source = join(root, 'source');
    mkdirSync(source);
    for (const name of ['analyze.ts', 'package.json'])
      copyFileSync(join(dirname(fileURLToPath(import.meta.url)), name), join(source, name));
    symlinkSync(source, join(root, 'alias'), 'dir');
    const data = Buffer.concat(Array.from({ length: 30 }, () => post())).toString('base64');
    writeFileSync(
      join(source, 'connections.jsonl'),
      JSON.stringify({ event: 'request-bytes', id: 1, data }) + '\n'
    );
    const requests = [18980, 18982].flatMap((port) =>
      ['/', '/api/hello', '/api/echo?probe=probe-concurrent-0'].map((path) => ({
        port,
        path,
        ms: 1,
      }))
    );
    const browser = [18980, 18982].map((port) => ({ port, cycles: [{ ms: 1 }] }));
    writeFileSync(
      join(source, 'results.json'),
      JSON.stringify({ success: true, requests, browser })
    );
    writeFileSync(
      join(source, 'metadata.json'),
      JSON.stringify({ runtime_revision: 'fictional-source-revision' })
    );
    const result = spawnSync(process.execPath, [join(root, 'alias/analyze.ts')], {
      encoding: 'utf8',
    });
    assert.equal(result.status, 0, result.stderr);
    assert(
      existsSync(join(source, 'summary.json')),
      'should execute the CLI entry and write its summary'
    );
    assert(existsSync(join(source, 'summary.md')), 'should write the human-readable summary');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
