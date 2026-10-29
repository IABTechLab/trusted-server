import { readFileSync } from 'node:fs';

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { AuctionBidData, AuctionSlot, PublisherGptSlot, TsjsApi } from '../../src/core/types';

import { GPT_BOOTSTRAP_PATH } from './paths';

type Implementation = 'runtime' | 'bootstrap' | 'waiting_handoff' | 'requested_handoff';

/** The same native calls and admission assertions run against every shipping implementation. */
export function publisherRegistrationContract(implementation: Implementation): void {
  describe(`publisher registration: ${implementation}`, () => {
    let frames: FrameRequestCallback[];
    let commands: Array<() => void>;
    let listeners: Array<[string, EventListenerOrEventListenerObject]>;
    let nativeSlots: PublisherGptSlot[];
    let policy: boolean | undefined;
    let ts: TsjsApi;
    let tag: ReturnType<typeof createTag>;
    let snapshots: Array<Record<string, string | string[]>>;
    let nativeDisplay: ReturnType<typeof vi.fn>;
    let nativeRefresh: ReturnType<typeof vi.fn>;
    let nativeDefine: ReturnType<typeof vi.fn>;
    const originalPushState = history.pushState;
    const originalReplaceState = history.replaceState;
    const configured: AuctionSlot = {
      id: 'header',
      gam_unit_path: '/123/example/header',
      div_id: 'ssr-prefix-',
      formats: [
        [300, 250],
        [300, 600],
      ],
      initialization: 'publisher_registered',
      targeting: { route: 'example' },
    };

    function createTag() {
      const service = {
        getSlots: vi.fn(() => nativeSlots),
        refresh: vi.fn((slots: PublisherGptSlot[]) => {
          expect(ts.gptSlotHandoffInternal).toBe(true);
          expect(ts.adInitRefreshInProgress).toBe(true);
          slots.forEach((slot) => snapshots.push({ ...targetingOf(slot) }));
        }),
        enableSingleRequest: vi.fn(),
        disableInitialLoad: vi.fn(),
        addEventListener: vi.fn(),
      };
      nativeRefresh = service.refresh;
      nativeDisplay = vi.fn();
      nativeDefine = vi.fn();
      return {
        cmd: {
          push: (callback: () => void) => {
            commands.push(callback);
          },
        },
        pubads: () => service,
        getConfig: () => (policy === undefined ? {} : { disableInitialLoad: policy }),
        defineSlot: nativeDefine,
        enableServices: vi.fn(),
        display: nativeDisplay,
        destroySlots: vi.fn(),
      };
    }
    const targetings = new WeakMap<PublisherGptSlot, Record<string, string | string[]>>();
    function targetingOf(slot: PublisherGptSlot) {
      return targetings.get(slot)!;
    }
    function mount(id = 'final-outside-prefix', path = configured.gam_unit_path) {
      const element = document.createElement('div');
      element.id = id;
      document.body.append(element);
      const targeting: Record<string, string | string[]> = { publisher: 'keep' };
      const slot: PublisherGptSlot = {
        getSlotElementId: () => id,
        getAdUnitPath: () => path,
        setTargeting: vi.fn((key, value) => {
          targeting[key] = value;
          return slot;
        }),
        clearTargeting: vi.fn((key) => {
          if (key) delete targeting[key];
          return slot;
        }),
        getTargeting: (key) => {
          const value = targeting[key];
          return value === undefined ? [] : Array.isArray(value) ? value : [value];
        },
      };
      targetings.set(slot, targeting);
      nativeSlots.push(slot);
      return { element, slot };
    }
    function options(
      slot: PublisherGptSlot,
      eligibleFormats: ReadonlyArray<readonly [number, number]> = [[300, 250]]
    ) {
      return { slotId: 'header', gptSlot: slot, navigationGeneration: 0, eligibleFormats };
    }
    function flush() {
      for (let i = 0; i < 8; i++) {
        const queue = commands.splice(0);
        queue.forEach((callback) => callback());
        const currentFrames = frames.splice(0);
        currentFrames.forEach((callback) => callback(0));
      }
    }
    async function runtime() {
      const module = await import('../../src/integrations/gpt/index');
      module.installTsAdInit();
      module.installTsRenderBridge();
    }
    async function install() {
      if (implementation === 'runtime') await runtime();
      else new Function(readFileSync(GPT_BOOTSTRAP_PATH, 'utf8'))();
      flush();
    }
    async function settle() {
      if (implementation === 'waiting_handoff') await runtime();
      ts.scheduleInitialAdInit?.(ts.bids ?? {}, ts.adSlots ?? []);
      flush();
      if (implementation === 'requested_handoff') {
        await runtime();
        flush();
      }
    }
    beforeEach(async () => {
      vi.resetModules();
      frames = [];
      commands = [];
      listeners = [];
      nativeSlots = [];
      snapshots = [];
      policy = true;
      history.pushState = originalPushState;
      history.replaceState = originalReplaceState;
      originalReplaceState.call(history, {}, '', '/');
      ts = { adSlots: [{ ...configured }], bids: {} } as TsjsApi;
      window.tsjs = ts;
      tag = createTag();
      Object.assign(window, { googletag: tag });
      vi.spyOn(window, 'requestAnimationFrame').mockImplementation((callback) => {
        frames.push(callback);
        return frames.length;
      });
      const add = window.addEventListener.bind(window);
      vi.spyOn(window, 'addEventListener').mockImplementation((type, listener, opts) => {
        listeners.push([type, listener]);
        add(type, listener, opts);
      });
      Object.defineProperty(document, 'readyState', { configurable: true, value: 'complete' });
      await install();
    });
    afterEach(() => {
      for (const [type, listener] of listeners) {
        window.removeEventListener(type, listener);
        window.removeEventListener(type, listener, true);
      }
      document.body.innerHTML = '';
      delete (window as unknown as Record<string, unknown>).googletag;
      delete (window as unknown as Record<string, unknown>).tsjs;
      delete (document as unknown as Record<string, unknown>).readyState;
      delete (document as unknown as Record<string, unknown>).hidden;
      history.pushState = originalPushState;
      history.replaceState = originalReplaceState;
      originalReplaceState.call(history, {}, '', '/');
      vi.restoreAllMocks();
      vi.unstubAllGlobals();
    });

    it('installs version 1 without claiming head/default bids or load admission', () => {
      expect(ts.publisherSlotRegistrationVersion).toBe(1);
      const { slot } = mount();
      const handle = ts.registerPublisherSlot!(options(slot));
      ts.scheduleInitialAdInit?.();
      window.dispatchEvent(new Event('load'));
      flush();
      expect(handle.getStatus().status).toBe('waiting_payload');
      expect(nativeDefine).not.toHaveBeenCalled();
      expect(nativeDisplay).not.toHaveBeenCalled();
      expect(nativeRefresh).not.toHaveBeenCalled();
    });
    it('joins registration first with current payload and delivers the exact slot once', async () => {
      const { slot } = mount();
      const handle = ts.registerPublisherSlot!(options(slot));
      expect(handle.getStatus().status).toBe('waiting_payload');
      await settle();
      expect(handle.getStatus().status).toBe('requested');
      expect(nativeDisplay).toHaveBeenCalledExactlyOnceWith('final-outside-prefix');
      expect(nativeRefresh).toHaveBeenCalledExactlyOnceWith([slot]);
      expect(snapshots).toEqual([{ publisher: 'keep', route: 'example', ts_initial: '1' }]);
      expect(ts.divToSlotId?.['final-outside-prefix']).toBe('header');
      expect(ts.prevGptSlots ?? []).not.toContain(slot);
      expect(nativeDefine).not.toHaveBeenCalled();
      expect(tag.enableServices).not.toHaveBeenCalled();
      expect(tag.pubads().enableSingleRequest).not.toHaveBeenCalled();
      expect(ts.gptSlotHandoffInternal).not.toBe(true);
      expect(ts.adInitRefreshInProgress).not.toBe(true);
    });
    it('joins payload first and keeps equivalent repeats idempotent', async () => {
      await settle();
      const { slot } = mount();
      const handle = ts.registerPublisherSlot!(
        options(slot, [
          [300, 250],
          [300, 600],
        ])
      );
      const repeated = ts.registerPublisherSlot!(
        options(slot, [
          [300, 600],
          [300, 250],
          [300, 250],
        ])
      );
      flush();
      expect(handle.getStatus()).toEqual(repeated.getStatus());
      expect(nativeRefresh).toHaveBeenCalledTimes(1);
      await runtime();
      flush();
      expect(nativeRefresh).toHaveBeenCalledTimes(1);
    });
    it('explicit empty admission is inactive without defining or delivering', async () => {
      const { slot } = mount();
      const handle = ts.registerPublisherSlot!(options(slot));
      ts.adSlots = [];
      await settle();
      expect(handle.getStatus().status).toBe('inactive');
      expect(nativeDisplay).not.toHaveBeenCalled();
    });
    it.each([false, undefined])('rejects enabled or unknown initial loading: %s', (value) => {
      policy = value;
      delete ts.gptInitialLoadDisabled;
      const { slot } = mount();
      const handle = ts.registerPublisherSlot!(options(slot));
      expect(handle.getStatus().status).toBe('rejected');
      expect(nativeDisplay).not.toHaveBeenCalled();
    });
    it('cancels only its own pending token and adopts the replacement eligibility', async () => {
      const { slot } = mount();
      const old = ts.registerPublisherSlot!(options(slot));
      old.cancel();
      const replacement = ts.registerPublisherSlot!(options(slot, [[300, 600]]));
      old.cancel();
      expect(old.getStatus().status).toBe('cancelled');
      await settle();
      expect(replacement.getStatus().status).toBe('requested');
      expect(nativeRefresh).toHaveBeenCalledTimes(1);
    });
    it.each([[[0, 250]], [[300, -1]], [[300.5, 250]], [[Infinity, 250]], [[NaN, 250]], [[300]]])(
      'rejects malformed finite formats %j',
      (formats) => {
        const { slot } = mount();
        const handle = ts.registerPublisherSlot!({
          ...options(slot),
          eligibleFormats: formats as ReadonlyArray<readonly [number, number]>,
        });
        expect(handle.getStatus()).toEqual({ status: 'rejected', reason: 'invalid_arguments' });
        expect(nativeDisplay).not.toHaveBeenCalled();
      }
    );
    it('rejects duplicate IDs, non-live native objects, stale generations and conflicting snapshots', () => {
      const { slot } = mount();
      const duplicate = document.createElement('div');
      duplicate.id = slot.getSlotElementId();
      document.body.append(duplicate);
      expect(ts.registerPublisherSlot!(options(slot)).getStatus()).toEqual({
        status: 'rejected',
        reason: 'duplicate_dom_id',
      });
      duplicate.remove();
      nativeSlots = [];
      expect(ts.registerPublisherSlot!(options(slot)).getStatus()).toEqual({
        status: 'rejected',
        reason: 'native_slot_missing',
      });
      nativeSlots.push(slot);
      expect(
        ts.registerPublisherSlot!({ ...options(slot), navigationGeneration: 1 }).getStatus()
      ).toEqual({ status: 'rejected', reason: 'stale_generation' });
      ts.registerPublisherSlot!(options(slot));
      expect(ts.registerPublisherSlot!(options(slot, [[300, 600]])).getStatus()).toEqual({
        status: 'rejected',
        reason: 'conflicting_registration',
      });
      expect(nativeDisplay).not.toHaveBeenCalled();
    });
    it.each(['element', 'id', 'native', 'path', 'policy', 'duplicate'] as const)(
      'rechecks %s immediately before mutation',
      async (change) => {
        const { slot, element } = mount();
        const handle = ts.registerPublisherSlot!(options(slot));
        if (change === 'element') {
          element.remove();
          const next = document.createElement('div');
          next.id = element.id;
          document.body.append(next);
        }
        if (change === 'id') element.id = 'changed';
        if (change === 'native') nativeSlots = [];
        if (change === 'path') slot.getAdUnitPath = () => '/123/example/other';
        if (change === 'policy') policy = false;
        if (change === 'duplicate') {
          const next = document.createElement('div');
          next.id = element.id;
          document.body.append(next);
        }
        await settle();
        expect(['cancelled', 'rejected']).toContain(handle.getStatus().status);
        expect(nativeDisplay).not.toHaveBeenCalled();
        expect(slot.setTargeting).not.toHaveBeenCalled();
      }
    );
    it('waits hidden, replaces eligibility synchronously and rejects all old callbacks', async () => {
      let hidden = true;
      Object.defineProperty(document, 'hidden', { configurable: true, get: () => hidden });
      ts.bids = {
        header: { hb_adid: 'tall', hb_pb: '2.00', w: 300, h: 600, adm: '<b>example</b>' },
      };
      const { slot } = mount();
      const old = ts.registerPublisherSlot!(options(slot, [[300, 600]]));
      await settle();
      expect(old.getStatus().status).toBe('waiting_delivery');
      expect(nativeDisplay).not.toHaveBeenCalled();
      old.cancel();
      const replacement = ts.registerPublisherSlot!(options(slot, [[300, 250]]));
      hidden = false;
      document.dispatchEvent(new Event('visibilitychange'));
      old.cancel();
      flush();
      expect(replacement.getStatus().status).toBe('requested');
      expect(nativeRefresh).toHaveBeenCalledTimes(1);
      expect(targetingOf(slot)).not.toHaveProperty('hb_adid');
      replacement.cancel();
      expect(ts.registerPublisherSlot!(options(slot, [[300, 600]])).getStatus().status).toBe(
        'rejected'
      );
      flush();
      expect(nativeRefresh).toHaveBeenCalledTimes(1);
    });
    it('does not revive a consumed logical placement when its physical element is replaced', async () => {
      const { slot, element } = mount();
      const handle = ts.registerPublisherSlot!(options(slot));
      await settle();
      handle.cancel();
      element.remove();
      nativeSlots = [];
      const replacement = mount();
      expect(ts.registerPublisherSlot!(options(replacement.slot)).getStatus().status).toBe(
        'rejected'
      );
      flush();
      expect(nativeRefresh).toHaveBeenCalledTimes(1);
    });
    it.each(['display', 'refresh', 'targeting'] as const)(
      'consumes before %s exceptions, with no retry',
      async (source) => {
        const { slot } = mount();
        if (source === 'display')
          nativeDisplay.mockImplementation(() => {
            throw new Error('native display failed');
          });
        if (source === 'refresh')
          nativeRefresh.mockImplementation(() => {
            throw new Error('native refresh failed');
          });
        if (source === 'targeting')
          vi.mocked(slot.setTargeting).mockImplementation(() => {
            throw new Error('targeting failed');
          });
        const handle = ts.registerPublisherSlot!(options(slot));
        await settle();
        expect(handle.getStatus().status).toBe('failed_after_dispatch');
        await runtime();
        ts.publisherSlotRegistrationDispatch?.();
        flush();
        expect(handle.getStatus().status).toBe('failed_after_dispatch');
        expect(nativeDisplay.mock.calls.length).toBeLessThanOrEqual(1);
        expect(nativeRefresh.mock.calls.length).toBeLessThanOrEqual(1);
        expect(ts.gptSlotHandoffInternal).not.toBe(true);
        expect(ts.adInitRefreshInProgress).not.toBe(true);
      }
    );
    it('leaves earlier publisher ownership and targeting intact', async () => {
      const { slot, element } = mount();
      ts.firstImpression = {
        generation: 0,
        nextToken: 0,
        fallbackSlots: {},
        slots: {
          [element.id]: {
            generation: 0,
            slotElementId: element.id,
            element,
            owner: 'publisher',
            phase: 'requested',
            expiresAt: Infinity,
            publisherAuctions: {},
          },
        },
      };
      const handle = ts.registerPublisherSlot!(options(slot));
      await settle();
      expect(handle.getStatus()).toEqual({ status: 'publisher_owned', reason: 'prior_claim' });
      expect(slot.setTargeting).not.toHaveBeenCalled();
      expect(nativeDisplay).not.toHaveBeenCalled();
    });
    it.each([
      [
        'fitting',
        { hb_adid: 'example-ad', hb_pb: '1.20', w: 300, h: 250, adm: '<b>example</b>' },
        [[300, 250]],
      ],
      [
        'incompatible',
        { hb_adid: 'example-ad', hb_pb: '1.20', w: 300, h: 600, adm: '<b>example</b>' },
        [[300, 250]],
      ],
      ['unknown', { hb_adid: 'example-ad', hb_pb: '1.20', adm: '<b>example</b>' }, [[300, 250]]],
      [
        'outside-config',
        { hb_adid: 'example-ad', hb_pb: '1.20', w: 728, h: 90, adm: '<b>example</b>' },
        [[728, 90]],
      ],
      [
        'empty',
        { hb_adid: 'example-ad', hb_pb: '1.20', w: 300, h: 250, adm: '<b>example</b>' },
        [],
      ],
      ['no-bid', {}, [[300, 250]]],
    ] as const)(
      'selects only renderer-backed eligible candidates: %s',
      async (name, bid, formats) => {
        ts.bids = { header: bid as AuctionBidData };
        const { slot } = mount();
        ts.registerPublisherSlot!(options(slot, formats));
        await settle();
        const capableAtDispatch =
          implementation === 'runtime' || implementation === 'waiting_handoff';
        expect(targetingOf(slot).hb_adid).toBe(
          name === 'fitting' && capableAtDispatch ? 'example-ad' : undefined
        );
        expect(nativeRefresh).toHaveBeenCalledTimes(1);
      }
    );
    it.each(['automatic_first', 'registered_first'])(
      'keeps registered mappings through %s and empty passes',
      async (order) => {
        const automatic = mount('automatic-final');
        const autoConfig = {
          ...configured,
          id: 'automatic',
          div_id: 'automatic-final',
          initialization: 'auto' as const,
        };
        ts.adSlots!.push(autoConfig);
        if (order === 'automatic_first') await settle();
        const { slot } = mount();
        ts.registerPublisherSlot!(options(slot));
        if (order === 'registered_first') await settle();
        else flush();
        expect(ts.divToSlotId?.['final-outside-prefix']).toBe('header');
        expect(ts.prevSlotTargetingKeys?.['final-outside-prefix']).toContain('route');
        expect(ts.prevGptSlots ?? []).not.toContain(slot);
        expect(ts.divToSlotId?.[automatic.element.id]).toBe('automatic');
        // No automatic placements are left, but the current registered record still owns its entry.
        ts.adSlots = [{ ...configured }];
        ts.adInit!();
        flush();
        expect(ts.divToSlotId?.['final-outside-prefix']).toBe('header');
      }
    );
    it('keeps two logical registrations sharing a GAM path independent', async () => {
      ts.adSlots!.push({ ...configured, id: 'footer' });
      const header = mount();
      const footer = mount('final-footer');
      const first = ts.registerPublisherSlot!(options(header.slot));
      const second = ts.registerPublisherSlot!({ ...options(footer.slot), slotId: 'footer' });
      await settle();
      expect(first.getStatus().status).toBe('requested');
      expect(second.getStatus().status).toBe('requested');
      expect(nativeRefresh).toHaveBeenCalledTimes(2);
      expect(nativeRefresh.mock.calls.map(([slots]) => slots)).toEqual([
        [header.slot],
        [footer.slot],
      ]);
      expect(ts.divToSlotId?.[header.element.id]).toBe('header');
      expect(ts.divToSlotId?.[footer.element.id]).toBe('footer');
    });
    it('does not clear or render into a replacement native/element that reuses a consumed ID', async () => {
      ts.bids = {
        header: {
          hb_adid: 'old-selected',
          hb_pb: '1.20',
          w: 300,
          h: 250,
          adm: '<b>old creative</b>',
        },
      };
      const old = mount();
      ts.registerPublisherSlot!(options(old.slot));
      await settle();
      old.element.remove();
      nativeSlots = [];
      const next = mount();
      next.slot.setTargeting('hb_adid', 'publisher-new');
      ts.adInit!();
      flush();
      expect(targetingOf(next.slot).hb_adid).toBe('publisher-new');
      expect(next.slot.clearTargeting).not.toHaveBeenCalled();
      if (implementation === 'runtime' || implementation === 'waiting_handoff') {
        const frame = document.createElement('iframe');
        next.element.append(frame);
        const postMessage = vi.fn();
        window.dispatchEvent(
          new MessageEvent('message', {
            data: { message: 'Prebid Request', adId: 'old-selected' },
            source: frame.contentWindow,
            ports: [{ postMessage } as unknown as MessagePort],
          })
        );
        expect(postMessage).not.toHaveBeenCalled();
      }
      await runtime();
      flush();
      vi.stubGlobal(
        'fetch',
        vi.fn(() => new Promise<Response>(() => {}))
      );
      history.pushState({}, '', '/replacement-cleanup-example');
      expect(targetingOf(next.slot).hb_adid).toBe('publisher-new');
      expect(next.slot.clearTargeting).not.toHaveBeenCalled();
    });
    it('uses prototype-safe logical and native ID dictionaries', async () => {
      ts.adSlots = [{ ...configured, id: '__proto__' }];
      const { slot } = mount('__proto__');
      const handle = ts.registerPublisherSlot!({ ...options(slot), slotId: '__proto__' });
      await settle();
      expect(handle.getStatus().status).toBe('requested');
      expect(Object.hasOwn(ts.divToSlotId!, '__proto__')).toBe(true);
      expect(ts.divToSlotId!['__proto__']).toBe('__proto__');
      expect(ts.firstImpression?.slots['__proto__']?.owner).toBe('trusted_server');
      expect(Object.getPrototypeOf(ts.publisherSlotRegistrationState!.records)).toBeNull();
    });
    it('drops stale queued payload/frame/command work and old-generation handles', async () => {
      const { slot } = mount();
      const old = ts.registerPublisherSlot!(options(slot));
      ts.scheduleInitialAdInit!({}, ts.adSlots);
      const oldFrames = frames.splice(0);
      ts.navGeneration = 1;
      ts.publisherSlotRegistrationDispatch!();
      const current = ts.registerPublisherSlot!({ ...options(slot), navigationGeneration: 1 });
      old.cancel();
      oldFrames.forEach((callback) => callback(0));
      flush();
      expect(current.getStatus().status).toBe('waiting_payload');
      expect(nativeDisplay).not.toHaveBeenCalled();
      ts.scheduleInitialAdInit!({}, ts.adSlots);
      flush();
      expect(current.getStatus().status).toBe('waiting_payload');
    });
    it('never recovers an excluded bid through the bridge or mixed-page debug injection', async () => {
      ts.bids = {
        header: {
          hb_adid: 'excluded-ad',
          hb_pb: '1.20',
          w: 300,
          h: 600,
          adm: '<b>excluded creative</b>',
          debug_bid: {},
        },
      };
      const automatic = mount('automatic-final');
      ts.adSlots!.push({
        ...configured,
        id: 'automatic',
        div_id: automatic.element.id,
        initialization: 'auto',
      });
      const { slot, element } = mount();
      ts.registerPublisherSlot!(options(slot));
      await settle();
      await runtime();
      flush();
      const frame = document.createElement('iframe');
      element.append(frame);
      const postMessage = vi.fn();
      window.dispatchEvent(
        new MessageEvent('message', {
          data: JSON.stringify({ message: 'Prebid Request', adId: 'excluded-ad' }),
          source: frame.contentWindow,
          ports: [{ postMessage } as unknown as MessagePort],
        })
      );
      for (const [name, callback] of tag.pubads().addEventListener.mock.calls as unknown as Array<
        [string, (event: { slot: PublisherGptSlot }) => void]
      >) {
        if (name === 'slotRenderEnded') callback({ slot });
      }
      expect(postMessage).not.toHaveBeenCalled();
      expect(element.innerHTML).not.toContain('excluded creative');
      expect(nativeRefresh).toHaveBeenCalledTimes(2);
      expect(targetingOf(slot)).not.toHaveProperty('hb_adid');
    });
    it('does not retroactively select a fitting bid after ordinary bootstrap dispatch', async () => {
      if (implementation === 'runtime' || implementation === 'waiting_handoff') return;
      ts.bids = {
        header: {
          hb_adid: 'fitting-but-no-bridge',
          hb_pb: '1.20',
          w: 300,
          h: 250,
          adm: '<b>example creative</b>',
        },
      };
      const { slot, element } = mount();
      ts.registerPublisherSlot!(options(slot));
      await settle();
      expect(targetingOf(slot)).not.toHaveProperty('hb_adid');
      await runtime();
      flush();
      const frame = document.createElement('iframe');
      element.append(frame);
      const postMessage = vi.fn();
      window.dispatchEvent(
        new MessageEvent('message', {
          data: { message: 'Prebid Request', adId: 'fitting-but-no-bridge' },
          source: frame.contentWindow,
          ports: [{ postMessage } as unknown as MessagePort],
        })
      );
      expect(postMessage).not.toHaveBeenCalled();
      expect(nativeRefresh).toHaveBeenCalledTimes(1);
    });
    it('settles SPA registrations before missing automatic DOM and snapshots exact old cleanup identities', async () => {
      const { slot: oldNative } = mount();
      ts.registerPublisherSlot!(options(oldNative));
      await settle();
      await runtime();
      flush();
      let resolveFetch: (response: Response) => void = () => {};
      vi.stubGlobal(
        'fetch',
        vi.fn(
          () =>
            new Promise<Response>((resolve) => {
              resolveFetch = resolve;
            })
        )
      );
      history.pushState({}, '', '/next-example');
      expect(targetingOf(oldNative)).toEqual({ publisher: 'keep' });
      const { slot: nextNative } = mount('next-outside-prefix');
      const next = ts.registerPublisherSlot!({ ...options(nextNative), navigationGeneration: 1 });
      resolveFetch(
        new Response(
          JSON.stringify({
            slots: [
              configured,
              {
                ...configured,
                id: 'automatic-missing',
                div_id: 'never-mounted',
                initialization: 'auto',
              },
            ],
            bids: {},
          }),
          { headers: { 'Content-Type': 'application/json' } }
        )
      );
      // Fetch/JSON microtasks settle without advancing the automatic DOM timeout.
      await new Promise((resolve) => setTimeout(resolve, 0));
      flush();
      expect(next.getStatus().status).toBe('requested');
      expect(ts.divToSlotId?.['next-outside-prefix']).toBe('header');
      expect(nativeRefresh).toHaveBeenCalledTimes(2);
      // Abort the intentionally unresolved automatic DOM wait.
      history.pushState({}, '', '/after-example');
    });
    it('accepts known legacy disabled-load observation without changing publisher policy', async () => {
      Reflect.deleteProperty(tag, 'getConfig');
      delete ts.gptInitialLoadDisabled;
      tag.pubads().disableInitialLoad();
      const { slot } = mount();
      const handle = ts.registerPublisherSlot!(options(slot));
      await settle();
      expect(handle.getStatus().status).toBe('requested');
      expect(tag.pubads().disableInitialLoad).not.toBeUndefined();
      expect(tag.pubads().enableSingleRequest).not.toHaveBeenCalled();
      expect(tag.enableServices).not.toHaveBeenCalled();
    });
    it('preserves omitted seam slots while accepting explicit bids', () => {
      ts.scheduleInitialAdInit!({});
      const { slot } = mount();
      const handle = ts.registerPublisherSlot!(options(slot));
      flush();
      expect(handle.getStatus().status).toBe('requested');
    });
    it('never admits a failed SPA response or a stale SSR callback', async () => {
      await runtime();
      vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('example fetch failure')));
      history.pushState({}, '', '/failed-example');
      const { slot } = mount();
      const handle = ts.registerPublisherSlot!({ ...options(slot), navigationGeneration: 1 });
      await new Promise((resolve) => setTimeout(resolve, 0));
      flush();
      ts.scheduleInitialAdInit!({}, [configured]);
      flush();
      expect(handle.getStatus().status).toBe('waiting_payload');
      expect(nativeDisplay).not.toHaveBeenCalled();
    });
    it('installs the real bridge once, without resetting pending registrations', async () => {
      expect(ts.tsRenderBridgeVersion).toBe(implementation === 'runtime' ? 1 : undefined);
      const { slot } = mount();
      const handle = ts.registerPublisherSlot!(options(slot));
      await runtime();
      const count = listeners.filter(([type]) => type === 'message').length;
      await runtime();
      await runtime();
      expect(listeners.filter(([type]) => type === 'message')).toHaveLength(count);
      expect(ts.tsRenderBridgeVersion).toBe(1);
      expect(handle.getStatus().status).toBe('waiting_payload');
      await settle();
      expect(nativeRefresh).toHaveBeenCalledTimes(1);
    });
    it('consumes before a reentrant native display tries to register again', async () => {
      const { slot } = mount();
      let nestedStatus: string | undefined;
      nativeDisplay.mockImplementation(() => {
        nestedStatus = ts.registerPublisherSlot!(options(slot)).getStatus().status;
      });
      const handle = ts.registerPublisherSlot!(options(slot));
      await settle();
      expect(nestedStatus).toBe('dispatching');
      expect(handle.getStatus().status).toBe('requested');
      expect(nativeDisplay).toHaveBeenCalledTimes(1);
      expect(nativeRefresh).toHaveBeenCalledTimes(1);
    });
    it('ignores an old queued GPT callback after cancellation replaces the token', async () => {
      const { slot } = mount();
      ts.scheduleInitialAdInit!({}, ts.adSlots);
      const old = ts.registerPublisherSlot!(options(slot));
      frames.splice(0).forEach((callback) => callback(0));
      const staleCommands = commands.splice(0);
      old.cancel();
      const current = ts.registerPublisherSlot!(options(slot, [[300, 600]]));
      staleCommands.forEach((callback) => callback());
      old.cancel();
      flush();
      expect(current.getStatus().status).toBe('requested');
      expect(nativeRefresh).toHaveBeenCalledTimes(1);
    });
    it.each([false, true])(
      'queued navigation cleanup uses old native objects, not reused IDs (old live=%s)',
      async (keepOld) => {
        const { slot: oldNative, element } = mount();
        ts.registerPublisherSlot!(options(oldNative));
        await settle();
        await runtime();
        flush();
        const service = tag.pubads;
        Reflect.deleteProperty(tag, 'pubads');
        vi.stubGlobal(
          'fetch',
          vi.fn(() => new Promise<Response>(() => {}))
        );
        history.pushState({}, '', '/queued-cleanup-example');
        element.remove();
        nativeSlots = keepOld ? [oldNative] : [];
        const { slot: nextNative } = mount();
        nextNative.setTargeting('hb_adid', 'new-publisher-ad');
        const previousClearCalls = vi.mocked(oldNative.clearTargeting).mock.calls.length;
        tag.pubads = service;
        ts.divToSlotId = { 'new-route-native': 'next' };
        ts.prevSlotTargetingKeys = { 'new-route-native': ['new_route_key'] };
        commands.splice(0).forEach((callback) => callback());
        expect(targetingOf(nextNative).hb_adid).toBe('new-publisher-ad');
        expect(nextNative.clearTargeting).not.toHaveBeenCalled();
        expect(vi.mocked(oldNative.clearTargeting).mock.calls.length > previousClearCalls).toBe(
          keepOld
        );
        expect(ts.divToSlotId).toEqual({ 'new-route-native': 'next' });
        expect(ts.prevSlotTargetingKeys).toEqual({ 'new-route-native': ['new_route_key'] });
      }
    );
    it.each(['pushState', 'replaceState'] as const)(
      'real %s invalidates admitted document payload before a fresh native registration',
      async (method) => {
        vi.stubGlobal(
          'fetch',
          vi.fn(() => new Promise<Response>(() => {}))
        );
        ts.scheduleInitialAdInit!({}, [{ ...configured, targeting: { section: 'admitted-a' } }]);
        flush();
        history[method]({}, '', '/unadmitted-b');
        const { slot } = mount();
        const handle = ts.registerPublisherSlot!({
          ...options(slot),
          navigationGeneration: ts.navGeneration ?? 0,
        });
        flush();
        expect(nativeDisplay).not.toHaveBeenCalled();
        expect(nativeRefresh).not.toHaveBeenCalled();
        expect(handle.getStatus().status).toBe('waiting_payload');
        expect(ts.navGeneration).toBe(1);
        expect(targetingOf(slot)).not.toHaveProperty('section');
      }
    );
    it('real navigation cancels a pending admitted token before stale frame/command callbacks execute', () => {
      vi.stubGlobal(
        'fetch',
        vi.fn(() => new Promise<Response>(() => {}))
      );
      const { slot } = mount();
      const old = ts.registerPublisherSlot!(options(slot));
      ts.scheduleInitialAdInit!({}, ts.adSlots);
      // Let the first foreground frame queue native commands, but do not drain them yet.
      frames.splice(0).forEach((callback) => callback(0));
      const staleCommands = commands.splice(0);
      history.pushState({}, '', '/pending-navigation-b');
      staleCommands.forEach((callback) => callback());
      flush();
      expect(nativeDisplay).not.toHaveBeenCalled();
      expect(nativeRefresh).not.toHaveBeenCalled();
      expect(old.getStatus()).toEqual({ status: 'cancelled', reason: 'navigation' });
    });
    it('real navigation before the SSR seam cannot admit that late document payload', () => {
      vi.stubGlobal(
        'fetch',
        vi.fn(() => new Promise<Response>(() => {}))
      );
      history.pushState({}, '', '/before-ssr-b');
      ts.scheduleInitialAdInit!({}, [configured]);
      const { slot } = mount();
      const current = ts.registerPublisherSlot!({
        ...options(slot),
        navigationGeneration: ts.navGeneration ?? 0,
      });
      window.dispatchEvent(new Event('load'));
      flush();
      expect(current.getStatus().status).toBe('waiting_payload');
      expect(nativeDisplay).not.toHaveBeenCalled();
      expect(ts.publisherSlotRegistrationState?.payload).toBeUndefined();
    });
    it('real A-to-B-to-A navigation advances generation and never re-adopts document-A admission', () => {
      vi.stubGlobal(
        'fetch',
        vi.fn(() => new Promise<Response>(() => {}))
      );
      ts.scheduleInitialAdInit!({}, [configured]);
      flush();
      history.pushState({}, '', '/round-trip-b');
      history.replaceState({}, '', '/round-trip-b?view=example#placement');
      expect(ts.navGeneration).toBe(1);
      history.pushState({}, '', '/');
      const { slot } = mount();
      const current = ts.registerPublisherSlot!({
        ...options(slot),
        navigationGeneration: ts.navGeneration ?? 0,
      });
      flush();
      expect(ts.navGeneration).toBe(2);
      expect(current.getStatus().status).toBe('waiting_payload');
      expect(nativeDisplay).not.toHaveBeenCalled();
    });
    it('real history traversal invalidates admission through popstate, not manual counter writes', async () => {
      vi.stubGlobal(
        'fetch',
        vi.fn(() => new Promise<Response>(() => {}))
      );
      ts.scheduleInitialAdInit!({}, [configured]);
      flush();
      history.pushState({}, '', '/history-traversal-b');
      const traversed = new Promise<void>((resolve) =>
        window.addEventListener('popstate', () => resolve(), { once: true })
      );
      history.back();
      await traversed;
      const { slot } = mount();
      const current = ts.registerPublisherSlot!({
        ...options(slot),
        navigationGeneration: ts.navGeneration ?? 0,
      });
      flush();
      expect(location.pathname).toBe('/');
      expect(ts.navGeneration).toBe(2);
      expect(current.getStatus().status).toBe('waiting_payload');
      expect(nativeDisplay).not.toHaveBeenCalled();
    });
    it('real navigation retires consumed registered targeting and cannot deliver another old-payload request', () => {
      vi.stubGlobal(
        'fetch',
        vi.fn(() => new Promise<Response>(() => {}))
      );
      const old = mount('initial-native-a');
      const oldHandle = ts.registerPublisherSlot!(options(old.slot));
      ts.scheduleInitialAdInit!({}, [configured]);
      flush();
      expect(nativeRefresh).toHaveBeenCalledTimes(1);
      const alias = {
        gamUnitPath: configured.gam_unit_path,
        formats: configured.formats,
        divIdPrefix: 'alias-',
        slotElementId: 'alias-a',
        publisherClaimed: false,
        suppressPublisherDisplay: false,
        suppressPublisherRefresh: false,
      };
      ts.gptSlotHandoffs = { unclaimed: alias, claimed: { ...alias, publisherClaimed: true } };
      history.pushState({}, '', '/consumed-navigation-b');
      expect(ts.firstImpression).toBeUndefined();
      expect(ts.gptSlotHandoffs.unclaimed).toBeUndefined();
      expect(ts.gptSlotHandoffs.claimed).toBeDefined();
      expect(targetingOf(old.slot)).toEqual({ publisher: 'keep' });
      expect(oldHandle.getStatus()).toEqual({ status: 'cancelled', reason: 'navigation' });
      const current = mount('new-native-b');
      const currentHandle = ts.registerPublisherSlot!({
        ...options(current.slot),
        navigationGeneration: ts.navGeneration ?? 0,
      });
      flush();
      expect(currentHandle.getStatus().status).toBe('waiting_payload');
      expect(nativeRefresh).toHaveBeenCalledTimes(1);
      expect(ts.divToSlotId).toEqual({});
      expect(ts.prevSlotTargetingKeys).toEqual({});
    });
    it.each([false, true])(
      'late runtime resumes only the current navigation payload without a second generation increment (return to A=%s)',
      async (returnToA) => {
        const responses: Array<(response: Response) => void> = [];
        const fetch = vi.fn(() => new Promise<Response>((resolve) => responses.push(resolve)));
        vi.stubGlobal('fetch', fetch);
        ts.scheduleInitialAdInit!({}, [{ ...configured, targeting: { section: 'admitted-a' } }]);
        flush();
        history.pushState({}, '', '/late-runtime-b');
        if (returnToA) history.pushState({}, '', '/');
        const expectedGeneration = returnToA ? 2 : 1;
        const expectedFetches = implementation === 'runtime' && returnToA ? 2 : 1;
        const currentSection = returnToA ? 'admitted-return-a' : 'admitted-b';
        const { slot } = mount();
        const current = ts.registerPublisherSlot!({
          ...options(slot),
          navigationGeneration: ts.navGeneration ?? 0,
        });
        await runtime();
        flush();
        expect(fetch).toHaveBeenCalledTimes(expectedFetches);
        expect(ts.navGeneration).toBe(expectedGeneration);
        expect(current.getStatus().status).toBe('waiting_payload');
        expect(nativeDisplay).not.toHaveBeenCalled();
        responses[expectedFetches - 1](
          new Response(
            JSON.stringify({
              slots: [{ ...configured, targeting: { section: currentSection } }],
              bids: {},
            }),
            { headers: { 'Content-Type': 'application/json' } }
          )
        );
        await new Promise((resolve) => setTimeout(resolve, 0));
        flush();
        expect(current.getStatus().status).toBe('requested');
        expect(nativeRefresh).toHaveBeenCalledTimes(1);
        expect(snapshots[0].section).toBe(currentSection);
        expect(ts.navGeneration).toBe(expectedGeneration);
        history.pushState({}, '', '/runtime-owned-c');
        expect(ts.navGeneration).toBe(expectedGeneration + 1);
        expect(fetch).toHaveBeenCalledTimes(expectedFetches + 1);
      }
    );
    it('same-path query/hash updates and throwing native history calls do not allocate a generation', () => {
      vi.stubGlobal(
        'fetch',
        vi.fn(() => new Promise<Response>(() => {}))
      );
      ts.scheduleInitialAdInit!({}, [configured]);
      history.pushState({}, '', '/?view=example#placement');
      history.replaceState({}, '', '/?view=other#placement');
      expect(() => history.pushState({}, '', 'https://outside.example.com/')).toThrow();
      expect(ts.navGeneration ?? 0).toBe(0);
      const { slot } = mount();
      const handle = ts.registerPublisherSlot!(options(slot));
      flush();
      expect(handle.getStatus().status).toBe('requested');
      expect(nativeRefresh).toHaveBeenCalledTimes(1);
    });
    it('takeover preserves a framework history wrapper and one observer/counter/fetch owner', async () => {
      const fetch = vi.fn(() => new Promise<Response>(() => {}));
      vi.stubGlobal('fetch', fetch);
      const observed = history.pushState;
      const frameworkWrapper = vi.fn(function (
        this: History,
        data: unknown,
        unused: string,
        url?: string | URL | null
      ) {
        observed.call(this, data, unused, url);
      });
      history.pushState = frameworkWrapper;
      await runtime();
      await runtime();
      expect(history.pushState).toBe(frameworkWrapper);
      const before = listeners.filter(([type]) => type === 'popstate').length;
      expect(before).toBe(1);
      history.pushState({}, '', '/framework-owned-b');
      expect(frameworkWrapper).toHaveBeenCalledTimes(1);
      expect(ts.navGeneration).toBe(1);
      expect(fetch).toHaveBeenCalledTimes(1);
      await runtime();
      expect(listeners.filter(([type]) => type === 'popstate')).toHaveLength(before);
    });
    it('no-navigation runtime installation does not start a catch-up auction or repeat delivery', async () => {
      const fetch = vi.fn(() => new Promise<Response>(() => {}));
      vi.stubGlobal('fetch', fetch);
      const { slot } = mount();
      const handle = ts.registerPublisherSlot!(options(slot));
      ts.scheduleInitialAdInit!({}, [configured]);
      flush();
      await runtime();
      await runtime();
      flush();
      expect(handle.getStatus().status).toBe('requested');
      expect(nativeRefresh).toHaveBeenCalledTimes(1);
      expect(fetch).not.toHaveBeenCalled();
    });
    it.each([false, true])(
      'catch-up empty success is inactive; failure stays unadmitted and permits same-path retry (failure=%s)',
      async (failure) => {
        const responses: Array<(response: Response) => void> = [];
        const fetch = vi.fn(() => new Promise<Response>((resolve) => responses.push(resolve)));
        vi.stubGlobal('fetch', fetch);
        ts.scheduleInitialAdInit!({}, [configured]);
        flush();
        history.pushState({}, '', '/catch-up-result-b');
        const { slot } = mount();
        const handle = ts.registerPublisherSlot!({
          ...options(slot),
          navigationGeneration: ts.navGeneration ?? 0,
        });
        const record = ts.publisherSlotRegistrationState?.records.header;
        const token = record?.token;
        await runtime();
        flush();
        expect(fetch).toHaveBeenCalledTimes(1);
        expect(ts.navGeneration).toBe(1);
        expect(ts.publisherSlotRegistrationState?.records.header).toBe(record);
        expect(record?.token).toBe(token);
        responses[0](
          new Response(JSON.stringify({ slots: [], bids: {} }), {
            status: failure ? 503 : 200,
            headers: { 'Content-Type': 'application/json' },
          })
        );
        await new Promise((resolve) => setTimeout(resolve, 0));
        flush();
        expect(handle.getStatus().status).toBe(failure ? 'waiting_payload' : 'inactive');
        expect(nativeDisplay).not.toHaveBeenCalled();
        if (!failure) return;
        expect(ts.publisherSlotRegistrationState?.payload).toBeUndefined();
        history.replaceState({}, '', '/catch-up-result-b?retry=1#placement');
        expect(fetch).toHaveBeenCalledTimes(2);
        const retryGeneration = implementation === 'runtime' ? 2 : 1;
        expect(ts.navGeneration).toBe(retryGeneration);
        const retry =
          implementation === 'runtime'
            ? ts.registerPublisherSlot!({ ...options(slot), navigationGeneration: retryGeneration })
            : handle;
        if (implementation !== 'runtime') {
          expect(ts.publisherSlotRegistrationState?.records.header).toBe(record);
          expect(record?.token).toBe(token);
          expect(record?.gptSlot).toBe(slot);
          expect(record?.element).toBe(document.getElementById(slot.getSlotElementId()));
          expect(handle.getStatus().status).toBe('waiting_payload');
        }
        // An already-running retry is not another retry or navigation.
        history.replaceState({}, '', '/catch-up-result-b?retry=2#other-placement');
        expect(fetch).toHaveBeenCalledTimes(2);
        expect(ts.navGeneration).toBe(retryGeneration);
        expect(nativeDisplay).not.toHaveBeenCalled();
        expect(slot.setTargeting).not.toHaveBeenCalled();
        responses[1](
          new Response(JSON.stringify({ slots: [configured], bids: {} }), {
            headers: { 'Content-Type': 'application/json' },
          })
        );
        await new Promise((resolve) => setTimeout(resolve, 0));
        flush();
        expect(retry.getStatus().status).toBe('requested');
        expect(nativeRefresh).toHaveBeenCalledTimes(1);
        history.replaceState({}, '', '/catch-up-result-b?retry=3#rendered');
        expect(fetch).toHaveBeenCalledTimes(2);
        expect(ts.navGeneration).toBe(retryGeneration);
        expect(nativeRefresh).toHaveBeenCalledTimes(1);
      }
    );
    it('a C transition aborts B catch-up and rejects a forced late B result before C admission', async () => {
      const responses: Array<(response: Response) => void> = [];
      const signals: AbortSignal[] = [];
      const fetch = vi.fn((_url: string, init: RequestInit) => {
        signals.push(init.signal as AbortSignal);
        return new Promise<Response>((resolve) => responses.push(resolve));
      });
      vi.stubGlobal('fetch', fetch);
      ts.scheduleInitialAdInit!({}, [configured]);
      flush();
      history.pushState({}, '', '/held-catch-up-b');
      const b = mount('native-b');
      const old = ts.registerPublisherSlot!({
        ...options(b.slot),
        navigationGeneration: ts.navGeneration ?? 0,
      });
      await runtime();
      expect(fetch).toHaveBeenCalledTimes(1);
      history.pushState({}, '', '/current-c');
      expect(ts.navGeneration).toBe(2);
      expect(fetch).toHaveBeenCalledTimes(2);
      expect(signals[0].aborted).toBe(true);
      expect(old.getStatus()).toEqual({ status: 'cancelled', reason: 'navigation' });
      const c = mount('native-c');
      const current = ts.registerPublisherSlot!({ ...options(c.slot), navigationGeneration: 2 });
      responses[0](
        new Response(
          JSON.stringify({
            slots: [{ ...configured, targeting: { section: 'stale-b' } }],
            bids: {},
          }),
          { headers: { 'Content-Type': 'application/json' } }
        )
      );
      await new Promise((resolve) => setTimeout(resolve, 0));
      flush();
      expect(current.getStatus().status).toBe('waiting_payload');
      expect(nativeDisplay).not.toHaveBeenCalled();
      expect(ts.publisherSlotRegistrationState?.payload).toBeUndefined();
      responses[1](
        new Response(
          JSON.stringify({
            slots: [{ ...configured, targeting: { section: 'current-c' } }],
            bids: {},
          }),
          { headers: { 'Content-Type': 'application/json' } }
        )
      );
      await new Promise((resolve) => setTimeout(resolve, 0));
      flush();
      expect(current.getStatus().status).toBe('requested');
      expect(snapshots).toEqual([{ publisher: 'keep', section: 'current-c', ts_initial: '1' }]);
    });
    it.each([false, true])(
      'provisional navigation snapshots old native cleanup before invalidation (old live=%s)',
      (keepOld) => {
        vi.stubGlobal(
          'fetch',
          vi.fn(() => new Promise<Response>(() => {}))
        );
        const old = mount();
        const oldHandle = ts.registerPublisherSlot!(options(old.slot));
        ts.scheduleInitialAdInit!({}, [configured]);
        flush();
        const service = tag.pubads;
        Reflect.deleteProperty(tag, 'pubads');
        history.pushState({}, '', '/provisional-cleanup-b');
        old.element.remove();
        nativeSlots = keepOld ? [old.slot] : [];
        const replacement = mount();
        replacement.slot.setTargeting('hb_adid', 'publisher-current');
        const oldClearCount = vi.mocked(old.slot.clearTargeting).mock.calls.length;
        tag.pubads = service;
        const current = ts.registerPublisherSlot!({
          ...options(replacement.slot),
          navigationGeneration: ts.navGeneration ?? 0,
        });
        commands.splice(0).forEach((callback) => callback());
        expect(oldHandle.getStatus()).toEqual({ status: 'cancelled', reason: 'navigation' });
        expect(current.getStatus().status).toBe('waiting_payload');
        expect(replacement.slot.clearTargeting).not.toHaveBeenCalled();
        expect(targetingOf(replacement.slot).hb_adid).toBe('publisher-current');
        expect(vi.mocked(old.slot.clearTargeting).mock.calls.length > oldClearCount).toBe(keepOld);
        expect(nativeRefresh).toHaveBeenCalledTimes(1);
      }
    );
    it('pre-runtime pathname navigation drops stale generation-zero automatic SSR work', () => {
      vi.stubGlobal(
        'fetch',
        vi.fn(() => new Promise<Response>(() => {}))
      );
      const automatic = {
        ...configured,
        initialization: 'auto' as const,
        div_id: 'automatic-stale-document',
      };
      const element = document.createElement('div');
      element.id = automatic.div_id;
      document.body.append(element);
      ts.scheduleInitialAdInit!({}, [automatic]);
      history.pushState({}, '', '/automatic-next-route');
      flush();
      expect(nativeDefine).not.toHaveBeenCalled();
      expect(nativeDisplay).not.toHaveBeenCalled();
      expect(nativeRefresh).not.toHaveBeenCalled();
      expect(tag.enableServices).not.toHaveBeenCalled();
    });
    // Runtime-only startup has no provisional navigation to catch up.
    if (implementation !== 'runtime') {
      it('returned-A catch-up failure leaves admission empty and does not strand a same-path retry', async () => {
        const responses: Array<(response: Response) => void> = [];
        const fetch = vi.fn(() => new Promise<Response>((resolve) => responses.push(resolve)));
        vi.stubGlobal('fetch', fetch);
        ts.scheduleInitialAdInit!({}, [
          { ...configured, targeting: { section: 'old-document-a' } },
        ]);
        flush();
        history.pushState({}, '', '/failed-round-trip-b');
        history.pushState({}, '', '/');
        const { slot } = mount();
        const old = ts.registerPublisherSlot!({
          ...options(slot),
          navigationGeneration: ts.navGeneration ?? 0,
        });
        const record = ts.publisherSlotRegistrationState?.records.header;
        const token = record?.token;
        await runtime();
        expect(ts.navGeneration).toBe(2);
        expect(fetch).toHaveBeenCalledTimes(1);
        responses[0](
          new Response('{}', { status: 503, headers: { 'Content-Type': 'application/json' } })
        );
        await new Promise((resolve) => setTimeout(resolve, 0));
        flush();
        expect(old.getStatus().status).toBe('waiting_payload');
        expect(ts.publisherSlotRegistrationState?.payload).toBeUndefined();
        expect(nativeDisplay).not.toHaveBeenCalled();
        history.replaceState({}, '', '/?retry=1#placement');
        expect(ts.navGeneration).toBe(2);
        expect(fetch).toHaveBeenCalledTimes(2);
        expect(ts.publisherSlotRegistrationState?.records.header).toBe(record);
        expect(record?.token).toBe(token);
        expect(record?.gptSlot).toBe(slot);
        const current = old;
        responses[1](
          new Response(
            JSON.stringify({
              slots: [{ ...configured, targeting: { section: 'new-returned-a' } }],
              bids: {},
            }),
            { headers: { 'Content-Type': 'application/json' } }
          )
        );
        await new Promise((resolve) => setTimeout(resolve, 0));
        flush();
        expect(current.getStatus().status).toBe('requested');
        expect(nativeRefresh).toHaveBeenCalledTimes(1);
        expect(snapshots[0].section).toBe('new-returned-a');
      });
    }
    it('tracks only a matching exact native render', async () => {
      const { slot } = mount();
      const handle = ts.registerPublisherSlot!(options(slot));
      await settle();
      for (const [name, callback] of tag.pubads().addEventListener.mock.calls as unknown as Array<
        [string, (event: { slot: PublisherGptSlot }) => void]
      >) {
        if (name === 'slotRenderEnded') callback({ slot: { ...slot } });
      }
      expect(handle.getStatus().status).toBe('requested');
      for (const [name, callback] of tag.pubads().addEventListener.mock.calls as unknown as Array<
        [string, (event: { slot: PublisherGptSlot }) => void]
      >) {
        if (name === 'slotRenderEnded') callback({ slot });
      }
      expect(handle.getStatus().status).toBe('rendered');
    });
  });
}
