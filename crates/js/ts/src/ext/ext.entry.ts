import { installPrebidJsShim } from './prebidjs';

// Execute immediately on import; safe no-op if pbjs is not present.
void installPrebidJsShim();
