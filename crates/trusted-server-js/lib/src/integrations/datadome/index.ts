import { EMBEDDED_RELEASE_ID } from '../../core/release';

import { createDataDomeIntegrationRegistration } from './module';

if (typeof window !== 'undefined') {
  const register = (window.tsjs as unknown as { _registerIntegration?: unknown } | undefined)
    ?._registerIntegration;
  if (typeof register === 'function') {
    Reflect.apply(register, window.tsjs, [
      createDataDomeIntegrationRegistration(EMBEDDED_RELEASE_ID),
    ]);
  }
}
