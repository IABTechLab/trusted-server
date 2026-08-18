import type { ApsRendererV1 } from '../types';

import {
  classifyApsRendererDescriptorV1,
  classifyApsRendererV1,
} from './generated/renderer_validator_v1';

type ValidatedRendererCacheEntry = {
  publisherOrigin: string;
  renderer: ApsRendererV1;
};

const validatedRendererCache = new WeakMap<object, ValidatedRendererCacheEntry>();

function isRecord(value: unknown): value is Record<string, unknown> {
  try {
    return typeof value === 'object' && value !== null && !Array.isArray(value);
  } catch {
    return false;
  }
}

/** Parse only the versioned descriptor shape; decoded-envelope trust checks happen separately. */
export function parseApsRendererDescriptor(value: unknown): ApsRendererV1 | undefined {
  try {
    if (classifyApsRendererDescriptorV1(value) !== 'accepted') return undefined;
    return value as unknown as ApsRendererV1;
  } catch {
    return undefined;
  }
}

/** Fully validate the exact APS envelope and cross-check every duplicated descriptor field. */
export function validateApsRenderer(
  value: unknown,
  publisherOrigin = window.location.origin
): ApsRendererV1 | undefined {
  try {
    if (isRecord(value)) {
      const cached = validatedRendererCache.get(value);
      if (cached?.publisherOrigin === publisherOrigin) return cached.renderer;
    }

    if (classifyApsRendererV1(value, publisherOrigin) !== 'accepted') return undefined;
    const renderer = value as ApsRendererV1;
    const validated = Object.freeze({ ...renderer }) as ApsRendererV1;
    validatedRendererCache.set(value as object, { publisherOrigin, renderer: validated });
    validatedRendererCache.set(validated, { publisherOrigin, renderer: validated });
    return validated;
  } catch {
    return undefined;
  }
}
