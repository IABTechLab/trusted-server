// Cross-runtime helpers for resolving windows/globals in creatives and pbjs shims.
import type { TsjsApi } from '../core/types';

export interface TsCreativeApi {
  installGuards(): void;
  setConfig?(cfg: TsCreativeConfig): void;
  getConfig?(): TsCreativeConfig;
}

export interface TsCreativeConfig {
  /** Enable click guard runtime. Defaults to true. */
  clickGuard?: boolean;
  /** Enable render guard (dynamic image/iframe src proxies). Defaults to false. */
  renderGuard?: boolean;
}

export type CreativeWindow = Window & {
  __ts_creative_installed?: boolean;
  tsCreativeConfig?: TsCreativeConfig;
};

export type CreativeGlobal = typeof globalThis & {
  localStorage?: Storage;
  tscreative?: TsCreativeApi;
  tsCreativeConfig?: TsCreativeConfig;
};

export const creativeGlobal = globalThis as CreativeGlobal;

// Support SSR/unit tests where window may live on globalThis or be undefined.
export function resolveWindow(): Window | undefined {
  if (typeof window !== 'undefined') return window;
  const maybeWindow = (globalThis as { window?: Window }).window;
  return maybeWindow;
}

export type PrebidWindow = Window & { tsjs?: TsjsApi; pbjs?: TsjsApi };

// Always hand back an object so shims can safely assign tsjs/pbjs globals.
export function resolvePrebidWindow(): PrebidWindow {
  const maybeWindow = resolveWindow();
  return (maybeWindow as PrebidWindow) ?? ({} as PrebidWindow);
}
