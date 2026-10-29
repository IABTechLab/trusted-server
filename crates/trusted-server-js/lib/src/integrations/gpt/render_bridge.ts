import { log } from '../../core/log';
import { resolveSlotElementByDivId } from '../../core/slot_element';
import type { AuctionBidData, GptDiagnosticsCreativeFailure, TsjsApi } from '../../core/types';
import {
  APS_UNIVERSAL_CREATIVE_RENDERER,
  APS_UNIVERSAL_CREATIVE_RENDERER_VERSION,
  apsRendererUrl,
  dispatchApsRendering,
  consumeApsPrebidRenderer,
  getApsPrebidRenderer,
  validateApsRenderer,
} from '../aps/render';

import { registeredCandidateAllowed } from './publisher_registration';

export function isNonEmptyString(value: unknown): value is string {
  return typeof value === 'string' && value.length > 0;
}

export function findSlotElementByDivId(divId: string): HTMLElement | null {
  return resolveSlotElementByDivId(divId).element;
}

function candidateSlotRoots(elementId: string): HTMLElement[] {
  const roots: HTMLElement[] = [];
  const slotEl = document.getElementById(elementId);
  if (slotEl) {
    roots.push(slotEl);
  }

  const container = document.getElementById(`${elementId}-container`);
  if (container && !roots.includes(container)) {
    roots.push(container);
  }

  return roots;
}

interface MessageSourceFrame {
  iframe: HTMLIFrameElement;
  root: HTMLElement;
}

function sourceFrameInRoots(
  source: MessageEventSource | null,
  roots: readonly HTMLElement[]
): MessageSourceFrame | undefined {
  if (!source) return undefined;
  const matches = new Map<HTMLIFrameElement, HTMLElement>();
  for (const root of roots) {
    for (const iframe of root.querySelectorAll('iframe')) {
      if (iframe.contentWindow === source && !matches.has(iframe)) matches.set(iframe, root);
    }
  }
  if (matches.size !== 1) return undefined;
  const [iframe, root] = matches.entries().next().value as [HTMLIFrameElement, HTMLElement];
  return { iframe, root };
}

interface ConfiguredMessageSourceFrame extends MessageSourceFrame {
  exact: boolean;
  prefixLength: number;
}

function sourceFrameForConfiguredDivId(
  source: MessageEventSource | null,
  divId: string
): ConfiguredMessageSourceFrame | undefined {
  const exact = document.getElementById(divId);
  const candidates = exact
    ? [exact]
    : Array.from(document.querySelectorAll<HTMLElement>('[id]')).filter(
        (element) => element.id.startsWith(divId) && !element.id.endsWith('-container')
      );
  const matches = candidates
    .map((element) => sourceFrameInRoots(source, candidateSlotRoots(element.id)))
    .filter((frame): frame is MessageSourceFrame => frame !== undefined);
  const frame = matches.length === 1 ? matches[0] : undefined;
  return frame ? { ...frame, exact: exact !== null, prefixLength: divId.length } : undefined;
}

function uniqueSourceFrame(
  frames: Array<MessageSourceFrame | undefined>
): MessageSourceFrame | undefined {
  const matches = new Map<HTMLIFrameElement, MessageSourceFrame>();
  for (const frame of frames) {
    if (frame) matches.set(frame.iframe, frame);
  }
  return matches.size === 1 ? matches.values().next().value : undefined;
}

function sourceFrameForSlotId(
  source: MessageEventSource | null,
  slotId: string
): MessageSourceFrame | undefined {
  const mappedFrames = Object.entries(window.tsjs?.divToSlotId ?? {})
    .filter(([, mappedSlotId]) => mappedSlotId === slotId)
    .map(([elementId]) => sourceFrameInRoots(source, candidateSlotRoots(elementId)));
  const configuredFrames = (window.tsjs?.adSlots ?? [])
    .filter((slot) => slot.id === slotId)
    .map((slot) => sourceFrameForConfiguredDivId(source, slot.div_id));
  return uniqueSourceFrame([...mappedFrames, ...configuredFrames]);
}

interface MessageSourceSlotFrame extends MessageSourceFrame {
  slotId: string;
}

function slotFrameForMessageSource(
  source: MessageEventSource | null
): MessageSourceSlotFrame | undefined {
  const candidates: Array<{
    slotId: string;
    frame: MessageSourceFrame;
    exact: boolean;
    prefixLength: number;
  }> = [];
  for (const [elementId, slotId] of Object.entries(window.tsjs?.divToSlotId ?? {})) {
    const frame = sourceFrameInRoots(source, candidateSlotRoots(elementId));
    if (frame) candidates.push({ slotId, frame, exact: true, prefixLength: elementId.length });
  }
  for (const slot of window.tsjs?.adSlots ?? []) {
    const frame = sourceFrameForConfiguredDivId(source, slot.div_id);
    if (frame) {
      candidates.push({
        slotId: slot.id,
        frame,
        exact: frame.exact,
        prefixLength: frame.prefixLength,
      });
    }
  }

  const exactCandidates = candidates.filter((candidate) => candidate.exact);
  const rankedCandidates =
    exactCandidates.length > 0
      ? exactCandidates
      : candidates.filter(
          (candidate) =>
            candidate.prefixLength ===
            Math.max(...candidates.map((possible) => possible.prefixLength))
        );
  const slotIds = new Set(rankedCandidates.map((candidate) => candidate.slotId));
  if (slotIds.size !== 1) return undefined;
  const slotId = slotIds.values().next().value as string;
  const frame = uniqueSourceFrame(
    rankedCandidates
      .filter((candidate) => candidate.slotId === slotId)
      .map((candidate) => candidate.frame)
  );
  return frame ? { ...frame, slotId } : undefined;
}

function sourceFrameForAdUnit(
  source: MessageEventSource | null,
  adUnitCode: string
): MessageSourceFrame | undefined {
  return sourceFrameForConfiguredDivId(source, adUnitCode);
}

function hasCollapsedDimension(element: HTMLElement, dimension: 'width' | 'height'): boolean {
  const value = window.getComputedStyle(element)[dimension];
  const match = /^(\d+(?:\.\d+)?)px$/.exec(value);
  return match !== null && Number(match[1]) <= 1;
}

function usesFixedPositioning(element: HTMLElement): boolean {
  const position = window.getComputedStyle(element).position;
  return position === 'fixed' || position === 'sticky';
}

const MAX_CREATIVE_SHELL_DIMENSION = 10_000;

function creativeFrameIsCurrent(
  source: MessageEventSource | null,
  frame: MessageSourceFrame,
  generation: number,
  stillOwnsCreative: () => boolean
): boolean {
  return (
    (window.tsjs?.navGeneration ?? 0) === generation &&
    stillOwnsCreative() &&
    frame.iframe.isConnected &&
    frame.root.isConnected &&
    frame.root.contains(frame.iframe) &&
    frame.iframe.contentWindow === source
  );
}

/** Resize the authenticated source iframe and collapsed ancestors through its slot root. */
function resizeCollapsedCreativeFrame(
  source: MessageEventSource | null,
  frame: MessageSourceFrame,
  width: number,
  height: number,
  generation: number,
  stillOwnsCreative: () => boolean
): void {
  if (
    !creativeFrameIsCurrent(source, frame, generation, stillOwnsCreative) ||
    !Number.isFinite(width) ||
    !Number.isFinite(height) ||
    width <= 0 ||
    height <= 0 ||
    width > MAX_CREATIVE_SHELL_DIMENSION ||
    height > MAX_CREATIVE_SHELL_DIMENSION ||
    frame.iframe.getAttribute('width') !== '1' ||
    frame.iframe.getAttribute('height') !== '1' ||
    !hasCollapsedDimension(frame.iframe, 'width') ||
    !hasCollapsedDimension(frame.iframe, 'height') ||
    usesFixedPositioning(frame.iframe) ||
    frame.iframe.closest(
      'ins[data-anchor-status], [data-google-interstitial], [data-vignette-loaded]'
    )
  ) {
    return;
  }

  const collapsedAncestors: HTMLElement[] = [];
  let reachedRoot = false;
  for (let ancestor = frame.iframe.parentElement; ancestor; ancestor = ancestor.parentElement) {
    if (
      ancestor === document.body ||
      ancestor === document.documentElement ||
      !ancestor.isConnected ||
      usesFixedPositioning(ancestor) ||
      ancestor.matches(
        'ins[data-anchor-status], [data-google-interstitial], [data-vignette-loaded]'
      )
    ) {
      return;
    }
    if (hasCollapsedDimension(ancestor, 'width') || hasCollapsedDimension(ancestor, 'height')) {
      collapsedAncestors.push(ancestor);
    }
    if (ancestor === frame.root) {
      reachedRoot = true;
      break;
    }
  }
  if (!reachedRoot) return;

  frame.iframe.width = String(width);
  frame.iframe.height = String(height);
  frame.iframe.style.width = `${width}px`;
  frame.iframe.style.height = `${height}px`;
  for (const ancestor of collapsedAncestors) {
    ancestor.style.width = `${width}px`;
    ancestor.style.height = `${height}px`;
  }
}

function fireWinBillingBeacons(slotId: string, bid: AuctionBidData): void {
  if (!slotId || (!bid.nurl && !bid.burl)) return;

  const fired = (window.tsjs!.firedBeacons ??= {});
  const bidIdentity = bid.hb_adid ?? bid.nurl ?? bid.burl ?? '';
  const urls = [
    ['nurl', bid.nurl],
    ['burl', bid.burl],
  ] as const;

  for (const [kind, url] of urls) {
    if (!url) continue;

    const beaconKey = `${slotId}|${bidIdentity}|${kind}|${url}`;
    if (fired[beaconKey]) continue;

    if (queueWinBillingBeacon(url)) {
      fired[beaconKey] = true;
    }
  }
}

function queueWinBillingBeacon(url: string): boolean {
  if (typeof navigator !== 'undefined' && typeof navigator.sendBeacon === 'function') {
    try {
      if (navigator.sendBeacon(url)) {
        return true;
      }
    } catch (err) {
      log.warn('[tsjs-gpt] win/billing sendBeacon failed', err);
    }
  }

  if (typeof fetch === 'function') {
    try {
      void fetch(url, { method: 'POST', keepalive: true, mode: 'no-cors' });
      return true;
    } catch (err) {
      log.warn('[tsjs-gpt] win/billing fetch fallback failed', err);
    }
  }

  return false;
}
/** Minimal display renderer injected into the ad iframe by pbRender. */
const TS_DISPLAY_RENDERER =
  '(function(){window.render=function(d,h,w){' +
  'var f=h.mkFrame(w.document,{width:d.width||"100%",height:d.height||"100%"});' +
  'if(d.adUrl&&!d.ad){f.src=d.adUrl;}else{f.srcdoc=d.ad;}' +
  'w.document.body.appendChild(f);};})();';

/** The clear-price auction macro DSPs embed in creative markup and tracking URLs. */
const AUCTION_PRICE_MACRO = '${AUCTION_PRICE}';

/**
 * Substitute the `${AUCTION_PRICE}` macro with a clearing price. Mirrors the
 * server-side `expand_auction_price_macro`: only the exact clear-price token is
 * replaced, so the encrypted `${AUCTION_PRICE:B64}` variant is left intact.
 */
function expandAuctionPriceMacro(markup: string, cpm: number): string {
  return markup.includes(AUCTION_PRICE_MACRO)
    ? markup.split(AUCTION_PRICE_MACRO).join(String(cpm))
    : markup;
}

/** A decoded PBS Cache bid: the renderable creative plus its render metadata. */
export interface CachedBid {
  adm: string;
  width?: number;
  height?: number;
  price?: number;
}

/**
 * Decode a PBS Cache GET response into a renderable bid.
 *
 * Prebid Cache entries are JSON bid objects (`{ "adm": "<div…>", "w": …, … }`);
 * the Prebid Universal Creative's own cache path `JSON.parse`s the response and
 * renders `bidObject.adm`, sizing from the cached dimensions. This mirrors that,
 * retaining the fields the fallback render needs — creative dimensions (`w`/`h`
 * or `width`/`height`) and clearing `price` for macro expansion — rather than
 * reducing the payload to a bare `adm` string that forces the first slot format
 * and leaves price macros unresolved.
 *
 * A non-JSON body is treated as raw creative markup (the `{ adm }`-only variant)
 * for backward compatibility with caches that store the creative directly.
 * Returns `undefined` when the JSON payload carries no usable string `adm`, so
 * the caller can decline to render instead of injecting a serialized object.
 */
export function parseCachedBid(body: string): CachedBid | undefined {
  let parsed: unknown;
  try {
    parsed = JSON.parse(body);
  } catch {
    // Not JSON — a cache that returned the creative markup directly. No render
    // metadata is available, so only the raw markup variant is returned.
    return body.trim().length > 0 ? { adm: body } : undefined;
  }
  if (!parsed || typeof parsed !== 'object') {
    // A JSON primitive (string/number/bool) is not a valid cached bid object.
    return undefined;
  }
  const obj = parsed as Record<string, unknown>;
  const adm = obj.adm;
  if (typeof adm !== 'string' || adm.length === 0) return undefined;

  const num = (v: unknown): number | undefined =>
    typeof v === 'number' && Number.isFinite(v) ? v : undefined;
  // A zero (or missing) dimension is not usable render metadata; treat it as
  // absent so the caller falls back to the slot format rather than sizing to 0.
  const dim = (v: unknown): number | undefined => {
    const n = num(v);
    return n !== undefined && n > 0 ? n : undefined;
  };

  return {
    adm,
    // PBS OpenRTB bids carry w/h; the Prebid.js cache format uses width/height.
    width: dim(obj.w) ?? dim(obj.width),
    height: dim(obj.h) ?? dim(obj.height),
    price: num(obj.price),
  };
}

function safelyRecordCreativeRequest(slotId: string): number | undefined {
  try {
    const attemptId =
      window.tsjs?.gptDiagnosticsRecorder?.recordTrustedServerCreativeRequest(slotId);
    return typeof attemptId === 'number' && Number.isFinite(attemptId) ? attemptId : undefined;
  } catch {
    return undefined;
  }
}

function safelyRecordCreativeResponse(attemptId: number | undefined): void {
  if (attemptId === undefined) return;

  try {
    window.tsjs?.gptDiagnosticsRecorder?.recordTrustedServerCreativeResponse(attemptId);
  } catch {
    // Diagnostics must not alter creative delivery.
  }
}

function safelyRecordCreativeFailure(
  attemptId: number | undefined,
  reason: GptDiagnosticsCreativeFailure
): void {
  if (attemptId === undefined) return;

  try {
    window.tsjs?.gptDiagnosticsRecorder?.recordTrustedServerCreativeFailure(attemptId, reason);
  } catch {
    // Diagnostics must not alter creative delivery.
  }
}

/** Maximum number of consumed APS Prebid IDs retained as security tombstones. */
const MAX_CONSUMED_PREBID_APS_IDS = 256;

function pruneConsumedPrebidApsIds(
  consumedIds: Map<string, { expiresAt: number }>,
  now: number
): void {
  for (const [adId, consumed] of consumedIds) {
    if (consumed.expiresAt <= now) consumedIds.delete(adId);
  }
}

function hasConsumedPrebidApsIdCapacity(
  consumedIds: Map<string, { expiresAt: number }>,
  adId: string
): boolean {
  if (consumedIds.has(adId) || consumedIds.size < MAX_CONSUMED_PREBID_APS_IDS) return true;

  log.warn(`[tsjs-gpt] APS Prebid renderer tombstone capacity reached; declining '${adId}'`);
  return false;
}

function recordConsumedPrebidApsId(
  consumedIds: Map<string, { expiresAt: number }>,
  adId: string,
  expiresAt: number
): void {
  consumedIds.set(adId, { expiresAt });
}

/**
 * Install the TS → pbRender bridge.
 *
 * Must be installed synchronously at module init — before `adInit()` fires
 * `refresh()`, which initiates the GAM request that may select the Prebid
 * creative. Installing post-load would miss first-impression `"Prebid Request"`
 * messages.
 *
 * When `adId` matches a TS server-side bid in `window.tsjs.bids` AND the bid
 * has renderable markup, the bridge:
 *   1. Uses the inline `adm` directly when present (the sanitized winning
 *      creative, now shipped in production), otherwise fetches from PBS Cache
 *      and extracts `adm` from the cached bid JSON (see `extractCachedAdm`).
 *   2. Replies via the MessageChannel port with a `"Prebid Response"`.
 *   3. Calls `stopImmediatePropagation()` so Prebid.js does not also process
 *      the message and log spurious failures.
 *
 * Shared by the early entry and GPT runtime, not Prebid, to avoid pulling the full
 * Prebid bundle into tsjs-gpt.js via inlineDynamicImports.
 */
export function installTsRenderBridge(): void {
  if (typeof window === 'undefined') return;
  const ts = (window.tsjs ??= {} as TsjsApi);
  if (ts.tsRenderBridgeVersion === 1) return;

  // `slotId|adId` renders whose PBS Cache fetch is in flight. `fireWinBillingBeacons`
  // only dedups after the async fetch resolves, so two Prebid Request messages for
  // the same render arriving before the first fetch settles would both fetch and
  // both fire the nurl/burl beacons. Tracking the in-flight render prevents the
  // concurrent double-fire; the entry is cleared once the fetch settles. The key
  // is scoped to the slot, not the bare adId: hb_adid is not unique per bid, so
  // keying on it alone would let one slot block a distinct slot's render.
  const renderingKeys = new Set<string>();
  const consumedPrebidApsIds = new Map<string, { expiresAt: number }>();
  // One consumed APS ad ID per slot is sufficient: a newer bid replaces the
  // slot's old ad ID in `window.tsjs.bids`, so the ownership guard rejects it.
  const consumedServerApsBySlot = new Map<string, string>();

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

    const now = Date.now();
    const generation = window.tsjs?.navGeneration ?? 0;
    pruneConsumedPrebidApsIds(consumedPrebidApsIds, now);
    const consumedPrebidAps = consumedPrebidApsIds.get(adId);
    if (consumedPrebidAps) {
      // Once TS claims an APS capability, keep the ad ID unavailable to every
      // other iframe. Letting Prebid's global handler answer a foreign source
      // would expose the creative despite the slot-bound capability check.
      e.stopImmediatePropagation();
      return;
    }

    const prebidRendererEntry = getApsPrebidRenderer(adId);
    if (prebidRendererEntry) {
      // Fail closed for a TS-owned APS ad ID before checking its source. Native
      // Prebid handles ad IDs globally and would otherwise answer a request from
      // an unrelated iframe when this slot-bound capability rejects it.
      e.stopImmediatePropagation();
      const sourceFrame = sourceFrameForAdUnit(e.source, prebidRendererEntry.adUnitCode);
      if (!sourceFrame) return;
      const renderer = validateApsRenderer(prebidRendererEntry.renderer);
      if (!renderer || !hasConsumedPrebidApsIdCapacity(consumedPrebidApsIds, adId)) return;
      if (!consumeApsPrebidRenderer(adId, prebidRendererEntry)) return;
      recordConsumedPrebidApsId(consumedPrebidApsIds, adId, prebidRendererEntry.expiresAt);

      const markUsed = (): void => {
        try {
          prebidRendererEntry.markUsed();
        } catch (err) {
          log.warn(`[tsjs-gpt] APS Prebid markUsed callback threw for '${adId}'`, err);
        }
      };
      const dispatched = dispatchApsRendering({
        slotId: prebidRendererEntry.adUnitCode,
        renderer,
        source: e.source,
        trustedServer: (validatedRenderer) => {
          const rendererUrl = apsRendererUrl();
          if (!rendererUrl) return false;
          const stillOwnsCreative = () =>
            sourceFrameForAdUnit(e.source, prebidRendererEntry.adUnitCode)?.iframe ===
            sourceFrame.iframe;
          if (!creativeFrameIsCurrent(e.source, sourceFrame, generation, stillOwnsCreative)) {
            return false;
          }
          try {
            port.postMessage(
              JSON.stringify({
                message: 'Prebid Response',
                adId,
                renderer: APS_UNIVERSAL_CREATIVE_RENDERER,
                rendererVersion: APS_UNIVERSAL_CREATIVE_RENDERER_VERSION,
                rendererUrl,
                apsRenderer: validatedRenderer,
                width: validatedRenderer.width,
                height: validatedRenderer.height,
              })
            );
            resizeCollapsedCreativeFrame(
              e.source,
              sourceFrame,
              validatedRenderer.width,
              validatedRenderer.height,
              generation,
              stillOwnsCreative
            );
            return creativeFrameIsCurrent(e.source, sourceFrame, generation, stillOwnsCreative);
          } catch (err) {
            log.warn(`[tsjs-gpt] APS Prebid response post failed for '${adId}'`, err);
            return false;
          }
        },
      });
      if (typeof dispatched === 'boolean') {
        if (dispatched) markUsed();
      } else {
        void dispatched.then((accepted) => {
          if (accepted) markUsed();
        });
      }
      return;
    }

    const sourceSlotFrame = slotFrameForMessageSource(e.source);
    if (!sourceSlotFrame) return;

    // Resolve the bid by the requesting slot, not by the first bid whose hb_adid
    // matches. hb_adid is not unique per bid: absent PBS Cache it falls back to a
    // creative id a bidder may reuse across slots, and only absent that too does it
    // fall back to the OpenRTB bid id, which is unique per bid instance. A
    // first-match-by-adId lookup would resolve every duplicate to one slot, so all
    // but that slot render blank.
    const bids = window.tsjs?.bids ?? {};
    const slotId = sourceSlotFrame.slotId;
    const matchedBid = bids[slotId];

    // Not a TS bid, or the requesting slot's bid does not own this adId — let
    // Prebid.js handle it. The adId guard also prevents an iframe under slot A from
    // pulling slot B's creative and firing slot B's win/billing beacons.
    if (
      !matchedBid ||
      matchedBid.hb_adid !== adId ||
      !registeredCandidateAllowed(window.tsjs!, slotId, matchedBid)
    )
      return;

    if (matchedBid.renderer !== undefined) {
      // This slot and ad ID belong to TS, so fail closed before validating the
      // descriptor and never let native Prebid answer a rejected or replayed request.
      e.stopImmediatePropagation();
      if (consumedServerApsBySlot.get(slotId) === adId) return;
      const renderer = validateApsRenderer(matchedBid.renderer);
      if (!renderer) return;
      consumedServerApsBySlot.set(slotId, adId);
      void Promise.resolve(
        dispatchApsRendering({
          slotId,
          renderer,
          source: e.source,
          trustedServer: (validatedRenderer) => {
            const rendererUrl = apsRendererUrl();
            if (!rendererUrl) return false;
            const stillOwnsCreative = () =>
              window.tsjs?.bids?.[slotId] === matchedBid &&
              matchedBid.hb_adid === adId &&
              sourceFrameForSlotId(e.source, slotId)?.iframe === sourceSlotFrame.iframe;
            if (!creativeFrameIsCurrent(e.source, sourceSlotFrame, generation, stillOwnsCreative)) {
              return false;
            }
            try {
              port.postMessage(
                JSON.stringify({
                  message: 'Prebid Response',
                  adId,
                  renderer: APS_UNIVERSAL_CREATIVE_RENDERER,
                  rendererVersion: APS_UNIVERSAL_CREATIVE_RENDERER_VERSION,
                  rendererUrl,
                  apsRenderer: validatedRenderer,
                  width: validatedRenderer.width,
                  height: validatedRenderer.height,
                })
              );
              resizeCollapsedCreativeFrame(
                e.source,
                sourceSlotFrame,
                validatedRenderer.width,
                validatedRenderer.height,
                generation,
                stillOwnsCreative
              );
              return creativeFrameIsCurrent(
                e.source,
                sourceSlotFrame,
                generation,
                stillOwnsCreative
              );
            } catch (err) {
              log.warn(`[tsjs-gpt] APS server response post failed for '${slotId}'`, err);
              return false;
            }
          },
        })
      );
      return;
    }

    const attemptId = safelyRecordCreativeRequest(slotId);

    const slot = window.tsjs?.adSlots?.find((s) => s.id === slotId);
    // Prefer the winning creative's own dimensions; the first configured slot
    // format is only a fallback and mis-sizes a multi-size slot whose winner is
    // not the first format.
    const [fallbackWidth, fallbackHeight] = slot?.formats?.[0] ?? [728, 90];
    const width = matchedBid.w ?? fallbackWidth;
    const height = matchedBid.h ?? fallbackHeight;
    const inlineAdm = isNonEmptyString(matchedBid.adm) ? matchedBid.adm : undefined;
    const cacheHost = isNonEmptyString(matchedBid.hb_cache_host)
      ? matchedBid.hb_cache_host
      : undefined;
    const cachePath = isNonEmptyString(matchedBid.hb_cache_path)
      ? matchedBid.hb_cache_path
      : undefined;

    if (inlineAdm) {
      e.stopImmediatePropagation();
      const stillOwnsCreative = () =>
        Boolean(
          window.tsjs?.bids?.[slotId] === matchedBid &&
          matchedBid.hb_adid === adId &&
          sourceFrameForSlotId(e.source, slotId)?.iframe === sourceSlotFrame.iframe
        );
      if (!creativeFrameIsCurrent(e.source, sourceSlotFrame, generation, stillOwnsCreative)) {
        return;
      }
      try {
        port.postMessage(
          JSON.stringify({
            message: 'Prebid Response',
            adId,
            ad: inlineAdm,
            renderer: TS_DISPLAY_RENDERER,
            width,
            height,
          })
        );
      } catch (err) {
        safelyRecordCreativeFailure(attemptId, 'response_post_failed');
        log.warn(`[tsjs-gpt] pbRender bridge: response post failed for '${slotId}'`, err);
        return;
      }
      resizeCollapsedCreativeFrame(
        e.source,
        sourceSlotFrame,
        width,
        height,
        generation,
        stillOwnsCreative
      );
      if (!creativeFrameIsCurrent(e.source, sourceSlotFrame, generation, stillOwnsCreative)) return;
      safelyRecordCreativeResponse(attemptId);
      fireWinBillingBeacons(slotId, matchedBid);
      log.debug(`[tsjs-gpt] pbRender bridge served '${slotId}' from inline adm`);
      return;
    }

    // No TS render source — let Prebid.js handle it.
    if (!cacheHost || !cachePath) {
      safelyRecordCreativeFailure(attemptId, 'missing_render_source');
      return;
    }

    // TS owns this adId — stop Prebid from also processing it.
    e.stopImmediatePropagation();

    // Skip a concurrent re-render of the same slot's adId so its win/billing
    // beacons fire at most once even before the first cache fetch resolves.
    const renderingKey = `${slotId}|${adId}`;
    if (renderingKeys.has(renderingKey)) return;
    renderingKeys.add(renderingKey);

    const cacheUrl = `https://${cacheHost}${cachePath}?uuid=${encodeURIComponent(adId)}`;

    const cachedBody = fetch(cacheUrl, { mode: 'cors' }).then((res) =>
      res.ok ? res.text() : Promise.reject(res.status)
    );

    cachedBody
      .then(
        (body) => {
          // PBS Cache returns the cached bid as a JSON object; decode its creative
          // and render metadata the same way the Prebid Universal Creative does.
          const cached = parseCachedBid(body);
          if (!cached) {
            // No renderable creative in the cache payload — decline rather than
            // ship a serialized bid document to PUC. Beacons stay unfired.
            safelyRecordCreativeFailure(attemptId, 'invalid_cache_payload');
            log.warn(
              `[tsjs-gpt] pbRender bridge: PBS Cache response for '${slotId}' had no renderable adm`
            );
            return;
          }
          // Resolve the auction-price macro from the cached clearing price, and
          // size from the cached bid's own dimensions, falling back to the slot
          // format only when the cache omits them.
          const ad =
            cached.price !== undefined
              ? expandAuctionPriceMacro(cached.adm, cached.price)
              : cached.adm;
          const cachedWidth = cached.width ?? width;
          const cachedHeight = cached.height ?? height;
          const stillOwnsCreative = () =>
            window.tsjs?.bids?.[slotId] === matchedBid &&
            matchedBid.hb_adid === adId &&
            sourceFrameForSlotId(e.source, slotId)?.iframe === sourceSlotFrame.iframe;
          if (!creativeFrameIsCurrent(e.source, sourceSlotFrame, generation, stillOwnsCreative)) {
            return;
          }
          try {
            port.postMessage(
              JSON.stringify({
                message: 'Prebid Response',
                adId,
                ad,
                renderer: TS_DISPLAY_RENDERER,
                width: cachedWidth,
                height: cachedHeight,
              })
            );
            resizeCollapsedCreativeFrame(
              e.source,
              sourceSlotFrame,
              cachedWidth,
              cachedHeight,
              generation,
              stillOwnsCreative
            );
          } catch (err) {
            safelyRecordCreativeFailure(attemptId, 'response_post_failed');
            log.warn(`[tsjs-gpt] pbRender bridge: response post failed for '${slotId}'`, err);
            return;
          }
          if (!creativeFrameIsCurrent(e.source, sourceSlotFrame, generation, stillOwnsCreative)) {
            return;
          }
          safelyRecordCreativeResponse(attemptId);
          // Beacons carry the server-expanded ${AUCTION_PRICE} from the auction's
          // clearing price, not `cached.price` — the auction result is the
          // authoritative clearing price, and the cached copy is only the render
          // source. Do not re-expand them here.
          fireWinBillingBeacons(slotId, matchedBid);
          log.debug(`[tsjs-gpt] pbRender bridge served '${slotId}' from PBS Cache`);
        },
        (err) => {
          safelyRecordCreativeFailure(attemptId, 'cache_fetch_failed');
          log.warn(`[tsjs-gpt] pbRender bridge: PBS Cache fetch failed for '${slotId}'`, err);
        }
      )
      .catch((err) => {
        // Errors reaching this stage were thrown after the cache body promise
        // settled, so parsing, posting follow-up work, beacons, success logging,
        // or failure reporting must not create new cache-failure evidence. Keep
        // the fire-and-forget bridge promise handled without changing evidence.
        try {
          log.warn(`[tsjs-gpt] pbRender bridge: response processing failed for '${slotId}'`, err);
        } catch {
          // Logging must not create an unhandled bridge rejection.
        }
      })
      .finally(() => {
        renderingKeys.delete(renderingKey);
      });
  });
  ts.tsRenderBridgeVersion = 1;
}
