import { expect, type Page } from '@playwright/test'
import { readState } from './state.js'

/** Clicks the real handoff button inside the existing closed shadow root. */
export async function clickTraceHandoff(page: Page): Promise<void> {
  const session = await page.context().newCDPSession(page)
  try {
    let backendNodeId: number | undefined
    await expect
      .poll(async () => {
        const tree = await session.send('Accessibility.getFullAXTree')
        const nodes = tree.nodes as Array<{
          ignored: boolean
          backendDOMNodeId?: number
          role?: { value?: string }
          name?: { value?: string }
        }>
        const buttons = nodes.filter(
          (node) =>
            !node.ignored &&
            node.role?.value === 'button' &&
            node.name?.value === 'View trace results'
        )
        backendNodeId =
          buttons.length === 1 ? buttons[0].backendDOMNodeId : undefined
        return backendNodeId
      })
      .toBeDefined()
    const { model } = await session.send('DOM.getBoxModel', { backendNodeId })
    const points = model.border as number[]
    // A real pointer click keeps browser user activation and the actual
    // production button handler. No private callback or report is injected.
    await page.mouse.click(
      (points[0] + points[2] + points[4] + points[6]) / 4,
      (points[1] + points[3] + points[5] + points[7]) / 4
    )
  } finally {
    await session.detach()
  }
}

/** Resolves a browser-suitable localhost URL for the dedicated trace fixture. */
export function traceRuntimeUrl(path: string, authenticated = false): string {
  const state = readState()
  const baseUrl = authenticated ? state.traceAuthBaseUrl : state.traceBaseUrl
  if (!baseUrl)
    throw new Error(
      'Run scripts/integration-tests-browser.sh to create the dedicated trace fixtures'
    )
  const url = new URL(path, baseUrl)
  url.hostname = 'localhost'
  return url.toString()
}

/** Fictional credentials already present in the integration secret-store fixture. */
export const TRACE_FIXTURE_CREDENTIALS = {
  username: 'admin',
  password: 'integration-admin-password-32-bytes-ok',
}
