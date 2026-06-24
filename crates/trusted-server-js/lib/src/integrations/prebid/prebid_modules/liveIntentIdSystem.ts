// Local ESM bridge for Prebid's LiveIntent User ID module.
//
// The public `prebid.js/modules/liveIntentIdSystem.js` wrapper contains a
// CommonJS `require()` mode switch, which Vite cannot bundle into our IIFE
// safely. Import the standard ESM implementation directly via build aliases.
import 'tsjs-prebid/liveIntentIdSystemStandard';
import { registerModule } from 'tsjs-prebid/prebidGlobal';

registerModule('liveIntentIdSystem');
