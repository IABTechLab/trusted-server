import { log } from '../../core/log';

import { installSourcepointGuard } from './script_guard';

type SourcepointWindow = Window & {
  __tsjs_sourcepoint?: {
    rewriteSdk?: boolean;
  };
};

function shouldInstallSourcepointGuard(): boolean {
  if (typeof window === 'undefined') {
    return false;
  }

  const config = (window as SourcepointWindow).__tsjs_sourcepoint;
  return config?.rewriteSdk !== false;
}

if (typeof window !== 'undefined' && shouldInstallSourcepointGuard()) {
  installSourcepointGuard();
  log.info('Sourcepoint integration initialized');
}
