const MAX_DIAGNOSTICS_OBSERVATION_DEPTH = 16;
const MAX_DIAGNOSTICS_OBSERVATION_NODES = 512;
const MAX_DIAGNOSTICS_PROPERTY_NAME_BYTES = 128;
const MAX_DIAGNOSTICS_STRING_BYTES = 4096;

export type DiagnosticsObservation = Readonly<Record<string, unknown>>;

export interface DiagnosticsIngressOptions {
  /** Closure-private core reducer; never included in the returned facade. */
  readonly reduce: (observation: DiagnosticsObservation) => void;
  readonly reportError?: (error: unknown) => void;
}

export interface DiagnosticsIngress {
  readonly publish: (candidate: unknown) => boolean;
  readonly dispose: () => void;
}

class InvalidDiagnosticsObservation extends Error {}

function utf8Bytes(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}

function copyDiagnosticsObservation(candidate: unknown): DiagnosticsObservation | undefined {
  const seen = new Set<object>();
  let nodes = 0;

  const copy = (value: unknown, depth: number): unknown => {
    nodes += 1;
    if (nodes > MAX_DIAGNOSTICS_OBSERVATION_NODES) {
      throw new InvalidDiagnosticsObservation();
    }
    if (depth > MAX_DIAGNOSTICS_OBSERVATION_DEPTH) {
      throw new InvalidDiagnosticsObservation();
    }
    if (value === null || typeof value === 'boolean') return value;
    if (typeof value === 'number') {
      if (!Number.isFinite(value)) throw new InvalidDiagnosticsObservation();
      return value;
    }
    if (typeof value === 'string') {
      if (utf8Bytes(value) > MAX_DIAGNOSTICS_STRING_BYTES) {
        throw new InvalidDiagnosticsObservation();
      }
      return value;
    }
    if (typeof value !== 'object') {
      throw new InvalidDiagnosticsObservation();
    }
    if (seen.has(value)) throw new InvalidDiagnosticsObservation();
    seen.add(value);

    if (Array.isArray(value)) {
      if (Object.getPrototypeOf(value) !== Array.prototype) {
        throw new InvalidDiagnosticsObservation();
      }
      const lengthDescriptor = Object.getOwnPropertyDescriptor(value, 'length');
      if (!lengthDescriptor || !('value' in lengthDescriptor)) {
        throw new InvalidDiagnosticsObservation();
      }
      const length = lengthDescriptor.value;
      if (!Number.isSafeInteger(length) || length < 0) {
        throw new InvalidDiagnosticsObservation();
      }
      const keys = Reflect.ownKeys(value);
      if (keys.length !== length + 1 || keys.some((key) => typeof key !== 'string')) {
        throw new InvalidDiagnosticsObservation();
      }
      const result: unknown[] = [];
      for (let index = 0; index < length; index += 1) {
        const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
        if (!descriptor?.enumerable || !('value' in descriptor)) {
          throw new InvalidDiagnosticsObservation();
        }
        result.push(copy(descriptor.value, depth + 1));
      }
      return Object.freeze(result);
    }

    const prototype = Object.getPrototypeOf(value) as unknown;
    if (prototype !== Object.prototype && prototype !== null) {
      throw new InvalidDiagnosticsObservation();
    }
    const result = Object.create(null) as Record<string, unknown>;
    for (const key of Reflect.ownKeys(value)) {
      if (typeof key !== 'string' || utf8Bytes(key) > MAX_DIAGNOSTICS_PROPERTY_NAME_BYTES) {
        throw new InvalidDiagnosticsObservation();
      }
      const descriptor = Object.getOwnPropertyDescriptor(value, key);
      if (!descriptor?.enumerable || !('value' in descriptor)) {
        throw new InvalidDiagnosticsObservation();
      }
      Object.defineProperty(result, key, {
        configurable: true,
        enumerable: true,
        value: copy(descriptor.value, depth + 1),
        writable: true,
      });
    }
    return Object.freeze(result) as DiagnosticsObservation;
  };

  try {
    if (typeof candidate !== 'object' || candidate === null || Array.isArray(candidate)) {
      return undefined;
    }
    return copy(candidate, 0) as DiagnosticsObservation;
  } catch {
    return undefined;
  }
}

/** Create the closure-private, synchronous diagnostics ingress for one runtime. */
export function createDiagnosticsIngress(options: DiagnosticsIngressOptions): DiagnosticsIngress {
  if (typeof options.reduce !== 'function') {
    throw new TypeError('diagnostics reducer must be callable');
  }
  let reduce: DiagnosticsIngressOptions['reduce'] | undefined = options.reduce;
  let reportError: DiagnosticsIngressOptions['reportError'] = options.reportError;
  let disposed = false;

  return Object.freeze({
    publish: (candidate: unknown): boolean => {
      if (disposed) return false;
      const observation = copyDiagnosticsObservation(candidate);
      if (!observation || disposed || !reduce) return false;
      try {
        reduce(observation);
      } catch (error) {
        try {
          reportError?.(error);
        } catch {
          // Local diagnostics reporting cannot affect an accepted publication.
        }
      }
      return true;
    },
    dispose: (): void => {
      if (disposed) return;
      disposed = true;
      reduce = undefined;
      reportError = undefined;
    },
  });
}
