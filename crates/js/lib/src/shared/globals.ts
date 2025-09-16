import type { TsjsApi } from '../core/types';

export interface TsCreativeApi {
  installProxyClickGuard(): void;
}

export type CreativeWindow = Window & { __ts_creative_installed?: boolean };

export type CreativeGlobal = typeof globalThis & {
  localStorage?: Storage;
  tscreative?: TsCreativeApi;
};

export const creativeGlobal = globalThis as CreativeGlobal;

export function resolveWindow(): Window | undefined {
  if (typeof window !== 'undefined') return window;
  const maybeWindow = (globalThis as { window?: Window }).window;
  return maybeWindow;
}

export type PrebidWindow = Window & { tsjs?: TsjsApi; pbjs?: TsjsApi };

export function resolvePrebidWindow(): PrebidWindow {
  const maybeWindow = resolveWindow();
  return (maybeWindow as PrebidWindow) ?? ({} as PrebidWindow);
}
