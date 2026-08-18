import { log } from '../../core/log';
import { createMutationScheduler, type MutationScheduler } from '../../shared/scheduler';

import type { CreativeGuardHandle } from './startup';

type ElementWithSrc = Element & { src: string };

type ElementCtor<E extends ElementWithSrc> = {
  prototype: E;
  new (...args: unknown[]): E;
};

type FactoryFunction<E extends ElementWithSrc> = {
  length: number;
  prototype: E;
  new (...args: unknown[]): E;
} & ((...args: unknown[]) => E);

interface InstancePatch {
  readonly installed: PropertyDescriptor;
  readonly original: PropertyDescriptor | undefined;
}

export interface DynamicSrcProxyOptions<E extends ElementWithSrc> {
  elementConstructor: ElementCtor<E> | undefined;
  selector: string;
  tagName: string;
  factoryName?: string;
  attributeName?: string;
  resourceName: string;
  logPrefix: string;
  shouldProxy(raw: string, element: E): boolean;
  signProxy(raw: string, element: E): Promise<string | null>;
}

function sameDescriptor(
  left: PropertyDescriptor | undefined,
  right: PropertyDescriptor | undefined
): boolean {
  if (!left || !right) return left === right;
  return (
    left.configurable === right.configurable &&
    left.enumerable === right.enumerable &&
    left.get === right.get &&
    left.set === right.set &&
    left.value === right.value &&
    left.writable === right.writable
  );
}

function inertHandle(): CreativeGuardHandle {
  return Object.freeze({ dispose: () => undefined, scan: () => undefined });
}

export function createDynamicSrcProxy<E extends ElementWithSrc>(
  options: DynamicSrcProxyOptions<E>
): (scanInitially?: boolean) => CreativeGuardHandle {
  const attr = (options.attributeName ?? 'src').toLowerCase();
  const tagName = options.tagName.toLowerCase();
  let installedHandle: CreativeGuardHandle | undefined;

  return function install(scanInitially = true): CreativeGuardHandle {
    if (installedHandle) return installedHandle;
    const ctor = options.elementConstructor;
    if (typeof ctor !== 'function') {
      installedHandle = inertHandle();
      return installedHandle;
    }

    const sourceDescriptor = Object.getOwnPropertyDescriptor(ctor.prototype, attr);
    if (!sourceDescriptor || typeof sourceDescriptor.set !== 'function') {
      log.debug(`${options.logPrefix}: ${ctor.name} proxy install skipped (no setter)`);
      installedHandle = inertHandle();
      return installedHandle;
    }

    const assignments = new WeakMap<E, { raw: string; requestId: number }>();
    const lastProcessed = new WeakMap<E, string>();
    const instancePatches = new Map<E, InstancePatch>();
    const nativeSet = sourceDescriptor.set as (this: E, value: string) => void;
    const nativeGet =
      typeof sourceDescriptor.get === 'function'
        ? (sourceDescriptor.get as (this: E) => string)
        : undefined;
    const nativeSetAttribute = ctor.prototype.setAttribute as (
      this: E,
      name: string,
      value: string
    ) => void;
    const nativeSetAttributeNS =
      typeof ctor.prototype.setAttributeNS === 'function'
        ? (ctor.prototype.setAttributeNS as (
            this: E,
            namespace: string | null,
            name: string,
            value: string
          ) => void)
        : undefined;
    const originalSetAttribute = Object.getOwnPropertyDescriptor(ctor.prototype, 'setAttribute');
    const originalSetAttributeNS = Object.getOwnPropertyDescriptor(
      ctor.prototype,
      'setAttributeNS'
    );
    const targetDocument = typeof document === 'undefined' ? undefined : document;
    const nativeCreateElement = targetDocument?.createElement;
    const originalCreateElement = targetDocument
      ? Object.getOwnPropertyDescriptor(targetDocument, 'createElement')
      : undefined;
    let active = true;
    let sequence = 0;
    let observer: MutationObserver | undefined;
    let scheduler: MutationScheduler<E> | undefined;
    let installedSource: PropertyDescriptor | undefined;
    let installedSetAttribute: PropertyDescriptor | undefined;
    let installedSetAttributeNS: PropertyDescriptor | undefined;
    let installedCreateElement: PropertyDescriptor | undefined;
    let factoryTarget: Record<string, unknown> | undefined;
    let factoryOriginal: PropertyDescriptor | undefined;
    let installedFactory: PropertyDescriptor | undefined;

    const restore = (
      target: object,
      key: PropertyKey,
      owned: PropertyDescriptor | undefined,
      original: PropertyDescriptor | undefined
    ): void => {
      try {
        if (!sameDescriptor(Object.getOwnPropertyDescriptor(target, key), owned)) return;
        if (original) Object.defineProperty(target, key, original);
        else Reflect.deleteProperty(target, key);
      } catch (error) {
        log.debug(`${options.logPrefix}: failed to restore ${String(key)}`, error);
      }
    };

    const apply = (element: E, value: string): void => {
      try {
        nativeSet.call(element, value);
      } catch (error) {
        try {
          nativeSetAttribute.call(element, attr, value);
        } catch (fallbackError) {
          log.debug(
            `${options.logPrefix}: failed to apply ${options.resourceName} ${attr}`,
            error,
            fallbackError
          );
        }
      }
    };

    const proxyAssignment = (element: E, rawInput: string): void => {
      if (!active) {
        apply(element, String(rawInput ?? ''));
        return;
      }
      const raw = String(rawInput || '');
      const last = lastProcessed.get(element);
      if (last === raw) return;
      lastProcessed.set(element, raw);

      const requestId = ++sequence;
      assignments.set(element, { raw, requestId });

      let proxyable = false;
      try {
        proxyable = options.shouldProxy(raw, element);
      } catch (error) {
        log.warn(`${options.logPrefix}: ${options.resourceName} policy failed`, error);
      }
      if (!proxyable || typeof fetch !== 'function') {
        log.info(`${options.logPrefix}: skipping proxy for ${attr}`, {
          reason: proxyable ? 'no-fetch' : 'non-proxyable',
          raw,
        });
        assignments.delete(element);
        apply(element, raw);
        return;
      }

      log.info(`${options.logPrefix}: signing ${options.resourceName} ${attr}`, { raw });
      let signing: Promise<string | null>;
      try {
        signing = options.signProxy(raw, element);
      } catch (error) {
        assignments.delete(element);
        log.warn(
          `${options.logPrefix}: failed to proxy dynamic ${options.resourceName}; using raw ${attr}`,
          error
        );
        apply(element, raw);
        return;
      }
      void signing
        .then((signed) => {
          if (!active) return;
          const current = assignments.get(element);
          if (!current || current.requestId !== requestId) return;
          assignments.delete(element);
          const finalUrl = signed || raw;
          if (signed) {
            log.info(`${options.logPrefix}: proxied dynamic ${options.resourceName}`, {
              base: raw,
              finalUrl,
            });
          }
          lastProcessed.set(element, finalUrl);
          apply(element, finalUrl);
        })
        .catch((error) => {
          if (!active) return;
          const current = assignments.get(element);
          if (!current || current.requestId !== requestId) return;
          assignments.delete(element);
          log.warn(
            `${options.logPrefix}: failed to proxy dynamic ${options.resourceName}; using raw ${attr}`,
            error
          );
          lastProcessed.set(element, raw);
          apply(element, raw);
        });
    };

    const ensureInstancePatched = (element: E | null | undefined): void => {
      if (!active || !element || instancePatches.has(element)) return;
      const original = Object.getOwnPropertyDescriptor(element, attr);
      try {
        Object.defineProperty(element, attr, {
          configurable: true,
          enumerable: true,
          get(this: E) {
            const pending = assignments.get(this);
            if (pending) return pending.raw;
            return nativeGet ? nativeGet.call(this) : '';
          },
          set(this: E, value: string) {
            if (!active) {
              apply(this, String(value ?? ''));
              return;
            }
            log.info(`${options.logPrefix}: ${tagName} instance ${attr} set`, value);
            proxyAssignment(this, String(value ?? ''));
          },
        });
        const installed = Object.getOwnPropertyDescriptor(element, attr);
        if (installed) instancePatches.set(element, { installed, original });
      } catch (error) {
        log.debug(`${options.logPrefix}: failed to patch ${tagName} instance ${attr}`, error);
      }
    };

    const scan = (): void => {
      if (!active || !targetDocument || !scheduler) return;
      targetDocument.querySelectorAll(options.selector).forEach((element) => {
        scheduler?.(element as E);
      });
    };

    const dispose = (): void => {
      if (!active) return;
      active = false;
      observer?.disconnect();
      scheduler?.dispose();
      for (const [element, patch] of instancePatches) {
        restore(element, attr, patch.installed, patch.original);
      }
      instancePatches.clear();
      if (targetDocument) {
        restore(targetDocument, 'createElement', installedCreateElement, originalCreateElement);
      }
      if (factoryTarget && options.factoryName) {
        restore(factoryTarget, options.factoryName, installedFactory, factoryOriginal);
      }
      restore(ctor.prototype, 'setAttributeNS', installedSetAttributeNS, originalSetAttributeNS);
      restore(ctor.prototype, 'setAttribute', installedSetAttribute, originalSetAttribute);
      restore(ctor.prototype, attr, installedSource, sourceDescriptor);
      if (installedHandle === handle) installedHandle = undefined;
    };

    const handle = Object.freeze({ dispose, scan });

    try {
      log.info(`${options.logPrefix}: installing dynamic ${options.resourceName} proxy hooks`);
      let prototypePatched = false;
      if (sourceDescriptor.configurable !== false) {
        Object.defineProperty(ctor.prototype, attr, {
          configurable: true,
          enumerable: sourceDescriptor.enumerable ?? true,
          get(this: E) {
            const pending = assignments.get(this);
            if (pending) return pending.raw;
            return nativeGet ? nativeGet.call(this) : '';
          },
          set(this: E, value: string) {
            if (!active) {
              apply(this, String(value ?? ''));
              return;
            }
            log.info(`${options.logPrefix}: ${ctor.name} ${attr} set`, value);
            proxyAssignment(this, String(value ?? ''));
          },
        });
        installedSource = Object.getOwnPropertyDescriptor(ctor.prototype, attr);
        prototypePatched = true;
      } else {
        log.debug(`${options.logPrefix}: prototype ${attr} not configurable; using fallback`);
      }

      ctor.prototype.setAttribute = function patchedSetAttribute(
        this: E,
        name: string,
        value: string
      ): void {
        if (!active || typeof name !== 'string' || name.toLowerCase() !== attr) {
          nativeSetAttribute.call(this, name, value);
          return;
        }
        log.debug(`${options.logPrefix}: ${ctor.name} setAttribute`, { name, value });
        proxyAssignment(this, String(value ?? ''));
      };
      installedSetAttribute = Object.getOwnPropertyDescriptor(ctor.prototype, 'setAttribute');

      if (nativeSetAttributeNS) {
        ctor.prototype.setAttributeNS = function patchedSetAttributeNS(
          this: E,
          namespace: string | null,
          name: string,
          value: string
        ): void {
          if (!active || typeof name !== 'string' || name.toLowerCase() !== attr) {
            nativeSetAttributeNS.call(this, namespace, name, value);
            return;
          }
          log.debug(`${options.logPrefix}: ${ctor.name} setAttributeNS`, {
            namespace,
            name,
            value,
          });
          proxyAssignment(this, String(value ?? ''));
        };
        installedSetAttributeNS = Object.getOwnPropertyDescriptor(ctor.prototype, 'setAttributeNS');
      }

      if (!prototypePatched) {
        if (targetDocument && nativeCreateElement) {
          targetDocument
            .querySelectorAll(options.selector)
            .forEach((element) => ensureInstancePatched(element as E));
          targetDocument.createElement = function patchedCreateElement(
            this: Document,
            name: string,
            creationOptions?: ElementCreationOptions
          ): HTMLElement {
            const element = nativeCreateElement.call(this, name, creationOptions);
            if (active && typeof name === 'string' && name.toLowerCase() === tagName) {
              ensureInstancePatched(element as unknown as E);
            }
            return element;
          } as typeof targetDocument.createElement;
          installedCreateElement = Object.getOwnPropertyDescriptor(targetDocument, 'createElement');
        }

        if (options.factoryName) {
          const globalObject = globalThis as Record<string, unknown>;
          const factory = globalObject[options.factoryName];
          if (typeof factory === 'function') {
            const factoryFunction = factory as FactoryFunction<E>;
            factoryTarget = globalObject;
            factoryOriginal = Object.getOwnPropertyDescriptor(globalObject, options.factoryName);
            const WrappedFactory = function (this: unknown, ...args: unknown[]) {
              const instance = Reflect.construct(
                factoryFunction,
                args,
                new.target ?? WrappedFactory
              ) as E;
              if (active) ensureInstancePatched(instance);
              return instance;
            };
            Object.defineProperty(WrappedFactory, 'length', {
              value: factoryFunction.length,
              configurable: true,
            });
            Object.defineProperty(WrappedFactory, 'name', {
              value: options.factoryName,
              configurable: true,
            });
            WrappedFactory.prototype = factoryFunction.prototype;
            Object.setPrototypeOf(WrappedFactory, factoryFunction);
            globalObject[options.factoryName] = WrappedFactory;
            installedFactory = Object.getOwnPropertyDescriptor(globalObject, options.factoryName);
          }
        }
      }

      if (targetDocument && typeof MutationObserver !== 'undefined') {
        scheduler = createMutationScheduler<E>((element) => {
          if (!active) return;
          ensureInstancePatched(element);
          const fromAttribute = element.getAttribute(attr) || '';
          const liveValue =
            (element as unknown as { [key: string]: string | undefined })[attr] || '';
          const raw = fromAttribute || liveValue;
          if (!raw) return;
          log.info(`${options.logPrefix}: observed ${attr} set`, { raw });
          proxyAssignment(element, raw);
        });
        observer = new MutationObserver((records) => {
          if (!active) return;
          for (const record of records) {
            if (record.type === 'attributes') {
              const target = record.target;
              if (target instanceof ctor && record.attributeName === attr) scheduler?.(target as E);
              continue;
            }
            if (record.type !== 'childList') continue;
            record.addedNodes.forEach((node) => {
              if (node instanceof ctor) {
                scheduler?.(node as E);
                return;
              }
              if (!(node instanceof Element)) return;
              node
                .querySelectorAll(options.selector)
                .forEach((element) => scheduler?.(element as E));
            });
          }
        });
        observer.observe(targetDocument, {
          subtree: true,
          childList: true,
          attributes: true,
          attributeFilter: [attr],
        });
      }

      installedHandle = handle;
      if (scanInitially) scan();
      return handle;
    } catch (error) {
      dispose();
      throw error;
    }
  };
}
