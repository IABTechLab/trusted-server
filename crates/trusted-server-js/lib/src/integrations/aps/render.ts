import { log } from '../../core/log';
import type { ApsPrebidRendererEntry, ApsRendererV1, TsjsApi } from '../../core/types';
import { validateApsRenderer } from '../../core/contracts/aps_renderer';

const objectFreezeIntrinsic = Object.freeze;

export { parseApsRendererDescriptor, validateApsRenderer } from '../../core/contracts/aps_renderer';

export const APS_RENDERER_PATH = '/integrations/aps/renderer';
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

/** Validate, copy, and freeze one APS tagged render source. */
export function prepareApsRenderSource(input: unknown): Readonly<ApsRendererV1> | undefined {
  try {
    const renderer = validateApsRenderer(input);
    return renderer
      ? (Reflect.apply(objectFreezeIntrinsic, Object, [renderer]) as Readonly<ApsRendererV1>)
      : undefined;
  } catch {
    return undefined;
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isExactRendererResult(value: unknown): value is Record<string, unknown> {
  if (!isRecord(value)) return false;
  const actual = Object.keys(value).sort();
  return actual.length === 2 && actual[0] === 'message' && actual[1] === 'nonce';
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
  lifecycle?: { markWinner(): void; markRendered(): void }
): boolean {
  if (
    !validPrebidAdId(adId) ||
    !validPrebidIdentity(adUnitCode) ||
    typeof lifecycle?.markWinner !== 'function' ||
    typeof lifecycle.markRendered !== 'function'
  ) {
    return false;
  }
  const renderer = prepareApsRenderSource(input);
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
    markWinner: lifecycle.markWinner,
    markRendered: lifecycle.markRendered,
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
    typeof entry.markWinner !== 'function' ||
    typeof entry.markRendered !== 'function'
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

function createNonce(): string | undefined {
  if (typeof crypto === 'undefined' || typeof crypto.getRandomValues !== 'function')
    return undefined;
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  let binary = '';
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/g, '');
}

/** Return the absolute, same-publisher URL used by direct and Universal Creative rendering. */
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
  const renderer = prepareApsRenderSource(input);
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
