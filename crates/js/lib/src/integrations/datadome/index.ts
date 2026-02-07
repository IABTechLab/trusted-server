import { log } from '../../core/log';

import { installDataDomeGuard } from './script_guard';

/**
 * DataDome integration for tsjs
 *
 * Installs a script guard to intercept dynamically inserted DataDome SDK
 * scripts and rewrites them to use the first-party proxy endpoint.
 *
 * The guard intercepts:
 * - Script elements with src containing js.datadome.co
 * - Link preload elements for DataDome scripts
 *
 * URLs are rewritten to preserve the original path:
 * - https://js.datadome.co/tags.js -> /integrations/datadome/tags.js
 * - https://js.datadome.co/js/check -> /integrations/datadome/js/check
 */

if (typeof window !== 'undefined') {
  installDataDomeGuard();
  log.info('DataDome integration initialized');
}
