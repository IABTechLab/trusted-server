import { log } from '../../core/log';

import { installSourcepointGuard } from './script_guard';

if (typeof window !== 'undefined') {
  installSourcepointGuard();
  log.info('Sourcepoint integration initialized');
}
