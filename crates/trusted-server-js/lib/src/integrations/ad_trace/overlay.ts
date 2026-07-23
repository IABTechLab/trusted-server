import type {
  AdTraceApi,
  RenderTraceSnapshot,
  RenderTraceVisibility,
  SlotTraceSnapshot,
} from '../../core/types';

const HOST_ID = 'ts-ad-trace-overlay';
const TRACE_ATTRIBUTES = [
  'data-ts-trace-seq',
  'data-ts-trace-generation',
  'data-ts-auction-trace-id',
  'data-ts-bid-trace-id',
  'data-ts-trace-outcome',
  'data-ts-trace-visibility',
] as const;

function stageLine(label: string, stage: { outcome: string; confidence: string }): string {
  return `${label}: ${stage.outcome} · ${stage.confidence}`;
}

function badgeText(slot: SlotTraceSnapshot, render?: RenderTraceSnapshot): string {
  return [
    render ? `#${render.sequence}: ${render.outcome} · ${render.visibility}` : undefined,
    stageLine('TS winner', slot.stages.trustedServer),
    stageLine('Prebid winner', slot.stages.prebid),
    stageLine('GAM result', slot.stages.gam),
    stageLine('Creative', slot.stages.creative),
  ]
    .filter(Boolean)
    .join('\n');
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
      color: #eefbf4; font: 11px/1.35 ui-monospace, monospace; white-space: pre; cursor: pointer; }
    .badge.probable { border-color: #67a8ff; }
    .panel { position: fixed; right: 12px; bottom: 12px; z-index: 2147483647; width: 460px;
      max-height: 60vh; overflow: auto; padding: 10px; background: #0a1210; color: #eefbf4;
      border: 1px solid #72e0a6; font: 11px/1.4 ui-monospace, monospace; }
    .controls { display: flex; gap: 6px; position: sticky; top: 0; background: #0a1210; }
    .warning { color: #ffd479; margin: 6px 0; }
    .row { border-top: 1px solid #29443a; padding: 6px 0; }
    .row strong { color: #72e0a6; }
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
  const warning = document.createElement('div');
  warning.className = 'warning';
  warning.textContent = 'A non-empty GAM response alone is not proof of a Trusted Server creative.';
  const rows = document.createElement('div');
  const details = document.createElement('pre');
  details.hidden = true;
  controls.append(collapseButton, exportButton, closeButton);
  panel.append(controls, warning, rows, details);
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
    warning.hidden = rows.hidden;
    collapseButton.textContent = rows.hidden ? 'Expand' : 'Collapse';
  });
  closeButton.addEventListener('click', () => {
    cleanup();
    host.remove();
  });

  let observedElements = new Set<HTMLElement>();
  const resizeObserver =
    typeof ResizeObserver === 'undefined' ? undefined : new ResizeObserver(() => schedule());

  const render = (): void => {
    badgeLayer.replaceChildren();
    rows.replaceChildren();
    const exported = api.export();
    const slotById = new Map(exported.slots.map((slot) => [slot.slotId, slot]));
    const latestBySlot = new Map<string, RenderTraceSnapshot>();
    for (const item of exported.renders) latestBySlot.set(item.slotId, item);
    const nextObserved = new Set<HTMLElement>();

    for (const item of [...exported.renders].reverse()) {
      const row = document.createElement('div');
      row.className = 'row';
      const title = document.createElement('strong');
      title.textContent = `#${item.sequence} ${item.slotId} · ${item.source}`;
      const summary = document.createElement('div');
      summary.textContent = `${item.outcome} · ${item.confidence} · ${item.visibility}`;
      row.append(title, summary);
      row.addEventListener('click', () => {
        details.hidden = false;
        details.textContent = JSON.stringify(
          { render: item, stages: slotById.get(item.slotId)?.stages },
          null,
          2
        );
      });
      rows.appendChild(row);
    }

    for (const [slotId, slot] of slotById) {
      const item = latestBySlot.get(slotId);
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
      const badge = document.createElement('div');
      badge.className = `badge ${item.outcome === 'confirmed' ? '' : 'probable'}`;
      badge.textContent = badgeText(slot, effectiveItem);
      badge.style.left = `${Math.max(0, rect.left)}px`;
      badge.style.top = `${Math.max(0, rect.top)}px`;
      badge.addEventListener('click', () => {
        panel.hidden = false;
        details.hidden = false;
        details.textContent = JSON.stringify(
          { render: effectiveItem, stages: slot.stages },
          null,
          2
        );
      });
      badgeLayer.appendChild(badge);
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
