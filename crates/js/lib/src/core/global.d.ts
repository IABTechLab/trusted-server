import type { TsjsApi } from './types';

declare global {
  interface Window {
    tsjs?: TsjsApi;
    pbjs?: TsjsApi;
  }
}

export {};
