import type {
  AdTraceStage,
  AdTraceStageName,
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
  className: TraceOverlayPresentationClass;
}

type TraceStages = Record<AdTraceStageName, AdTraceStage>;

const STAGE_ORDER: readonly AdTraceStageName[] = ['trustedServer', 'prebid', 'gam', 'creative'];

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
    case 'gam_only':
      return 'GAM rendered an ad — source not attributed';
    case 'empty':
      return 'GAM returned no ad';
    case 'unresolved':
      return undefined;
  }
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
export function presentTraceOverlay(
  stages: TraceStages,
  render?: RenderTraceSnapshot
): TraceOverlayPresentation {
  const facts = new Set<string>();
  for (const name of STAGE_ORDER) {
    for (const fact of stageFacts(name, stages[name])) facts.add(fact);
  }
  const status = renderStatus(render);
  if (status) facts.add(status);
  const visibility = visibilityFact(render?.visibility);
  if (visibility) facts.add(visibility);
  if (render?.viewability === 'viewable') facts.add('Viewable impression observed');
  const factList = [...facts];
  const primaryStatus =
    status ?? [...factList].reverse().find((fact) => !fact.startsWith('Slot element currently '));

  return {
    facts: factList,
    renderStatus: status,
    ...(primaryStatus ? { primaryStatus } : {}),
    className: presentationClass(stages, render),
  };
}
