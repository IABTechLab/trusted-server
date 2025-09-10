import { describe, it, expect, beforeEach, vi } from 'vitest'

// Ensure mocks referenced inside vi.mock factory are hoisted
const { renderMock } = vi.hoisted(() => ({ renderMock: vi.fn() }))

describe('bids.requestBids', () => {
  beforeEach(async () => {
    await vi.resetModules()
    document.body.innerHTML = ''
  })

  it('sends fetch and renders creatives from response', async () => {
    // mock render module to capture calls
    vi.mock('../../src/core/render', async () => {
      const actual = await vi.importActual<any>('../../src/core/render')
      return {
        ...actual,
        renderCreativeIntoSlot: (slotId: string, html: string) => renderMock(slotId, html),
      }
    })

    // mock fetch
    ;(globalThis as any).fetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      headers: { get: () => 'application/json' },
      json: async () => ({ seatbid: [{ bid: [{ impid: 'slot1', adm: '<div>ad</div>' }] }] }),
    })

    const { addAdUnits } = await import('../../src/core/registry')
    const { requestBids } = await import('../../src/core/bids')

    document.body.innerHTML = '<div id="slot1"></div>'
    addAdUnits({ code: 'slot1', mediaTypes: { banner: { sizes: [[300, 250]] } } } as any)

    requestBids()
    // wait microtasks
    await Promise.resolve()
    await Promise.resolve()

    expect((globalThis as any).fetch).toHaveBeenCalled()
    expect(renderMock).toHaveBeenCalledWith('slot1', '<div>ad</div>')
  })
})
