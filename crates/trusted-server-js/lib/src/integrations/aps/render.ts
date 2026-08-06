import { log } from '../../core/log';
import { findSlot } from '../../core/render';
import type { ApsPrebidRendererEntry, ApsRendererV1, TsjsApi } from '../../core/types';

import {
  classifyApsRendererDescriptorV1,
  classifyApsRendererV1,
} from './generated/renderer_validator_v1';

export const APS_RENDERER_PATH = '/integrations/aps/renderer';
export const APS_RENDERING_MODE_ATTRIBUTE_NAME = 'data-ts-aps-rendering-mode';
export const APS_PREBID_CREATIVE_RUNNER_URL =
  'https://client.aps.amazon-adsystem.com/prebid-creative.js';
export const APS_NATIVE_RENDERER_TIMEOUT_MS = 10_000;
export const APS_RENDERER_SANDBOX =
  'allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation';
export const APS_UNIVERSAL_CREATIVE_RENDERER_VERSION = 4;

const activeFrames = new WeakMap<HTMLElement, HTMLIFrameElement>();
const pendingFrameCancels = new WeakMap<HTMLElement, () => void>();
const RENDERER_READY_MESSAGE = 'trusted-server/aps/renderer-ready';
const RENDERER_FAILED_MESSAGE = 'trusted-server/aps/renderer-failed';
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
const publisherNativeRendering =
  typeof document !== 'undefined' &&
  document.currentScript?.getAttribute(APS_RENDERING_MODE_ATTRIBUTE_NAME) === 'publisher_native';

function releaseNativeDispatch(slotId: string, dispatch: symbol): boolean {
  if (nativeDispatches.get(slotId) !== dispatch) return false;
  nativeDispatches.delete(slotId);
  return true;
}

function sourceBelongsToElement(
  source: MessageEventSource | null | undefined,
  element: HTMLElement
): boolean {
  return source
    ? Array.from(element.querySelectorAll('iframe')).some(
        (iframe) => iframe.contentWindow === source
      )
    : false;
}

function sourceMatchedCandidates(
  candidates: HTMLElement[],
  source?: MessageEventSource | null
): HTMLElement[] {
  if (!source) return candidates;
  const sourceMatches = candidates.filter((element) => sourceBelongsToElement(source, element));
  return sourceMatches.length > 0 ? sourceMatches : candidates;
}

function dynamicSlotCandidates(
  divIdPrefix: string,
  source?: MessageEventSource | null
): HTMLElement[] {
  const candidates = Array.from(document.querySelectorAll<HTMLElement>('[id]')).filter(
    (element) => element.id.startsWith(divIdPrefix) && !element.id.endsWith('-container')
  );
  return sourceMatchedCandidates(candidates, source);
}

function uniqueSlotCandidate(candidates: HTMLElement[]): HTMLElement | null {
  return candidates.length === 1 ? candidates[0]! : null;
}

function findApsContainer(slotId: string, source?: MessageEventSource | null): HTMLElement | null {
  try {
    const mapping = window.tsjs?.divToSlotId ?? {};
    const mappedCandidates = sourceMatchedCandidates(
      Object.entries(mapping)
        .filter(([, mappedSlotId]) => mappedSlotId === slotId)
        .map(([divId]) => findSlot(divId))
        .filter((element): element is HTMLElement => element !== null),
      source
    );
    const mapped = uniqueSlotCandidate(mappedCandidates);
    if (mapped) return mapped;

    if (slotId.endsWith('-container')) {
      const inner = findSlot(slotId.slice(0, -'-container'.length));
      if (inner) return inner;
    }

    const direct = findSlot(slotId);
    if (direct && !direct.id.endsWith('-container')) return direct;

    const configuredDivId = window.tsjs?.adSlots?.find((slot) => slot.id === slotId)?.div_id;
    if (configuredDivId) {
      const configured = findSlot(configuredDivId);
      if (configured) return configured;

      const dynamic = uniqueSlotCandidate(dynamicSlotCandidates(configuredDivId, source));
      if (dynamic) return dynamic;
    }

    const dynamic = uniqueSlotCandidate(dynamicSlotCandidates(slotId, source));
    return dynamic ?? direct;
  } catch {
    return null;
  }
}

function cancelPendingApsRendering(slotId: string, source?: MessageEventSource | null): void {
  const container = findApsContainer(slotId, source);
  if (container) pendingFrameCancels.get(container)?.();
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isExactRendererResult(value: unknown): value is Record<string, unknown> {
  if (!isRecord(value)) return false;
  const actual = Object.keys(value).sort();
  return actual.length === 2 && actual[0] === 'message' && actual[1] === 'nonce';
}

/** Parse only the versioned descriptor shape; decoded-envelope trust checks happen separately. */
export function parseApsRendererDescriptor(value: unknown): ApsRendererV1 | undefined {
  if (classifyApsRendererDescriptorV1(value) !== 'accepted') {
    return undefined;
  }

  return value as unknown as ApsRendererV1;
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

  if (classifyApsRendererV1(value, publisherOrigin) !== 'accepted') return undefined;
  const renderer = value as ApsRendererV1;

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

export interface DispatchApsRenderingOptions {
  slotId: string;
  renderer: unknown;
  source?: MessageEventSource | null;
  /** Existing Trusted Server owner, invoked only in the default mode. */
  trustedServer: (renderer: ApsRendererV1) => boolean;
}

/**
 * Dispatch a validated APS descriptor to exactly one configured rendering owner.
 *
 * Native mode loads APS's fixed Prebid creative runner in a publisher-origin friendly
 * frame. Superseded attempts are cancelled and never fall back to the opaque renderer.
 */
export function dispatchApsRendering({
  slotId,
  renderer: input,
  source,
  trustedServer,
}: DispatchApsRenderingOptions): boolean | Promise<boolean> {
  // Every native attempt supersedes a pending frame for this slot, including an
  // invalid replacement. Default mode preserves a valid in-flight frame until a
  // validated replacement reaches renderApsCreative.
  if (publisherNativeRendering) cancelPendingApsRendering(slotId, source);
  const dispatch = Symbol(slotId);
  nativeDispatches.set(slotId, dispatch);

  const renderer = validateApsRenderer(input);
  if (!renderer) {
    releaseNativeDispatch(slotId, dispatch);
    log.warn('APS renderer: rejected descriptor');
    return false;
  }
  if (!publisherNativeRendering) {
    try {
      return trustedServer(renderer);
    } finally {
      releaseNativeDispatch(slotId, dispatch);
    }
  }

  let rendering: Promise<boolean>;
  try {
    rendering = renderApsPublisherNative({ slotId, renderer, source });
  } catch {
    releaseNativeDispatch(slotId, dispatch);
    log.warn('APS native renderer: failed to start publisher-origin frame');
    return Promise.resolve(false);
  }

  return rendering.then((accepted) => {
    if (!releaseNativeDispatch(slotId, dispatch)) {
      if (accepted) log.warn('APS native renderer: ignored stale completion');
      return false;
    }
    return accepted;
  });
}

interface RenderApsPublisherNativeOptions {
  slotId: string;
  renderer: unknown;
  source?: MessageEventSource | null;
}

function prepareApsRunnerDocument(
  frameWindow: Window & typeof globalThis,
  frameDocument: Document
): void {
  for (const element of [frameDocument.documentElement, frameDocument.body]) {
    element.style.margin = '0px';
    element.style.padding = '0px';
  }

  const normalizeFrame = (node: Node): void => {
    if (
      node instanceof frameWindow.HTMLIFrameElement &&
      node.parentElement === frameDocument.body
    ) {
      node.style.display = 'block';
    }
  };
  Array.from(frameDocument.body.children).forEach(normalizeFrame);
  new frameWindow.MutationObserver((records) => {
    for (const record of records) record.addedNodes.forEach(normalizeFrame);
  }).observe(frameDocument.body, { childList: true });
}

/** Render the exact selected response through APS's fixed runner in a friendly iframe. */
function renderApsPublisherNative({
  slotId,
  renderer: input,
  source,
}: RenderApsPublisherNativeOptions): Promise<boolean> {
  const renderer = validateApsRenderer(input);
  const container = findApsContainer(slotId, source);
  if (!renderer || !container) {
    log.warn(
      renderer ? 'APS native renderer: slot not found' : 'APS renderer: rejected descriptor'
    );
    return Promise.resolve(false);
  }

  // Keep an already committed creative visible until the replacement runner loads.
  pendingFrameCancels.get(container)?.();
  const iframe = document.createElement('iframe');
  iframe.title = 'Ad content';
  iframe.width = String(renderer.width);
  iframe.height = String(renderer.height);
  iframe.style.border = '0';
  iframe.style.display = 'none';
  activeFrames.set(container, iframe);

  return new Promise<boolean>((resolve) => {
    let settled = false;
    let runner: HTMLScriptElement | undefined;

    const cleanup = (): void => {
      window.clearTimeout(timeoutId);
      runner?.removeEventListener('load', commit);
      runner?.removeEventListener('error', fail);
    };
    const finish = (accepted: boolean, warning?: string): void => {
      if (settled) return;
      settled = true;
      cleanup();
      if (pendingFrameCancels.get(container) === cancel) pendingFrameCancels.delete(container);

      if (!accepted || activeFrames.get(container) !== iframe || !iframe.isConnected) {
        if (activeFrames.get(container) === iframe) activeFrames.delete(container);
        iframe.remove();
        if (warning) log.warn(warning);
        resolve(false);
        return;
      }

      for (const child of Array.from(container.children)) {
        if (child !== iframe) child.remove();
      }
      iframe.style.display = '';
      resolve(true);
    };
    const cancel = (): void => finish(false);
    function fail(): void {
      finish(false, 'APS native renderer: creative runner failed');
    }
    function commit(): void {
      finish(true);
    }

    const timeoutId = window.setTimeout(
      () => finish(false, 'APS native renderer: creative runner timed out'),
      APS_NATIVE_RENDERER_TIMEOUT_MS
    );
    pendingFrameCancels.set(container, cancel);
    container.appendChild(iframe);

    try {
      const frameWindow = iframe.contentWindow as
        | (Window &
            typeof globalThis & {
              _aps: Map<string, { queue: Event[]; store: Map<string, Map<unknown, unknown>> }>;
            })
        | null;
      const frameDocument = iframe.contentDocument;
      if (!frameWindow || !frameDocument) {
        fail();
        return;
      }

      frameDocument.open();
      frameDocument.write(
        '<!doctype html><html><head><meta charset="utf-8">' +
          '<meta name="referrer" content="no-referrer"></head><body></body></html>'
      );
      frameDocument.close();
      prepareApsRunnerDocument(frameWindow, frameDocument);
      frameWindow._aps = new Map();
      frameWindow._aps.set(renderer.accountId, {
        queue: [
          new frameWindow.CustomEvent('prebid/creative/render', {
            detail: { aaxResponse: renderer.aaxResponse, seatBidId: renderer.bidId },
          }),
        ],
        store: new Map([['listeners', new Map()]]),
      });

      runner = frameDocument.createElement('script');
      runner.src = APS_PREBID_CREATIVE_RUNNER_URL;
      runner.addEventListener('load', commit, { once: true });
      runner.addEventListener('error', fail, { once: true });
      frameDocument.head.appendChild(runner);
    } catch {
      fail();
    }
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
    if (event.source !== iframe.contentWindow || !isExactRendererResult(event.data)) {
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
function fail(){if(done)return;done=true;clean();f.remove();reject(new Error("APS renderer frame failed"));}
function receive(e){var m=e.data;if(e.source!==f.contentWindow||!m||m.nonce!==n)return;
if(m.message==="${RENDERER_READY_MESSAGE}"){done=true;clean();resolve();}
else if(m.message==="${RENDERER_FAILED_MESSAGE}")fail();}
f.width=String(r.width);f.height=String(r.height);f.style.border="0";
f.setAttribute("sandbox","${APS_RENDERER_SANDBOX}");
f.src=p.href+"#tsaps="+n;f.onload=function(){if(!done&&f.contentWindow)f.contentWindow.postMessage({nonce:n,renderer:r},"*");};
f.onerror=fail;w.addEventListener("message",receive);t=w.setTimeout(fail,${RENDERER_READY_TIMEOUT_MS});w.document.body.appendChild(f);
}catch(e){reject(e);}});};})();`;
