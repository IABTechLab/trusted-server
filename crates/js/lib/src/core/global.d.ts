import type { TsjsApi } from './types';

declare global {
  interface Window {
    tsjs?: TsjsApi;
    // pbjs is Prebid.js which has its own types
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    pbjs?: any;
  }
}

export {};
