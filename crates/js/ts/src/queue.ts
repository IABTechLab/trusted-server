import { log } from './log'

export function installQueue<T extends { que?: Array<() => void> }>(
  target: T,
  w: Window & { tsjs?: T; pbjs?: T }
) {
  const q: Array<() => void> = []
  q.push = ((fn: () => void) => {
    if (typeof fn === 'function') {
      try {
        fn.call(target)
        log.debug('queue: push executed immediately')
      } catch {
        /* ignore queued fn error */
      }
    }
    return q.length
  }) as typeof q.push
  target.que = q
  if (w.tsjs) w.tsjs.que = q
  if (w.pbjs) w.pbjs.que = q
}
