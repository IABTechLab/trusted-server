// Permutive segment extraction from localStorage.
// This logic is owned by the Permutive integration, keeping core free of
// integration-specific data-reading code.
import { log } from '../../core/log';

/**
 * Read Permutive segment IDs from localStorage.
 *
 * Permutive stores cohort data in the `permutive-app` key. We check two
 * locations (most reliable first):
 *
 * 1. `core.cohorts.all` — full cohort membership (numeric IDs + activation keys).
 * 2. `eventPublication.eventUpload` — transient event data; we iterate
 *    most-recent-first looking for any event whose `properties.segments` is a
 *    non-empty array.
 *
 * Returns an array of segment ID strings, or an empty array if unavailable.
 */
export function getPermutiveSegments(): string[] {
  try {
    const raw = localStorage.getItem('permutive-app');
    if (!raw) return [];

    const data = JSON.parse(raw);

    // Primary: core.cohorts.all (full cohort membership — numeric IDs + activation keys)
    const all = data?.core?.cohorts?.all;
    if (Array.isArray(all) && all.length > 0) {
      log.debug('getPermutiveSegments: found segments in core.cohorts.all', { count: all.length });
      return all
        .filter((s: unknown) => typeof s === 'string' || typeof s === 'number')
        .map(String);
    }

    // Fallback: eventUpload entries (transient event data)
    const uploads: unknown[] = data?.eventPublication?.eventUpload;
    if (Array.isArray(uploads)) {
      for (let i = uploads.length - 1; i >= 0; i--) {
        const entry = uploads[i];
        if (!Array.isArray(entry) || entry.length < 2) continue;

        const segments = entry[1]?.event?.properties?.segments;
        if (Array.isArray(segments) && segments.length > 0) {
          log.debug('getPermutiveSegments: found segments in eventUpload', {
            count: segments.length,
          });
          return segments
            .filter((s: unknown) => typeof s === 'string' || typeof s === 'number')
            .map(String);
        }
      }
    }
  } catch {
    log.debug('getPermutiveSegments: failed to read from localStorage');
  }
  return [];
}
