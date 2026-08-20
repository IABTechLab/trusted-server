import { log } from '../../core/log';
import type {
  ApsNativeRendererHook,
  ApsPrebidRendererEntry,
  ApsRendererV1,
  GptDiagnosticsCreativeFailure,
  TsjsApi,
} from '../../core/types';

export const APS_RENDERER_PATH = '/integrations/aps/renderer';
export const APS_RENDERING_MODE_META_NAME = 'trusted-server-aps-rendering-mode';
export const APS_NATIVE_RENDERER_ACK_TIMEOUT_MS = 10_000;
export const APS_RENDERER_SANDBOX =
  'allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation';
export const APS_UNIVERSAL_CREATIVE_RENDERER_VERSION = 4;

const MAX_ACCOUNT_ID_BYTES = 1024;
const MAX_CREATIVE_ID_BYTES = 1024;
const MAX_CREATIVE_URL_BYTES = 4096;
const MAX_RENDER_ENVELOPE_BYTES = 256 * 1024;
const MAX_RENDER_ENVELOPE_BASE64_BYTES = 4 * Math.ceil(MAX_RENDER_ENVELOPE_BYTES / 3);
const DESCRIPTOR_KEYS = [
  'aaxResponse',
  'accountId',
  'bidId',
  'creativeUrl',
  'height',
  'tagType',
  'type',
  'version',
  'width',
] as const;
const DESCRIPTOR_KEYS_WITH_CREATIVE_ID = [...DESCRIPTOR_KEYS, 'creativeId'].sort();
const activeFrames = new WeakMap<HTMLElement, HTMLIFrameElement>();
const pendingFrameCancels = new WeakMap<HTMLElement, () => void>();
const RENDERER_READY_MESSAGE = 'trusted-server/aps/renderer-ready';
const RENDERER_FAILED_MESSAGE = 'trusted-server/aps/renderer-failed';
/**
 * Message the Universal Creative frame relays to the top window when an APS
 * render never completes.
 *
 * The creative frame is cross-origin, so the top-window listener treats every
 * field as untrusted and validates the reason against
 * [`APS_RENDER_FAILURE_REASONS`] before recording it. The relay is
 * diagnostics-only and never influences creative delivery.
 */
export const APS_RENDER_FAILED_MESSAGE = 'trusted-server/aps/render-failed';

/**
 * Wire reasons the render path can emit, mapped onto safe diagnostic categories.
 *
 * Built on a null prototype so a hostile `__proto__`, `constructor`, or
 * `toString` relayed by the cross-origin creative frame resolves to `undefined`
 * rather than an inherited member.
 */
const APS_RENDER_FAILURE_REASONS: Readonly<Record<string, GptDiagnosticsCreativeFailure>> =
  Object.freeze(
    Object.assign(
      Object.create(null) as Record<string, GptDiagnosticsCreativeFailure>,
      {
        bad_hash: 'aps_bad_hash',
        nonce_mismatch: 'aps_nonce_mismatch',
        source_mismatch: 'aps_source_mismatch',
        descriptor_keys: 'aps_descriptor_keys',
        descriptor_fields: 'aps_descriptor_fields',
        descriptor_envelope: 'aps_descriptor_envelope',
        amazon_script_error: 'aps_runner_script_error',
        frame_timeout: 'aps_frame_timeout',
        frame_load_error: 'aps_frame_load_error',
        frame_reported_failure: 'aps_frame_reported_failure',
        unknown: 'aps_unknown',
      } as const
    )
  );

/**
 * Resolve a relayed render failure reason to a safe diagnostic category.
 *
 * Returns `undefined` for anything not on the allowlist, so an unrecognized or
 * hostile value from the cross-origin creative frame is dropped instead of
 * being recorded.
 *
 * @example
 * apsRenderFailureReason('frame_timeout'); // 'aps_frame_timeout'
 * apsRenderFailureReason('__proto__'); // undefined
 */
export function apsRenderFailureReason(value: unknown): GptDiagnosticsCreativeFailure | undefined {
  return typeof value === 'string' ? APS_RENDER_FAILURE_REASONS[value] : undefined;
}
const RENDERER_READY_TIMEOUT_MS = 10_000;
const MAX_PREBID_RENDERER_ENTRIES = 256;
const DEFAULT_PREBID_RENDERER_TTL_SECONDS = 300;
const MAX_PREBID_RENDERER_TTL_SECONDS = 3600;
const MAX_PREBID_ID_BYTES = 1024;

type ValidatedRendererCacheEntry = {
  publisherOrigin: string;
  renderer: ApsRendererV1;
};
const validatedRendererCache = new WeakMap<object, ValidatedRendererCacheEntry>();
const nativeDispatches = new Map<string, symbol>();

function releaseNativeDispatch(slotId: string, dispatch: symbol): boolean {
  if (nativeDispatches.get(slotId) !== dispatch) return false;
  nativeDispatches.delete(slotId);
  return true;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function hasExactKeys(
  value: unknown,
  expected: readonly string[]
): value is Record<string, unknown> {
  if (!isRecord(value)) return false;
  const actual = Object.keys(value).sort();
  const sortedExpected = [...expected].sort();
  return (
    actual.length === sortedExpected.length &&
    actual.every((key, index) => key === sortedExpected[index])
  );
}

/** Parse only the versioned descriptor shape; decoded-envelope trust checks happen separately. */
export function parseApsRendererDescriptor(value: unknown): ApsRendererV1 | undefined {
  if (
    !hasExactKeys(value, DESCRIPTOR_KEYS) &&
    !hasExactKeys(value, DESCRIPTOR_KEYS_WITH_CREATIVE_ID)
  ) {
    return undefined;
  }

  if (
    value.type !== 'aps' ||
    value.version !== 1 ||
    typeof value.accountId !== 'string' ||
    value.accountId.length === 0 ||
    new TextEncoder().encode(value.accountId).length > MAX_ACCOUNT_ID_BYTES ||
    typeof value.bidId !== 'string' ||
    value.bidId.length === 0 ||
    (Object.prototype.hasOwnProperty.call(value, 'creativeId') &&
      (typeof value.creativeId !== 'string' ||
        value.creativeId.length === 0 ||
        new TextEncoder().encode(value.creativeId).length > MAX_CREATIVE_ID_BYTES)) ||
    (value.tagType !== 'iframe' && value.tagType !== 'script') ||
    typeof value.creativeUrl !== 'string' ||
    typeof value.aaxResponse !== 'string' ||
    value.aaxResponse.length > MAX_RENDER_ENVELOPE_BASE64_BYTES ||
    !Number.isSafeInteger(value.width) ||
    (value.width as number) <= 0 ||
    !Number.isSafeInteger(value.height) ||
    (value.height as number) <= 0
  ) {
    return undefined;
  }

  return value as unknown as ApsRendererV1;
}

function decodeStandardBase64(value: string): Uint8Array | undefined {
  if (
    value.length === 0 ||
    value.length % 4 !== 0 ||
    !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(value)
  ) {
    return undefined;
  }

  try {
    const binary = atob(value);
    if (binary.length > MAX_RENDER_ENVELOPE_BYTES || btoa(binary) !== value) return undefined;
    return Uint8Array.from(binary, (character) => character.charCodeAt(0));
  } catch {
    return undefined;
  }
}

function validCreativeUrl(value: string, publisherOrigin: string): boolean {
  if (new TextEncoder().encode(value).length > MAX_CREATIVE_URL_BYTES) return false;

  try {
    const url = new URL(value);
    return (
      url.protocol === 'https:' &&
      url.username === '' &&
      url.password === '' &&
      url.origin !== publisherOrigin
    );
  } catch {
    return false;
  }
}

/** Fully validate the exact APS envelope and cross-check every duplicated descriptor field. */
export function validateApsRenderer(
  value: unknown,
  publisherOrigin = window.location.origin
): ApsRendererV1 | undefined {
  if (isRecord(value)) {
    const cached = validatedRendererCache.get(value);
    if (cached?.publisherOrigin === publisherOrigin) return cached.renderer;
  }

  const renderer = parseApsRendererDescriptor(value);
  if (!renderer || !validCreativeUrl(renderer.creativeUrl, publisherOrigin)) return undefined;

  const bytes = decodeStandardBase64(renderer.aaxResponse);
  if (!bytes) return undefined;

  let decoded: unknown;
  try {
    decoded = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes));
  } catch {
    return undefined;
  }

  if (!hasExactKeys(decoded, ['seatbid'])) return undefined;
  const seatbids = decoded.seatbid;
  if (!Array.isArray(seatbids) || seatbids.length !== 1) return undefined;
  const seat = seatbids[0];
  if (!hasExactKeys(seat, ['bid']) || !Array.isArray(seat.bid) || seat.bid.length !== 1) {
    return undefined;
  }

  const bid = seat.bid[0];
  if (!hasExactKeys(bid, ['ext', 'h', 'id', 'price', 'w'])) return undefined;
  if (!hasExactKeys(bid.ext, ['creativeurl', 'tagtype'])) return undefined;

  if (
    bid.id !== renderer.bidId ||
    bid.w !== renderer.width ||
    bid.h !== renderer.height ||
    bid.ext.creativeurl !== renderer.creativeUrl ||
    bid.ext.tagtype !== renderer.tagType ||
    typeof bid.price !== 'number' ||
    !Number.isFinite(bid.price) ||
    bid.price < 0
  ) {
    return undefined;
  }

  const validated = Object.freeze({ ...renderer }) as ApsRendererV1;
  validatedRendererCache.set(value as object, { publisherOrigin, renderer: validated });
  validatedRendererCache.set(validated, { publisherOrigin, renderer: validated });
  return validated;
}

function validPrebidIdentity(value: unknown): value is string {
  return (
    typeof value === 'string' &&
    value.length > 0 &&
    new TextEncoder().encode(value).length <= MAX_PREBID_ID_BYTES
  );
}

function validPrebidAdId(value: unknown): value is string {
  return validPrebidIdentity(value) && /^[A-Za-z0-9-]+$/.test(value);
}

function prunePrebidRenderers(registry: Record<string, ApsPrebidRendererEntry>, now: number): void {
  for (const [adId, entry] of Object.entries(registry)) {
    if (!Number.isFinite(entry.expiresAt) || entry.expiresAt <= now) delete registry[adId];
  }

  const entries = Object.entries(registry);
  if (entries.length <= MAX_PREBID_RENDERER_ENTRIES) return;
  entries
    .sort(([, left], [, right]) => left.registeredAt - right.registeredAt)
    .slice(0, entries.length - MAX_PREBID_RENDERER_ENTRIES)
    .forEach(([adId]) => delete registry[adId]);
}

/** Bind Prebid's generated ad ID to a fully validated APS renderer capability. */
export function registerApsPrebidRenderer(
  adId: unknown,
  adUnitCode: unknown,
  input: unknown,
  ttlSeconds: unknown = DEFAULT_PREBID_RENDERER_TTL_SECONDS,
  lifecycle?: { markUsed(): void }
): boolean {
  if (
    !validPrebidAdId(adId) ||
    !validPrebidIdentity(adUnitCode) ||
    typeof lifecycle?.markUsed !== 'function'
  ) {
    return false;
  }
  const renderer = validateApsRenderer(input);
  if (!renderer) return false;

  const now = Date.now();
  const boundedTtlSeconds =
    typeof ttlSeconds === 'number' && Number.isFinite(ttlSeconds) && ttlSeconds > 0
      ? Math.min(ttlSeconds, MAX_PREBID_RENDERER_TTL_SECONDS)
      : DEFAULT_PREBID_RENDERER_TTL_SECONDS;
  const tsjs = (window.tsjs ??= {} as TsjsApi);
  const registry = (tsjs.apsPrebidRenderers ??= Object.create(null) as Record<
    string,
    ApsPrebidRendererEntry
  >);
  prunePrebidRenderers(registry, now);

  if (!(adId in registry) && Object.keys(registry).length >= MAX_PREBID_RENDERER_ENTRIES) {
    const oldest = Object.entries(registry).sort(
      ([, left], [, right]) => left.registeredAt - right.registeredAt
    )[0];
    if (oldest) delete registry[oldest[0]];
  }

  registry[adId] = {
    adUnitCode,
    renderer,
    registeredAt: now,
    expiresAt: now + boundedTtlSeconds * 1000,
    markUsed: lifecycle.markUsed,
  };
  return true;
}

/** Return an unexpired Prebid APS capability without consuming it. */
export function getApsPrebidRenderer(adId: string): ApsPrebidRendererEntry | undefined {
  if (!validPrebidAdId(adId)) return undefined;
  const registry = window.tsjs?.apsPrebidRenderers;
  const entry = registry?.[adId];
  if (!entry) return undefined;
  if (
    !Number.isFinite(entry.expiresAt) ||
    entry.expiresAt <= Date.now() ||
    typeof entry.markUsed !== 'function'
  ) {
    delete registry![adId];
    return undefined;
  }
  return entry;
}

/** Atomically consume the exact capability previously returned by the registry. */
export function consumeApsPrebidRenderer(adId: string, expected: ApsPrebidRendererEntry): boolean {
  const registry = window.tsjs?.apsPrebidRenderers;
  if (!registry || registry[adId] !== expected) return false;
  delete registry[adId];
  return true;
}

/** Whether the server explicitly selected the opt-in publisher-native hook mode. */
export function isPublisherNativeApsRendering(): boolean {
  return (
    document.head.querySelector(
      `meta[name="${APS_RENDERING_MODE_META_NAME}"][content="publisher_native"]`
    ) !== null
  );
}

export interface DispatchApsRenderingOptions {
  slotId: string;
  renderer: unknown;
  /** Existing Trusted Server owner, invoked only in the default mode. */
  trustedServer: (renderer: ApsRendererV1) => boolean;
}

/**
 * Dispatch a validated APS descriptor to exactly one configured rendering owner.
 *
 * Publisher hooks own side-effect cancellation and render completion. Trusted Server
 * ignores superseded hook acknowledgements and never falls back to its iframe.
 */
export function dispatchApsRendering({
  slotId,
  renderer: input,
  trustedServer,
}: DispatchApsRenderingOptions): boolean | Promise<boolean> {
  // Record every attempt before any early return so it supersedes an older
  // pending native acknowledgement for the same slot.
  const dispatch = Symbol(slotId);
  nativeDispatches.set(slotId, dispatch);

  const renderer = validateApsRenderer(input);
  if (!renderer) {
    releaseNativeDispatch(slotId, dispatch);
    log.warn('APS renderer: rejected descriptor');
    return false;
  }
  if (!isPublisherNativeApsRendering()) {
    try {
      return trustedServer(renderer);
    } finally {
      releaseNativeDispatch(slotId, dispatch);
    }
  }

  let hook: ApsNativeRendererHook | undefined;
  let render: ApsNativeRendererHook['render'] | undefined;
  try {
    hook = window.tsjs?.apsNativeRenderer;
    render = hook?.render;
  } catch {
    releaseNativeDispatch(slotId, dispatch);
    log.warn('APS native renderer: publisher hook lookup threw');
    return Promise.resolve(false);
  }
  if (!hook || typeof render !== 'function') {
    releaseNativeDispatch(slotId, dispatch);
    log.warn('APS native renderer: publisher hook is unavailable');
    return Promise.resolve(false);
  }

  let response: unknown;
  try {
    response = Reflect.apply(render, hook, [{ version: 1, slotId, renderer }]);
  } catch {
    releaseNativeDispatch(slotId, dispatch);
    log.warn('APS native renderer: publisher hook threw');
    return Promise.resolve(false);
  }

  if (nativeDispatches.get(slotId) !== dispatch) {
    log.warn('APS native renderer: ignored stale acknowledgement');
    return Promise.resolve(false);
  }

  return new Promise<boolean>((resolve) => {
    let settled = false;
    const settle = (accepted: boolean, warning?: string): void => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      if (!releaseNativeDispatch(slotId, dispatch)) {
        log.warn('APS native renderer: ignored stale acknowledgement');
        resolve(false);
        return;
      }
      if (warning) log.warn(warning);
      resolve(accepted);
    };
    const timeout = setTimeout(() => {
      settle(false, 'APS native renderer: publisher hook acknowledgement timed out');
    }, APS_NATIVE_RENDERER_ACK_TIMEOUT_MS);

    Promise.resolve(response).then(
      (value) => {
        let accepted: boolean;
        try {
          if (
            !isRecord(value) ||
            (!hasExactKeys(value, ['accepted']) && !hasExactKeys(value, ['accepted', 'reason'])) ||
            typeof value.accepted !== 'boolean' ||
            (Object.prototype.hasOwnProperty.call(value, 'reason') &&
              typeof value.reason !== 'string')
          ) {
            settle(false, 'APS native renderer: publisher hook returned malformed acknowledgement');
            return;
          }
          accepted = value.accepted;
        } catch {
          settle(false, 'APS native renderer: publisher hook returned malformed acknowledgement');
          return;
        }

        if (!accepted) {
          settle(false, 'APS native renderer: publisher hook declined descriptor');
          return;
        }
        settle(true);
      },
      () => {
        settle(false, 'APS native renderer: publisher hook rejected');
      }
    );
  });
}

function createNonce(): string | undefined {
  if (typeof crypto === 'undefined' || typeof crypto.getRandomValues !== 'function')
    return undefined;
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  let binary = '';
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/g, '');
}

/**
 * Return the absolute same-publisher URL used by direct and Universal Creative rendering.
 *
 * This intentionally inherits the publisher page scheme for same-origin deployments,
 * including local development. APS endpoints and third-party creative URLs remain
 * HTTPS-only.
 */
export function apsRendererUrl(pageOrigin = window.location.origin): string | undefined {
  try {
    const origin = new URL(pageOrigin);
    const url = new URL(APS_RENDERER_PATH, origin);
    if (
      url.origin !== origin.origin ||
      url.pathname !== APS_RENDERER_PATH ||
      url.search !== '' ||
      url.hash !== ''
    ) {
      return undefined;
    }
    return url.href;
  } catch {
    return undefined;
  }
}

export interface RenderApsCreativeOptions {
  slotId: string;
  renderer: unknown;
}

/** Render APS through the static endpoint under an outer opaque-origin sandbox. */
export function renderApsCreative({ slotId, renderer: input }: RenderApsCreativeOptions): boolean {
  const renderer = validateApsRenderer(input);
  const rendererUrl = apsRendererUrl();
  const nonce = createNonce();
  if (!renderer || !rendererUrl || !nonce) {
    log.warn('APS renderer: rejected descriptor');
    return false;
  }

  const container = document.getElementById(slotId);
  if (!container) {
    log.warn('APS renderer: slot not found');
    return false;
  }

  const iframe = document.createElement('iframe');
  iframe.title = 'Ad content';
  iframe.width = String(renderer.width);
  iframe.height = String(renderer.height);
  iframe.style.border = '0';
  iframe.style.display = 'none';
  iframe.setAttribute('sandbox', APS_RENDERER_SANDBOX);
  iframe.src = `${rendererUrl}#tsaps=${nonce}`;

  // A replacement must cancel a pending frame, not merely detach it: its
  // message listener and ready timeout would otherwise remain live until expiry.
  pendingFrameCancels.get(container)?.();
  activeFrames.set(container, iframe);

  let settled = false;
  const cleanup = (): void => {
    window.removeEventListener('message', receive);
    window.clearTimeout(timeoutId);
  };
  const cancel = (): void => {
    if (settled) return;
    settled = true;
    cleanup();
    if (pendingFrameCancels.get(container) === cancel) pendingFrameCancels.delete(container);
    if (activeFrames.get(container) === iframe) activeFrames.delete(container);
    iframe.remove();
  };
  const fail = (): void => {
    if (settled) return;
    cancel();
    log.warn('APS renderer: frame load failed');
  };
  const commit = (): void => {
    if (settled || activeFrames.get(container) !== iframe || !iframe.isConnected) return;
    settled = true;
    cleanup();
    if (pendingFrameCancels.get(container) === cancel) pendingFrameCancels.delete(container);
    for (const child of Array.from(container.children)) {
      if (child !== iframe) child.remove();
    }
    iframe.style.display = '';
  };
  function receive(event: MessageEvent): void {
    if (event.source !== iframe.contentWindow || !hasExactKeys(event.data, ['message', 'nonce'])) {
      return;
    }
    if (event.data.nonce !== nonce) return;
    if (event.data.message === RENDERER_READY_MESSAGE) commit();
    else if (event.data.message === RENDERER_FAILED_MESSAGE) fail();
  }

  window.addEventListener('message', receive);
  iframe.addEventListener(
    'load',
    () => {
      if (settled || activeFrames.get(container) !== iframe || !iframe.isConnected) return;
      try {
        const target = iframe.contentWindow;
        if (!target) {
          fail();
          return;
        }
        target.postMessage({ nonce, renderer }, '*');
      } catch {
        fail();
      }
    },
    { once: true }
  );
  iframe.addEventListener('error', fail, { once: true });

  const timeoutId = window.setTimeout(fail, RENDERER_READY_TIMEOUT_MS);
  pendingFrameCancels.set(container, cancel);
  container.appendChild(iframe);
  return true;
}

/**
 * Static source executed by Prebid Universal Creative's dynamic-renderer frame.
 * It reads only the validated descriptor and trusted absolute endpoint URL from data.
 */
export const APS_UNIVERSAL_CREATIVE_RENDERER = String.raw`(function(){window.render=function(d,_h,w){return new Promise(function(resolve,reject){
try{var r=d&&d.apsRenderer,u=d&&d.rendererUrl;if(!r||typeof u!=="string")throw new Error("invalid APS renderer data");
var p=new URL(u);if((p.protocol!=="https:"&&p.protocol!=="http:")||p.username||p.password||p.pathname!=="${APS_RENDERER_PATH}"||p.search||p.hash)throw new Error("invalid APS renderer URL");
var c=w.crypto;if(!c||typeof c.getRandomValues!=="function")throw new Error("APS renderer randomness unavailable");
var b=new Uint8Array(16);c.getRandomValues(b);var s="";for(var i=0;i<b.length;i++)s+=String.fromCharCode(b[i]);
var n=w.btoa(s).replace(/\+/g,"-").replace(/\//g,"_").replace(/=+$/g,"");
var f=w.document.createElement("iframe"),done=false,t;
function clean(){w.removeEventListener("message",receive);if(t)w.clearTimeout(t);}
function report(x){try{(w.top||w).postMessage({message:"${APS_RENDER_FAILED_MESSAGE}",adId:(d&&typeof d.adId==="string")?d.adId:"",reason:x},"*");}catch(_e){}}
function fail(x){if(done)return;done=true;clean();f.remove();report(x||"unknown");reject(new Error("APS renderer frame failed"));}
function receive(e){var m=e.data;if(e.source!==f.contentWindow||!m)return;
if(m.message==="${RENDERER_READY_MESSAGE}"&&m.nonce===n){done=true;clean();resolve();}
else if(m.message==="${RENDERER_FAILED_MESSAGE}"&&(m.nonce===n||m.nonce===undefined))fail(typeof m.reason==="string"?m.reason:"frame_reported_failure");}
f.width=String(r.width);f.height=String(r.height);f.style.border="0";
f.setAttribute("sandbox","${APS_RENDERER_SANDBOX}");
f.src=p.href+"#tsaps="+n;f.onload=function(){if(!done&&f.contentWindow)f.contentWindow.postMessage({nonce:n,renderer:r},"*");};
f.onerror=function(){fail("frame_load_error");};w.addEventListener("message",receive);t=w.setTimeout(function(){fail("frame_timeout");},${RENDERER_READY_TIMEOUT_MS});w.document.body.appendChild(f);
}catch(e){reject(e);}});};})();`;
