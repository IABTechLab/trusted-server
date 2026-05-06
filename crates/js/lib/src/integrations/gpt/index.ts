import { log } from '../../core/log';

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

// ------------------------------------------------------------------
// googletag type stubs (minimal surface needed by the shim)
// ------------------------------------------------------------------

interface GoogleTagSlot {
  getAdUnitPath(): string;
  getSlotElementId(): string;
  setTargeting(key: string, value: string | string[]): GoogleTagSlot;
  addService(service: GoogleTagPubAdsService): GoogleTagSlot;
}

interface GoogleTagPubAdsService {
  setTargeting(key: string, value: string | string[]): GoogleTagPubAdsService;
  getTargeting(key: string): string[];
  enableSingleRequest(): void;
  addEventListener(event: string, fn: (e: any) => void): void;
  refresh(): void;
}

interface GoogleTag {
  cmd: { push: (fn: () => void) => unknown };
  pubads(): GoogleTagPubAdsService;
  defineSlot(
    adUnitPath: string,
    size: Array<number | number[]>,
    elementId: string
  ): GoogleTagSlot | null;
  enableServices(): void;
  display(elementId: string): void;
  _loaded_?: boolean;
}

type GptWindow = Window & {
  googletag?: Partial<GoogleTag>;
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
  tag.cmd = tag.cmd ?? (([] as unknown) as { push: (fn: () => void) => unknown });
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
    tag.cmd = ([] as unknown) as { push: (fn: () => void) => unknown };
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
// Trusted Server ad-init types
// ------------------------------------------------------------------

interface TsAdSlot {
  id: string;
  gam_unit_path: string;
  div_id: string;
  formats: Array<number[]>;
  targeting: Record<string, string>;
}

interface TsBidData {
  hb_pb?: string;
  hb_bidder?: string;
  hb_adid?: string;
  nurl?: string;
  burl?: string;
}

type TsWindow = Window & {
  __ts_ad_slots?: TsAdSlot[];
  __ts_bids?: Record<string, TsBidData>;
  __tsAdInit?: () => void;
};

/**
 * Install `window.__tsAdInit`.
 *
 * Reads `window.__ts_ad_slots` (injected at head-open) and `window.__ts_bids`
 * (injected before </body>) synchronously — no fetch, no Promise. Applies bid
 * targeting to GPT slots, sets the `ts_initial` sentinel, registers
 * `slotRenderEnded` to fire both nurl and burl via sendBeacon when our
 * specific Prebid bid wins the GAM line item match, then calls refresh().
 */
export function installTsAdInit(): void {
  const w = window as TsWindow;
  w.__tsAdInit = function () {
    const slots = w.__ts_ad_slots ?? [];
    const bids = w.__ts_bids ?? {};
    const g = (window as GptWindow).googletag;
    if (!g) return;

    g.cmd.push(() => {
      slots
        .map((slot) => {
          const gptSlot = g.defineSlot?.(slot.gam_unit_path, slot.formats as Array<number | number[]>, slot.div_id);
          if (!gptSlot) return null;
          gptSlot.addService(g.pubads!());
          Object.entries(slot.targeting ?? {}).forEach(([k, v]) => gptSlot.setTargeting(k, v));
          const bid = bids[slot.id] ?? {};
          (['hb_pb', 'hb_bidder', 'hb_adid'] as const).forEach((key) => {
            if (bid[key]) gptSlot.setTargeting(key, bid[key]!);
          });
          gptSlot.setTargeting('ts_initial', '1');
          return { id: slot.id, gptSlot };
        })
        .filter(Boolean);

      g.pubads!().enableSingleRequest();
      g.enableServices?.();

      g.pubads!().addEventListener?.('slotRenderEnded', (event: any) => {
        const slotId: string = event.slot?.getSlotElementId?.() ?? '';
        const bid = bids[slotId] ?? {};
        const ourBidWon =
          !event.isEmpty &&
          bid.hb_adid &&
          event.slot?.getTargeting?.('hb_adid')?.[0] === bid.hb_adid;
        if (ourBidWon) {
          if (bid.nurl) navigator.sendBeacon(bid.nurl);
          if (bid.burl) navigator.sendBeacon(bid.burl);
        }
      });

      g.pubads!().refresh();
    });
  };
}

/**
 * Register the slim-Prebid lazy loader. Fires after window.load — off the
 * critical path. slim-Prebid handles refresh auctions and userID module
 * warm-up (ID5, sharedID, LiveRamp ATS, Lockr). It skips initial-render slots
 * (ts_initial=1) and registers as the GPT refresh handler for scroll/sticky auctions.
 *
 * Phase 1: no-op unless window.__tsjs_slim_prebid_url is set (it won't be until
 * the slim-Prebid bundle build target ships in a later phase).
 */
export function installSlimPrebidLoader(): void {
  const url = (window as any).__tsjs_slim_prebid_url as string | undefined;
  if (!url) return;
  window.addEventListener('load', () => {
    const script = document.createElement('script');
    script.src = url;
    script.defer = true;
    document.head.appendChild(script);
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
  const win = window as Record<string, unknown>

  win.__tsjs_installGptShim = installGptShim

  if (win.__tsjs_gpt_enabled === true) {
    installGptShim()
  }

  installTsAdInit()
}
