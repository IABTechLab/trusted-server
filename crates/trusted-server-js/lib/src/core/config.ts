// Global configuration storage for the tsjs runtime (logging, debug, etc.).
import { log } from './log';
import type { LogLevel } from './log';
import type { CacheFetchPolicyV1 } from './types';

export interface Config {
  debug?: boolean;
  logLevel?: 'silent' | 'error' | 'warn' | 'info' | 'debug';
  [key: string]: unknown;
}

let CONFIG: Config = {};

// Merge publisher-provided config and adjust the log level accordingly.
export function setConfig(cfg: Config): void {
  CONFIG = { ...CONFIG, ...cfg };
  const debugFlag = cfg.debug;
  const l = cfg.logLevel as LogLevel | undefined;
  if (typeof l === 'string') log.setLevel(l);
  else if (debugFlag === true) log.setLevel('debug');
  log.info('setConfig:', cfg);
}

// Return a defensive copy so callers can't mutate shared state.
export function getConfig(): Config {
  return { ...CONFIG };
}

function exactOwnDataObject(
  value: unknown,
  expectedKeys: readonly string[]
): Record<string, unknown> | undefined {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return undefined;
  if (Object.getPrototypeOf(value) !== Object.prototype) return undefined;
  if (Object.getOwnPropertySymbols(value).length !== 0) return undefined;
  const names = Object.getOwnPropertyNames(value);
  if (names.length !== expectedKeys.length || expectedKeys.some((key) => !names.includes(key))) {
    return undefined;
  }
  for (const name of names) {
    const descriptor = Object.getOwnPropertyDescriptor(value, name);
    if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return undefined;
  }
  return value as Record<string, unknown>;
}

function validUnicodeScalars(value: string): boolean {
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = value.charCodeAt(index + 1);
      if (!(next >= 0xdc00 && next <= 0xdfff)) return false;
      index += 1;
    } else if (code >= 0xdc00 && code <= 0xdfff) {
      return false;
    }
  }
  return true;
}

function hasAsciiControl(value: string): boolean {
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code <= 0x1f || code === 0x7f) return true;
  }
  return false;
}

/** Validate, snapshot, and freeze the immutable cache-fetch boot policy. */
export function parseCacheFetchPolicyV1(value: unknown): Readonly<CacheFetchPolicyV1> | undefined {
  const policy = exactOwnDataObject(value, ['version', 'baseUrl']);
  if (
    !policy ||
    policy.version !== 1 ||
    typeof policy.baseUrl !== 'string' ||
    policy.baseUrl.length === 0 ||
    !validUnicodeScalars(policy.baseUrl) ||
    new TextEncoder().encode(policy.baseUrl).length > 4096 ||
    hasAsciiControl(policy.baseUrl)
  ) {
    return undefined;
  }

  let base: URL;
  try {
    base = new URL(policy.baseUrl);
  } catch {
    return undefined;
  }
  if (
    base.protocol !== 'https:' ||
    base.hostname === '' ||
    base.username !== '' ||
    base.password !== '' ||
    base.search !== '' ||
    base.hash !== '' ||
    base.pathname === '/'
  ) {
    return undefined;
  }

  return Object.freeze({ version: 1, baseUrl: policy.baseUrl });
}
