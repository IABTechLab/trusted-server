import type {
  AdTraceGamIdentity,
  AdTraceStage,
  AdTraceStageName,
  GenerationTraceDiagnostics,
  RenderTraceSnapshot,
  RenderTraceVisibility,
} from '../../core/types';

export type TraceOverlayPresentationClass = 'attributed' | 'unattributed' | 'empty' | 'failed';

export interface TraceOverlayPresentation {
  /** Facts suitable for operator-facing badges and primary timeline rows. */
  facts: readonly string[];
  /** One concise description of the render result when render evidence exists. */
  renderStatus?: string;
  /** Best observed fact for a compact primary timeline row. */
  primaryStatus?: string;
  /** Concise, single-line badge summary. */
  badgeStatus?: string;
  className: TraceOverlayPresentationClass;
}

type TraceStages = Record<AdTraceStageName, AdTraceStage>;

const STAGE_ORDER: readonly AdTraceStageName[] = ['trustedServer', 'prebid', 'gam', 'creative'];
const UNATTRIBUTED_RENDER_STATUS = 'GAM rendered an ad — source not attributed';
const NAMED_GAM_OUTCOMES = new Set([
  'trusted_server_won',
  'other_gam_demand',
  'other_reservation',
  'gam_default_or_unclassified',
]);

function stageFacts(name: AdTraceStageName, stage: AdTraceStage): readonly string[] {
  if (stage.outcome === 'not_observed' || stage.outcome === 'not_run') return [];

  switch (name) {
    case 'trustedServer':
      switch (stage.outcome) {
        case 'won':
          return ['Trusted Server selected a bid'];
        case 'no_bid':
          return ['Trusted Server returned no bid'];
        case 'skipped':
          return ['Trusted Server auction skipped'];
        case 'failed':
        case 'abandoned':
          return ['Trusted Server auction did not complete'];
        default:
          return [];
      }
    case 'prebid':
      if (stage.outcome === 'won') {
        return stage.reason === 'selected_targeting_with_bid_won'
          ? ['Prebid selected the Trusted Server bid', 'Prebid reported the bid won']
          : ['Prebid selected the Trusted Server bid'];
      }
      if (stage.outcome === 'client_bid_won' || stage.outcome === 'lost') {
        return stage.reason === 'selected_targeting_with_bid_won'
          ? ['Prebid selected a client bid', 'Prebid reported the bid won']
          : ['Prebid selected a client bid'];
      }
      return [];
    case 'gam':
      switch (stage.outcome) {
        case 'empty':
          return ['GAM returned no ad'];
        case 'backfill':
          return ['GAM returned backfill'];
        case 'trusted_server_won':
          return ['GAM selected the Trusted Server creative'];
        case 'other_gam_demand':
          return ['GAM delivered other demand — the Trusted Server creative never ran'];
        case 'other_reservation':
          return ['GAM delivered another reservation line item'];
        case 'gam_default_or_unclassified':
          return ['GAM delivered its own default or backup ad'];
        case 'trusted_server_candidate':
        case 'client_prebid_candidate':
        case 'direct_or_unattributed':
          return ['GAM rendered an ad — source not attributed'];
        default:
          return [];
      }
    case 'creative':
      switch (stage.outcome) {
        case 'gpt_iframe_onload':
          return ['GAM creative iframe loaded'];
        case 'load_acknowledged':
          return [
            stage.reason === 'direct_iframe_load'
              ? 'Creative iframe load confirmed'
              : 'Trusted Server creative load confirmed',
          ];
        case 'prebid_render_succeeded':
          return ['Prebid reported render succeeded'];
        case 'render_failed':
          return ['Prebid reported render failed'];
        case 'aps_renderer_ready':
          return ['APS renderer reported ready'];
        case 'renderer_served':
          if (stage.reason === 'direct_aps_renderer') {
            return ['APS renderer started creative loading'];
          }
          if (stage.reason === 'aps_renderer') return ['APS renderer response sent'];
          return ['Creative response sent to the renderer'];
        case 'rejected':
          return ['Trusted Server direct render rejected'];
        case 'ack_timed_out':
          return ['Creative confirmation timed out'];
        case 'ack_missing_token':
          return ['Creative confirmation unavailable — trace token missing'];
        case 'ack_source_mismatched':
          return ['Creative acknowledgement source did not match'];
        case 'ack_superseded':
          return ['Creative confirmation superseded'];
        default:
          return [];
      }
  }
}

function renderStatus(render?: RenderTraceSnapshot): string | undefined {
  if (!render) return undefined;

  switch (render.outcome) {
    case 'confirmed':
      return render.source === 'direct_auction'
        ? 'Creative iframe load confirmed'
        : 'Trusted Server creative load confirmed';
    case 'served':
      if (render.reason === 'direct_aps_renderer_ready') return 'APS renderer reported ready';
      if (render.reason === 'direct_aps_renderer') return 'APS renderer started creative loading';
      if (render.reason === 'aps_renderer') return 'APS renderer response sent';
      if (render.reason === 'direct_iframe_created') return 'Creative iframe created';
      return 'Creative response sent to the renderer';
    case 'timed_out':
      return 'Creative confirmation timed out';
    case 'gam_only':
      return render.reason === 'gpt_backfill'
        ? 'GAM returned backfill'
        : 'GAM rendered an ad — source not attributed';
    case 'empty':
      return 'GAM returned no ad';
    case 'unresolved':
      return render.reason === 'gpt_slot_requested' ? 'GAM request observed' : undefined;
  }
}

/**
 * Name the delivered ad using Ad Manager's own report of what it served.
 *
 * These identifiers are the operator's own Ad Manager data — the same values
 * `?google_console=1` shows — and are the only way to tell a house ad from a
 * direct-sold line item when Trusted Server did not win the Ad Manager decision.
 */
function gamIdentityFact(identity: AdTraceGamIdentity | undefined): string | undefined {
  if (!identity) return undefined;

  const parts: string[] = [];
  const lineItem = identity.lineItemId ?? identity.sourceAgnosticLineItemId;
  const creative = identity.creativeId ?? identity.sourceAgnosticCreativeId;
  if (lineItem) parts.push(`line item ${lineItem}`);
  if (identity.campaignId) parts.push(`order ${identity.campaignId}`);
  if (identity.advertiserId) parts.push(`advertiser ${identity.advertiserId}`);
  if (creative) parts.push(`creative ${creative}`);
  if (identity.yieldGroupIds?.length) {
    parts.push(`yield group ${identity.yieldGroupIds.join(', ')}`);
  }
  if (identity.companyIds?.length) parts.push(`company ${identity.companyIds.join(', ')}`);
  return parts.length > 0 ? `GAM reported ${parts.join(' · ')}` : undefined;
}

function visibilityFact(visibility: RenderTraceVisibility | undefined): string | undefined {
  if (visibility === 'visible') return 'Slot element currently visible';
  if (visibility === 'hidden') return 'Slot element currently hidden';
  return undefined;
}

function presentationClass(
  stages: TraceStages,
  render?: RenderTraceSnapshot
): TraceOverlayPresentationClass {
  if (
    stages.creative.outcome === 'load_acknowledged' ||
    stages.gam.outcome === 'trusted_server_won' ||
    render?.outcome === 'confirmed'
  ) {
    return 'attributed';
  }
  if (stages.creative.outcome === 'render_failed') return 'failed';
  if (stages.gam.outcome === 'empty' || render?.outcome === 'empty') return 'empty';
  return 'unattributed';
}

/**
 * Convert internal trace stages into factual operator-facing language.
 *
 * Raw outcomes, confidence, reasons, sequence IDs, and generation IDs remain
 * available in technical details and exports; they are intentionally excluded
 * from this presentation surface.
 */
function compactBadgeStatus(
  stages: TraceStages,
  render: RenderTraceSnapshot | undefined,
  diagnostics: GenerationTraceDiagnostics | undefined,
  primaryStatus: string | undefined
): string | undefined {
  let response: string | undefined;
  if (stages.gam.outcome === 'trusted_server_won') response = 'TS creative';
  else if (stages.gam.outcome === 'backfill' || diagnostics?.responseClass === 'backfill') {
    response = 'GAM backfill';
  } else if (stages.gam.outcome === 'empty' || diagnostics?.responseClass === 'empty') {
    response = 'GAM empty';
  } else if (stages.gam.outcome === 'gam_default_or_unclassified') {
    response = 'GAM default ad';
  } else if (
    stages.gam.outcome === 'other_gam_demand' ||
    stages.gam.outcome === 'other_reservation'
  ) {
    const lineItem =
      diagnostics?.gamIdentity?.lineItemId ?? diagnostics?.gamIdentity?.sourceAgnosticLineItemId;
    response = lineItem ? `GAM line item ${lineItem}` : 'GAM other demand';
  } else if (
    stages.gam.outcome === 'trusted_server_candidate' ||
    stages.gam.outcome === 'client_prebid_candidate' ||
    stages.gam.outcome === 'direct_or_unattributed' ||
    render?.outcome === 'gam_only'
  ) {
    response = 'GAM ad';
  }

  let delivery: string | undefined;
  if (diagnostics?.acknowledgement === 'confirmed') delivery = 'confirmed';
  else if (stages.creative.outcome === 'gpt_iframe_onload') delivery = 'loaded';
  else if (diagnostics?.acknowledgement === 'timed_out' || render?.outcome === 'timed_out') {
    delivery = 'confirmation timed out';
  } else if (stages.creative.outcome === 'render_failed') delivery = 'render failed';

  const parts = [response, delivery, render?.viewability === 'viewable' ? 'viewable' : undefined]
    .filter((part): part is string => !!part)
    .slice(0, 3);
  return parts.length > 0 ? parts.join(' · ') : primaryStatus;
}

export function presentTraceOverlay(
  stages: TraceStages,
  render?: RenderTraceSnapshot,
  diagnostics?: GenerationTraceDiagnostics
): TraceOverlayPresentation {
  const facts = new Set<string>();
  for (const name of STAGE_ORDER) {
    for (const fact of stageFacts(name, stages[name])) facts.add(fact);
  }
  // A named GAM decision outranks the generic render-timeline wording, which
  // cannot distinguish other Ad Manager demand from an unresolved Trusted
  // Server render.
  const namedGamStatus = NAMED_GAM_OUTCOMES.has(stages.gam.outcome)
    ? stageFacts('gam', stages.gam)[0]
    : undefined;
  const timelineStatus = renderStatus(render);
  const status =
    namedGamStatus && timelineStatus === UNATTRIBUTED_RENDER_STATUS
      ? namedGamStatus
      : timelineStatus;
  if (status) facts.add(status);
  const identity = gamIdentityFact(diagnostics?.gamIdentity);
  if (identity) facts.add(identity);
  const visibility = visibilityFact(render?.visibility);
  if (visibility) facts.add(visibility);
  if (render?.viewability === 'viewable') facts.add('Viewable impression observed');
  const factList = [...facts];
  const primaryStatus =
    status ?? [...factList].reverse().find((fact) => !fact.startsWith('Slot element currently '));
  const badgeStatus = compactBadgeStatus(stages, render, diagnostics, primaryStatus);

  return {
    facts: factList,
    renderStatus: status,
    ...(primaryStatus ? { primaryStatus } : {}),
    ...(badgeStatus ? { badgeStatus } : {}),
    className: presentationClass(stages, render),
  };
}
