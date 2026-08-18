import { log } from '../../core/log';
import type { ApsPrebidRendererEntry, ApsRendererV1, TsjsApi } from '../../core/types';

import APS_RENDERER_CONTAINER_DOCUMENT from './renderer-container.html?raw';
import APS_RENDERER_DOCUMENT from './renderer.html?raw';

export const APS_RENDERER_PATH = '/integrations/aps/renderer';
export const APS_RENDERER_DATA_URL = `data:text/html;charset=utf-8,${encodeURIComponent(
  APS_RENDERER_DOCUMENT
)}`;
const APS_RENDERER_BOOTSTRAP_SANDBOX =
  'allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation';
export const APS_RENDERER_SANDBOX =
  'allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-same-origin allow-scripts allow-top-navigation-by-user-activation';
export const APS_UNIVERSAL_CREATIVE_RENDERER_VERSION = 6;

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
type CommittedApsMount = {
  frame: HTMLIFrameElement;
  hiddenSiblings: Array<{
    element: HTMLElement;
    display: { value: string; priority: string };
  }>;
};
const committedApsMounts = new WeakMap<HTMLElement, CommittedApsMount>();
const RENDERER_READY_MESSAGE = 'trusted-server/aps/renderer-ready';
const RENDERER_FAILED_MESSAGE = 'trusted-server/aps/renderer-failed';
const RENDERER_READY_TIMEOUT_MS = 10_000;
const MAX_PREBID_RENDERER_ENTRIES = 256;
const DEFAULT_PREBID_RENDERER_TTL_SECONDS = 300;
const MAX_PREBID_RENDERER_TTL_SECONDS = 3600;
const MAX_PREBID_ID_BYTES = 1024;
const APS_BOOTSTRAP_READY_MESSAGE = 'trusted-server/aps/bootstrap-ready';
const APS_BOOTSTRAP_NAVIGATE_MESSAGE = 'trusted-server/aps/bootstrap-navigate';
const APS_CONTAINER_READY_MESSAGE = 'trusted-server/aps/container-ready';
const APS_CHANNEL_READY_MESSAGE = 'trusted-server/aps/channel-ready';
const APS_MOUNT_REQUEST_MESSAGE = 'trusted-server/aps/mount-request';
const APS_MOUNT_RESULT_MESSAGE = 'trusted-server/aps/mount-result';
const LEGACY_RENDERER_FALLBACK_MS = 100;
const MAX_PENDING_UNIVERSAL_MOUNTS = 256;

type ValidatedRendererCacheEntry = {
  publisherOrigin: string;
  renderer: ApsRendererV1;
};
const validatedRendererCache = new WeakMap<object, ValidatedRendererCacheEntry>();
type PendingUniversalMount = {
  container: HTMLElement;
  expiresAt: number;
  renderer: ApsRendererV1;
};
const pendingUniversalMounts = new Map<string, PendingUniversalMount>();
let universalMountListenerInstalled = false;

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

function createNonce(): string | undefined {
  if (typeof crypto === 'undefined' || typeof crypto.getRandomValues !== 'function')
    return undefined;
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  let binary = '';
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/g, '');
}

function apsRendererContainerDataUrl(
  renderer: ApsRendererV1,
  rendererDataUrl: string,
  innerNonce: string
): string | undefined {
  try {
    const creativeOrigin = new URL(renderer.creativeUrl).origin;
    const document = APS_RENDERER_CONTAINER_DOCUMENT.replace(
      '__TS_APS_CREATIVE_ORIGIN__',
      creativeOrigin
    )
      .replace('__TS_APS_CREATIVE_ORIGIN_JSON__', JSON.stringify(creativeOrigin))
      .replace('__TS_APS_INNER_NONCE__', JSON.stringify(innerNonce))
      .replace('__TS_APS_RENDERER_SANDBOX__', JSON.stringify(APS_RENDERER_SANDBOX))
      .replace('__TS_APS_RENDERER_URL__', JSON.stringify(rendererDataUrl));
    if (document.includes('__TS_APS_')) return undefined;
    return `data:text/html;charset=utf-8,${encodeURIComponent(document)}`;
  } catch {
    return undefined;
  }
}

type MountApsRendererOptions = {
  mode: 'replace' | 'universal-creative';
  onFailed?: () => void;
  onReady?: () => void;
};

function mountApsRendererFrame(
  container: HTMLElement,
  renderer: ApsRendererV1,
  options: MountApsRendererOptions
): boolean {
  const publisherOrigin = window.location.origin;
  const bootstrapUrl = apsRendererBootstrapUrl(publisherOrigin);
  const nonce = createNonce();
  const innerNonce = createNonce();
  if (!bootstrapUrl || !nonce || !innerNonce) return false;
  const rendererDataUrl = `${APS_RENDERER_DATA_URL}#tsaps=${innerNonce}`;
  const rendererContainerDataUrl = apsRendererContainerDataUrl(
    renderer,
    rendererDataUrl,
    innerNonce
  );
  if (!rendererContainerDataUrl) return false;
  const rendererContainerUrl = `${rendererContainerDataUrl}#tsaps=${nonce}`;

  const iframe = document.createElement('iframe');
  iframe.title = 'Ad content';
  iframe.width = String(renderer.width);
  iframe.height = String(renderer.height);
  iframe.style.border = '0';
  iframe.style.display = 'none';
  iframe.dataset.tsApsRenderer = 'true';
  // The publisher-origin bootstrap starts under the stricter sandbox. It asks
  // the parent to add allow-same-origin only for navigation to a naturally
  // opaque data container whose CSP confines the nested renderer.
  iframe.setAttribute('sandbox', APS_RENDERER_BOOTSTRAP_SANDBOX);
  iframe.src = `${bootstrapUrl}#tsaps=${nonce}`;

  // A replacement must cancel a pending frame, not merely detach it: its
  // message listener and ready timeout would otherwise remain live until expiry.
  pendingFrameCancels.get(container)?.();
  activeFrames.set(container, iframe);

  let phase: 'active' | 'bootstrap' | 'channel' | 'renderer' = 'bootstrap';
  let rendererChannel: MessagePort | undefined;
  let settled = false;
  let legacyDescriptorPosted = false;
  let legacyFallbackId: number | undefined;
  const cleanup = (): void => {
    window.removeEventListener('message', receive);
    iframe.removeEventListener('load', load);
    rendererChannel?.close();
    rendererChannel = undefined;
    window.clearTimeout(timeoutId);
    if (legacyFallbackId !== undefined) window.clearTimeout(legacyFallbackId);
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
    options.onFailed?.();
  };
  const commit = (): void => {
    if (settled || activeFrames.get(container) !== iframe || !iframe.isConnected) return;
    settled = true;
    cleanup();
    if (pendingFrameCancels.get(container) === cancel) pendingFrameCancels.delete(container);

    const hiddenSiblings: CommittedApsMount['hiddenSiblings'] = [];
    for (const child of Array.from(container.children)) {
      if (child === iframe) continue;
      if (options.mode === 'replace' || (child as HTMLElement).dataset.tsApsRenderer === 'true') {
        child.remove();
      } else if (child instanceof HTMLElement) {
        // Keep Universal Creative connected long enough to receive the success
        // acknowledgement, but never leave two visible rendering surfaces.
        hiddenSiblings.push({
          element: child,
          display: {
            value: child.style.getPropertyValue('display'),
            priority: child.style.getPropertyPriority('display'),
          },
        });
        child.style.setProperty('display', 'none', 'important');
      }
    }
    committedApsMounts.set(container, { frame: iframe, hiddenSiblings });
    iframe.style.display = '';
    options.onReady?.();
  };
  const postLegacyDescriptor = (): void => {
    const target = iframe.contentWindow;
    if (!target) {
      fail();
      return;
    }
    // The query-free renderer retained for rollback accepts this exact legacy
    // shape. Keep the modern bootstrap phase live in case its ready message is
    // delayed past the compatibility timer.
    legacyDescriptorPosted = true;
    target.postMessage({ nonce, renderer }, '*');
  };
  function receiveChannel(event: MessageEvent): void {
    if (!hasExactKeys(event.data, ['message', 'nonce']) || event.data.nonce !== innerNonce) return;
    if (event.data.message === APS_CHANNEL_READY_MESSAGE && phase === 'channel') {
      phase = 'active';
      rendererChannel?.postMessage({ nonce: innerNonce, publisherOrigin, renderer });
    } else if (event.data.message === RENDERER_READY_MESSAGE && phase === 'active') {
      commit();
    } else if (event.data.message === RENDERER_FAILED_MESSAGE) {
      fail();
    }
  }
  function receive(event: MessageEvent): void {
    if (event.source !== iframe.contentWindow || !hasExactKeys(event.data, ['message', 'nonce'])) {
      return;
    }
    if (event.data.nonce !== nonce) return;
    if (event.data.message === APS_BOOTSTRAP_READY_MESSAGE && phase === 'bootstrap') {
      if (legacyFallbackId !== undefined) window.clearTimeout(legacyFallbackId);
      phase = 'renderer';
      iframe.setAttribute('sandbox', APS_RENDERER_SANDBOX);
      iframe.contentWindow?.postMessage(
        { message: APS_BOOTSTRAP_NAVIGATE_MESSAGE, nonce, rendererUrl: rendererContainerUrl },
        '*'
      );
    } else if (event.data.message === APS_CONTAINER_READY_MESSAGE && phase === 'renderer') {
      if (event.ports.length !== 1) {
        fail();
        return;
      }
      phase = 'channel';
      rendererChannel = event.ports[0];
      rendererChannel.onmessage = receiveChannel;
      rendererChannel.start();
    } else if (
      event.data.message === RENDERER_READY_MESSAGE &&
      phase === 'bootstrap' &&
      legacyDescriptorPosted
    ) {
      commit();
    } else if (event.data.message === RENDERER_FAILED_MESSAGE) {
      fail();
    }
  }
  function load(): void {
    if (settled || activeFrames.get(container) !== iframe || !iframe.isConnected) return;
    try {
      if (phase === 'bootstrap' && legacyFallbackId === undefined) {
        legacyFallbackId = window.setTimeout(() => {
          if (phase !== 'bootstrap' || settled) return;
          postLegacyDescriptor();
        }, LEGACY_RENDERER_FALLBACK_MS);
      }
    } catch {
      fail();
    }
  }

  window.addEventListener('message', receive);
  iframe.addEventListener('load', load);
  iframe.addEventListener('error', fail, { once: true });

  const timeoutId = window.setTimeout(fail, RENDERER_READY_TIMEOUT_MS);
  pendingFrameCancels.set(container, cancel);
  container.appendChild(iframe);
  return true;
}

/** Cancel pending or committed APS work before another renderer replaces this container. */
export function cancelPendingApsRender(container: HTMLElement): void {
  pendingFrameCancels.get(container)?.();

  const committed = committedApsMounts.get(container);
  if (committed) {
    committedApsMounts.delete(container);
    if (activeFrames.get(container) === committed.frame) activeFrames.delete(container);
    committed.frame.remove();
    for (const sibling of committed.hiddenSiblings) {
      if (sibling.element.parentElement === container) {
        if (sibling.display.value) {
          sibling.element.style.setProperty(
            'display',
            sibling.display.value,
            sibling.display.priority
          );
        } else {
          sibling.element.style.removeProperty('display');
        }
      }
    }
  }

  for (const [mountId, entry] of pendingUniversalMounts) {
    if (entry.container === container) pendingUniversalMounts.delete(mountId);
  }
}

function prunePendingUniversalMounts(now: number): void {
  for (const [mountId, entry] of pendingUniversalMounts) {
    if (entry.expiresAt <= now || !entry.container.isConnected) {
      pendingUniversalMounts.delete(mountId);
    }
  }
}

/**
 * Register a one-shot bearer capability for Universal Creative to request a top-page mount.
 *
 * Real PUC asks from a nested dynamic-renderer frame rather than the controller
 * that received the authenticated bridge response, so source-window equality
 * cannot survive the handoff. The 128-bit ID is disclosed only in that response,
 * consumed once, and a newer registration revokes the same container's old ID.
 */
export function registerApsUniversalCreativeMount(
  container: HTMLElement,
  input: unknown
): string | undefined {
  const renderer = validateApsRenderer(input);
  if (!renderer || !container.isConnected) return undefined;

  const now = Date.now();
  prunePendingUniversalMounts(now);

  // Validate every synchronous failure before replacing an already committed
  // creative. A successful re-registration still revokes this container's old
  // pending capability, but capacity consumed by other slots must not fork it.
  const mountId = createNonce();
  if (!mountId || pendingUniversalMounts.has(mountId)) return undefined;
  const pendingForContainer = Array.from(pendingUniversalMounts.values()).filter(
    (entry) => entry.container === container
  ).length;
  if (pendingUniversalMounts.size - pendingForContainer >= MAX_PENDING_UNIVERSAL_MOUNTS) {
    return undefined;
  }

  cancelPendingApsRender(container);
  pendingUniversalMounts.set(mountId, {
    container,
    expiresAt: now + RENDERER_READY_TIMEOUT_MS,
    renderer,
  });
  if (!universalMountListenerInstalled) {
    universalMountListenerInstalled = true;
    window.addEventListener('message', receiveUniversalMountRequest);
  }
  return mountId;
}

function sendUniversalMountResult(
  target: MessageEventSource,
  mountId: string,
  nonce: string,
  status: 'failed' | 'ready'
): void {
  if (!('postMessage' in target)) return;
  try {
    (target as Window).postMessage(
      { message: APS_MOUNT_RESULT_MESSAGE, mountId, nonce, status },
      '*'
    );
  } catch {
    // The requesting frame may have been detached while APS was loading.
  }
}

function receiveUniversalMountRequest(event: MessageEvent): void {
  if (
    !hasExactKeys(event.data, ['message', 'mountId', 'nonce']) ||
    event.data.message !== APS_MOUNT_REQUEST_MESSAGE ||
    typeof event.data.mountId !== 'string' ||
    typeof event.data.nonce !== 'string' ||
    !/^[A-Za-z0-9_-]{22}$/.test(event.data.mountId) ||
    !/^[A-Za-z0-9_-]{22}$/.test(event.data.nonce) ||
    !event.source
  ) {
    return;
  }

  const entry = pendingUniversalMounts.get(event.data.mountId);
  if (!entry) return;
  pendingUniversalMounts.delete(event.data.mountId);

  const { mountId, nonce } = event.data;
  if (entry.expiresAt <= Date.now() || !entry.container.isConnected) {
    sendUniversalMountResult(event.source, mountId, nonce, 'failed');
    return;
  }

  if (
    !mountApsRendererFrame(entry.container, entry.renderer, {
      mode: 'universal-creative',
      onFailed: () => sendUniversalMountResult(event.source!, mountId, nonce, 'failed'),
      onReady: () => sendUniversalMountResult(event.source!, mountId, nonce, 'ready'),
    })
  ) {
    sendUniversalMountResult(event.source, mountId, nonce, 'failed');
  }
}

/**
 * Return the absolute same-publisher URL retained for cached-client compatibility.
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

export function apsRendererBootstrapUrl(pageOrigin = window.location.origin): string | undefined {
  const rendererUrl = apsRendererUrl(pageOrigin);
  if (!rendererUrl) return undefined;
  const url = new URL(rendererUrl);
  url.search = 'mode=data-bootstrap';
  return url.href;
}

export interface RenderApsCreativeOptions {
  slotId: string;
  renderer: unknown;
}

/** Render APS through nested, naturally opaque data documents under an outer sandbox. */
export function renderApsCreative({ slotId, renderer: input }: RenderApsCreativeOptions): boolean {
  const renderer = validateApsRenderer(input);
  if (!renderer) {
    log.warn('APS renderer: rejected descriptor');
    return false;
  }

  const container = document.getElementById(slotId);
  if (!container) {
    log.warn('APS renderer: slot not found');
    return false;
  }

  const mounted = mountApsRendererFrame(container, renderer, {
    mode: 'replace',
    onFailed: () => log.warn('APS renderer: frame load failed'),
  });
  if (!mounted) log.warn('APS renderer: failed to initialize frame');
  return mounted;
}

/**
 * Static source executed by Prebid Universal Creative's hidden dynamic-renderer frame.
 * A one-shot capability asks trusted top-page TSJS to mount the actual data renderer
 * outside inherited GAM and Universal Creative sandbox restrictions.
 */
export const APS_UNIVERSAL_CREATIVE_RENDERER = String.raw`(function(){window.render=function(d){return new Promise(function(resolve,reject){
try{var i=d&&d.apsMountId,o=d&&d.publisherOrigin;if(typeof i!=="string"||!/^[A-Za-z0-9_-]{22}$/.test(i)||typeof o!=="string")throw new Error("invalid APS mount data");
var p=new URL(o);if((p.protocol!=="https:"&&p.protocol!=="http:")||p.username||p.password||p.origin!==o||p.pathname!=="/"||p.search||p.hash)throw new Error("invalid publisher origin");
var c=window.crypto;if(!c||typeof c.getRandomValues!=="function")throw new Error("APS renderer randomness unavailable");
var b=new Uint8Array(16);c.getRandomValues(b);var s="";for(var j=0;j<b.length;j++)s+=String.fromCharCode(b[j]);
var n=window.btoa(s).replace(/\+/g,"-").replace(/\//g,"_").replace(/=+$/g,""),done=false,t;
function clean(){window.removeEventListener("message",receive);if(t)window.clearTimeout(t);}
function fail(){if(done)return;done=true;clean();reject(new Error("APS top-page mount failed"));}
function receive(e){var m=e.data;if(e.source!==top||!m||m.message!=="${APS_MOUNT_RESULT_MESSAGE}"||m.mountId!==i||m.nonce!==n)return;
if(m.status==="ready"){done=true;clean();resolve();}else if(m.status==="failed")fail();}
window.addEventListener("message",receive);t=window.setTimeout(fail,${RENDERER_READY_TIMEOUT_MS});
top.postMessage({message:"${APS_MOUNT_REQUEST_MESSAGE}",mountId:i,nonce:n},o);
}catch(e){reject(e);}});};})();`;
