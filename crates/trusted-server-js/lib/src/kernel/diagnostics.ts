import type { BootManifestV1 } from '../core/types';

const MAX_INTEGRATION_SUBSCRIPTIONS = 16;
const MAX_PENDING_OBSERVATIONS = 512;
const MAX_OBSERVATION_DEPTH = 16;
const MAX_OBSERVATION_NODES = 512;
const INTEGRATION_ID = /^[a-z0-9][a-z0-9_-]{0,63}$/;

export type DiagnosticsObservation = Readonly<Record<string, unknown>>;
export type DiagnosticsListener = (observation: DiagnosticsObservation) => void;

export interface DiagnosticsScheduler {
  readonly set: (callback: () => void, milliseconds: number) => unknown;
  readonly clear: (handle: unknown) => void;
}

export interface DiagnosticsBusOptions {
  readonly manifest: Readonly<BootManifestV1>;
  /** Closure-private core observer; never included in the returned bus facade. */
  readonly onObservation?: (observation: DiagnosticsObservation) => void;
  readonly onOverflow?: (droppedObservations: number) => void;
  readonly onSubscriberError?: (error: unknown) => void;
  readonly pendingCapacity?: number;
  readonly scheduler?: DiagnosticsScheduler;
}

export interface DiagnosticsBus {
  readonly publish: (observation: DiagnosticsObservation) => boolean;
  readonly subscribe: (id: string, listener: DiagnosticsListener) => (() => void) | undefined;
  readonly dispose: () => void;
}

interface Subscription {
  readonly id: string;
  readonly listener: DiagnosticsListener;
  active: boolean;
}

interface PendingObservation {
  readonly observation: DiagnosticsObservation;
  readonly subscriptions: readonly Subscription[];
}

function defaultScheduler(): DiagnosticsScheduler {
  return Object.freeze({
    clear: (handle: unknown): void => {
      globalThis.clearTimeout(handle as ReturnType<typeof globalThis.setTimeout>);
    },
    set: (callback: () => void, milliseconds: number): unknown =>
      globalThis.setTimeout(callback, milliseconds),
  });
}

function recursivelyFrozenRecord(candidate: unknown): candidate is DiagnosticsObservation {
  if (typeof candidate !== 'object' || candidate === null || Array.isArray(candidate)) return false;
  const visited = new Set<object>();
  let nodes = 0;
  const visit = (value: unknown, depth: number): boolean => {
    if (typeof value === 'function') return false;
    if (typeof value !== 'object' || value === null) return true;
    if (visited.has(value)) return true;
    if (depth > MAX_OBSERVATION_DEPTH || nodes >= MAX_OBSERVATION_NODES) return false;
    visited.add(value);
    nodes += 1;
    try {
      const prototype = Object.getPrototypeOf(value) as unknown;
      if (!Array.isArray(value) && prototype !== Object.prototype && prototype !== null) {
        return false;
      }
      if (!Object.isFrozen(value)) return false;
      const keys = Reflect.ownKeys(value);
      for (let index = 0; index < keys.length; index += 1) {
        const key = keys[index];
        if (key === undefined) return false;
        const descriptor = Object.getOwnPropertyDescriptor(value, key);
        if (!descriptor || !('value' in descriptor) || !visit(descriptor.value, depth + 1)) {
          return false;
        }
      }
      return true;
    } catch {
      return false;
    }
  };
  return visit(candidate, 0);
}

/** Create the closure-private, failure-isolated diagnostics transport for one runtime. */
export function createDiagnosticsBus(options: DiagnosticsBusOptions): DiagnosticsBus {
  const allowedIds = new Set<string>();
  try {
    const integrationsDescriptor = Object.getOwnPropertyDescriptor(
      options.manifest,
      'integrations'
    );
    const integrations =
      integrationsDescriptor && 'value' in integrationsDescriptor
        ? (integrationsDescriptor.value as readonly unknown[])
        : [];
    for (let index = 0; index < integrations.length; index += 1) {
      const entry = integrations[index];
      if (typeof entry !== 'object' || entry === null) continue;
      const idDescriptor = Object.getOwnPropertyDescriptor(entry, 'id');
      const id = idDescriptor && 'value' in idDescriptor ? idDescriptor.value : undefined;
      if (typeof id === 'string' && INTEGRATION_ID.test(id)) allowedIds.add(id);
    }
  } catch {
    // Invalid manifest identities admit no diagnostic consumers.
  }
  const pendingCapacity =
    Number.isSafeInteger(options.pendingCapacity) &&
    (options.pendingCapacity ?? 0) > 0 &&
    (options.pendingCapacity ?? 0) <= MAX_PENDING_OBSERVATIONS
      ? options.pendingCapacity!
      : MAX_PENDING_OBSERVATIONS;
  const scheduler = options.scheduler ?? defaultScheduler();
  const subscriptions = new Map<string, Subscription>();
  const pending: PendingObservation[] = [];
  let disposed = false;
  let droppedObservations = 0;
  let scheduled = false;
  let scheduledHandle: unknown;

  const reportSubscriberError = (error: unknown): void => {
    try {
      options.onSubscriberError?.(error);
    } catch {
      // Diagnostics error reporting is observation only.
    }
  };

  const drain = (): void => {
    scheduled = false;
    scheduledHandle = undefined;
    if (disposed) {
      pending.length = 0;
      return;
    }
    while (pending.length > 0 && !disposed) {
      const item = pending.shift();
      if (!item) continue;
      for (let index = 0; index < item.subscriptions.length; index += 1) {
        const subscription = item.subscriptions[index];
        if (!subscription?.active || subscriptions.get(subscription.id) !== subscription) {
          continue;
        }
        try {
          subscription.listener(item.observation);
        } catch (error) {
          reportSubscriberError(error);
        }
      }
    }
  };

  const scheduleDrain = (): boolean => {
    if (scheduled) return true;
    scheduled = true;
    try {
      const handle = scheduler.set(drain, 0);
      if (scheduled) scheduledHandle = handle;
      return true;
    } catch {
      scheduled = false;
      scheduledHandle = undefined;
      pending.length = 0;
      return false;
    }
  };

  return Object.freeze({
    publish: (observation: DiagnosticsObservation): boolean => {
      if (disposed || !recursivelyFrozenRecord(observation)) return false;
      try {
        options.onObservation?.(observation);
      } catch {
        // Core diagnostics consumption cannot affect correctness publication.
      }
      const captured = Object.freeze([...subscriptions.values()]);
      if (captured.length === 0) return true;
      if (pending.length >= pendingCapacity) {
        pending.shift();
        droppedObservations += 1;
        try {
          options.onOverflow?.(droppedObservations);
        } catch {
          // Diagnostics overflow accounting cannot affect correctness work.
        }
      }
      pending.push(Object.freeze({ observation, subscriptions: captured }));
      return scheduleDrain();
    },
    subscribe: (id: string, listener: DiagnosticsListener): (() => void) | undefined => {
      if (
        disposed ||
        typeof listener !== 'function' ||
        !allowedIds.has(id) ||
        subscriptions.has(id) ||
        subscriptions.size >= MAX_INTEGRATION_SUBSCRIPTIONS
      ) {
        return undefined;
      }
      const subscription: Subscription = { id, listener, active: true };
      subscriptions.set(id, subscription);
      return (): void => {
        if (!subscription.active) return;
        subscription.active = false;
        if (subscriptions.get(id) === subscription) subscriptions.delete(id);
      };
    },
    dispose: (): void => {
      if (disposed) return;
      disposed = true;
      for (const subscription of subscriptions.values()) subscription.active = false;
      subscriptions.clear();
      pending.length = 0;
      if (scheduled) {
        scheduled = false;
        try {
          scheduler.clear(scheduledHandle);
        } catch {
          // The disposed flag suppresses a hostile late scheduler callback.
        }
      }
      scheduledHandle = undefined;
    },
  });
}
