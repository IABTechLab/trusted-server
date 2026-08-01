import { log } from '../../core/log';
import type { GptDiagnosticsApi, TsjsApi } from '../../core/types';

import { GptDiagnosticsApiController } from './api';
import { GptDiagnosticsBadgeManager } from './badges';
import { GptDiagnosticsBindingManager } from './binding';
import { GptDiagnosticsObserver } from './observer';
import type { GptObserverWindow } from './observer';
import { GptDiagnosticsOverlay } from './overlay';
import { GptDiagnosticsStore } from './store';

interface GptDiagnosticsRuntime {
  api: GptDiagnosticsApi;
  destroy(): void;
}

type GptDiagnosticsWindow = Window &
  typeof globalThis &
  GptObserverWindow & {
    __tsjs_gpt_diagnostics_active?: boolean;
    __tsjs_gpt_diagnostics_runtime?: GptDiagnosticsRuntime;
    tsjs?: TsjsApi;
  };

/** Whether the early bootstrap activated diagnostics for this document. */
export function isGptDiagnosticsActive(
  target: Pick<
    GptDiagnosticsWindow,
    '__tsjs_gpt_diagnostics_active'
  > = window as GptDiagnosticsWindow
): boolean {
  return target.__tsjs_gpt_diagnostics_active === true;
}

/** Installs one active diagnostics runtime for the current document. */
export function installGptDiagnosticsRuntime(
  target: GptDiagnosticsWindow = window as GptDiagnosticsWindow
): GptDiagnosticsApi | undefined {
  if (!isGptDiagnosticsActive(target)) return undefined;
  if (target.__tsjs_gpt_diagnostics_runtime) {
    return target.__tsjs_gpt_diagnostics_runtime.api;
  }

  let bindings: GptDiagnosticsBindingManager | undefined;
  let badges: GptDiagnosticsBadgeManager | undefined;
  let overlay: GptDiagnosticsOverlay | undefined;
  let apiController: GptDiagnosticsApiController | undefined;

  try {
    if (!target.tsjs) throw new Error('TSJS core API unavailable');

    const store = new GptDiagnosticsStore();
    const observer = new GptDiagnosticsObserver(store, { window: target });
    bindings = new GptDiagnosticsBindingManager(store, {
      window: target,
      document: target.document,
    });
    badges = new GptDiagnosticsBadgeManager(store, bindings, {
      window: target,
      document: target.document,
    });
    overlay = new GptDiagnosticsOverlay(store, bindings, {
      window: target,
      document: target.document,
      onExport: () => apiController?.api.export(),
      onBadgeLayerChange: (layer) => badges?.setLayer(layer),
    });
    apiController = new GptDiagnosticsApiController(store, bindings, overlay, {
      window: target,
      document: target.document,
    });

    observer.install();
    const api = apiController.api;
    const runtime: GptDiagnosticsRuntime = {
      api,
      destroy: () => {
        if (target.tsjs?.gptDiagnostics === api) delete target.tsjs.gptDiagnostics;
        apiController?.destroy();
        overlay?.destroy();
        badges?.destroy();
        bindings?.destroy();
        delete target.__tsjs_gpt_diagnostics_runtime;
      },
    };
    target.tsjs.gptDiagnostics = api;
    target.__tsjs_gpt_diagnostics_runtime = runtime;
    return api;
  } catch (error) {
    apiController?.destroy();
    overlay?.destroy();
    badges?.destroy();
    bindings?.destroy();
    log.warn('gpt diagnostics: runtime installation failed', error);
    return undefined;
  }
}

if (typeof window !== 'undefined' && isGptDiagnosticsActive(window as GptDiagnosticsWindow)) {
  installGptDiagnosticsRuntime(window as GptDiagnosticsWindow);
}
