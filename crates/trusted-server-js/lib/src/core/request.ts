// Request orchestration for tsjs: unified auction endpoint with iframe-based creative rendering.
import { dispatchApsRendering, renderApsCreative } from '../integrations/aps/render';

import { buildAdRequest, sendAuction } from './auction';
import { collectContext } from './context';
import { log } from './log';
import { getAllUnits, firstSize } from './registry';
import { createAdIframe, findSlot, buildCreativeDocument, sanitizeCreativeHtml } from './render';

const REQUEST_ADS_DEFAULT_TIMEOUT_MS = 10_000;
const REQUEST_ADS_MAX_SLOTS = 256;
const abortSignalAbortedGetter =
  typeof AbortSignal === 'undefined'
    ? undefined
    : Object.getOwnPropertyDescriptor(AbortSignal.prototype, 'aborted')?.get;

export type RequestAdsInputErrorCode =
  | 'invalid_options'
  | 'invalid_slots'
  | 'empty_slots'
  | 'duplicate_slot'
  | 'invalid_timeout'
  | 'invalid_signal';

export class RequestAdsInputError extends Error {
  public readonly code: RequestAdsInputErrorCode;

  public constructor(code: RequestAdsInputErrorCode) {
    super(code);
    this.name = 'RequestAdsInputError';
    this.code = code;
  }
}

export interface ValidatedRequestAdsOptions {
  readonly aborted: boolean;
  readonly signal: AbortSignal | undefined;
  readonly slots: readonly string[] | undefined;
  readonly timeoutMs: number;
}

function ownDataOptions(value: unknown): Record<string, unknown> | undefined {
  try {
    if (typeof value !== 'object' || value === null || Array.isArray(value)) return undefined;
    const prototype = Object.getPrototypeOf(value) as unknown;
    if (prototype !== Object.prototype && prototype !== null) return undefined;
    if (Object.getOwnPropertySymbols(value).length !== 0) return undefined;
    const output: Record<string, unknown> = Object.create(null) as Record<string, unknown>;
    for (const key of Object.getOwnPropertyNames(value)) {
      const descriptor = Object.getOwnPropertyDescriptor(value, key);
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return undefined;
      output[key] = descriptor.value;
    }
    return output;
  } catch {
    return undefined;
  }
}

function ownDataSlots(value: unknown): readonly unknown[] | undefined {
  try {
    if (
      !Array.isArray(value) ||
      Object.getPrototypeOf(value) !== Array.prototype ||
      Object.getOwnPropertySymbols(value).length !== 0
    ) {
      return undefined;
    }
    const length = Object.getOwnPropertyDescriptor(value, 'length');
    if (
      !length ||
      !('value' in length) ||
      !Number.isSafeInteger(length.value) ||
      length.value < 0 ||
      length.value > REQUEST_ADS_MAX_SLOTS ||
      Object.getOwnPropertyNames(value).length !== length.value + 1
    ) {
      return undefined;
    }
    const output: unknown[] = [];
    for (let index = 0; index < length.value; index += 1) {
      const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return undefined;
      output[index] = descriptor.value;
    }
    return output;
  } catch {
    return undefined;
  }
}

function readAbortSignal(signal: unknown): boolean | undefined {
  try {
    return typeof abortSignalAbortedGetter === 'function'
      ? (Reflect.apply(abortSignalAbortedGetter, signal, []) as boolean)
      : undefined;
  } catch {
    return undefined;
  }
}

/** Validate and detach the complete public request before creating attempts. */
export function validateRequestAdsOptions(value: unknown): ValidatedRequestAdsOptions {
  if (value === undefined) {
    return Object.freeze({
      aborted: false,
      signal: undefined,
      slots: undefined,
      timeoutMs: REQUEST_ADS_DEFAULT_TIMEOUT_MS,
    });
  }
  const options = ownDataOptions(value);
  if (
    !options ||
    !Object.keys(options).every((key) => key === 'slots' || key === 'timeoutMs' || key === 'signal')
  ) {
    throw new RequestAdsInputError('invalid_options');
  }

  let slots: readonly string[] | undefined;
  if (Object.prototype.hasOwnProperty.call(options, 'slots')) {
    const rawSlots = ownDataSlots(options.slots);
    if (!rawSlots) throw new RequestAdsInputError('invalid_slots');
    if (rawSlots.length === 0) throw new RequestAdsInputError('empty_slots');
    const seen = new Set<string>();
    const copy: string[] = [];
    for (const slot of rawSlots) {
      if (
        typeof slot !== 'string' ||
        slot.length === 0 ||
        new TextEncoder().encode(slot).byteLength > 256 ||
        /[\p{Cc}]/u.test(slot) ||
        /[\uD800-\uDFFF]/u.test(slot)
      ) {
        throw new RequestAdsInputError('invalid_slots');
      }
      if (seen.has(slot)) throw new RequestAdsInputError('duplicate_slot');
      seen.add(slot);
      copy.push(slot);
    }
    slots = Object.freeze(copy);
  }

  let timeoutMs = REQUEST_ADS_DEFAULT_TIMEOUT_MS;
  if (Object.prototype.hasOwnProperty.call(options, 'timeoutMs')) {
    if (
      typeof options.timeoutMs !== 'number' ||
      !Number.isInteger(options.timeoutMs) ||
      options.timeoutMs < 100 ||
      options.timeoutMs > 30_000
    ) {
      throw new RequestAdsInputError('invalid_timeout');
    }
    timeoutMs = options.timeoutMs;
  }

  let signal: AbortSignal | undefined;
  let aborted = false;
  if (Object.prototype.hasOwnProperty.call(options, 'signal')) {
    const observed = readAbortSignal(options.signal);
    if (observed === undefined) throw new RequestAdsInputError('invalid_signal');
    signal = options.signal as AbortSignal;
    aborted = observed;
  }
  return Object.freeze({ aborted, signal, slots, timeoutMs });
}

export type RequestAdsCallback = () => void;
export interface LegacyRequestAdsOptions {
  bidsBackHandler?: RequestAdsCallback | undefined;
  timeout?: number | undefined;
}

type RenderCreativeInlineOptions = {
  slotId: string;
  // Accept unknown input here because bidder JSON is untrusted at runtime.
  creativeHtml: unknown;
  creativeWidth?: number | undefined;
  creativeHeight?: number | undefined;
  seat: string;
  creativeId: string;
  auctionId?: string | undefined;
  bidId?: string | undefined;
  admHash?: string | undefined;
};

// Entry point matching Prebid's requestBids signature; uses unified /auction endpoint.
export function requestAds(
  callbackOrOpts?: RequestAdsCallback | LegacyRequestAdsOptions,
  _maybeOpts?: LegacyRequestAdsOptions
): void {
  let callback: RequestAdsCallback | undefined;
  if (typeof callbackOrOpts === 'function') {
    callback = callbackOrOpts as RequestAdsCallback;
  } else {
    callback = (callbackOrOpts as LegacyRequestAdsOptions | undefined)?.bidsBackHandler;
  }

  log.info('requestAds: called', { hasCallback: typeof callback === 'function' });
  try {
    const adUnits = getAllUnits();
    const config = collectContext();
    const payload = { ...buildAdRequest(adUnits), config };
    log.debug('requestAds: payload', { units: adUnits.length, contextKeys: Object.keys(config) });

    // Use unified auction endpoint
    void sendAuction('/auction', payload)
      .then((bids) => {
        log.info('requestAds: got bids', { count: bids.length });
        for (const bid of bids) {
          if (!bid.impid) continue;
          if (bid.renderer) {
            void Promise.resolve(
              dispatchApsRendering({
                slotId: bid.impid,
                renderer: bid.renderer,
                trustedServer: (renderer) => renderApsCreative({ slotId: bid.impid, renderer }),
              })
            );
            continue;
          }
          if (!bid.adm) {
            log.debug('requestAds: bid has no adm, skipping', { slotId: bid.impid });
            continue;
          }
          renderCreativeInline({
            slotId: bid.impid,
            creativeHtml: bid.adm,
            creativeWidth: bid.width,
            creativeHeight: bid.height,
            seat: bid.seat,
            creativeId: bid.creativeId,
          });
        }
        log.info('requestAds: rendered creatives from response');
      })
      .catch((err) => {
        log.warn('requestAds: auction failed', err);
      });

    // Synchronously invoke callback to match test expectations
    try {
      if (callback) callback();
    } catch {
      /* ignore callback errors */
    }
  } catch {
    log.warn('requestAds: failed to initiate');
  }
}

// Render a creative by writing its HTML into a sandboxed iframe. The markup may
// be raw bidder output (server-side sanitization is opt-in); the sandbox's
// origin isolation is the security boundary.
function renderCreativeInline({
  slotId,
  creativeHtml,
  creativeWidth,
  creativeHeight,
  seat,
  creativeId,
}: RenderCreativeInlineOptions): void {
  const container = findSlot(slotId) as HTMLElement | null;
  if (!container) {
    log.warn('renderCreativeInline: slot not found; skipping render', { slotId, seat, creativeId });
    return;
  }

  try {
    const sanitization = sanitizeCreativeHtml(creativeHtml);
    if (sanitization.kind === 'rejected') {
      log.warn('renderCreativeInline: rejected creative', {
        slotId,
        seat,
        creativeId,
        originalLength: sanitization.originalLength,
        rejectionReason: sanitization.rejectionReason,
      });
      return;
    }

    // Clear the slot only after sanitization succeeds so rejected creatives never blank existing content.
    container.innerHTML = '';

    // Determine size with fallback chain: creative size → ad unit size → 300x250
    let width: number;
    let height: number;

    if (creativeWidth && creativeHeight && creativeWidth > 0 && creativeHeight > 0) {
      width = creativeWidth;
      height = creativeHeight;
      log.debug('renderCreativeInline: using creative dimensions', { width, height });
    } else {
      const unit = getAllUnits().find((u) => u.code === slotId);
      const size = (unit && firstSize(unit)) || [300, 250];
      width = size[0];
      height = size[1];
      log.debug('renderCreativeInline: using ad unit dimensions', { width, height });
    }

    const iframe = createAdIframe(container, {
      name: `tsjs_iframe_${slotId}`,
      title: 'Ad content',
      width,
      height,
    });

    iframe.srcdoc = buildCreativeDocument(sanitization.sanitizedHtml);

    log.info('renderCreativeInline: rendered', {
      slotId,
      seat,
      creativeId,
      width,
      height,
      originalLength: sanitization.originalLength,
    });
  } catch (err) {
    log.warn('renderCreativeInline: failed', { slotId, seat, creativeId, err });
  }
}
