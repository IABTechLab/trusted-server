import prebid from 'prebid.js';

// This import precedes selected modules and publisher queue processing in the
// generated external bundle. A late shim cannot provide this ordering guarantee.
window.tsjs?.prebidTargeting?.install(prebid);
