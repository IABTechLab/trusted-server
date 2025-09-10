import { describe, it, expect, beforeEach, vi } from 'vitest'

describe('config', () => {
  beforeEach(async () => {
    // reset module state between tests
    await vi.resetModules()
  })

  it('sets and gets config, controls log level', async () => {
    const { setConfig, getConfig } = await import('../../src/core/config')
    const { log } = await import('../../src/core/log')

    setConfig({ a: 1 })
    expect(getConfig()).toMatchObject({ a: 1 })

    setConfig({ debug: true })
    expect(log.getLevel()).toBe('debug')

    setConfig({ logLevel: 'info' as any })
    expect(log.getLevel()).toBe('info')
  })
})
