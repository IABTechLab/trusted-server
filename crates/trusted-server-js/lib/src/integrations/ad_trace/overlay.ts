import type {
  AdTraceApi,
  AdTraceCoverageCategory,
  AdTraceStage,
  AdTraceStageName,
  GenerationTraceSnapshot,
  RenderTraceSnapshot,
  RenderTraceVisibility,
  SlotTraceSnapshot,
} from '../../core/types';

import { presentTraceOverlay } from './presentation';

const HOST_ID = 'ts-ad-trace-overlay';
const TRACE_ATTRIBUTES = [
  'data-ts-trace-seq',
  'data-ts-trace-generation',
  'data-ts-auction-trace-id',
  'data-ts-bid-trace-id',
  'data-ts-trace-outcome',
  'data-ts-trace-visibility',
] as const;

const EMPTY_STAGES: Record<AdTraceStageName, AdTraceStage> = {
  trustedServer: { outcome: 'not_observed', confidence: 'none', reason: 'none' },
  prebid: { outcome: 'not_observed', confidence: 'none', reason: 'none' },
  gam: { outcome: 'not_observed', confidence: 'none', reason: 'none' },
  creative: { outcome: 'not_observed', confidence: 'none', reason: 'none' },
};

function generationForRender(
  slot: SlotTraceSnapshot,
  render: RenderTraceSnapshot
): GenerationTraceSnapshot | undefined {
  return slot.generations.find((generation) => generation.generation === render.generation);
}

function stagesForRender(slot: SlotTraceSnapshot, render: RenderTraceSnapshot) {
  return (
    generationForRender(slot, render)?.stages ??
    (slot.latestGeneration === render.generation ? slot.stages : undefined)
  );
}

export interface TraceBadgeLayoutInput {
  key: string;
  left: number;
  top: number;
  width: number;
  height: number;
}

export interface TraceBadgePlacement {
  key: string;
  left: number;
  top: number;
}

function overlaps(
  left: number,
  top: number,
  width: number,
  height: number,
  placed: TraceBadgeLayoutInput
): boolean {
  return (
    left < placed.left + placed.width &&
    left + width > placed.left &&
    top < placed.top + placed.height &&
    top + height > placed.top
  );
}

/** Place visible badges deterministically without allowing viewport-edge piles. */
export function placeTraceBadges(
  inputs: readonly TraceBadgeLayoutInput[],
  viewportWidth: number,
  viewportHeight: number,
  gap = 4
): TraceBadgePlacement[] {
  const placed: TraceBadgeLayoutInput[] = [];
  for (const input of [...inputs].sort(
    (left, right) =>
      left.top - right.top || left.left - right.left || left.key.localeCompare(right.key)
  )) {
    const width = Math.min(input.width, viewportWidth);
    const height = Math.min(input.height, viewportHeight);
    const left = Math.max(0, Math.min(input.left, viewportWidth - width));
    let top = Math.max(0, input.top);
    let collision = placed.find((item) => overlaps(left, top, width, height, item));
    while (collision) {
      top = collision.top + collision.height + gap;
      collision = placed.find((item) => overlaps(left, top, width, height, item));
    }
    if (top + height > viewportHeight) continue;
    placed.push({ key: input.key, left, top, width, height });
  }
  return placed.map(({ key, left, top }) => ({ key, left, top }));
}

function latestRenderForSlot(
  renders: readonly RenderTraceSnapshot[],
  slot: SlotTraceSnapshot
): RenderTraceSnapshot | undefined {
  for (let index = renders.length - 1; index >= 0; index -= 1) {
    const render = renders[index];
    if (render.slotId === slot.slotId && render.generation === slot.latestGeneration) return render;
  }
  return undefined;
}

function removeTraceAttributes(element: HTMLElement): void {
  for (const attribute of TRACE_ATTRIBUTES) element.removeAttribute(attribute);
}

function effectiveVisibility(element: HTMLElement, rect: DOMRect): RenderTraceVisibility {
  if (!element.isConnected) return 'disconnected';
  if (rect.width <= 0 || rect.height <= 0) return 'hidden';
  let current: HTMLElement | null = element;
  while (current) {
    const style = getComputedStyle(current);
    if (style.display === 'none' || style.visibility === 'hidden' || style.opacity === '0') {
      return 'hidden';
    }
    current = current.parentElement;
  }
  return 'visible';
}

function isRectInViewport(rect: DOMRect): boolean {
  const right = Number.isFinite(rect.right) ? rect.right : rect.left + rect.width;
  const bottom = Number.isFinite(rect.bottom) ? rect.bottom : rect.top + rect.height;
  return (
    rect.width > 0 &&
    rect.height > 0 &&
    right > 0 &&
    bottom > 0 &&
    rect.left < window.innerWidth &&
    rect.top < window.innerHeight
  );
}

type TraceFilter =
  | 'all'
  | 'visible'
  | 'current'
  | 'attributed'
  | 'unattributed'
  | 'backfill'
  | 'empty'
  | 'failed'
  | 'unresolved';

function matchesFilter(
  filter: TraceFilter,
  slot: SlotTraceSnapshot | undefined,
  render: RenderTraceSnapshot,
  stages: Record<AdTraceStageName, AdTraceStage>,
  className: string
): boolean {
  switch (filter) {
    case 'visible':
      return render.visibility === 'visible';
    case 'current':
      return slot !== undefined && render.generation === slot.latestGeneration;
    case 'attributed':
      return className === 'attributed';
    case 'unattributed':
      return className === 'unattributed';
    case 'backfill':
      return stages.gam.outcome === 'backfill' || render.reason === 'gpt_backfill';
    case 'empty':
      return stages.gam.outcome === 'empty' || render.outcome === 'empty';
    case 'failed':
      return className === 'failed';
    case 'unresolved':
      return render.outcome === 'unresolved' || stages.gam.outcome === 'unresolved';
    case 'all':
      return true;
  }
}

const COVERAGE_LABELS: ReadonlyArray<[AdTraceCoverageCategory, string]> = [
  ['gpt_requests', 'GPT requests'],
  ['gpt_responses', 'GPT responses'],
  ['gpt_renders', 'GPT renders'],
  ['gpt_loads', 'GPT loads'],
  ['gpt_visibility', 'GPT visibility'],
  ['gpt_viewability', 'GPT viewability'],
  ['prebid_render_succeeded', 'Prebid render success'],
  ['prebid_render_failed', 'Prebid render failure'],
];

function coverageSummary(
  coverage: ReturnType<AdTraceApi['export']>['metadata']['coverage'] | undefined
): string {
  if (!coverage) return '';
  return COVERAGE_LABELS.filter(([category]) => coverage[category].observed > 0)
    .map(
      ([category, label]) =>
        `${label}: ${coverage[category].correlated}/${coverage[category].observed} correlated`
    )
    .join('\n');
}

function anomalySummary(anomalies: Record<string, number> | undefined): string {
  const entries = Object.entries(anomalies ?? {}).filter(([, count]) => count > 0);
  if (entries.length === 0) return '';
  return `Anomalies: ${entries
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([reason, count]) => `${reason} (${count})`)
    .join(', ')}`;
}

function stampRender(element: HTMLElement, render: RenderTraceSnapshot): void {
  removeTraceAttributes(element);
  element.setAttribute('data-ts-trace-seq', String(render.sequence));
  element.setAttribute('data-ts-trace-generation', String(render.generation));
  element.setAttribute('data-ts-trace-outcome', render.outcome);
  element.setAttribute('data-ts-trace-visibility', render.visibility);
  if (render.auctionTraceId)
    element.setAttribute('data-ts-auction-trace-id', render.auctionTraceId);
  if (render.bidTraceId) element.setAttribute('data-ts-bid-trace-id', render.bidTraceId);
}

/** Install one read-only Shadow DOM trace console. */
export function installAdTraceOverlay(
  api: AdTraceApi,
  subscribe: (fn: () => void) => () => void
): void {
  if (document.getElementById(HOST_ID)) return;
  const host = document.createElement('div');
  host.id = HOST_ID;
  const root = host.attachShadow({ mode: 'closed' });
  const style = document.createElement('style');
  style.textContent = `
    :host { all: initial; }
    .badge { position: fixed; z-index: 2147483647; max-width: 300px; padding: 6px 8px;
      border: 1px solid #72e0a6; border-radius: 4px; background: rgba(10,18,16,.94);
      color: #eefbf4; font: 11px/1.35 ui-monospace, monospace; white-space: nowrap; cursor: pointer; }
    .badge.attributed { border-color: #72e0a6; }
    .badge.unattributed { border-color: #67a8ff; }
    .badge.empty { border-color: #ffd479; }
    .badge.failed { border-color: #ff7b72; }
    .panel { position: fixed; right: 12px; bottom: 12px; z-index: 2147483647; width: 460px;
      max-height: 60vh; overflow: auto; padding: 10px; background: #0a1210; color: #eefbf4;
      border: 1px solid #72e0a6; font: 11px/1.4 ui-monospace, monospace; }
    .controls { display: flex; gap: 6px; position: sticky; top: 0; background: #0a1210; }
    .health { white-space: pre-wrap; color: #b8d8c7; margin: 6px 0; }
    .warning { color: #ffd479; margin: 6px 0; }
    .row { border-top: 1px solid #29443a; padding: 6px 0; }
    .row strong { color: #72e0a6; }
    .facts { white-space: pre-wrap; color: #b8d8c7; }
    button { margin-bottom: 6px; } pre { white-space: pre-wrap; }`;
  root.appendChild(style);
  const badgeLayer = document.createElement('div');
  const panel = document.createElement('div');
  panel.className = 'panel';
  const controls = document.createElement('div');
  controls.className = 'controls';
  const collapseButton = document.createElement('button');
  collapseButton.textContent = 'Collapse';
  const exportButton = document.createElement('button');
  exportButton.textContent = 'Export trace';
  const closeButton = document.createElement('button');
  closeButton.textContent = 'Close';
  const filterSelect = document.createElement('select');
  for (const [value, label] of [
    ['all', 'All'],
    ['visible', 'Visible'],
    ['current', 'Current generations'],
    ['attributed', 'Attributed'],
    ['unattributed', 'Unattributed'],
    ['backfill', 'Backfill'],
    ['empty', 'Empty'],
    ['failed', 'Failed'],
    ['unresolved', 'Unresolved'],
  ] as const) {
    const option = document.createElement('option');
    option.value = value;
    option.textContent = label;
    filterSelect.appendChild(option);
  }
  const health = document.createElement('div');
  health.className = 'health';
  const warning = document.createElement('div');
  warning.className = 'warning';
  warning.textContent = 'A non-empty GAM response alone is not proof of a Trusted Server creative.';
  const rows = document.createElement('div');
  const details = document.createElement('pre');
  details.hidden = true;
  controls.append(collapseButton, exportButton, filterSelect, closeButton);
  panel.append(controls, health, warning, rows, details);
  root.append(badgeLayer, panel);
  document.documentElement.appendChild(host);
  let cleanup = (): void => {};

  exportButton.addEventListener('click', () => {
    const blob = new Blob([JSON.stringify(api.export(), null, 2)], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const link = document.createElement('a');
    link.href = url;
    link.download = 'trusted-server-ad-trace.json';
    link.click();
    URL.revokeObjectURL(url);
  });
  collapseButton.addEventListener('click', () => {
    rows.hidden = !rows.hidden;
    health.hidden = rows.hidden;
    warning.hidden = rows.hidden;
    collapseButton.textContent = rows.hidden ? 'Expand' : 'Collapse';
  });
  closeButton.addEventListener('click', () => {
    cleanup();
    host.remove();
  });
  filterSelect.addEventListener('change', () => schedule());

  let observedElements = new Set<HTMLElement>();
  const resizeObserver =
    typeof ResizeObserver === 'undefined' ? undefined : new ResizeObserver(() => schedule());

  const render = (): void => {
    badgeLayer.replaceChildren();
    rows.replaceChildren();
    const exported = api.export();
    const filter = filterSelect.value as TraceFilter;
    const slotById = new Map(exported.slots.map((slot) => [slot.slotId, slot]));
    const nextObserved = new Set<HTMLElement>();
    const badgeElements = new Map<string, HTMLElement>();
    const badgeInputs: TraceBadgeLayoutInput[] = [];
    health.textContent =
      [coverageSummary(exported.metadata.coverage), anomalySummary(exported.metadata.anomalies)]
        .filter(Boolean)
        .join('\n') || 'No callback health observed';

    for (const item of [...exported.renders].reverse()) {
      const slot = slotById.get(item.slotId);
      const generation = slot && generationForRender(slot, item);
      const stages = generation?.stages ?? (slot && stagesForRender(slot, item));
      // Render history outlives bounded generation-stage retention. Its own
      // factual render outcome remains safe to show when the stages are gone.
      const presentation = presentTraceOverlay(
        stages ?? EMPTY_STAGES,
        item,
        generation?.diagnostics
      );
      if (!matchesFilter(filter, slot, item, stages ?? EMPTY_STAGES, presentation.className)) {
        continue;
      }
      const row = document.createElement('div');
      row.className = `row ${presentation.className}`;
      const title = document.createElement('strong');
      const requestNumber = generation?.diagnostics?.requestNumber;
      title.textContent = requestNumber
        ? `${item.slotId} · ${requestNumber === 1 ? 'Initial request' : `Refresh ${requestNumber - 1}`}`
        : item.slotId;
      const summary = document.createElement('div');
      summary.textContent = presentation.primaryStatus ?? 'No trace result observed';
      row.append(title, summary);
      const secondaryFacts = presentation.facts.filter(
        (fact) => fact !== presentation.primaryStatus
      );
      if (secondaryFacts.length > 0) {
        const facts = document.createElement('div');
        facts.className = 'facts';
        facts.textContent = secondaryFacts.join('\n');
        row.appendChild(facts);
      }
      row.addEventListener('click', () => {
        details.hidden = false;
        details.textContent = JSON.stringify(
          { render: item, stages, diagnostics: generation?.diagnostics },
          null,
          2
        );
      });
      rows.appendChild(row);
    }

    for (const [slotId, slot] of slotById) {
      const item = latestRenderForSlot(exported.renders, slot);
      const generation = item && generationForRender(slot, item);
      const element = item ? window.tsjs?.getAdTraceElement?.(slotId, item.generation) : undefined;
      if (!element || !item) continue;
      const rect = element.getBoundingClientRect();
      const visibility = effectiveVisibility(element, rect);
      window.tsjs?.updateAdTraceVisibility?.(slotId, item.generation, visibility);
      const effectiveItem = visibility === item.visibility ? item : { ...item, visibility };
      if (visibility === 'disconnected') {
        resizeObserver?.unobserve(element);
        removeTraceAttributes(element);
        continue;
      }
      nextObserved.add(element);
      if (!observedElements.has(element)) resizeObserver?.observe(element);
      stampRender(element, effectiveItem);
      const presentation = presentTraceOverlay(
        generation?.stages ?? slot.stages,
        effectiveItem,
        generation?.diagnostics
      );
      if (
        visibility !== 'visible' ||
        !isRectInViewport(rect) ||
        !presentation.badgeStatus ||
        !matchesFilter(
          filter,
          slot,
          effectiveItem,
          generation?.stages ?? slot.stages,
          presentation.className
        )
      ) {
        continue;
      }
      const key = `${slotId}:${item.generation}`;
      const badge = document.createElement('div');
      badge.className = `badge ${presentation.className}`;
      badge.textContent = presentation.badgeStatus;
      badge.style.visibility = 'hidden';
      badge.addEventListener('click', () => {
        panel.hidden = false;
        details.hidden = false;
        details.textContent = JSON.stringify(
          {
            render: effectiveItem,
            stages: generation?.stages ?? slot.stages,
            diagnostics: generation?.diagnostics,
          },
          null,
          2
        );
      });
      badgeLayer.appendChild(badge);
      const measured = badge.getBoundingClientRect();
      badgeInputs.push({
        key,
        left: rect.left,
        top: rect.top,
        width: measured.width || Math.min(300, Math.max(120, presentation.badgeStatus.length * 7)),
        height: measured.height || 30,
      });
      badgeElements.set(key, badge);
    }
    const placements = placeTraceBadges(badgeInputs, window.innerWidth, window.innerHeight);
    const placedKeys = new Set(placements.map((placement) => placement.key));
    for (const placement of placements) {
      const badge = badgeElements.get(placement.key);
      if (!badge) continue;
      badge.style.left = `${placement.left}px`;
      badge.style.top = `${placement.top}px`;
      badge.style.visibility = 'visible';
    }
    for (const [key, badge] of badgeElements) {
      if (!placedKeys.has(key)) badge.remove();
    }
    for (const element of observedElements) {
      if (!nextObserved.has(element)) {
        resizeObserver?.unobserve(element);
        removeTraceAttributes(element);
      }
    }
    observedElements = nextObserved;
  };

  let framePending = false;
  const schedule = (): void => {
    if (framePending) return;
    framePending = true;
    requestAnimationFrame(() => {
      framePending = false;
      if (host.isConnected) render();
    });
  };
  const unsubscribe = subscribe(schedule);
  let cleaned = false;
  cleanup = (): void => {
    if (cleaned) return;
    cleaned = true;
    unsubscribe();
    resizeObserver?.disconnect();
    for (const element of observedElements) removeTraceAttributes(element);
    window.removeEventListener('scroll', schedule);
    window.removeEventListener('resize', schedule);
    lifecycleObserver.disconnect();
  };
  const lifecycleObserver = new MutationObserver(() => {
    if (!host.isConnected) cleanup();
  });
  lifecycleObserver.observe(document.documentElement, { childList: true, subtree: true });
  window.addEventListener('scroll', schedule, { passive: true });
  window.addEventListener('resize', schedule, { passive: true });
  render();
}
