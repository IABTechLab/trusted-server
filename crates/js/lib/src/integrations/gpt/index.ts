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
  getTargeting(key: string): string[];
  addService(service: GoogleTagPubAdsService): GoogleTagSlot;
}

interface GoogleTagPubAdsService {
  setTargeting(key: string, value: string | string[]): GoogleTagPubAdsService;
  getTargeting(key: string): string[];
  enableSingleRequest(): void;
  addEventListener(
    eventName: 'slotRenderEnded',
    callback: (event: SlotRenderEndedEvent) => void
  ): void;
  refresh(slots?: GoogleTagSlot[]): void;
}

interface GoogleTag {
  cmd: Array<() => void>;
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
  __ts_ad_slots?: TsAdSlot[];
  __ts_request_id?: string;
  __tsAdInit?: () => boolean;
  __tsAdInitInstalled?: boolean;
};

type TsAdSlot = {
  id?: string;
  gam_unit_path?: string;
  div_id?: string;
  formats?: Array<number | number[]>;
  targeting?: Record<string, string | string[]>;
};

type TsBidTargeting = {
  hb_pb?: string;
  hb_bidder?: string;
  hb_adid?: string;
  burl?: string;
};

type TsBidMap = Record<string, TsBidTargeting | undefined>;

type DefinedTsSlot = {
  descriptor: TsAdSlot;
  slot: GoogleTagSlot;
};

type SlotRenderEndedEvent = {
  slot?: {
    getTargeting?: (key: string) => string[];
  };
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
  if (!Array.isArray(tag.cmd)) {
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
  queue.push = function (...callbacks: Array<() => void>): number {
    const wrapped = callbacks.map(wrapCommand);
    return originalPush(...wrapped);
  };

  // Mark as patched to prevent double-wrapping.
  (queue as { __tsPushed?: boolean }).__tsPushed = true;

  // Re-wrap any callbacks that were queued before we patched.
  for (let i = 0; i < queue.length; i++) {
    queue[i] = wrapCommand(queue[i]);
  }

  log.debug('GPT shim: command queue patched', { pendingCommands: queue.length });
}

function readTsAdSlots(win: GptWindow): TsAdSlot[] {
  return Array.isArray(win.__ts_ad_slots) ? win.__ts_ad_slots : [];
}

function fetchTsBids(win: GptWindow): Promise<TsBidMap> {
  const rid = win.__ts_request_id;
  if (!rid || typeof fetch !== 'function') {
    return Promise.resolve({});
  }

  return fetch(`/ts-bids?rid=${encodeURIComponent(rid)}`, { credentials: 'omit' })
    .then((response) => response.json() as Promise<TsBidMap>)
    .catch(() => ({}));
}

function applyStaticTargeting(slot: GoogleTagSlot, targeting: TsAdSlot['targeting']): void {
  for (const [key, value] of Object.entries(targeting ?? {})) {
    slot.setTargeting(key, value);
  }
}

function applyBidTargeting(slot: GoogleTagSlot, bid: TsBidTargeting): void {
  for (const key of ['hb_pb', 'hb_bidder', 'hb_adid'] as const) {
    const value = bid[key];
    if (value != null) {
      slot.setTargeting(key, String(value));
    }
  }
}

function installBurlListener(
  pubads: GoogleTagPubAdsService,
  bidsByAdId: Map<string, TsBidTargeting>
): void {
  if (typeof pubads.addEventListener !== 'function') {
    return;
  }

  pubads.addEventListener('slotRenderEnded', (event) => {
    const hbAdIds = event.slot?.getTargeting?.('hb_adid') ?? [];
    const hbAdId = hbAdIds[0];
    const bid = hbAdId ? bidsByAdId.get(hbAdId) : undefined;

    if (
      !bid?.burl ||
      typeof navigator === 'undefined' ||
      typeof navigator.sendBeacon !== 'function'
    ) {
      return;
    }

    navigator.sendBeacon(bid.burl);
    bidsByAdId.delete(hbAdId);
  });
}

function runTsAdInit(win: GptWindow): void {
  const tag = win.googletag as GoogleTag | undefined;
  const bidsPromise = fetchTsBids(win);
  const slots = readTsAdSlots(win);
  const definedSlots: DefinedTsSlot[] = [];
  const bidsByAdId = new Map<string, TsBidTargeting>();

  if (
    !tag ||
    typeof tag.defineSlot !== 'function' ||
    typeof tag.pubads !== 'function' ||
    typeof tag.enableServices !== 'function'
  ) {
    return;
  }

  const pubads = tag.pubads();
  installBurlListener(pubads, bidsByAdId);

  for (const descriptor of slots) {
    if (!descriptor.gam_unit_path || !descriptor.div_id || !descriptor.id) {
      continue;
    }

    const slot = tag.defineSlot(
      descriptor.gam_unit_path,
      descriptor.formats ?? [],
      descriptor.div_id
    );
    if (!slot) {
      continue;
    }

    if (typeof slot.addService === 'function') {
      slot.addService(pubads);
    }
    applyStaticTargeting(slot, descriptor.targeting);
    definedSlots.push({ descriptor, slot });
  }

  tag.enableServices();

  for (const { descriptor } of definedSlots) {
    if (typeof tag.display === 'function') {
      tag.display(descriptor.div_id as string);
    }
  }

  bidsPromise.then((bids) => {
    for (const { descriptor, slot } of definedSlots) {
      const bid = bids[descriptor.id as string];
      if (!bid) {
        continue;
      }

      applyBidTargeting(slot, bid);
      if (bid.hb_adid) {
        bidsByAdId.set(String(bid.hb_adid), bid);
      }
    }

    if (typeof pubads.refresh === 'function') {
      pubads.refresh(definedSlots.map(({ slot }) => slot));
    }
  });
}

/**
 * Install the Trusted Server ad bootstrap for GPT slots.
 *
 * The bootstrap reads `window.__ts_ad_slots` and `window.__ts_request_id`,
 * defines GPT slots immediately, then applies server-side bid targeting from
 * `/ts-bids` before refreshing the slots.
 */
export function installTsAdInit(): boolean {
  if (typeof window === 'undefined') {
    return false;
  }

  const win = window as GptWindow;
  if (win.__tsAdInitInstalled) {
    return true;
  }

  win.__tsAdInitInstalled = true;
  const tag = ensureGoogleTagStub(win);
  tag.cmd!.push(() => runTsAdInit(win));
  return true;
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
  installTsAdInit();

  log.info('GPT shim installed');
  return true;
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
  win.__tsAdInit = installTsAdInit;

  if (win.__tsjs_gpt_enabled === true) {
    installGptShim();
  }
}
