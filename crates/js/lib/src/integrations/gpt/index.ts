import { log } from '../../core/log';
import type { AuctionSlot, AuctionBidData, TsjsApi } from '../../core/types';

import { installGptGuard } from './script_guard';

/**
 * Google Publisher Tags (GPT) Integration Shim
 *
 * Hooks into the googletag.cmd command queue so the Trusted Server can
 * observe and augment ad-slot definitions before GPT processes them.
 * The shim ensures the googletag stub exists early (matching GPT's own
 * bootstrap pattern) and patches `cmd.push` to wrap queued callbacks.
 *
 * Current capabilities:
 *   - Installs a script guard that rewrites dynamically inserted GPT
 *     `<script>` elements to the first-party proxy endpoint.
 *   - Takes over the `googletag.cmd` array so that every callback runs
 *     through a wrapper that can inject targeting, logging, or consent
 *     signals before the real GPT processes the command.
 *
 * Future enhancements (driven by config or tsjs API):
 *   - Inject EC ID as page-level key-value targeting.
 *   - Gate ad requests on consent status.
 *   - Rewrite ad-unit paths for A/B testing.
 */

const TS_INITIAL_TARGETING_KEY = 'ts_initial' as const;
const TS_BID_TARGETING_KEYS = [
  'hb_pb',
  'hb_bidder',
  'hb_adid',
  'hb_cache_host',
  'hb_cache_path',
] as const;
const TS_BASE_TARGETING_KEYS = [...TS_BID_TARGETING_KEYS, TS_INITIAL_TARGETING_KEY] as const;

// ------------------------------------------------------------------
// googletag type stubs (minimal surface needed by the shim)
// ------------------------------------------------------------------

interface GoogleTagSlot {
  getAdUnitPath(): string;
  getSlotElementId(): string;
  setTargeting(key: string, value: string | string[]): GoogleTagSlot;
  clearTargeting?(key?: string): GoogleTagSlot;
  addService(service: GoogleTagPubAdsService): GoogleTagSlot;
  getTargeting?(key: string): string[];
}

interface SlotRenderEndedEvent {
  isEmpty: boolean;
  slot: GoogleTagSlot;
}

function findSlotElementByDivId(divId: string): HTMLElement | null {
  const exact = document.getElementById(divId);
  if (exact) return exact;

  return (
    Array.from(document.querySelectorAll<HTMLElement>('[id]')).find(
      (el) => el.id.startsWith(divId) && !el.id.endsWith('-container')
    ) ?? null
  );
}

function candidateSlotRoots(divId: string): HTMLElement[] {
  const roots: HTMLElement[] = [];
  const slotEl = findSlotElementByDivId(divId);
  if (slotEl) {
    roots.push(slotEl);
    const container = document.getElementById(`${slotEl.id}-container`);
    if (container) roots.push(container);
  }

  const configuredContainer = document.getElementById(`${divId}-container`);
  if (configuredContainer && !roots.includes(configuredContainer)) {
    roots.push(configuredContainer);
  }

  return roots;
}

function messageSourceBelongsToConfiguredSlot(source: MessageEventSource | null): boolean {
  if (!source) return false;

  const slots = window.tsjs?.adSlots ?? [];
  return slots.some((slot) =>
    candidateSlotRoots(slot.div_id).some((root) =>
      Array.from(root.querySelectorAll('iframe')).some((iframe) => iframe.contentWindow === source)
    )
  );
}

function clearTargetingKeys(slot: GoogleTagSlot, keys: Iterable<string>): void {
  if (typeof slot.clearTargeting !== 'function') return;

  for (const key of new Set(keys)) {
    slot.clearTargeting(key);
  }
}

interface GoogleTagPubAdsService {
  setTargeting(key: string, value: string | string[]): GoogleTagPubAdsService;
  getTargeting(key: string): string[];
  enableSingleRequest(): void;
  addEventListener(event: string, fn: (e: SlotRenderEndedEvent) => void): void;
  refresh(slots?: GoogleTagSlot[]): void;
  getSlots?(): GoogleTagSlot[];
}

interface GoogleTag {
  cmd: Array<() => void>;
  pubads(): GoogleTagPubAdsService;
  defineSlot(
    adUnitPath: string,
    size: Array<number | number[]>,
    elementId: string
  ): GoogleTagSlot | null;
  destroySlots(slots?: GoogleTagSlot[]): boolean;
  enableServices(): void;
  display(elementId: string): void;
  _loaded_?: boolean;
}

type GptWindow = Window & {
  googletag?: Partial<GoogleTag>;
  __tsjs_slim_prebid_url?: string;
};

// ------------------------------------------------------------------
// Shim implementation
// ------------------------------------------------------------------

/**
 * Ensure the `googletag` stub exists on `window`.
 *
 * This mirrors the official GPT bootstrap snippet:
 * ```js
 * window.googletag = window.googletag || {};
 * googletag.cmd = googletag.cmd || [];
 * ```
 * By running before the publisher's own snippet we can patch `cmd` early.
 */
function ensureGoogleTagStub(win: GptWindow): Partial<GoogleTag> {
  const tag = (win.googletag = win.googletag ?? {});
  tag.cmd = tag.cmd ?? [];
  return tag;
}

/**
 * Wrap a queued GPT callback to add instrumentation and future hook points.
 *
 * Today the wrapper only logs; as the integration matures it will inject
 * EC ID targeting and consent gates.
 */
function wrapCommand(fn: () => void): () => void {
  return () => {
    try {
      fn();
    } catch (err) {
      log.error('GPT shim: queued command threw', err);
    }
  };
}

/**
 * Patch `googletag.cmd` so every pushed callback runs through [`wrapCommand`].
 *
 * Preserves the existing `tag.cmd` array identity so that GPT's own custom
 * `cmd.push` behaviour (immediate execution when the library is already
 * loaded) is not lost. The original `push` is saved and delegated to after
 * wrapping each callback.
 *
 * Already-queued callbacks are re-wrapped in place so GPT processes them
 * through our wrapper when it drains the queue.
 */
function patchCommandQueue(tag: Partial<GoogleTag>): void {
  // Ensure the queue exists.
  if (!tag.cmd) {
    // Cast through unknown so an array satisfies the { push } type.
    tag.cmd = [];
  }

  const queue = tag.cmd;

  // Guard against double-patching (idempotent install).
  if ((queue as { __tsPushed?: boolean }).__tsPushed) {
    log.debug('GPT shim: command queue already patched, skipping');
    return;
  }

  const originalPush = queue.push.bind(queue);

  // Override push on the *existing* array — preserves object identity so
  // GPT (if already loaded) keeps its reference.
  (queue as { push: (...cbs: Array<() => void>) => unknown }).push = function (
    ...callbacks: Array<() => void>
  ): unknown {
    const wrapped = callbacks.map(wrapCommand);
    return originalPush(...wrapped);
  };

  // Mark as patched to prevent double-wrapping.
  (queue as { __tsPushed?: boolean }).__tsPushed = true;

  // Re-wrap any callbacks that were queued before we patched.
  // Only applicable when cmd is an array (pre-GPT-load case).
  if (Array.isArray(queue)) {
    for (let i = 0; i < queue.length; i++) {
      queue[i] = wrapCommand(queue[i]);
    }
    log.debug('GPT shim: command queue patched', { pendingCommands: queue.length });
  } else {
    log.debug('GPT shim: command queue patched');
  }
}

/**
 * Install the GPT integration shim.
 *
 * Sets up the script guard for dynamic script interception and patches the
 * `googletag.cmd` command queue.
 */
export function installGptShim(): boolean {
  if (typeof window === 'undefined') {
    return false;
  }

  const win = window as GptWindow;

  // Install DOM interception guard first so any dynamic GPT script insertions
  // are rewritten before the browser fetches them.
  installGptGuard();

  const tag = ensureGoogleTagStub(win);
  patchCommandQueue(tag);

  log.info('GPT shim installed');
  return true;
}

// ------------------------------------------------------------------
// GAM interceptor (testing only)
// ------------------------------------------------------------------

/**
 * Replace the GAM-rendered creative with the server-side auction adm.
 *
 * Adapted from PR #241 (github.com/IABTechLab/trusted-server/pull/241).
 * Instead of reading from pbjs, reads adm directly from window.tsjs.bids.
 * Only active when inject_adm_for_testing injects adm server-side.
 *
 * Strategy:
 * 1. If adm contains an <iframe src="...">, set that src on the GAM iframe
 *    directly — avoids cross-origin document access.
 * 2. Otherwise replace the slot element's content with a srcdoc iframe.
 */
function injectAdmIntoSlot(divId: string, adm: string): void {
  try {
    // divId may be the container div (used by GPT slot) or the inner div.
    // Search both so we can find the GAM iframe wherever it was rendered.
    const slotEl = document.getElementById(divId);
    if (!slotEl) return;

    // Extract the first iframe src from the adm (e.g. mocktioneer creative
    // wraps a first-party proxy iframe in a div).
    const srcMatch = adm.match(/<iframe[^>]+\bsrc=["']([^"']+)["']/i);
    const innerSrc = srcMatch?.[1];
    const gamIframe = slotEl.querySelector('iframe') as HTMLIFrameElement | null;

    if (innerSrc && gamIframe) {
      // Set the GAM iframe src — works even cross-origin (no document access needed).
      gamIframe.src = innerSrc.startsWith('//') ? `https:${innerSrc}` : innerSrc;
      log.debug(`[tsjs-gpt] gam-intercept: set iframe src for '${divId}'`);
    } else if (innerSrc) {
      // GAM iframe not yet in DOM (APS renders async after slotRenderEnded).
      // Retry on next animation frame so APS has a tick to insert its iframe;
      // if it still isn't there, replace slot content directly.
      requestAnimationFrame(() => {
        const retryIframe = slotEl!.querySelector('iframe') as HTMLIFrameElement | null;
        if (retryIframe) {
          retryIframe.src = innerSrc.startsWith('//') ? `https:${innerSrc}` : innerSrc;
          log.debug(`[tsjs-gpt] gam-intercept: set iframe src (retry) for '${divId}'`);
        } else {
          slotEl!.innerHTML = '';
          const f = document.createElement('iframe');
          f.style.cssText = 'border:none';
          f.width = String(slotEl!.offsetWidth || 728);
          f.height = String(slotEl!.offsetHeight || 90);
          f.setAttribute('sandbox', 'allow-scripts allow-popups allow-forms allow-same-origin');
          f.src = innerSrc.startsWith('//') ? `https:${innerSrc}` : innerSrc;
          slotEl!.appendChild(f);
          log.debug(`[tsjs-gpt] gam-intercept: inserted src iframe for '${divId}'`);
        }
      });
    } else {
      // No extractable src — replace slot content with a sandboxed srcdoc iframe.
      slotEl.innerHTML = '';
      const f = document.createElement('iframe');
      f.style.border = 'none';
      f.width = String(slotEl.offsetWidth || 728);
      f.height = String(slotEl.offsetHeight || 90);
      f.setAttribute('sandbox', 'allow-scripts allow-popups allow-forms allow-same-origin');
      f.srcdoc = adm;
      slotEl.appendChild(f);
      log.debug(`[tsjs-gpt] gam-intercept: replaced slot content for '${divId}'`);
    }
  } catch (err) {
    log.warn('[tsjs-gpt] gam-intercept: error injecting adm', err);
  }
}

// ------------------------------------------------------------------
// Trusted Server ad-init
// ------------------------------------------------------------------

/**
 * Install `window.tsjs.adInit`.
 *
 * Reads `window.tsjs.adSlots` (injected at head-open) and `window.tsjs.bids`
 * (injected before </body>) synchronously — no fetch, no Promise. Applies bid
 * targeting to GPT slots, sets the `ts_initial` sentinel, registers
 * `slotRenderEnded` to fire both nurl and burl via sendBeacon when our
 * specific Prebid bid wins the GAM line item match, then calls refresh().
 *
 * Idempotent: destroys previously created TS-managed slots before redefining them,
 * so it is safe to call again after SPA navigation updates `tsjs.adSlots`/`tsjs.bids`.
 */
export function installTsAdInit(): void {
  const ts = (window.tsjs ??= {} as TsjsApi);
  ts.adInit = function () {
    const slots = ts.adSlots ?? [];
    // Snapshot bids at adInit() call time — correct for targeting setup.
    // The slotRenderEnded listener below reads ts.bids live so SPA navigation
    // updates (new ts.bids injected before </body>) are picked up at render time.
    const bids = ts.bids ?? {};
    const g = (window as GptWindow).googletag;
    if (!g) return;

    g.cmd?.push(() => {
      // Destroy previously defined TS slots before redefining for the new page.
      if (ts.prevGptSlots && ts.prevGptSlots.length > 0) {
        g.destroySlots?.(ts.prevGptSlots as GoogleTagSlot[]);
        ts.prevGptSlots = [];
      }

      // Slots TS defined itself — tracked for SPA destroy. Publisher-owned
      // slots are reused but never destroyed by TS on navigation.
      const newSlots: GoogleTagSlot[] = [];
      // All slots to refresh (TS-defined + publisher-owned reused).
      const slotsToRefresh: GoogleTagSlot[] = [];
      const divToSlotId: Record<string, string> = {};
      const prevSlotTargetingKeys = ts.prevSlotTargetingKeys ?? {};
      const nextSlotTargetingKeys: Record<string, string[]> = {};

      // Clear TS-managed targeting from every previously TS-touched GPT slot
      // before applying the current route. Without this sweep, navigating to a
      // route with no matching TS slots (or one where a previously touched
      // publisher-owned slot is absent from the new slot list) leaves stale
      // hb_* / ts_initial / route targeting that later publisher refreshes
      // would reuse.
      const prevTouchedDivIds = new Set([
        ...Object.keys(prevSlotTargetingKeys),
        ...Object.keys(ts.divToSlotId ?? {}),
      ]);
      if (prevTouchedDivIds.size > 0) {
        (g.pubads!().getSlots?.() ?? []).forEach((gptSlot: GoogleTagSlot) => {
          const elementId = gptSlot.getSlotElementId();
          if (!prevTouchedDivIds.has(elementId)) return;
          clearTargetingKeys(gptSlot, [
            ...TS_BASE_TARGETING_KEYS,
            ...(prevSlotTargetingKeys[elementId] ?? []),
          ]);
        });
      }

      slots.forEach((slot) => {
        // Resolve actual div ID: exact match first, then prefix query.
        // div_id in config may be a stable prefix (e.g. "ad-header-0-") when
        // the suffix is dynamically generated by the framework at render time.
        const el = findSlotElementByDivId(slot.div_id);
        if (!el) return;
        const actualDivId = el.id;
        const bid = bids[slot.id] ?? {};

        const existingSlot = g.pubads!()
          .getSlots?.()
          ?.find?.((s: GoogleTagSlot) => s.getSlotElementId() === actualDivId);
        let gptSlot: GoogleTagSlot;
        let tsOwned = false;
        if (existingSlot) {
          gptSlot = existingSlot;
        } else {
          // Use outer container div for TS's slot when publisher hasn't defined
          // theirs yet — keeps both slots on separate divs so publisher's
          // later defineSlot on the inner div doesn't conflict.
          const containerEl = document.getElementById(`${actualDivId}-container`);
          const slotDivId = containerEl?.id ?? actualDivId;
          const defined = g.defineSlot?.(slot.gam_unit_path, slot.formats, slotDivId);
          if (!defined) return;
          defined.addService(g.pubads!());
          gptSlot = defined;
          tsOwned = true;
        }

        const slotDivId2 = gptSlot.getSlotElementId?.() ?? actualDivId;
        clearTargetingKeys(gptSlot, [
          ...TS_BASE_TARGETING_KEYS,
          ...(prevSlotTargetingKeys[actualDivId] ?? []),
          ...(prevSlotTargetingKeys[slotDivId2] ?? []),
        ]);

        Object.entries(slot.targeting ?? {}).forEach(([k, v]) => gptSlot.setTargeting(k, v));
        TS_BID_TARGETING_KEYS.forEach((key) => {
          if (bid[key]) gptSlot.setTargeting(key, String(bid[key]!));
        });
        gptSlot.setTargeting(TS_INITIAL_TARGETING_KEY, '1');
        // Map both inner div and container div → slot ID so slotRenderEnded
        // (which reports the GPT slot's div, i.e. slotDivId/container) can look up
        // the slot, while adm injection (which targets the inner div) also works.
        divToSlotId[actualDivId] = slot.id;
        if (slotDivId2 !== actualDivId) divToSlotId[slotDivId2] = slot.id;
        const slotTargetingKeys = Object.keys(slot.targeting ?? {});
        nextSlotTargetingKeys[actualDivId] = slotTargetingKeys;
        if (slotDivId2 !== actualDivId) nextSlotTargetingKeys[slotDivId2] = slotTargetingKeys;
        if (tsOwned) newSlots.push(gptSlot);
        slotsToRefresh.push(gptSlot);

        // APS: signal to apstag that bids are ready so Amazon's GAM creative
        // can render.  apstag must already be initialised on the page (which it
        // is on production publisher pages).  Safe no-op if apstag is absent.
        if (bid.hb_bidder === 'aps' || bid.hb_bidder === 'amazon-aps') {
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          (window as any).apstag?.setDisplayBids?.();
        }
      });

      ts.prevGptSlots = newSlots as unknown[];
      // Replace (not merge) so destroyed slots from previous navigation don't linger.
      ts.divToSlotId = divToSlotId;
      ts.prevSlotTargetingKeys = nextSlotTargetingKeys;

      // enableSingleRequest and enableServices must only be called once per page load.
      if (!ts.servicesEnabled) {
        g.pubads!().enableSingleRequest();
        g.enableServices?.();
        ts.servicesEnabled = true;

        g.pubads!().addEventListener?.('slotRenderEnded', (event: SlotRenderEndedEvent) => {
          const divId: string = event.slot?.getSlotElementId?.() ?? '';
          const slotId = (ts.divToSlotId ?? {})[divId];
          if (!slotId) return;
          // Read ts.bids live (not the snapshot above) so post-navigation bid data is used.
          const bid = (ts.bids ?? {})[slotId] ?? {};
          // Compare hb_adid targeting to verify the specific creative won.
          // APS bids carry no hb_adid — fall back to hb_bidder presence
          // (same heuristic as the inline bootstrap) so APS wins still bill.
          const ourBidWon =
            !event.isEmpty &&
            (bid.hb_adid
              ? event.slot?.getTargeting?.('hb_adid')?.[0] === bid.hb_adid
              : !!bid.hb_bidder);
          if (ourBidWon && (bid.nurl || bid.burl)) {
            // Fire win/billing beacons at most once per bid: GAM re-renders
            // (publisher refreshes, repeated slotRenderEnded for the same
            // line item) must not re-bill. New auctions produce new bid
            // identities, so post-navigation bids still fire. Keyed in
            // shared tsjs state so the inline-bootstrap listener and this
            // one can never double-fire the same bid.
            const beaconKey = `${slotId}|${bid.hb_adid ?? bid.nurl ?? bid.burl ?? ''}`;
            const fired = (ts.firedBeacons ??= {});
            if (!fired[beaconKey]) {
              fired[beaconKey] = true;
              if (bid.nurl) navigator.sendBeacon(bid.nurl);
              if (bid.burl) navigator.sendBeacon(bid.burl);
            }
          }

          // GAM interceptor (testing): when adm is present, replace the GAM creative.
          // Adapted from PR #241 — uses window.tsjs.bids[slotId].adm instead of pbjs.
          // Only active when inject_adm_for_testing injects adm into bids server-side.
          if (bid.adm) {
            injectAdmIntoSlot(divId, bid.adm);
          }
        });
      }

      if (slotsToRefresh.length > 0) {
        // One-shot bypass: this internal refresh delivers the just-applied
        // server-side targeting to GAM. If slim-Prebid has wrapped refresh(),
        // it must pass this call straight through — not clear the targeting
        // and run a duplicate client-side auction. Later publisher-initiated
        // refreshes of the same slots still go through the wrapper normally.
        ts.adInitRefreshInProgress = true;
        try {
          g.pubads!().refresh(slotsToRefresh);
        } finally {
          ts.adInitRefreshInProgress = false;
        }
      }
    });
  };
}

interface PageBidsResponse {
  slots: AuctionSlot[];
  bids: Record<string, AuctionBidData>;
}

/**
 * Install SPA navigation hook.
 *
 * Patches `history.pushState` and `history.replaceState`, and listens to
 * `popstate`, so that after each client-side route change the trusted server
 * fetches fresh slots + bids from `/__ts/page-bids?path=<new_path>`, updates
 * `window.tsjs.adSlots` / `window.tsjs.bids`, and calls `window.tsjs.adInit()`.
 *
 * Idempotent: guarded by `window.tsjs.spaHookInstalled` so multiple calls are safe.
 */
export function installSpaAuctionHook(): void {
  if (typeof window === 'undefined') return;
  const ts = (window.tsjs ??= {} as TsjsApi);
  if (ts.spaHookInstalled) return;
  ts.spaHookInstalled = true;

  let inflight: AbortController | null = null;

  async function onNavigate(path: string): Promise<void> {
    inflight?.abort();
    const controller = new AbortController();
    inflight = controller;

    try {
      const res = await fetch(`/__ts/page-bids?path=${encodeURIComponent(path)}`, {
        credentials: 'include',
        signal: controller.signal,
      });
      if (!res.ok) return;
      const data = (await res.json()) as PageBidsResponse;
      if (inflight !== controller) return;
      ts.adSlots = data.slots;
      ts.bids = data.bids;
      ts.adInit?.();
    } catch (err) {
      if (err instanceof DOMException && err.name === 'AbortError') return;
      log.warn('SPA auction hook: fetch failed', err);
    }
  }

  function patchHistoryMethod(method: 'pushState' | 'replaceState'): void {
    const original = history[method].bind(history);
    history[method] = function (state: unknown, unused: string, url?: string | URL | null): void {
      const prevPath = location.pathname;
      original(state, unused, url);
      const newPath = url ? new URL(String(url), location.href).pathname : location.pathname;
      if (newPath !== prevPath) {
        void onNavigate(newPath);
      }
    };
  }

  patchHistoryMethod('pushState');
  patchHistoryMethod('replaceState');

  window.addEventListener('popstate', () => {
    void onNavigate(location.pathname);
  });
}

/**
 * Register the slim-Prebid lazy loader. Fires after window.load — off the
 * critical path. Slim-Prebid handles scroll/refresh auctions and userID
 * module warm-up (ID5, sharedID, LiveRamp ATS, Lockr).
 *
 * Phase 1: no-op unless `window.__tsjs_slim_prebid_url` is set (the slim
 * bundle build target ships in a later phase).
 */
export function installSlimPrebidLoader(): void {
  const url = (window as GptWindow).__tsjs_slim_prebid_url;
  if (!url) return;
  window.addEventListener('load', () => {
    const script = document.createElement('script');
    script.src = url;
    script.defer = true;
    document.head.appendChild(script);
  });
}

/** Minimal display renderer injected into the ad iframe by pbRender. */
const TS_DISPLAY_RENDERER =
  '(function(){window.render=function(d,h,w){' +
  'var f=h.mkFrame(w.document,{width:d.width||"100%",height:d.height||"100%"});' +
  'if(d.adUrl&&!d.ad){f.src=d.adUrl;}else{f.srcdoc=d.ad;}' +
  'w.document.body.appendChild(f);};})();';

/**
 * Install the TS → pbRender bridge.
 *
 * Must be installed synchronously at module init — before `adInit()` fires
 * `refresh()`, which triggers GAM to serve the Prebid creative. Installing
 * post-load would miss first-impression `"Prebid Request"` messages.
 *
 * When `adId` matches a TS server-side bid in `window.tsjs.bids` AND the bid
 * has renderable markup, the bridge:
 *   1. Uses debug `adm` directly when present, otherwise fetches from PBS Cache.
 *   2. Replies via the MessageChannel port with a `"Prebid Response"`.
 *   3. Calls `stopImmediatePropagation()` so Prebid.js does not also process
 *      the message and log spurious failures.
 *
 * Lives in gpt/index.ts (not prebid/index.ts) to avoid pulling the full
 * Prebid bundle into tsjs-gpt.js via inlineDynamicImports.
 */
export function installTsRenderBridge(): void {
  if (typeof window === 'undefined') return;

  window.addEventListener('message', (e: MessageEvent) => {
    let data: Record<string, unknown>;
    try {
      data =
        typeof e.data === 'object'
          ? (e.data as Record<string, unknown>)
          : (JSON.parse(e.data as string) as Record<string, unknown>);
    } catch {
      return;
    }

    if (data['message'] !== 'Prebid Request') return;
    const adId = data['adId'] as string | undefined;
    if (!adId) return;

    const port = e.ports?.[0];
    if (!port) return;
    if (!messageSourceBelongsToConfiguredSlot(e.source)) return;

    // Build reverse map adId → slotId from live window.tsjs.bids.
    const bids = window.tsjs?.bids ?? {};
    let slotId: string | undefined;
    let matchedBid: (typeof bids)[string] | undefined;
    for (const [sid, bid] of Object.entries(bids)) {
      if (bid.hb_adid === adId) {
        slotId = sid;
        matchedBid = bid;
        break;
      }
    }

    // Not a TS bid — let Prebid.js handle it.
    if (!slotId || !matchedBid) return;

    const slot = window.tsjs?.adSlots?.find((s) => s.id === slotId);
    const [width, height] = slot?.formats?.[0] ?? [728, 90];

    if (matchedBid.adm) {
      e.stopImmediatePropagation();
      port.postMessage(
        JSON.stringify({
          message: 'Prebid Response',
          adId,
          ad: matchedBid.adm,
          renderer: TS_DISPLAY_RENDERER,
          width,
          height,
        })
      );
      log.debug(`[tsjs-gpt] pbRender bridge served '${slotId}' from debug adm`);
      return;
    }

    // No TS render source — let Prebid.js handle it.
    if (!matchedBid.hb_cache_host || !matchedBid.hb_cache_path) return;

    // TS owns this adId — stop Prebid from also processing it.
    e.stopImmediatePropagation();

    const cacheUrl = `https://${matchedBid.hb_cache_host}${matchedBid.hb_cache_path}?uuid=${encodeURIComponent(adId)}`;

    fetch(cacheUrl, { mode: 'cors' })
      .then((res) => (res.ok ? res.text() : Promise.reject(res.status)))
      .then((ad) => {
        port.postMessage(
          JSON.stringify({
            message: 'Prebid Response',
            adId,
            ad,
            renderer: TS_DISPLAY_RENDERER,
            width,
            height,
          })
        );
        log.debug(`[tsjs-gpt] pbRender bridge served '${slotId}' from PBS Cache`);
      })
      .catch((err) => {
        log.warn(`[tsjs-gpt] pbRender bridge: PBS Cache fetch failed for '${slotId}'`, err);
      });
  });
}

// Register the activation function on `window` so the server-injected inline
// script can call it explicitly. The server emits:
//   <script>window.__tsjs_gpt_enabled=true;
//          window.__tsjs_installGptShim&&window.__tsjs_installGptShim();</script>
// The HTML pipeline currently injects that inline script before the unified
// bundle, so the explicit call is best-effort only. To make activation robust
// regardless of script order, the module also checks for a pre-set enable flag
// immediately after registering the function.
if (typeof window !== 'undefined') {
  const win = window as unknown as Record<string, unknown>;

  win.__tsjs_installGptShim = installGptShim;

  if (win.__tsjs_gpt_enabled === true) {
    installGptShim();
  }

  installTsAdInit();
  installSpaAuctionHook();
  installSlimPrebidLoader();
  installTsRenderBridge();
}
