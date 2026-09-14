import { log } from './log';

export type QueueCallback = (this: object) => void;

export interface PublishedQueue {
  readonly queue: unknown[];
  readonly drain: () => void;
}

type QueueOwner = object & { que?: unknown };

function immediatePush(owner: object): unknown[]['push'] {
  return function (item: unknown): number {
    if (typeof item !== 'function') return 0;
    try {
      (item as QueueCallback).call(owner);
    } catch (error) {
      try {
        log.warn('queue: callback failed', error);
      } catch {
        // Callback isolation cannot depend on an observer.
      }
      return 0;
    }
    try {
      log.debug('queue: push executed immediately');
    } catch {
      // Queue behavior cannot depend on an observer.
    }
    return 0;
  } as unknown[]['push'];
}

function ownArrayEntries(value: unknown[]): readonly [number, unknown][] {
  const entries: [number, unknown][] = [];
  for (const key of Reflect.ownKeys(value)) {
    if (typeof key !== 'string' || !/^(0|[1-9][0-9]*)$/.test(key)) continue;
    const index = Number(key);
    if (!Number.isSafeInteger(index) || index < 0 || index >= 4_294_967_295) continue;
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    if (descriptor && 'value' in descriptor) entries.push([index, descriptor.value]);
  }
  entries.sort(([left], [right]) => left - right);
  return entries;
}

function canReuseIngress(value: unknown[]): boolean {
  if (!Object.isExtensible(value)) return false;
  const lengthDescriptor = Object.getOwnPropertyDescriptor(value, 'length');
  if (!lengthDescriptor?.writable) return false;
  const pushDescriptor = Object.getOwnPropertyDescriptor(value, 'push');
  if (pushDescriptor && !pushDescriptor.configurable) return false;
  return ownArrayEntries(value).every(([index]) => {
    const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
    return descriptor?.configurable === true;
  });
}

function preflightTerminalFields(
  target: QueueOwner,
  committedFields: Readonly<Record<string, unknown>>,
  removedFields: readonly string[]
): Readonly<{ committed: readonly string[]; removed: readonly string[] }> {
  const keys: string[] = [];
  const seen = new Set<string>();
  for (const key of Reflect.ownKeys(committedFields)) {
    if (typeof key !== 'string') continue;
    const field = Object.getOwnPropertyDescriptor(committedFields, key);
    if (!field || !('value' in field)) continue;
    const existing = Object.getOwnPropertyDescriptor(target, key);
    if (existing && !existing.configurable) {
      throw new TypeError(`TSJS terminal field is not configurable: ${key}`);
    }
    keys.push(key);
    seen.add(key);
  }
  const removed: string[] = [];
  for (const key of removedFields) {
    if (seen.has(key) || removed.includes(key)) {
      throw new TypeError(`TSJS terminal field inventory overlaps: ${key}`);
    }
    const existing = Object.getOwnPropertyDescriptor(target, key);
    if (existing && !existing.configurable) {
      throw new TypeError(`TSJS removed field is not configurable: ${key}`);
    }
    removed.push(key);
  }
  // A terminal publication is an exact replacement, not a compatibility
  // merge. Remove every other own string field without carrying an inventory
  // of retired public names into the shipped bundle.
  for (const key of Object.getOwnPropertyNames(target)) {
    if (key === 'que' || seen.has(key) || removed.includes(key)) continue;
    const existing = Object.getOwnPropertyDescriptor(target, key);
    if (existing && !existing.configurable) {
      throw new TypeError(`TSJS unpublished field is not configurable: ${key}`);
    }
    removed.push(key);
  }
  const queueDescriptor = Object.getOwnPropertyDescriptor(target, 'que');
  if (queueDescriptor && !queueDescriptor.configurable) {
    throw new TypeError('TSJS terminal field is not configurable: que');
  }
  return Object.freeze({ committed: Object.freeze(keys), removed: Object.freeze(removed) });
}

/** Side-effect-free ordinary-object preflight used before fallback queue normalization. */
export function canPublishTerminalFields(
  target: QueueOwner,
  committedFields: Readonly<Record<string, unknown>>,
  removedFields: readonly string[] = Object.freeze([])
): boolean {
  try {
    preflightTerminalFields(target, committedFields, removedFields);
    return true;
  } catch {
    return false;
  }
}

function preflightPublication(
  target: QueueOwner,
  ingress: unknown[],
  committedFields: Readonly<Record<string, unknown>>,
  removedFields: readonly string[]
): Readonly<{ committed: readonly string[]; removed: readonly string[] }> {
  if (!canReuseIngress(ingress)) {
    throw new TypeError('TSJS ingress queue cannot be committed');
  }
  return preflightTerminalFields(target, committedFields, removedFields);
}

/** Establishes the mutable preload queue used only during bootstrap preparation. */
export function prepareQueue<T extends QueueOwner>(target: T): unknown[] {
  const existing = Object.getOwnPropertyDescriptor(target, 'que');
  const publisherQueue =
    existing && 'value' in existing && Array.isArray(existing.value) ? existing.value : undefined;
  const ingress = publisherQueue && canReuseIngress(publisherQueue) ? publisherQueue : [];
  if (publisherQueue && ingress !== publisherQueue) {
    for (const [index, value] of ownArrayEntries(publisherQueue)) ingress[index] = value;
  }
  Object.defineProperty(ingress, 'push', {
    configurable: true,
    enumerable: false,
    value: Array.prototype.push,
    writable: true,
  });
  Object.defineProperty(target, 'que', {
    configurable: true,
    enumerable: true,
    value: ingress,
    writable: false,
  });
  return ingress;
}

/**
 * Performs the terminal, synchronous queue and public-field handoff.
 *
 * The returned queue is a frozen real Array whose own `push` executes callable
 * entries immediately without ever retaining them.
 */
export function publishQueue<T extends QueueOwner>(
  target: T,
  ingress: unknown[],
  committedFields: Readonly<Record<string, unknown>> = {},
  removedFields: readonly string[] = Object.freeze([])
): PublishedQueue {
  const inventory = preflightPublication(target, ingress, committedFields, removedFields);
  const queue: unknown[] = [];
  Object.defineProperty(queue, 'push', {
    configurable: false,
    enumerable: false,
    value: immediatePush(target),
    writable: false,
  });
  Object.freeze(queue);

  const snapshot: QueueCallback[] = [];
  for (const [, value] of ownArrayEntries(ingress)) {
    if (typeof value === 'function') snapshot.push(value as QueueCallback);
  }

  ingress.length = 0;
  Object.defineProperty(ingress, 'push', {
    configurable: false,
    enumerable: false,
    value: immediatePush(target),
    writable: false,
  });
  Object.freeze(ingress);

  for (const key of inventory.removed) {
    if (!Reflect.deleteProperty(target, key)) {
      throw new TypeError(`TSJS removed field could not be deleted: ${key}`);
    }
  }
  for (const key of inventory.committed) {
    const descriptor = Object.getOwnPropertyDescriptor(committedFields, key);
    if (!descriptor || !('value' in descriptor)) {
      throw new TypeError(`TSJS terminal field changed during publication: ${key}`);
    }
    Object.defineProperty(target, key, {
      configurable: false,
      enumerable: descriptor.enumerable ?? true,
      value: descriptor.value,
      writable: false,
    });
  }
  Object.defineProperty(target, 'que', {
    configurable: false,
    enumerable: true,
    value: queue,
    writable: false,
  });

  let drained = false;
  return Object.freeze({
    queue,
    drain: () => {
      if (drained) return;
      drained = true;
      for (const callback of snapshot) queue.push(callback);
    },
  });
}

/** Publish and immediately drain a queue outside the transactional registry. */
export function commitQueue<T extends QueueOwner>(
  target: T,
  ingress: unknown[],
  committedFields: Readonly<Record<string, unknown>> = {}
): unknown[] {
  const published = publishQueue(target, ingress, committedFields);
  published.drain();
  return published.queue;
}

// Replace the legacy Prebid-style queue with an immediate executor so queued work runs in order.
export function installQueue<T extends { que?: Array<() => void> }>(
  target: T,
  w: Window & { tsjs?: T }
) {
  const q: Array<() => void> = [];
  q.push = ((fn: () => void) => {
    if (typeof fn === 'function') {
      try {
        fn.call(target);
        log.debug('queue: push executed immediately');
      } catch {
        /* ignore queued fn error */
      }
    }
    return q.length;
  }) as typeof q.push;
  target.que = q;
  if (w.tsjs) w.tsjs.que = q;
}
