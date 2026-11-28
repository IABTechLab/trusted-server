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
