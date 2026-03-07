import { log } from '../core/log';

type AppendChildMethod = typeof Element.prototype.appendChild;
type InsertBeforeMethod = typeof Element.prototype.insertBefore;

const DOM_INSERTION_DISPATCHER_KEY = Symbol.for('trusted-server.domInsertionDispatcher');

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

function getDispatcherState(): DomInsertionDispatcherState {
  const globalObject = globalThis as Record<PropertyKey, unknown>;
  const existingState = globalObject[DOM_INSERTION_DISPATCHER_KEY];

  if (existingState) {
    return existingState as DomInsertionDispatcherState;
  }

  const state: DomInsertionDispatcherState = {
    handlers: new Map<number, RegisteredDomInsertionHandler>(),
    nextSequence: 0,
    orderedHandlers: [],
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
