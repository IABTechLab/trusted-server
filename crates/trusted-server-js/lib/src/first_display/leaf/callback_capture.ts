import type { FirstDisplaySliceActivationContext } from '../transaction';

interface TestlightGlobal {
  que?: unknown[];
  [key: string]: unknown;
}

interface TestlightTarget {
  testlight?: TestlightGlobal;
}

interface TestlightInitialBindings {
  readonly enqueue: (callback: () => void) => void;
  readonly observe: (name: 'callback_count', count: number) => void;
  readonly target: TestlightTarget;
}

function snapshotBindings(candidate: unknown): TestlightInitialBindings | undefined {
  try {
    if (
      typeof candidate !== 'object' ||
      candidate === null ||
      Array.isArray(candidate) ||
      Object.getPrototypeOf(candidate) !== Object.prototype ||
      !Object.isFrozen(candidate) ||
      Reflect.ownKeys(candidate).length !== 3
    ) {
      return undefined;
    }
    const values: Record<string, unknown> = {};
    for (const key of ['enqueue', 'observe', 'target']) {
      const descriptor = Object.getOwnPropertyDescriptor(candidate, key);
      if (!descriptor?.enumerable || !('value' in descriptor)) return undefined;
      values[key] = descriptor.value;
    }
    if (
      typeof values.enqueue !== 'function' ||
      typeof values.observe !== 'function' ||
      typeof values.target !== 'object' ||
      values.target === null
    ) {
      return undefined;
    }
    return {
      enqueue: values.enqueue as TestlightInitialBindings['enqueue'],
      observe: values.observe as TestlightInitialBindings['observe'],
      target: values.target as TestlightTarget,
    };
  } catch {
    return undefined;
  }
}

function ownQueueValues(candidate: unknown): unknown[] {
  if (!Array.isArray(candidate)) return [];
  const entries: Array<readonly [number, unknown]> = [];
  try {
    for (const key of Reflect.ownKeys(candidate)) {
      if (typeof key !== 'string' || !/^(0|[1-9][0-9]*)$/.test(key)) continue;
      const index = Number(key);
      if (!Number.isSafeInteger(index) || index < 0 || index >= 4_294_967_295) continue;
      const descriptor = Object.getOwnPropertyDescriptor(candidate, key);
      if (descriptor?.enumerable && 'value' in descriptor) entries.push([index, descriptor.value]);
    }
  } catch {
    return [];
  }
  entries.sort(([left], [right]) => left - right);
  return entries.map(([, value]) => value);
}

/** Capture preexisting and later Testlight callbacks into the bootstrap ingress once. */
export function installTestlightInitial(
  candidate: unknown,
  own: FirstDisplaySliceActivationContext['own']
): void {
  const bindings = snapshotBindings(candidate);
  if (!bindings || typeof own !== 'function') {
    throw new TypeError('invalid Testlight initial bindings');
  }
  const { enqueue, observe, target } = bindings;
  const previousTargetDescriptor = Object.getOwnPropertyDescriptor(target, 'testlight');
  if (previousTargetDescriptor && !('value' in previousTargetDescriptor)) {
    throw new TypeError('Testlight publisher global accessor is unsupported');
  }
  const currentGlobal = previousTargetDescriptor?.value;
  const global: TestlightGlobal =
    typeof currentGlobal === 'object' && currentGlobal !== null ? currentGlobal : {};
  const createdGlobal = global !== currentGlobal;
  if (createdGlobal && !Reflect.set(target, 'testlight', global)) {
    throw new TypeError('Testlight publisher global is not writable');
  }
  const previousQueueDescriptor = Object.getOwnPropertyDescriptor(global, 'que');
  if (previousQueueDescriptor && !('value' in previousQueueDescriptor)) {
    throw new TypeError('Testlight publisher queue accessor is unsupported');
  }
  const originalQueue =
    previousQueueDescriptor &&
    'value' in previousQueueDescriptor &&
    Array.isArray(previousQueueDescriptor.value)
      ? previousQueueDescriptor.value
      : undefined;
  const queue = ownQueueValues(originalQueue);
  if (originalQueue) originalQueue.length = 0;
  Object.defineProperty(global, 'que', {
    configurable: true,
    enumerable: true,
    value: queue,
    writable: true,
  });

  let active = true;
  own(() => {
    if (!active) return;
    active = false;
    try {
      if (Object.getOwnPropertyDescriptor(global, 'que')?.value === queue) {
        if (previousQueueDescriptor) Object.defineProperty(global, 'que', previousQueueDescriptor);
        else Reflect.deleteProperty(global, 'que');
      }
      if (
        createdGlobal &&
        Object.getOwnPropertyDescriptor(target, 'testlight')?.value === global &&
        Reflect.ownKeys(global).length === 0
      ) {
        if (previousTargetDescriptor) {
          Object.defineProperty(target, 'testlight', previousTargetDescriptor);
        } else {
          Reflect.deleteProperty(target, 'testlight');
        }
      }
    } catch {
      // Publisher replacement wins over provisional rollback.
    }
  });

  let forwarded = 0;
  const forward = (values: readonly unknown[]): void => {
    for (const value of values) {
      if (typeof value !== 'function') continue;
      try {
        enqueue(value as () => void);
      } catch {
        // One publisher callback or hostile ingress cannot block later callbacks.
      }
      forwarded += 1;
    }
    observe('callback_count', forwarded);
  };
  const pending = ownQueueValues(queue);
  queue.length = 0;
  const nativePush = queue.push.bind(queue);
  Object.defineProperty(queue, 'push', {
    configurable: true,
    enumerable: false,
    value: (...values: unknown[]): number => {
      if (!active) return nativePush(...values);
      const length = nativePush(...values);
      const later = ownQueueValues(queue);
      queue.length = 0;
      forward(later);
      return length;
    },
    writable: false,
  });
  forward(pending);
}
