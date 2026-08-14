declare const __TSJS_EMBEDDED_RELEASE_ID_V1__: string;

const HASH = /^[0-9a-f]{64}$/;
const COMPONENT_ID = /^[a-z][a-z0-9_]{0,63}$/;
const FIRST_DISPLAY_SRC =
  /^\/static\/tsjs=tsjs-first-display\.min\.js\?m=[0-9a-f]{4}&v=[0-9a-f]{64}$/;
const PARSER_SLICE_ORDER = Object.freeze([
  'aps_initial',
  'creative_initial',
  'datadome_initial',
  'didomi_initial',
  'google_tag_manager_initial',
  'gpt_initial',
  'lockr_initial',
  'osano_initial',
  'permutive_initial',
  'sourcepoint_initial',
  'prebid_initial',
  'testlight_initial',
]);

export const FIRST_DISPLAY_REGISTRATION_FIELD = '_registerFirstDisplay' as const;

export interface FirstDisplayComponentRegistrationV1 {
  readonly abi: 1;
  readonly id: string;
  readonly releaseId: string;
  /** Absolute catalog order, not the component's position in one selected mask. */
  readonly order: number;
  readonly prepare: (host: unknown) => unknown;
}

type FirstDisplayBootstrapTarget = object & {
  readonly [FIRST_DISPLAY_REGISTRATION_FIELD]?: unknown;
};

export interface FirstDisplayParserStateCollector {
  readonly register: (sliceId: string) => boolean;
  readonly observe: (sliceId: string, key: unknown, value: unknown) => boolean;
  readonly snapshot: () => readonly Readonly<{
    sliceId: string;
    observations: readonly string[];
    values: readonly (readonly [string, string | number | boolean | null])[];
  }>[];
}

/** Retain only bounded ordinary-data observations needed by persistent slice owners. */
export function createFirstDisplayParserStateCollector(): FirstDisplayParserStateCollector {
  const states = new Map<
    string,
    { observations: string[]; values: Map<string, string | number | boolean | null> }
  >();
  const encoder = new TextEncoder();
  const register = (sliceId: string): boolean => {
    if (!PARSER_SLICE_ORDER.includes(sliceId)) return false;
    if (!states.has(sliceId)) states.set(sliceId, { observations: [], values: new Map() });
    return true;
  };
  const bounded = (value: string, bytes: number, allowEmpty = false): boolean => {
    if ((!allowEmpty && value.length === 0) || encoder.encode(value).byteLength > bytes)
      return false;
    for (let index = 0; index < value.length; index += 1) {
      const code = value.charCodeAt(index);
      if (code <= 0x1f || code === 0x7f) return false;
    }
    return true;
  };
  return Object.freeze({
    register,
    observe: (sliceId: string, key: unknown, value: unknown): boolean => {
      if (
        !register(sliceId) ||
        typeof key !== 'string' ||
        !bounded(key, 128) ||
        (value !== null &&
          typeof value !== 'string' &&
          typeof value !== 'boolean' &&
          !(typeof value === 'number' && Number.isFinite(value))) ||
        (typeof value === 'string' && !bounded(value, 4096, true))
      ) {
        return false;
      }
      const state = states.get(sliceId)!;
      if (!state.values.has(key)) {
        if (state.values.size >= 256) return false;
        state.observations.push(key);
      }
      state.values.set(key, value as string | number | boolean | null);
      return true;
    },
    snapshot: () =>
      Object.freeze(
        PARSER_SLICE_ORDER.flatMap((sliceId) => {
          const state = states.get(sliceId);
          if (!state) return [];
          return [
            Object.freeze({
              sliceId,
              observations: Object.freeze([...state.observations]),
              values: Object.freeze(
                state.observations.map((key) =>
                  Object.freeze([key, state.values.get(key)!] as const)
                )
              ),
            }),
          ];
        })
      ),
  });
}

/** Wrap one exact frozen slice observation channel with the final-revision ledger. */
export function captureMutationObservedBindings(
  candidate: unknown,
  observeMutation: () => boolean,
  captureObservation?: (key: unknown, value: unknown) => void
): unknown {
  try {
    if (
      typeof candidate !== 'object' ||
      candidate === null ||
      Array.isArray(candidate) ||
      Object.getPrototypeOf(candidate) !== Object.prototype ||
      !Object.isFrozen(candidate) ||
      typeof observeMutation !== 'function'
    ) {
      return candidate;
    }
    const descriptors = Object.getOwnPropertyDescriptors(candidate);
    const observe = descriptors.observe;
    if (!observe?.enumerable || !('value' in observe) || typeof observe.value !== 'function') {
      return candidate;
    }
    const original = observe.value as (...arguments_: unknown[]) => unknown;
    const captured = Object.create(Object.prototype) as Record<PropertyKey, unknown>;
    Object.defineProperties(captured, {
      ...descriptors,
      observe: {
        ...observe,
        value: (...arguments_: unknown[]): unknown => {
          const result = Reflect.apply(original, candidate, arguments_);
          captureObservation?.(arguments_[0], arguments_[1]);
          observeMutation();
          return result;
        },
      },
    });
    return Object.freeze(captured);
  } catch {
    return candidate;
  }
}

/** Copy one untrusted component record without invoking accessors or inherited hooks. */
export function snapshotFirstDisplayComponentRegistration(
  candidate: unknown
): FirstDisplayComponentRegistrationV1 | undefined {
  try {
    if (
      typeof candidate !== 'object' ||
      candidate === null ||
      Array.isArray(candidate) ||
      Object.getPrototypeOf(candidate) !== Object.prototype ||
      !Object.isFrozen(candidate)
    ) {
      return undefined;
    }
    const keys = Reflect.ownKeys(candidate);
    if (
      keys.length !== 5 ||
      !keys.every(
        (key) =>
          typeof key === 'string' && ['abi', 'id', 'releaseId', 'order', 'prepare'].includes(key)
      )
    ) {
      return undefined;
    }
    for (const key of keys) {
      const descriptor = Object.getOwnPropertyDescriptor(candidate, key);
      if (!descriptor?.enumerable || !('value' in descriptor)) return undefined;
    }
    const registration = candidate as unknown as FirstDisplayComponentRegistrationV1;
    if (
      registration.abi !== 1 ||
      !COMPONENT_ID.test(registration.id) ||
      !HASH.test(registration.releaseId) ||
      !Number.isInteger(registration.order) ||
      registration.order < 1 ||
      registration.order > 13 ||
      typeof registration.prepare !== 'function'
    ) {
      return undefined;
    }
    return Object.freeze({
      abi: 1,
      id: registration.id,
      releaseId: registration.releaseId,
      order: registration.order,
      prepare: registration.prepare,
    });
  } catch {
    return undefined;
  }
}

function bootstrapTarget(browser: Window): FirstDisplayBootstrapTarget | undefined {
  try {
    const descriptor = Object.getOwnPropertyDescriptor(browser, 'tsjs');
    if (!descriptor || !('value' in descriptor)) return undefined;
    const target = descriptor.value;
    return (typeof target === 'object' || typeof target === 'function') && target !== null
      ? (target as FirstDisplayBootstrapTarget)
      : undefined;
  } catch {
    return undefined;
  }
}

function authenticatedCurrentScript(browser: Window): HTMLScriptElement | undefined {
  try {
    const { document } = browser;
    const candidate = document.currentScript;
    const Script = document.defaultView?.HTMLScriptElement;
    if (!Script || !(candidate instanceof Script)) {
      return undefined;
    }
    const script = candidate as HTMLScriptElement;
    if (
      script.id !== 'trustedserver-js' ||
      !script.isConnected ||
      script.ownerDocument !== document
    ) {
      return undefined;
    }
    const origin = document.location.origin;
    if (!/^https?:\/\//.test(origin)) return undefined;
    const source = new URL(script.src, origin);
    if (
      source.origin !== origin ||
      source.hash !== '' ||
      !FIRST_DISPLAY_SRC.test(`${source.pathname}${source.search}`)
    ) {
      return undefined;
    }
    return script;
  } catch {
    return undefined;
  }
}

/**
 * Register one independently built component through the bootstrap's ephemeral sink.
 * The sink receives the authenticated current script so its closure can compare the
 * exact parser-inserted owner without consulting `document.currentScript` later.
 */
export function registerFirstDisplayComponent(
  browser: Window,
  registration: FirstDisplayComponentRegistrationV1
): boolean {
  if (!snapshotFirstDisplayComponentRegistration(registration)) return false;
  const script = authenticatedCurrentScript(browser);
  const target = script ? bootstrapTarget(browser) : undefined;
  if (!script || !target) return false;
  try {
    const descriptor = Object.getOwnPropertyDescriptor(target, FIRST_DISPLAY_REGISTRATION_FIELD);
    if (
      !descriptor ||
      !('value' in descriptor) ||
      typeof descriptor.value !== 'function' ||
      descriptor.enumerable ||
      !descriptor.configurable ||
      descriptor.writable
    ) {
      return false;
    }
    return Reflect.apply(descriptor.value, target, [registration, script]) === true;
  } catch {
    return false;
  }
}

/** Create one immutable release-bound registration shared by the build entries. */
export function firstDisplayComponentRegistration(
  id: string,
  order: number,
  prepare: FirstDisplayComponentRegistrationV1['prepare']
): FirstDisplayComponentRegistrationV1 {
  return Object.freeze({
    abi: 1,
    id,
    releaseId: __TSJS_EMBEDDED_RELEASE_ID_V1__,
    order,
    prepare,
  });
}

/** Execute only in a browser build; unit imports without a current script remain inert. */
export function registerCurrentFirstDisplayComponent(
  registration: FirstDisplayComponentRegistrationV1
): boolean {
  try {
    const browser = (globalThis as unknown as { window?: Window }).window;
    return browser ? registerFirstDisplayComponent(browser, registration) : false;
  } catch {
    return false;
  }
}
