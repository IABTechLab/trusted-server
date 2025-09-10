import type { TsjsApi } from '../core/types'
import { log } from '../core/log'
import { installQueue } from '../core/queue'

export function installPrebidJsShim(): boolean {
  const w: (Window & { tsjs?: TsjsApi; pbjs?: TsjsApi }) =
    ((globalThis as unknown as { window?: Window }).window as Window & {
      tsjs?: TsjsApi
      pbjs?: TsjsApi
    }) || ({} as Window & { tsjs?: TsjsApi; pbjs?: TsjsApi })

  // Ensure core exists
  const api: TsjsApi = (w.tsjs ??= { version: '0.0.0', que: [] } as TsjsApi)

  // Capture any queued pbjs callbacks before aliasing
  const pending: Array<() => void> = Array.isArray(w.pbjs?.que) ? [...(w.pbjs as TsjsApi).que] : []

  // Core provides requestBids/getHighestCpmBids; extension only aliases pbjs

  // Alias pbjs to tsjs and ensure a single shared queue
  w.pbjs = api
  if (!Array.isArray(api.que)) {
    installQueue(api, w)
  }
  // Make sure both globals share the same queue
  if (Array.isArray(api.que)) {
    (w.pbjs as TsjsApi).que = api.que
  }

  // Flush previously queued pbjs callbacks
  for (const fn of pending) {
    try {
      if (typeof fn === 'function') {
        fn.call(api)
        log.debug('prebidjs extension: flushed callback')
      }
    } catch {
      /* ignore queued callback error */
    }
  }

  log.info('prebidjs extension installed', {
    hasRequestBids: typeof api.requestBids === 'function',
    hasGetHighestCpmBids: typeof api.getHighestCpmBids === 'function',
  })

  return true
}

export default installPrebidJsShim
