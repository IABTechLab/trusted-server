import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import {
  baselineExporter,
  fixtureBridge,
} from './prepare-client-cwv-baseline.mjs'

const exporter = readFileSync(
  new URL(
    '../crates/trusted-server-integration-tests/examples/cwv_html.rs',
    import.meta.url
  ),
  'utf8'
)
const publisher = readFileSync(
  new URL('../crates/trusted-server-core/src/publisher.rs', import.meta.url),
  'utf8'
)

test('baseline adapts only the copied exporter and retains production HTML/JS interfaces', () => {
  const adapted = baselineExporter(exporter)
  assert(!adapted.includes('CreativeOpportunityInitialization'))
  assert(!adapted.includes('.with_request_path('))
  assert(!adapted.includes('.with_enforced_csp('))
  assert(!adapted.includes('("registered", "/news/article"'))
  for (const required of [
    'create_html_processor(config)',
    'handle_tsjs_dynamic(',
    'fixture_ad_scripts(',
    'sourceFingerprints',
    'include_str!("cwv_html.rs")',
  ]) {
    assert(adapted.includes(required), `Must retain ${required}`)
  }
  assert(
    exporter.includes('.with_request_path('),
    'Original candidate exporter remains unchanged'
  )
  assert.throws(
    () => baselineExporter(adapted),
    /Expected one fixture adaptation/
  )
})

test('fixture bridge is test-only and delegates to baseline production serializers', () => {
  const bridge = fixtureBridge(publisher)
  assert(bridge.includes('#[cfg(feature = "test-utils")]'))
  for (const call of [
    'match_renderable_slots(',
    'template_ad_slots_script(',
    'seam_ad_slots_json(',
    'build_bids_script(',
    'build_seam_script(',
  ])
    assert(bridge.includes(call))
  assert(!bridge.includes('publisher_script_tags'))
  assert(!bridge.includes('render_slot_reservations'))
  assert(!bridge.includes('fn build_slot_json'))
  assert.throws(() => fixtureBridge(''), /Expected the delimited/)
})
