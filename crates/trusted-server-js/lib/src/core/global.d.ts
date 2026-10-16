import type { TsjsApi } from './types';

declare global {
  interface Window {
    __tsjs_trace_active?: unknown;
    __tsjs_trace_request_context?: unknown;
    tsjs?: TsjsApi;
    pbjs?: TsjsApi;
  }
}

export {};
