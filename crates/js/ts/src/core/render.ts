import { log } from './log'
import type { AdUnit } from './types'
import { getUnit, getAllUnits, firstSize } from './registry'

export function findSlot(id: string): HTMLElement | null {
  return document.getElementById(id)
}

function ensureSlot(id: string): HTMLElement {
  let el = document.getElementById(id)
  if (!el) {
    el = document.createElement('div')
    el.id = id
    document.body.appendChild(el)
  }
  return el
}

export function renderAdUnit(codeOrUnit: string | AdUnit): void {
  const code = typeof codeOrUnit === 'string' ? codeOrUnit : codeOrUnit?.code
  if (!code) return
  const unit = typeof codeOrUnit === 'string' ? getUnit(code) : codeOrUnit
  const size = (unit && firstSize(unit)) || [300, 250]
  const el = ensureSlot(code)
  try {
    el.textContent = `Trusted Server — ${size[0]}x${size[1]}`
    log.info('renderAdUnit: rendered placeholder', { code, size })
  } catch {
    log.warn('renderAdUnit: failed', { code })
  }
}

export function renderAllAdUnits(): void {
  try {
    for (const u of getAllUnits()) {
      renderAdUnit(u)
    }
    log.info('renderAllAdUnits: rendered all placeholders', { count: getAllUnits().length })
  } catch {
    log.warn('renderAllAdUnits: failed')
  }
}

export function renderCreativeIntoSlot(slotId: string, html: string): void {
  const el = findSlot(slotId)
  if (!el) {
    log.warn('renderCreativeIntoSlot: slot not found', { slotId })
    return
  }
  try {
    el.innerHTML = html
    log.info('renderCreativeIntoSlot: rendered', { slotId })
  } catch (err) {
    log.warn('renderCreativeIntoSlot: failed', { slotId, err })
  }
}

