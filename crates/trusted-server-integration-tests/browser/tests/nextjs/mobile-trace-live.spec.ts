import { expect, test, type Page } from '@playwright/test'
import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { readState } from '../../helpers/state.js'
import {
  clickTraceHandoff,
  traceRuntimeUrl,
} from '../../helpers/trace-fixture.js'

function bidderEndpoint(): string {
  return `http://127.0.0.1:${process.env.INTEGRATION_ORIGIN_PORT || '8888'}/api/trace-bidder`
}

const pucBanner = readFileSync(
  resolve(
    __dirname,
    '../../node_modules/prebid-universal-creative/dist/banner.js'
  ),
  'utf8'
)

async function activatePublisher(page: Page, path: string): Promise<string> {
  await page.addInitScript({
    path: resolve(__dirname, '../../helpers/trace-gpt-fixture.js'),
  })
  const publisher = traceRuntimeUrl(path)
  await page.goto(publisher)
  expect(
    await page.evaluate(() => Reflect.get(window, '__tsjs_trace_active'))
  ).toBeUndefined()
  await page.goto(traceRuntimeUrl('/_ts/trace'))
  await page
    .getByRole('button', { name: 'Enable tracing', exact: true })
    .click()
  await expect(page.locator('#trace-session-state')).toContainText(
    'Tracing is on'
  )
  await page
    .getByRole('button', { name: 'Return to previous page', exact: true })
    .click()
  await page.waitForURL(publisher)
  const response = await page.reload()
  expect(response?.headers()['cache-control']).toContain('no-store')
  await page.waitForFunction(() => {
    const fixture = Reflect.get(window, '__traceGptFixture')
    return (
      fixture?.requests.length > 0 &&
      Boolean(Reflect.get(window, 'tsjs')?.gptDiagnostics)
    )
  })
  return publisher
}

test.beforeEach(async ({ request }, testInfo) => {
  if (readState().framework !== 'nextjs') testInfo.skip()
  expect(
    (await request.put(bidderEndpoint(), { data: { mode: 'empty' } })).status()
  ).toBe(200)
})

test('captures a real zero-bid SSAT auction and carries its GPT cycle through the same-tab viewer', async ({
  page,
  request,
}) => {
  const observedRequests = (await (await request.get(bidderEndpoint())).json())
    .requests
  const publisher = await activatePublisher(
    page,
    '/gpt-diagnostics?trace_fixture=empty'
  )
  await page.evaluate(() =>
    Reflect.get(window, '__traceGptFixture').complete(0)
  )
  await expect
    .poll(
      async () => (await (await request.get(bidderEndpoint())).json()).requests
    )
    .toBeGreaterThanOrEqual(observedRequests + 2)
  const evidence = await page.evaluate(() =>
    Reflect.get(window, 'tsjs').traceEvidence.snapshot()
  )
  expect(evidence.ok).toBe(true)
  expect(evidence.value.serverAuctions).toHaveLength(1)
  expect(evidence.value.serverAuctions[0]).toMatchObject({
    source: 'initial_navigation_ssat',
    terminal_status: 'completed',
    provider_calls: [
      {
        provider_number: 1,
        role: 'bidder',
        status: 'no_bid',
        returned_bid_count: 0,
      },
    ],
    slots: [
      { slot_number: 1, candidate: 'no_candidate', returned_bid_count: 0 },
    ],
  })
  expect(evidence.value.slotCorrelations).toHaveLength(1)
  expect(evidence.value.slotCorrelations[0].diagnostic_auction_id).toBe(
    evidence.value.serverAuctions[0].diagnostic_auction_id
  )
  expect(evidence.value.slotCorrelations[0].slot_ref).toBe(
    evidence.value.serverAuctions[0].slots[0].slot_ref
  )
  await page.route('https://other.example/**', (route) => route.abort())
  await page.evaluate(() => {
    const base = document.createElement('base')
    base.href = 'https://other.example/'
    document.head.prepend(base)
  })
  expect(await page.evaluate(() => document.baseURI)).toBe('https://other.example/')
  await clickTraceHandoff(page)
  await page.waitForURL(traceRuntimeUrl('/_ts/trace'))
  await expect(
    page.getByRole('heading', {
      name: 'Trusted Server trace results',
      exact: true,
    })
  ).toBeVisible()
  await expect(
    page.getByText('Browser-carried, unverified diagnostic data', {
      exact: true,
    })
  ).toBeVisible()
  const report = await page.evaluate(
    () =>
      JSON.parse(sessionStorage.getItem('trusted-server.trace.report.v1')!)
        .report
  )
  expect(report.server_auctions).toEqual(evidence.value.serverAuctions)
  expect(report.slot_correlations).toEqual(evidence.value.slotCorrelations)
  expect(report.gpt_diagnostics.page.origin).toBe(new URL(publisher).origin)
  expect(report.gpt_diagnostics.page.pathname).toBe('/[redacted]')
  expect(report.gpt_diagnostics.slots[0].requests[0].isEmpty).toBe(true)
  await expect(
    page.getByText('Auction 1: Initial-page server auction (SSAT)', {
      exact: true,
    })
  ).toBeVisible()
  await expect(
    page.getByText('Unavailable in v1', {
      exact: true,
    })
  ).toBeVisible()
})

test('retains the real failed-provider SSAT observation without claiming a delivered creative', async ({
  page,
  request,
}) => {
  expect(
    (await request.put(bidderEndpoint(), { data: { mode: 'error' } })).status()
  ).toBe(200)
  const before = (await (await request.get(bidderEndpoint())).json()).requests
  await activatePublisher(page, '/gpt-diagnostics')
  await page.evaluate(() =>
    Reflect.get(window, '__traceGptFixture').complete(0)
  )
  await expect
    .poll(
      async () => (await (await request.get(bidderEndpoint())).json()).requests
    )
    .toBeGreaterThanOrEqual(before + 2)
  const evidence = await page.evaluate(() =>
    Reflect.get(window, 'tsjs').traceEvidence.snapshot()
  )
  expect(evidence.ok).toBe(true)
  expect(evidence.value.serverAuctions).toHaveLength(1)
  expect(evidence.value.serverAuctions[0]).toMatchObject({
    source: 'initial_navigation_ssat',
    terminal_status: 'completed',
    provider_calls: [{ status: 'error', returned_bid_count: 0 }],
    slots: [{ candidate: 'no_candidate', returned_bid_count: 0 }],
  })
  await expect(page.locator('iframe')).toHaveCount(0)
  await clickTraceHandoff(page)
  await page.waitForURL(traceRuntimeUrl('/_ts/trace'))
  await expect(page.locator('#trace-report')).toBeVisible()
  await expect(
    page.getByText('Trusted Server creative rendered', { exact: true })
  ).toHaveCount(0)
  const report = await page.evaluate(
    () =>
      JSON.parse(sessionStorage.getItem('trusted-server.trace.report.v1')!)
        .report
  )
  expect(report.server_auctions).toEqual(evidence.value.serverAuctions)
  expect(JSON.stringify(report)).not.toContain('controlled fixture failure')
})

test('proves a selected SSAT creative through the real PUC message bridge before reporting participation', async ({
  page,
  request,
}) => {
  expect(
    (
      await request.put(bidderEndpoint(), { data: { mode: 'selected' } })
    ).status()
  ).toBe(200)
  const before = (await (await request.get(bidderEndpoint())).json()).requests
  const publisherOrigin = new URL(traceRuntimeUrl('/')).origin
  await page.route(
    'https://creative.example.com/fixture-puc*',
    async (route) => {
      const url = new URL(route.request().url())
      if (url.pathname === '/fixture-puc.js') {
        await route.fulfill({ contentType: 'text/javascript', body: pucBanner })
        return
      }
      await route.fulfill({
        contentType: 'text/html',
        body: `<!doctype html><script src="/fixture-puc.js"></script><script>
window.ucTag.renderAd(document, {
  adId: ${JSON.stringify(url.searchParams.get('adId'))},
  pubUrl: ${JSON.stringify(publisherOrigin)}
});
</script>`,
      })
    }
  )
  await activatePublisher(page, '/gpt-diagnostics')
  await page.evaluate(() =>
    Reflect.get(window, '__traceGptFixture').complete(0)
  )
  await expect
    .poll(async () => {
      const texts = await Promise.all(
        page.frames().map((frame) => frame.locator('.marker').allTextContents())
      )
      return texts.flat()
    })
    .toContain('Trace fixture creative')
  await expect
    .poll(
      async () => (await (await request.get(bidderEndpoint())).json()).requests
    )
    .toBeGreaterThanOrEqual(before + 2)
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          Reflect.get(window, 'tsjs').gptDiagnostics.snapshot().slots[0]
            ?.requests[0]?.delivery
      )
    )
    .toBe('trusted_server_response_sent')
  const evidence = await page.evaluate(() =>
    Reflect.get(window, 'tsjs').traceEvidence.snapshot()
  )
  expect(evidence.ok).toBe(true)
  expect(evidence.value.serverAuctions).toHaveLength(1)
  expect(evidence.value.serverAuctions[0]).toMatchObject({
    source: 'initial_navigation_ssat',
    terminal_status: 'completed',
    slots: [{ candidate: 'selected', selected_creative_size: [300, 250] }],
  })
  expect(evidence.value.slotCorrelations).toHaveLength(1)
  expect(evidence.value.slotCorrelations[0]).toMatchObject({
    diagnostic_auction_id:
      evidence.value.serverAuctions[0].diagnostic_auction_id,
    slot_ref: evidence.value.serverAuctions[0].slots[0].slot_ref,
    request_number: 1,
  })
  await clickTraceHandoff(page)
  await page.waitForURL(traceRuntimeUrl('/_ts/trace'))
  await expect(
    page
      .locator('section')
      .filter({
        has: page.getByRole('heading', {
          name: 'Server auctions',
          exact: true,
        }),
      })
      .getByText('Trusted Server creative rendered', { exact: true })
  ).toBeVisible()
  const report = await page.evaluate(
    () =>
      JSON.parse(sessionStorage.getItem('trusted-server.trace.report.v1')!)
        .report
  )
  expect(report.server_auctions).toEqual(evidence.value.serverAuctions)
  expect(report.slot_correlations).toEqual(evidence.value.slotCorrelations)
  expect(report.gpt_diagnostics.slots[0].requests[0]).toMatchObject({
    isEmpty: false,
    delivery: 'trusted_server_response_sent',
    trustedServerCreativeResponseAtMs: expect.any(Number),
    renderAtMs: expect.any(Number),
  })
  expect(JSON.stringify(report)).not.toContain('Trace fixture creative')
  expect(JSON.stringify(report)).not.toContain('example-creative')
})

for (const mode of ['selected', 'empty'] as const) {
  test(`captures the core /auction caller's real ${mode} response independently of GPT`, async ({
    page,
    request,
  }) => {
    await activatePublisher(page, '/gpt-diagnostics')
    expect(
      (await request.put(bidderEndpoint(), { data: { mode } })).status()
    ).toBe(200)
    const before = (await (await request.get(bidderEndpoint())).json()).requests
    const responsePromise = page.waitForResponse(
      (response) =>
        new URL(response.url()).pathname === '/auction' &&
        response.request().method() === 'POST'
    )
    await page.evaluate(() => {
      const container = document.createElement('div')
      container.id = 'example-core-api-slot'
      document.body.append(container)
      const api = Reflect.get(window, 'tsjs')
      api.addAdUnits({
        code: container.id,
        mediaTypes: { banner: { sizes: [[300, 250]] } },
        bids: [{ bidder: 'example', params: {} }],
      })
      api.requestAds()
    })
    const response = await responsePromise
    expect(response.status()).toBe(200)
    expect(response.headers()['cache-control']).toContain('no-store')
    const outgoing = response.request().postDataJSON()
    const slotRef = outgoing.adUnits[0].ext.trusted_server.trace_slot_ref
    expect(slotRef).toMatch(
      /^ts-slot-[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/
    )
    const body = await response.json()
    const evidence = body.ext?.trusted_server?.trace_auction?.evidence
    expect(evidence).toMatchObject({
      source: 'auction_api',
      terminal_status: 'completed',
      provider_calls: [
        {
          provider_number: 1,
          role: 'bidder',
          status: mode === 'selected' ? 'success' : 'no_bid',
          returned_bid_count: mode === 'selected' ? 1 : 0,
        },
      ],
      slots: [
        {
          slot_number: 1,
          slot_ref: slotRef,
          candidate: mode === 'selected' ? 'selected' : 'no_candidate',
          returned_bid_count: mode === 'selected' ? 1 : 0,
        },
      ],
    })
    await expect
      .poll(
        async () =>
          (await (await request.get(bidderEndpoint())).json()).requests
      )
      .toBe(before + 1)
    await expect
      .poll(() =>
        page.evaluate(() =>
          Reflect.get(window, 'tsjs')
            .traceEvidence.snapshot()
            .value.serverAuctions.filter(
              (auction: { source: string }) => auction.source === 'auction_api'
            )
        )
      )
      .toEqual([evidence])
    const sidecars = await page.evaluate(
      () =>
        Reflect.get(window, 'tsjs').traceEvidence.snapshot().value
          .slotCorrelations
    )
    expect(
      sidecars.filter(
        (sidecar: { diagnostic_auction_id: string }) =>
          sidecar.diagnostic_auction_id === evidence.diagnostic_auction_id
      )
    ).toEqual([])
    const publicEvidence = JSON.stringify(evidence)
    for (const forbidden of [
      'example-core-api-slot',
      'example-bidder',
      'example-creative',
      'Trace fixture creative',
      'bidder.example.com',
    ])
      expect(publicEvidence).not.toContain(forbidden)
    if (mode === 'selected') {
      await expect(
        page.frameLocator('#example-core-api-slot iframe').locator('.marker')
      ).toHaveText('Trace fixture creative')
    } else {
      await expect(page.locator('#example-core-api-slot iframe')).toHaveCount(0)
    }
  })
}

for (const transport of ['absent', 'malformed'] as const) {
  test(`preserves the real core API creative when optional trace evidence is ${transport}`, async ({
    page,
    request,
  }) => {
    await activatePublisher(page, '/gpt-diagnostics')
    expect(
      (
        await request.put(bidderEndpoint(), { data: { mode: 'selected' } })
      ).status()
    ).toBe(200)
    const before = (await (await request.get(bidderEndpoint())).json()).requests
    await page.route('**/auction', async (route) => {
      const response = await route.fetch()
      const body = await response.json()
      // Preserve the actual bidder response and ads. Alter only the optional
      // diagnostic member to exercise missing and invalid transport behavior.
      expect(body.ext?.trusted_server?.trace_auction?.evidence.source).toBe(
        'auction_api'
      )
      if (transport === 'absent') delete body.ext.trusted_server.trace_auction
      else body.ext.trusted_server.trace_auction = { schema_version: 999 }
      await route.fulfill({ response, json: body })
    })
    await page.evaluate(() => {
      const container = document.createElement('div')
      container.id = 'example-core-api-slot'
      document.body.append(container)
      const api = Reflect.get(window, 'tsjs')
      api.addAdUnits({
        code: container.id,
        mediaTypes: { banner: { sizes: [[300, 250]] } },
        bids: [{ bidder: 'example', params: {} }],
      })
      api.requestAds()
    })
    await expect(
      page.frameLocator('#example-core-api-slot iframe').locator('.marker')
    ).toHaveText('Trace fixture creative')
    await expect
      .poll(
        async () =>
          (await (await request.get(bidderEndpoint())).json()).requests
      )
      .toBe(before + 1)
    const captured = await page.evaluate(() =>
      Reflect.get(window, 'tsjs').traceEvidence.snapshot()
    )
    expect(captured.ok).toBe(true)
    expect(
      captured.value.serverAuctions.filter(
        (auction: { source: string }) => auction.source === 'auction_api'
      )
    ).toEqual([])
    expect(captured.value.issues).toEqual(
      transport === 'malformed' ? ['evidence_validation_failed'] : []
    )
  })
}

test('captures the pinned real Prebid adapter /auction response without a GPT join', async ({
  page,
  request,
}) => {
  await activatePublisher(page, '/gpt-diagnostics')
  const dist = resolve(__dirname, '../../../../trusted-server-js/dist')
  const manifest = JSON.parse(
    readFileSync(resolve(dist, 'prebid/manifest.json'), 'utf8')
  )
  const external = readFileSync(resolve(dist, 'prebid', manifest.filename))
  expect(createHash('sha256').update(external).digest('hex')).toBe(
    manifest.sha256
  )
  await page.addScriptTag({ content: external.toString('utf8') })
  await page.addScriptTag({ path: resolve(dist, 'tsjs-prebid.js') })
  expect(
    await page.evaluate(() => typeof Reflect.get(window, 'pbjs').onEvent)
  ).toBe('function')
  const before = (await (await request.get(bidderEndpoint())).json()).requests
  const responsePromise = page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === '/auction' &&
      response.request().method() === 'POST'
  )
  await page.evaluate(
    () =>
      new Promise<void>((done) => {
        Reflect.get(window, 'pbjs').requestBids({
          adUnits: [
            {
              code: 'example-prebid-api-slot',
              mediaTypes: { banner: { sizes: [[300, 250]] } },
              bids: [],
            },
          ],
          timeout: 3000,
          bidsBackHandler: done,
        })
      })
  )
  const response = await responsePromise
  expect(response.status()).toBe(200)
  expect(response.headers()['cache-control']).toContain('no-store')
  const outgoing = response.request().postDataJSON()
  const slotRef = outgoing.adUnits[0].ext.trusted_server.trace_slot_ref
  expect(slotRef).toMatch(
    /^ts-slot-[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/
  )
  const body = await response.json()
  const auction = body.ext?.trusted_server?.trace_auction?.evidence
  expect(auction).toMatchObject({
    source: 'auction_api',
    terminal_status: 'completed',
    provider_calls: [{ status: 'no_bid', returned_bid_count: 0 }],
    slots: [
      { slot_ref: slotRef, candidate: 'no_candidate', returned_bid_count: 0 },
    ],
  })
  await expect
    .poll(
      async () => (await (await request.get(bidderEndpoint())).json()).requests
    )
    .toBe(before + 1)
  await expect
    .poll(() =>
      page.evaluate(() =>
        Reflect.get(window, 'tsjs')
          .traceEvidence.snapshot()
          .value.serverAuctions.filter(
            (record: { source: string }) => record.source === 'auction_api'
          )
      )
    )
    .toEqual([auction])
  expect(
    await page.evaluate(
      (id) =>
        Reflect.get(window, 'tsjs')
          .traceEvidence.snapshot()
          .value.slotCorrelations.filter(
            (sidecar: { diagnostic_auction_id: string }) =>
              sidecar.diagnostic_auction_id === id
          ),
      auction.diagnostic_auction_id
    )
  ).toEqual([])
})

for (const legacy of [false, true]) {
  test(`carries real ${legacy ? 'legacy fallback' : 'canonical'} SPA page-bids into its exact GPT cycle`, async ({
    page,
    request,
  }) => {
    await activatePublisher(page, '/gpt-diagnostics')
    await page.evaluate(() =>
      Reflect.get(window, '__traceGptFixture').complete(0)
    )
    const before = (await (await request.get(bidderEndpoint())).json()).requests
    if (legacy) {
      // Simulate an unavailable canonical route only. The legacy response and
      // every auction/evidence payload still come from the real Rust server.
      await page.route('**/_ts/page-bids?*', (route) =>
        route.fulfill({ status: 404, body: '' })
      )
    }
    const endpoint = legacy ? '/__ts/page-bids' : '/_ts/page-bids'
    const homeResponse = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return url.pathname === endpoint && url.searchParams.get('path') === '/'
    })
    await page
      .getByRole('navigation', { name: 'Fixture navigation', exact: true })
      .getByRole('link', { name: 'Home', exact: true })
      .click()
    await page.waitForURL(traceRuntimeUrl('/'))
    const home = await homeResponse
    expect(home.status()).toBe(200)
    const homeAuction = (await home.json()).trace_auction?.evidence
    expect(homeAuction).toMatchObject({
      source: 'spa_page_bids',
      terminal_status: 'skipped',
      terminal_reason: 'no_eligible_slots',
      slots: [],
    })
    await expect
      .poll(() =>
        page.evaluate(
          (id) =>
            Reflect.get(window, 'tsjs')
              .traceEvidence.snapshot()
              .value.serverAuctions.some(
                (auction: { diagnostic_auction_id: string }) =>
                  auction.diagnostic_auction_id === id
              ),
          homeAuction.diagnostic_auction_id
        )
      )
      .toBe(true)
    const pageBidsResponse = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return (
        url.pathname === endpoint &&
        url.searchParams.get('path') === '/gpt-diagnostics'
      )
    })
    await page.goBack()
    await page.waitForURL(traceRuntimeUrl('/gpt-diagnostics'))
    const response = await pageBidsResponse
    expect(response.status()).toBe(200)
    expect(response.headers()['cache-control']).toContain('no-store')
    expect(response.request().headers()['x-tsjs-page-bids']).toBe(
      legacy ? 'fallback' : '1'
    )
    const body = await response.json()
    const auction = body.trace_auction?.evidence
    expect(auction).toMatchObject({
      source: 'spa_page_bids',
      terminal_status: 'completed',
      provider_calls: [{ status: 'no_bid', returned_bid_count: 0 }],
      slots: [{ candidate: 'no_candidate', returned_bid_count: 0 }],
    })
    expect(body.slots[0].ext.trusted_server.trace_slot_ref).toBe(
      auction.slots[0].slot_ref
    )
    await expect
      .poll(
        async () =>
          (await (await request.get(bidderEndpoint())).json()).requests
      )
      .toBe(before + 1)
    await page.waitForFunction(
      () => Reflect.get(window, '__traceGptFixture').requests.length >= 2
    )
    await page.evaluate(() => {
      const fixture = Reflect.get(window, '__traceGptFixture')
      fixture.complete(fixture.requests.length - 1)
    })
    await expect
      .poll(() =>
        page.evaluate(
          (id) =>
            Reflect.get(window, 'tsjs')
              .traceEvidence.snapshot()
              .value.slotCorrelations.filter(
                (sidecar: { diagnostic_auction_id: string }) =>
                  sidecar.diagnostic_auction_id === id
              ),
          auction.diagnostic_auction_id
        )
      )
      .toEqual([
        {
          schema_version: 1,
          diagnostic_auction_id: auction.diagnostic_auction_id,
          slot_ref: auction.slots[0].slot_ref,
          runtime_slot_number: expect.any(Number),
          request_number: expect.any(Number),
        },
      ])
    await clickTraceHandoff(page)
    await page.waitForURL(traceRuntimeUrl('/_ts/trace'))
    const report = await page.evaluate(
      () =>
        JSON.parse(sessionStorage.getItem('trusted-server.trace.report.v1')!)
          .report
    )
    expect(report.server_auctions).toContainEqual(auction)
    expect(
      report.server_auctions.some(
        (record: { source: string }) =>
          record.source === 'initial_navigation_ssat'
      )
    ).toBe(true)
    await expect(
      page
        .locator('section')
        .filter({
          has: page.getByRole('heading', {
            name: 'Server auctions',
            exact: true,
          }),
        })
        .locator(':scope > details > summary')
    ).toHaveText([
      'Auction 1: Initial-page server auction (SSAT)',
      'Auction 2: Trusted Server page-refresh auction',
      'Auction 3: Trusted Server page-refresh auction',
    ])
  })
}

test('controlled bidder serves real OpenRTB selected, empty and error responses', async ({
  request,
}) => {
  const endpoint = bidderEndpoint()
  const before = await request.get(endpoint)
  expect(before.status()).toBe(200)
  const observedRequests = (await before.json()).requests
  const auction = (mode: string) => ({
    id: 'example-request',
    site: {
      page: `https://publisher.example.com/gpt-diagnostics?trace_fixture=${mode}`,
    },
    imp: [{ id: 'example-slot', banner: { format: [{ w: 300, h: 250 }] } }],
  })
  const selected = await request.post(endpoint, { data: auction('selected') })
  expect(selected.status()).toBe(200)
  expect(await selected.json()).toMatchObject({
    id: 'example-request',
    cur: 'USD',
    seatbid: [
      {
        seat: 'example-bidder',
        bid: [{ impid: 'example-slot', price: 1, w: 300, h: 250 }],
      },
    ],
  })
  const empty = await request.post(endpoint, { data: auction('empty') })
  expect(empty.status()).toBe(200)
  expect(await empty.json()).toEqual({
    id: 'example-request',
    cur: 'USD',
    seatbid: [],
  })
  const failed = await request.post(endpoint, { data: auction('error') })
  expect(failed.status()).toBe(503)
  expect(await failed.json()).toEqual({ error: 'controlled fixture failure' })
  const after = await request.get(endpoint)
  expect((await after.json()).requests).toBe(observedRequests + 3)
  const control = await request.put(endpoint, { data: { mode: 'selected' } })
  expect(control.status()).toBe(200)
  expect(await control.json()).toEqual({ mode: 'selected' })
  const controlled = await request.post(endpoint, {
    data: {
      ...auction('empty'),
      site: { page: 'https://publisher.example.com/gpt-diagnostics' },
    },
  })
  expect((await controlled.json()).seatbid).toHaveLength(1)
  const invalidControl = await request.put(endpoint, {
    data: { mode: 'unexpected' },
  })
  expect(invalidControl.status()).toBe(400)
  for (const invalidBody of ['null', '{']) {
    const invalid = await request.put(endpoint, {
      data: invalidBody,
      headers: { 'Content-Type': 'application/json' },
    })
    expect(invalid.status()).toBe(400)
    expect(await invalid.json()).toEqual({ error: 'invalid fixture mode' })
  }
  await request.put(endpoint, { data: { mode: 'empty' } })
})
