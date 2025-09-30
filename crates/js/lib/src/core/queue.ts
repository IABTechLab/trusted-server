// Minimal Prebid-style queue shim that executes callbacks immediately.
import { log } from './log';

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
