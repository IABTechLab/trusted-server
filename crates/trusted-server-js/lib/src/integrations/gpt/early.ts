// Install GPT dependencies before core flushes publisher callbacks.
import { installGptShim, installTrustedServerPageTargeting } from './shim';
import { installTsRenderBridge } from './render_bridge';
import type { GptWindow } from './types';

if (typeof window !== 'undefined') {
  const win = window as GptWindow & {
    __tsjs_gpt_enabled?: boolean;
    __tsjs_installGptShim?: () => boolean;
  };
  if (win.__tsjs_gpt_enabled === true) {
    win.__tsjs_gpt_early ??= {
      installShim: installGptShim,
      installPageTargeting: installTrustedServerPageTargeting,
    };
    win.__tsjs_installGptShim = win.__tsjs_gpt_early.installShim;
    installGptShim();
    installTrustedServerPageTargeting();
    installTsRenderBridge();
  }
}
