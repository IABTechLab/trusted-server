// Mutation observer helper that batches callbacks onto the microtask queue.
import { queueTask } from './async';

export interface MutationScheduler<T extends Element> {
  (target: T): void;
  readonly dispose: () => void;
}

// Coalesce repeated mutation callbacks on the same element into a single microtask run.
export function createMutationScheduler<T extends Element>(
  perform: (target: T) => void
): MutationScheduler<T> {
  const queued = new WeakSet<T>();
  let active = true;
  const schedule = ((target: T): void => {
    if (!active) return;
    if (queued.has(target)) return;
    queued.add(target);
    queueTask(() => {
      queued.delete(target);
      if (!active) return;
      perform(target);
    });
  }) as MutationScheduler<T>;
  Object.defineProperty(schedule, 'dispose', {
    configurable: false,
    enumerable: true,
    value: (): void => {
      active = false;
    },
    writable: false,
  });
  return Object.freeze(schedule);
}
