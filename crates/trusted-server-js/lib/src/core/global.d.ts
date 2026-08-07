import type { LegacyTsjsApi } from './types';

declare global {
  interface Window {
    /** Publisher-owned object identity is retained through dormant Task 8 bootstrap tests. */
    tsjs?: LegacyTsjsApi;
    pbjs?: LegacyTsjsApi;
  }
}

export {};
