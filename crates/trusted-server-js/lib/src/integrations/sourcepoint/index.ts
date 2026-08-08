import { log } from '../../core/log';

import { initializeSourcepointConsentMirror } from './consent_mirror';
import { installSourcepointGuard } from './script_guard';

export {
  disposeSourcepointConsentMirror,
  initializeSourcepointConsentMirror,
  mirrorSourcepointConsent,
} from './consent_mirror';

type SourcepointWindow = Window & {
  __tsjs_sourcepoint?: { rewriteSdk?: boolean };
};

// Legacy entry point retained until the coordinated Task 19 wiring cutover.
if (typeof window !== 'undefined') {
  if ((window as SourcepointWindow).__tsjs_sourcepoint?.rewriteSdk !== false) {
    installSourcepointGuard();
  }
  initializeSourcepointConsentMirror();
  log.info('Sourcepoint integration initialized');
}
