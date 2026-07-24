import { createAdTraceStore } from '../../core/ad_trace';
import type { AdTraceApi, TsjsApi } from '../../core/types';

import { installAdTraceOverlay } from './overlay';

function consumeActiveBootstrap(): boolean {
  if (window.__tsjs_adTraceActive !== true) return false;
  delete window.__tsjs_adTraceActive;
  return true;
}

/** Install the session-scoped recorder, immutable API, and overlay once. */
export function installAdTrace(): boolean {
  if (typeof window === 'undefined') return false;
  if (window.tsjs?.adTrace) return true;
  if (!consumeActiveBootstrap()) return false;
  const ts = (window.tsjs ??= {} as TsjsApi);

  const store = createAdTraceStore();
  const api: AdTraceApi = Object.freeze({
    getSlot: store.getSlot,
    getEvents: store.getEvents,
    getRenderTimeline: store.getRenderTimeline,
    export: store.export,
  });
  ts.adTrace = api;
  ts.recordAdTrace = store.record;
  ts.recordAdTraceCoverage = store.recordCoverage;
  ts.nextAdTraceGeneration = store.nextGeneration;
  ts.subscribeAdTrace = store.subscribe;
  ts.bindAdTraceElement = store.bindElement;
  ts.getAdTraceElement = store.getBoundElement;
  ts.updateAdTraceVisibility = store.updateVisibility;
  if (!ts.captureAdTraceRequest) {
    ts.captureAdTraceRequest = (slot, trigger, snapshot) => {
      const pending = (ts.pendingAdTraceRequests ??= []);
      if (pending.length < 64) pending.push({ slot, trigger, snapshot });
      return 0;
    };
  }

  // Server evidence is attached by the GPT/request integration only after an
  // exact request owner exists. Seeding here would let an absent slot element
  // leak generationless evidence into a later route or refresh.
  installAdTraceOverlay(api, store.subscribe);
  return true;
}

if (typeof window !== 'undefined') installAdTrace();
