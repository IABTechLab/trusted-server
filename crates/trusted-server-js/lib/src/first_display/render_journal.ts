import { PUC_DYNAMIC_OWNER } from '../kernel/contracts/puc_dynamic_owner';
import type { FirstDisplaySliceActivationContext } from '../shared/first_display_transaction';

import type {
  FirstDisplayGptBoundCycleV1,
  FirstDisplayGptRenderResult,
} from './adapters/googletag';
import type { FirstDisplayRenderBridgeV1, FirstDisplayRenderHandoffArtifactV1 } from './driver';

const ADM_SANDBOX =
  'allow-forms allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation';
const INTERNAL_ERROR = 'internal_error';
const ADM_DOCUMENT_NO_LOAD = 'adm_document_no_load';
const NAVIGATION_DISPOSED = 'navigation_disposed';
const CLAIM_DEADLINE_MS = 3_000;
const INSERTION_DEADLINE_MS = 1_000;
const ADM_LOAD_DEADLINE_MS = 5_000;
const TICKET_TTL_MS = 3_000;
const RESERVATION_TTL_MS = 15 * 60 * 1_000;
const MAX_CAPABILITIES = 320;
const MAX_DRAWS = 8;
const MAX_GLOBAL_MESSAGE_BYTES = 4_096;
const MAX_DOMAIN_BYTES = 2_048;
const MAX_OWNER_BYTES = 64 * 1_024;
const MAX_RESPONSE_BYTES = 72 * 1_024;
const RESERVATION_ID = /^r1_[A-Za-z0-9_-]{22}$/;
const TICKET_ID = /^t1_[A-Za-z0-9_-]{22}$/;
const BASE64URL = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_';
const textEncoder = new TextEncoder();

export interface FirstDisplayPortLikeV1 {
  readonly addEventListener?: (name: string, listener: (event: unknown) => void) => void;
  readonly close: () => void;
  readonly postMessage: (message: unknown, transfer?: readonly FirstDisplayPortLikeV1[]) => void;
  readonly removeEventListener?: (name: string, listener: (event: unknown) => void) => void;
  readonly start?: () => void;
}

export interface FirstDisplayChannelLikeV1 {
  readonly port1: FirstDisplayPortLikeV1;
  readonly port2: FirstDisplayPortLikeV1;
}

export interface FirstDisplayRenderOwnerOptionsV1 {
  readonly browser: Window;
  readonly clearTimer: (handle: unknown) => void;
  readonly createChannel: () => FirstDisplayChannelLikeV1;
  readonly document: Document;
  readonly fillRandom: (bytes: Uint8Array) => void;
  readonly now: () => number;
  readonly onNativeMutation?: () => boolean;
  readonly setTimer: (callback: () => void, delayMs: number) => unknown;
}

export interface FirstDisplayCommittedRenderArtifactV1 extends FirstDisplayRenderHandoffArtifactV1 {
  readonly current: () => boolean;
  readonly retire: () => void;
}

export interface FirstDisplayRenderStrategyCallbacksV1 {
  readonly accept: (artifact: FirstDisplayCommittedRenderArtifactV1) => void;
  readonly fail: (reason: string) => void;
}

export interface FirstDisplayRenderStrategyAttemptV1 {
  readonly cancel: () => void;
}

/** Source-specific authority is narrowed before it reaches the render owner. */
export interface FirstDisplayRenderStrategyV1 {
  readonly supports: (source: unknown) => boolean;
  readonly start: (
    cycle: FirstDisplayGptBoundCycleV1,
    overlay: boolean,
    callbacks: FirstDisplayRenderStrategyCallbacksV1
  ) => FirstDisplayRenderStrategyAttemptV1 | undefined;
  readonly dispose: () => void;
}

export interface FirstDisplayRenderOwnerProtocolV1 {
  readonly version: 1;
  readonly id: 'render_owner';
  readonly createRenderBridge: (
    options: FirstDisplayRenderOwnerOptionsV1,
    strategy?: FirstDisplayRenderStrategyV1
  ) => FirstDisplayRenderBridgeV1;
}

interface RenderOwnerInitialBindings {
  readonly observe: (name: 'protocol_version', value: number) => void;
  readonly register: (protocol: FirstDisplayRenderOwnerProtocolV1) => () => void;
}

interface PendingClaim {
  readonly port: FirstDisplayPortLikeV1;
  readonly source: object;
}

interface Attempt {
  readonly cycle: FirstDisplayGptBoundCycleV1;
  readonly onTerminal: (result: 'accepted' | 'failed' | 'cancelled', reason: string | null) => void;
  readonly reservationId: string;
  active: boolean;
  claim: PendingClaim | undefined;
  claimTimer: unknown;
  controlPort: FirstDisplayPortLikeV1 | undefined;
  controlRelease: (() => void) | undefined;
  execution: FirstDisplayRenderStrategyAttemptV1 | undefined;
  gam: FirstDisplayGptRenderResult | undefined;
  insertionTimer: unknown;
  inserted: boolean;
  ownerSource: object | undefined;
  ownerTicket: string | undefined;
  phaseValue:
    | 'waiting_for_gam_and_claim'
    | 'waiting_for_owner'
    | 'waiting_for_insertion'
    | 'rendering_direct';
  ticket: string | undefined;
}

interface LiveTicket {
  readonly attempt: Attempt;
  readonly expiresAtInternal: number;
  readonly ordinalInternal: number;
  readonly registryState: 'live';
  timer?: unknown;
}

interface TicketTombstone {
  readonly expiresAtInternal: number;
  readonly ordinalInternal: number;
  readonly registryState: 'tombstone';
  timer?: unknown;
}

type TicketEntry = LiveTicket | TicketTombstone;

interface ReservationEntry {
  readonly expiresAtInternal: number;
  readonly ordinalInternal: number;
  readonly registryState: 'live' | 'tombstone';
}

function utf8Length(value: string): number {
  return textEncoder.encode(value).byteLength;
}

function exactRecord(value: unknown, keys: readonly string[]): Record<string, unknown> | undefined {
  try {
    if (
      typeof value !== 'object' ||
      value === null ||
      Array.isArray(value) ||
      Object.getPrototypeOf(value) !== Object.prototype ||
      Object.getOwnPropertySymbols(value).length !== 0
    ) {
      return undefined;
    }
    const names = Object.getOwnPropertyNames(value).sort();
    const expected = [...keys].sort();
    if (names.length !== expected.length) return undefined;
    const result: Record<string, unknown> = {};
    for (let index = 0; index < expected.length; index += 1) {
      const name = expected[index];
      if (!name || names[index] !== name) return undefined;
      const descriptor = Object.getOwnPropertyDescriptor(value, name);
      if (!descriptor?.enumerable || !('value' in descriptor)) return undefined;
      result[name] = descriptor.value;
    }
    return result;
  } catch {
    return undefined;
  }
}

function parseJson(source: string): unknown {
  if (utf8Length(source) > MAX_GLOBAL_MESSAGE_BYTES) return undefined;
  try {
    return JSON.parse(source) as unknown;
  } catch {
    return undefined;
  }
}

function routingMessage(data: unknown): Readonly<{
  adId?: string;
  lifecycleTicket?: string;
  message?: string;
}> {
  const value = typeof data === 'string' ? parseJson(data) : data;
  try {
    if (
      typeof value !== 'object' ||
      value === null ||
      Array.isArray(value) ||
      Object.getPrototypeOf(value) !== Object.prototype
    ) {
      return {};
    }
    const read = (name: string): string | undefined => {
      const descriptor = Object.getOwnPropertyDescriptor(value, name);
      return descriptor?.enumerable && 'value' in descriptor && typeof descriptor.value === 'string'
        ? descriptor.value
        : undefined;
    };
    const message = read('message');
    const adId = read('adId');
    const lifecycleTicket = read('lifecycleTicket');
    return {
      ...(message ? { message } : {}),
      ...(adId ? { adId } : {}),
      ...(lifecycleTicket ? { lifecycleTicket } : {}),
    };
  } catch {
    return {};
  }
}

function exactPrebidRequest(data: unknown): Record<string, unknown> | undefined {
  if (typeof data !== 'string') return undefined;
  const fields = exactRecord(parseJson(data), ['message', 'adId', 'adServerDomain']);
  return fields?.message === 'Prebid Request' &&
    typeof fields.adId === 'string' &&
    RESERVATION_ID.test(fields.adId) &&
    typeof fields.adServerDomain === 'string' &&
    fields.adServerDomain.length > 0 &&
    utf8Length(fields.adServerDomain) <= MAX_DOMAIN_BYTES &&
    data ===
      JSON.stringify({
        message: 'Prebid Request',
        adId: fields.adId,
        adServerDomain: fields.adServerDomain,
      })
    ? fields
    : undefined;
}

function exactOwnerRegistration(data: unknown): Record<string, unknown> | undefined {
  if (typeof data !== 'string') return undefined;
  const fields = exactRecord(parseJson(data), ['message', 'adId', 'version', 'lifecycleTicket']);
  return fields?.message === 'TS Render Owner Register' &&
    fields.version === 1 &&
    typeof fields.adId === 'string' &&
    RESERVATION_ID.test(fields.adId) &&
    typeof fields.lifecycleTicket === 'string' &&
    TICKET_ID.test(fields.lifecycleTicket) &&
    data ===
      JSON.stringify({
        message: 'TS Render Owner Register',
        adId: fields.adId,
        version: 1,
        lifecycleTicket: fields.lifecycleTicket,
      })
    ? fields
    : undefined;
}

function eventField(
  event: unknown,
  name: 'data' | 'ports' | 'source',
  trustedPrototype: object | undefined
): unknown {
  try {
    if (typeof event !== 'object' || event === null) return undefined;
    const own = Object.getOwnPropertyDescriptor(event, name);
    if (own) return 'value' in own ? own.value : undefined;
    const prototype = Object.getPrototypeOf(event);
    if (!trustedPrototype || prototype !== trustedPrototype) return undefined;
    const inherited = Object.getOwnPropertyDescriptor(trustedPrototype, name);
    return inherited?.get ? Reflect.apply(inherited.get, event, []) : undefined;
  } catch {
    return undefined;
  }
}

function usablePort(value: unknown): value is FirstDisplayPortLikeV1 {
  try {
    return (
      typeof value === 'object' &&
      value !== null &&
      typeof Reflect.get(value, 'postMessage') === 'function' &&
      typeof Reflect.get(value, 'close') === 'function'
    );
  } catch {
    return false;
  }
}

function inspectPorts(
  event: unknown,
  trustedPrototype: object | undefined
):
  | Readonly<{
      exact: boolean;
      originalCount: number;
      ports: readonly FirstDisplayPortLikeV1[];
    }>
  | undefined {
  const value = eventField(event, 'ports', trustedPrototype);
  try {
    if (!Array.isArray(value)) return undefined;
    const length = Object.getOwnPropertyDescriptor(value, 'length');
    if (!length || !('value' in length) || !Number.isSafeInteger(length.value)) return undefined;
    let exact =
      Object.getPrototypeOf(value) === Array.prototype &&
      Object.getOwnPropertySymbols(value).length === 0 &&
      Object.getOwnPropertyNames(value).length === length.value + 1;
    const ports: FirstDisplayPortLikeV1[] = [];
    const seen = new Set<FirstDisplayPortLikeV1>();
    for (let index = 0; index < length.value; index += 1) {
      const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
      if (!descriptor?.enumerable || !('value' in descriptor) || !usablePort(descriptor.value)) {
        exact = false;
        continue;
      }
      if (seen.has(descriptor.value)) {
        exact = false;
        continue;
      }
      seen.add(descriptor.value);
      ports.push(descriptor.value);
    }
    return { exact, originalCount: length.value, ports };
  } catch {
    return undefined;
  }
}

function eventSource(event: unknown, trustedPrototype: object | undefined): object | undefined {
  const value = eventField(event, 'source', trustedPrototype);
  return (typeof value === 'object' || typeof value === 'function') && value !== null
    ? value
    : undefined;
}

function suppress(event: unknown): boolean {
  try {
    if (typeof event !== 'object' || event === null) return false;
    const stop = Reflect.get(event, 'stopImmediatePropagation');
    if (typeof stop !== 'function') return false;
    Reflect.apply(stop, event, []);
    return true;
  } catch {
    return false;
  }
}

function closePort(port: FirstDisplayPortLikeV1 | undefined): void {
  try {
    port?.close();
  } catch {
    // The endpoint is already generation-inert.
  }
}

function post(
  port: FirstDisplayPortLikeV1,
  data: unknown,
  transfer: readonly FirstDisplayPortLikeV1[] = []
): boolean {
  try {
    Reflect.apply(port.postMessage, port, [data, transfer]);
    return true;
  } catch {
    return false;
  }
}

function installPortListeners(
  port: FirstDisplayPortLikeV1,
  receive: (event: unknown) => void,
  receiveError: () => void,
  publishRelease: (release: () => void) => void
): boolean {
  try {
    if (typeof port.addEventListener !== 'function') return false;
    let live = true;
    const release = (): void => {
      if (!live) return;
      live = false;
      try {
        if (typeof port.removeEventListener === 'function') {
          Reflect.apply(port.removeEventListener, port, ['message', receive]);
          Reflect.apply(port.removeEventListener, port, ['messageerror', receiveError]);
        }
      } catch {
        // Port closure remains authoritative.
      }
    };
    Reflect.apply(port.addEventListener, port, ['message', receive]);
    Reflect.apply(port.addEventListener, port, ['messageerror', receiveError]);
    publishRelease(release);
    if (typeof port.start === 'function') Reflect.apply(port.start, port, []);
    return live;
  } catch {
    return false;
  }
}

function encodeOpaque(bytes: Uint8Array): string {
  let output = '';
  let buffer = 0;
  let bits = 0;
  for (const byte of bytes) {
    buffer = (buffer << 8) | byte;
    bits += 8;
    while (bits >= 6) {
      bits -= 6;
      output += BASE64URL[(buffer >>> bits) & 63];
    }
    buffer &= (1 << bits) - 1;
  }
  if (bits > 0) output += BASE64URL[(buffer << (6 - bits)) & 63];
  return output;
}

function refusedResponse(adId: string): string {
  return JSON.stringify({
    message: 'Prebid Response',
    adId,
    rendererVersion: '4',
    tsOwner: { version: 1, status: 'refused' },
  });
}

function ownerRefused(adId: string): string {
  return JSON.stringify({ message: 'TS Render Owner Refused', adId, version: 1 });
}

function snapshotFrameAttributes(frame: HTMLIFrameElement): string | undefined {
  try {
    return JSON.stringify(
      [...frame.attributes]
        .map((attribute) => [attribute.name, attribute.value] as const)
        .sort(([left], [right]) => left.localeCompare(right))
    );
  } catch {
    return undefined;
  }
}

function exactPublisherFrame(document: Document, source: object): HTMLIFrameElement | undefined {
  try {
    const frames = document.querySelectorAll('iframe');
    let selected: HTMLIFrameElement | undefined;
    for (let index = 0; index < frames.length; index += 1) {
      const candidate = frames.item(index);
      if (candidate?.isConnected && candidate.contentWindow === source) {
        if (selected) return undefined;
        selected = candidate;
      }
    }
    return selected;
  } catch {
    return undefined;
  }
}

function resizeCollapsedPucShell(
  document: Document,
  source: object,
  width: number,
  height: number
): boolean {
  try {
    const browser = document.defaultView;
    const selected = exactPublisherFrame(document, source);
    if (!browser || !selected || width <= 0 || height <= 0) return false;
    const onePixelAttribute = (element: Element, name: 'width' | 'height'): boolean => {
      const value = element.getAttribute(name);
      if (value === null || !/^\d+(?:\.\d+)?$/.test(value)) return false;
      const parsed = Number(value);
      return Number.isFinite(parsed) && parsed <= 1;
    };
    const ordinaryCollapsed = (element: HTMLElement): boolean => {
      const style = browser.getComputedStyle(element);
      const pixel = (value: string): boolean => {
        const match = /^(\d+(?:\.\d+)?)px$/.exec(value);
        return match !== null && Number(match[1]) <= 1;
      };
      return (
        style.position !== 'fixed' &&
        style.position !== 'sticky' &&
        pixel(style.width) &&
        pixel(style.height)
      );
    };
    if (
      !onePixelAttribute(selected, 'width') ||
      !onePixelAttribute(selected, 'height') ||
      !ordinaryCollapsed(selected) ||
      selected.closest('a,[data-anchor-status]') !== null
    )
      return false;
    const wrapper = selected.parentElement;
    if (
      !wrapper ||
      wrapper === document.body ||
      wrapper === document.documentElement ||
      wrapper.tagName === 'A' ||
      !wrapper.isConnected ||
      !ordinaryCollapsed(wrapper) ||
      wrapper.closest('a,[data-anchor-status]') !== null
    )
      return false;
    selected.style.setProperty('width', `${width}px`);
    selected.style.setProperty('height', `${height}px`);
    wrapper.style.setProperty('width', `${width}px`);
    wrapper.style.setProperty('height', `${height}px`);
    return true;
  } catch {
    return false;
  }
}

function configureAdmFrame(frame: HTMLIFrameElement, width: number, height: number): void {
  frame.setAttribute('sandbox', ADM_SANDBOX);
  frame.setAttribute('referrerpolicy', 'no-referrer');
  frame.setAttribute('width', String(width));
  frame.setAttribute('height', String(height));
  frame.setAttribute('scrolling', 'no');
  frame.setAttribute('frameborder', '0');
  frame.setAttribute('marginwidth', '0');
  frame.setAttribute('marginheight', '0');
  frame.setAttribute('title', 'Ad content');
  frame.setAttribute('aria-label', 'Advertisement');
  frame.setAttribute(
    'style',
    `border: 0; margin: 0; overflow: hidden; display: block; width: ${width}px; height: ${height}px;`
  );
}

function validCycle(
  cycle: FirstDisplayGptBoundCycleV1,
  strategy: FirstDisplayRenderStrategyV1 | undefined
): boolean {
  try {
    const source = cycle.bid.renderSource;
    const document = cycle.element.ownerDocument;
    let exactElementMatches = 0;
    const elements = document.getElementsByTagName('*');
    for (let index = 0; index < elements.length; index += 1) {
      const candidate = elements.item(index);
      if (candidate?.id !== cycle.element.id) continue;
      if (candidate !== cycle.element) return false;
      exactElementMatches += 1;
    }
    return (
      cycle.isCurrent() &&
      cycle.slotId === cycle.bid.slot &&
      cycle.slotId === cycle.placement.slot &&
      exactElementMatches === 1 &&
      document.getElementById(cycle.element.id) === cycle.element &&
      RESERVATION_ID.test(cycle.bid.rendererReservationId) &&
      (source.type === 'adm' || strategy?.supports(source) === true)
    );
  } catch {
    return false;
  }
}

function publisherArtifact(
  document: Document,
  attempt: Attempt
): FirstDisplayCommittedRenderArtifactV1 | undefined {
  const source = attempt.ownerSource;
  if (!source) return undefined;
  const frame = exactPublisherFrame(document, source);
  const attributes = frame ? snapshotFrameAttributes(frame) : undefined;
  const parent = frame?.parentNode;
  const frameWindow = frame?.contentWindow;
  if (!frame || attributes === undefined || !parent || !frameWindow) return undefined;
  return Object.freeze({
    hostPosition: null,
    hostPositionPriority: null,
    identity: frame,
    kind: 'gpt_adm' as const,
    owner: 'publisher' as const,
    slotId: attempt.cycle.slotId,
    token: attempt.reservationId,
    current: () => {
      try {
        return (
          frame.ownerDocument === document &&
          frame.isConnected &&
          frame.parentNode === parent &&
          frame.contentWindow === frameWindow &&
          frame.contentWindow === source &&
          snapshotFrameAttributes(frame) === attributes
        );
      } catch {
        return false;
      }
    },
    retire: () => undefined,
  });
}

function directAdmExecution(
  document: Document,
  cycle: FirstDisplayGptBoundCycleV1,
  callbacks: FirstDisplayRenderStrategyCallbacksV1,
  setTimer: (callback: () => void, delayMs: number) => unknown,
  clearTimer: (handle: unknown) => void
): FirstDisplayRenderStrategyAttemptV1 | undefined {
  const source = cycle.bid.renderSource;
  if (source.type !== 'adm') return undefined;
  let live = true;
  let timer: unknown;
  let frame: HTMLIFrameElement | undefined;
  const retire = (): void => {
    if (!live) return;
    live = false;
    try {
      if (timer !== undefined) clearTimer(timer);
    } catch {
      // Exact-node retirement remains authoritative.
    }
    if (!frame) return;
    frame.onload = null;
    frame.onerror = null;
    try {
      frame.remove();
    } catch {
      // The exact node cannot regain authority.
    }
  };
  try {
    const created = document.createElement('iframe');
    frame = created;
    configureAdmFrame(created, source.width, source.height);
    const intended = `<!doctype html><html><head><meta charset="utf-8"><meta name="referrer" content="no-referrer"><style>html,body{border:0;margin:0;padding:0;overflow:hidden}</style></head><body>${source.adm}</body></html>`;
    created.onload = () => {
      if (!live) return;
      const attributes = snapshotFrameAttributes(created);
      const frameWindow = created.contentWindow;
      if (
        !cycle.isCurrent() ||
        created.parentNode !== cycle.element ||
        created.srcdoc !== intended ||
        created.getAttribute('src') !== null ||
        attributes === undefined ||
        !frameWindow
      ) {
        callbacks.fail(ADM_DOCUMENT_NO_LOAD);
        return;
      }
      if (timer !== undefined) clearTimer(timer);
      timer = undefined;
      created.onload = null;
      created.onerror = null;
      callbacks.accept(
        Object.freeze({
          hostPosition: null,
          hostPositionPriority: null,
          identity: created,
          kind: 'gpt_adm' as const,
          owner: 'trusted_server' as const,
          slotId: cycle.slotId,
          token: cycle.bid.rendererReservationId,
          current: () => {
            try {
              return (
                live &&
                cycle.isCurrent() &&
                created.isConnected &&
                created.parentNode === cycle.element &&
                created.contentWindow === frameWindow &&
                created.srcdoc === intended &&
                created.getAttribute('src') === null &&
                snapshotFrameAttributes(created) === attributes
              );
            } catch {
              return false;
            }
          },
          retire,
        })
      );
    };
    created.onerror = () => callbacks.fail(ADM_DOCUMENT_NO_LOAD);
    created.srcdoc = intended;
    cycle.element.appendChild(created);
    let scheduling = true;
    let firedSynchronously = false;
    timer = setTimer(() => {
      if (scheduling) {
        firedSynchronously = true;
        return;
      }
      if (live) callbacks.fail(ADM_DOCUMENT_NO_LOAD);
    }, ADM_LOAD_DEADLINE_MS);
    scheduling = false;
    if (timer === undefined || firedSynchronously) {
      if (timer !== undefined) clearTimer(timer);
      retire();
      return undefined;
    }
    return Object.freeze({ cancel: retire });
  } catch {
    retire();
    return undefined;
  }
}

/** Own the bounded, source-neutral render journal for one first-display batch. */
export function createFirstDisplayRenderJournal(
  options: FirstDisplayRenderOwnerOptionsV1,
  strategy?: FirstDisplayRenderStrategyV1
): FirstDisplayRenderBridgeV1 {
  const attempts = new Map<string, Attempt>();
  const reservations = new Map<string, ReservationEntry>();
  const tickets = new Map<string, TicketEntry>();
  const committed = new Map<string, FirstDisplayCommittedRenderArtifactV1>();
  const timers = new Set<unknown>();
  let disposed = false;
  let sealed = false;
  let ingressClosed = false;
  let handoffCaptured = false;
  let committedArtifactsDetached = false;
  let nextTicketOrdinal = 1;
  let nextReservationOrdinal = 1;
  let lastNow = Number.NEGATIVE_INFINITY;
  const messageEventPrototype = (() => {
    try {
      const constructor = Reflect.get(options.browser, 'MessageEvent');
      const prototype =
        typeof constructor === 'function' ? Reflect.get(constructor, 'prototype') : undefined;
      return typeof prototype === 'object' && prototype !== null ? prototype : undefined;
    } catch {
      return undefined;
    }
  })();

  const readNow = (): number | undefined => {
    try {
      const value = options.now();
      if (!Number.isFinite(value) || value < 0 || value < lastNow) return undefined;
      lastNow = value;
      return value;
    } catch {
      return undefined;
    }
  };

  const notifyNativeMutation = (): void => {
    try {
      options.onNativeMutation?.();
    } catch {
      // Observation cannot alter the admitted event.
    }
  };

  const clearOwnedTimer = (handle: unknown): void => {
    if (handle === undefined || !timers.delete(handle)) return;
    try {
      options.clearTimer(handle);
    } catch {
      // Timer state is already generation-inert.
    }
  };

  const arm = (callback: () => void, delayMs: number): unknown => {
    let handle: unknown;
    let scheduling = true;
    let firedSynchronously = false;
    try {
      handle = options.setTimer(() => {
        if (scheduling) {
          firedSynchronously = true;
          return;
        }
        if (!timers.delete(handle)) return;
        callback();
      }, delayMs);
    } catch {
      handle = undefined;
    }
    scheduling = false;
    if (handle === undefined) return undefined;
    if (firedSynchronously) {
      try {
        options.clearTimer(handle);
      } catch {
        // Synchronous timers are refused regardless of cleanup outcome.
      }
      return undefined;
    }
    timers.add(handle);
    return handle;
  };

  const mint = (registry: ReadonlyMap<string, unknown>): string | undefined => {
    for (let draw = 0; draw < MAX_DRAWS; draw += 1) {
      const bytes = new Uint8Array(16);
      try {
        options.fillRandom(bytes);
      } catch {
        return undefined;
      }
      const candidate = `t1_${encodeOpaque(bytes)}`;
      if (!registry.has(candidate)) return candidate;
    }
    return undefined;
  };

  const retireTicket = (attempt: Attempt): void => {
    const ticket = attempt.ticket;
    attempt.ticket = undefined;
    if (!ticket) return;
    const entry = tickets.get(ticket);
    if (entry?.registryState !== 'live' || entry.attempt !== attempt) return;
    tickets.set(ticket, {
      registryState: 'tombstone',
      expiresAtInternal: entry.expiresAtInternal,
      ordinalInternal: entry.ordinalInternal,
      timer: entry.timer,
    });
  };

  const retireReservation = (attempt: Attempt): void => {
    const entry = reservations.get(attempt.reservationId);
    if (entry?.registryState !== 'live') return;
    reservations.set(attempt.reservationId, {
      expiresAtInternal: entry.expiresAtInternal,
      ordinalInternal: entry.ordinalInternal,
      registryState: 'tombstone',
    });
  };

  const releaseAttempt = (
    attempt: Attempt,
    cancelExecution: boolean
  ): Readonly<{
    claim?: PendingClaim;
    controlPort?: FirstDisplayPortLikeV1;
    controlRelease?: () => void;
    execution?: FirstDisplayRenderStrategyAttemptV1;
  }> => {
    clearOwnedTimer(attempt.claimTimer);
    clearOwnedTimer(attempt.insertionTimer);
    attempt.claimTimer = undefined;
    attempt.insertionTimer = undefined;
    const claim = attempt.claim;
    const controlPort = attempt.controlPort;
    const controlRelease = attempt.controlRelease;
    const execution = cancelExecution ? attempt.execution : undefined;
    attempt.claim = undefined;
    attempt.controlPort = undefined;
    attempt.controlRelease = undefined;
    attempt.execution = undefined;
    attempt.ownerSource = undefined;
    attempt.ownerTicket = undefined;
    retireTicket(attempt);
    retireReservation(attempt);
    attempts.delete(attempt.reservationId);
    return {
      ...(claim ? { claim } : {}),
      ...(controlPort ? { controlPort } : {}),
      ...(controlRelease ? { controlRelease } : {}),
      ...(execution ? { execution } : {}),
    };
  };

  const settle = (
    attempt: Attempt,
    result: 'accepted' | 'failed' | 'cancelled',
    reason = result === 'cancelled' ? NAVIGATION_DISPOSED : INTERNAL_ERROR,
    candidateArtifact?: FirstDisplayCommittedRenderArtifactV1
  ): boolean => {
    if (!attempt.active) return false;
    if (result === 'accepted') {
      if (
        !candidateArtifact ||
        candidateArtifact.slotId !== attempt.cycle.slotId ||
        candidateArtifact.token !== attempt.reservationId ||
        candidateArtifact.current() !== true
      ) {
        return settle(attempt, 'failed', INTERNAL_ERROR);
      }
      committed.set(attempt.cycle.slotId, candidateArtifact);
    }
    attempt.active = false;
    const ticket = attempt.ownerTicket;
    const released = releaseAttempt(attempt, result !== 'accepted');
    notifyNativeMutation();
    try {
      released.controlRelease?.();
    } catch {
      // State was detached before publisher-controlled cleanup.
    }
    try {
      released.execution?.cancel();
    } catch {
      // Strategy authority is detached from this generation.
    }
    if (released.controlPort && ticket) {
      const settlement: Record<string, unknown> = {
        message: 'TS Owner Settled',
        version: 1,
        lifecycleTicket: ticket,
        outcome: result,
      };
      if (result !== 'accepted') settlement.reason = reason;
      post(released.controlPort, settlement);
    }
    closePort(released.claim?.port);
    closePort(released.controlPort);
    try {
      attempt.onTerminal(result, result === 'accepted' ? null : reason);
    } catch {
      // A terminal observer cannot restore detached authority.
    }
    return true;
  };

  const fail = (attempt: Attempt, reason: string, refuseClaim = false): boolean => {
    if (!attempt.active) return false;
    if (refuseClaim && attempt.claim) {
      const claim = attempt.claim;
      attempt.claim = undefined;
      post(claim.port, refusedResponse(attempt.reservationId));
      closePort(claim.port);
      if (!attempt.active) return false;
    }
    return settle(attempt, 'failed', reason);
  };

  const issueTicket = (attempt: Attempt): string | undefined => {
    if (tickets.size >= MAX_CAPABILITIES || utf8Length(PUC_DYNAMIC_OWNER) > MAX_OWNER_BYTES) {
      return undefined;
    }
    const ticket = mint(tickets);
    if (!ticket || !TICKET_ID.test(ticket)) return undefined;
    const issuedAt = readNow();
    if (issuedAt === undefined) return undefined;
    const expiresAt = issuedAt + TICKET_TTL_MS;
    const ordinal = nextTicketOrdinal;
    nextTicketOrdinal += 1;
    const entry: LiveTicket = {
      registryState: 'live',
      attempt,
      expiresAtInternal: expiresAt,
      ordinalInternal: ordinal,
    };
    tickets.set(ticket, entry);
    attempt.ticket = ticket;
    entry.timer = arm(() => {
      const current = tickets.get(ticket);
      if (current !== entry) {
        const observedAt = readNow();
        if (
          current?.registryState === 'tombstone' &&
          observedAt !== undefined &&
          current.expiresAtInternal <= observedAt
        ) {
          tickets.delete(ticket);
          notifyNativeMutation();
        }
        return;
      }
      tickets.delete(ticket);
      attempt.ticket = undefined;
      notifyNativeMutation();
      fail(attempt, 'owner_registration_timeout');
    }, TICKET_TTL_MS);
    if (entry.timer === undefined) {
      tickets.delete(ticket);
      attempt.ticket = undefined;
      return undefined;
    }
    return ticket;
  };

  const join = (attempt: Attempt): boolean => {
    const claim = attempt.claim;
    if (
      !attempt.active ||
      !claim ||
      attempt.gam !== 'nonempty_gam' ||
      attempt.phaseValue !== 'waiting_for_gam_and_claim'
    )
      return false;
    clearOwnedTimer(attempt.claimTimer);
    attempt.claimTimer = undefined;
    const ticket = issueTicket(attempt);
    if (!ticket) return fail(attempt, 'capability_registry_full', true);
    const response = JSON.stringify({
      message: 'Prebid Response',
      adId: attempt.reservationId,
      renderer: PUC_DYNAMIC_OWNER,
      rendererVersion: '4',
      tsOwner: {
        version: 1,
        status: 'ready',
        kind: attempt.cycle.bid.renderSource.type,
        lifecycleTicket: ticket,
      },
    });
    attempt.claim = undefined;
    attempt.ownerSource = claim.source;
    attempt.phaseValue = 'waiting_for_owner';
    retireReservation(attempt);
    const source = attempt.cycle.bid.renderSource;
    resizeCollapsedPucShell(options.document, claim.source, source.width, source.height);
    if (
      !attempt.active ||
      attempt.phaseValue !== 'waiting_for_owner' ||
      attempt.ownerSource !== claim.source ||
      attempt.ticket !== ticket
    ) {
      closePort(claim.port);
      return false;
    }
    const posted = utf8Length(response) <= MAX_RESPONSE_BYTES && post(claim.port, response);
    closePort(claim.port);
    if (!posted && attempt.active) return fail(attempt, INTERNAL_ERROR);
    return posted;
  };

  const beginExecution = (attempt: Attempt, overlay: boolean): boolean => {
    type Pending =
      | Readonly<{ kind: 'accept'; artifact: FirstDisplayCommittedRenderArtifactV1 }>
      | Readonly<{ kind: 'fail'; reason: string }>;
    let starting = true;
    let pending: Pending | undefined;
    let duplicate = false;
    const enqueue = (next: Pending): void => {
      if (!attempt.active) return;
      if (starting) {
        if (pending) duplicate = true;
        else pending = next;
        return;
      }
      if (next.kind === 'accept') settle(attempt, 'accepted', INTERNAL_ERROR, next.artifact);
      else fail(attempt, next.reason);
    };
    const callbacks = Object.freeze({
      accept: (artifact: FirstDisplayCommittedRenderArtifactV1) =>
        enqueue(Object.freeze({ kind: 'accept' as const, artifact })),
      fail: (reason: string) => enqueue(Object.freeze({ kind: 'fail' as const, reason })),
    });
    let execution: FirstDisplayRenderStrategyAttemptV1 | undefined;
    if (attempt.cycle.bid.renderSource.type === 'adm') {
      if (overlay) return false;
      execution = directAdmExecution(
        options.document,
        attempt.cycle,
        callbacks,
        options.setTimer,
        options.clearTimer
      );
    } else if (strategy?.supports(attempt.cycle.bid.renderSource) === true) {
      try {
        execution = strategy.start(attempt.cycle, overlay, callbacks);
      } catch {
        execution = undefined;
      }
    }
    starting = false;
    if (!execution || duplicate) {
      try {
        execution?.cancel();
      } catch {
        // Refused execution cannot retain authority.
      }
      return false;
    }
    attempt.execution = execution;
    if (pending?.kind === 'accept') settle(attempt, 'accepted', INTERNAL_ERROR, pending.artifact);
    else if (pending?.kind === 'fail') fail(attempt, pending.reason);
    return true;
  };

  const ownerInserted = (attempt: Attempt): void => {
    if (!attempt.active || attempt.phaseValue !== 'waiting_for_insertion' || attempt.inserted)
      return;
    attempt.inserted = true;
    clearOwnedTimer(attempt.insertionTimer);
    attempt.insertionTimer = undefined;
    if (attempt.cycle.bid.renderSource.type === 'adm') {
      attempt.insertionTimer = arm(() => fail(attempt, ADM_DOCUMENT_NO_LOAD), ADM_LOAD_DEADLINE_MS);
      if (attempt.insertionTimer === undefined) fail(attempt, INTERNAL_ERROR);
    }
  };

  const handleOwnerControl = (attempt: Attempt, event: unknown): void => {
    if (!attempt.active) return;
    const ports = inspectPorts(event, messageEventPrototype);
    if (!ports?.exact || ports.originalCount !== 0 || ports.ports.length !== 0) {
      fail(attempt, INTERNAL_ERROR);
      return;
    }
    const message = exactRecord(eventField(event, 'data', messageEventPrototype), [
      'message',
      'version',
      'lifecycleTicket',
    ]);
    const ticket = attempt.ownerTicket;
    if (
      message?.message === 'TS Owner Inserted' &&
      message.version === 1 &&
      message.lifecycleTicket === ticket
    ) {
      ownerInserted(attempt);
      return;
    }
    if (attempt.cycle.bid.renderSource.type !== 'adm') {
      fail(attempt, INTERNAL_ERROR);
      return;
    }
    if (
      message?.message === 'TS ADM Loaded' &&
      message.version === 1 &&
      message.lifecycleTicket === ticket &&
      attempt.inserted
    ) {
      clearOwnedTimer(attempt.insertionTimer);
      attempt.insertionTimer = undefined;
      const artifact = publisherArtifact(options.document, attempt);
      if (artifact) settle(attempt, 'accepted', INTERNAL_ERROR, artifact);
      else fail(attempt, ADM_DOCUMENT_NO_LOAD);
      return;
    }
    if (
      message?.message === 'TS ADM Failed' &&
      message.version === 1 &&
      message.lifecycleTicket === ticket
    ) {
      fail(attempt, ADM_DOCUMENT_NO_LOAD);
      return;
    }
    fail(attempt, ADM_DOCUMENT_NO_LOAD);
  };

  const startOwner = (attempt: Attempt): boolean => {
    const controlPort = attempt.controlPort;
    const ticket = attempt.ownerTicket;
    if (!controlPort || !ticket || !attempt.active) return false;
    if (!validCycle(attempt.cycle, strategy)) return fail(attempt, 'slot_unresolved');
    attempt.insertionTimer = arm(
      () => fail(attempt, 'owner_insertion_timeout'),
      INSERTION_DEADLINE_MS
    );
    if (attempt.insertionTimer === undefined) return fail(attempt, INTERNAL_ERROR);
    if (attempt.cycle.bid.renderSource.type === 'adm') {
      return (
        post(controlPort, {
          message: 'TS ADM Start',
          version: 1,
          lifecycleTicket: ticket,
          source: attempt.cycle.bid.renderSource,
        }) || fail(attempt, INTERNAL_ERROR)
      );
    }
    if (!beginExecution(attempt, true)) return fail(attempt, 'winner_not_renderable');
    if (attempt.active) ownerInserted(attempt);
    return (
      post(controlPort, {
        message: 'TS APS Top Mount Started',
        version: 1,
        lifecycleTicket: ticket,
      }) || fail(attempt, INTERNAL_ERROR)
    );
  };

  const handleOwnerRegistration = (
    event: unknown,
    data: unknown,
    routing: Readonly<{ adId?: string; lifecycleTicket?: string }>
  ): void => {
    const ticket = routing.lifecycleTicket;
    if (!ticket) return;
    const beforeSuppress = tickets.get(ticket);
    if (!beforeSuppress || !suppress(event)) return;
    const entry = tickets.get(ticket);
    const inspection = inspectPorts(event, messageEventPrototype);
    const responsePort = inspection?.ports[0];
    const refuse = (): void => {
      if (responsePort) post(responsePort, ownerRefused(routing.adId ?? ''));
      for (const port of inspection?.ports ?? []) closePort(port);
    };
    if (entry !== beforeSuppress || entry.registryState !== 'live') {
      refuse();
      return;
    }
    const exact = exactOwnerRegistration(data);
    const attempt = entry.attempt;
    if (
      !exact ||
      !inspection?.exact ||
      inspection.originalCount !== 1 ||
      inspection.ports.length !== 1 ||
      !responsePort ||
      exact.adId !== attempt.reservationId ||
      exact.lifecycleTicket !== ticket ||
      eventSource(event, messageEventPrototype) !== attempt.ownerSource ||
      !attempt.active ||
      attempt.phaseValue !== 'waiting_for_owner'
    ) {
      refuse();
      if (attempt.active) fail(attempt, 'bridge_id_mismatch');
      return;
    }
    let channel: FirstDisplayChannelLikeV1;
    try {
      channel = options.createChannel();
    } catch {
      post(responsePort, ownerRefused(attempt.reservationId));
      closePort(responsePort);
      fail(attempt, INTERNAL_ERROR);
      return;
    }
    if (
      tickets.get(ticket) !== entry ||
      !attempt.active ||
      attempt.phaseValue !== 'waiting_for_owner'
    ) {
      closePort(channel.port1);
      closePort(channel.port2);
      refuse();
      return;
    }
    retireTicket(attempt);
    attempt.ownerTicket = ticket;
    attempt.controlPort = channel.port1;
    attempt.phaseValue = 'waiting_for_insertion';
    const listening = installPortListeners(
      channel.port1,
      (message) => handleOwnerControl(attempt, message),
      () =>
        fail(
          attempt,
          attempt.cycle.bid.renderSource.type === 'adm' ? ADM_DOCUMENT_NO_LOAD : INTERNAL_ERROR
        ),
      (release) => {
        attempt.controlRelease = release;
      }
    );
    if (!listening || !attempt.active) {
      closePort(responsePort);
      closePort(channel.port2);
      if (attempt.active) fail(attempt, INTERNAL_ERROR);
      return;
    }
    const registered = JSON.stringify({
      message: 'TS Render Owner Registered',
      adId: attempt.reservationId,
      version: 1,
      lifecycleTicket: ticket,
    });
    const posted = post(responsePort, registered, [channel.port2]);
    closePort(responsePort);
    closePort(channel.port2);
    if (!posted || !attempt.active || !startOwner(attempt)) {
      if (attempt.active) fail(attempt, INTERNAL_ERROR);
    }
  };

  const dispatch = (event: unknown): void => {
    if (disposed || ingressClosed) return;
    const data = eventField(event, 'data', messageEventPrototype);
    const routing = routingMessage(data);
    if (routing.message === 'TS Render Owner Register') {
      notifyNativeMutation();
      handleOwnerRegistration(event, data, routing);
      return;
    }
    if (routing.message !== 'Prebid Request' || !routing.adId) return;
    const beforeSuppress = reservations.get(routing.adId);
    if (!beforeSuppress || !suppress(event)) return;
    notifyNativeMutation();
    const reservationState = reservations.get(routing.adId);
    const inspection = inspectPorts(event, messageEventPrototype);
    const responsePort = inspection?.ports[0];
    const refuse = (): void => {
      if (responsePort) post(responsePort, refusedResponse(routing.adId!));
      for (const port of inspection?.ports ?? []) closePort(port);
    };
    if (
      reservationState !== beforeSuppress ||
      reservationState.registryState !== 'live' ||
      !inspection?.exact ||
      inspection.originalCount !== 1 ||
      inspection.ports.length !== 1 ||
      !responsePort
    ) {
      refuse();
      return;
    }
    const exact = exactPrebidRequest(data);
    const attempt = attempts.get(routing.adId);
    const source = eventSource(event, messageEventPrototype);
    if (
      !exact ||
      exact.adId !== routing.adId ||
      !attempt?.active ||
      attempt.phaseValue !== 'waiting_for_gam_and_claim' ||
      attempt.claim ||
      !source
    ) {
      refuse();
      return;
    }
    attempt.claim = Object.freeze({ port: responsePort, source });
    if (attempt.gam === 'nonempty_gam') join(attempt);
  };

  try {
    options.browser.addEventListener('message', dispatch as EventListener, true);
  } catch {
    throw new TypeError('tsjs');
  }

  const sweepCommittedArtifacts = (): number => {
    if (disposed || committedArtifactsDetached) return 0;
    let retired = 0;
    for (const [slotId, artifact] of [...committed.entries()]) {
      if (committed.get(slotId) !== artifact || artifact.current()) continue;
      committed.delete(slotId);
      try {
        artifact.retire();
      } catch {
        // Invalidated identity is already detached from the journal.
      }
      retired += 1;
    }
    if (retired > 0) notifyNativeMutation();
    return retired;
  };

  return Object.freeze({
    bind: (
      cycle: FirstDisplayGptBoundCycleV1,
      onTerminal: (result: 'accepted' | 'failed' | 'cancelled', reason: string | null) => void
    ): boolean => {
      if (
        disposed ||
        ingressClosed ||
        sealed ||
        typeof onTerminal !== 'function' ||
        !validCycle(cycle, strategy) ||
        reservations.size >= MAX_CAPABILITIES ||
        reservations.has(cycle.bid.rendererReservationId)
      )
        return false;
      const observedAt = readNow();
      if (observedAt === undefined) return false;
      const expiresAt = observedAt + RESERVATION_TTL_MS;
      const ordinal = nextReservationOrdinal;
      if (!Number.isFinite(expiresAt) || expiresAt <= observedAt || ordinal > 4_294_967_295) {
        return false;
      }
      const attempt: Attempt = {
        active: true,
        claim: undefined,
        claimTimer: undefined,
        controlPort: undefined,
        controlRelease: undefined,
        cycle,
        execution: undefined,
        gam: undefined,
        insertionTimer: undefined,
        inserted: false,
        onTerminal,
        ownerSource: undefined,
        ownerTicket: undefined,
        phaseValue: 'waiting_for_gam_and_claim',
        reservationId: cycle.bid.rendererReservationId,
        ticket: undefined,
      };
      attempts.set(attempt.reservationId, attempt);
      reservations.set(attempt.reservationId, {
        expiresAtInternal: expiresAt,
        ordinalInternal: ordinal,
        registryState: 'live',
      });
      nextReservationOrdinal += 1;
      return true;
    },
    recordGam: (
      cycle: FirstDisplayGptBoundCycleV1,
      result: FirstDisplayGptRenderResult
    ): boolean => {
      const attempt = attempts.get(cycle.bid.rendererReservationId);
      if (
        !attempt?.active ||
        attempt.cycle !== cycle ||
        attempt.gam ||
        attempt.phaseValue !== 'waiting_for_gam_and_claim'
      )
        return false;
      attempt.gam = result;
      if (result === 'gam_empty') {
        attempt.phaseValue = 'rendering_direct';
        retireReservation(attempt);
        const claim = attempt.claim;
        attempt.claim = undefined;
        if (claim) {
          post(claim.port, refusedResponse(attempt.reservationId));
          closePort(claim.port);
          if (!attempt.active) return false;
        }
        return beginExecution(attempt, false) || fail(attempt, 'winner_not_renderable');
      }
      if (attempt.claim) return join(attempt);
      attempt.claimTimer = arm(() => fail(attempt, 'bridge_claim_timeout'), CLAIM_DEADLINE_MS);
      return attempt.claimTimer !== undefined || fail(attempt, INTERNAL_ERROR);
    },
    recordFailure: (cycle: FirstDisplayGptBoundCycleV1): boolean => {
      const attempt = attempts.get(cycle.bid.rendererReservationId);
      return Boolean(
        attempt?.active && attempt.cycle === cycle && fail(attempt, 'gpt_request_failed')
      );
    },
    retire: (cycle: FirstDisplayGptBoundCycleV1): boolean => {
      if (disposed || committedArtifactsDetached) return false;
      const artifact = committed.get(cycle.slotId);
      if (!artifact || artifact.token !== cycle.bid.rendererReservationId) return false;
      committed.delete(cycle.slotId);
      try {
        artifact.retire();
      } catch {
        // Exact identity is already retired from the journal.
      }
      notifyNativeMutation();
      return true;
    },
    sweepCommittedArtifacts,
    sealTsAdmission: (): void => {
      if (disposed || [...attempts.values()].some((attempt) => attempt.active)) {
        throw new TypeError('tsjs');
      }
      sealed = true;
    },
    closeIngress: (): boolean => {
      if (
        disposed ||
        ingressClosed ||
        !sealed ||
        [...attempts.values()].some((attempt) => attempt.active)
      )
        return false;
      ingressClosed = true;
      try {
        options.browser.removeEventListener('message', dispatch as EventListener, true);
      } catch {
        // Closed generation state remains authoritative.
      }
      for (const handle of [...timers]) clearOwnedTimer(handle);
      return true;
    },
    captureHandoff: () => {
      if (disposed || !ingressClosed || handoffCaptured) return undefined;
      sweepCommittedArtifacts();
      const observedAt = readNow();
      if (observedAt === undefined) return undefined;
      const reservationTombstones = [...reservations.entries()]
        .filter(
          ([, entry]) => entry.registryState === 'tombstone' && entry.expiresAtInternal > observedAt
        )
        .map(([value, entry]) =>
          Object.freeze({
            kind: 'reservation' as const,
            value,
            expiresAtMs: entry.expiresAtInternal,
            ordinal: entry.ordinalInternal,
          })
        );
      const ticketTombstones = [...tickets.entries()]
        .filter(
          ([, entry]) => entry.registryState === 'tombstone' && entry.expiresAtInternal > observedAt
        )
        .map(([value, entry]) =>
          Object.freeze({
            kind: 'ticket' as const,
            value,
            expiresAtMs: entry.expiresAtInternal,
            ordinal: entry.ordinalInternal,
          })
        );
      const artifacts = [...committed.values()].map((artifact) =>
        Object.freeze({
          hostPosition: artifact.hostPosition,
          hostPositionPriority: artifact.hostPositionPriority,
          identity: artifact.identity,
          kind: artifact.kind,
          owner: artifact.owner,
          slotId: artifact.slotId,
          token: artifact.token,
        })
      );
      handoffCaptured = true;
      return Object.freeze({
        artifacts: Object.freeze(artifacts),
        clockEpochMs: observedAt,
        nextReservationOrdinal,
        nextTicketOrdinal,
        tombstones: Object.freeze([...reservationTombstones, ...ticketTombstones]),
      });
    },
    detachCommittedArtifacts: (): boolean => {
      if (disposed || !ingressClosed || !handoffCaptured || committedArtifactsDetached) {
        return false;
      }
      if (sweepCommittedArtifacts() > 0) return false;
      committedArtifactsDetached = true;
      return true;
    },
    dispose: (): void => {
      if (disposed) return;
      disposed = true;
      if (!ingressClosed) {
        ingressClosed = true;
        try {
          options.browser.removeEventListener('message', dispatch as EventListener, true);
        } catch {
          // Generation latching keeps a failed physical removal inert.
        }
      }
      for (const attempt of [...attempts.values()]) {
        if (attempt.active) settle(attempt, 'cancelled', NAVIGATION_DISPOSED);
      }
      for (const handle of [...timers]) clearOwnedTimer(handle);
      if (!committedArtifactsDetached) {
        for (const artifact of committed.values()) {
          try {
            artifact.retire();
          } catch {
            // Exact artifact retirement is best-effort after ownership removal.
          }
        }
      }
      committed.clear();
      attempts.clear();
      reservations.clear();
      tickets.clear();
      try {
        strategy?.dispose();
      } catch {
        // Strategy authority is generation-latched.
      }
    },
  });
}

function exactBindings(candidate: unknown): RenderOwnerInitialBindings | undefined {
  try {
    if (
      typeof candidate !== 'object' ||
      candidate === null ||
      Array.isArray(candidate) ||
      Object.getPrototypeOf(candidate) !== Object.prototype ||
      !Object.isFrozen(candidate) ||
      Reflect.ownKeys(candidate).length !== 2
    )
      return undefined;
    const observe = Object.getOwnPropertyDescriptor(candidate, 'observe');
    const register = Object.getOwnPropertyDescriptor(candidate, 'register');
    if (
      !observe?.enumerable ||
      !('value' in observe) ||
      typeof observe.value !== 'function' ||
      !register?.enumerable ||
      !('value' in register) ||
      typeof register.value !== 'function'
    )
      return undefined;
    return candidate as RenderOwnerInitialBindings;
  } catch {
    return undefined;
  }
}

/** Install the one-use release-private source-neutral initial render owner. */
export function installRenderOwnerInitial(
  candidate: unknown,
  own: FirstDisplaySliceActivationContext['own']
): Readonly<{ version: 1; id: 'render_owner' }> {
  const bindings = exactBindings(candidate);
  if (!bindings || typeof own !== 'function') throw new TypeError('tsjs');
  let consumed = false;
  let created: FirstDisplayRenderBridgeV1 | undefined;
  const protocol: FirstDisplayRenderOwnerProtocolV1 = Object.freeze({
    version: 1,
    id: 'render_owner',
    createRenderBridge: (
      options: FirstDisplayRenderOwnerOptionsV1,
      strategy?: FirstDisplayRenderStrategyV1
    ) => {
      if (consumed) throw new TypeError('tsjs');
      consumed = true;
      created = createFirstDisplayRenderJournal(options, strategy);
      return created;
    },
  });
  const release = bindings.register(protocol);
  if (typeof release !== 'function') throw new TypeError('tsjs');
  own(() => {
    const bridge = created;
    created = undefined;
    try {
      bridge?.dispose();
    } finally {
      release();
    }
  });
  bindings.observe('protocol_version', 1);
  return Object.freeze({ version: 1, id: 'render_owner' });
}
