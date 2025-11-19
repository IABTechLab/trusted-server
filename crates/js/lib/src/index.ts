// Unified tsjs bundle entry point
// This file conditionally imports modules based on build-time configuration
import type { TsjsApi } from './core/types';
import { modules, type ModuleName } from './generated-modules';
import { log } from './core/log';

const VERSION = '0.1.0-unified';

// Ensure we have a window object
const w: Window & { tsjs?: TsjsApi } =
  ((globalThis as unknown as { window?: Window }).window as Window & {
    tsjs?: TsjsApi;
  }) || ({} as Window & { tsjs?: TsjsApi });

// Log which modules are included in this build
const includedModules = Object.keys(modules) as ModuleName[];
log.info('tsjs unified bundle initialized', {
  version: VERSION,
  modules: includedModules,
});

// The core module sets up the main API and should always be included
// If core is included, it will have already initialized the tsjs global
// and set up the queue system

// Initialize optional modules if they're included
for (const [moduleName, moduleExports] of Object.entries(modules)) {
  if (moduleName === 'core') {
    // Core is already initialized via its own IIFE-style init code
    continue;
  }

  // For other modules, check if they have an init function or are self-initializing
  if (typeof moduleExports === 'object' && moduleExports !== null) {
    // Log that the module is available
    log.debug(`tsjs: module '${moduleName}' loaded`);

    // Some modules like 'ext' are self-initializing (they run on import)
    // Some modules like 'creative' export an API object
    // We don't need to do anything special here - just importing them is enough
  }
}

// Re-export core types for convenience
export type { AdUnit, TsjsApi } from './core/types';

// Export the modules object for advanced use cases
export { modules };
