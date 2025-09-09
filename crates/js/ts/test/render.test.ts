import { describe, it, expect, beforeEach, vi } from 'vitest'

describe('render', () => {
  beforeEach(async () => {
    await vi.resetModules()
    document.body.innerHTML = ''
  })

  it('injects creative into existing slot', async () => {
    const { renderCreativeIntoSlot } = await import('../src/render')
    const div = document.createElement('div')
    div.id = 'slotA'
    document.body.appendChild(div)
    renderCreativeIntoSlot('slotA', '<span>ad</span>')
    expect(document.getElementById('slotA')!.innerHTML).toContain('<span>ad</span>')
  })
})
