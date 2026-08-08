export {
  disposeOsanoConsentMirror,
  initializeOsanoConsentMirror,
  mirrorOsanoConsent,
} from './consent_mirror';

import { initializeOsanoConsentMirror } from './consent_mirror';

// Legacy entry point retained until the coordinated Task 19 wiring cutover.
initializeOsanoConsentMirror();
