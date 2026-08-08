import type { GptDiagnosticsApi } from '../../core/types';
import { EMBEDDED_RELEASE_ID } from '../../core/release';

import { GptDiagnosticsApiController } from './api';
import { GptDiagnosticsBadgeManager } from './badges';
import { GptDiagnosticsBindingManager } from './binding';
import type { GptDiagnosticsFactBuffer } from './facts';
import { GptDiagnosticsObserver } from './observer';
import { GptDiagnosticsOverlay } from './overlay';
import { GptDiagnosticsStore } from './store';
import { createGptDiagnosticsIntegrationRegistration } from './module';

type GptDiagnosticsWindow = Window & typeof globalThis;

export interface GptDiagnosticsRuntimeOptions {
  readonly document?: Document | undefined;
  readonly window?: GptDiagnosticsWindow | undefined;
}

export interface GptDiagnosticsRuntime {
  readonly activate: () => () => void;
  readonly currentApi: () => GptDiagnosticsApi | undefined;
}

interface ActiveRuntime {
  readonly api: GptDiagnosticsApi;
  readonly release: () => void;
}

function isolate(callback: () => void): void {
  try {
    callback();
  } catch {
    // Diagnostics cleanup cannot retain another independently owned resource.
  }
}

/** Creates an inert GPT diagnostics runtime over the adapter-owned fact transport. */
export function createGptDiagnosticsRuntime(
  facts: Pick<GptDiagnosticsFactBuffer, 'activate'>,
  options: GptDiagnosticsRuntimeOptions = {}
): GptDiagnosticsRuntime {
  const targetWindow = options.window ?? (window as GptDiagnosticsWindow);
  const targetDocument = options.document ?? document;
  let active: ActiveRuntime | undefined;

  const activate = (): (() => void) => {
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
  };

  return Object.freeze({
    activate,
    currentApi: (): GptDiagnosticsApi | undefined => active?.api,
  });
}

if (typeof window !== 'undefined') {
  const register = (window.tsjs as unknown as { _registerIntegration?: unknown } | undefined)
    ?._registerIntegration;
  if (typeof register === 'function') {
    Reflect.apply(register, window.tsjs, [
      createGptDiagnosticsIntegrationRegistration(EMBEDDED_RELEASE_ID),
    ]);
  }
}
