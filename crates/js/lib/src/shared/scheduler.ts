// Mutation observer helper that batches callbacks onto the microtask queue.
import { queueTask } from './async';

// Coalesce repeated mutation callbacks on the same element into a single microtask run.
export function createMutationScheduler<T extends Element>(perform: (target: T) => void) {
  const queued = new WeakSet<T>();
  return (target: T) => {
    if (queued.has(target)) return;
    queued.add(target);
    queueTask(() => {
      queued.delete(target);
      perform(target);
    });
  };
}
