// @vitest-environment node

// Unit tests share one module graph. These production IIFEs must share state
// even though each bundle contains its own copy of the context and log modules.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { JSDOM } from 'jsdom';
import { build } from 'vite';
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from 'vitest';

import { moduleBuildOptions } from '../build-module-options.mjs';

const libDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const bundles = new Map();
let outputDirectory;
let dom;

beforeAll(async () => {
  outputDirectory = fs.mkdtempSync(path.join(os.tmpdir(), 'trusted-server-iife-state-'));
  for (const name of ['core', 'creative', 'permutive']) {
    const entryPath =
      name === 'core'
        ? path.join(libDir, 'src', 'core', 'index.ts')
        : path.join(libDir, 'src', 'integrations', name, 'index.ts');
    await build(moduleBuildOptions({ name, entryPath, outDir: outputDirectory }));
    bundles.set(name, fs.readFileSync(path.join(outputDirectory, `tsjs-${name}.js`), 'utf8'));
  }
}, 60_000);

afterEach(() => {
  dom?.window.close();
});

afterAll(() => {
  if (outputDirectory) fs.rmSync(outputDirectory, { recursive: true, force: true });
});

function createPage({
  modules = ['core', 'creative', 'permutive'],
  logLevel,
  debug = false,
  storedDebug = false,
  origin = 'https://publisher.example.com',
  scriptSrc,
  stampOrigin,
} = {}) {
  dom = new JSDOM('<!doctype html><head></head><body><div id="slot1"></div></body>', {
    url: `${origin}/${debug ? '?tsdebug=1' : ''}`,
    runScripts: 'outside-only',
  });
  const { window } = dom;
  if (storedDebug) window.localStorage.setItem('tsdebug', '1');
  // Avoid SDK polling and external requests. Script elements are not fetched
  // with outside-only execution and no resource loader.
  window.permutive = { config: {} };
  window.localStorage.setItem(
    'permutive-app',
    JSON.stringify({ core: { cohorts: { all: ['111', '222'] } } })
  );
  const info = vi.fn();
  const debugLog = vi.fn();
  window.console.info = info;
  window.console.log = debugLog;
  window.fetch = vi.fn(async () => ({
    ok: true,
    headers: { get: () => 'application/json' },
    json: async () => ({ seatbid: [] }),
  }));
  if (logLevel) {
    window.tsjs = { que: [() => window.tsjs.setConfig({ logLevel })] };
  }
  if (stampOrigin) window.__tsCreativeOrigin = stampOrigin;
  if (scriptSrc) {
    const script = window.document.createElement('script');
    script.src = scriptSrc;
    Object.defineProperty(window.document, 'currentScript', { configurable: true, value: script });
  }
  // Match bundle.rs: core first and independent IIFEs joined into one script.
  window.eval(modules.map((name) => bundles.get(name)).join(';\n'));
  return { window, info, debugLog };
}

function insertPermutiveScript(window) {
  const script = window.document.createElement('script');
  script.src = 'https://cdn.permutive.com/example-web.js';
  window.document.head.appendChild(script);
  expect(script.src).toBe('https://publisher.example.com/integrations/permutive/sdk');
}

function expectPermutiveInfo(window, info, enabled) {
  info.mockClear();
  insertPermutiveScript(window);
  const lines = info.mock.calls.filter((args) =>
    args.some((arg) => String(arg).includes('Permutive guard: rewriting'))
  );
  expect(lines).toHaveLength(enabled ? 1 : 0);
}

function requestContext(window) {
  window.tsjs.addAdUnits({ code: 'slot1', mediaTypes: { banner: { sizes: [[300, 250]] } } });
  window.tsjs.requestAds();
  expect(window.fetch).toHaveBeenCalledTimes(1);
  const [url, init] = window.fetch.mock.calls[0];
  expect(url).toBe('/auction');
  expect(init.method).toBe('POST');
  return JSON.parse(init.body).config;
}

describe('shared state across production IIFEs', () => {
  it('keeps the captured creative endpoint and repaired click authority across independent IIFEs', async () => {
    const endpoint = 'https://ads.example.com:8443';
    const original = `${endpoint}/first-party/click?tsurl=https%3A%2F%2Fexample.com%2Flanding&foo=1&tstoken=original`;
    const rebuilt = `${endpoint}/first-party/click?tsurl=https%3A%2F%2Fexample.com%2Flanding&bar=2&tstoken=rebuilt`;
    const rebuiltAgain = rebuilt
      .replace('bar=2', 'baz=3')
      .replace('tstoken=rebuilt', 'tstoken=again');
    const { window } = createPage({
      modules: ['core', 'creative'],
      origin: 'https://foreign.example',
      scriptSrc: `${endpoint}/static/tsjs=tsjs-unified.min.js?v=hash`,
      stampOrigin: 'https://publisher.example',
    });
    Object.defineProperty(window.document, 'currentScript', { configurable: true, value: null });
    dom.reconfigure({ url: 'https://later-foreign.example/creative' });
    window.__tsCreativeOrigin = 'https://later-publisher.example';
    window.fetch = vi
      .fn()
      .mockResolvedValueOnce({ ok: true, json: async () => ({ href: rebuilt }) })
      .mockResolvedValue({ ok: true, json: async () => ({ href: rebuiltAgain }) });
    window.open = vi.fn();
    const anchor = window.document.createElement('a');
    anchor.setAttribute('data-tsclick', original);
    anchor.setAttribute('href', original);
    anchor.target = '_blank';
    window.document.body.appendChild(anchor);

    anchor.setAttribute('href', 'https://example.com/landing?bar=2');
    await vi.waitFor(() => {
      expect(anchor.getAttribute('href')).toBe(rebuilt);
      expect(anchor.getAttribute('data-tsclick')).toBe(rebuilt);
    });
    expect(window.fetch).toHaveBeenCalledTimes(1);
    expect(window.fetch.mock.calls[0][0]).toBe(`${endpoint}/first-party/proxy-rebuild`);
    expect(JSON.parse(window.fetch.mock.calls[0][1].body)).toEqual({
      tsclick: original,
      add: { bar: '2' },
      del: ['foo'],
    });
    anchor.click();
    await vi.waitFor(() =>
      expect(window.open).toHaveBeenLastCalledWith(rebuilt, '_blank', 'noopener,noreferrer')
    );
    expect(window.fetch).toHaveBeenCalledTimes(1);

    anchor.setAttribute('href', 'https://example.com/landing?baz=3');
    await vi.waitFor(() => expect(anchor.getAttribute('data-tsclick')).toBe(rebuiltAgain));
    expect(window.fetch).toHaveBeenCalledTimes(2);
    expect(window.fetch.mock.calls[1][0]).toBe(`${endpoint}/first-party/proxy-rebuild`);
    expect(JSON.parse(window.fetch.mock.calls[1][1].body)).toEqual({
      tsclick: rebuilt,
      add: { baz: '3' },
      del: ['bar'],
    });
    anchor.click();
    await vi.waitFor(() =>
      expect(window.open).toHaveBeenLastCalledWith(rebuiltAgain, '_blank', 'noopener,noreferrer')
    );
    expect(anchor.getAttribute('href')).toBe(rebuiltAgain);
    expect(window.fetch).toHaveBeenCalledTimes(2);
  });

  it('sends Permutive context from an integration bundle through core requestAds', () => {
    const { window } = createPage();
    expect(requestContext(window)).toEqual({ permutive_segments: ['111', '222'] });
  });

  it('keeps context empty when Permutive has no cohorts', () => {
    const { window } = createPage();
    window.localStorage.removeItem('permutive-app');
    expect(requestContext(window)).toEqual({});
  });

  it('keeps context empty when the Permutive integration is not loaded', () => {
    const { window } = createPage({ modules: ['core', 'creative'] });
    expect(requestContext(window)).toEqual({});
  });

  it('keeps default logging at warn, including creative installation', () => {
    const { window, info } = createPage();
    expect(window.tsjs.log.getLevel()).toBe('warn');
    expect(info).not.toHaveBeenCalled();
    expectPermutiveInfo(window, info, false);
  });

  it('shares setConfig logLevel changes with integration bundles', () => {
    const { window, info } = createPage();
    window.tsjs.setConfig({ logLevel: 'info' });
    expectPermutiveInfo(window, info, true);
    window.tsjs.setConfig({ logLevel: 'warn' });
    expectPermutiveInfo(window, info, false);
  });

  it('shares setConfig debug changes with integration bundles', () => {
    const { window, info, debugLog } = createPage();
    window.tsjs.setConfig({ debug: true });
    expectPermutiveInfo(window, info, true);
    debugLog.mockClear();
    window.tsjs.requestAds();
    expect(
      debugLog.mock.calls.some((args) =>
        args.some((arg) => String(arg).includes('getPermutiveSegments: found segments'))
      )
    ).toBe(true);
  });

  it('shares direct log.setLevel changes with integration bundles', () => {
    const { window, info } = createPage();
    window.tsjs.log.setLevel('info');
    expectPermutiveInfo(window, info, true);
    window.tsjs.log.setLevel('silent');
    expectPermutiveInfo(window, info, false);
  });

  it('preserves publisher logging configured before integration bundles load', () => {
    const { window, info } = createPage({ logLevel: 'info' });
    expect(window.tsjs.log.getLevel()).toBe('info');
    expectPermutiveInfo(window, info, true);
  });

  it('does not let creative overwrite an explicit publisher warn level', () => {
    const { window, info } = createPage({ logLevel: 'warn' });
    expect(window.tsjs.log.getLevel()).toBe('warn');
    expect(info).not.toHaveBeenCalled();
  });

  it('enables shared logging with the explicit tsdebug flag', () => {
    const { window, info } = createPage({ debug: true });
    expect(window.tsjs.log.getLevel()).toBe('debug');
    expectPermutiveInfo(window, info, true);
  });

  it.each([
    ['query', 'warn'],
    ['query', 'silent'],
    ['localStorage', 'warn'],
    ['localStorage', 'silent'],
  ])('lets %s tsdebug override an earlier publisher %s level', (source, logLevel) => {
    const { window, info, debugLog } = createPage({
      logLevel,
      debug: source === 'query',
      storedDebug: source === 'localStorage',
    });
    expect(window.tsjs.log.getLevel()).toBe('debug');
    expectPermutiveInfo(window, info, true);
    debugLog.mockClear();
    expect(requestContext(window)).toEqual({ permutive_segments: ['111', '222'] });
    expect(
      debugLog.mock.calls.some((args) =>
        args.some((arg) => String(arg).includes('getPermutiveSegments: found segments'))
      )
    ).toBe(true);

    window.tsjs.setConfig({ logLevel });
    expect(window.tsjs.log.getLevel()).toBe(logLevel);
    expectPermutiveInfo(window, info, false);

    window.tsjs.log.setLevel('debug');
    expectPermutiveInfo(window, info, true);
    window.tsjs.log.setLevel(logLevel);
    expect(window.tsjs.log.getLevel()).toBe(logLevel);
    expectPermutiveInfo(window, info, false);
  });

  it('adopts context registered by an integration loaded before core', () => {
    const { window } = createPage({ modules: ['permutive'] });
    window.eval(bundles.get('core'));
    expect(requestContext(window)).toEqual({ permutive_segments: ['111', '222'] });
  });
});
