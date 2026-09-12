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
const rendererNoncePattern = /^n1_[A-Za-z0-9_-]{22}$/;

function isRecord(value: unknown): value is Record<string, unknown> {
  try {
    return typeof value === 'object' && value !== null && !Array.isArray(value);
  } catch {
    return false;
  }
}

function exactDataRecord(
  value: unknown,
  expectedKeys: readonly string[]
): Readonly<Record<string, unknown>> | undefined {
  try {
    if (
      typeof value !== 'object' ||
      value === null ||
      Array.isArray(value) ||
      Object.getPrototypeOf(value) !== Object.prototype
    ) {
      return undefined;
    }
    const keys = Reflect.ownKeys(value);
    if (
      keys.length !== expectedKeys.length ||
      keys.some((key) => typeof key !== 'string' || !expectedKeys.includes(key))
    ) {
      return undefined;
    }
    const record: Record<string, unknown> = {};
    for (const key of expectedKeys) {
      const descriptor = Object.getOwnPropertyDescriptor(value, key);
      if (
        !descriptor ||
        descriptor.enumerable !== true ||
        !Object.prototype.hasOwnProperty.call(descriptor, 'value')
      ) {
        return undefined;
      }
      record[key] = descriptor.value;
    }
    return Object.freeze(record);
  } catch {
    return undefined;
  }
}

function exactHttpOrigin(value: unknown): value is string {
  if (typeof value !== 'string' || new TextEncoder().encode(value).byteLength > 2_048) {
    return false;
  }
  try {
    const parsed = new URL(value);
    return (
      (parsed.protocol === 'http:' || parsed.protocol === 'https:') &&
      parsed.hostname !== '' &&
      parsed.username === '' &&
      parsed.password === '' &&
      parsed.origin === value &&
      parsed.pathname === '/' &&
      parsed.search === '' &&
      parsed.hash === ''
    );
  } catch {
    return false;
  }
}

export interface ApsDocumentEnvelopeV1 {
  readonly version: 1;
  readonly nonce: string;
  readonly publisherOrigin: string;
  readonly renderer: Readonly<ApsRendererV1>;
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

/** Parse, fully validate, copy, and freeze the one inner-document envelope. */
export function parseApsDocumentEnvelopeV1(
  candidate: unknown,
  expectedNonce: string,
  expectedPublisherOrigin: string
): Readonly<ApsDocumentEnvelopeV1> | undefined {
  try {
    if (!rendererNoncePattern.test(expectedNonce) || !exactHttpOrigin(expectedPublisherOrigin)) {
      return undefined;
    }
    const envelope = exactDataRecord(candidate, [
      'version',
      'nonce',
      'publisherOrigin',
      'renderer',
    ]);
    if (
      envelope?.['version'] !== 1 ||
      envelope['nonce'] !== expectedNonce ||
      envelope['publisherOrigin'] !== expectedPublisherOrigin
    ) {
      return undefined;
    }
    const renderer = validateApsRenderer(envelope['renderer'], expectedPublisherOrigin);
    if (!renderer) return undefined;
    return Object.freeze({
      version: 1,
      nonce: expectedNonce,
      publisherOrigin: expectedPublisherOrigin,
      renderer,
    });
  } catch {
    return undefined;
  }
}
