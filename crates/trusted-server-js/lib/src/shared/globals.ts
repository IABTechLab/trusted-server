/** @file Cross-runtime global resolution for creatives and Prebid shims. */
import type { TsjsApi } from '../core/types';

/** Public creative-runtime controls installed on the resolved global. */
export interface TsCreativeApi {
  installGuards(): void;
  setConfig?(cfg: TsCreativeConfig): void;
  getConfig?(): TsCreativeConfig;
}

/** Optional creative click and dynamic-source guard switches. */
export interface TsCreativeConfig {
  /** Enable click guard runtime. Defaults to true. */
  clickGuard?: boolean;
  /** Enable render guard (dynamic image/iframe src proxies). Defaults to false. */
  renderGuard?: boolean;
}

/** Browser window fields owned by the creative runtime. */
export type CreativeWindow = Window & {
  __ts_creative_installed?: boolean;
  tsCreativeConfig?: TsCreativeConfig;
};

/** Global shape used in browsers, SSR, and DOM-based unit tests. */
export type CreativeGlobal = typeof globalThis & {
  localStorage?: Storage;
  tscreative?: TsCreativeApi;
  tsCreativeConfig?: TsCreativeConfig;
};

/** Current global cast to the bounded creative-runtime surface. */
export const creativeGlobal = globalThis as CreativeGlobal;

/** Resolve a browser window without assuming one exists during SSR or tests. */
export function resolveWindow(): Window | undefined {
  if (typeof window !== 'undefined') return window;
  const maybeWindow = (globalThis as { window?: Window }).window;
  return maybeWindow;
}

/** Window fields shared by the TSJS and lightweight Prebid-compatible APIs. */
export type PrebidWindow = Window & { tsjs?: TsjsApi; pbjs?: TsjsApi };

/** Return an assignable Prebid global, using an isolated fallback without a window. */
export function resolvePrebidWindow(): PrebidWindow {
  const maybeWindow = resolveWindow();
  return (maybeWindow as PrebidWindow) ?? ({} as PrebidWindow);
}
