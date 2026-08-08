import { startProductionRuntime } from '../core/index';

import { createBrowserRuntimeComposition } from './browser';

if (typeof window !== 'undefined' && typeof document !== 'undefined') {
  startProductionRuntime(createBrowserRuntimeComposition);
}
