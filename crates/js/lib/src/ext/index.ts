import { installPrebidJsShim } from './prebidjs';
import { installPermutiveShim } from './permutive';

// Execute immediately on import; safe no-op if pbjs is not present.
void installPrebidJsShim();

setTimeout(() => installPermutiveShim(), 50);
