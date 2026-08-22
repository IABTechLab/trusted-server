import type { FirstDisplayGptBoundCycleV1 } from './adapters/googletag';
import type { FirstDisplayApsProtocolV1 } from './leaf/aps_protocol';
import type {
  FirstDisplayCommittedRenderArtifactV1,
  FirstDisplayRenderOwnerOptionsV1,
  FirstDisplayRenderStrategyAttemptV1,
  FirstDisplayRenderStrategyCallbacksV1,
  FirstDisplayRenderStrategyV1,
} from './render_journal';

const RENDERER_DOCUMENT_NO_LOAD = 'renderer_document_no_load';
const INTERNAL_ERROR = 'internal_error';
const RUNNER_FAILED = 'runner_failed';
const WINNER_NOT_RENDERABLE = 'winner_not_renderable';
const MAX_DRAWS = 8;
const BASE64URL = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_';

interface PortLike {
  readonly addEventListener?: (name: string, listener: (event: unknown) => void) => void;
  readonly close: () => void;
  readonly postMessage: (message: unknown, transfer?: readonly PortLike[]) => void;
  readonly removeEventListener?: (name: string, listener: (event: unknown) => void) => void;
  readonly start?: () => void;
}

interface ApsAttempt {
  readonly callbacks: FirstDisplayRenderStrategyCallbacksV1;
  readonly cycle: FirstDisplayGptBoundCycleV1;
  readonly overlay: boolean;
  active: boolean;
  accepted: boolean;
  bootstrapNavigated: boolean;
  bootstrapNonceInternal: string;
  bootstrapSource: object | undefined;
  completionTimer: unknown;
  documentAccepted: boolean;
  documentPort: PortLike | undefined;
  documentRelease: (() => void) | undefined;
  documentTimer: unknown;
  frame: HTMLIFrameElement;
  hostPositionOwned: boolean;
  pendingTerminal:
    | 'completed'
    | typeof WINNER_NOT_RENDERABLE
    | 'runner_no_load'
    | typeof RUNNER_FAILED
    | undefined;
  previousHostPosition: string;
  previousHostPositionPriority: string;
  rendererNonceInternal: string;
}

function eventField(
  event: unknown,
  name: 'data' | 'origin' | 'ports' | 'source',
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

function exactPorts(
  event: unknown,
  trustedPrototype: object | undefined,
  expected: 0 | 1
): readonly PortLike[] | undefined {
  const value = eventField(event, 'ports', trustedPrototype);
  try {
    if (!Array.isArray(value)) return undefined;
    const length = Object.getOwnPropertyDescriptor(value, 'length');
    if (!length || !('value' in length) || !Number.isSafeInteger(length.value)) return undefined;
    let exact =
      length.value === expected &&
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
    if (exact && ports.length === expected) return ports;
    for (const port of ports) closePort(port);
    return undefined;
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

function closePort(port: PortLike | undefined): void {
  try {
    port?.close();
  } catch {
    // The endpoint is already generation-inert.
  }
}

function post(port: PortLike, message: unknown): boolean {
  try {
    Reflect.apply(port.postMessage, port, [message, []]);
    return true;
  } catch {
    return false;
  }
}

function postWindow(target: object, message: string): boolean {
  try {
    const postMessage = Reflect.get(target, 'postMessage');
    if (typeof postMessage !== 'function') return false;
    Reflect.apply(postMessage, target, [message, '*', []]);
    return true;
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

function snapshotFrameAttributes(frame: HTMLIFrameElement): string | undefined {
  try {
    return frame.outerHTML;
  } catch {
    return undefined;
  }
}

/** Create the APS-owned URL, nonce, document-port, and overlay strategy. */
export function createFirstDisplayApsRenderStrategy(
  options: FirstDisplayRenderOwnerOptionsV1,
  aps: FirstDisplayApsProtocolV1
): FirstDisplayRenderStrategyV1 {
  const attempts = new Set<ApsAttempt>();
  const bootstrapNonces = new Map<string, ApsAttempt>();
  const rendererNonces = new Map<string, ApsAttempt>();
  const timers = new Set<unknown>();
  let disposed = false;
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

  const notifyNativeMutation = (): void => {
    try {
      options.onNativeMutation?.();
    } catch {
      // Observation cannot alter admitted APS state.
    }
  };

  const clearOwnedTimer = (handle: unknown): void => {
    if (handle === undefined || !timers.delete(handle)) return;
    try {
      options.clearTimer(handle);
    } catch {
      // Timer state is already detached.
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

  const mint = (
    prefix: 'b1_' | 'n1_',
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

  const configureFrame = (
    frame: HTMLIFrameElement,
    width: number,
    height: number,
    overlay: boolean
  ): void => {
    frame.setAttribute('sandbox', aps.sandbox);
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
      overlay
        ? `border: 0; display: block; height: ${height}px; inset: 0; margin: 0; overflow: hidden; position: absolute; visibility: hidden; width: ${width}px; z-index: 2147483647;`
        : `border: 0; margin: 0; overflow: hidden; display: block; width: ${width}px; height: ${height}px;`
    );
  };

  const restoreHostPosition = (attempt: ApsAttempt): void => {
    if (!attempt.hostPositionOwned) return;
    attempt.hostPositionOwned = false;
    try {
      const style = attempt.cycle.element.style;
      if (
        style.getPropertyValue('position') !== 'relative' ||
        style.getPropertyPriority('position') !== ''
      )
        return;
      if (attempt.previousHostPosition === '') style.removeProperty('position');
      else
        style.setProperty(
          'position',
          attempt.previousHostPosition,
          attempt.previousHostPositionPriority
        );
    } catch {
      // Compare-owned restoration never overwrites publisher changes.
    }
  };

  const acquireHostPosition = (attempt: ApsAttempt): boolean => {
    if (!attempt.overlay) return true;
    try {
      const host = attempt.cycle.element;
      const browser = host.ownerDocument.defaultView;
      if (!browser || !attempt.cycle.isCurrent()) return false;
      if (browser.getComputedStyle(host).position !== 'static') return true;
      attempt.previousHostPosition = host.style.getPropertyValue('position');
      attempt.previousHostPositionPriority = host.style.getPropertyPriority('position');
      host.style.setProperty('position', 'relative');
      attempt.hostPositionOwned =
        host.style.getPropertyValue('position') === 'relative' &&
        host.style.getPropertyPriority('position') === '';
      return attempt.hostPositionOwned && attempt.cycle.isCurrent();
    } catch {
      return false;
    }
  };

  const exactFrame = (attempt: ApsAttempt, permanent: boolean): boolean => {
    try {
      return (
        attempt.active &&
        attempt.cycle.isCurrent() &&
        attempt.frame.isConnected &&
        attempt.frame.parentNode === attempt.cycle.element &&
        attempt.frame.contentWindow === attempt.bootstrapSource &&
        attempt.frame.getAttribute('src') ===
          `${aps.rendererUrl}#${attempt.bootstrapNonceInternal}` &&
        attempt.frame.src === `${aps.rendererUrl}#${attempt.bootstrapNonceInternal}` &&
        attempt.frame.getAttribute('sandbox') ===
          (permanent ? aps.permanentSandbox : aps.sandbox) &&
        (!attempt.hostPositionOwned ||
          (attempt.cycle.element.style.getPropertyValue('position') === 'relative' &&
            attempt.cycle.element.style.getPropertyPriority('position') === ''))
      );
    } catch {
      return false;
    }
  };

  const releasePort = (attempt: ApsAttempt): void => {
    const release = attempt.documentRelease;
    const port = attempt.documentPort;
    attempt.documentRelease = undefined;
    attempt.documentPort = undefined;
    try {
      release?.();
    } catch {
      // Port closure remains authoritative.
    }
    closePort(port);
  };

  const detachAttempt = (attempt: ApsAttempt): void => {
    clearOwnedTimer(attempt.documentTimer);
    clearOwnedTimer(attempt.completionTimer);
    attempt.documentTimer = undefined;
    attempt.completionTimer = undefined;
    if (bootstrapNonces.get(attempt.bootstrapNonceInternal) === attempt) {
      bootstrapNonces.delete(attempt.bootstrapNonceInternal);
    }
    if (rendererNonces.get(attempt.rendererNonceInternal) === attempt) {
      rendererNonces.delete(attempt.rendererNonceInternal);
    }
    attempts.delete(attempt);
    releasePort(attempt);
  };

  const cancelAttempt = (attempt: ApsAttempt): void => {
    if (!attempt.active && !attempt.accepted) return;
    attempt.active = false;
    attempt.accepted = false;
    detachAttempt(attempt);
    attempt.frame.onload = null;
    attempt.frame.onerror = null;
    try {
      attempt.frame.remove();
    } catch {
      // The exact node cannot regain authority.
    }
    restoreHostPosition(attempt);
    notifyNativeMutation();
  };

  const fail = (attempt: ApsAttempt, reason: string): void => {
    if (!attempt.active) return;
    attempt.active = false;
    detachAttempt(attempt);
    attempt.frame.onload = null;
    attempt.frame.onerror = null;
    try {
      attempt.frame.remove();
    } catch {
      // Failed APS identity is already detached.
    }
    restoreHostPosition(attempt);
    notifyNativeMutation();
    try {
      attempt.callbacks.fail(reason);
    } catch {
      // Consumers cannot restore APS authority.
    }
  };

  const installDocumentPort = (
    attempt: ApsAttempt,
    port: PortLike,
    receive: (event: unknown) => void,
    receiveError: () => void
  ): boolean => {
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
      attempt.documentRelease = release;
      if (typeof port.start === 'function') Reflect.apply(port.start, port, []);
      return live && attempt.active && attempt.documentPort === port;
    } catch {
      return false;
    }
  };

  const complete = (attempt: ApsAttempt): void => {
    if (!attempt.active || !exactFrame(attempt, true)) {
      fail(attempt, 'slot_unresolved');
      return;
    }
    if (attempt.overlay) {
      try {
        attempt.frame.style.setProperty('visibility', 'visible');
        if (attempt.frame.style.getPropertyValue('visibility') !== 'visible') {
          fail(attempt, INTERNAL_ERROR);
          return;
        }
      } catch {
        fail(attempt, INTERNAL_ERROR);
        return;
      }
    }
    const attributes = snapshotFrameAttributes(attempt.frame);
    const frameWindow = attempt.frame.contentWindow;
    const frameSource = attempt.frame.src;
    const frameSourceDocument = attempt.frame.srcdoc;
    if (attributes === undefined || !frameWindow) {
      fail(attempt, INTERNAL_ERROR);
      return;
    }
    attempt.active = false;
    attempt.accepted = true;
    detachAttempt(attempt);
    attempt.frame.onload = null;
    attempt.frame.onerror = null;
    let artifactLive = true;
    const retire = (): void => {
      if (!artifactLive) return;
      artifactLive = false;
      attempt.accepted = false;
      try {
        attempt.frame.remove();
      } catch {
        // Exact-node retirement is best-effort.
      }
      restoreHostPosition(attempt);
      notifyNativeMutation();
    };
    const artifact: FirstDisplayCommittedRenderArtifactV1 = Object.freeze({
      hostPosition: attempt.hostPositionOwned ? attempt.previousHostPosition : null,
      hostPositionPriority: attempt.hostPositionOwned ? attempt.previousHostPositionPriority : null,
      identity: attempt.frame,
      kind: 'aps',
      owner: 'trusted_server',
      slotId: attempt.cycle.slotId,
      token: attempt.cycle.bid.rendererReservationId,
      current: () => {
        try {
          return (
            artifactLive &&
            attempt.accepted &&
            attempt.cycle.isCurrent() &&
            attempt.frame.isConnected &&
            attempt.frame.parentNode === attempt.cycle.element &&
            attempt.frame.contentWindow === frameWindow &&
            attempt.frame.src === frameSource &&
            attempt.frame.srcdoc === frameSourceDocument &&
            snapshotFrameAttributes(attempt.frame) === attributes &&
            (!attempt.hostPositionOwned ||
              (attempt.cycle.element.style.getPropertyValue('position') === 'relative' &&
                attempt.cycle.element.style.getPropertyPriority('position') === ''))
          );
        } catch {
          return false;
        }
      },
      retire,
    });
    notifyNativeMutation();
    try {
      attempt.callbacks.accept(artifact);
    } catch {
      retire();
    }
  };

  const handleDocument = (attempt: ApsAttempt, event: unknown): void => {
    if (!attempt.active || !exactFrame(attempt, true)) {
      fail(attempt, attempt.documentAccepted ? RUNNER_FAILED : RENDERER_DOCUMENT_NO_LOAD);
      return;
    }
    if (!exactPorts(event, messageEventPrototype, 0)) {
      fail(attempt, attempt.documentAccepted ? RUNNER_FAILED : RENDERER_DOCUMENT_NO_LOAD);
      return;
    }
    const parsed = aps.parseDocumentMessage(
      eventField(event, 'data', messageEventPrototype),
      attempt.rendererNonceInternal
    );
    if (!parsed) {
      fail(attempt, attempt.documentAccepted ? RUNNER_FAILED : RENDERER_DOCUMENT_NO_LOAD);
      return;
    }
    if (parsed.kind === 'document_accepted') {
      if (attempt.documentAccepted) return;
      attempt.documentAccepted = true;
      if (rendererNonces.get(attempt.rendererNonceInternal) === attempt) {
        rendererNonces.delete(attempt.rendererNonceInternal);
      }
      clearOwnedTimer(attempt.documentTimer);
      attempt.documentTimer = undefined;
      attempt.completionTimer = arm(() => fail(attempt, RUNNER_FAILED), aps.deadlines.completionMs);
      if (attempt.completionTimer === undefined) {
        fail(attempt, INTERNAL_ERROR);
        return;
      }
      const pending = attempt.pendingTerminal;
      attempt.pendingTerminal = undefined;
      if (pending === 'completed') complete(attempt);
      else if (pending) fail(attempt, pending);
      return;
    }
    if (parsed.kind === 'runner_loaded') return;
    if (parsed.kind === 'render_completed') {
      if (attempt.documentAccepted) complete(attempt);
      else if (!attempt.pendingTerminal) attempt.pendingTerminal = 'completed';
      else fail(attempt, RENDERER_DOCUMENT_NO_LOAD);
      return;
    }
    const failureReason =
      parsed.reason === 'descriptor_invalid' ? WINNER_NOT_RENDERABLE : parsed.reason;
    if (attempt.documentAccepted) fail(attempt, failureReason);
    else if (!attempt.pendingTerminal) attempt.pendingTerminal = failureReason;
    else fail(attempt, RENDERER_DOCUMENT_NO_LOAD);
  };

  const dispatch = (event: unknown): void => {
    if (disposed) return;
    const data = eventField(event, 'data', messageEventPrototype);
    const message = aps.parseWindowMessage(data);
    if (!message) return;
    if (message.kind === 'bootstrap_ready') {
      const nonce = message.bootstrap;
      const attempt = bootstrapNonces.get(nonce);
      const source = eventSource(event, messageEventPrototype);
      if (
        !attempt ||
        eventField(event, 'origin', messageEventPrototype) !== 'null' ||
        source !== attempt.bootstrapSource
      )
        return;
      if (!exactPorts(event, messageEventPrototype, 0)) {
        fail(attempt, RENDERER_DOCUMENT_NO_LOAD);
        return;
      }
      if (attempt.bootstrapNavigated || !exactFrame(attempt, false)) {
        fail(attempt, RENDERER_DOCUMENT_NO_LOAD);
        return;
      }
      const policy = aps.bootstrapPolicy(attempt.cycle.bid.renderSource);
      if (!policy) {
        fail(attempt, WINNER_NOT_RENDERABLE);
        return;
      }
      try {
        attempt.frame.setAttribute('sandbox', aps.permanentSandbox);
      } catch {
        fail(attempt, RENDERER_DOCUMENT_NO_LOAD);
        return;
      }
      const navigation = JSON.stringify({
        message: 'TS APS Bootstrap Configure',
        version: 2,
        bootstrapNonce: nonce,
        rendererNonce: attempt.rendererNonceInternal,
        creativeOrigin: policy.creativeOrigin,
        tagType: policy.tagType,
      });
      if (!exactFrame(attempt, true) || !postWindow(source!, navigation)) {
        fail(attempt, RENDERER_DOCUMENT_NO_LOAD);
        return;
      }
      attempt.bootstrapNavigated = true;
      return;
    }

    const { bootstrap: bootstrapNonce, renderer: rendererNonce } = message;
    const attempt = bootstrapNonces.get(bootstrapNonce);
    const source = eventSource(event, messageEventPrototype);
    if (
      !attempt ||
      rendererNonces.get(rendererNonce) !== attempt ||
      eventField(event, 'origin', messageEventPrototype) !== 'null' ||
      source !== attempt.bootstrapSource
    )
      return;
    const ports = exactPorts(event, messageEventPrototype, 1);
    const port = ports?.[0];
    if (
      !port ||
      !attempt.bootstrapNavigated ||
      attempt.documentPort ||
      !exactFrame(attempt, true)
    ) {
      fail(attempt, RENDERER_DOCUMENT_NO_LOAD);
      return;
    }
    attempt.documentPort = port;
    const listening = installDocumentPort(
      attempt,
      port,
      (portEvent) => handleDocument(attempt, portEvent),
      () => fail(attempt, attempt.documentAccepted ? RUNNER_FAILED : RENDERER_DOCUMENT_NO_LOAD)
    );
    if (!listening || !attempt.active) {
      if (attempt.active) fail(attempt, RENDERER_DOCUMENT_NO_LOAD);
      return;
    }
    bootstrapNonces.delete(bootstrapNonce);
    if (
      !post(port, {
        version: 1,
        nonce: rendererNonce,
        ['publisherOrigin']: aps.publisherOrigin,
        renderer: attempt.cycle.bid.renderSource,
      }) ||
      !attempt.active ||
      !exactFrame(attempt, true)
    )
      fail(attempt, RENDERER_DOCUMENT_NO_LOAD);
  };

  try {
    options.browser.addEventListener('message', dispatch as EventListener, true);
  } catch {
    throw new TypeError('tsjs');
  }

  return Object.freeze({
    supports: (source: unknown): boolean => {
      try {
        return aps.bootstrapPolicy(source) !== undefined;
      } catch {
        return false;
      }
    },
    start: (
      cycle: FirstDisplayGptBoundCycleV1,
      overlay: boolean,
      callbacks: FirstDisplayRenderStrategyCallbacksV1
    ): FirstDisplayRenderStrategyAttemptV1 | undefined => {
      const source = cycle.bid.renderSource;
      if (disposed || source.type !== 'aps' || !cycle.isCurrent() || !aps.bootstrapPolicy(source)) {
        return undefined;
      }
      const bootstrapNonce = mint('b1_', bootstrapNonces);
      const rendererNonce = mint('n1_', rendererNonces);
      if (
        !bootstrapNonce ||
        !rendererNonce ||
        !aps.isBootstrapNonce(bootstrapNonce) ||
        !aps.isRendererNonce(rendererNonce)
      )
        return undefined;
      let frame: HTMLIFrameElement;
      try {
        frame = options.document.createElement('iframe');
        configureFrame(frame, source.width, source.height, overlay);
      } catch {
        return undefined;
      }
      const attempt: ApsAttempt = {
        active: true,
        accepted: false,
        bootstrapNavigated: false,
        bootstrapNonceInternal: bootstrapNonce,
        bootstrapSource: undefined,
        callbacks,
        completionTimer: undefined,
        cycle,
        documentAccepted: false,
        documentPort: undefined,
        documentRelease: undefined,
        documentTimer: undefined,
        frame,
        hostPositionOwned: false,
        overlay,
        pendingTerminal: undefined,
        previousHostPosition: '',
        previousHostPositionPriority: '',
        rendererNonceInternal: rendererNonce,
      };
      attempts.add(attempt);
      bootstrapNonces.set(bootstrapNonce, attempt);
      rendererNonces.set(rendererNonce, attempt);
      try {
        frame.onerror = () => fail(attempt, RENDERER_DOCUMENT_NO_LOAD);
        frame.src = `${aps.rendererUrl}#${bootstrapNonce}`;
        if (!acquireHostPosition(attempt)) {
          cancelAttempt(attempt);
          return undefined;
        }
        cycle.element.appendChild(frame);
        const frameWindow = frame.contentWindow;
        if (!frameWindow) {
          cancelAttempt(attempt);
          return undefined;
        }
        attempt.bootstrapSource = frameWindow;
        attempt.documentTimer = arm(
          () => fail(attempt, RENDERER_DOCUMENT_NO_LOAD),
          aps.deadlines.documentAcceptanceMs
        );
        if (attempt.documentTimer === undefined || !exactFrame(attempt, false)) {
          cancelAttempt(attempt);
          return undefined;
        }
      } catch {
        cancelAttempt(attempt);
        return undefined;
      }
      notifyNativeMutation();
      return Object.freeze({ cancel: () => cancelAttempt(attempt) });
    },
    dispose: (): void => {
      if (disposed) return;
      disposed = true;
      try {
        options.browser.removeEventListener('message', dispatch as EventListener, true);
      } catch {
        // Generation state remains authoritative.
      }
      for (const attempt of [...attempts]) cancelAttempt(attempt);
      for (const handle of [...timers]) clearOwnedTimer(handle);
      attempts.clear();
      bootstrapNonces.clear();
      rendererNonces.clear();
    },
  });
}
