import { expect, test } from '@playwright/test'
import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { runtimeUrl, readState } from '../../helpers/state.js'
import {
  traceRuntimeUrl,
  TRACE_FIXTURE_CREDENTIALS,
} from '../../helpers/trace-fixture.js'
import { storedTraceReportFixture } from '../../helpers/trace-report-fixture.js'

const SESSION = '__Host-ts-console'
const CSP =
  "default-src 'none'; script-src 'self'; style-src 'self'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'; form-action 'none'; connect-src 'self'; img-src data:"

test.describe('mobile trace foundation', () => {
  test('serves a read-only hardened shell and HEAD without a session mutation', async ({
    page,
    request,
  }) => {
    const paths: string[] = []
    page.on('request', (request) => paths.push(new URL(request.url()).pathname))
    const response = await page.goto(traceRuntimeUrl('/_ts/trace'))
    expect(response?.status()).toBe(200)
    const headers = response?.headers() ?? {}
    expect(headers['content-security-policy']).toBe(CSP)
    expect(headers['cache-control']).toBe('no-store, private')
    expect(headers['x-content-type-options']).toBe('nosniff')
    expect(headers['referrer-policy']).toBe('no-referrer')
    expect(headers['permissions-policy']).toBe(
      'camera=(), microphone=(), geolocation=(), payment=(), usb=()'
    )
    expect(headers['set-cookie']).toBeUndefined()
    await expect(
      page.getByRole('heading', { name: 'Setup request', exact: true })
    ).toBeVisible()
    expect(await page.context().cookies()).toEqual([])
    expect(await page.locator('link[rel="icon"]').getAttribute('href')).toMatch(
      /^data:/
    )
    expect(
      await page.locator('script:not([src]), style, [style]').count()
    ).toBe(0)
    expect(paths.sort()).toEqual([
      '/_ts/trace',
      '/_ts/trace/assets/v1.css',
      '/_ts/trace/assets/v1.js',
    ])
    const head = await request.head(traceRuntimeUrl('/_ts/trace'))
    expect(head.status()).toBe(200)
    expect(await head.body()).toHaveLength(0)
    expect(head.headers()['set-cookie']).toBeUndefined()
    const manifest = JSON.parse(
      readFileSync(
        resolve(
          __dirname,
          '../../../../trusted-server-js/lib/trace-assets-manifest.json'
        ),
        'utf8'
      )
    ) as { assets: { path: string; sha256: string }[] }
    for (const asset of manifest.assets) {
      const delivered = await request.get(traceRuntimeUrl(asset.path))
      expect(delivered.status()).toBe(200)
      expect(
        createHash('sha256')
          .update(await delivered.body())
          .digest('hex')
      ).toBe(asset.sha256)
      expect(delivered.headers()['etag']).toBe(`"${asset.sha256}"`)
      expect(delivered.headers()['cache-control']).toBe(
        'public, max-age=31536000, immutable'
      )
      expect(delivered.headers()['set-cookie']).toBeUndefined()
      const assetHead = await request.head(traceRuntimeUrl(asset.path))
      expect(assetHead.status()).toBe(200)
      expect(await assetHead.body()).toHaveLength(0)
      expect(assetHead.headers()['etag']).toBe(delivered.headers()['etag'])
    }
  })

  test('accepts the unchanged browser cookie only after an explicit tap and confirms through a separate request', async ({
    page,
  }) => {
    const requests: string[] = []
    page.on('request', (request) => {
      if (new URL(request.url()).pathname.startsWith('/_ts/trace/'))
        requests.push(`${request.method()} ${new URL(request.url()).pathname}`)
    })
    await page.goto(traceRuntimeUrl('/_ts/trace'))
    const history = await page.evaluate(() => window.history.length)
    await page
      .getByRole('button', { name: 'Enable tracing', exact: true })
      .click()
    await expect(page.locator('#trace-session-state')).toHaveText(
      'Tracing is on — cookie observed by server'
    )
    expect(requests.filter((entry) => !entry.includes('/assets/'))).toEqual([
      'POST /_ts/trace/enable',
      'GET /_ts/trace/state',
    ])
    expect(await page.evaluate(() => window.history.length)).toBe(history)
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
    await expect(page.locator('#trace-status')).toContainText('reload once')
    await page.getByRole('button', { name: 'End tracing', exact: true }).click()
    await expect(page.locator('#trace-session-state')).toHaveText(
      'Tracing is off — no valid diagnostics session observed'
    )
    expect(
      (await page.context().cookies()).find((item) => item.name === SESSION)
    ).toBeUndefined()
  })

  test('GET control routes cannot activate or end a browser session', async ({
    request,
  }) => {
    for (const path of ['enable', 'end']) {
      const response = await request.get(traceRuntimeUrl(`/_ts/trace/${path}`))
      expect(response.status()).toBe(405)
      expect(response.headers()['set-cookie']).toBeUndefined()
      expect(response.headers()['cache-control']).toBe('no-store, private')
    }
    const state = await request.get(traceRuntimeUrl('/_ts/trace/state'))
    expect(await state.json()).toEqual({ observed_active: false })
  })

  test('returns through history and activates only a freshly reloaded eligible publisher document', async ({
    page,
  }) => {
    await page.goto(traceRuntimeUrl('/'))
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
    await page.waitForURL(traceRuntimeUrl('/'))
    await page.reload()
    await page.waitForFunction(
      () => Reflect.get(window, '__tsjs_trace_active') === true
    )
    expect(
      await page.evaluate(() => {
        const context = Reflect.get(window, '__tsjs_trace_request_context')
        return [
          context,
          context.network,
          context.cookies,
          ...Object.values(context.cookies),
        ].every(Object.isFrozen)
      })
    ).toBe(true)
    await page.goto(traceRuntimeUrl('/?ts_console=0'))
    expect(
      await page.evaluate(() => Reflect.get(window, '__tsjs_trace_active'))
    ).toBeUndefined()
    expect(
      (await page.context().cookies()).find((item) => item.name === SESSION)
    ).toBeUndefined()
  })

  test('protects shell and assets before feature or method handling when configured', async ({
    request,
  }) => {
    for (const path of [
      '/_ts/trace',
      '/_ts/trace/assets/v1.js',
      '/_ts/trace/not-a-route',
    ]) {
      const response = await request.get(traceRuntimeUrl(path, true))
      expect(response.status()).toBe(401)
      expect(response.headers()['www-authenticate']).toBeDefined()
      expect(response.headers()['cache-control']).toBe('no-store, private')
      expect(response.headers()['set-cookie']).toBeUndefined()
    }
    const credentials = Buffer.from(
      `${TRACE_FIXTURE_CREDENTIALS.username}:${TRACE_FIXTURE_CREDENTIALS.password}`
    ).toString('base64')
    const shell = await request.get(traceRuntimeUrl('/_ts/trace', true), {
      headers: { Authorization: `Basic ${credentials}` },
    })
    expect(shell.status()).toBe(200)
    const asset = await request.get(
      traceRuntimeUrl('/_ts/trace/assets/v1.js', true),
      { headers: { Authorization: `Basic ${credentials}` } }
    )
    expect(asset.status()).toBe(200)
    expect(asset.headers()['cache-control']).toBe('no-store, private')
  })

  test('leaves the disabled runtime inert even with a browser session cookie', async ({
    page,
  }) => {
    await page.goto(traceRuntimeUrl('/_ts/trace'))
    await page
      .getByRole('button', { name: 'Enable tracing', exact: true })
      .click()
    await expect(page.locator('#trace-session-state')).toContainText(
      'Tracing is on'
    )
    const disabled = new URL(runtimeUrl('/'))
    disabled.hostname = 'localhost'
    await page.goto(disabled.toString())
    expect(
      await page.evaluate(() => ({
        active: Reflect.get(window, '__tsjs_trace_active'),
        context: Reflect.get(window, '__tsjs_trace_request_context'),
        evidence: Reflect.get(window, 'tsjs')?.traceEvidence,
      }))
    ).toEqual({ active: undefined, context: undefined, evidence: undefined })
    const response = await page
      .context()
      .request.get(new URL('/_ts/trace', disabled).toString())
    expect(response.status()).toBe(404)
  })

  test('rejects genuine cross-site form and fetch mutations without storing a cookie', async ({
    page,
  }) => {
    const origin = `http://127.0.0.1:${process.env.INTEGRATION_ORIGIN_PORT ?? '8888'}`
    await page.goto(origin)
    const destination = traceRuntimeUrl('/_ts/trace/enable')
    const result = page.waitForResponse(
      (response) => response.url() === destination
    )
    await Promise.all([
      page.waitForURL(destination, { waitUntil: 'domcontentloaded' }),
      page.evaluate((url) => {
        const form = document.createElement('form')
        form.method = 'POST'
        form.action = url
        document.body.append(form)
        form.submit()
      }, destination),
    ])
    expect((await result).status()).toBe(403)
    expect(
      (await page.context().cookies()).find((item) => item.name === SESSION)
    ).toBeUndefined()
    await page.goto(origin)
    const fetchResponse = page.waitForResponse(
      (response) =>
        response.url() === destination && response.request().method() === 'POST'
    )
    await page.evaluate(async (url) => {
      try {
        await fetch(url, {
          method: 'POST',
          mode: 'no-cors',
          credentials: 'include',
        })
      } catch {
        /* The browser may also block access to the response. */
      }
    }, destination)
    expect((await fetchResponse).status()).toBe(403)
    expect(
      (await page.context().cookies()).find((item) => item.name === SESSION)
    ).toBeUndefined()
    expect(readState().framework).toMatch(/^(nextjs|wordpress)$/)
  })
})

test.describe('browser-carried trace report viewer', () => {
  test('blocks injected scripts, styles, frames and off-origin connections under the delivered CSP', async ({
    page,
  }) => {
    await page.goto(traceRuntimeUrl('/_ts/trace'))
    const requests: string[] = []
    page.on('request', (request) => requests.push(request.url()))
    await page.evaluate(() => {
      const violations: string[] = []
      Reflect.set(window, '__traceFixtureCspViolations', violations)
      document.addEventListener('securitypolicyviolation', (event) =>
        violations.push(event.effectiveDirective)
      )
      const script = document.createElement('script')
      script.textContent = 'window.__traceFixtureInjectedScript = true'
      document.body.append(script)
      const style = document.createElement('style')
      style.textContent = 'body { display: none }'
      document.body.append(style)
      const frame = document.createElement('iframe')
      frame.src = 'https://blocked.example.com/trace-fixture'
      document.body.append(frame)
      void fetch('https://blocked.example.com/trace-fixture').catch(() => {})
    })
    await expect
      .poll(() =>
        page.evaluate(() => Reflect.get(window, '__traceFixtureCspViolations'))
      )
      .toEqual(
        expect.arrayContaining([
          'script-src-elem',
          'style-src-elem',
          'frame-src',
          'connect-src',
        ])
      )
    expect(
      await page.evaluate(() =>
        Reflect.get(window, '__traceFixtureInjectedScript')
      )
    ).toBeUndefined()
    await expect(page.locator('body')).toBeVisible()
    expect(requests).toEqual([])
  })

  test('treats an opener-cloned report as separate unverified tab data and expires it on a later load', async ({
    page,
  }) => {
    const url = traceRuntimeUrl('/_ts/trace')
    await page.goto(url)
    const captured = Date.now()
    const stored = storedTraceReportFixture(new URL(url).origin, captured)
    await page.evaluate((value) => {
      sessionStorage.setItem(
        'trusted-server.trace.report.v1',
        JSON.stringify(value)
      )
    }, stored)
    await page.reload()
    const opening = page.waitForEvent('popup')
    await page.evaluate((destination) => {
      window.open(destination, '_blank')
    }, url)
    const clone = await opening
    try {
      await expect(clone.locator('#trace-report')).toContainText(
        'Browser-carried, unverified diagnostic data'
      )
      await page
        .getByRole('button', { name: 'Delete local report', exact: true })
        .click()
      await expect(page.locator('#trace-report')).toHaveCount(0)
      await expect(clone.locator('#trace-report')).toBeVisible()
      expect(
        await clone.evaluate(
          () =>
            JSON.parse(
              sessionStorage.getItem('trusted-server.trace.report.v1')!
            ).report
        )
      ).toEqual(stored.report)
      // This is a browser clock-restoration simulation, not a browser restart.
      await clone.clock.install({ time: captured + 16 * 60 * 1000 })
      await clone.reload()
      await expect(clone.locator('#trace-report')).toHaveCount(0)
      await expect(clone.locator('#trace-report-notice')).toContainText(
        'unavailable, expired, or unsupported'
      )
      expect(
        await clone.evaluate(() =>
          sessionStorage.getItem('trusted-server.trace.report.v1')
        )
      ).toBeNull()
    } finally {
      await clone.close()
    }
  })

  for (const rejection of [
    'expired',
    'future clock',
    'different origin',
    'unsupported version',
    'forbidden member',
  ] as const) {
    test(`rejects and removes saved data for ${rejection} without rendering or exporting it`, async ({
      page,
    }) => {
      const url = traceRuntimeUrl('/_ts/trace')
      await page.goto(url)
      const now = Date.now()
      const stored = storedTraceReportFixture(
        new URL(url).origin,
        rejection === 'expired'
          ? now - 15 * 60 * 1000 - 1000
          : rejection === 'future clock'
            ? now + 2 * 60 * 1000
            : now
      )
      if (rejection === 'different origin')
        stored.report.gpt_diagnostics.page.origin = 'https://other.example.com'
      if (rejection === 'unsupported version') stored.report.schema_version = 2
      if (rejection === 'forbidden member')
        Reflect.set(stored.report, 'raw_cookie', 'forbidden-fixture-value')
      await page.evaluate((value) => {
        sessionStorage.setItem(
          'trusted-server.trace.report.v1',
          JSON.stringify(value)
        )
        sessionStorage.setItem('unrelated-fixture-key', 'retained')
      }, stored)
      await page.reload()
      await expect(page.locator('#trace-report')).toHaveCount(0)
      await expect(page.locator('#trace-report-notice')).toContainText(
        'unavailable, expired, or unsupported'
      )
      await expect(
        page.getByRole('button', { name: 'Download', exact: true })
      ).toHaveCount(0)
      expect(
        await page.evaluate(() => ({
          report: sessionStorage.getItem('trusted-server.trace.report.v1'),
          unrelated: sessionStorage.getItem('unrelated-fixture-key'),
        }))
      ).toEqual({ report: null, unrelated: 'retained' })
      await expect(page.locator('body')).not.toContainText(
        'forbidden-fixture-value'
      )
    })
  }

  test('keeps a report exportable when local deletion fails while independently ending the real server session', async ({
    page,
  }) => {
    const url = traceRuntimeUrl('/_ts/trace')
    await page.goto(url)
    await page
      .getByRole('button', { name: 'Enable tracing', exact: true })
      .click()
    await expect(page.locator('#trace-session-state')).toContainText(
      'Tracing is on'
    )
    const stored = storedTraceReportFixture(new URL(url).origin)
    await page.evaluate((value) => {
      sessionStorage.setItem(
        'trusted-server.trace.report.v1',
        JSON.stringify(value)
      )
    }, stored)
    await page.addInitScript(() => {
      const remove = Storage.prototype.removeItem
      Reflect.set(window, '__traceFixtureDeletionBlocked', true)
      Storage.prototype.removeItem = function (key: string) {
        if (
          key === 'trusted-server.trace.report.v1' &&
          Reflect.get(window, '__traceFixtureDeletionBlocked')
        )
          throw new DOMException('Fixture deletion blocked', 'SecurityError')
        return remove.call(this, key)
      }
    })
    await page.reload()
    page.once('dialog', (dialog) => dialog.accept())
    await page
      .getByRole('button', {
        name: 'Clear report and end tracing',
        exact: true,
      })
      .click()
    await expect(page.locator('#trace-cleanup-local-status')).toContainText(
      'Local report deletion failed'
    )
    await expect(page.locator('#trace-cleanup-server-status')).toContainText(
      'Tracing is off'
    )
    await expect(page.locator('#trace-report')).toBeVisible()
    expect(
      (await page.context().cookies()).find((item) => item.name === SESSION)
    ).toBeUndefined()
    const download = page.waitForEvent('download')
    await page.getByRole('button', { name: 'Download', exact: true }).click()
    const file = await (await download).path()
    expect(JSON.parse(readFileSync(file!, 'utf8'))).toEqual(stored.report)
    await page.evaluate(() =>
      Reflect.set(window, '__traceFixtureDeletionBlocked', false)
    )
    await page
      .getByRole('button', { name: 'Delete local report', exact: true })
      .click()
    await expect(page.locator('#trace-report')).toHaveCount(0)
    await expect(page.locator('#trace-cleanup-local-status')).toHaveText(
      'Local report deleted from this tab.'
    )
    await expect(page.locator('#trace-cleanup-local-status')).toBeFocused()
  })

  test('deletes the report offline and retries end with a separate server observation after reconnecting', async ({
    page,
  }) => {
    const url = traceRuntimeUrl('/_ts/trace')
    await page.goto(url)
    await page
      .getByRole('button', { name: 'Enable tracing', exact: true })
      .click()
    await expect(page.locator('#trace-session-state')).toContainText(
      'Tracing is on'
    )
    await page.evaluate(
      (value) => {
        sessionStorage.setItem(
          'trusted-server.trace.report.v1',
          JSON.stringify(value)
        )
      },
      storedTraceReportFixture(new URL(url).origin)
    )
    await page.reload()
    await page.context().setOffline(true)
    page.once('dialog', (dialog) => dialog.accept())
    await page
      .getByRole('button', {
        name: 'Clear report and end tracing',
        exact: true,
      })
      .click()
    await expect(page.locator('#trace-report')).toHaveCount(0)
    await expect(page.locator('#trace-cleanup-local-status')).toHaveText(
      'Local report deleted from this tab.'
    )
    await expect(page.locator('#trace-cleanup-server-status')).toContainText(
      'End tracing unconfirmed'
    )
    expect(
      (await page.context().cookies()).find((item) => item.name === SESSION)
    ).toBeDefined()
    const requests: string[] = []
    page.on('request', (request) => {
      const path = new URL(request.url()).pathname
      if (path === '/_ts/trace/end' || path === '/_ts/trace/state')
        requests.push(`${request.method()} ${path}`)
    })
    await page.context().setOffline(false)
    await page
      .getByRole('button', { name: 'Retry end tracing', exact: true })
      .click()
    await expect(page.locator('#trace-cleanup-server-status')).toContainText(
      'Tracing is off'
    )
    await expect(page.locator('#trace-cleanup-server-status')).toBeFocused()
    expect(requests).toEqual(['POST /_ts/trace/end', 'GET /_ts/trace/state'])
    expect(
      (await page.context().cookies()).find((item) => item.name === SESSION)
    ).toBeUndefined()
  })

  test('renders safely at 320 px and exports the same report through real clipboard and Blob download', async ({
    page,
  }) => {
    await page.setViewportSize({ width: 320, height: 640 })
    const origin = new URL(traceRuntimeUrl('/_ts/trace')).origin
    await page
      .context()
      .grantPermissions(['clipboard-read', 'clipboard-write'], { origin })
    await page.goto(traceRuntimeUrl('/_ts/trace'))
    const stored = storedTraceReportFixture(origin)
    await page.evaluate((value) => {
      sessionStorage.setItem(
        'trusted-server.trace.report.v1',
        JSON.stringify(value)
      )
      sessionStorage.setItem('unrelated-fixture-key', 'retained')
    }, stored)
    const paths: string[] = []
    page.on('request', (request) => paths.push(new URL(request.url()).pathname))
    await page.reload()
    const article = page.locator('#trace-report')
    await expect(article).toContainText(
      'Browser-carried, unverified diagnostic data'
    )
    await expect(article).toContainText(
      stored.report.request_context.network.tls_cipher
    )
    expect(
      await article.locator('img, script, iframe, style, [style]').count()
    ).toBe(0)
    expect(
      await page.locator('#trace-viewer-setup').getAttribute('open')
    ).toBeNull()
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth
      )
    ).toBe(true)
    for (const label of [
      'Copy',
      'Download',
      'Share',
      'Clear report and end tracing',
      'Delete local report',
    ]) {
      const button = page.getByRole('button', { name: label, exact: true })
      const box = await button.boundingBox()
      expect(box?.width).toBeGreaterThanOrEqual(44)
      expect(box?.height).toBeGreaterThanOrEqual(44)
    }
    expect(paths.sort()).toEqual([
      '/_ts/trace',
      '/_ts/trace/assets/v1.css',
      '/_ts/trace/assets/v1.js',
    ])
    const copy = page.getByRole('button', { name: 'Copy', exact: true })
    await copy.focus()
    await page.keyboard.press('Enter')
    await expect(page.locator('#trace-export-status')).toHaveText(
      'Copied JSON.'
    )
    const copied = await page.evaluate(() => navigator.clipboard.readText())
    const downloading = page.waitForEvent('download')
    await page.getByRole('button', { name: 'Download', exact: true }).click()
    const downloaded = await downloading
    expect(downloaded.suggestedFilename()).toBe('trusted-server-trace-v1.json')
    const destination = await downloaded.path()
    expect(destination).not.toBeNull()
    const file = readFileSync(destination!, 'utf8')
    expect(file).toBe(copied)
    expect(JSON.parse(file)).toEqual(stored.report)
    expect(file).not.toContain('stored_at_ms')
    await expect(page.locator('#trace-export-status')).toHaveAttribute(
      'aria-live',
      'polite'
    )
    const mutations: string[] = []
    page.on('request', (request) => {
      if (
        new URL(request.url()).pathname.startsWith('/_ts/trace/') &&
        !request.url().includes('/assets/')
      )
        mutations.push(`${request.method()} ${new URL(request.url()).pathname}`)
    })
    page.once('dialog', (dialog) => dialog.accept())
    await page
      .getByRole('button', {
        name: 'Clear report and end tracing',
        exact: true,
      })
      .click()
    await expect(article).toHaveCount(0)
    await expect(page.locator('#trace-cleanup-local-status')).toHaveText(
      'Local report deleted from this tab.'
    )
    await expect(page.locator('#trace-cleanup-server-status')).toHaveText(
      'Tracing is off — no valid diagnostics session observed.'
    )
    expect(
      await page.evaluate(() =>
        sessionStorage.getItem('trusted-server.trace.report.v1')
      )
    ).toBeNull()
    expect(
      await page.evaluate(() => sessionStorage.getItem('unrelated-fixture-key'))
    ).toBe('retained')
    expect(mutations).toEqual(['POST /_ts/trace/end', 'GET /_ts/trace/state'])
  })
})
