const assert = require('node:assert/strict')
const { readFileSync } = require('node:fs')
const { test } = require('node:test')
const { JSDOM } = require('jsdom')
const source = readFileSync(require.resolve('./trace-gpt-fixture.js'), 'utf8')

for (const creativeOrigin of [
  undefined,
  'https://creative.example.com:32443',
]) {
  test(`GPT fixture renders its captured ad ID at ${creativeOrigin ?? 'the legacy origin'}`, () => {
    const dom = new JSDOM('<div id="example-slot"></div>', {
      url: 'http://localhost:31234/',
      runScripts: 'outside-only',
    })
    const { window } = dom
    if (creativeOrigin) window.__traceGptFixtureConfig = { creativeOrigin }
    window.eval(source)
    const gpt = window.googletag
    const slot = gpt.defineSlot(
      '/123456789/example-slot',
      [300, 250],
      'example-slot'
    )
    slot.setTargeting('hb_adid', 'observed-ad<&')
    gpt.pubads().refresh([slot])
    slot.setTargeting('hb_adid', 'later-ad')
    window.__traceGptFixture.complete(0)
    const url = new URL(window.document.querySelector('iframe').src)
    assert.equal(url.origin, creativeOrigin ?? 'https://creative.example.com')
    assert.equal(url.pathname, '/fixture-puc')
    assert.equal(url.searchParams.get('adId'), 'observed-ad<&')
    assert.equal(window.__traceGptFixture.requests.length, 1)
    dom.window.close()
  })
}

test('display requests internally while explicit refresh remains wrappable', () => {
  const dom = new JSDOM('<div id="example-slot"></div>', {
    url: 'http://localhost:31234/',
    runScripts: 'outside-only',
  })
  const { window } = dom
  window.eval(source)
  const gpt = window.googletag
  const slot = gpt.defineSlot(
    '/123456789/example-slot',
    [300, 250],
    'example-slot'
  )
  slot.setTargeting('hb_adid', 'observed-server-ad')
  const refresh = gpt.pubads().refresh
  let publicRefreshes = 0
  gpt.pubads().refresh = (slots) => {
    publicRefreshes++
    return refresh(slots)
  }
  gpt.display('example-slot')
  assert.equal(
    publicRefreshes,
    0,
    'display must not call the publisher-facing refresh hook'
  )
  assert.equal(window.__traceGptFixture.requests.length, 1)
  assert.equal(
    window.__traceGptFixture.requests[0].targeting.hb_adid[0],
    'observed-server-ad'
  )
  gpt.pubads().disableInitialLoad()
  gpt.display('example-slot')
  assert.equal(
    window.__traceGptFixture.requests.length,
    1,
    'disabled initial load must suppress display requests'
  )
  gpt.pubads().refresh([slot])
  assert.equal(
    publicRefreshes,
    1,
    'explicit refresh must retain the public hook'
  )
  assert.equal(window.__traceGptFixture.requests.length, 2)
  dom.window.close()
})
