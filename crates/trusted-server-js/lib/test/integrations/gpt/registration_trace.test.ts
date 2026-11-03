import { readFileSync } from 'node:fs';

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type {
  AuctionBidData,
  AuctionSlot,
  PublisherGptSlot,
  TsjsApi,
} from '../../../src/core/types';
import { GptDiagnosticsStore } from '../../../src/integrations/gpt_diagnostics/store';
import { settlePublisherSlotPayload } from '../../../src/integrations/gpt/publisher_registration';
import { installTraceRuntime } from '../../../src/trace/runtime';
import { GPT_BOOTSTRAP_PATH } from '../../fixtures/paths';
import { AUCTION_TOKEN, SLOT_TOKEN } from '../../trace/fixtures';
import { gptTransport, joinedGptStore } from '../../trace/gpt-fixtures';

for (const implementation of [
  'runtime',
  'bootstrap',
  'waiting_takeover',
  'requested_takeover',
] as const) {
  describe(`registered trace opportunities: ${implementation}`, () => {
    let ts: TsjsApi;
    let store: GptDiagnosticsStore;
    let collector: NonNullable<ReturnType<typeof installTraceRuntime>>;
    let frames: FrameRequestCallback[];
    let commands: Array<() => void>;
    let nativeSlots: PublisherGptSlot[];
    let listeners: Array<[string, EventListenerOrEventListenerObject]>;
    let refresh: ReturnType<typeof vi.fn>;
    let display: ReturnType<typeof vi.fn>;
    const pushState = history.pushState;
    const replaceState = history.replaceState;

    function configured(
      initialization: 'auto' | 'publisher_registered' = 'publisher_registered'
    ): AuctionSlot {
      return {
        id: 'example',
        div_id: 'example',
        gam_unit_path: '/123/example',
        formats: [[300, 250]],
        initialization,
        ext: { trusted_server: { trace_slot_ref: SLOT_TOKEN } },
      };
    }
    function selectedTransport(source = 'initial_navigation_ssat') {
      const transport = gptTransport(source);
      return {
        ...transport,
        evidence: {
          ...transport.evidence,
          slots: [
            {
              ...transport.evidence.slots[0],
              returned_bid_count: 1,
              candidate: 'selected',
              selected_creative_size: [300, 250],
            },
          ],
        },
      };
    }
    function bid(): AuctionBidData {
      return {
        hb_adid: 'example-creative',
        hb_bidder: 'exampleBidder',
        hb_pb: '1.00',
        hb_auction_id: AUCTION_TOKEN,
        w: 300,
        h: 250,
        adm: '<b>example creative</b>',
      };
    }
    function mount(id = 'example') {
      const element = document.createElement('div');
      element.id = id;
      document.body.append(element);
      const targeting: Record<string, string | string[]> = {};
      const native: PublisherGptSlot = {
        getSlotElementId: () => id,
        getAdUnitPath: () => '/123/example',
        setTargeting: (key, value) => {
          targeting[key] = value;
          return native;
        },
        clearTargeting: (key) => {
          if (key) delete targeting[key];
          return native;
        },
        getTargeting: (key) => {
          const value = targeting[key];
          return value === undefined ? [] : Array.isArray(value) ? value : [value];
        },
      };
      nativeSlots.push(native);
      return { native, targeting };
    }
    function register(
      native: PublisherGptSlot,
      eligibleFormats: readonly (readonly [number, number])[] = [[300, 250]]
    ) {
      return ts.registerPublisherSlot!({
        slotId: 'example',
        gptSlot: native,
        navigationGeneration: ts.navGeneration ?? 0,
        eligibleFormats,
      });
    }
    function tick() {
      frames.splice(0).forEach((callback) => callback(0));
      commands.splice(0).forEach((callback) => callback());
    }
    function flush() {
      for (let index = 0; index < 6; index++) tick();
    }
    async function runtime() {
      const module = await import('../../../src/integrations/gpt/index');
      module.installTsAdInit();
      module.installSpaAuctionHook();
      module.installTsRenderBridge();
    }
    async function takeoverBeforeDispatch() {
      if (implementation === 'waiting_takeover') await runtime();
    }
    async function takeoverAfterDispatch() {
      if (implementation === 'requested_takeover') {
        await runtime();
        flush();
      }
    }
    function assertJoined(opportunity = 'renderable_candidate') {
      expect(refresh).toHaveBeenCalledTimes(1);
      expect(store.snapshot().slots[0]?.requests[0]?.trustedServerOpportunity).toBe(opportunity);
      expect(collector.snapshot().value?.slotCorrelations).toEqual([
        {
          schema_version: 1,
          diagnostic_auction_id: AUCTION_TOKEN,
          slot_ref: SLOT_TOKEN,
          runtime_slot_number: 1,
          request_number: 1,
        },
      ]);
      expect(joinedGptStore(store, collector)?.auctions[0]?.slots[0]?.correlation).toBe('matched');
    }
    beforeEach(async () => {
      vi.resetModules();
      vi.useFakeTimers();
      frames = [];
      commands = [];
      nativeSlots = [];
      listeners = [];
      history.pushState = pushState;
      history.replaceState = replaceState;
      replaceState.call(history, {}, '', '/');
      ts = {} as TsjsApi;
      window.tsjs = ts;
      window.__tsjs_trace_active = true;
      collector = installTraceRuntime(ts)!;
      // Keep the real bridge closures, with a mutable facade for call-order spies.
      ts.traceGpt = { ...ts.traceGpt! };
      store = new GptDiagnosticsStore({
        onTraceCorrelation: (value) => collector.recordCorrelation(value),
      });
      ts.gptDiagnosticsRecorder = store;
      refresh = vi.fn((slots: PublisherGptSlot[]) =>
        slots.forEach((slot) => store.recordSlotRequested(slot))
      );
      display = vi.fn();
      const service = {
        getSlots: () => nativeSlots,
        refresh,
        addEventListener: vi.fn(),
        enableSingleRequest: vi.fn(),
      };
      Object.assign(window, {
        googletag: {
          cmd: { push: (callback: () => void) => commands.push(callback) },
          getConfig: () => ({ disableInitialLoad: true }),
          pubads: () => service,
          display,
          enableServices: vi.fn(),
          defineSlot: vi.fn(),
        },
      });
      vi.spyOn(window, 'requestAnimationFrame').mockImplementation((callback) => {
        frames.push(callback);
        return frames.length;
      });
      vi.spyOn(document, 'hidden', 'get').mockReturnValue(false);
      vi.spyOn(document, 'readyState', 'get').mockReturnValue('complete');
      const add = window.addEventListener.bind(window);
      vi.spyOn(window, 'addEventListener').mockImplementation((type, listener, options) => {
        listeners.push([type, listener]);
        add(type, listener, options);
      });
      vi.stubGlobal(
        'fetch',
        vi.fn(() => new Promise<Response>(() => {}))
      );
      if (implementation === 'runtime') await runtime();
      else {
        // Install the actual renderer, not a capability flag, to exercise bootstrap candidate parity.
        const { installTsRenderBridge } =
          await import('../../../src/integrations/gpt/render_bridge');
        installTsRenderBridge();
        new Function(readFileSync(GPT_BOOTSTRAP_PATH, 'utf8'))();
      }
      flush();
    });
    afterEach(() => {
      for (const [type, listener] of listeners) {
        window.removeEventListener(type, listener);
        window.removeEventListener(type, listener, true);
      }
      history.pushState = pushState;
      history.replaceState = replaceState;
      replaceState.call(history, {}, '', '/');
      document.body.innerHTML = '';
      Reflect.deleteProperty(window, 'googletag');
      Reflect.deleteProperty(window, 'tsjs');
      Reflect.deleteProperty(window, '__tsjs_trace_active');
      vi.clearAllTimers();
      vi.useRealTimers();
      vi.restoreAllMocks();
      vi.unstubAllGlobals();
    });

    it.each(['registration_first', 'payload_first', 'prebound'] as const)(
      'joins the first native request with the exact accepted trace identity: %s',
      async (order) => {
        const slot = configured();
        const { native, targeting } = mount();
        const observe = vi.spyOn(ts.traceGpt!, 'observeTransport');
        if (order === 'prebound') {
          // Isolate registration's identity forwarding from the scheduler's ordering.
          ts.traceGpt!.observeTransport([slot], selectedTransport(), 'initial_navigation_ssat');
          ts.adSlots = [slot];
          ts.bids = { example: bid() };
          settlePublisherSlotPayload(ts, 0, ts.adSlots, ts.bids);
          register(native);
        } else {
          if (order === 'registration_first') register(native);
          ts.scheduleInitialAdInit!({ example: bid() }, [slot], selectedTransport());
          if (order === 'payload_first') register(native);
        }
        await takeoverBeforeDispatch();
        // Registered work may dispatch a frame before automatic adInit. Identity must already exist.
        tick();
        assertJoined();
        expect(targeting.hb_adid).toBe('example-creative');
        expect(ts.publisherSlotRegistrationState?.records.example.selectedCandidate?.hb_adid).toBe(
          'example-creative'
        );
        flush();
        await takeoverAfterDispatch();
        assertJoined();
        expect(observe).toHaveBeenCalledTimes(1);
        expect(display).toHaveBeenCalledTimes(1);
      }
    );
    it('inherits the validated diagnostic marker only when the selected candidate has none', async () => {
      const slot = configured();
      const { native } = mount();
      register(native);
      const candidate = bid();
      delete candidate.hb_auction_id;
      ts.scheduleInitialAdInit!({ example: candidate }, [slot], selectedTransport());
      await takeoverBeforeDispatch();
      flush();
      assertJoined();
      expect(store.snapshot().slots[0]?.requests[0]?.trustedServerAuctionId).toBe(AUCTION_TOKEN);
    });
    it.each(['no_bid', 'ineligible', 'renderer_unavailable'] as const)(
      'correlates a native no-candidate request without selecting the server candidate: %s',
      async (reason) => {
        const slot = configured();
        const { native, targeting } = mount();
        register(native, reason === 'ineligible' ? [[300, 600]] : [[300, 250]]);
        if (reason === 'renderer_unavailable') delete ts.tsRenderBridgeVersion;
        const candidate = bid();
        // An excluded candidate's marker must not become this native request's identity.
        if (reason === 'ineligible')
          candidate.hb_auction_id = 'ts-auc-2234567812344abc8def123456789abc';
        ts.scheduleInitialAdInit!(
          reason === 'no_bid' ? {} : { example: candidate },
          [slot],
          reason === 'no_bid' ? gptTransport() : selectedTransport()
        );
        // No takeover before dispatch: the missing-renderer observation belongs to this pass.
        flush();
        assertJoined('no_candidate');
        expect(targeting).not.toHaveProperty('hb_adid');
        expect(
          ts.publisherSlotRegistrationState?.records.example.selectedCandidate
        ).toBeUndefined();
        expect(ts.publisherSlotRegistrationState?.records.example.noCandidateReason).toBe(
          reason === 'ineligible' ? 'incompatible_dimensions' : reason
        );
        await takeoverAfterDispatch();
        assertJoined('no_candidate');
      }
    );
    it('matches the automatic path through the same validated bridge and recorder', async () => {
      const slot = configured('auto');
      mount();
      ts.scheduleInitialAdInit!({ example: bid() }, [slot], selectedTransport());
      await takeoverBeforeDispatch();
      flush();
      await takeoverAfterDispatch();
      assertJoined();
    });
    it.each([
      'inactive',
      'malformed_transport',
      'throwing_bridge',
      'throwing_recorder',
      'mismatched_marker',
    ] as const)('keeps delivery independent of %s diagnostics', async (failure) => {
      const slot = configured();
      const { native, targeting } = mount();
      register(native);
      let transport: unknown = selectedTransport();
      if (failure === 'inactive') window.__tsjs_trace_active = false;
      if (failure === 'malformed_transport') transport = { schema_version: 1 };
      if (failure === 'throwing_bridge') {
        ts.traceGpt = {
          ...ts.traceGpt!,
          observeTransport: () => {
            throw new Error('example transport error');
          },
          opportunity: () => {
            throw new Error('example binding error');
          },
        };
      }
      if (failure === 'throwing_recorder')
        vi.spyOn(store, 'recordTrustedServerOpportunity').mockImplementation(() => {
          throw new Error('example recorder error');
        });
      const candidate = bid();
      if (failure === 'mismatched_marker')
        candidate.hb_auction_id = 'ts-auc-2234567812344abc8def123456789abc';
      ts.scheduleInitialAdInit!({ example: candidate }, [slot], transport);
      await takeoverBeforeDispatch();
      flush();
      await takeoverAfterDispatch();
      expect(refresh).toHaveBeenCalledTimes(1);
      expect(targeting.hb_adid).toBe('example-creative');
      expect(ts.publisherSlotRegistrationState?.records.example.status.status).toBe('requested');
      expect(collector.snapshot().value?.slotCorrelations ?? []).toEqual([]);
    });
    it('does not re-observe the initial batch after registered delivery and erase a newer binding', async () => {
      const slot = configured();
      const { native } = mount();
      register(native);
      ts.scheduleInitialAdInit!({ example: bid() }, [slot], selectedTransport());
      await takeoverBeforeDispatch();
      tick();
      assertJoined();
      const newer = configured();
      ts.traceGpt!.observeTransport([newer], gptTransport('spa_page_bids'), 'spa_page_bids');
      flush();
      expect(ts.traceGpt!.identity(newer)).toEqual({
        diagnostic_auction_id: AUCTION_TOKEN,
        slot_ref: SLOT_TOKEN,
      });
      expect(ts.traceGpt!.identity(slot)).toBeUndefined();
      expect(refresh).toHaveBeenCalledTimes(1);
    });
    it('does not observe or admit a document transport arriving after navigation', async () => {
      const observe = vi.spyOn(ts.traceGpt!, 'observeTransport');
      history.pushState({}, '', '/before-document-seam');
      const { native } = mount();
      const handle = register(native);
      ts.scheduleInitialAdInit!({ example: bid() }, [configured()], selectedTransport());
      flush();
      expect(observe).not.toHaveBeenCalled();
      expect(collector.snapshot().value?.serverAuctions ?? []).toEqual([]);
      expect(handle.getStatus().status).toBe('waiting_payload');
      expect(refresh).not.toHaveBeenCalled();
    });
    it('rejects a forced stale SPA response before observing transport or settling registrations', async () => {
      await runtime();
      flush();
      const responses: Array<(response: Response) => void> = [];
      vi.mocked(fetch).mockImplementation(
        () => new Promise<Response>((resolve) => responses.push(resolve))
      );
      const observe = vi.spyOn(ts.traceGpt!, 'observePageBids');
      history.pushState({}, '', '/stale-example');
      const stale = mount('stale-native');
      const old = register(stale.native);
      history.pushState({}, '', '/current-example');
      const { native } = mount();
      const current = register(native);
      const response = () =>
        new Response(
          JSON.stringify({
            slots: [configured()],
            bids: { example: bid() },
            trace_auction: selectedTransport('spa_page_bids'),
          })
        );
      responses[0](response());
      await vi.advanceTimersByTimeAsync(0);
      flush();
      expect(observe).not.toHaveBeenCalled();
      expect(old.getStatus().status).toBe('cancelled');
      expect(current.getStatus().status).toBe('waiting_payload');
      expect(refresh).not.toHaveBeenCalled();
      responses[1](response());
      await vi.advanceTimersByTimeAsync(0);
      flush();
      assertJoined();
      expect(observe).toHaveBeenCalledTimes(1);
      expect(current.getStatus().status).toBe('requested');
    });
    it('observes accepted SPA transport before registered dispatch, not after the automatic DOM wait', async () => {
      await runtime();
      flush();
      const payload = {
        slots: [
          configured(),
          { ...configured('auto'), id: 'missing', div_id: 'missing', ext: undefined },
        ],
        bids: { example: bid() },
        trace_auction: selectedTransport('spa_page_bids'),
      };
      vi.mocked(fetch).mockResolvedValueOnce(new Response(JSON.stringify(payload)));
      const observe = vi.spyOn(ts.traceGpt!, 'observePageBids');
      history.pushState({}, '', '/example-spa');
      const { native } = mount();
      register(native);
      await vi.advanceTimersByTimeAsync(0);
      tick();
      assertJoined();
      expect(observe).toHaveBeenCalledTimes(1);
      const accepted = ts.publisherSlotRegistrationState!.payload!.slots[0];
      expect(ts.traceGpt!.identity(accepted)).toEqual({
        diagnostic_auction_id: AUCTION_TOKEN,
        slot_ref: SLOT_TOKEN,
      });
      const newer = configured();
      ts.traceGpt!.observeTransport([newer], gptTransport('spa_page_bids'), 'spa_page_bids');
      await vi.advanceTimersByTimeAsync(2000);
      flush();
      expect(observe).toHaveBeenCalledTimes(1);
      expect(ts.traceGpt!.identity(newer)).toBeDefined();
      expect(refresh).toHaveBeenCalledTimes(1);
    });
  });
}
