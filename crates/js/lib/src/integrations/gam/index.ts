// GAM (Google Ad Manager) Interceptor - forces Prebid creatives to render when
// GAM doesn't have matching line items configured.
//
// This integration intercepts GPT's slotRenderEnded event and replaces GAM's
// creative with the Prebid winning bid when:
// 1. A Prebid bid exists for the slot (hb_adid targeting is set)
// 2. The bid meets the configured criteria (specific bidder or any bidder)
//
// Configuration options:
// - enabled: boolean (default: false) - Master switch for the interceptor
// - bidders: string[] (default: []) - Only intercept for these bidders. Empty = all bidders
// - forceRender: boolean (default: false) - Render even if GAM has a line item
//
// Usage:
//   window.tsGamConfig = { enabled: true, bidders: ['mocktioneer'] };
//   // or via tsjs.setConfig({ gam: { enabled: true, bidders: ['mocktioneer'] } })

import { log } from '../../core/log';

export interface TsGamConfig {
  /** Enable the GAM interceptor. Defaults to false. */
  enabled?: boolean;
  /** Only intercept bids from these bidders. Empty array = all bidders. */
  bidders?: string[];
  /** Force render Prebid creative even if GAM returned a line item. Defaults to false. */
  forceRender?: boolean;
}

export interface TsGamApi {
  setConfig(cfg: TsGamConfig): void;
  getConfig(): TsGamConfig;
  getStats(): GamInterceptStats;
}

interface GamInterceptStats {
  intercepted: number;
  rendered: Array<{
    slotId: string;
    adId: string;
    bidder: string;
    method: string;
    timestamp: number;
  }>;
}

type GamWindow = Window & {
  googletag?: {
    pubads?: () => {
      addEventListener: (event: string, callback: (e: SlotRenderEndedEvent) => void) => void;
      getSlots?: () => GptSlot[];
    };
  };
  pbjs?: {
    getBidResponsesForAdUnitCode?: (code: string) => { bids?: PrebidBid[] };
    renderAd?: (doc: Document, adId: string) => void;
  };
  tsGamConfig?: TsGamConfig;
  __tsGamInstalled?: boolean;
};

interface SlotRenderEndedEvent {
  slot: GptSlot;
  isEmpty: boolean;
  lineItemId: number | null;
}

interface GptSlot {
  getSlotElementId(): string;
  getTargeting(key: string): string[];
  getTargetingKeys(): string[];
}

interface PrebidBid {
  adId?: string;
  ad?: string;
  adUrl?: string;
  bidder?: string;
  cpm?: number;
}

interface IframeAttrs {
  src: string;
  width?: string;
  height?: string;
}

/**
 * Extract iframe attributes from a creative that is just an iframe wrapper.
 * Returns null if the creative is not a simple iframe tag.
 * Exported for testing.
 */
export function extractIframeAttrs(html: string): IframeAttrs | null {
  const trimmed = html.trim();
  // Check if it's a simple iframe tag (possibly with whitespace/newline after)
  if (!trimmed.toLowerCase().startsWith('<iframe ')) {
    return null;
  }

  // Use regex to extract src attribute
  const srcMatch = trimmed.match(/\bsrc=["']([^"']+)["']/i);
  if (!srcMatch) {
    return null;
  }

  // Verify this is mostly just an iframe (no complex content after)
  // Allow trailing whitespace, newlines, and closing tag
  const afterIframe = trimmed.replace(/<iframe[^>]*>[\s\S]*?<\/iframe>/i, '').trim();
  if (afterIframe.length > 0 && !afterIframe.match(/^[\s\n]*$/)) {
    // Has significant content after iframe, not a simple wrapper
    return null;
  }

  // Extract width and height if present
  const widthMatch = trimmed.match(/\bwidth=["']?(\d+)["']?/i);
  const heightMatch = trimmed.match(/\bheight=["']?(\d+)["']?/i);

  return {
    src: srcMatch[1],
    width: widthMatch?.[1],
    height: heightMatch?.[1],
  };
}

/** @deprecated Use extractIframeAttrs instead */
export function extractIframeSrc(html: string): string | null {
  const attrs = extractIframeAttrs(html);
  return attrs?.src ?? null;
}

const DEFAULT_CONFIG: Required<TsGamConfig> = {
  enabled: false,
  bidders: [],
  forceRender: false,
};

let currentConfig: Required<TsGamConfig> = { ...DEFAULT_CONFIG };
let installed = false;

const stats: GamInterceptStats = {
  intercepted: 0,
  rendered: [],
};

function shouldIntercept(hbBidder: string, lineItemId: number | null): boolean {
  if (!currentConfig.enabled) return false;

  // Check if we should intercept this bidder
  if (currentConfig.bidders.length > 0 && !currentConfig.bidders.includes(hbBidder)) {
    return false;
  }

  // If forceRender is false, only intercept when GAM has no line item
  if (!currentConfig.forceRender && lineItemId !== null) {
    return false;
  }

  return true;
}

function renderPrebidCreative(
  slotId: string,
  hbAdId: string,
  hbBidder: string,
  iframe: HTMLIFrameElement,
  win: GamWindow
): boolean {
  try {
    const pbjs = win.pbjs;
    if (!pbjs?.getBidResponsesForAdUnitCode) {
      log.warn('gam-intercept: pbjs.getBidResponsesForAdUnitCode not available');
      return false;
    }

    const bidResponses = pbjs.getBidResponsesForAdUnitCode(slotId);
    const bid = bidResponses?.bids?.find((b) => b.adId === hbAdId);

    if (bid?.ad) {
      // Check if the creative is a simple iframe wrapper
      // GAM's iframe has CSP frame-src 'none' which blocks nested iframes
      // So we extract the iframe src and set it on the parent iframe directly
      const iframeAttrs = extractIframeAttrs(bid.ad);
      if (iframeAttrs) {
        log.debug('gam-intercept: creative is iframe wrapper, setting src directly', {
          slotId,
          adId: hbAdId,
          src: iframeAttrs.src,
          width: iframeAttrs.width,
          height: iframeAttrs.height,
        });
        iframe.src = iframeAttrs.src;
        // Apply dimensions from the creative if specified
        if (iframeAttrs.width) {
          iframe.width = iframeAttrs.width;
        }
        if (iframeAttrs.height) {
          iframe.height = iframeAttrs.height;
        }
        stats.rendered.push({
          slotId,
          adId: hbAdId,
          bidder: hbBidder,
          method: 'iframe.src (unwrapped)',
          timestamp: Date.now(),
        });
        stats.intercepted++;
        log.info('gam-intercept: rendered creative', { slotId, bidder: hbBidder });
        return true;
      }

      // Not an iframe wrapper, use doc.write
      log.debug('gam-intercept: rendering ad creative via doc.write', { slotId, adId: hbAdId });
      const doc = iframe.contentDocument || iframe.contentWindow?.document;
      if (doc) {
        doc.open();
        doc.write(bid.ad);
        doc.close();
        stats.rendered.push({
          slotId,
          adId: hbAdId,
          bidder: hbBidder,
          method: 'doc.write',
          timestamp: Date.now(),
        });
        stats.intercepted++;
        log.info('gam-intercept: rendered creative', { slotId, bidder: hbBidder });
        return true;
      }
    } else if (bid?.adUrl) {
      log.debug('gam-intercept: rendering ad via iframe src', { slotId, adId: hbAdId });
      iframe.src = bid.adUrl;
      stats.rendered.push({
        slotId,
        adId: hbAdId,
        bidder: hbBidder,
        method: 'iframe.src',
        timestamp: Date.now(),
      });
      stats.intercepted++;
      log.info('gam-intercept: set iframe src', { slotId, bidder: hbBidder });
      return true;
    } else if (pbjs.renderAd) {
      log.debug('gam-intercept: rendering via pbjs.renderAd', { slotId, adId: hbAdId });
      const doc = iframe.contentDocument || iframe.contentWindow?.document;
      if (doc) {
        pbjs.renderAd(doc, hbAdId);
        stats.rendered.push({
          slotId,
          adId: hbAdId,
          bidder: hbBidder,
          method: 'pbjs.renderAd',
          timestamp: Date.now(),
        });
        stats.intercepted++;
        log.info('gam-intercept: called pbjs.renderAd', { slotId, bidder: hbBidder });
        return true;
      }
    }

    log.warn('gam-intercept: no valid creative found', { slotId, adId: hbAdId });
    return false;
  } catch (err) {
    log.warn('gam-intercept: error rendering creative', { slotId, error: err });
    return false;
  }
}

function handleSlotRenderEnded(event: SlotRenderEndedEvent, win: GamWindow): void {
  const slotId = event.slot.getSlotElementId();
  const hbAdId = event.slot.getTargeting('hb_adid')?.[0];
  const hbBidder = event.slot.getTargeting('hb_bidder')?.[0];

  log.debug('gam-intercept: slotRenderEnded', {
    slotId,
    isEmpty: event.isEmpty,
    lineItemId: event.lineItemId,
    hbBidder,
    hbAdId,
  });

  // Need both adId and bidder to render
  if (!hbAdId || !hbBidder) {
    return;
  }

  // Check if we should intercept this slot
  if (!shouldIntercept(hbBidder, event.lineItemId)) {
    log.debug('gam-intercept: skipping slot (not matching criteria)', { slotId, hbBidder });
    return;
  }

  // Find the iframe in the slot
  const slotElement = document.getElementById(slotId);
  const iframe = slotElement?.querySelector('iframe') as HTMLIFrameElement | null;

  if (!iframe) {
    log.warn('gam-intercept: no iframe found in slot', { slotId });
    return;
  }

  renderPrebidCreative(slotId, hbAdId, hbBidder, iframe, win);
}

function installInterceptor(win: GamWindow): void {
  if (installed || win.__tsGamInstalled) {
    return;
  }

  const googletag = win.googletag;
  const pubads = googletag?.pubads?.();

  if (!pubads?.addEventListener) {
    log.debug('gam-intercept: googletag.pubads not ready');
    return;
  }

  pubads.addEventListener('slotRenderEnded', (event: SlotRenderEndedEvent) => {
    handleSlotRenderEnded(event, win);
  });

  installed = true;
  win.__tsGamInstalled = true;
  log.info('gam-intercept: installed slotRenderEnded listener', {
    enabled: currentConfig.enabled,
    bidders: currentConfig.bidders,
    forceRender: currentConfig.forceRender,
  });
}

function waitForGpt(win: GamWindow): void {
  let attempts = 0;
  const maxAttempts = 300; // 30 seconds at 100ms intervals

  const check = setInterval(() => {
    attempts++;

    // Check for config on each poll - it may be set after module load
    if (win.tsGamConfig && !currentConfig.enabled) {
      setGamConfig(win.tsGamConfig);
    }

    if (win.googletag?.pubads && win.pbjs) {
      clearInterval(check);
      installInterceptor(win);
      return;
    }

    if (attempts >= maxAttempts) {
      clearInterval(check);
      log.debug('gam-intercept: timeout waiting for googletag/pbjs');
    }
  }, 100);
}

export function setGamConfig(cfg: TsGamConfig): void {
  currentConfig = {
    enabled: cfg.enabled ?? currentConfig.enabled,
    bidders: cfg.bidders ?? currentConfig.bidders,
    forceRender: cfg.forceRender ?? currentConfig.forceRender,
  };
  log.debug('gam-intercept: config updated', currentConfig);
}

export function getGamConfig(): TsGamConfig {
  return { ...currentConfig };
}

export function getGamStats(): GamInterceptStats {
  return {
    intercepted: stats.intercepted,
    rendered: [...stats.rendered],
  };
}

export const tsGam: TsGamApi = {
  setConfig: setGamConfig,
  getConfig: getGamConfig,
  getStats: getGamStats,
};

// Auto-initialize on module load
(function autoInit(): void {
  if (typeof window === 'undefined') return;

  const win = window as GamWindow;

  // Check for pre-set config
  const initialConfig = win.tsGamConfig;
  if (initialConfig) {
    setGamConfig(initialConfig);
  }

  // Start waiting for GPT
  waitForGpt(win);
})();

export default tsGam;
