import { log } from '../../core/log';
import type { GptDiagnosticsApi, GptDiagnosticsBinding, RenderTraceRecord } from '../../core/types';
import { EMBEDDED_RELEASE_ID } from '../../core/release';
import type {
  RenderTracePresentationControls,
  RenderTracePresentationFactory,
  RenderTracePresentationSource,
} from '../../core/trace';
import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  IntegrationRegistration,
} from '../../kernel/integration_registry';
import { realmOwnedDocument } from '../../shared/realm';

import { GptDiagnosticsBadgeManager } from './badges';
import { GptDiagnosticsBindingManager } from './binding';
import type {
  GptDiagnosticsPresentationControls,
  GptDiagnosticsPresentationFactory,
  GptDiagnosticsPresentationSource,
} from './data_api';
import { GptDiagnosticsOverlay } from './overlay';

type GptDiagnosticsWindow = Window & typeof globalThis;

/** DOM id of the deferred diagnostics-owned render-trace panel. */
export const TRACE_PANEL_ID = 'ts-render-trace-panel';

/** CSS class of the deferred diagnostics-owned per-slot badge. */
export const TRACE_BADGE_CLASS = 'ts-render-badge';

type TracePanelStatus = 'ok' | 'hidden' | 'gam-only' | 'empty';

const TRACE_STATUS_STYLE: Record<TracePanelStatus, { color: string; mark: string; label: string }> =
  {
    ok: { color: '#3fb950', mark: '✓', label: 'ok' },
    hidden: { color: '#d29922', mark: '⚠', label: 'hidden' },
    'gam-only': { color: '#58a6ff', mark: '◐', label: 'gam-only' },
    empty: { color: '#f85149', mark: '✗', label: 'empty' },
  };

const RUNTIME_TRACE_ATTRIBUTES = [
  'data-ts-slot-id',
  'data-ts-render-path',
  'data-ts-rendered',
  'data-ts-auction-id',
  'data-ts-bidder',
  'data-ts-ad-id',
  'data-ts-bid-id',
  'data-ts-creative-id',
  'data-ts-adm-hash',
  'data-ts-served-from',
  'data-ts-gam-empty',
  'data-ts-injected',
  'data-ts-visible',
] as const;

interface PresentedTraceSlot {
  readonly element: HTMLElement;
  readonly priorInlinePosition?: string;
}

interface RenderTracePresentationOptions {
  readonly document: Document;
  readonly exportRecord?: ((record: Readonly<RenderTraceRecord>) => void) | undefined;
  readonly onError?: ((error: unknown) => void) | undefined;
}

interface TracePresentationCapability {
  readonly attachPresentation: (factory: RenderTracePresentationFactory) => () => void;
}

function tracePanelStatus(record: Readonly<RenderTraceRecord>): TracePanelStatus {
  if (!record.rendered || record.gamEmpty === true) return 'empty';
  if (record.visible !== true) return 'hidden';
  return record.injected === true ? 'ok' : 'gam-only';
}

function copiedTraceRecord(record: Readonly<RenderTraceRecord>): Readonly<RenderTraceRecord> {
  return Object.freeze({ ...record });
}

/** Build the complete render-trace DOM/export subscriber owned by the deferred artifact. */
export function createRenderTracePresentation(
  source: RenderTracePresentationSource,
  options: RenderTracePresentationOptions
): RenderTracePresentationControls {
  const targetDocument = options.document;
  const presented = new Map<string, PresentedTraceSlot>();
  const panelRecords = new Map<number, Readonly<RenderTraceRecord>>();
  const panelRows = new Map<number, HTMLButtonElement>();
  let panel: HTMLElement | undefined;
  let panelHeading: HTMLElement | undefined;
  let panelRowsHost: HTMLElement | undefined;
  let unsubscribe: (() => void) | undefined;
  let disposed = false;

  const report = (error: unknown): void => {
    try {
      options.onError?.(error);
    } catch {
      // Deferred diagnostics reporting cannot affect trace data or ads.
    }
  };
  const removeBadge = (element: HTMLElement): void => {
    for (const badge of element.querySelectorAll(`:scope > .${TRACE_BADGE_CLASS}`)) {
      badge.remove();
    }
  };
  const clearElement = (presentedSlot: PresentedTraceSlot): void => {
    const { element, priorInlinePosition } = presentedSlot;
    for (const attribute of RUNTIME_TRACE_ATTRIBUTES) element.removeAttribute(attribute);
    removeBadge(element);
    if (priorInlinePosition !== undefined && element.style.position === 'relative') {
      element.style.position = priorInlinePosition;
    }
  };
  const createBadge = (
    element: HTMLElement,
    record: Readonly<RenderTraceRecord>
  ): PresentedTraceSlot => {
    let priorInlinePosition: string | undefined;
    try {
      const position = targetDocument.defaultView?.getComputedStyle(element).position;
      if (position === 'static' || position === '') {
        priorInlinePosition = element.style.position;
        element.style.position = 'relative';
      }
    } catch {
      // A badge remains noninteractive even if its containing block is hostile.
    }
    const style = TRACE_STATUS_STYLE[tracePanelStatus(record)];
    const badge = targetDocument.createElement('div');
    badge.className = TRACE_BADGE_CLASS;
    badge.textContent =
      `TS ${style.mark} #${record.seq}` +
      `${record.bidder ? ` · ${record.bidder}` : ''}` +
      `${style.label === 'ok' ? '' : ` · ${style.label}`}`;
    badge.style.setProperty('position', 'absolute');
    badge.style.setProperty('top', '4px');
    badge.style.setProperty('left', '4px');
    badge.style.setProperty('z-index', '2147483646');
    badge.style.setProperty('pointer-events', 'none');
    badge.style.setProperty('font', '10px/1.5 ui-monospace, Menlo, Consolas, monospace');
    badge.style.setProperty('padding', '1px 5px');
    badge.style.setProperty('color', '#fff');
    badge.style.setProperty('background', style.color);
    badge.style.setProperty('border-radius', '3px');
    element.appendChild(badge);
    return { element, ...(priorInlinePosition === undefined ? {} : { priorInlinePosition }) };
  };
  const exportRow = (record: Readonly<RenderTraceRecord>): void => {
    const copied = copiedTraceRecord(record);
    try {
      if (options.exportRecord) {
        options.exportRecord(copied);
        return;
      }
      const clipboard = targetDocument.defaultView?.navigator.clipboard;
      const write = clipboard?.writeText;
      if (typeof write !== 'function') return;
      const pending = Reflect.apply(write, clipboard, [JSON.stringify(copied, null, 2)]) as
        Promise<void> | undefined;
      void pending?.catch(report);
    } catch (error) {
      report(error);
    }
  };
  const presentRecord = (record: Readonly<RenderTraceRecord>): void => {
    const prior = presented.get(record.slotId);
    const elementId = record.elementId ?? record.slotId;
    const candidate = targetDocument.getElementById(elementId);
    const ElementConstructor = targetDocument.defaultView?.HTMLElement;
    const element =
      typeof ElementConstructor === 'function' && candidate instanceof ElementConstructor
        ? (candidate as HTMLElement)
        : undefined;
    if (prior && prior.element !== element) {
      clearElement(prior);
      presented.delete(record.slotId);
    }
    if (!element) return;
    const retainedPosition = prior?.element === element ? prior.priorInlinePosition : undefined;
    removeBadge(element);
    const values: Readonly<Record<(typeof RUNTIME_TRACE_ATTRIBUTES)[number], string | undefined>> =
      {
        'data-ts-slot-id': record.slotId,
        'data-ts-render-path': record.path,
        'data-ts-rendered': String(record.rendered),
        'data-ts-auction-id': record.auctionId,
        'data-ts-bidder': record.bidder,
        'data-ts-ad-id': record.adId,
        'data-ts-bid-id': record.bidId,
        'data-ts-creative-id': record.creativeId,
        'data-ts-adm-hash': record.admHash,
        'data-ts-served-from': record.servedFrom,
        'data-ts-gam-empty': record.gamEmpty === undefined ? undefined : String(record.gamEmpty),
        'data-ts-injected': record.injected === undefined ? undefined : String(record.injected),
        'data-ts-visible': record.visible === undefined ? undefined : String(record.visible),
      };
    for (const attribute of RUNTIME_TRACE_ATTRIBUTES) {
      const value = values[attribute];
      if (value === undefined || value === '') element.removeAttribute(attribute);
      else element.setAttribute(attribute, value);
    }
    const status = tracePanelStatus(record);
    if (element.tagName !== 'IFRAME' && (status === 'ok' || status === 'gam-only')) {
      const next = createBadge(element, record);
      presented.set(record.slotId, {
        element,
        ...(retainedPosition === undefined
          ? next.priorInlinePosition === undefined
            ? {}
            : { priorInlinePosition: next.priorInlinePosition }
          : { priorInlinePosition: retainedPosition }),
      });
    } else {
      if (retainedPosition !== undefined && element.style.position === 'relative') {
        element.style.position = retainedPosition;
      }
      presented.set(record.slotId, { element });
    }
  };
  const ensurePanel = (): boolean => {
    if (!targetDocument.body) return false;
    if (panel) return true;
    if (targetDocument.getElementById(TRACE_PANEL_ID)) return false;
    panel = targetDocument.createElement('div');
    panel.id = TRACE_PANEL_ID;
    panel.setAttribute('data-ts-render-trace-owner', '1');
    panel.style.setProperty('position', 'fixed');
    panel.style.setProperty('bottom', '12px');
    panel.style.setProperty('right', '12px');
    panel.style.setProperty('z-index', '2147483647');
    panel.style.setProperty('max-width', '360px');
    panel.style.setProperty('max-height', '45vh');
    panel.style.setProperty('overflow', 'auto');
    panel.style.setProperty('background', 'rgba(17,17,17,0.94)');
    panel.style.setProperty('color', '#eee');
    panel.style.setProperty('font', '11px/1.5 ui-monospace, Menlo, Consolas, monospace');
    panel.style.setProperty('border', '1px solid #333');
    panel.style.setProperty('border-radius', '6px');
    panel.style.setProperty('box-shadow', '0 4px 16px rgba(0,0,0,0.4)');
    panelHeading = targetDocument.createElement('div');
    panelHeading.style.setProperty('padding', '6px 10px');
    panelHeading.style.setProperty('font-weight', '700');
    panelRowsHost = targetDocument.createElement('div');
    panel.append(panelHeading, panelRowsHost);
    targetDocument.body.appendChild(panel);
    return true;
  };
  const renderPanel = (history: readonly Readonly<RenderTraceRecord>[]): void => {
    if (!ensurePanel()) return;
    panelHeading!.textContent = `TS Render Trace · ${history.length} renders`;
    const retainedSequences = new Set(history.map(({ seq }) => seq));
    for (const [sequence, row] of panelRows) {
      if (retainedSequences.has(sequence)) continue;
      row.remove();
      panelRows.delete(sequence);
      panelRecords.delete(sequence);
    }
    for (const record of history) {
      panelRecords.set(record.seq, record);
      let row = panelRows.get(record.seq);
      if (!row) {
        row = targetDocument.createElement('button');
        row.type = 'button';
        row.setAttribute('data-ts-trace-seq', String(record.seq));
        row.style.setProperty('display', 'block');
        row.style.setProperty('width', '100%');
        row.style.setProperty('padding', '6px 10px');
        row.style.setProperty('border', '0');
        row.style.setProperty('border-top', '1px solid #2a2a2a');
        row.style.setProperty('background', 'transparent');
        row.style.setProperty('font', 'inherit');
        row.style.setProperty('text-align', 'left');
        row.style.setProperty('cursor', 'pointer');
        const sequence = record.seq;
        row.addEventListener('click', () => {
          const exported = panelRecords.get(sequence);
          if (exported) exportRow(exported);
        });
        panelRows.set(record.seq, row);
        panelRowsHost!.prepend(row);
      }
      const style = TRACE_STATUS_STYLE[tracePanelStatus(record)];
      row.textContent = `#${record.seq} ${style.mark} ${record.slotId} · ${style.label} · ${record.path}`;
      row.style.setProperty('border-left', `3px solid ${style.color}`);
      row.style.setProperty('color', style.color);
    }
  };
  const refresh = (): void => {
    if (disposed) return;
    try {
      const current = source.current();
      const history = source.history();
      const currentIds = new Set(Object.keys(current));
      for (const [slotId, existing] of presented) {
        if (currentIds.has(slotId)) continue;
        clearElement(existing);
        presented.delete(slotId);
      }
      for (const record of Object.values(current)) presentRecord(record);
      renderPanel(history);
    } catch (error) {
      report(error);
    }
  };
  const cleanup = (): void => {
    if (disposed) return;
    disposed = true;
    isolate(() => unsubscribe?.());
    unsubscribe = undefined;
    for (const presentedSlot of presented.values()) isolate(() => clearElement(presentedSlot));
    presented.clear();
    isolate(() => panel?.remove());
    panel = undefined;
    panelHeading = undefined;
    panelRowsHost = undefined;
    panelRecords.clear();
    panelRows.clear();
  };

  try {
    unsubscribe = source.subscribe(refresh);
    if (typeof unsubscribe !== 'function') {
      throw new TypeError('render trace presentation disposer is unavailable');
    }
    refresh();
    return Object.freeze({ dispose: cleanup });
  } catch (error) {
    cleanup();
    throw error;
  }
}

interface GptDiagnosticsDataCapability {
  readonly api: GptDiagnosticsApi;
  readonly attachPresentation: (factory: GptDiagnosticsPresentationFactory) => () => void;
}

function isolate(callback: () => void): void {
  try {
    callback();
  } catch {
    // Diagnostics cleanup cannot retain another independently owned resource.
  }
}

function dataCapability(
  interfaces: Readonly<Record<string, unknown>>
): GptDiagnosticsDataCapability | undefined {
  try {
    const candidate = interfaces['gpt_diag.v1'];
    if (
      typeof candidate !== 'object' ||
      candidate === null ||
      !Object.isFrozen(candidate) ||
      Reflect.ownKeys(candidate).sort().join(',') !== 'api,attachPresentation'
    ) {
      return undefined;
    }
    const fields = candidate as Readonly<Record<string, unknown>>;
    return typeof fields['attachPresentation'] === 'function' &&
      typeof fields['api'] === 'object' &&
      fields['api'] !== null &&
      Object.isFrozen(fields['api'])
      ? (candidate as GptDiagnosticsDataCapability)
      : undefined;
  } catch {
    return undefined;
  }
}

function renderTraceCapability(
  interfaces: Readonly<Record<string, unknown>>
): TracePresentationCapability | undefined {
  try {
    const capability = Object.getOwnPropertyDescriptor(interfaces, 'trace.presentation.v1');
    if (!capability || !('value' in capability)) return undefined;
    const candidate = capability.value;
    if (
      typeof candidate !== 'object' ||
      candidate === null ||
      Array.isArray(candidate) ||
      !Object.isFrozen(candidate) ||
      Object.getPrototypeOf(candidate) !== Object.prototype ||
      Reflect.ownKeys(candidate).length !== 1
    ) {
      return undefined;
    }
    const attach = Object.getOwnPropertyDescriptor(candidate, 'attachPresentation');
    return attach?.enumerable && 'value' in attach && typeof attach.value === 'function'
      ? (candidate as TracePresentationCapability)
      : undefined;
  } catch {
    return undefined;
  }
}

function diagnosticsSelection(runtime: Readonly<Record<string, unknown>>):
  | Readonly<{
      gptActive: boolean;
      renderTraceOverlay: boolean;
    }>
  | undefined {
  try {
    const bootDescriptor = Object.getOwnPropertyDescriptor(runtime, 'boot');
    if (
      !bootDescriptor ||
      !('value' in bootDescriptor) ||
      typeof bootDescriptor.value !== 'function'
    ) {
      return undefined;
    }
    const boot = Reflect.apply(bootDescriptor.value, runtime, []);
    if (typeof boot !== 'object' || boot === null || !Object.isFrozen(boot)) return undefined;
    const diagnosticsDescriptor = Object.getOwnPropertyDescriptor(boot, 'diagnostics');
    if (!diagnosticsDescriptor || !('value' in diagnosticsDescriptor)) return undefined;
    const diagnostics = diagnosticsDescriptor.value;
    if (
      typeof diagnostics !== 'object' ||
      diagnostics === null ||
      !Object.isFrozen(diagnostics) ||
      Reflect.ownKeys(diagnostics).sort().join(',') !== 'gpt,renderTraceOverlay,version'
    ) {
      return undefined;
    }
    const fields = diagnostics as Readonly<Record<string, unknown>>;
    const gpt = fields['gpt'];
    if (
      fields['version'] !== 1 ||
      typeof fields['renderTraceOverlay'] !== 'boolean' ||
      typeof gpt !== 'object' ||
      gpt === null ||
      !Object.isFrozen(gpt) ||
      Reflect.ownKeys(gpt).join(',') !== 'active' ||
      typeof (gpt as Readonly<Record<string, unknown>>)['active'] !== 'boolean'
    ) {
      return undefined;
    }
    return Object.freeze({
      gptActive: (gpt as Readonly<Record<string, unknown>>)['active'] as boolean,
      renderTraceOverlay: fields['renderTraceOverlay'] as boolean,
    });
  } catch {
    return undefined;
  }
}

function downloadSnapshot(
  api: GptDiagnosticsApi,
  targetWindow: GptDiagnosticsWindow,
  targetDocument: Document
): void {
  const snapshot = api.snapshot();
  const objectUrl = targetWindow.URL.createObjectURL(
    new targetWindow.Blob([JSON.stringify(snapshot, null, 2)], { type: 'application/json' })
  );
  const anchor = targetDocument.createElement('a');
  anchor.href = objectUrl;
  anchor.download = `trusted-server-gpt-diagnostics-${snapshot.capturedAt.replace(/[:.]/g, '-')}.json`;
  anchor.hidden = true;
  targetDocument.body?.append(anchor);
  try {
    anchor.click();
  } finally {
    anchor.remove();
    targetWindow.URL.revokeObjectURL(objectUrl);
  }
}

function createPresentationControls(
  source: GptDiagnosticsPresentationSource,
  api: GptDiagnosticsApi,
  targetWindow: GptDiagnosticsWindow,
  targetDocument: Document
): GptDiagnosticsPresentationControls {
  const bindings = new GptDiagnosticsBindingManager(source, {
    window: targetWindow,
    document: targetDocument,
  });
  let badges: GptDiagnosticsBadgeManager | undefined;
  let overlay: GptDiagnosticsOverlay | undefined;
  const cleanup = (): void => {
    isolate(() => overlay?.destroy());
    isolate(() => badges?.destroy());
    isolate(() => bindings.destroy());
  };
  try {
    badges = new GptDiagnosticsBadgeManager(source, bindings, {
      window: targetWindow,
      document: targetDocument,
    });
    overlay = new GptDiagnosticsOverlay(source, bindings, {
      window: targetWindow,
      document: targetDocument,
      onExport: () => api.export(),
      onBadgeLayerChange: (layer) => badges?.setLayer(layer),
    });
    const controls = Object.freeze({
      dispose: cleanup,
      download: () => downloadSnapshot(api, targetWindow, targetDocument),
      exportBinding: (runtimeSlotNumber: number): GptDiagnosticsBinding =>
        bindings.exportBinding(runtimeSlotNumber),
      hide: () => overlay?.hide(),
      show: () => overlay?.show(),
      subscribe: (listener: () => void) => bindings.subscribe(listener),
    });
    return controls;
  } catch (error) {
    cleanup();
    throw error;
  }
}

/** Build the deferred diagnostics presentation registration. */
export function createDiagnosticsPresentationIntegrationRegistration(
  releaseId: string
): IntegrationRegistration {
  return Object.freeze({
    abi: 1,
    id: 'diagnostics_presentation',
    phase: 'deferred',
    releaseId,
    prepare: ({ config, interfaces }: IntegrationPrepareContext) => {
      const runtime = interfaces['runtime.v1'] as Readonly<Record<string, unknown>> | undefined;
      const targetDocument = realmOwnedDocument(runtime?.['document']);
      const targetWindow = targetDocument?.defaultView;
      const trace = runtime ? renderTraceCapability(interfaces) : undefined;
      const selection = runtime ? diagnosticsSelection(runtime) : undefined;
      if (
        config !== undefined ||
        !runtime ||
        !Object.isFrozen(runtime) ||
        !targetDocument ||
        !targetWindow ||
        !trace ||
        !selection
      ) {
        throw new TypeError('diagnostics presentation capability graph is malformed');
      }
      const data = dataCapability(interfaces);
      if (
        selection.gptActive !== Boolean(data) ||
        (!selection.renderTraceOverlay && !selection.gptActive)
      ) {
        throw new TypeError('diagnostics presentation activation graph is malformed');
      }
      return Object.freeze({
        activate: ({ onDispose }: IntegrationActivationContext) => {
          const traceOwnership: { release?: () => void } = {};
          const gptOwnership: { release?: () => void } = {};
          onDispose(() => isolate(() => traceOwnership.release?.()));
          onDispose(() => isolate(() => gptOwnership.release?.()));
          if (selection.renderTraceOverlay) {
            const release = trace.attachPresentation((source) =>
              createRenderTracePresentation(source, {
                document: targetDocument,
                onError: (error) => log.warn('render trace presentation failed', error),
              })
            );
            if (typeof release !== 'function') {
              throw new TypeError('render trace presentation disposer is unavailable');
            }
            traceOwnership.release = release;
          }
          if (data) {
            const release = data.attachPresentation((source, api) =>
              createPresentationControls(source, api, targetWindow, targetDocument)
            );
            if (typeof release !== 'function') {
              throw new TypeError('GPT diagnostics presentation disposer is unavailable');
            }
            gptOwnership.release = release;
          }
        },
      });
    },
  });
}

if (typeof window !== 'undefined') {
  const register = (window.tsjs as unknown as { _registerIntegration?: unknown } | undefined)
    ?._registerIntegration;
  if (typeof register === 'function') {
    Reflect.apply(register, window.tsjs, [
      createDiagnosticsPresentationIntegrationRegistration(EMBEDDED_RELEASE_ID),
    ]);
  }
}
