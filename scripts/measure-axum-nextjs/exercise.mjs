import http from 'node:http';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import { createRequire } from 'node:module';
const require = createRequire(
  process.env.PLAYWRIGHT_PACKAGE ||
    process.env.MEASURE_REPO + '/crates/trusted-server-integration-tests/browser/package.json'
);
const { chromium } = require('playwright');
const root = new URL('./', import.meta.url);
const results = { at: new Date().toISOString(), requests: [], browser: [], errors: [] };
const agents = new Map();
function request(
  port,
  path,
  {
    method = 'GET',
    body = Buffer.alloc(0),
    headers = {},
    chunks = false,
    freshIncoming = false,
  } = {}
) {
  return new Promise((resolve, reject) => {
    const started = performance.now();
    if (!agents.has(port)) agents.set(port, new http.Agent({ keepAlive: true, maxSockets: 16 }));
    const req = http.request(
      {
        host: '127.0.0.1',
        port,
        path,
        method,
        agent: freshIncoming ? false : agents.get(port),
        headers: { ...headers, ...(chunks ? {} : { 'content-length': body.length }) },
      },
      (res) => {
        const data = [];
        res.on('data', (d) => data.push(d));
        res.on('error', reject);
        res.on('end', () => {
          const result = {
            port,
            path,
            method,
            status: res.statusCode,
            headers: res.headers,
            body: Buffer.concat(data).toString(),
            ms: performance.now() - started,
          };
          results.requests.push({ ...result, body: result.body.slice(0, 1000) });
          resolve(result);
        });
      }
    );
    req.setTimeout(15000, () => req.destroy(new Error('request timeout')));
    req.on('error', reject);
    if (chunks) {
      let i = 0;
      const write = () => {
        if (i >= body.length) return req.end();
        req.write(body.subarray(i, i + 4096));
        i += 4096;
        setTimeout(write, 2);
      };
      write();
    } else req.end(body);
  });
}
const digest = (body) => createHash('sha256').update(body).digest('hex');
async function checkEcho(port, id, body, chunks = false) {
  const r = await request(port, '/api/echo?probe=' + id, {
    method: 'POST',
    body,
    chunks,
    headers: { cookie: 'probe=' + id, 'content-type': 'application/octet-stream' },
  });
  assert.equal(r.status, 200);
  const j = JSON.parse(r.body);
  assert.equal(j.method, 'POST');
  assert.equal(j.bytes, body.length);
  assert.equal(j.sha256, digest(body));
  assert(j.cookie.split('; ').includes('probe=' + id));
  return r;
}
async function api(port) {
  for (let i = 0; i < 30; i++) {
    const r = await request(port, '/', {
      method: 'POST',
      body: Buffer.from('[]'),
      chunks: i % 2 === 0,
      freshIncoming: true,
      headers: { 'next-action': 'test-action-id', 'content-type': 'text/plain;charset=UTF-8' },
    });
    assert([200, 404, 405].includes(r.status));
    if (r.status === 200) assert(/404|not found|Not Found/.test(r.body));
    const a = await request(port, '/api/hello', { freshIncoming: true });
    assert.equal(a.status, 200);
    assert.equal(JSON.parse(a.body).message, 'Hello from Next.js');
  }
  await checkEcho(port, 'probe-empty', Buffer.alloc(0));
  await checkEcho(port, 'probe-chunked', Buffer.from('binary-π\0'.repeat(8192)), true);
  for (const status of [303, 307, 308]) {
    const body = Buffer.from('probe-redirect-' + status);
    const r = await request(port, '/api/redirect/' + status, {
      method: 'POST',
      body,
      headers: { cookie: 'probe=before', 'content-type': 'text/plain' },
    });
    assert.equal(r.status, status);
    assert(r.headers['set-cookie'].some((c) => c.startsWith('redirect_probe=present;')));
    const url = new URL(r.headers.location, `http://127.0.0.1:${port}`);
    assert.equal(url.port, String(port));
    const follow = await request(port, url.pathname, {
      method: status === 303 ? 'GET' : 'POST',
      body: status === 303 ? Buffer.alloc(0) : body,
      headers: { cookie: 'probe=before; redirect_probe=present', 'content-type': 'text/plain' },
    });
    assert.equal(follow.status, 200);
    const j = JSON.parse(follow.body);
    assert.equal(j.method, status === 303 ? 'GET' : 'POST');
    assert.equal(j.sha256, digest(status === 303 ? Buffer.alloc(0) : body));
    assert(j.cookie.includes('redirect_probe=present'));
  }
  await Promise.all(
    Array.from({ length: 48 }, (_, i) =>
      checkEcho(
        port,
        'probe-concurrent-' + i,
        Buffer.from(('unique-' + i + '-').repeat(16384)),
        i % 2 === 0
      )
    )
  );
  const after = await request(port, '/api/hello');
  assert.equal(after.status, 200);
}
async function browserRun(browser, port, index, javascript = true) {
  const context = await browser.newContext({ javaScriptEnabled: javascript });
  const page = await context.newPage();
  const actions = [];
  const errors = [];
  const cycles = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  page.on('response', (r) => {
    if (r.request().method() === 'POST')
      actions.push({
        status: r.status(),
        url: r.url(),
        action: r.request().headers()['next-action'] || null,
      });
  });
  const started = performance.now();
  try {
    for (let step = 0; step < 3; step++) {
      const cycleStarted = performance.now();
      await page.goto(`http://127.0.0.1:${port}/`);
      if (javascript)
        await page.waitForFunction(() =>
          Boolean(
            document.querySelector('form')?.[
              Object.keys(document.querySelector('form')).find((k) => k.startsWith('__reactProps$'))
            ]
          )
        );
      const token = `probe-browser-${port}-${index}-${step}`;
      await page.locator('input[name=token]').fill(token);
      await page.getByRole('button', { name: 'Submit' }).click();
      await page.waitForURL('**/done?token=' + token);
      await page.locator('#cookie').waitFor();
      assert.equal(await page.locator('#token').innerText(), token);
      assert.equal(await page.locator('#cookie').innerText(), token);
      const cookie = (await context.cookies()).find((c) => c.name === 'probe');
      assert.equal(cookie.value, token);
      assert.equal(cookie.httpOnly, true);
      await page.getByRole('link', { name: 'Return', exact: true }).click();
      await page.waitForURL(`http://127.0.0.1:${port}/`);
      await page.getByRole('heading', { name: 'Action probe' }).waitFor();
      cycles.push({ step, ms: performance.now() - cycleStarted });
    }
    assert.equal(actions.length, 3);
    assert(actions.every((action) => action.status === 303));
    assert(
      actions.every((action) => (javascript ? Boolean(action.action) : action.action === null))
    );
    assert.equal(errors.length, 0, errors.join('\n'));
    results.browser.push({
      port,
      index,
      javascript,
      actions,
      cycles,
      ms: performance.now() - started,
      errors,
    });
  } finally {
    await context.close();
  }
}
try {
  for (const port of [18980, 18982]) await api(port);
  const browser = await chromium.launch({ headless: true });
  results.chromium = browser.version();
  try {
    for (const port of [18980, 18982]) {
      await browserRun(browser, port, 0);
      await browserRun(browser, port, 1, false);
      await Promise.all(Array.from({ length: 4 }, (_, i) => browserRun(browser, port, i + 2)));
    }
  } finally {
    await browser.close();
  }
  results.success = true;
} catch (error) {
  results.errors.push(String(error.stack));
  process.exitCode = 1;
} finally {
  for (const agent of agents.values()) agent.destroy();
  fs.writeFileSync(new URL('results.json', root), JSON.stringify(results, null, 2));
  console.log(
    JSON.stringify({
      success: results.success,
      requests: results.requests.length,
      browser: results.browser.length,
      errors: results.errors,
    })
  );
}
