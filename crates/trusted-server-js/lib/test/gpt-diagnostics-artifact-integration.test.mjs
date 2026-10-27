// @vitest-environment node

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { JSDOM } from 'jsdom';
import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest';

import { moduleBuildOptions } from '../build-module-options.mjs';

const libDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
let outputDirectory;
const bundles = {};

beforeAll(async () => {
  outputDirectory = fs.mkdtempSync(path.join(os.tmpdir(), 'tsjs-diagnostics-artifacts-'));
  const { build } = await import('vite');
  for (const name of ['prebid', 'gpt_diagnostics']) {
    await build(
      moduleBuildOptions({
        name,
        entryPath: path.join(libDir, 'src', 'integrations', name, 'index.ts'),
        outDir: outputDirectory,
      })
    );
    bundles[name] = fs.readFileSync(path.join(outputDirectory, `tsjs-${name}.js`), 'utf8');
  }
}, 60_000);

afterAll(() => fs.rmSync(outputDirectory, { recursive: true, force: true }));

function page() {
  const dom = new JSDOM('<!doctype html><div id="ad-slot"></div>', {
    url: 'https://publisher.example/article',
    runScripts: 'outside-only',
    pretendToBeVisual: true,
  });
  const target = dom.window;
  target.TextEncoder = TextEncoder;
  target.CSS = { escape: (value) => value };
  target.tsjs = { navGeneration: 1 };
  target.__tsjs_gpt_diagnostics_active = true;
  target.__tsjs_prebid = { serverSideBidders: [] };
  target.__tsjs_prebid_bundle = {
    schemaVersion: 1,
    modules: { bidder: [], userId: [], analytics: [] },
    runtimeCodes: { bidder: [], analytics: [] },
  };
  const events = new Map();
  const targeting = new Map();
  const slot = {
    getSlotElementId: () => 'ad-slot',
    getAdUnitPath: () => '/example/ad-slot',
    getSizes: () => [[300, 250]],
    getTargeting: vi.fn((key) => targeting.get(key) ?? []),
    clearTargeting: (key) => targeting.delete(key),
  };
  const refresh = vi.fn(() => {
    for (const callback of events.get('slotRequested') ?? []) callback({ slot });
  });
  const pubads = {
    getSlots: () => [slot],
    refresh,
    addEventListener: (name, callback) => {
      events.set(name, [...(events.get(name) ?? []), callback]);
    },
  };
  target.googletag = { cmd: { push: (callback) => callback() }, pubads: () => pubads };
  let auctionSequence = 0;
  const prebidEvents = new Map();
  const prebid = (target.pbjs = {
    que: [],
    cmd: [],
    adUnits: [],
    registerBidAdapter: vi.fn(),
    setConfig: vi.fn(),
    getConfig: () => ({}),
    getBidderCodes: () => ['trustedServer'],
    processQueue: vi.fn(),
    requestBids: vi.fn((options) =>
      options.bidsBackHandler({}, false, `auction-${++auctionSequence}`)
    ),
    setTargetingForGPTAsync: vi.fn(() => {
      targeting.set('hb_bidder', ['client']);
      targeting.set('hb_pb', ['2.40']);
    }),
    onEvent: vi.fn((name, callback) => {
      const listeners = prebidEvents.get(name) ?? new Set();
      listeners.add(callback);
      prebidEvents.set(name, listeners);
    }),
    offEvent: vi.fn((name, callback) => {
      prebidEvents.get(name)?.delete(callback);
    }),
  });
  return {
    dom,
    target,
    prebid,
    prebidEvents,
    pubads,
    refresh,
    slot,
    emitPrebid: (name, bid) => {
      for (const callback of prebidEvents.get(name) ?? []) callback(bid);
    },
  };
}

describe('separately built Prebid shim and diagnostics artifacts', () => {
  it('does not subscribe or read diagnostic targeting when only the shim is loaded', () => {
    const p = page();
    try {
      p.target.eval(bundles.prebid);
      p.pubads.refresh([p.slot]);
      expect(p.refresh).toHaveBeenCalledOnce();
      expect(p.prebid.onEvent).not.toHaveBeenCalled();
      expect(p.slot.getTargeting).not.toHaveBeenCalledWith('hb_bidder');
      expect(p.slot.getTargeting).not.toHaveBeenCalledWith('hb_pb');
      expect(p.target.tsjs.gptDiagnosticsRecorder).toBeUndefined();
    } finally {
      p.dom.window.close();
    }
  });

  it.each([
    ['gpt_diagnostics', 'prebid'],
    ['prebid', 'gpt_diagnostics'],
  ])('records targeting and bidWon with %s loaded before %s', (first, second) => {
    const p = page();
    try {
      p.target.eval(bundles[first]);
      p.target.eval(bundles[second]);
      // The shim captured this instance before a publisher replaced its global.
      const decoy = { onEvent: vi.fn() };
      p.target.pbjs = decoy;
      p.pubads.refresh([p.slot]);
      expect(p.refresh).toHaveBeenCalledExactlyOnceWith([p.slot], undefined);
      expect(p.prebid.setTargetingForGPTAsync).toHaveBeenCalledWith(['ad-slot']);
      expect(decoy.onEvent).not.toHaveBeenCalled();
      expect(p.prebid.onEvent).toHaveBeenCalledExactlyOnceWith('bidWon', expect.any(Function));
      let cycle = p.target.tsjs.gptDiagnostics.snapshot().slots[0].requests[0];
      expect(cycle.prebidAuction).toMatchObject({
        auctionId: 'auction-1',
        targetingCandidate: { bidder: 'client', priceBucket: '2.40' },
      });
      p.emitPrebid('bidWon', {
        auctionId: 'auction-1',
        adUnitCode: 'ad-slot',
        latestTargetedAuctionId: 'auction-1',
        adserverTargeting: { hb_bidder: 'client', hb_pb: '2.40' },
      });
      cycle = p.target.tsjs.gptDiagnostics.snapshot().slots[0].requests[0];
      expect(cycle.prebidAuction.win).toMatchObject({ bidder: 'client', priceBucket: '2.40' });
    } finally {
      p.target.__tsjs_gpt_diagnostics_runtime?.destroy();
      p.dom.window.close();
    }
  });

  it('removes only its own listener across runtime destroy and reinstall', () => {
    const p = page();
    try {
      const publisher = vi.fn();
      p.prebid.onEvent('bidWon', publisher);
      p.target.eval(bundles.gpt_diagnostics);
      p.target.eval(bundles.prebid);
      const previousRecorder = p.target.tsjs.gptDiagnosticsRecorder;
      const previousWin = vi.spyOn(previousRecorder, 'recordPrebidWin');
      p.pubads.refresh([p.slot]);
      const previousListener = p.prebid.onEvent.mock.calls[1][1];
      expect(p.prebidEvents.get('bidWon').size).toBe(2);
      p.target.__tsjs_gpt_diagnostics_runtime.destroy();
      expect(p.prebid.offEvent).toHaveBeenCalledExactlyOnceWith('bidWon', previousListener);
      expect([...p.prebidEvents.get('bidWon')]).toEqual([publisher]);

      p.target.eval(bundles.gpt_diagnostics);
      const currentWin = vi.spyOn(p.target.tsjs.gptDiagnosticsRecorder, 'recordPrebidWin');
      p.pubads.refresh([p.slot]);
      expect(p.prebidEvents.get('bidWon').size).toBe(2);
      const bid = {
        auctionId: 'auction-2',
        adUnitCode: 'ad-slot',
        adserverTargeting: { hb_bidder: 'client', hb_pb: '2.40' },
      };
      previousListener(bid);
      p.emitPrebid('bidWon', bid);
      expect(previousWin).not.toHaveBeenCalled();
      expect(currentWin).toHaveBeenCalledExactlyOnceWith(p.slot, 'auction-2', {
        bidder: 'client',
        priceBucket: '2.40',
      });
      expect(publisher).toHaveBeenCalledExactlyOnceWith(bid);
    } finally {
      p.target.__tsjs_gpt_diagnostics_runtime?.destroy();
      p.dom.window.close();
    }
  });

  it('keeps targeting and GAM refresh fail-open when recorder or subscription throws', () => {
    const p = page();
    try {
      p.target.eval(bundles.gpt_diagnostics);
      p.target.eval(bundles.prebid);
      p.prebid.onEvent.mockImplementationOnce(() => {
        throw new Error('registration failed');
      });
      p.pubads.refresh([p.slot]);
      expect(p.refresh).toHaveBeenCalledTimes(1);
      p.pubads.refresh([p.slot]);
      expect(p.prebid.onEvent).toHaveBeenCalledTimes(2);
      p.target.tsjs.gptDiagnosticsRecorder.recordCompletedPrebidAuction = () => {
        throw new Error('diagnostics failed');
      };
      p.pubads.refresh([p.slot]);
      expect(p.prebid.setTargetingForGPTAsync).toHaveBeenCalledTimes(3);
      expect(p.refresh).toHaveBeenCalledTimes(3);
    } finally {
      p.target.__tsjs_gpt_diagnostics_runtime?.destroy();
      p.dom.window.close();
    }
  });
});
