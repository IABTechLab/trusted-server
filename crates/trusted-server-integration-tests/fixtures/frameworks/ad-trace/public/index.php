<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Trusted Server ad trace fixture</title>
  <style>
    body { font: 16px/1.4 system-ui, sans-serif; margin: 24px; }
    #ad-trace-slot { width: 300px; height: 250px; border: 1px solid #777; }
    #ad-trace-slot iframe { width: 100%; height: 100%; border: 0; }
  </style>
  <script>
    (() => {
      const tag = (window.googletag = window.googletag || { cmd: [] });
      const listeners = new Map();
      const slots = [];
      let suppressCreative = false;
      let nextRender = {};

      function emit(name, event) {
        for (const listener of listeners.get(name) || []) listener(event);
      }

      function firstTarget(slot, key) {
        const values = slot.getTargeting(key);
        return values.length ? values[0] : undefined;
      }

      function creativeFrame(slot) {
        const adId = firstTarget(slot, 'hb_adid');
        if (!adId || suppressCreative) return;
        const root = document.getElementById(slot.getSlotElementId());
        if (!root) return;
        const frame = document.createElement('iframe');
        frame.title = 'Example universal creative';
        frame.srcdoc = `<!doctype html><script>
          const channel = new MessageChannel();
          channel.port1.onmessage = (event) => {
            const payload = JSON.parse(event.data);
            if (payload.message !== 'Prebid Response') return;
            (0, eval)(payload.renderer);
            const helper = {
              mkFrame(doc, attrs) {
                const child = doc.createElement('iframe');
                Object.assign(child, attrs);
                return child;
              },
            };
            window.render(payload, helper, window);
          };
          top.postMessage(${JSON.stringify(JSON.stringify({ message: 'Prebid Request', adId }))}, '*', [channel.port2]);
        <\/script>`;
        frame.addEventListener('load', () => emit('slotOnload', { slot }), { once: true });
        root.replaceChildren(frame);
      }

      function requestSlot(slot) {
        const flags = nextRender;
        nextRender = {};
        // Real GPT responses are asynchronous. Deferring one task also gives
        // the injected diagnostic module time to install its bridge/listeners
        // after the server's end-of-body bid script starts this request.
        setTimeout(() => {
          emit('slotRequested', { slot });
          emit('slotResponseReceived', { slot });
          creativeFrame(slot);
          // Universal Creative requests pbRender from its iframe before GPT's
          // terminal render callback. A second task preserves that ordering and
          // prevents the debug ADM interceptor from replacing the requester.
          setTimeout(() => {
            emit('slotRenderEnded', {
              slot,
              isEmpty: flags.isEmpty === true,
              isBackfill: flags.isBackfill === true,
            });
          }, 500);
        }, 0);
      }

      const service = {
        setTargeting() { return service; },
        getTargeting() { return []; },
        enableSingleRequest() {},
        disableInitialLoad() {},
        addEventListener(name, listener) {
          const entries = listeners.get(name) || [];
          entries.push(listener);
          listeners.set(name, entries);
        },
        getSlots() { return slots.slice(); },
        refresh(requested) {
          for (const slot of requested || slots) requestSlot(slot);
        },
      };

      tag.pubads = () => service;
      tag.defineSlot = (unitPath, sizes, elementId) => {
        const targeting = new Map();
        const slot = {
          getAdUnitPath: () => unitPath,
          getSlotElementId: () => elementId,
          getSizes: () => sizes.map((size) => ({
            getWidth: () => Array.isArray(size) ? size[0] : 300,
            getHeight: () => Array.isArray(size) ? size[1] : 250,
          })),
          addService() { return slot; },
          setTargeting(key, value) {
            targeting.set(key, Array.isArray(value) ? value.map(String) : [String(value)]);
            return slot;
          },
          updateTargetingFromMap(values) {
            for (const [key, value] of Object.entries(values || {})) {
              if (value == null) targeting.delete(key);
              else targeting.set(key, Array.isArray(value) ? value.map(String) : [String(value)]);
            }
            return slot;
          },
          clearTargeting(key) {
            if (key) targeting.delete(key); else targeting.clear();
            return slot;
          },
          getTargeting(key) { return targeting.get(key) || []; },
        };
        slots.push(slot);
        return slot;
      };
      tag.destroySlots = (destroyed) => {
        for (const slot of destroyed || slots.slice()) {
          const index = slots.indexOf(slot);
          if (index >= 0) slots.splice(index, 1);
        }
        return true;
      };
      tag.enableServices = () => { tag._loaded_ = true; };
      tag.display = (elementId) => {
        const slot = slots.find((candidate) => candidate.getSlotElementId() === elementId);
        if (slot) requestSlot(slot);
      };

      const queued = Array.isArray(tag.cmd) ? tag.cmd.slice() : [];
      tag.cmd = { push(callback) { callback(); return 1; } };
      queued.forEach((callback) => callback());

      window.adTraceFixture = {
        latestSlot: () => slots.at(-1),
        setSuppressCreative(value) { suppressCreative = value; },
        setNextRender(flags) { nextRender = { ...flags }; },
        requestCurrent() {
          const slot = slots.at(-1);
          if (slot) requestSlot(slot);
        },
        simulateClientSelection() {
          const slot = slots.at(-1);
          const traceToken = window.tsjs?.bids?.['ad-trace-slot']?.trace?.bidTraceId;
          if (!slot || !traceToken) return;
          window.tsjs.prebidCorrelation = [
            {
              auctionId: 'example-client-auction',
              slotId: 'ad-trace-slot',
              requestId: 'example-client-request',
              adId: 'example-client-ad',
              bidder: 'example-client-bidder',
              events: ['prebid_bid_won'],
            },
            {
              auctionId: 'example-client-auction',
              slotId: 'ad-trace-slot',
              requestId: 'example-ts-request',
              adId: 'example-ts-loser',
              bidder: 'trustedServer',
              traceToken,
            },
          ];
          slot.clearTargeting();
          slot.setTargeting('hb_adid', 'example-client-ad');
          slot.setTargeting('hb_bidder', 'example-client-bidder');
          suppressCreative = true;
          window.tsjs.captureAdTraceRequest(slot, 'fixture_client_selection');
          requestSlot(slot);
        },
        simulateRetainedGenerationAcknowledgement() {
          const ts = window.tsjs;
          const first = ts.nextAdTraceGeneration('ad-trace-slot');
          const second = ts.nextAdTraceGeneration('ad-trace-slot');
          ts.recordAdTrace({
            kind: 'creative_load_acknowledged',
            slotId: 'ad-trace-slot',
            generation: first,
          });
          return { first, second };
        },
      };
    })();
  </script>
</head>
<body>
  <h1>Ad trace contract fixture</h1>
  <p>This page uses deterministic local PBS, GPT, and universal creative protocol mocks.</p>
  <div id="ad-trace-slot" aria-label="Example ad slot"></div>
</body>
</html>
