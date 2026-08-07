const REQUEST_ADS_DEFAULT_TIMEOUT_MS = 10_000;
const REQUEST_ADS_MAX_SLOTS = 256;
const reflectApplyIntrinsic = Reflect.apply;
const textEncoder = new TextEncoder();
const textEncoderEncodeIntrinsic = TextEncoder.prototype.encode;
const regExpTestIntrinsic = RegExp.prototype.test;
const loneSurrogatePattern = /[\uD800-\uDFFF]/u;
const abortSignalAbortedGetter =
  typeof AbortSignal === 'undefined'
    ? undefined
    : Object.getOwnPropertyDescriptor(AbortSignal.prototype, 'aborted')?.get;

export type RequestAdsInputErrorCode =
  | 'invalid_options'
  | 'invalid_slots'
  | 'empty_slots'
  | 'duplicate_slot'
  | 'invalid_timeout'
  | 'invalid_signal';

export class RequestAdsInputError extends Error {
  public readonly code: RequestAdsInputErrorCode;

  public constructor(code: RequestAdsInputErrorCode) {
    super(code);
    this.name = 'RequestAdsInputError';
    this.code = code;
  }
}

export interface ValidatedRequestAdsOptions {
  readonly aborted: boolean;
  readonly signal: AbortSignal | undefined;
  readonly slots: readonly string[] | undefined;
  readonly timeoutMs: number;
}

function ownDataOptions(value: unknown): Record<string, unknown> | undefined {
  try {
    if (typeof value !== 'object' || value === null || Array.isArray(value)) return undefined;
    const prototype = Object.getPrototypeOf(value) as unknown;
    if (prototype !== Object.prototype && prototype !== null) return undefined;
    if (Object.getOwnPropertySymbols(value).length !== 0) return undefined;
    const output: Record<string, unknown> = Object.create(null) as Record<string, unknown>;
    const names = Object.getOwnPropertyNames(value);
    for (let index = 0; index < names.length; index += 1) {
      const key = names[index];
      if (key === undefined) return undefined;
      const descriptor = Object.getOwnPropertyDescriptor(value, key);
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return undefined;
      output[key] = descriptor.value;
    }
    return output;
  } catch {
    return undefined;
  }
}

function ownDataSlots(value: unknown): readonly unknown[] | undefined {
  try {
    if (
      !Array.isArray(value) ||
      Object.getPrototypeOf(value) !== Array.prototype ||
      Object.getOwnPropertySymbols(value).length !== 0
    ) {
      return undefined;
    }
    const length = Object.getOwnPropertyDescriptor(value, 'length');
    if (
      !length ||
      !('value' in length) ||
      !Number.isSafeInteger(length.value) ||
      length.value < 0 ||
      length.value > REQUEST_ADS_MAX_SLOTS ||
      Object.getOwnPropertyNames(value).length !== length.value + 1
    ) {
      return undefined;
    }
    const output: unknown[] = [];
    for (let index = 0; index < length.value; index += 1) {
      const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return undefined;
      output[index] = descriptor.value;
    }
    return output;
  } catch {
    return undefined;
  }
}

function readAbortSignal(signal: unknown): boolean | undefined {
  try {
    return typeof abortSignalAbortedGetter === 'function'
      ? (reflectApplyIntrinsic(abortSignalAbortedGetter, signal, []) as boolean)
      : undefined;
  } catch {
    return undefined;
  }
}

function hasAsciiControl(value: string): boolean {
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code <= 0x1f || code === 0x7f) return true;
  }
  return false;
}

/** Validate and detach the complete public request before creating attempts. */
export function validateRequestAdsOptions(value: unknown): ValidatedRequestAdsOptions {
  if (value === undefined) {
    return Object.freeze({
      aborted: false,
      signal: undefined,
      slots: undefined,
      timeoutMs: REQUEST_ADS_DEFAULT_TIMEOUT_MS,
    });
  }
  const options = ownDataOptions(value);
  if (
    !options ||
    !Object.keys(options).every((key) => key === 'slots' || key === 'timeoutMs' || key === 'signal')
  ) {
    throw new RequestAdsInputError('invalid_options');
  }

  let slots: readonly string[] | undefined;
  if (Object.prototype.hasOwnProperty.call(options, 'slots')) {
    const rawSlots = ownDataSlots(options.slots);
    if (!rawSlots) throw new RequestAdsInputError('invalid_slots');
    if (rawSlots.length === 0) throw new RequestAdsInputError('empty_slots');
    const seen = new Set<string>();
    const copy: string[] = [];
    for (let index = 0; index < rawSlots.length; index += 1) {
      const slot = rawSlots[index];
      if (
        typeof slot !== 'string' ||
        slot.length === 0 ||
        (reflectApplyIntrinsic(textEncoderEncodeIntrinsic, textEncoder, [slot]) as Uint8Array)
          .byteLength > 256 ||
        hasAsciiControl(slot) ||
        reflectApplyIntrinsic(regExpTestIntrinsic, loneSurrogatePattern, [slot])
      ) {
        throw new RequestAdsInputError('invalid_slots');
      }
      if (seen.has(slot)) throw new RequestAdsInputError('duplicate_slot');
      seen.add(slot);
      copy.push(slot);
    }
    slots = Object.freeze(copy);
  }

  let timeoutMs = REQUEST_ADS_DEFAULT_TIMEOUT_MS;
  if (Object.prototype.hasOwnProperty.call(options, 'timeoutMs')) {
    if (
      typeof options.timeoutMs !== 'number' ||
      !Number.isInteger(options.timeoutMs) ||
      options.timeoutMs < 100 ||
      options.timeoutMs > 30_000
    ) {
      throw new RequestAdsInputError('invalid_timeout');
    }
    timeoutMs = options.timeoutMs;
  }

  let signal: AbortSignal | undefined;
  let aborted = false;
  if (Object.prototype.hasOwnProperty.call(options, 'signal')) {
    const observed = readAbortSignal(options.signal);
    if (observed === undefined) throw new RequestAdsInputError('invalid_signal');
    signal = options.signal as AbortSignal;
    aborted = observed;
  }
  return Object.freeze({ aborted, signal, slots, timeoutMs });
}
