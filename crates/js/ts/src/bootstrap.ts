import type { TsjsApi } from './types'
import { addAdUnits } from './registry'
import { renderAdUnit, renderAllAdUnits } from './render'
import { log } from './log'
import { setConfig, getConfig } from './config'
import { requestBids, getHighestCpmBids } from './prebid'
import { installQueue } from './queue'

const VERSION = '0.1.0'

const w: (Window & { tsjs?: TsjsApi; pbjs?: TsjsApi }) =
  ((globalThis as unknown as { window?: Window }).window as Window & {
    tsjs?: TsjsApi
    pbjs?: TsjsApi
  }) || ({} as Window & { tsjs?: TsjsApi; pbjs?: TsjsApi })

// Collect existing queued fns before we overwrite
const existingTsjsQue: Array<() => void> = Array.isArray(w.tsjs?.que) ? [...w.tsjs.que] : []
const existingPbjsQue: Array<() => void> = Array.isArray(w.pbjs?.que) ? [...w.pbjs.que] : []
const pending = [...existingTsjsQue, ...existingPbjsQue]

// Create API and attach methods
const api: TsjsApi = (w.tsjs ??= {} as TsjsApi)
api.version = VERSION
api.addAdUnits = addAdUnits
api.renderAdUnit = renderAdUnit
api.renderAllAdUnits = () => renderAllAdUnits()
api.log = log
api.setConfig = setConfig
api.getConfig = getConfig
api.requestBids = requestBids
api.getHighestCpmBids = getHighestCpmBids

// Point both globals to the same object
w.tsjs = api
w.pbjs = api

// Single shared queue
installQueue(api, w)

// Flush prior queued callbacks
for (const fn of pending) {
  try {
    if (typeof fn === 'function') {
      fn.call(api)
      log.debug('queue: flushed callback')
    }
  } catch {
    /* ignore queued callback error */
  }
}

log.info('tsjs initialized', {
  methods: [
    'setConfig',
    'getConfig',
    'requestBids',
    'getHighestCpmBids',
    'addAdUnits',
    'renderAdUnit',
    'renderAllAdUnits',
  ],
  hasGetHighestCpmBids: typeof w.pbjs?.getHighestCpmBids === 'function',
})
