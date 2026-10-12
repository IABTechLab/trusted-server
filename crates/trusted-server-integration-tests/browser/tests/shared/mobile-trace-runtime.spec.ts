import { expect, test } from '@playwright/test'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { clickTraceHandoff } from '../../helpers/trace-fixture.js'

const SESSION = '__Host-ts-console'
const REPORT_KEY = 'trusted-server.trace.report.v1'
const PUBLISHER_PATH = '/gpt-diagnostics'

test.skip(
  !process.env.TRACE_BROWSER_ORIGIN,
  'Run the Rust-owned runtime workflow with playwright.trace-runtime.config.ts'
)

test('stores a real browser session and captures, views, exports and clears its publisher report', async ({
  page,
  baseURL,
}) => {
  if (!baseURL)
    throw new Error('The Rust harness must supply the runtime origin')
  const publisher = new URL(PUBLISHER_PATH, baseURL).toString()
  const setup = new URL('/_ts/trace', baseURL).toString()
  const controls: string[] = []
  page.on('request', (request) => {
    const path = new URL(request.url()).pathname
    if (
      path.startsWith('/_ts/trace/') &&
      !path.startsWith('/_ts/trace/assets/')
    )
      controls.push(`${request.method()} ${path}`)
  })
  // Only GPT's callbacks are mocked. The adapter serves all activation,
  // request-context, TSJS, capture, handoff and viewer bytes itself.
  await page.addInitScript({
    path: resolve(__dirname, '../../helpers/trace-gpt-fixture.js'),
  })
  await page.goto(publisher)
  expect(
    await page.evaluate(() => Reflect.get(window, '__tsjs_trace_active'))
  ).toBeUndefined()
  expect(
    (await page.context().cookies()).find((cookie) => cookie.name === SESSION)
  ).toBeUndefined()

  const shell = await page.goto(setup)
  expect(shell?.status()).toBe(200)
  expect(shell?.headers()['cache-control']).toBe('no-store, private')
  expect(shell?.headers()['set-cookie']).toBeUndefined()
  await expect(
    page.getByRole('heading', { name: 'Setup request', exact: true })
  ).toBeVisible()
  expect(controls).toEqual([])
  expect(
    (await page.context().cookies()).find((cookie) => cookie.name === SESSION)
  ).toBeUndefined()
  const historyLength = await page.evaluate(() => history.length)
  const observedState = page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === '/_ts/trace/state' &&
      response.request().method() === 'GET'
  )
  await page
    .getByRole('button', { name: 'Enable tracing', exact: true })
    .click()
  expect(await (await observedState).json()).toEqual({ observed_active: true })
  await expect(page.locator('#trace-session-state')).toHaveText(
    'Tracing is on — cookie observed by server'
  )
  expect(controls).toEqual(['POST /_ts/trace/enable', 'GET /_ts/trace/state'])
  expect(await page.evaluate(() => history.length)).toBe(historyLength)
  const cookie = (await page.context().cookies()).find(
    (item) => item.name === SESSION
  )
  expect(cookie).toMatchObject({
    value: '1',
    domain: 'localhost',
    path: '/',
    secure: true,
    httpOnly: true,
    sameSite: 'Lax',
  })
  expect(
    Math.abs((cookie?.expires ?? 0) - (Date.now() / 1000 + 1800))
  ).toBeLessThan(10)
  expect(await page.evaluate(() => document.cookie)).not.toContain(SESSION)

  await page
    .getByRole('button', { name: 'Return to previous page', exact: true })
    .click()
  await page.waitForURL(publisher)
  const documentResponse = await page.reload()
  expect(documentResponse?.status()).toBe(200)
  expect(documentResponse?.headers()['cache-control']).toContain('no-store')
  expect(documentResponse?.headers()['cache-control']).toContain('private')
  await page.waitForFunction(() => {
    const api = Reflect.get(window, 'tsjs')
    return (
      Reflect.get(window, '__tsjs_trace_active') === true &&
      Boolean(api?.gptDiagnostics && api?.traceEvidence)
    )
  })
  const context = await page.evaluate(() => {
    const value = Reflect.get(window, '__tsjs_trace_request_context')
    if (
      ![
        value,
        value.network,
        value.cookies,
        ...Object.values(value.cookies),
      ].every(Object.isFrozen)
    )
      throw new Error('The publisher must emit frozen request context')
    return value
  })
  expect(context.cookies.diagnostics_session).toMatchObject({
    source: 'request',
    state: 'present_valid',
    detail: 'valid_diagnostics_value',
  })

  // No server opportunity or correlation tokens are invented for this disabled
  // auction fixture. A client GPT cycle exercises the real observer and export.
  await page.evaluate(() => {
    const gpt = Reflect.get(window, 'googletag')
    gpt
      .defineSlot(
        '/123456789/example-runtime',
        [300, 250],
        'gpt-diagnostics-slot-primary'
      )
      .addService(gpt.pubads())
    gpt.display('gpt-diagnostics-slot-primary')
    Reflect.get(window, '__traceGptFixture').complete(0)
  })
  await page.waitForFunction(
    () =>
      Reflect.get(window, 'tsjs').gptDiagnostics.snapshot().slots[0]
        ?.requests[0]?.isEmpty === true
  )
  const evidence = await page.evaluate(() =>
    Reflect.get(window, 'tsjs').traceEvidence.snapshot()
  )
  expect(evidence.ok).toBe(true)
  expect(evidence.value.slotCorrelations).toEqual([])
  await page.evaluate(() =>
    sessionStorage.setItem('unrelated-fixture-key', 'retained')
  )
  await clickTraceHandoff(page)
  await page.waitForURL(setup)
  const article = page.locator('#trace-report')
  await expect(article).toContainText(
    'Browser-carried, unverified diagnostic data'
  )
  const stored = await page.evaluate(
    (key) => JSON.parse(sessionStorage.getItem(key)!),
    REPORT_KEY
  )
  expect(stored.report.request_context).toEqual(context)
  expect(stored.report.server_auctions).toEqual(evidence.value.serverAuctions)
  expect(stored.report.slot_correlations).toEqual([])
  expect(stored.report.auction_coverage.capture_status).toBe(
    evidence.value.serverAuctions.length ? 'complete' : 'not_observed'
  )
  expect(stored.report.gpt_diagnostics.page).toEqual({
    origin: baseURL,
    pathname: '/[redacted]',
  })
  expect(stored.report.gpt_diagnostics.slots[0].requests[0].isEmpty).toBe(true)
  expect(
    stored.report.gpt_diagnostics.slots[0].requests[0].trustedServerAuctionId
  ).toBeUndefined()

  await page.context().grantPermissions(['clipboard-read', 'clipboard-write'], {
    origin: baseURL,
  })
  await page.getByRole('button', { name: 'Copy', exact: true }).focus()
  await page.keyboard.press('Enter')
  await expect(page.locator('#trace-export-status')).toHaveText('Copied JSON.')
  const copied = await page.evaluate(() => navigator.clipboard.readText())
  const downloading = page.waitForEvent('download')
  await page.getByRole('button', { name: 'Download', exact: true }).click()
  const downloaded = await downloading
  expect(downloaded.suggestedFilename()).toBe('trusted-server-trace-v1.json')
  const destination = await downloaded.path()
  expect(destination).not.toBeNull()
  const bytes = readFileSync(destination!, 'utf8')
  expect(bytes).toBe(copied)
  expect(JSON.parse(bytes)).toEqual(stored.report)
  expect(bytes).not.toContain('stored_at_ms')
  expect(bytes).not.toContain('gpt-diagnostics-slot-primary')
  expect(bytes).not.toContain('/123456789/example-runtime')

  controls.length = 0
  const endedState = page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === '/_ts/trace/state' &&
      response.request().method() === 'GET'
  )
  page.once('dialog', (dialog) => dialog.accept())
  await page
    .getByRole('button', { name: 'Clear report and end tracing', exact: true })
    .click()
  expect(await (await endedState).json()).toEqual({ observed_active: false })
  await expect(article).toHaveCount(0)
  await expect(page.locator('#trace-cleanup-local-status')).toHaveText(
    'Local report deleted from this tab.'
  )
  await expect(page.locator('#trace-cleanup-server-status')).toHaveText(
    'Tracing is off — no valid diagnostics session observed.'
  )
  expect(controls).toEqual(['POST /_ts/trace/end', 'GET /_ts/trace/state'])
  expect(
    await page.evaluate((key) => sessionStorage.getItem(key), REPORT_KEY)
  ).toBeNull()
  expect(
    await page.evaluate(() => sessionStorage.getItem('unrelated-fixture-key'))
  ).toBe('retained')
  expect(
    (await page.context().cookies()).find((item) => item.name === SESSION)
  ).toBeUndefined()
  await page.goto(publisher)
  expect(
    await page.evaluate(() => Reflect.get(window, '__tsjs_trace_active'))
  ).toBeUndefined()
})
