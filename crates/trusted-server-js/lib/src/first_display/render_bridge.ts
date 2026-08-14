import { PUC_DYNAMIC_OWNER } from '../kernel/contracts/puc_dynamic_owner';

import type {
  FirstDisplayGptBoundCycleV1,
  FirstDisplayGptRenderResult,
} from './adapters/googletag';
import type { FirstDisplayRenderBridgeV1 } from './driver';
import type { FirstDisplayApsProtocolV1 } from './leaf/aps_protocol';

const ADM_SANDBOX =
  'allow-forms allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation';
const CLAIM_DEADLINE_MS = 3_000;
const ADM_LOAD_DEADLINE_MS = 5_000;
const TICKET_TTL_MS = 3_000;
const RESERVATION_TTL_MS = 15 * 60 * 1_000;
const MAX_CAPABILITIES = 320;
const MAX_NONCES = 256;
const MAX_DRAWS = 8;
const MAX_GLOBAL_MESSAGE_BYTES = 4_096;
const MAX_DOMAIN_BYTES = 2_048;
const MAX_OWNER_BYTES = 64 * 1_024;
const MAX_RESPONSE_BYTES = 72 * 1_024;
const RESERVATION_ID = /^r1_[A-Za-z0-9_-]{22}$/;
const TICKET_ID = /^t1_[A-Za-z0-9_-]{22}$/;
const NONCE_ID = /^n1_[A-Za-z0-9_-]{22}$/;
const BASE64URL = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_';
const textEncoder = new TextEncoder();

interface PortLike {
  readonly addEventListener?: (name: string, listener: (event: unknown) => void) => void;
  readonly close: () => void;
  readonly postMessage: (message: unknown, transfer?: readonly PortLike[]) => void;
  readonly removeEventListener?: (name: string, listener: (event: unknown) => void) => void;
  readonly start?: () => void;
}

interface ChannelLike {
  readonly port1: PortLike;
  readonly port2: PortLike;
}

export interface FirstDisplayRenderBridgeOptionsV1 {
  readonly browser: Window;
  readonly clearTimer: (handle: unknown) => void;
  readonly createChannel: () => ChannelLike;
  readonly document: Document;
  readonly fillRandom: (bytes: Uint8Array) => void;
  readonly getAps: () => FirstDisplayApsProtocolV1 | undefined;
  readonly now: () => number;
  readonly onNativeMutation?: () => boolean;
  readonly setTimer: (callback: () => void, delayMs: number) => unknown;
}

interface PendingClaim {
  readonly port: PortLike;
  readonly source: object;
}

interface Attempt {
  readonly cycle: FirstDisplayGptBoundCycleV1;
  readonly onTerminal: (result: 'accepted' | 'failed' | 'cancelled', reason: string | null) => void;
  readonly reservationId: string;
  active: boolean;
  claim: PendingClaim | undefined;
  claimTimer: unknown;
  completionTimer: unknown;
  controlPort: PortLike | undefined;
  controlRelease: (() => void) | undefined;
  directFrame: HTMLIFrameElement | undefined;
  documentAccepted: boolean;
  documentAcceptancePending: boolean;
  documentPort: PortLike | undefined;
  documentRelease: (() => void) | undefined;
  documentTimer: unknown;
  documentTransferred: PortLike | undefined;
  gam: FirstDisplayGptRenderResult | undefined;
  inserted: boolean;
  insertionTimer: unknown;
  ownerTicket: string | undefined;
  rendererNonce: string | undefined;
  ownerSource: object | undefined;
  pendingDocumentTerminal:
    'completed' | 'descriptor_invalid' | 'runner_no_load' | 'runner_failed' | undefined;
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

function skipJsonWhitespace(source: string, start: number): number {
  let index = start;
  while (
    source[index] === ' ' ||
    source[index] === '\t' ||
    source[index] === '\n' ||
    source[index] === '\r'
  ) {
    index += 1;
  }
  return index;
}

function scanJsonString(source: string, start: number): number | undefined {
  if (source[start] !== '"') return undefined;
  let index = start + 1;
  while (index < source.length) {
    const character = source[index];
    if (character === '"') return index + 1;
    if (character === '\\') {
      index += 1;
      if (index >= source.length) return undefined;
      if (source[index] === 'u') {
        if (!/^[0-9a-fA-F]{4}$/.test(source.slice(index + 1, index + 5))) return undefined;
        index += 4;
      }
    } else if (character !== undefined && character.charCodeAt(0) < 0x20) {
      return undefined;
    }
    index += 1;
  }
  return undefined;
}

function scanJsonValue(source: string, start: number): number | undefined {
  let index = skipJsonWhitespace(source, start);
  if (source[index] === '"') return scanJsonString(source, index);
  if (source[index] === '[') {
    index = skipJsonWhitespace(source, index + 1);
    if (source[index] === ']') return index + 1;
    while (index < source.length) {
      const end = scanJsonValue(source, index);
      if (end === undefined) return undefined;
      index = skipJsonWhitespace(source, end);
      if (source[index] === ']') return index + 1;
      if (source[index] !== ',') return undefined;
      index = skipJsonWhitespace(source, index + 1);
    }
    return undefined;
  }
  if (source[index] === '{') {
    const keys = new Set<string>();
    index = skipJsonWhitespace(source, index + 1);
    if (source[index] === '}') return index + 1;
    while (index < source.length) {
      const keyEnd = scanJsonString(source, index);
      if (keyEnd === undefined) return undefined;
      let key: unknown;
      try {
        key = JSON.parse(source.slice(index, keyEnd)) as unknown;
      } catch {
        return undefined;
      }
      if (typeof key !== 'string' || keys.has(key)) return undefined;
      keys.add(key);
      index = skipJsonWhitespace(source, keyEnd);
      if (source[index] !== ':') return undefined;
      const valueEnd = scanJsonValue(source, index + 1);
      if (valueEnd === undefined) return undefined;
      index = skipJsonWhitespace(source, valueEnd);
      if (source[index] === '}') return index + 1;
      if (source[index] !== ',') return undefined;
      index = skipJsonWhitespace(source, index + 1);
    }
    return undefined;
  }
  const match = /^(?:true|false|null|-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?)/.exec(
    source.slice(index)
  );
  return match ? index + match[0].length : undefined;
}

function parseJson(source: string): unknown {
  if (utf8Length(source) > MAX_GLOBAL_MESSAGE_BYTES) return undefined;
  const end = scanJsonValue(source, 0);
  if (end === undefined || skipJsonWhitespace(source, end) !== source.length) return undefined;
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
      return Object.freeze({});
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
    return Object.freeze({
      ...(message ? { message } : {}),
      ...(adId ? { adId } : {}),
      ...(lifecycleTicket ? { lifecycleTicket } : {}),
    });
  } catch {
    return Object.freeze({});
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
    utf8Length(fields.adServerDomain) <= MAX_DOMAIN_BYTES
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
    TICKET_ID.test(fields.lifecycleTicket)
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

function usablePort(value: unknown): value is PortLike {
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
      ports: readonly PortLike[];
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
    const ports: PortLike[] = [];
    const seen = new Set<PortLike>();
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
    return Object.freeze({ exact, originalCount: length.value, ports: Object.freeze(ports) });
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

function closePort(port: PortLike | undefined): void {
  try {
    port?.close();
  } catch {
    // Authority is already inert; endpoint cleanup remains best-effort.
  }
}

function post(port: PortLike, data: unknown, transfer: readonly PortLike[] = []): boolean {
  try {
    Reflect.apply(port.postMessage, port, [data, transfer]);
    return true;
  } catch {
    return false;
  }
}

function listen(
  port: PortLike,
  receive: (event: unknown) => void,
  receiveError: () => void
): (() => void) | undefined {
  try {
    if (typeof port.addEventListener !== 'function') return undefined;
    Reflect.apply(port.addEventListener, port, ['message', receive]);
    Reflect.apply(port.addEventListener, port, ['messageerror', receiveError]);
    if (typeof port.start === 'function') Reflect.apply(port.start, port, []);
    let live = true;
    return () => {
      if (!live) return;
      live = false;
      try {
        if (typeof port.removeEventListener === 'function') {
          Reflect.apply(port.removeEventListener, port, ['message', receive]);
          Reflect.apply(port.removeEventListener, port, ['messageerror', receiveError]);
        }
      } catch {
        // Port closure below remains authoritative.
      }
    };
  } catch {
    return undefined;
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

function validCycle(cycle: FirstDisplayGptBoundCycleV1): boolean {
  try {
    const source = cycle.bid.renderSource;
    return (
      cycle.slotId === cycle.bid.slot &&
      cycle.slotId === cycle.placement.slot &&
      cycle.element.ownerDocument !== null &&
      cycle.element.ownerDocument.getElementById(cycle.element.id) === cycle.element &&
      RESERVATION_ID.test(cycle.bid.rendererReservationId) &&
      (source.type === 'adm' || source.type === 'aps')
    );
  } catch {
    return false;
  }
}

function refusedResponse(adId: string): string {
  return JSON.stringify({
    message: 'Prebid Response',
    adId,
    rendererVersion: '3',
    tsOwner: { version: 1, status: 'refused' },
  });
}

function ownerRefused(adId: string): string {
  return JSON.stringify({ message: 'TS Render Owner Refused', adId, version: 1 });
}

function resizeCollapsedPucShell(
  document: Document,
  source: object,
  width: number,
  height: number
): boolean {
  try {
    const browser = document.defaultView;
    if (
      !browser ||
      !Number.isFinite(width) ||
      !Number.isFinite(height) ||
      width <= 0 ||
      height <= 0
    ) {
      return false;
    }
    const frames = document.querySelectorAll('iframe');
    let selected: HTMLIFrameElement | undefined;
    for (let index = 0; index < frames.length; index += 1) {
      const candidate = frames.item(index);
      if (candidate?.isConnected && candidate.contentWindow === source) {
        if (selected) return false;
        selected = candidate;
      }
    }
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
      !selected ||
      !onePixelAttribute(selected, 'width') ||
      !onePixelAttribute(selected, 'height') ||
      !ordinaryCollapsed(selected) ||
      selected.closest('a,[data-anchor-status]') !== null
    ) {
      return false;
    }
    const wrapper = selected.parentElement;
    if (
      !wrapper ||
      wrapper === document.body ||
      wrapper === document.documentElement ||
      wrapper.tagName === 'A' ||
      !wrapper.isConnected ||
      !ordinaryCollapsed(wrapper) ||
      wrapper.closest('a,[data-anchor-status]') !== null
    ) {
      return false;
    }
    selected.style.setProperty('width', `${width}px`);
    selected.style.setProperty('height', `${height}px`);
    wrapper.style.setProperty('width', `${width}px`);
    wrapper.style.setProperty('height', `${height}px`);
    return true;
  } catch {
    return false;
  }
}

/** Own the bounded APS/ADM authority used only by the immutable first-display batch. */
export function createFirstDisplayRenderBridge(
  options: FirstDisplayRenderBridgeOptionsV1
): FirstDisplayRenderBridgeV1 {
  const attempts = new Map<string, Attempt>();
  const reservations = new Map<string, ReservationEntry>();
  const tickets = new Map<string, TicketEntry>();
  const nonces = new Map<string, Attempt>();
  const committedFrames = new Map<
    string,
    Readonly<{
      frame: HTMLIFrameElement;
      kind: 'gpt_adm' | 'aps';
      owner: 'trusted_server';
      token: string;
    }>
  >();
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

  const apsProtocol = (): FirstDisplayApsProtocolV1 | undefined => {
    try {
      return options.getAps();
    } catch {
      return undefined;
    }
  };

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
      // Mutation observation cannot alter the admitted publisher or browser event.
    }
  };

  const clearOwnedTimer = (handle: unknown): void => {
    if (handle === undefined || !timers.delete(handle)) return;
    try {
      options.clearTimer(handle);
    } catch {
      // The state guarded by the timer is already generation-inert.
    }
  };

  const arm = (callback: () => void, delayMs: number): unknown => {
    let handle: unknown;
    try {
      handle = options.setTimer(() => {
        if (!timers.delete(handle)) return;
        callback();
      }, delayMs);
    } catch {
      handle = undefined;
    }
    if (handle !== undefined) timers.add(handle);
    return handle;
  };

  const mint = (
    prefix: 't1_' | 'n1_',
    registry: ReadonlyMap<string, unknown>
  ): string | undefined => {
    for (let draw = 0; draw < MAX_DRAWS; draw += 1) {
      const bytes = new Uint8Array(16);
      try {
        options.fillRandom(bytes);
      } catch {
        return undefined;
      }
      const candidate = `${prefix}${encodeOpaque(bytes)}`;
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
    notifyNativeMutation();
  };

  const retireReservation = (attempt: Attempt): void => {
    const entry = reservations.get(attempt.reservationId);
    if (entry?.registryState !== 'live') return;
    reservations.set(attempt.reservationId, {
      expiresAtInternal: entry.expiresAtInternal,
      ordinalInternal: entry.ordinalInternal,
      registryState: 'tombstone',
    });
    notifyNativeMutation();
  };

  const releaseAttempt = (attempt: Attempt, removeFrame: boolean): void => {
    clearOwnedTimer(attempt.claimTimer);
    clearOwnedTimer(attempt.completionTimer);
    clearOwnedTimer(attempt.documentTimer);
    clearOwnedTimer(attempt.insertionTimer);
    attempt.claimTimer = undefined;
    attempt.completionTimer = undefined;
    attempt.documentTimer = undefined;
    attempt.insertionTimer = undefined;
    const claim = attempt.claim;
    attempt.claim = undefined;
    closePort(claim?.port);
    attempt.controlRelease?.();
    attempt.documentRelease?.();
    attempt.controlRelease = undefined;
    attempt.documentRelease = undefined;
    closePort(attempt.controlPort);
    closePort(attempt.documentPort);
    closePort(attempt.documentTransferred);
    attempt.controlPort = undefined;
    attempt.documentPort = undefined;
    attempt.documentTransferred = undefined;
    if (attempt.rendererNonce && nonces.get(attempt.rendererNonce) === attempt) {
      nonces.delete(attempt.rendererNonce);
    }
    attempt.rendererNonce = undefined;
    retireTicket(attempt);
    retireReservation(attempt);
    attempts.delete(attempt.reservationId);
    if (attempt.directFrame) {
      try {
        attempt.directFrame.onload = null;
        attempt.directFrame.onerror = null;
        if (removeFrame) attempt.directFrame.remove();
      } catch {
        // The exact frame is no longer authoritative after terminal settlement.
      }
    }
    attempt.directFrame = undefined;
  };

  const settle = (
    attempt: Attempt,
    result: 'accepted' | 'failed' | 'cancelled',
    reason = result === 'cancelled' ? 'navigation_disposed' : 'internal_error'
  ): boolean => {
    if (!attempt.active) return false;
    attempt.active = false;
    if (attempt.controlPort && attempt.ownerTicket) {
      const settlement: Record<string, unknown> = {
        message: 'TS Owner Settled',
        version: 1,
        lifecycleTicket: attempt.ownerTicket,
        outcome: result,
      };
      if (result !== 'accepted') settlement.reason = reason;
      post(attempt.controlPort, settlement);
    }
    const committedFrame = result === 'accepted' ? attempt.directFrame : undefined;
    releaseAttempt(attempt, result !== 'accepted');
    if (committedFrame?.isConnected) {
      committedFrames.set(
        attempt.cycle.slotId,
        Object.freeze({
          frame: committedFrame,
          kind: attempt.cycle.bid.renderSource.type === 'adm' ? 'gpt_adm' : 'aps',
          owner: 'trusted_server',
          token: attempt.reservationId,
        })
      );
    }
    try {
      attempt.onTerminal(result, result === 'accepted' ? null : reason);
    } catch {
      // A consumer callback cannot restore released render authority.
    }
    notifyNativeMutation();
    return true;
  };

  const fail = (attempt: Attempt, reason: string, refuseClaim = false): boolean => {
    if (!attempt.active) return false;
    if (refuseClaim && attempt.claim) {
      const claim = attempt.claim;
      attempt.claim = undefined;
      post(claim.port, refusedResponse(attempt.reservationId));
      closePort(claim.port);
    }
    return settle(attempt, 'failed', reason);
  };

  const issueNonce = (attempt: Attempt): string | undefined => {
    if (nonces.size >= MAX_NONCES || attempt.rendererNonce) return undefined;
    const nonce = mint('n1_', nonces);
    if (!nonce || !NONCE_ID.test(nonce)) return undefined;
    nonces.set(nonce, attempt);
    attempt.rendererNonce = nonce;
    return nonce;
  };

  const handleApsDocument = (attempt: Attempt, event: unknown): void => {
    const aps = apsProtocol();
    if (!attempt.active || !attempt.rendererNonce || !aps) return;
    const inspection = inspectPorts(event, messageEventPrototype);
    if (!inspection?.exact || inspection.originalCount !== 0 || inspection.ports.length !== 0) {
      fail(attempt, attempt.documentAccepted ? 'runner_failed' : 'renderer_document_no_load');
      return;
    }
    const parsed = aps.parseDocumentMessage(
      eventField(event, 'data', messageEventPrototype),
      attempt.rendererNonce
    );
    if (!parsed) {
      fail(attempt, attempt.documentAccepted ? 'runner_failed' : 'renderer_document_no_load');
      return;
    }
    const acceptDocument = (): void => {
      if (!attempt.active || attempt.documentAccepted || !attempt.inserted) return;
      attempt.documentAccepted = true;
      attempt.documentAcceptancePending = false;
      clearOwnedTimer(attempt.documentTimer);
      attempt.documentTimer = undefined;
      attempt.completionTimer = arm(
        () => fail(attempt, 'runner_failed'),
        aps.deadlines.completionMs
      );
      const pending = attempt.pendingDocumentTerminal;
      attempt.pendingDocumentTerminal = undefined;
      if (pending === 'completed') settle(attempt, 'accepted');
      else if (pending) fail(attempt, pending);
    };
    if (parsed.kind === 'document_accepted') {
      if (attempt.documentAccepted) return;
      if (!attempt.inserted) attempt.documentAcceptancePending = true;
      else acceptDocument();
      return;
    }
    if (parsed.kind === 'runner_loaded') {
      if (!attempt.documentAccepted && !attempt.documentAcceptancePending) {
        fail(attempt, 'renderer_document_no_load');
      }
      return;
    }
    if (parsed.kind === 'render_completed') {
      if (attempt.documentAccepted) settle(attempt, 'accepted');
      else if (attempt.documentAcceptancePending && !attempt.pendingDocumentTerminal) {
        attempt.pendingDocumentTerminal = 'completed';
      } else fail(attempt, 'renderer_document_no_load');
      return;
    }
    if (attempt.documentAccepted) fail(attempt, parsed.reason);
    else if (attempt.documentAcceptancePending && !attempt.pendingDocumentTerminal) {
      attempt.pendingDocumentTerminal = parsed.reason;
    } else fail(attempt, 'renderer_document_no_load');
  };

  const ownerInserted = (attempt: Attempt): void => {
    if (!attempt.active || attempt.inserted) return;
    attempt.inserted = true;
    clearOwnedTimer(attempt.insertionTimer);
    attempt.insertionTimer = undefined;
    if (attempt.cycle.bid.renderSource.type === 'adm') {
      attempt.documentTimer = arm(
        () => fail(attempt, 'adm_document_no_load'),
        ADM_LOAD_DEADLINE_MS
      );
    } else if (attempt.documentAcceptancePending) {
      const nonce = attempt.rendererNonce;
      if (nonce) {
        handleApsDocument(attempt, {
          data: { message: 'TS APS Document Accepted', version: 1, nonce },
          ports: [],
        });
      }
    } else {
      const aps = apsProtocol();
      attempt.documentTimer = arm(
        () => fail(attempt, 'renderer_document_no_load'),
        aps?.deadlines.documentAcceptanceMs ?? 3_000
      );
    }
  };

  const handleOwnerControl = (attempt: Attempt, event: unknown): void => {
    if (!attempt.active) return;
    const ports = inspectPorts(event, messageEventPrototype);
    if (!ports?.exact || ports.originalCount !== 0 || ports.ports.length !== 0) {
      fail(attempt, 'internal_error');
      return;
    }
    const data = eventField(event, 'data', messageEventPrototype);
    const ticket = attempt.ownerTicket;
    const inserted = exactRecord(data, ['message', 'version', 'lifecycleTicket']);
    if (
      inserted?.message === 'TS Owner Inserted' &&
      inserted.version === 1 &&
      inserted.lifecycleTicket === ticket
    ) {
      ownerInserted(attempt);
      return;
    }
    if (attempt.cycle.bid.renderSource.type !== 'adm') {
      fail(attempt, 'internal_error');
      return;
    }
    const loaded = exactRecord(data, ['message', 'version', 'lifecycleTicket']);
    if (
      loaded?.message === 'TS ADM Loaded' &&
      loaded.version === 1 &&
      loaded.lifecycleTicket === ticket &&
      attempt.inserted
    ) {
      settle(attempt, 'accepted');
      return;
    }
    if (
      loaded?.message === 'TS ADM Failed' &&
      loaded.version === 1 &&
      loaded.lifecycleTicket === ticket
    ) {
      fail(attempt, 'adm_document_no_load');
      return;
    }
    fail(attempt, 'adm_document_no_load');
  };

  const startOwner = (attempt: Attempt): boolean => {
    const controlPort = attempt.controlPort;
    const ticket = attempt.ownerTicket;
    if (!controlPort || !ticket || !attempt.active) return false;
    const aps = apsProtocol();
    attempt.insertionTimer = arm(
      () => fail(attempt, 'owner_insertion_timeout'),
      aps?.deadlines.insertionMs ?? 1_000
    );
    if (attempt.cycle.bid.renderSource.type === 'adm') {
      return post(controlPort, {
        message: 'TS ADM Start',
        version: 1,
        lifecycleTicket: ticket,
        source: attempt.cycle.bid.renderSource,
      });
    }
    if (!aps) return fail(attempt, 'winner_not_renderable');
    const nonce = issueNonce(attempt);
    if (!nonce) return fail(attempt, 'capability_registry_full');
    let channel: ChannelLike;
    try {
      channel = options.createChannel();
    } catch {
      return fail(attempt, 'internal_error');
    }
    attempt.documentPort = channel.port1;
    attempt.documentTransferred = channel.port2;
    attempt.documentRelease = listen(
      channel.port1,
      (event) => handleApsDocument(attempt, event),
      () => fail(attempt, attempt.documentAccepted ? 'runner_failed' : 'renderer_document_no_load')
    );
    if (!attempt.documentRelease) return fail(attempt, 'internal_error');
    const started = post(
      controlPort,
      {
        message: 'TS APS Start',
        version: 1,
        lifecycleTicket: ticket,
        rendererUrl: aps.rendererUrl,
        envelope: {
          version: 1,
          nonce,
          publisherOrigin: aps.publisherOrigin,
          renderer: attempt.cycle.bid.renderSource,
        },
      },
      [channel.port2]
    );
    closePort(channel.port2);
    attempt.documentTransferred = undefined;
    return started || fail(attempt, 'internal_error');
  };

  const handleOwnerRegistration = (
    event: unknown,
    data: unknown,
    routing: Readonly<{ adId?: string; lifecycleTicket?: string }>
  ): void => {
    const ticket = routing.lifecycleTicket;
    if (!ticket) return;
    const entry = tickets.get(ticket);
    if (!entry || !suppress(event)) return;
    const inspection = inspectPorts(event, messageEventPrototype);
    const responsePort = inspection?.ports[0];
    const refuse = (): void => {
      if (responsePort) post(responsePort, ownerRefused(routing.adId ?? ''));
      for (const port of inspection?.ports ?? []) closePort(port);
    };
    if (entry.registryState !== 'live') {
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
    retireTicket(attempt);
    let channel: ChannelLike;
    try {
      channel = options.createChannel();
    } catch {
      post(responsePort, ownerRefused(attempt.reservationId));
      closePort(responsePort);
      fail(attempt, 'internal_error');
      return;
    }
    attempt.ownerTicket = ticket;
    attempt.controlPort = channel.port1;
    attempt.controlRelease = listen(
      channel.port1,
      (message) => handleOwnerControl(attempt, message),
      () =>
        fail(
          attempt,
          attempt.cycle.bid.renderSource.type === 'adm' ? 'adm_document_no_load' : 'internal_error'
        )
    );
    attempt.phaseValue = 'waiting_for_insertion';
    const registered = JSON.stringify({
      message: 'TS Render Owner Registered',
      adId: attempt.reservationId,
      version: 1,
      lifecycleTicket: ticket,
    });
    const posted =
      Boolean(attempt.controlRelease) && post(responsePort, registered, [channel.port2]);
    closePort(responsePort);
    closePort(channel.port2);
    if (!posted || !startOwner(attempt)) {
      if (attempt.active) fail(attempt, 'internal_error');
    }
  };

  const issueTicket = (attempt: Attempt): string | undefined => {
    if (tickets.size >= MAX_CAPABILITIES || utf8Length(PUC_DYNAMIC_OWNER) > MAX_OWNER_BYTES) {
      return undefined;
    }
    const ticket = mint('t1_', tickets);
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
    ) {
      return false;
    }
    clearOwnedTimer(attempt.claimTimer);
    attempt.claimTimer = undefined;
    const ticket = issueTicket(attempt);
    if (!ticket) return fail(attempt, 'capability_registry_full', true);
    const owner = {
      version: 1,
      status: 'ready',
      kind: attempt.cycle.bid.renderSource.type,
      lifecycleTicket: ticket,
    };
    const response = JSON.stringify({
      message: 'Prebid Response',
      adId: attempt.reservationId,
      renderer: PUC_DYNAMIC_OWNER,
      rendererVersion: '3',
      tsOwner: owner,
    });
    attempt.claim = undefined;
    const posted = utf8Length(response) <= MAX_RESPONSE_BYTES && post(claim.port, response);
    closePort(claim.port);
    if (!posted || !attempt.active) {
      if (attempt.active) fail(attempt, 'internal_error');
      return false;
    }
    attempt.ownerSource = claim.source;
    attempt.phaseValue = 'waiting_for_owner';
    retireReservation(attempt);
    const source = attempt.cycle.bid.renderSource;
    resizeCollapsedPucShell(options.document, claim.source, source.width, source.height);
    return true;
  };

  const configureFrame = (
    frame: HTMLIFrameElement,
    width: number,
    height: number,
    sandbox: string
  ): void => {
    frame.setAttribute('sandbox', sandbox);
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
  };

  const renderDirectAdm = (attempt: Attempt): boolean => {
    const source = attempt.cycle.bid.renderSource;
    if (source.type !== 'adm') return false;
    try {
      const frame = options.document.createElement('iframe');
      configureFrame(frame, source.width, source.height, ADM_SANDBOX);
      const intended = `<!doctype html><html><head><meta charset="utf-8"><meta name="referrer" content="no-referrer"><style>html,body{border:0;margin:0;padding:0;overflow:hidden}</style></head><body>${source.adm}</body></html>`;
      frame.onload = () => {
        if (
          attempt.active &&
          attempt.directFrame === frame &&
          frame.parentNode === attempt.cycle.element &&
          frame.srcdoc === intended &&
          frame.getAttribute('src') === null
        ) {
          settle(attempt, 'accepted');
        }
      };
      frame.onerror = () => fail(attempt, 'adm_document_no_load');
      frame.srcdoc = intended;
      attempt.directFrame = frame;
      attempt.documentTimer = arm(
        () => fail(attempt, 'adm_document_no_load'),
        ADM_LOAD_DEADLINE_MS
      );
      attempt.cycle.element.appendChild(frame);
      return true;
    } catch {
      return fail(attempt, 'adm_document_no_load');
    }
  };

  const renderDirectAps = (attempt: Attempt): boolean => {
    const source = attempt.cycle.bid.renderSource;
    const aps = apsProtocol();
    if (source.type !== 'aps' || !aps) return false;
    const nonce = issueNonce(attempt);
    if (!nonce) return fail(attempt, 'capability_registry_full');
    let channel: ChannelLike | undefined;
    try {
      channel = options.createChannel();
      const frame = options.document.createElement('iframe');
      configureFrame(frame, source.width, source.height, aps.sandbox);
      const intended = `${aps.rendererUrl}#tsaps=${nonce}`;
      let intendedWindow: Window | null = null;
      attempt.documentPort = channel.port1;
      attempt.documentTransferred = channel.port2;
      attempt.documentRelease = listen(
        channel.port1,
        (event) => handleApsDocument(attempt, event),
        () =>
          fail(attempt, attempt.documentAccepted ? 'runner_failed' : 'renderer_document_no_load')
      );
      if (!attempt.documentRelease) throw new Error('tsjs');
      frame.onload = () => {
        if (
          !attempt.active ||
          attempt.directFrame !== frame ||
          frame.parentNode !== attempt.cycle.element ||
          frame.getAttribute('src') !== intended ||
          frame.contentWindow !== intendedWindow ||
          !intendedWindow ||
          !attempt.documentTransferred
        ) {
          return;
        }
        const transferred = attempt.documentTransferred;
        attempt.documentTransferred = undefined;
        try {
          intendedWindow.postMessage(
            {
              version: 1,
              nonce,
              publisherOrigin: aps.publisherOrigin,
              renderer: source,
            },
            '*',
            [transferred as unknown as Transferable]
          );
          closePort(transferred);
        } catch {
          fail(attempt, 'renderer_document_no_load');
        }
      };
      frame.onerror = () => fail(attempt, 'renderer_document_no_load');
      frame.src = intended;
      attempt.directFrame = frame;
      attempt.inserted = true;
      attempt.documentTimer = arm(
        () => fail(attempt, 'renderer_document_no_load'),
        aps.deadlines.documentAcceptanceMs
      );
      attempt.cycle.element.appendChild(frame);
      intendedWindow = frame.contentWindow;
      return true;
    } catch {
      closePort(channel?.port1);
      closePort(channel?.port2);
      return fail(attempt, 'renderer_document_no_load');
    }
  };

  const renderDirectFallback = (attempt: Attempt): boolean => {
    attempt.phaseValue = 'rendering_direct';
    retireReservation(attempt);
    const claim = attempt.claim;
    attempt.claim = undefined;
    if (claim) {
      post(claim.port, refusedResponse(attempt.reservationId));
      closePort(claim.port);
    }
    return attempt.cycle.bid.renderSource.type === 'adm'
      ? renderDirectAdm(attempt)
      : renderDirectAps(attempt);
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
    const reservationState = reservations.get(routing.adId);
    if (!reservationState || !suppress(event)) return;
    notifyNativeMutation();
    const inspection = inspectPorts(event, messageEventPrototype);
    const responsePort = inspection?.ports[0];
    const exact = exactPrebidRequest(data);
    const refuse = (): void => {
      if (responsePort) post(responsePort, refusedResponse(routing.adId!));
      for (const port of inspection?.ports ?? []) closePort(port);
    };
    if (
      reservationState.registryState !== 'live' ||
      !inspection?.exact ||
      inspection.originalCount !== 1 ||
      inspection.ports.length !== 1 ||
      !responsePort ||
      !exact ||
      exact.adId !== routing.adId
    ) {
      refuse();
      return;
    }
    const attempt = attempts.get(routing.adId);
    const source = eventSource(event, messageEventPrototype);
    if (
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
        !validCycle(cycle) ||
        reservations.size >= MAX_CAPABILITIES ||
        reservations.has(cycle.bid.rendererReservationId)
      ) {
        return false;
      }
      const observedAt = readNow();
      if (observedAt === undefined) return false;
      const expiresAt = observedAt + RESERVATION_TTL_MS;
      const reservationOrdinal = nextReservationOrdinal;
      if (
        !Number.isFinite(expiresAt) ||
        expiresAt <= observedAt ||
        reservationOrdinal > 4_294_967_295
      ) {
        return false;
      }
      const attempt: Attempt = {
        active: true,
        claim: undefined,
        claimTimer: undefined,
        completionTimer: undefined,
        controlPort: undefined,
        controlRelease: undefined,
        cycle,
        directFrame: undefined,
        documentAccepted: false,
        documentAcceptancePending: false,
        documentPort: undefined,
        documentRelease: undefined,
        documentTimer: undefined,
        documentTransferred: undefined,
        gam: undefined,
        inserted: false,
        insertionTimer: undefined,
        ownerTicket: undefined,
        rendererNonce: undefined,
        onTerminal,
        ownerSource: undefined,
        pendingDocumentTerminal: undefined,
        reservationId: cycle.bid.rendererReservationId,
        phaseValue: 'waiting_for_gam_and_claim',
        ticket: undefined,
      };
      attempts.set(attempt.reservationId, attempt);
      reservations.set(attempt.reservationId, {
        expiresAtInternal: expiresAt,
        ordinalInternal: reservationOrdinal,
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
      ) {
        return false;
      }
      attempt.gam = result;
      if (result === 'gam_empty') return renderDirectFallback(attempt);
      if (attempt.claim) return join(attempt);
      attempt.claimTimer = arm(() => fail(attempt, 'bridge_claim_timeout'), CLAIM_DEADLINE_MS);
      return attempt.claimTimer !== undefined || fail(attempt, 'internal_error');
    },
    recordFailure: (cycle: FirstDisplayGptBoundCycleV1): boolean => {
      const attempt = attempts.get(cycle.bid.rendererReservationId);
      return Boolean(
        attempt?.active && attempt.cycle === cycle && fail(attempt, 'gpt_request_failed')
      );
    },
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
      ) {
        return false;
      }
      ingressClosed = true;
      try {
        options.browser.removeEventListener('message', dispatch as EventListener, true);
      } catch {
        // The closed generation cannot regain authority through a failed physical removal.
      }
      for (const handle of [...timers]) clearOwnedTimer(handle);
      return true;
    },
    captureHandoff: () => {
      if (disposed || !ingressClosed || handoffCaptured) return undefined;
      const observedAt = readNow();
      if (observedAt === undefined) return undefined;
      const reservationTombstones = [...reservations.entries()]
        .filter((entry): entry is [string, ReservationEntry] => {
          const value = entry[1];
          return value.registryState === 'tombstone' && value.expiresAtInternal > observedAt;
        })
        .map(([value, entry]) =>
          Object.freeze({
            kind: 'reservation' as const,
            value,
            expiresAtMs: entry.expiresAtInternal,
            ordinal: entry.ordinalInternal,
          })
        );
      const ticketTombstones = [...tickets.entries()]
        .filter((entry): entry is [string, TicketTombstone] => {
          const value = entry[1];
          return value.registryState === 'tombstone' && value.expiresAtInternal > observedAt;
        })
        .map(([value, entry]) =>
          Object.freeze({
            kind: 'ticket' as const,
            value,
            expiresAtMs: entry.expiresAtInternal,
            ordinal: entry.ordinalInternal,
          })
        );
      const artifacts = [...committedFrames.entries()].map(([slotId, entry]) =>
        Object.freeze({
          identity: entry.frame,
          kind: entry.kind,
          owner: entry.owner,
          slotId,
          token: entry.token,
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
          // Generation latching makes a failed physical removal inert.
        }
      }
      for (const attempt of [...attempts.values()]) {
        if (attempt.active) settle(attempt, 'cancelled', 'navigation_disposed');
      }
      for (const handle of [...timers]) clearOwnedTimer(handle);
      if (!committedArtifactsDetached) {
        for (const { frame } of committedFrames.values()) {
          try {
            frame.remove();
          } catch {
            // The committed owner is terminal and cannot be restored by a hostile DOM node.
          }
        }
      }
      committedFrames.clear();
      attempts.clear();
      reservations.clear();
      tickets.clear();
      nonces.clear();
    },
  });
}
