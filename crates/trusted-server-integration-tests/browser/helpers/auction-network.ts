import type { BrowserContext } from '@playwright/test'

export interface NetworkViolation {
  kind: 'http' | 'websocket'
  url: string
  authority: string
}

// Entries come from the worker's listener manifest, never observed traffic.
export async function guardAuctionNetwork(
  context: BrowserContext,
  origins: string[]
) {
  const allowed = new Set(origins)
  const violations: NetworkViolation[] = []
  function check(raw: string, kind: NetworkViolation['kind']) {
    const url = new URL(raw)
    if (['data:', 'about:', 'blob:'].includes(url.protocol)) return true
    if (kind === 'http' && allowed.has(url.origin)) return true
    violations.push({
      kind,
      url: raw,
      authority: `${url.protocol}//${url.host}`,
    })
    return false
  }
  // Redirect hops may bypass route interception, but emit context events.
  context.on('request', (request) => {
    check(request.url(), 'http')
  })
  await context.route('**/*', (route) => {
    const url = new URL(route.request().url())
    return allowed.has(url.origin)
      ? route.continue()
      : route.abort('blockedbyclient')
  })
  context.on('page', (page) => {
    page.on('websocket', (socket) => {
      check(socket.url(), 'websocket')
    })
  })
  await context.routeWebSocket('**/*', (socket) => {
    check(socket.url(), 'websocket')
    socket.close({ code: 1008, reason: 'Fixture egress is forbidden' })
  })
  return {
    violations,
    assertClean() {
      if (violations.length)
        throw new Error(
          `Off-list fixture requests: ${JSON.stringify(violations)}`
        )
    },
  }
}
