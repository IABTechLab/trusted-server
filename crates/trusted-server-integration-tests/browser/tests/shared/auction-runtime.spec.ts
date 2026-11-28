import { test, expect, type Page } from '@playwright/test'
import { createHash } from 'node:crypto'
import { readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { guardAuctionNetwork } from '../../helpers/auction-network'

test.skip(
  !process.env.AUCTIONEER_EVIDENCE_MANIFEST,
  'Run the isolated production artifact coordinator'
)
const manifest = process.env.AUCTIONEER_EVIDENCE_MANIFEST
  ? JSON.parse(readFileSync(process.env.AUCTIONEER_EVIDENCE_MANIFEST, 'utf8'))
  : {}
const helper = readFileSync(
  resolve(__dirname, '../../helpers/trace-gpt-fixture.js'),
  'utf8'
)
const evidence: unknown[] = []

async function visibleCreative(page: Page) {
  for (const frame of page.frames()) {
    if (frame.isDetached()) continue
    const marker = frame.locator('.production-creative')
    if (!(await marker.count()) || !(await marker.isVisible())) continue
    let embedded = frame
    let visible = true
    while (embedded.parentFrame()) {
      const element = await embedded.frameElement()
      const box = await element.boundingBox()
      if (
        !(await element.isVisible()) ||
        !box ||
        box.width <= 0 ||
        box.height <= 0
      ) {
        visible = false
        break
      }
      embedded = embedded.parentFrame()!
    }
    if (!visible) continue
    const asset = frame.locator('.production-asset')
    if (!(await asset.count())) continue
    const image = await asset.evaluate((element: HTMLImageElement) => ({
      decoded: element.complete && element.naturalWidth > 0,
      url: element.currentSrc,
    }))
    if (image.decoded)
      return {
        text: await marker.textContent(),
        asset: image.url,
        frame: frame.url(),
      }
  }
  return null
}

test.beforeEach(() => {
  writeFileSync('/work/bid-mode.json', JSON.stringify({ empty: false }))
})

test.beforeAll(async ({ browser }) => {
  evidence.push({
    chromiumVersion: browser.version(),
    vendorFixture: 'GPT callbacks',
    auctionResponsesMocked: false,
  })
})
test.afterAll(() => {
  if (process.env.AUCTIONEER_EVIDENCE_MANIFEST)
    writeFileSync(
      '/results/browser-observations.json',
      JSON.stringify(evidence, null, 2)
    )
})

for (const mode of ['direct', 'publisher', 'empty'] as const) {
  test(`production PBS ${mode} delivers real targeting and creative`, async ({
    page,
    context,
  }) => {
    const guard = await guardAuctionNetwork(context, manifest.allowedOrigins)
    const [name, value] = manifest.consentCookie.split('=')
    await context.addCookies([{ name, value, domain: 'localhost', path: '/' }])
    const responses: unknown[] = []
    const pending: Promise<void>[] = []
    context.on('response', (response) => {
      if (
        response.url().includes('/auction') ||
        response.url().includes('/browser-') ||
        response.url().includes('/static/') ||
        response.url().includes('/integrations/prebid/bundle.js')
      )
        pending.push(
          response.body().then((body) => {
            responses.push({
              url: response.url(),
              status: response.status(),
              sha256: createHash('sha256').update(body).digest('hex'),
            })
          })
        )
    })
    // This is the sole vendor API fixture. No auction or injected TSJS bytes are replaced.
    await page.addInitScript({
      content: `window.__traceGptFixtureConfig = ${JSON.stringify({ creativeOrigin: manifest.creativeOrigin })};\n${helper}`,
    })
    const document = await page.goto(`/browser-${mode}`)
    expect(document?.status()).toBe(200)
    await page.waitForFunction(
      () => typeof Reflect.get(window, 'pbjs')?.requestBids === 'function'
    )
    if (mode !== 'publisher') {
      await page.evaluate((empty) => {
        const gpt = Reflect.get(window, 'googletag')
        const slot = gpt
          .defineSlot(
            '/123456789/example-slot',
            [300, 250],
            empty ? 'empty-slot' : 'example-slot'
          )
          .addService(gpt.pubads())
        gpt.pubads().disableInitialLoad()
        Reflect.set(
          window,
          'fixtureRunAuction',
          () =>
            new Promise<void>((done) => {
              const pbjs = Reflect.get(window, 'pbjs')
              pbjs.requestBids({
                adUnits: [
                  {
                    code: empty ? 'empty-slot' : 'example-slot',
                    mediaTypes: { banner: { sizes: [[300, 250]] } },
                    bids: [
                      {
                        bidder: 'example-bidder',
                        params: { placement: 'fixture' },
                      },
                    ],
                  },
                ],
                timeout: 10_000,
                bidsBackHandler() {
                  pbjs.setTargetingForGPTAsync(
                    empty ? ['empty-slot'] : ['example-slot']
                  )
                  gpt.pubads().refresh([slot])
                  done()
                },
              })
            })
        )
      }, mode === 'empty')
      await page.evaluate(() => Reflect.get(window, 'fixtureRunAuction')())
    }
    await page.waitForFunction(
      () => Reflect.get(window, '__traceGptFixture').requests.length > 0
    )
    const snapshot = await page.evaluate(() => {
      const pbjs = Reflect.get(window, 'pbjs')
      const fixture = Reflect.get(window, '__traceGptFixture')
      const api = Reflect.get(window, 'tsjs')
      return {
        publicTargeting: pbjs.getAdserverTargeting(),
        unitTargeting: pbjs.getAdserverTargetingForAdUnitCode('example-slot'),
        targetingString:
          pbjs.getAdserverTargetingForAdUnitCodeStr('example-slot'),
        targeting: fixture.requests[0].targeting,
        injectedSlots: api?.adSlots,
        injectedBids: api?.bids,
        version: pbjs.version,
      }
    })
    if (mode === 'empty') {
      expect(snapshot.targeting.hb_adid).toBeUndefined()
      expect(snapshot.publicTargeting['empty-slot']?.hb_adid).toBeUndefined()
    } else {
      expect(snapshot.targeting.hb_adid?.[0]).toBeTruthy()
      expect(snapshot.targeting.hb_pb?.[0]).toBeTruthy()
      if (mode === 'direct') {
        expect(snapshot.publicTargeting['example-slot'].hb_adid).toBe(
          snapshot.targeting.hb_adid[0]
        )
        expect(snapshot.publicTargeting['example-slot'].hb_pb).toBe(
          snapshot.targeting.hb_pb[0]
        )
        expect(snapshot.unitTargeting.hb_adid).toBe(
          snapshot.targeting.hb_adid[0]
        )
        expect(
          new URLSearchParams(snapshot.targetingString).get('hb_adid')
        ).toBe(snapshot.targeting.hb_adid[0])
        expect(snapshot.injectedBids?.['example-slot']).toBeUndefined()
      } else {
        expect(snapshot.injectedBids['example-slot'].hb_adid).toBe(
          snapshot.targeting.hb_adid[0]
        )
        expect(snapshot.injectedSlots.length).toBe(1)
      }
    }
    await page.evaluate(() =>
      Reflect.get(window, '__traceGptFixture').complete(0)
    )
    if (mode !== 'empty') {
      await expect
        .poll(async () => (await visibleCreative(page))?.text)
        .toBe('Production fixture 1.23')
      evidence.push({ mode, firstCreative: await visibleCreative(page) })
    }
    if (mode === 'direct') {
      const first = await visibleCreative(page)
      // Drive the independent GPT refresh wrapper after the first delivery ends.
      await page.evaluate(() => {
        const gpt = Reflect.get(window, 'googletag')
        gpt.pubads().refresh(gpt.pubads().getSlots())
      })
      await page.waitForFunction(
        () => Reflect.get(window, '__traceGptFixture').requests.length === 2
      )
      const refresh = await page.evaluate(() => ({
        gpt: Reflect.get(window, '__traceGptFixture').requests[1].targeting,
        public: Reflect.get(window, 'pbjs').getAdserverTargetingForAdUnitCode(
          'example-slot'
        ),
      }))
      expect(refresh.gpt.hb_adid?.[0]).toBeTruthy()
      expect(refresh.gpt.hb_adid[0]).not.toBe(snapshot.targeting.hb_adid[0])
      expect(refresh.public.hb_adid).toBe(refresh.gpt.hb_adid[0])
      await page.evaluate(() =>
        Reflect.get(window, '__traceGptFixture').complete(1)
      )
      await expect
        .poll(async () => {
          const creative = await visibleCreative(page)
          return Boolean(
            creative &&
            creative.asset !== first?.asset &&
            creative.text === 'Production fixture 1.23'
          )
        })
        .toBe(true)
      const renderedRefresh = await visibleCreative(page)
      writeFileSync('/work/bid-mode.json', JSON.stringify({ empty: true }))
      await page.evaluate(() => {
        const gpt = Reflect.get(window, 'googletag')
        gpt.pubads().refresh(gpt.pubads().getSlots())
      })
      await page.waitForFunction(
        () => Reflect.get(window, '__traceGptFixture').requests.length === 3
      )
      const emptyRefresh = await page.evaluate(() => {
        const pbjs = Reflect.get(window, 'pbjs')
        return {
          gpt: Reflect.get(window, '__traceGptFixture').requests[2].targeting,
          aggregate: pbjs.getAdserverTargeting(),
          unit: pbjs.getAdserverTargetingForAdUnitCode('example-slot'),
          text: pbjs.getAdserverTargetingForAdUnitCodeStr('example-slot'),
        }
      })
      expect(emptyRefresh.gpt.hb_adid).toBeUndefined()
      expect(emptyRefresh.gpt.hb_pb).toBeUndefined()
      expect(emptyRefresh.aggregate['example-slot']?.hb_adid).toBeUndefined()
      expect(emptyRefresh.unit.hb_adid).toBeUndefined()
      expect(emptyRefresh.unit.hb_pb).toBeUndefined()
      expect(emptyRefresh.aggregate['example-slot']?.hb_pb).toBeUndefined()
      expect(new URLSearchParams(emptyRefresh.text).has('hb_adid')).toBe(false)
      expect(new URLSearchParams(emptyRefresh.text).has('hb_pb')).toBe(false)
      await page.evaluate(() =>
        Reflect.get(window, '__traceGptFixture').complete(2)
      )
      // The labelled GPT fixture leaves the last frame in place on an empty response.
      // No stale ad ID is delivered and no replacement PUC/creative is created.
      expect(await visibleCreative(page)).toEqual(renderedRefresh)
      evidence.push({ refresh, renderedRefresh, emptyRefresh })
    }
    await Promise.all(pending)
    guard.assertClean()
    evidence.push({ mode, snapshot, responses, violations: guard.violations })
  })
}

for (const [name, port] of [
  ['wrong-san', 18444],
  ['untrusted-ca', 18445],
] as const) {
  test(`browser rejects ${name} without certificate bypass`, async ({
    page,
    context,
  }) => {
    const guard = await guardAuctionNetwork(context, manifest.allowedOrigins)
    await expect(
      page.goto(`https://creative.example.com:${port}/health`)
    ).rejects.toThrow(/ERR_CERT/)
    guard.assertClean()
    evidence.push({
      browserTlsRejected: name,
      port,
      violations: guard.violations,
    })
  })
}

for (const mode of ['direct', 'redirect', 'websocket'] as const) {
  test(`browser guard detects off-list ${mode}`, async ({ page, context }) => {
    const guard = await guardAuctionNetwork(context, manifest.allowedOrigins)
    await page.goto('http://localhost:18081/health')
    await page.evaluate((kind) => {
      if (kind === 'websocket') {
        new WebSocket('wss://off-list.example.com/socket')
      } else {
        const image = new Image()
        image.src =
          kind === 'redirect'
            ? 'https://creative.example.com:18443/redirect-off-list'
            : 'https://off-list.example.com/pixel'
        document.body.append(image)
      }
    }, mode)
    await expect.poll(() => guard.violations.length).toBeGreaterThan(0)
    expect(
      guard.violations.some(
        (violation) =>
          violation.authority ===
          `${mode === 'websocket' ? 'wss' : 'https'}://off-list.example.com`
      )
    ).toBe(true)
    expect(() => guard.assertClean()).toThrow(/Off-list fixture requests/)
    evidence.push({ negativeEgressCase: mode, violations: guard.violations })
  })
}
