import { log } from '../core/log';

type AppendChildMethod = typeof Element.prototype.appendChild;
type InsertBeforeMethod = typeof Element.prototype.insertBefore;

const DOM_INSERTION_DISPATCHER_KEY = Symbol.for('trusted-server.domInsertionDispatcher');
const DOM_INSERTION_DISPATCHER_STATE_VERSION = 1;

export const DEFAULT_DOM_INSERTION_HANDLER_PRIORITY = 100;

export interface DomInsertionScriptCandidate {
  element: HTMLScriptElement;
  kind: 'script';
  url: string;
}

export interface DomInsertionLinkCandidate {
  element: HTMLLinkElement;
  kind: 'link';
  rel: 'preload' | 'prefetch';
  url: string;
}

export type DomInsertionCandidate = DomInsertionScriptCandidate | DomInsertionLinkCandidate;

export interface DomInsertionHandler {
  /**
   * Process a normalized DOM insertion candidate.
   *
   * Return `true` when this handler has finished processing the candidate and
   * no subsequent handlers should run. Return `false` to leave the candidate
   * available for later handlers. The dispatcher still inserts the node into
   * the DOM regardless of the return value.
   */
  handle: (candidate: DomInsertionCandidate) => boolean;
  id: string;
  priority: number;
}

interface RegisteredDomInsertionHandler extends DomInsertionHandler {
  sequence: number;
}

interface DomInsertionDispatcherState {
  appendChildWrapper?: AppendChildMethod;
  baselineAppendChild?: AppendChildMethod;
  baselineInsertBefore?: InsertBeforeMethod;
  handlers: Map<number, RegisteredDomInsertionHandler>;
  insertBeforeWrapper?: InsertBeforeMethod;
  nextSequence: number;
  orderedHandlers: RegisteredDomInsertionHandler[];
  version: number;
}

interface LegacyDomInsertionDispatcherState {
  appendChildWrapper?: unknown;
  baselineAppendChild?: unknown;
  baselineInsertBefore?: unknown;
  insertBeforeWrapper?: unknown;
  version?: unknown;
}

function isOptionalFunction(
  value: unknown
): value is ((...args: unknown[]) => unknown) | undefined {
  return value === undefined || typeof value === 'function';
}

function isRegisteredDomInsertionHandler(value: unknown): value is RegisteredDomInsertionHandler {
  if (typeof value !== 'object' || value === null) {
    return false;
  }

  const candidate = value as Partial<RegisteredDomInsertionHandler>;
  return (
    typeof candidate.handle === 'function' &&
    typeof candidate.id === 'string' &&
    typeof candidate.priority === 'number' &&
    typeof candidate.sequence === 'number'
  );
}

function isDispatcherState(value: unknown): value is DomInsertionDispatcherState {
  if (typeof value !== 'object' || value === null) {
    return false;
  }

  const candidate = value as Partial<DomInsertionDispatcherState>;
  return (
    candidate.version === DOM_INSERTION_DISPATCHER_STATE_VERSION &&
    candidate.handlers instanceof Map &&
    [...candidate.handlers.values()].every(isRegisteredDomInsertionHandler) &&
    Array.isArray(candidate.orderedHandlers) &&
    candidate.orderedHandlers.every(isRegisteredDomInsertionHandler) &&
    typeof candidate.nextSequence === 'number' &&
    isOptionalFunction(candidate.appendChildWrapper) &&
    isOptionalFunction(candidate.baselineAppendChild) &&
    isOptionalFunction(candidate.insertBeforeWrapper) &&
    isOptionalFunction(candidate.baselineInsertBefore)
  );
}

function compareHandlers(
  left: RegisteredDomInsertionHandler,
  right: RegisteredDomInsertionHandler
): number {
  if (left.priority !== right.priority) {
    return left.priority - right.priority;
  }

  if (left.id !== right.id) {
    return left.id.localeCompare(right.id);
  }

  return left.sequence - right.sequence;
}

function getStateVersion(state: unknown): unknown {
  return typeof state === 'object' && state !== null
    ? (state as { version?: unknown }).version
    : undefined;
}

function restoreStaleDispatcherMethods(existingState: unknown): void {
  if (
    typeof Element === 'undefined' ||
    typeof existingState !== 'object' ||
    existingState === null
  ) {
    return;
  }

  const staleState = existingState as LegacyDomInsertionDispatcherState;

  if (
    typeof staleState.appendChildWrapper === 'function' &&
    typeof staleState.baselineAppendChild === 'function' &&
    Element.prototype.appendChild === staleState.appendChildWrapper
  ) {
    Element.prototype.appendChild = staleState.baselineAppendChild as AppendChildMethod;
  }

  if (
    typeof staleState.insertBeforeWrapper === 'function' &&
    typeof staleState.baselineInsertBefore === 'function' &&
    Element.prototype.insertBefore === staleState.insertBeforeWrapper
  ) {
    Element.prototype.insertBefore = staleState.baselineInsertBefore as InsertBeforeMethod;
  }
}

function getDispatcherState(): DomInsertionDispatcherState {
  const globalObject = globalThis as Record<PropertyKey, unknown>;
  const existingState = globalObject[DOM_INSERTION_DISPATCHER_KEY];
  const existingStateVersion = getStateVersion(existingState);

  if (isDispatcherState(existingState)) {
    return existingState;
  }

  if (existingState) {
    log.warn('DOM insertion dispatcher: replacing stale global state', {
      expectedVersion: DOM_INSERTION_DISPATCHER_STATE_VERSION,
      foundVersion: existingStateVersion,
      validShape: false,
    });
    restoreStaleDispatcherMethods(existingState);
  }

  const state: DomInsertionDispatcherState = {
    handlers: new Map<number, RegisteredDomInsertionHandler>(),
    nextSequence: 0,
    orderedHandlers: [],
    version: DOM_INSERTION_DISPATCHER_STATE_VERSION,
  };

  globalObject[DOM_INSERTION_DISPATCHER_KEY] = state;
  return state;
}

function rebuildOrderedHandlers(state: DomInsertionDispatcherState): void {
  state.orderedHandlers = [...state.handlers.values()].sort(compareHandlers);
}

function normalizeInsertedNode(node: Node): DomInsertionCandidate | null {
  if (node.nodeType !== Node.ELEMENT_NODE) {
    return null;
  }

  if ((node as Element).tagName === 'SCRIPT') {
    const script = node as HTMLScriptElement;
    const url = script.src || script.getAttribute('src') || '';
    return url
      ? {
          element: script,
          kind: 'script',
          url,
        }
      : null;
  }

  if ((node as Element).tagName === 'LINK') {
    const link = node as HTMLLinkElement;
    const rel = link.getAttribute('rel');
    if ((rel !== 'preload' && rel !== 'prefetch') || link.getAttribute('as') !== 'script') {
      return null;
    }

    const url = link.href || link.getAttribute('href') || '';
    return url
      ? {
          element: link,
          kind: 'link',
          rel,
          url,
        }
      : null;
  }

  return null;
}

function dispatchInsertedNode(state: DomInsertionDispatcherState, node: Node): void {
  const candidate = normalizeInsertedNode(node);
  if (!candidate) {
    return;
  }

  for (const handler of state.orderedHandlers) {
    try {
      if (handler.handle(candidate)) {
        return;
      }
    } catch (error) {
      log.error('DOM insertion dispatcher: handler threw, continuing', {
        error,
        handlerId: handler.id,
      });
    }
  }
}

function clearDispatcherState(state: DomInsertionDispatcherState): void {
  state.appendChildWrapper = undefined;
  state.baselineAppendChild = undefined;
  state.baselineInsertBefore = undefined;
  state.handlers.clear();
  state.insertBeforeWrapper = undefined;
  state.nextSequence = 0;
  state.orderedHandlers = [];
}

function installDispatcher(state: DomInsertionDispatcherState): void {
  if (typeof Element === 'undefined' || state.appendChildWrapper || state.insertBeforeWrapper) {
    return;
  }

  state.baselineAppendChild = Element.prototype.appendChild;
  state.baselineInsertBefore = Element.prototype.insertBefore;

  state.appendChildWrapper = function <T extends Node>(this: Element, node: T): T {
    dispatchInsertedNode(state, node);
    return state.baselineAppendChild!.call(this, node) as T;
  };

  state.insertBeforeWrapper = function <T extends Node>(
    this: Element,
    node: T,
    reference: Node | null
  ): T {
    dispatchInsertedNode(state, node);
    return state.baselineInsertBefore!.call(this, node, reference) as T;
  };

  Element.prototype.appendChild = state.appendChildWrapper;
  Element.prototype.insertBefore = state.insertBeforeWrapper;

  log.info('DOM insertion dispatcher: installed shared prototype patch');
}

function teardownDispatcher(state: DomInsertionDispatcherState): void {
  if (typeof Element !== 'undefined') {
    if (state.appendChildWrapper && state.baselineAppendChild) {
      if (Element.prototype.appendChild === state.appendChildWrapper) {
        Element.prototype.appendChild = state.baselineAppendChild;
      } else {
        log.debug('DOM insertion dispatcher: appendChild changed externally, skipping restore');
      }
    }

    if (state.insertBeforeWrapper && state.baselineInsertBefore) {
      if (Element.prototype.insertBefore === state.insertBeforeWrapper) {
        Element.prototype.insertBefore = state.baselineInsertBefore;
      } else {
        log.debug('DOM insertion dispatcher: insertBefore changed externally, skipping restore');
      }
    }
  }

  clearDispatcherState(state);
}

export function registerDomInsertionHandler(handler: DomInsertionHandler): () => void {
  if (typeof Element === 'undefined') {
    return () => {};
  }

  const state = getDispatcherState();
  const sequence = state.nextSequence++;

  state.handlers.set(sequence, { ...handler, sequence });
  rebuildOrderedHandlers(state);

  if (state.handlers.size === 1) {
    installDispatcher(state);
  }

  let active = true;

  return () => {
    if (!active) {
      return;
    }

    active = false;
    state.handlers.delete(sequence);
    rebuildOrderedHandlers(state);

    if (state.handlers.size === 0) {
      teardownDispatcher(state);
    }
  };
}

export function resetDomInsertionDispatcherForTests(): void {
  teardownDispatcher(getDispatcherState());
}
