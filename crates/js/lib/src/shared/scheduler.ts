import { queueTask } from './async';

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
