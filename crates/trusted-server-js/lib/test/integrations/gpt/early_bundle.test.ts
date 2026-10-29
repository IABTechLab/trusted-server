// @vitest-environment node
import { readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

import { JSDOM } from 'jsdom';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { build, type InlineConfig } from 'vite';

import type { AuctionSlot, PublisherGptSlot, TsjsApi } from '../../../src/core/types';
import envelope from '../../fixtures/aps-renderer-v1.json';
import { GPT_BOOTSTRAP_PATH } from '../../fixtures/paths';

const rawBootstrap = readFileSync(GPT_BOOTSTRAP_PATH, 'utf8');
const libRoot = resolve(import.meta.dirname, '../../..');
const configured: AuctionSlot = {
  id: 'header',
  gam_unit_path: '/123/example/header',
  div_id: 'header-final',
  formats: [[300, 250]],
  initialization: 'publisher_registered',
  targeting: { route: 'example' },
};

async function bundle(name: string): Promise<string> {
  const { moduleBuildOptions } = await import(
    pathToFileURL(resolve(libRoot, 'build-module-options.mjs')).href
  );
  const options = moduleBuildOptions({
    name,
    entryPath: resolve(libRoot, `src/integrations/${name}/index.ts`),
    outDir: resolve(tmpdir(), 'tsjs-early-bundle-unused'),
  }) as InlineConfig;
  // Exercise the production bundler without writing into any shared distribution.
  options.build!.write = false;
  const result = await build(options);
  for (const output of Array.isArray(result) ? result : [result]) {
    if (!('output' in output)) continue;
    for (const chunk of output.output) {
      if (chunk.type === 'chunk') return chunk.code;
    }
  }
  throw new Error(`Missing compiled ${name} entry`);
}

/** Isolated document using exact production IIFEs, not a second loading implementation. */
function documentFixture(earlySource: string, attributes: Record<string, string> = {}) {
  const dom = new JSDOM('<html><head></head><body><div id="header-final"></div></body></html>', {
    url: 'https://publisher.example.com/start',
    runScripts: 'outside-only',
    pretendToBeVisual: true,
  });
  const win = dom.window;
  const ts = { adSlots: [configured], bids: {} } as TsjsApi;
  Object.assign(win, {
    tsjs: ts,
    __tsjs_gpt_enabled: true,
    __tsjs_gam_attribution_enabled: attributes['data-ts-gam-attribution'] === 'true',
  });
  const frames: FrameRequestCallback[] = [];
  win.requestAnimationFrame = (callback) => frames.push(callback);
  const targeting: Record<string, string | string[]> = {};
  const native: PublisherGptSlot = {
    getAdUnitPath: () => configured.gam_unit_path,
    getSlotElementId: () => 'header-final',
    setTargeting: (key, value) => {
      targeting[key] = value;
      return native;
    },
    clearTargeting: (key) => {
      if (key) delete targeting[key];
      return native;
    },
  };
  const service = {
    getSlots: () => [native],
    addEventListener: vi.fn(),
    enableSingleRequest: vi.fn(),
    refresh: vi.fn(),
  };
  const tag = {
    cmd: { push: (callback: () => void) => callback() },
    getConfig: () => ({ disableInitialLoad: true }),
    setConfig: vi.fn(),
    defineSlot: vi.fn(),
    display: vi.fn(),
    enableServices: vi.fn(),
    pubads: () => service,
  };
  const nativeRefresh = service.refresh;
  const nativeSetConfig = tag.setConfig;
  Object.assign(win, { googletag: tag });
  win.fetch = vi.fn(async () => ({
    ok: true,
    headers: { get: () => 'application/json' },
    json: async () => ({ seatbid: [] }),
  })) as unknown as typeof win.fetch;
  const script = win.document.createElement('script');
  for (const [name, value] of Object.entries(attributes)) script.setAttribute(name, value);
  Object.defineProperty(win.document, 'currentScript', { configurable: true, get: () => script });
  Object.defineProperty(win.document, 'readyState', { configurable: true, value: 'complete' });
  win.eval(rawBootstrap);

  function flushFrames() {
    for (let pass = 0; pass < 4; pass++) {
      const pending = frames.splice(0);
      pending.forEach((callback) => callback(0));
    }
  }
  function register() {
    return ts.registerPublisherSlot!({
      slotId: 'header',
      gptSlot: native,
      navigationGeneration: ts.navGeneration ?? 0,
      eligibleFormats: [[300, 250]],
    });
  }
  function requestCreative() {
    const iframe = win.document.createElement('iframe');
    win.document.getElementById('header-final')!.appendChild(iframe);
    const port = { postMessage: vi.fn() };
    const event = new win.MessageEvent('message', {
      data: { message: 'Prebid Request', adId: 'early-ad' },
      source: iframe.contentWindow,
      ports: [port as unknown as MessagePort],
    });
    win.dispatchEvent(event);
    return { iframe, port };
  }
  return {
    dom,
    win,
    ts,
    tag,
    service,
    nativeRefresh,
    nativeSetConfig,
    targeting,
    register,
    requestCreative,
    flushFrames,
    installEarly: () => win.eval(earlySource),
  };
}

describe('compiled early GPT entry', () => {
  let earlySource: string;
  let runtimeSource: string;
  const documents: JSDOM[] = [];

  beforeAll(async () => {
    earlySource = await bundle('bootstrap');
    runtimeSource = await bundle('gpt');
  });
  afterEach(() => {
    documents.splice(0).forEach((dom) => dom.window.close());
  });
  function fixture(attributes?: Record<string, string>) {
    const result = documentFixture(earlySource, attributes);
    documents.push(result.dom);
    return result;
  }
  function candidate(f: ReturnType<typeof fixture>) {
    f.ts.bids = {
      header: {
        hb_adid: 'early-ad',
        hb_pb: '1.00',
        w: 300,
        h: 250,
        adm: '<div>early creative</div>',
      },
    };
  }

  it('installs real rendering before core flushes publisher callbacks', () => {
    const f = fixture();
    candidate(f);
    let callbackCompleted = false;
    const callback = vi.fn(() => {
      expect(f.ts.tsRenderBridgeVersion).toBe(1);
      expect(typeof f.ts.getConfig).toBe('function');
      const script = f.win.document.createElement('script');
      script.src = 'https://securepubads.g.doubleclick.net/pagead/example.js';
      expect(script.src).toBe('https://publisher.example.com/integrations/gpt/pagead/example.js');
      const handle = f.register();
      f.ts.scheduleInitialAdInit!(f.ts.bids, [configured]);
      f.flushFrames();
      expect(handle.getStatus().status).toBe('requested');
      expect(f.requestCreative().port.postMessage).toHaveBeenCalledOnce();
      callbackCompleted = true;
    });
    f.ts.que = [callback];
    f.installEarly();
    expect(callback).toHaveBeenCalledOnce();
    expect(callbackCompleted).toBe(true);
    expect(f.nativeRefresh).toHaveBeenCalledOnce();
  });

  it('preserves direct core APIs, queue timing and early state across runtime takeover', () => {
    const f = fixture();
    f.installEarly();
    const getConfig = f.ts.getConfig;
    f.ts.setConfig!({ debug: false, publisherValue: 'keep' });
    f.ts.addAdUnits({ code: 'header-final', mediaTypes: { banner: { sizes: [[300, 250]] } } });
    const queued = vi.fn();
    f.ts.que.push(queued);
    expect(queued).toHaveBeenCalledOnce();
    const callback = vi.fn();
    f.ts.requestAds!(callback);
    expect(callback).toHaveBeenCalledOnce();
    expect(f.win.fetch).toHaveBeenCalledOnce();
    expect(f.ts.getConfig!()).toMatchObject({ publisherValue: 'keep' });

    f.win.eval(runtimeSource);
    expect(f.ts.getConfig).toBe(getConfig);
    expect(f.ts.getConfig!()).toMatchObject({ publisherValue: 'keep' });
    f.ts.renderAdUnit('header-final');
    expect(f.win.document.getElementById('header-final')!.textContent).toContain('300x250');
    expect(f.win.tsjs).toBe(f.ts);
  });

  it('renders the eligible initial candidate without the deferred runtime and never re-requests it', () => {
    const f = fixture();
    candidate(f);
    f.installEarly();
    const handle = f.register();
    f.ts.scheduleInitialAdInit!(f.ts.bids, [configured]);
    f.flushFrames();
    expect(handle.getStatus().status).toBe('requested');
    expect(f.targeting.hb_adid).toBe('early-ad');
    const first = f.requestCreative();
    expect(first.port.postMessage).toHaveBeenCalledOnce();
    expect(JSON.parse(first.port.postMessage.mock.calls[0][0])).toMatchObject({
      message: 'Prebid Response',
      ad: '<div>early creative</div>',
      width: 300,
      height: 250,
    });
    const addListener = vi.spyOn(f.win, 'addEventListener');
    f.win.eval(runtimeSource);
    f.flushFrames();
    expect(handle.getStatus().status).toBe('requested');
    expect(f.nativeRefresh).toHaveBeenCalledOnce();
    expect(addListener.mock.calls.filter(([name]) => name === 'message')).toHaveLength(0);
  });

  it('keeps one native guard and attribution owner across separately compiled IIFEs', () => {
    const f = fixture({ 'data-ts-gam-attribution': 'true' });
    const nativeWrite = f.win.document.write;
    // Preserve the existing raw-bootstrap marker plus one compiled-entry marker.
    expect(f.nativeSetConfig).toHaveBeenCalledExactlyOnceWith({ targeting: { ts: 'true' } });
    f.installEarly();
    const early = {
      write: f.win.document.write,
      writeln: f.win.document.writeln,
      createElement: f.win.document.createElement,
      setAttribute: f.win.HTMLScriptElement.prototype.setAttribute,
      src: Object.getOwnPropertyDescriptor(f.win.HTMLScriptElement.prototype, 'src'),
      appendChild: f.win.Node.prototype.appendChild,
      installShim: f.win.__tsjs_installGptShim,
    };
    expect(early.write).not.toBe(nativeWrite);
    expect(f.nativeSetConfig).toHaveBeenCalledTimes(2);
    f.win.eval(runtimeSource);
    f.win.__tsjs_installGptShim();
    expect(f.win.document.write).toBe(early.write);
    expect(f.win.document.writeln).toBe(early.writeln);
    expect(f.win.document.createElement).toBe(early.createElement);
    expect(f.win.HTMLScriptElement.prototype.setAttribute).toBe(early.setAttribute);
    expect(Object.getOwnPropertyDescriptor(f.win.HTMLScriptElement.prototype, 'src')).toEqual(
      early.src
    );
    expect(f.win.Node.prototype.appendChild).toBe(early.appendChild);
    expect(f.win.__tsjs_installGptShim).toBe(early.installShim);
    expect(f.nativeSetConfig).toHaveBeenCalledTimes(2);
  });

  it('captures trusted GAM attribution from the early executing tag', () => {
    const f = fixture({ 'data-ts-gam-attribution': 'true' });
    f.installEarly();
    expect(f.nativeSetConfig).toHaveBeenCalledWith({ targeting: { ts: 'true' } });
  });

  it('captures publisher-native APS mode before either runtime or publisher script runs', async () => {
    const f = fixture({ 'data-ts-aps-rendering-mode': 'publisher_native' });
    candidate(f);
    const bid = envelope.seatbid[0].bid[0];
    f.ts.bids!.header.renderer = {
      type: 'aps',
      version: 1,
      accountId: 'example-account-id',
      bidId: bid.id,
      tagType: bid.ext.tagtype as 'iframe',
      creativeUrl: bid.ext.creativeurl,
      aaxResponse: Buffer.from(JSON.stringify(envelope)).toString('base64'),
      width: bid.w,
      height: bid.h,
    };
    f.installEarly();
    f.register();
    f.ts.scheduleInitialAdInit!(f.ts.bids, [configured]);
    f.flushFrames();
    f.requestCreative();
    const runner = f.win.document.querySelectorAll('iframe')[1];
    expect(runner).toBeDefined();
    expect(runner.getAttribute('sandbox')).toBeNull();
    const script = runner.contentDocument!.querySelector('script')!;
    expect(script.src).toBe('https://client.aps.amazon-adsystem.com/prebid-creative.js');
    script.dispatchEvent(new f.win.Event('error'));
    await Promise.resolve();
  });

  it('adopts a pre-runtime navigation without replaying the SSR payload or changing its generation', async () => {
    const f = fixture();
    candidate(f);
    f.installEarly();
    f.ts.scheduleInitialAdInit!(f.ts.bids, [configured]);
    const frameworkPush = vi.fn(f.win.history.pushState.bind(f.win.history));
    f.win.history.pushState = frameworkPush;
    f.win.history.pushState({}, '', '/next');
    expect(f.ts.navGeneration).toBe(1);
    const handle = f.register();
    f.flushFrames();
    expect(handle.getStatus().status).toBe('waiting_payload');
    expect(f.nativeRefresh).not.toHaveBeenCalled();
    const fetch = vi.fn(async () => ({ ok: true, json: async () => ({ slots: [], bids: {} }) }));
    f.win.fetch = fetch as unknown as typeof f.win.fetch;
    f.win.eval(runtimeSource);
    await vi.waitFor(() => expect(handle.getStatus().status).toBe('inactive'));
    expect(fetch).toHaveBeenCalledExactlyOnceWith(
      '/_ts/page-bids?path=%2Fnext',
      expect.any(Object)
    );
    expect(f.ts.navGeneration).toBe(1);
    expect(f.win.history.pushState).toBe(frameworkPush);
    expect(f.nativeRefresh).not.toHaveBeenCalled();
  });
});
