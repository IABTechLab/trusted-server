import { describe, expect, it } from 'vitest';

import { presentTraceOverlay } from '../../../src/integrations/ad_trace/presentation';
import type { AdTraceStage, AdTraceStageName, RenderTraceSnapshot } from '../../../src/core/types';

function stage(outcome = 'not_observed', reason = 'none'): AdTraceStage {
  return { outcome, confidence: 'none', reason };
}

function stages(overrides: Partial<Record<AdTraceStageName, AdTraceStage>> = {}) {
  return {
    trustedServer: stage(),
    prebid: stage(),
    gam: stage(),
    creative: stage(),
    ...overrides,
  };
}

function render(
  outcome: RenderTraceSnapshot['outcome'],
  overrides: Partial<RenderTraceSnapshot> = {}
): RenderTraceSnapshot {
  return {
    sequence: 4,
    slotId: 'slot-a',
    generation: 2,
    source: 'gpt',
    outcome,
    confidence: 'probable',
    visibility: 'unknown',
    createdAt: 1,
    updatedAt: 1,
    ...overrides,
  };
}

describe('presentTraceOverlay', () => {
  it.each([
    [
      'server winner',
      stages({ trustedServer: stage('won') }),
      undefined,
      ['Trusted Server selected a bid'],
    ],
    [
      'server no bid',
      stages({ trustedServer: stage('no_bid') }),
      undefined,
      ['Trusted Server returned no bid'],
    ],
    [
      'server skip',
      stages({ trustedServer: stage('skipped') }),
      undefined,
      ['Trusted Server auction skipped'],
    ],
    [
      'server failure',
      stages({ trustedServer: stage('failed') }),
      undefined,
      ['Trusted Server auction did not complete'],
    ],
    [
      'server abandonment',
      stages({ trustedServer: stage('abandoned') }),
      undefined,
      ['Trusted Server auction did not complete'],
    ],
    [
      'traced Prebid selection',
      stages({ prebid: stage('won') }),
      undefined,
      ['Prebid selected the Trusted Server bid'],
    ],
    [
      'client Prebid selection',
      stages({ prebid: stage('client_bid_won') }),
      undefined,
      ['Prebid selected a client bid'],
    ],
    [
      'client Prebid selection recorded as lost server targeting',
      stages({ prebid: stage('lost') }),
      undefined,
      ['Prebid selected a client bid'],
    ],
    [
      'reported Prebid win',
      stages({ prebid: stage('won', 'selected_targeting_with_bid_won') }),
      undefined,
      ['Prebid selected the Trusted Server bid', 'Prebid reported the bid won'],
    ],
    ['GAM empty', stages({ gam: stage('empty') }), undefined, ['GAM returned no ad']],
    ['GAM backfill', stages({ gam: stage('backfill') }), undefined, ['GAM returned backfill']],
    [
      'unattributed GAM render',
      stages({ gam: stage('direct_or_unattributed') }),
      undefined,
      ['GAM rendered an ad — source not attributed'],
    ],
    [
      'selected Trusted Server GAM creative',
      stages({ gam: stage('trusted_server_won') }),
      undefined,
      ['GAM selected the Trusted Server creative'],
    ],
    [
      'GAM iframe load',
      stages({ creative: stage('gpt_iframe_onload') }),
      undefined,
      ['GAM creative iframe loaded'],
    ],
    [
      'creative acknowledgement',
      stages({ creative: stage('load_acknowledged') }),
      undefined,
      ['Trusted Server creative load confirmed'],
    ],
    [
      'direct iframe acknowledgement',
      stages({ creative: stage('load_acknowledged', 'direct_iframe_load') }),
      render('confirmed', { source: 'direct_auction' }),
      ['Creative iframe load confirmed'],
    ],
    [
      'Prebid render success',
      stages({ creative: stage('prebid_render_succeeded') }),
      undefined,
      ['Prebid reported render succeeded'],
    ],
    [
      'Prebid render failure',
      stages({ creative: stage('render_failed') }),
      undefined,
      ['Prebid reported render failed'],
    ],
    [
      'direct APS renderer started',
      stages({ creative: stage('renderer_served', 'direct_aps_renderer') }),
      render('served', { source: 'direct_auction', reason: 'direct_aps_renderer' }),
      ['APS renderer started creative loading'],
    ],
    [
      'direct APS renderer ready',
      stages({ creative: stage('aps_renderer_ready', 'direct_aps_renderer_ready') }),
      render('served', { source: 'direct_auction', reason: 'direct_aps_renderer_ready' }),
      ['APS renderer reported ready'],
    ],
    [
      'direct render rejection',
      stages({ creative: stage('rejected') }),
      undefined,
      ['Trusted Server direct render rejected'],
    ],
    [
      'creative acknowledgement timeout',
      stages({ creative: stage('ack_timed_out') }),
      render('timed_out'),
      ['Creative confirmation timed out'],
    ],
    [
      'creative acknowledgement source mismatch',
      stages({ creative: stage('ack_source_mismatched') }),
      undefined,
      ['Creative acknowledgement source did not match'],
    ],
    [
      'creative acknowledgement missing token',
      stages({ creative: stage('ack_missing_token') }),
      undefined,
      ['Creative confirmation unavailable — trace token missing'],
    ],
    [
      'creative acknowledgement superseded',
      stages({ creative: stage('ack_superseded') }),
      undefined,
      ['Creative confirmation superseded'],
    ],
    [
      'current visibility',
      stages(),
      render('unresolved', { visibility: 'visible' }),
      ['Slot element currently visible'],
    ],
    [
      'viewable impression independent of live visibility',
      stages({ creative: stage('gpt_iframe_onload') }),
      render('gam_only', { visibility: 'hidden', viewability: 'viewable' }),
      [
        'GAM creative iframe loaded',
        'GAM rendered an ad — source not attributed',
        'Slot element currently hidden',
        'Viewable impression observed',
      ],
    ],
  ])('%s uses factual operator language', (_name, input, snapshot, expected) => {
    expect(presentTraceOverlay(input, snapshot).facts).toEqual(expected);
  });

  it('hides unobserved and inapplicable stages without leaking internal vocabulary', () => {
    const presentation = presentTraceOverlay(
      stages({
        trustedServer: stage('unresolved'),
        prebid: stage('not_run', 'direct'),
        gam: stage('not_observed'),
        creative: stage('not_observed'),
      }),
      render('unresolved', { visibility: 'unknown' })
    );

    expect(presentation.facts).toEqual([]);
    expect(presentation.renderStatus).toBeUndefined();
    expect(JSON.stringify(presentation)).not.toMatch(
      /definitive|strong|probable|not_run|not_observed|unresolved|gam_only|client_bid_won/
    );
  });

  it.each([
    ['attributed', stages({ creative: stage('load_acknowledged') }), undefined],
    ['empty', stages({ gam: stage('empty') }), undefined],
    ['failed', stages({ creative: stage('render_failed') }), undefined],
    ['unattributed', stages({ gam: stage('trusted_server_candidate') }), render('gam_only')],
  ] as const)('uses an evidence-based %s presentation class', (expected, input, snapshot) => {
    expect(presentTraceOverlay(input, snapshot).className).toBe(expected);
  });

  it('creates one concise badge from independent response, load, and viewability facts', () => {
    const presentation = presentTraceOverlay(
      stages({
        gam: stage('backfill'),
        creative: stage('gpt_iframe_onload'),
      }),
      render('gam_only', { reason: 'gpt_backfill', viewability: 'viewable' }),
      {
        requestNumber: 2,
        terminalState: 'rendered',
        responseClass: 'backfill',
        durations: {},
      }
    );

    expect(presentation.badgeStatus).toBe('GAM backfill · loaded · viewable');
    expect(presentation.facts).toContain('GAM returned backfill');
    expect(presentation.facts).toContain('GAM creative iframe loaded');
    expect(presentation.facts).toContain('Viewable impression observed');
  });

  it.each([
    [
      'confirmed Trusted Server creative',
      render('confirmed'),
      'Trusted Server creative load confirmed',
    ],
    [
      'confirmed direct creative',
      render('confirmed', { source: 'direct_auction' }),
      'Creative iframe load confirmed',
    ],
    ['served renderer', render('served'), 'Creative response sent to the renderer'],
    ['timed-out confirmation', render('timed_out'), 'Creative confirmation timed out'],
    ['unattributed GAM render', render('gam_only'), 'GAM rendered an ad — source not attributed'],
    [
      'GAM backfill response',
      render('gam_only', { reason: 'gpt_backfill' }),
      'GAM returned backfill',
    ],
    [
      'generic GPT request',
      render('unresolved', { reason: 'gpt_slot_requested' }),
      'GAM request observed',
    ],
    ['empty GAM response', render('empty'), 'GAM returned no ad'],
  ] as const)('renders %s as a concise factual row status', (_name, snapshot, expected) => {
    expect(presentTraceOverlay(stages(), snapshot).renderStatus).toBe(expected);
  });

  it('deduplicates the GAM backfill stage and render facts', () => {
    const presentation = presentTraceOverlay(
      stages({ gam: stage('backfill') }),
      render('gam_only', { reason: 'gpt_backfill' })
    );

    expect(presentation.facts).toEqual(['GAM returned backfill']);
    expect(presentation.primaryStatus).toBe('GAM returned backfill');
  });

  it('falls back to the strongest observed stage fact for a primary row', () => {
    const presentation = presentTraceOverlay(
      stages({ creative: stage('render_failed') }),
      render('unresolved')
    );

    expect(presentation.renderStatus).toBeUndefined();
    expect(presentation.primaryStatus).toBe('Prebid reported render failed');
  });
});
