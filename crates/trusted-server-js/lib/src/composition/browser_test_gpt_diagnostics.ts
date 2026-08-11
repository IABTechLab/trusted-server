import type { GptDiagnosticsApi } from '../core/types';
import { GptDiagnosticsApiController } from '../integrations/gpt_diagnostics/api';
import { GptDiagnosticsBadgeManager } from '../integrations/gpt_diagnostics/badges';
import { GptDiagnosticsBindingManager } from '../integrations/gpt_diagnostics/binding';
import type { GptDiagnosticsFactBuffer } from '../integrations/gpt/diagnostics_facts';
import { GptDiagnosticsObserver } from '../integrations/gpt_diagnostics/observer';
import { GptDiagnosticsOverlay } from '../integrations/gpt_diagnostics/overlay';
import { GptDiagnosticsStore } from '../integrations/gpt_diagnostics/store';

type GptDiagnosticsWindow = Window & typeof globalThis;

export interface GptDiagnosticsRuntimeOptions {
  readonly document?: Document | undefined;
  readonly window?: GptDiagnosticsWindow | undefined;
}

export interface GptDiagnosticsRuntime {
  readonly activate: () => () => void;
  readonly currentApi: () => GptDiagnosticsApi | undefined;
}

function isolate(callback: () => void): void {
  try {
    callback();
  } catch {
    // Test-composition cleanup cannot retain another independently owned resource.
  }
}

/** Legacy-composition harness excluded from every production artifact entry. */
export function createGptDiagnosticsRuntime(
  facts: Pick<GptDiagnosticsFactBuffer, 'activate'>,
  options: GptDiagnosticsRuntimeOptions = {}
): GptDiagnosticsRuntime {
  const targetWindow = options.window ?? (window as GptDiagnosticsWindow);
  const targetDocument = options.document ?? document;
  let active: Readonly<{ api: GptDiagnosticsApi; release: () => void }> | undefined;
  return Object.freeze({
    activate: (): (() => void) => {
      if (active) throw new Error('GPT diagnostics runtime is already active');
      const store = new GptDiagnosticsStore();
      const observer = new GptDiagnosticsObserver(store);
      let releaseFacts: (() => void) | undefined;
      let bindings: GptDiagnosticsBindingManager | undefined;
      let badges: GptDiagnosticsBadgeManager | undefined;
      let overlay: GptDiagnosticsOverlay | undefined;
      let apiController: GptDiagnosticsApiController | undefined;
      const cleanup = (): void => {
        isolate(() => releaseFacts?.());
        isolate(() => apiController?.destroy());
        isolate(() => overlay?.destroy());
        isolate(() => badges?.destroy());
        isolate(() => bindings?.destroy());
      };
      try {
        observer.start();
        releaseFacts = facts.activate((fact) => observer.consume(fact));
        if (!releaseFacts) throw new Error('GPT diagnostics fact consumer is unavailable');
        bindings = new GptDiagnosticsBindingManager(store, {
          window: targetWindow,
          document: targetDocument,
        });
        badges = new GptDiagnosticsBadgeManager(store, bindings, {
          window: targetWindow,
          document: targetDocument,
        });
        overlay = new GptDiagnosticsOverlay(store, bindings, {
          window: targetWindow,
          document: targetDocument,
          onExport: () => apiController?.api.export(),
          onBadgeLayerChange: (layer) => badges?.setLayer(layer),
        });
        apiController = new GptDiagnosticsApiController(store, bindings, overlay, {
          window: targetWindow,
          document: targetDocument,
        });
      } catch (error) {
        cleanup();
        throw error;
      }
      const api = apiController.api;
      let released = false;
      const release = (): void => {
        if (released) return;
        released = true;
        if (active?.release === release) active = undefined;
        cleanup();
      };
      active = Object.freeze({ api, release });
      return release;
    },
    currentApi: (): GptDiagnosticsApi | undefined => active?.api,
  });
}
