import {
  claimFirstImpressionForTrustedServer,
  firstImpressionClaim,
  observeFirstImpressionGptLifecycle,
  publisherFirstImpressionRetryDelay,
  releaseTrustedServerFirstImpressionClaim,
  reservePublisherFirstImpressionFallback,
} from '../../core/first_impression';
import { log } from '../../core/log';
import { resolveSlotElementByDivId } from '../../core/slot_element';
import type {
  AuctionSlot,
  AuctionBidData,
  GptDiagnosticsTrustedServerOpportunity,
  GptSlotHandoff,
  TsjsApi,
} from '../../core/types';

import {
  installPublisherSlotRegistration,
  invalidatePublisherSlotRegistrations,
  observePublisherSlotRender,
  registeredCandidateAllowed,
  retainPublisherSlotMappings,
  settlePublisherSlotPayload,
} from './publisher_registration';
import type {
  GoogleTag,
  GoogleTagSlot,
  GoogleTagPubAdsService,
  GoogleTagDisplayTarget,
  GoogleTagRefreshOptions,
  GptWindow,
  SlotRenderEndedEvent,
} from './types';
import { installGptShim, installTrustedServerPageTargeting } from './shim';
import { findSlotElementByDivId, isNonEmptyString, installTsRenderBridge } from './render_bridge';
export { installGptShim } from './shim';
export { installTsRenderBridge, parseCachedBid } from './render_bridge';
export type { CachedBid } from './render_bridge';

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

function trustedServerOpportunity(bid: AuctionBidData): GptDiagnosticsTrustedServerOpportunity {
  const hasBidTargeting = TS_BID_TARGETING_KEYS.some((key) => isNonEmptyString(bid[key]));
  if (!hasBidTargeting) return 'no_candidate';

  const hasAdId = isNonEmptyString(bid.hb_adid);
  const hasInline = isNonEmptyString(bid.adm);
  const hasCache = isNonEmptyString(bid.hb_cache_host) && isNonEmptyString(bid.hb_cache_path);

  return hasAdId && (hasInline || hasCache) ? 'renderable_candidate' : 'unrenderable_candidate';
}

function clearTargetingKeys(slot: GoogleTagSlot, keys: Iterable<string>): void {
  if (typeof slot.clearTargeting !== 'function') return;

  for (const key of new Set(keys)) {
    slot.clearTargeting(key);
  }
}

// ------------------------------------------------------------------
// GAM interceptor (testing only)
// ------------------------------------------------------------------

/**
 * Sandbox token list for debug ADM fallback iframes.
 *
 * Deliberately excludes `allow-same-origin`: combined with `allow-scripts` on
 * srcdoc (or first-party src) content, that pair effectively removes the
 * sandbox's origin isolation and would let SSP-provided markup run with the
 * publisher origin's privileges.
 */
export const ADM_IFRAME_SANDBOX = 'allow-scripts allow-popups allow-forms';

/**
 * Resolve an ADM-extracted iframe src to a safe, loadable URL.
 *
 * Protocol-relative URLs are upgraded to https. Only http(s) URLs (including
 * page-relative paths, which resolve against the page origin) are accepted —
 * anything else (javascript:, data:, blob:, …) is rejected so SSP-provided
 * markup cannot smuggle a script-executing URL into the unsandboxed GAM
 * iframe.
 */
export function safeAdmIframeSrc(src: string): string | undefined {
  const resolved = src.startsWith('//') ? `https:${src}` : src;
  try {
    const parsed = new URL(resolved, window.location.href);
    if (parsed.protocol === 'https:' || parsed.protocol === 'http:') {
      return resolved;
    }
  } catch {
    // Unparseable URL — treat as unsafe.
  }
  return undefined;
}

/**
 * Replace the GAM-rendered creative with the server-side auction adm.
 *
 * Adapted from PR #241 (github.com/IABTechLab/trusted-server/pull/241).
 * Instead of reading from pbjs, reads adm directly from window.tsjs.bids.
 *
 * This is the testing-only direct-replace path that bypasses GAM entirely. The
 * sanitized `adm` now ships in production for the pbRender bridge, so `adm`
 * presence no longer gates it; the caller gates on the per-bid `debug_bid`
 * signal (present only under `inject_adm_for_testing`) instead.
 *
 * Strategy:
 * 1. If adm contains an <iframe src="..."> with a safe http(s) src, set that
 *    src on the GAM iframe directly — avoids cross-origin document access.
 * 2. Otherwise replace the slot element's content with a sandboxed srcdoc
 *    iframe (no `allow-same-origin` — see [ADM_IFRAME_SANDBOX]).
 */
function injectAdmIntoSlot(divId: string, adm: string): void {
  try {
    // divId may be the container div (used by GPT slot) or the inner div.
    // Resolve it the same way the rest of adInit does (exact then prefix) so
    // a config div_id prefix with a render-time suffix still finds the element.
    const slotEl = findSlotElementByDivId(divId);
    if (!slotEl) return;

    // Extract the first iframe src from the adm (e.g. mocktioneer creative
    // wraps a first-party proxy iframe in a div). Reject non-http(s) schemes.
    const srcMatch = adm.match(/<iframe[^>]+\bsrc=["']([^"']+)["']/i);
    const innerSrc = srcMatch?.[1] ? safeAdmIframeSrc(srcMatch[1]) : undefined;
    const gamIframe = slotEl.querySelector('iframe') as HTMLIFrameElement | null;

    if (innerSrc && gamIframe) {
      // Set the GAM iframe src — works even cross-origin (no document access needed).
      gamIframe.src = innerSrc;
      log.debug(`[tsjs-gpt] gam-intercept: set iframe src for '${divId}'`);
    } else if (innerSrc) {
      // GAM iframe not yet in DOM (APS renders async after slotRenderEnded).
      // Retry on next animation frame so APS has a tick to insert its iframe;
      // if it still isn't there, replace slot content directly.
      requestAnimationFrame(() => {
        const retryIframe = slotEl!.querySelector('iframe') as HTMLIFrameElement | null;
        if (retryIframe) {
          retryIframe.src = innerSrc;
          log.debug(`[tsjs-gpt] gam-intercept: set iframe src (retry) for '${divId}'`);
        } else {
          slotEl!.innerHTML = '';
          const f = document.createElement('iframe');
          f.style.cssText = 'border:none';
          f.width = String(slotEl!.offsetWidth || 728);
          f.height = String(slotEl!.offsetHeight || 90);
          f.setAttribute('sandbox', ADM_IFRAME_SANDBOX);
          f.src = innerSrc;
          slotEl!.appendChild(f);
          log.debug(`[tsjs-gpt] gam-intercept: inserted src iframe for '${divId}'`);
        }
      });
    } else {
      // No extractable safe src — replace slot content with a sandboxed srcdoc iframe.
      slotEl.innerHTML = '';
      const f = document.createElement('iframe');
      f.style.border = 'none';
      f.width = String(slotEl.offsetWidth || 728);
      f.height = String(slotEl.offsetHeight || 90);
      f.setAttribute('sandbox', ADM_IFRAME_SANDBOX);
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
 * targeting to GPT slots, sets the `ts_initial` sentinel, then calls refresh().
 * Win/billing beacons fire from the TS render bridge after a matching Prebid
 * Universal Creative request selects the TS bid and markup is successfully posted
 * to its MessagePort. Neither observation proves that PUC consumed the response or
 * that the creative rendered pixels.
 *
 * Idempotent: destroys previously created TS-managed slots before redefining them,
 * so it is safe to call again after SPA navigation updates `tsjs.adSlots`/`tsjs.bids`.
 */
/**
 * Track whether the publisher disabled GPT initial load.
 *
 * Read GPT's effective state through `getConfig()` when available and wrap both
 * configuration APIs so changes are synchronized immediately. The wrappers also
 * provide a fallback for runtimes where the getter is unavailable. With initial
 * load disabled, `display()` only registers a slot — the ad request
 * must come from a later `refresh()`. adInit() reads this to refresh its own
 * freshly defined slots so they are not left blank.
 *
 * Installed via the command queue so it runs before the publisher's own GPT
 * configuration (the TS core script is injected ahead of the publisher's GPT
 * setup). Idempotent per googletag object and pubads service.
 *
 * Only hooks an existing `googletag` stub — it never creates one. A plain module
 * import that does not activate the GPT integration must not touch
 * `window.googletag`. When the GPT shim is active it creates the stub before
 * `installTsAdInit` runs, so the detector is still queued ahead of the
 * publisher's GPT setup.
 */
function syncInitialLoadDisabled(gpt: Partial<GoogleTag>, ts: TsjsApi): boolean {
  if (typeof gpt.getConfig !== 'function') return false;

  const config = gpt.getConfig('disableInitialLoad');
  if (!config || config.disableInitialLoad === undefined) return false;

  ts.gptInitialLoadDisabled = config.disableInitialLoad === true;
  return true;
}

function installInitialLoadDetector(ts: TsjsApi): void {
  const win = window as GptWindow;
  const cmd = win.googletag?.cmd;
  if (!cmd) return;
  cmd.push(() => {
    const gpt = win.googletag as
      | (Partial<GoogleTag> & { __tsInitialLoadConfigHooked?: boolean })
      | undefined;
    if (!gpt) return;

    syncInitialLoadDisabled(gpt, ts);

    if (typeof gpt.setConfig === 'function' && !gpt.__tsInitialLoadConfigHooked) {
      const originalSetConfig = gpt.setConfig.bind(gpt);
      gpt.setConfig = function (...args: Parameters<typeof originalSetConfig>) {
        const config = args[0];
        const result = originalSetConfig(...args);
        if (!syncInitialLoadDisabled(gpt, ts) && config && 'disableInitialLoad' in config) {
          ts.gptInitialLoadDisabled = config.disableInitialLoad === true;
        }
        return result;
      };
      gpt.__tsInitialLoadConfigHooked = true;
    }

    const pubads = gpt.pubads?.();
    if (!pubads) return;
    const service = pubads as GoogleTagPubAdsService & { __tsInitialLoadHooked?: boolean };
    if (typeof service.disableInitialLoad !== 'function' || service.__tsInitialLoadHooked) {
      return;
    }
    const originalDisableInitialLoad = service.disableInitialLoad.bind(service);
    service.disableInitialLoad = function () {
      const result = originalDisableInitialLoad();
      if (!syncInitialLoadDisabled(gpt, ts)) {
        ts.gptInitialLoadDisabled = true;
      }
      return result;
    };
    service.__tsInitialLoadHooked = true;
  });
}

/**
 * Install `window.tsjs.scheduleInitialAdInit`.
 *
 * The server-injected `</body>` bids script calls this to run the initial
 * `adInit()` after React hydration instead of synchronously at body-parse
 * time. `adInit()` defines GPT slots on the publisher's `-container`
 * wrappers, mutating those ad-slot subtrees; on a Next.js App Router page a
 * synchronous call lands that mutation inside React's hydration window and
 * trips a #418 hydration mismatch. Deferral: gate on window `load` (the
 * client bundles that hydrate the tree have executed by then), then a double
 * `requestAnimationFrame` so the call runs after React has committed. A
 * single deferred call — no retry timer.
 *
 * The SSR document is navigation generation 0 by definition, so the scheduler
 * pins the whole initial pass to generation 0 rather than capturing whatever
 * the counter reads when the `</body>` script runs: the SPA hook is installed
 * by the synchronous head bundle, so a navigation can commit while the HTML
 * is still streaming, and capturing that advanced value would adopt the stale
 * SSR bootstrap as current. For the same reason the initial bids payload is
 * passed in and applied here, generation-guarded — assigning it
 * unconditionally at body end would clobber the live bids a faster SPA
 * navigation already applied.
 *
 * Shared-template seams pass `initialSlots`; inline documents omit them because
 * their head script already installed the slots. An explicit empty array clears
 * that state, while omission preserves it. The scheduler accepts only its first
 * generation-0 call so duplicate public API calls cannot define and display the
 * initial slots twice. The latch lives on `tsjs` so a bootstrap fallback that
 * claims the initial pass keeps that claim when the bundle replaces its
 * scheduler. If a navigation commits before scheduling or before the deferred
 * callback, the SSR payload and `adInit()` are both dropped. The generation
 * counter (not a URL comparison) keeps this aligned with the SPA auction hook's
 * navigation identity.
 *
 * Hidden documents: browsers do not service `requestAnimationFrame` while a
 * document is hidden, so a background-tab load (Cmd+click, open-in-new-tab)
 * holds the initial `adInit()` until the tab is first viewed. This is
 * intended, not an oversight: the initial ad request then spends its
 * impression on a tab someone is actually looking at instead of firing —
 * unviewable — at parse time in a tab that may never be foregrounded, and
 * riding rAF keeps a single code path whose post-hydration-commit guarantee
 * holds whenever the request is actually issued.
 */
function installScheduleInitialAdInit(ts: TsjsApi): void {
  ts.scheduleInitialAdInit = function (
    initialBids?: Record<string, AuctionBidData>,
    initialSlots?: AuctionSlot[]
  ) {
    if ((ts.navGeneration ?? 0) !== 0 || ts.initialAdInitScheduled) return;
    ts.initialAdInitScheduled = true;
    if (initialSlots !== undefined) ts.adSlots = initialSlots;
    if (initialBids !== undefined) ts.bids = initialBids;
    if (initialBids !== undefined || initialSlots !== undefined) {
      settlePublisherSlotPayload(ts, 0, ts.adSlots ?? [], ts.bids ?? {});
    }
    const runUnlessNavigated = (): void => {
      if ((ts.navGeneration ?? 0) !== 0) return;
      ts.adInit?.();
    };
    const afterHydrationFrames = (): void => {
      window.requestAnimationFrame(() => {
        window.requestAnimationFrame(runUnlessNavigated);
      });
    };
    if (document.readyState === 'complete') {
      afterHydrationFrames();
    } else {
      window.addEventListener('load', afterHydrationFrames, { once: true });
    }
  };
}

interface HandoffPatchedFunction {
  __tsSlotHandoffPatched?: boolean;
}

function findGptSlotByElementId(
  pubads: GoogleTagPubAdsService,
  elementId: string
): GoogleTagSlot | undefined {
  return pubads.getSlots?.().find((slot) => slot.getSlotElementId() === elementId);
}

function handoffForSlot(ts: TsjsApi, slot: GoogleTagSlot): GptSlotHandoff | undefined {
  return ts.gptSlotHandoffs?.[slot.getSlotElementId()];
}

function displayTargetElementId(target: GoogleTagDisplayTarget): string | undefined {
  if (typeof target === 'string') return target;
  if (typeof (target as GoogleTagSlot).getSlotElementId === 'function') {
    return (target as GoogleTagSlot).getSlotElementId();
  }
  return (target as Element).id || undefined;
}

function normalizedGptFormats(formats: Array<number | number[]>): Array<number | number[]> {
  return formats.length === 2 && formats.every((format) => typeof format === 'number')
    ? [formats as number[]]
    : formats;
}

function handoffFormatsMatch(handoff: GptSlotHandoff, formats: Array<number | number[]>): boolean {
  return JSON.stringify(handoff.formats) === JSON.stringify(normalizedGptFormats(formats));
}

function matchingHandoff(
  ts: TsjsApi,
  pubads: GoogleTagPubAdsService,
  adUnitPath: string,
  formats: Array<number | number[]>,
  elementId: string
): GptSlotHandoff | undefined {
  const exact = ts.gptSlotHandoffs?.[elementId];
  if (exact) return exact.publisherClaimed ? undefined : exact;

  const candidates = new Set(Object.values(ts.gptSlotHandoffs ?? {})).values();
  const matching = Array.from(candidates).filter(
    (handoff) =>
      !handoff.publisherClaimed &&
      !document.getElementById(handoff.slotElementId) &&
      elementId.startsWith(handoff.divIdPrefix) &&
      handoff.gamUnitPath === adUnitPath &&
      handoffFormatsMatch(handoff, formats) &&
      findGptSlotByElementId(pubads, handoff.slotElementId)
  );
  return matching.length === 1 ? matching[0] : undefined;
}

function registerHandoffAlias(ts: TsjsApi, elementId: string, handoff: GptSlotHandoff): void {
  (ts.gptSlotHandoffs ??= {})[elementId] = handoff;
}

function withGptSlotHandoffInternal<T>(ts: TsjsApi, callback: () => T): T {
  const wasInternal = ts.gptSlotHandoffInternal;
  ts.gptSlotHandoffInternal = true;
  try {
    return callback();
  } finally {
    ts.gptSlotHandoffInternal = wasInternal;
  }
}

/**
 * Reuse a TS-created inner-div slot when its publisher defines that div later.
 *
 * TS cannot wait an arbitrary amount of time for framework hydration: doing so
 * would leave placements blank when no publisher slot is ever defined. Instead,
 * TS creates its fallback on the publisher's actual div and aliases only a later
 * `defineSlot()` for that exact div, or for a hydration-renamed replacement after
 * the original div is gone. The first duplicate publisher request is suppressed
 * because TS has already issued the initial request with TS targeting.
 */
function installLatePublisherSlotHandoff(ts: TsjsApi): void {
  const win = window as GptWindow;
  const cmd = win.googletag?.cmd;
  if (!cmd) return;

  cmd.push(() => {
    const g = win.googletag;
    const pubads = g?.pubads?.();
    if (!g?.defineSlot || !g.display || !pubads) return;

    const defineSlot = g.defineSlot;
    if (!(defineSlot as HandoffPatchedFunction).__tsSlotHandoffPatched) {
      const originalDefineSlot = defineSlot.bind(g);
      const patchedDefineSlot = (
        adUnitPath: string,
        formats: Array<number | number[]>,
        elementId?: string
      ): GoogleTagSlot | null => {
        if (!ts.gptSlotHandoffInternal && typeof elementId === 'string') {
          const handoff = matchingHandoff(ts, pubads, adUnitPath, formats, elementId);
          if (handoff) {
            const existingSlot = findGptSlotByElementId(pubads, handoff.slotElementId);
            if (existingSlot) {
              registerHandoffAlias(ts, elementId, handoff);
              handoff.publisherClaimed = true;
              // The supported publisher lifecycle is defineSlot → addService → display.
              // Intentionally wait for that display instead of applying a time heuristic.
              handoff.suppressPublisherDisplay = true;
              handoff.suppressPublisherRefresh = ts.gptInitialLoadDisabled === true;
              ts.prevGptSlots = (ts.prevGptSlots ?? []).filter(
                (ownedSlot) => ownedSlot !== existingSlot
              );
              if (handoff.gamUnitPath !== adUnitPath || !handoffFormatsMatch(handoff, formats)) {
                log.warn('GPT slot handoff: publisher definition differs from TS configuration', {
                  elementId,
                  tsGamUnitPath: handoff.gamUnitPath,
                  publisherGamUnitPath: adUnitPath,
                });
              }
              return existingSlot;
            }
          }
        }
        return elementId === undefined
          ? originalDefineSlot(adUnitPath, formats)
          : originalDefineSlot(adUnitPath, formats, elementId);
      };
      (patchedDefineSlot as HandoffPatchedFunction).__tsSlotHandoffPatched = true;
      g.defineSlot = patchedDefineSlot;
    }

    const display = g.display;
    if (!(display as HandoffPatchedFunction).__tsSlotHandoffPatched) {
      const originalDisplay = display.bind(g);
      const patchedDisplay = (target: GoogleTagDisplayTarget): void => {
        const elementId = displayTargetElementId(target);
        const handoff = elementId ? ts.gptSlotHandoffs?.[elementId] : undefined;
        if (!ts.gptSlotHandoffInternal && handoff?.suppressPublisherDisplay) {
          handoff.suppressPublisherDisplay = false;
          return;
        }
        originalDisplay(target);
      };
      (patchedDisplay as HandoffPatchedFunction).__tsSlotHandoffPatched = true;
      g.display = patchedDisplay;
    }

    const refresh = pubads.refresh;
    if (!(refresh as HandoffPatchedFunction).__tsSlotHandoffPatched) {
      const originalRefresh = refresh.bind(pubads);
      const callRefresh = (
        slots: GoogleTagSlot[] | undefined,
        options: GoogleTagRefreshOptions | undefined
      ): void => {
        if (options === undefined) {
          originalRefresh(slots);
        } else {
          originalRefresh(slots, options);
        }
      };
      const patchedRefresh = (
        requestedSlots?: GoogleTagSlot[],
        options?: GoogleTagRefreshOptions
      ): void => {
        if (ts.gptSlotHandoffInternal) {
          callRefresh(requestedSlots, options);
          return;
        }

        const slots = requestedSlots ?? pubads.getSlots?.();
        if (!slots) {
          callRefresh(requestedSlots, options);
          return;
        }

        let suppressed = false;
        const remainingSlots = slots.filter((slot) => {
          const handoff = handoffForSlot(ts, slot);
          if (!handoff?.suppressPublisherRefresh) return true;
          handoff.suppressPublisherRefresh = false;
          suppressed = true;
          return false;
        });
        if (!suppressed) {
          callRefresh(requestedSlots, options);
        } else if (remainingSlots.length > 0) {
          callRefresh(remainingSlots, options);
        }
      };
      (patchedRefresh as HandoffPatchedFunction).__tsSlotHandoffPatched = true;
      pubads.refresh = patchedRefresh;
    }
  });
}

function installFirstImpressionLifecycleObservers(ts: TsjsApi, g: Partial<GoogleTag>): void {
  if (ts.firstImpressionListenersInstalled) return;
  g.cmd?.push(() => {
    if (ts.firstImpressionListenersInstalled) return;
    const pubads = g.pubads?.();
    if (!pubads?.addEventListener) return;

    const observe =
      (phase: 'requested' | 'rendered') =>
      (event: SlotRenderEndedEvent): void => {
        const elementId = event.slot?.getSlotElementId?.();
        const element = elementId ? document.getElementById(elementId) : null;
        if (element) observeFirstImpressionGptLifecycle(ts, element, phase);
        if (phase === 'rendered') observePublisherSlotRender(ts, event.slot);
      };
    pubads.addEventListener('slotRequested', observe('requested'));
    pubads.addEventListener('slotRenderEnded', observe('rendered'));
    ts.firstImpressionListenersInstalled = true;
  });
}

function trustedServerTargeting(
  slot: AuctionSlot,
  bid: AuctionBidData
): Record<string, string | string[]> {
  const targeting: Record<string, string | string[]> = { ...(slot.targeting ?? {}) };
  for (const key of TS_BID_TARGETING_KEYS) {
    if (bid[key]) targeting[key] = String(bid[key]);
  }
  targeting[TS_INITIAL_TARGETING_KEY] = '1';
  return targeting;
}

function applyTrustedServerTargeting(
  ts: TsjsApi,
  gptSlot: GoogleTagSlot,
  slot: AuctionSlot,
  bid: AuctionBidData,
  elementIds: readonly string[]
): string[] {
  const previousKeys = ts.prevSlotTargetingKeys ?? {};
  clearTargetingKeys(gptSlot, [
    ...TS_BASE_TARGETING_KEYS,
    ...elementIds.flatMap((elementId) => previousKeys[elementId] ?? []),
  ]);
  const targeting = trustedServerTargeting(slot, bid);
  for (const [key, value] of Object.entries(targeting)) gptSlot.setTargeting(key, value);
  const element = document.getElementById(elementIds[0]!);
  const claim = element ? firstImpressionClaim(ts, element) : undefined;
  if (claim?.owner === 'trusted_server') claim.targeting = targeting;
  return Object.keys(slot.targeting ?? {});
}

function clearPreviousTargetingSnapshot(
  g: Partial<GoogleTag>,
  snapshot: ReadonlyMap<GoogleTagSlot, readonly string[]>
): boolean {
  const pubads = g.pubads?.();
  if (!pubads) return false;
  const liveSlots = pubads.getSlots?.() ?? [];
  for (const [slot, keys] of snapshot) {
    if (!liveSlots.includes(slot)) continue;
    clearTargetingKeys(slot, keys);
  }
  return true;
}

function clearPreviousNavigationTargeting(ts: TsjsApi, g: Partial<GoogleTag>): void {
  const previousKeys: Record<string, string[]> = Object.assign(
    Object.create(null),
    ts.prevSlotTargetingKeys
  );
  const registeredRecords = Object.values(ts.publisherSlotRegistrationState?.records ?? {}).filter(
    (record) => record.consumed && record.targetingKeys
  );
  for (const record of registeredRecords) previousKeys[record.elementId] = record.targetingKeys!;
  const touchedElementIds = new Set([
    ...Object.keys(previousKeys),
    ...Object.keys(ts.divToSlotId ?? {}),
  ]);

  // Capture exact old native identities before retiring records or advancing navigation.
  const snapshot = new Map<GoogleTagSlot, readonly string[]>();
  const registeredElementIds = new Set(registeredRecords.map((record) => record.elementId));
  for (const record of registeredRecords) {
    if (record.gptSlot) {
      snapshot.set(record.gptSlot as GoogleTagSlot, [
        ...TS_BASE_TARGETING_KEYS,
        ...record.targetingKeys!,
      ]);
    }
  }
  for (const slot of [
    ...(g.pubads?.().getSlots?.() ?? []),
    ...((ts.prevGptSlots ?? []) as GoogleTagSlot[]),
  ]) {
    const elementId = slot.getSlotElementId();
    if (touchedElementIds.has(elementId) && !registeredElementIds.has(elementId)) {
      snapshot.set(slot, [...TS_BASE_TARGETING_KEYS, ...(previousKeys[elementId] ?? [])]);
    }
  }
  if (snapshot.size > 0 && !clearPreviousTargetingSnapshot(g, snapshot)) {
    g.cmd?.push(() => {
      clearPreviousTargetingSnapshot(g, snapshot);
    });
  }

  ts.prevSlotTargetingKeys = Object.create(null) as Record<string, string[]>;
  ts.divToSlotId = Object.create(null) as Record<string, string>;
}

function schedulePublisherFirstImpressionFallback(
  ts: TsjsApi,
  g: Partial<GoogleTag>,
  slot: AuctionSlot,
  bid: AuctionBidData,
  element: HTMLElement,
  generation: number
): void {
  if (!reservePublisherFirstImpressionFallback(ts, element)) return;

  const retry = (): void => {
    if (
      (ts.navGeneration ?? 0) !== generation ||
      !element.isConnected ||
      document.getElementById(element.id) !== element
    ) {
      return;
    }
    const delay = publisherFirstImpressionRetryDelay(ts, element);
    if (delay === undefined) return;
    if (delay > 0) {
      window.setTimeout(retry, delay + 1);
      return;
    }

    g.cmd?.push(() => {
      if (
        (ts.navGeneration ?? 0) !== generation ||
        !element.isConnected ||
        document.getElementById(element.id) !== element
      ) {
        return;
      }
      const claim = claimFirstImpressionForTrustedServer(ts, element);
      if (!claim) return;

      const pubads = g.pubads?.();
      if (!pubads) {
        releaseTrustedServerFirstImpressionClaim(ts, element, claim);
        return;
      }
      let gptSlot = pubads
        .getSlots?.()
        .find((candidate) => candidate.getSlotElementId() === element.id);
      let tsOwned = false;
      if (!gptSlot) {
        gptSlot =
          withGptSlotHandoffInternal(ts, () =>
            g.defineSlot?.(slot.gam_unit_path, slot.formats, element.id)
          ) ?? undefined;
        if (!gptSlot) {
          releaseTrustedServerFirstImpressionClaim(ts, element, claim);
          return;
        }
        gptSlot.addService(pubads);
        tsOwned = true;
        (ts.gptSlotHandoffs ??= {})[element.id] = {
          gamUnitPath: slot.gam_unit_path,
          formats: slot.formats,
          divIdPrefix: slot.div_id,
          slotElementId: element.id,
          publisherClaimed: false,
          suppressPublisherDisplay: false,
          suppressPublisherRefresh: false,
        };
      }

      const slotElementId = gptSlot.getSlotElementId?.() ?? element.id;
      const targetingKeys = applyTrustedServerTargeting(ts, gptSlot, slot, bid, [
        element.id,
        slotElementId,
      ]);
      (ts.divToSlotId ??= {})[element.id] = slot.id;
      if (slotElementId !== element.id) ts.divToSlotId[slotElementId] = slot.id;
      (ts.prevSlotTargetingKeys ??= {})[element.id] = targetingKeys;
      if (slotElementId !== element.id) ts.prevSlotTargetingKeys[slotElementId] = targetingKeys;
      if (tsOwned) (ts.prevGptSlots ??= []).push(gptSlot);

      try {
        ts.gptDiagnosticsRecorder?.recordTrustedServerOpportunity(
          gptSlot,
          slot.id,
          trustedServerOpportunity(bid),
          bid.hb_auction_id,
          slot.formats
        );
      } catch {
        // Diagnostics must not alter fallback delivery.
      }

      if (!ts.servicesEnabled) {
        pubads.enableSingleRequest();
        g.enableServices?.();
        ts.servicesEnabled = true;
      }
      if (tsOwned) withGptSlotHandoffInternal(ts, () => g.display?.(slotElementId));
      syncInitialLoadDisabled(g, ts);
      if (!tsOwned || ts.gptInitialLoadDisabled) {
        ts.adInitRefreshInProgress = true;
        try {
          withGptSlotHandoffInternal(ts, () => pubads.refresh([gptSlot!]));
        } finally {
          ts.adInitRefreshInProgress = false;
        }
      }
    });
  };

  retry();
}

export function installTsAdInit(): void {
  const ts = (window.tsjs ??= {} as TsjsApi);
  installInitialLoadDetector(ts);
  installPublisherSlotRegistration(ts);
  installScheduleInitialAdInit(ts);

  const g = (window as GptWindow).googletag;
  if (g) installFirstImpressionLifecycleObservers(ts, g);
  installLatePublisherSlotHandoff(ts);
  ts.adInit = function () {
    const slots = (ts.adSlots ?? []).filter(
      (slot) => slot.initialization !== 'publisher_registered'
    );
    // Snapshot bids at adInit() call time — correct for targeting setup.
    // The slotRenderEnded listener below reads ts.bids live so SPA navigation
    // updates (new ts.bids injected before </body>) are picked up at render time.
    const bids = ts.bids ?? {};
    // Generation this invocation belongs to. The destructive slot work below
    // is queued on googletag.cmd, which only drains when GPT itself loads —
    // possibly much later (e.g. consent-gated GPT). A navigation can commit
    // in that gap, so the queued callback rechecks the generation as its
    // first act and stands down rather than applying this invocation's
    // slots/bids to the newer route's DOM and double-requesting it.
    const generation = ts.navGeneration ?? 0;
    const g = (window as GptWindow).googletag;
    if (!g) return;
    installFirstImpressionLifecycleObservers(ts, g);
    const warnedResolutionFailures = new Set<string>();

    g.cmd?.push(() => {
      if ((ts.navGeneration ?? 0) !== generation) return;
      // Destroy previously defined TS slots before redefining for the new page.
      if (ts.prevGptSlots && ts.prevGptSlots.length > 0) {
        const destroyedSlotElementIds = new Set(
          (ts.prevGptSlots as GoogleTagSlot[]).map((slot) => slot.getSlotElementId())
        );
        g.destroySlots?.(ts.prevGptSlots as GoogleTagSlot[]);
        if (ts.gptSlotHandoffs) {
          for (const [elementId, handoff] of Object.entries(ts.gptSlotHandoffs)) {
            if (destroyedSlotElementIds.has(handoff.slotElementId)) {
              delete ts.gptSlotHandoffs[elementId];
            }
          }
        }
        ts.prevGptSlots = [];
      }

      // Slots TS defined itself — tracked for SPA destroy. Publisher-owned
      // slots are reused but never destroyed by TS on navigation.
      const newSlots: GoogleTagSlot[] = [];
      // Publisher-owned slots TS reused — refreshed to pick up server-side
      // targeting. The publisher already display()ed these.
      const slotsToRefresh: GoogleTagSlot[] = [];
      // Element IDs of slots TS defined itself this call. GPT requires a
      // display() call to register/render a freshly-defined slot; refresh()
      // alone no-ops for a slot that was never displayed, so these are
      // display()ed instead of refreshed.
      const slotsToDisplay: string[] = [];
      const divToSlotId: Record<string, string> = Object.create(null);
      const prevSlotTargetingKeys = ts.prevSlotTargetingKeys ?? {};
      const nextSlotTargetingKeys: Record<string, string[]> = Object.create(null);

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
      const registeredElementIds = new Set(
        Object.values(ts.publisherSlotRegistrationState?.records ?? {})
          .filter((record) => record.generation === generation && record.consumed)
          .map((record) => record.elementId)
      );
      if (prevTouchedDivIds.size > 0) {
        (g.pubads!().getSlots?.() ?? []).forEach((gptSlot: GoogleTagSlot) => {
          const elementId = gptSlot.getSlotElementId();
          if (!prevTouchedDivIds.has(elementId) || registeredElementIds.has(elementId)) return;
          const element = document.getElementById(elementId);
          if (element && firstImpressionClaim(ts, element)) return;
          clearTargetingKeys(gptSlot, [
            ...TS_BASE_TARGETING_KEYS,
            ...(prevSlotTargetingKeys[elementId] ?? []),
          ]);
        });
      }

      slots.forEach((slot) => {
        // Resolve actual div ID: exact match first, then the visibility and
        // geometry tiers for prefix matches. div_id in config may be a stable
        // prefix (e.g. "ad-header-0-") when the suffix is dynamically
        // generated by the framework at render time.
        const resolution = resolveSlotElementByDivId(slot.div_id);
        const el = resolution.element;
        if (!el) {
          if (!warnedResolutionFailures.has(slot.div_id)) {
            if (resolution.prefixMatchCount > 1) {
              warnedResolutionFailures.add(slot.div_id);
              log.warn('GPT slot prefix did not resolve to one active element', {
                divId: slot.div_id,
                prefixMatchCount: resolution.prefixMatchCount,
                activeMatchCount: resolution.activeMatchCount,
              });
            } else if (resolution.prefixMatchCount === 1 && resolution.activeMatchCount === 0) {
              // The common breakpoint-hidden config: the prefix matched one
              // element but it is hidden, so the slot is skipped. Logged so a
              // blank placement is diagnosable without stepping the resolver.
              warnedResolutionFailures.add(slot.div_id);
              log.debug('GPT slot prefix matched only a hidden element; skipping slot', {
                divId: slot.div_id,
              });
            }
          }
          return;
        }
        const actualDivId = el.id;
        const bid = bids[slot.id] ?? {};
        const firstImpression = claimFirstImpressionForTrustedServer(ts, el);
        if (!firstImpression) {
          const claim = firstImpressionClaim(ts, el);
          if (claim?.owner === 'publisher') {
            schedulePublisherFirstImpressionFallback(ts, g, slot, bid, el, generation);
          }
          return;
        }

        const existingSlot = g.pubads!()
          .getSlots?.()
          ?.find?.((s: GoogleTagSlot) => s.getSlotElementId() === actualDivId);
        let gptSlot: GoogleTagSlot;
        let tsOwned = false;
        if (existingSlot) {
          gptSlot = existingSlot;
        } else {
          // Define TS's fallback on the publisher's actual div. A late publisher
          // defineSlot() for this div is handed the same slot by the scoped GPT
          // wrapper, preventing a competing container-slot request.
          const defined = withGptSlotHandoffInternal(ts, () =>
            g.defineSlot?.(slot.gam_unit_path, slot.formats, actualDivId)
          );
          if (!defined) {
            releaseTrustedServerFirstImpressionClaim(ts, el, firstImpression);
            return;
          }
          defined.addService(g.pubads!());
          gptSlot = defined;
          tsOwned = true;
          (ts.gptSlotHandoffs ??= {})[actualDivId] = {
            gamUnitPath: slot.gam_unit_path,
            formats: slot.formats,
            divIdPrefix: slot.div_id,
            slotElementId: actualDivId,
            publisherClaimed: false,
            suppressPublisherDisplay: false,
            suppressPublisherRefresh: false,
          };
        }

        const slotDivId2 = gptSlot.getSlotElementId?.() ?? actualDivId;
        const slotTargetingKeys = applyTrustedServerTargeting(ts, gptSlot, slot, bid, [
          actualDivId,
          slotDivId2,
        ]);
        // Diagnostics are observational only. A missing or malformed debug
        // implementation must never interrupt slot mapping or delivery.
        try {
          const requestedSlotSizes = ts.gptSlotHandoffs?.[slotDivId2]?.formats;
          const opportunity = trustedServerOpportunity(bid);
          ts.gptDiagnosticsRecorder?.recordTrustedServerOpportunity(
            gptSlot,
            slot.id,
            opportunity,
            bid.hb_auction_id,
            requestedSlotSizes
          );
        } catch {
          // Diagnostics must not alter ad delivery.
        }
        // Map the resolved inner div to the slot ID so slotRenderEnded and ADM
        // injection address the same, single GPT slot.
        divToSlotId[actualDivId] = slot.id;
        if (slotDivId2 !== actualDivId) divToSlotId[slotDivId2] = slot.id;
        nextSlotTargetingKeys[actualDivId] = slotTargetingKeys;
        if (slotDivId2 !== actualDivId) nextSlotTargetingKeys[slotDivId2] = slotTargetingKeys;
        if (tsOwned) {
          newSlots.push(gptSlot);
          slotsToDisplay.push(slotDivId2);
        } else {
          slotsToRefresh.push(gptSlot);
        }

        // Trusted Server APS winners carry their own typed renderer and never
        // enter the publisher-owned native apstag rendering path.
      });

      ts.prevGptSlots = newSlots as unknown[];
      // Replace automatic entries, deriving registered entries from their consumed records.
      retainPublisherSlotMappings(ts, divToSlotId, nextSlotTargetingKeys);
      ts.divToSlotId = divToSlotId;
      ts.prevSlotTargetingKeys = nextSlotTargetingKeys;

      // Whether this call produced any TS slot to render. A gated page-bids
      // response (template switch, auction gate, or consent denial) returns no
      // slots, so the loops above leave these empty.
      const hasRenderableWork = slotsToDisplay.length > 0 || slotsToRefresh.length > 0;

      // enableSingleRequest and enableServices must only be called once per page
      // load. Skip activating GPT services when TS has nothing to display or
      // refresh and has not already enabled them: a consent-denied or
      // kill-switched navigation must not turn on the publisher's GPT services
      // or race their own setup. The targeting sweep above still runs so stale
      // TS targeting from a prior navigation is cleared.
      if (!ts.servicesEnabled && hasRenderableWork) {
        g.pubads!().enableSingleRequest();
        g.enableServices?.();
        ts.servicesEnabled = true;

        g.pubads!().addEventListener?.('slotRenderEnded', (event: SlotRenderEndedEvent) => {
          const divId: string = event.slot?.getSlotElementId?.() ?? '';
          const slotId = (ts.divToSlotId ?? {})[divId];
          if (!slotId) return;
          // Read ts.bids live (not the snapshot above) so post-navigation bid data is used.
          const bid = (ts.bids ?? {})[slotId] ?? {};

          // GAM interceptor (testing bypass): directly replace the GAM creative.
          // `adm` is now always injected in production, so it can no longer gate
          // this path. `debug_bid` is present only when inject_adm_for_testing is
          // on, so it is the per-bid signal that the testing bypass is enabled.
          // In production the render bridge serves the creative and GAM stays in
          // the loop; this direct replace stays testing-only.
          if (bid.adm && bid.debug_bid && registeredCandidateAllowed(ts, slotId, bid)) {
            injectAdmIntoSlot(divId, bid.adm);
          }
        });
      }

      // Register and render TS-defined slots. GPT requires display() for a
      // freshly-defined slot — without it the slot no-ops ("defineSlot was
      // called without a matching display call") and misses its impression.
      // Must run after enableServices(); on SPA navigation services are already
      // enabled, so this runs unconditionally for any newly-defined slots.
      slotsToDisplay.forEach((divId) => withGptSlotHandoffInternal(ts, () => g.display?.(divId)));

      syncInitialLoadDisabled(g, ts);
      // Slots needing an explicit ad request via refresh(). Reused
      // publisher-owned slots always need one to pick up the just-applied
      // server-side targeting. TS-defined slots are normally fetched by the
      // display() above — but when the publisher called
      // pubads().disableInitialLoad(), display() only registers the slot and the
      // ad request must come from refresh(). Without this, a TS-owned
      // first-impression slot renders blank on initial-load-disabled pages. Only
      // add them in that case; otherwise display() + refresh() would
      // double-request the impression.
      const slotsNeedingRefresh = ts.gptInitialLoadDisabled
        ? slotsToRefresh.concat(newSlots)
        : slotsToRefresh;

      if (slotsNeedingRefresh.length > 0) {
        // One-shot bypass: this internal refresh delivers the just-applied
        // server-side targeting to GAM. If slim-Prebid has wrapped refresh(), it
        // must pass this call straight through — not clear the targeting and run
        // a duplicate client-side auction. Later publisher-initiated refreshes of
        // the same slots still go through the wrapper normally.
        ts.adInitRefreshInProgress = true;
        try {
          withGptSlotHandoffInternal(ts, () => g.pubads!().refresh(slotsNeedingRefresh));
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

/** Canonical SPA re-auction endpoint. Mirrors `PAGE_BIDS_PATH` in Rust. */
const PAGE_BIDS_PATH = '/_ts/page-bids';

/**
 * Deprecated alias of {@link PAGE_BIDS_PATH}, kept registered server-side so
 * pre-rename bundles keep working. This bundle falls back to it when the
 * canonical path does not serve page-bids: a server rolled back to before the
 * rename does not register the canonical path, and an operator `[[handlers]]`
 * auth regex broad enough to cover `/_ts` answers it with `401` that no
 * anonymous browser fetch can satisfy. Without the fallback either case
 * silently drops ads on every SPA navigation.
 *
 * Removed together with the server-side alias in IABTechLab/trusted-server#970.
 */
const PAGE_BIDS_LEGACY_PATH = '/__ts/page-bids';

/**
 * `X-TSJS-Page-Bids` value sent on a fallback request, so the server can tell
 * a current bundle that could not use the canonical path from a pre-rename
 * bundle that only knows the alias. Only the former signals a deployment that
 * needs fixing. Mirrors `PAGE_BIDS_FALLBACK_MARKER` in Rust.
 */
const PAGE_BIDS_FALLBACK_MARKER = 'fallback';

/** Outcome of one page-bids request against a specific endpoint path. */
interface PageBidsAttempt {
  /** Parsed payload, or `null` when this endpoint did not serve one. */
  data: PageBidsResponse | null;
  /**
   * The response says this path does not serve page-bids on this deployment,
   * so the other registered path is worth trying. Transient failures and the
   * endpoint's own cross-site denial apply equally to both paths and do not
   * set this.
   */
  wrongEndpoint: boolean;
}

async function fetchPageBids(
  endpoint: string,
  path: string,
  signal: AbortSignal
): Promise<PageBidsAttempt> {
  const res = await fetch(`${endpoint}?path=${encodeURIComponent(path)}`, {
    credentials: 'include',
    // Non-simple header doubles as a CSRF token: the server rejects
    // requests that carry neither same-origin Fetch Metadata nor this
    // header, and cross-origin pages cannot send it without a CORS
    // preflight the endpoint never grants. The server checks presence, not
    // value, so the value carries the fallback diagnostic.
    headers: {
      'X-TSJS-Page-Bids': endpoint === PAGE_BIDS_LEGACY_PATH ? PAGE_BIDS_FALLBACK_MARKER : '1',
    },
    signal,
  });
  if (!res.ok) {
    // 401: an operator auth handler regex covers this path. 404: this server
    // does not know the route. Either way the other path may still answer.
    // 403 (cross-site gate) and 5xx would repeat on both, so they do not.
    return { data: null, wrongEndpoint: res.status === 401 || res.status === 404 };
  }
  try {
    return { data: (await res.json()) as PageBidsResponse, wrongEndpoint: false };
  } catch (err) {
    if (err instanceof DOMException && err.name === 'AbortError') throw err;
    // A server with no route for this path proxies it to the publisher origin,
    // which answers 200 HTML. An unparseable body is the wrong endpoint, not a
    // transient failure.
    return { data: null, wrongEndpoint: true };
  }
}

/**
 * Upper bound (ms) on how long the SPA hook waits for a route's ad containers
 * to appear before applying bids anyway.
 */
const SPA_SLOT_WAIT_MS = 2000;

/**
 * Resolve once every configured `slots` entry has a container element in the DOM, or
 * after `SPA_SLOT_WAIT_MS`, whichever comes first.
 *
 * Many SPA routers update `history` before the new route's markup commits. If
 * bids were applied immediately, `adInit()` would look up each slot element
 * once and silently skip every not-yet-rendered slot, dropping that route's
 * server-side bids with no retry. Waiting via `MutationObserver` lets the apply
 * step run as soon as the route's full slot set exists; the timeout guarantees
 * a slot that never renders cannot hang the hook (the subsequent `adInit()`
 * skips missing elements exactly as before). Resolves immediately when there is
 * nothing to wait for, or when `MutationObserver` is unavailable.
 */
function waitForSlotElements(slots: AuctionSlot[], signal: AbortSignal): Promise<void> {
  // A newer navigation may have aborted this signal before we were called; skip
  // installing an observer/timer that the stale run would only tear down.
  if (signal.aborted) return Promise.resolve();
  // Presence and eligibility are different questions here. The tiered
  // resolver returns no element for a prefix match that is hidden (e.g. a
  // breakpoint-hidden mobile-only placement), but such a slot has rendered and
  // will never "appear" — waiting on it would stall every slot on the route
  // for the full timeout. Count it as present; adInit still applies the strict
  // tiers when it runs and skips ineligible slots.
  const allPresent = (): boolean =>
    slots.every((slot) => {
      const resolution = resolveSlotElementByDivId(slot.div_id);
      return resolution.element !== null || resolution.prefixMatchCount > 0;
    });
  if (slots.length === 0 || allPresent() || typeof MutationObserver === 'undefined') {
    return Promise.resolve();
  }

  return new Promise<void>((resolve) => {
    let settled = false;
    let animationFrame: number | undefined;
    const finish = (): void => {
      if (settled) return;
      settled = true;
      if (animationFrame !== undefined) cancelAnimationFrame(animationFrame);
      observer.disconnect();
      clearTimeout(timer);
      signal.removeEventListener('abort', finish);
      resolve();
    };
    const observer = new MutationObserver(() => {
      if (document.visibilityState === 'hidden' || typeof requestAnimationFrame === 'undefined') {
        if (animationFrame !== undefined) {
          cancelAnimationFrame(animationFrame);
          animationFrame = undefined;
        }
        if (allPresent()) finish();
        return;
      }
      if (animationFrame !== undefined) return;
      animationFrame = requestAnimationFrame(() => {
        animationFrame = undefined;
        if (allPresent()) finish();
      });
    });
    observer.observe(document.documentElement, { childList: true, subtree: true });
    const timer = setTimeout(finish, SPA_SLOT_WAIT_MS);
    signal.addEventListener('abort', finish);
  });
}

/** Reuse one history observer while transferring its active navigation handler. */
function installGptNavigationBridge(ts: TsjsApi, onNavigate: (path: string) => void): void {
  if (ts.gptNavigationBridge) {
    ts.gptNavigationBridge.onNavigate = onNavigate;
    return;
  }
  ts.gptNavigationBridge = { onNavigate };
  const observe = (): void => ts.gptNavigationBridge?.onNavigate(location.pathname);
  for (const method of ['pushState', 'replaceState'] as const) {
    const original = history[method].bind(history);
    history[method] = function (state: unknown, unused: string, url?: string | URL | null): void {
      original(state, unused, url);
      observe();
    };
  }
  window.addEventListener('popstate', observe, true);
}

/**
 * Install SPA navigation and adopt a provisional bootstrap transition, if any.
 *
 * Accepted pathname changes and current-generation catch-up share the existing
 * page-bids operation. Installing without a navigation does not re-auction SSR.
 * The shared bridge preserves framework wrappers and one navigation writer.
 */
export function installSpaAuctionHook(): void {
  if (typeof window === 'undefined') return;
  const ts = (window.tsjs ??= {} as TsjsApi);
  if (ts.spaHookInstalled) return;
  ts.spaHookInstalled = true;
  // Navigation identity for the deferred initial-adInit bootstrap (see
  // installScheduleInitialAdInit). Initialized here, and incremented
  // synchronously in onNavigate the moment a route change is accepted, so the
  // counter can never lag the auction decision. Deliberately NOT rolled back
  // when a navigation later fails: the framework has already swapped the
  // route's DOM by then, so a pending initial callback must still stand down.
  ts.navGeneration ??= 0;

  const pendingGeneration = ts.gptNavigationBridge?.pendingGeneration;
  const catchingUp = pendingGeneration !== undefined && pendingGeneration === ts.navGeneration;
  let inflight: AbortController | null = null;
  // Keep the captured path/generation for a failed startup catch-up. Retrying
  // that still-unapplied navigation must preserve its pending publisher records.
  const catchUpPath = catchingUp ? location.pathname : undefined;
  // A provisional navigation is not applied merely because its URL is current.
  // The null state also permits a later same-path retry after catch-up fails.
  let currentPath: string | null = catchingUp ? null : location.pathname;
  // Last path whose slots/bids were actually applied — the initial SSR page
  // counts. A failed navigation rolls `currentPath` back to this rather than to
  // the immediately-previous committed value: on rapid A→B where A was aborted
  // mid-flight and B then fails, rolling back to A (never loaded) would strand
  // it behind the no-op guard, so we roll back to the last applied route instead.
  let lastAppliedPath: string | null = catchingUp ? null : location.pathname;
  // Endpoint this session requests. Starts canonical; if the deployment does
  // not serve page-bids there, one navigation retries on the deprecated alias
  // and the session stays on it rather than re-probing every navigation.
  let pageBidsEndpoint = PAGE_BIDS_PATH;

  async function requestPageBids(
    path: string,
    signal: AbortSignal
  ): Promise<PageBidsResponse | null> {
    const attempt = await fetchPageBids(pageBidsEndpoint, path, signal);
    if (attempt.data || !attempt.wrongEndpoint || pageBidsEndpoint !== PAGE_BIDS_PATH) {
      return attempt.data;
    }

    const fallback = await fetchPageBids(PAGE_BIDS_LEGACY_PATH, path, signal);
    if (!fallback.data) return null;
    log.warn(
      `SPA auction hook: ${PAGE_BIDS_PATH} does not serve page-bids here, ` +
        `falling back to ${PAGE_BIDS_LEGACY_PATH}`
    );
    pageBidsEndpoint = PAGE_BIDS_LEGACY_PATH;
    return fallback.data;
  }

  async function onNavigate(path: string): Promise<void> {
    if (path === currentPath) return;
    currentPath = path;
    if (
      catchingUp &&
      path === catchUpPath &&
      pendingGeneration === ts.navGeneration &&
      lastAppliedPath === null &&
      !ts.publisherSlotRegistrationState?.payload
    ) {
      await fetchAndApplyPageBids(path, pendingGeneration);
      return;
    }
    const g = (window as GptWindow).googletag;
    if (g) clearPreviousNavigationTargeting(ts, g);
    ts.navGeneration = (ts.navGeneration ?? 0) + 1;
    invalidatePublisherSlotRegistrations(ts);
    delete ts.firstImpression;
    // A route change invalidates hydration aliases before the new route's
    // publisher can define a same-prefix slot while page-bids is in flight.
    for (const [elementId, handoff] of Object.entries(ts.gptSlotHandoffs ?? {})) {
      if (!handoff.publisherClaimed) delete ts.gptSlotHandoffs![elementId];
    }
    await fetchAndApplyPageBids(path, ts.navGeneration);
  }

  async function fetchAndApplyPageBids(path: string, generation: number): Promise<void> {
    inflight?.abort();
    const controller = new AbortController();
    inflight = controller;

    try {
      const data = await requestPageBids(path, controller.signal);
      if (!data) {
        // A transient page-bids failure must not strand this route: roll the
        // committed path back so a later navigation here retries instead of
        // being skipped by the no-op guard at the top. Only roll back when no
        // newer navigation has already advanced currentPath.
        if (inflight === controller && ts.navGeneration === generation)
          currentPath = lastAppliedPath;
        return;
      }
      if (inflight !== controller || ts.navGeneration !== generation) return;
      // Registered delivery and its renderer need the accepted current payload now.
      // Automatic-only routes retain the existing delayed public assignment below.
      if (data.slots.some((slot) => slot.initialization === 'publisher_registered')) {
        ts.adSlots = data.slots;
        ts.bids = data.bids;
      }
      settlePublisherSlotPayload(ts, generation, data.slots, data.bids);
      // Automatic placements retain their existing DOM wait.
      await waitForSlotElements(
        data.slots.filter((slot) => slot.initialization !== 'publisher_registered'),
        controller.signal
      );
      if (inflight !== controller || ts.navGeneration !== generation) return;
      ts.adSlots = data.slots;
      ts.bids = data.bids;
      // This route is now the committed, loaded state — a later failed
      // navigation rolls back here, and a return trip no-ops correctly.
      lastAppliedPath = path;
      // An empty page-bids response (template switch, auction, or consent gate)
      // carries no TS slots. Only run adInit() when there are slots to apply or
      // prior TS state to sweep — otherwise a gated navigation must not enter
      // the GPT command queue and risk activating services.
      const hasPriorTsState =
        (ts.prevGptSlots?.length ?? 0) > 0 ||
        Object.keys(ts.prevSlotTargetingKeys ?? {}).length > 0 ||
        Object.keys(ts.divToSlotId ?? {}).length > 0;
      if (data.slots.length > 0 || hasPriorTsState) {
        ts.adInit?.();
      }
    } catch (err) {
      if (err instanceof DOMException && err.name === 'AbortError') return;
      if (inflight === controller && ts.navGeneration === generation) currentPath = lastAppliedPath;
      log.warn('SPA auction hook: fetch failed', err);
    }
  }

  installGptNavigationBridge(ts, (path) => {
    void onNavigate(path);
  });
  if (ts.gptNavigationBridge) delete ts.gptNavigationBridge.pendingGeneration;
  if (catchingUp) {
    currentPath = location.pathname;
    // Adopt the pending records and token at the already-advanced generation.
    // Only an accepted page-bids response can authorize their delivery.
    void fetchAndApplyPageBids(currentPath, pendingGeneration);
  }
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
  if (typeof window === 'undefined') return;
  const url = (window as GptWindow).__tsjs_slim_prebid_url;
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
  const win = window as unknown as Record<string, unknown>;

  win.__tsjs_installGptShim = (window as GptWindow).__tsjs_gpt_early?.installShim ?? installGptShim;

  if (win.__tsjs_gpt_enabled === true) {
    installGptShim();
  }

  installTrustedServerPageTargeting();
  installTsAdInit();
  installSpaAuctionHook();
  installSlimPrebidLoader();
  installTsRenderBridge();
}
