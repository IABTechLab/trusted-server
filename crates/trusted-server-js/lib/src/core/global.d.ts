import type { TsjsApi } from './types';

declare global {
  interface Window {
    /** Request-scoped server bootstrap consumed synchronously by ad trace. */
    __tsjs_adTraceActive?: boolean;
    tsjs?: TsjsApi;
    pbjs?: TsjsApi;
  }
}

export {};
