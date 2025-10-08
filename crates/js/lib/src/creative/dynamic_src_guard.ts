import { log } from '../core/log';
import { createMutationScheduler } from '../shared/scheduler';

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

export function createDynamicSrcProxy<E extends ElementWithSrc>(
  options: DynamicSrcProxyOptions<E>
): () => void {
  const attr = (options.attributeName ?? 'src').toLowerCase();
  const tagName = options.tagName.toLowerCase();

  const assignments = new WeakMap<E, { raw: string; requestId: number }>();
  const lastProcessed = new WeakMap<E, string>();
  let sequence = 0;
  let proxyInstalled = false;
  let observerInstalled = false;
  let nativeSet: ((this: E, value: string) => void) | undefined;
  let nativeGet: ((this: E) => string) | undefined;
  let nativeSetAttribute: (this: E, name: string, value: string) => void = () => undefined;
  let nativeSetAttributeNS:
    | ((this: E, namespace: string | null, name: string, value: string) => void)
    | undefined;
  const wrappedInstances = new WeakSet<E>();
  let createElementPatched = false;
  let factoryPatched = false;
  const nativeCreateElement =
    typeof document === 'undefined' ? undefined : document.createElement.bind(document);

  function apply(element: E, value: string): void {
    try {
      if (typeof nativeSet === 'function') {
        nativeSet.call(element, value);
      } else {
        nativeSetAttribute.call(element, attr, value);
      }
    } catch (err) {
      log.debug(`${options.logPrefix}: failed to apply ${options.resourceName} ${attr}`, err);
    }
  }

  function proxyAssignment(element: E, rawInput: string): void {
    const raw = String(rawInput || '');
    const last = lastProcessed.get(element);
    if (last === raw) return;
    lastProcessed.set(element, raw);

    const requestId = ++sequence;
    assignments.set(element, { raw, requestId });

    const proxyable = options.shouldProxy(raw, element);
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
    void options
      .signProxy(raw, element)
      .then((signed) => {
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
      .catch((err) => {
        const current = assignments.get(element);
        if (!current || current.requestId !== requestId) return;
        assignments.delete(element);
        log.warn(
          `${options.logPrefix}: failed to proxy dynamic ${options.resourceName}; using raw ${attr}`,
          err
        );
        lastProcessed.set(element, raw);
        apply(element, raw);
      });
  }

  function monitorMutations(ctor: ElementCtor<E>): void {
    if (observerInstalled) return;
    if (typeof document === 'undefined' || typeof MutationObserver === 'undefined') return;

    const schedule = createMutationScheduler<E>((element) => {
      ensureInstancePatched(element);
      const fromAttr = element.getAttribute(attr) || '';
      const liveValue = (element as unknown as { [key: string]: string | undefined })[attr] || '';
      const raw = fromAttr || liveValue;
      if (!raw) return;
      log.info(`${options.logPrefix}: observed ${attr} set`, { raw });
      proxyAssignment(element, raw);
    });

    const scan = () => {
      document.querySelectorAll(options.selector).forEach((el) => {
        schedule(el as E);
      });
    };

    log.info(`${options.logPrefix}: initial ${options.resourceName} scan`);
    scan();

    const observer = new MutationObserver((records) => {
      for (const record of records) {
        if (record.type === 'attributes') {
          const target = record.target;
          if (target instanceof ctor && record.attributeName === attr) {
            schedule(target as E);
          }
          continue;
        }

        if (record.type === 'childList') {
          record.addedNodes.forEach((node) => {
            if (node instanceof ctor) {
              schedule(node as E);
              return;
            }
            if (!(node instanceof Element)) return;
            node.querySelectorAll(options.selector).forEach((el) => schedule(el as E));
          });
        }
      }
    });

    observer.observe(document, {
      subtree: true,
      childList: true,
      attributes: true,
      attributeFilter: [attr],
    });

    observerInstalled = true;
    log.info(`${options.logPrefix}: mutation observer active`);
  }

  function ensureInstancePatched(element: E | null | undefined): void {
    if (!element || wrappedInstances.has(element)) return;
    wrappedInstances.add(element);
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
          log.info(`${options.logPrefix}: ${tagName} instance ${attr} set`, value);
          proxyAssignment(this, String(value ?? ''));
        },
      });
    } catch (err) {
      log.debug(`${options.logPrefix}: failed to patch ${tagName} instance ${attr}`, err);
    }
  }

  function patchDocumentCreateElement(): void {
    if (createElementPatched || typeof document === 'undefined' || !nativeCreateElement) return;
    createElementPatched = true;
    document.createElement = function patchedCreateElement(
      this: Document,
      name: string,
      options?: ElementCreationOptions
    ): HTMLElement {
      const el = nativeCreateElement(name, options);
      if (typeof name === 'string' && name.toLowerCase() === tagName) {
        ensureInstancePatched(el as unknown as E);
      }
      return el;
    } as typeof document.createElement;
  }

  function patchFactory(): void {
    if (!options.factoryName || factoryPatched) return;
    const globalObj = globalThis as Record<string, unknown>;
    const factory = globalObj[options.factoryName];
    if (typeof factory !== 'function') return;
    const factoryFn = factory as FactoryFunction<E>;

    const WrappedFactory = function (this: unknown, ...args: unknown[]) {
      const instance = Reflect.construct(factoryFn, args, new.target ?? WrappedFactory) as E;
      ensureInstancePatched(instance);
      return instance;
    };

    Object.defineProperty(WrappedFactory, 'length', {
      value: factoryFn.length,
      configurable: true,
    });
    Object.defineProperty(WrappedFactory, 'name', {
      value: options.factoryName,
      configurable: true,
    });
    WrappedFactory.prototype = factoryFn.prototype;
    Object.setPrototypeOf(WrappedFactory, factoryFn);

    globalObj[options.factoryName] = WrappedFactory as unknown;
    factoryPatched = true;
  }

  return function install(): void {
    if (proxyInstalled) return;
    const ctor = options.elementConstructor;
    if (typeof ctor !== 'function') return;

    log.info(`${options.logPrefix}: installing dynamic ${options.resourceName} proxy hooks`);

    const descriptor = Object.getOwnPropertyDescriptor(ctor.prototype, attr);
    if (!descriptor || typeof descriptor.set !== 'function') {
      log.debug(`${options.logPrefix}: ${ctor.name} proxy install skipped (no setter)`);
      return;
    }

    nativeSet = descriptor.set as typeof nativeSet;
    nativeGet =
      typeof descriptor.get === 'function' ? (descriptor.get as typeof nativeGet) : undefined;
    nativeSetAttribute = ctor.prototype.setAttribute as typeof nativeSetAttribute;
    nativeSetAttributeNS =
      typeof ctor.prototype.setAttributeNS === 'function'
        ? (ctor.prototype.setAttributeNS as typeof nativeSetAttributeNS)
        : undefined;

    let prototypePatched = false;
    if (descriptor.configurable !== false) {
      try {
        Object.defineProperty(ctor.prototype, attr, {
          configurable: true,
          enumerable: descriptor.enumerable ?? true,
          get(this: E) {
            log.info(`${options.logPrefix}: ${ctor.name} ${attr} get`);
            const pending = assignments.get(this);
            if (pending) return pending.raw;
            return nativeGet ? nativeGet.call(this) : '';
          },
          set(this: E, value: string) {
            log.info(`${options.logPrefix}: ${ctor.name} ${attr} set`, value);
            proxyAssignment(this, String(value ?? ''));
          },
        });
        prototypePatched = true;
      } catch (err) {
        log.debug(`${options.logPrefix}: failed to patch prototype ${attr}`, err);
      }
    } else {
      log.debug(`${options.logPrefix}: prototype ${attr} not configurable; using fallback`);
    }

    ctor.prototype.setAttribute = function patchedSetAttribute(
      this: E,
      name: string,
      value: string
    ) {
      log.debug(`${options.logPrefix}: ${ctor.name} setAttribute`, { name, value });
      if (typeof name === 'string' && name.toLowerCase() === attr) {
        proxyAssignment(this, String(value ?? ''));
        return;
      }
      nativeSetAttribute.call(this, name, value);
    };

    if (nativeSetAttributeNS) {
      ctor.prototype.setAttributeNS = function patchedSetAttributeNS(
        this: E,
        namespace: string | null,
        name: string,
        value: string
      ): void {
        log.debug(`${options.logPrefix}: ${ctor.name} setAttributeNS`, { namespace, name, value });
        if (typeof name === 'string' && name.toLowerCase() === attr) {
          proxyAssignment(this, String(value ?? ''));
          return;
        }
        nativeSetAttributeNS!.call(this, namespace, name, value);
      };
    }

    proxyInstalled = true;
    log.info(`${options.logPrefix}: dynamic ${options.resourceName} proxy installed`);

    if (!prototypePatched) {
      log.info(`${options.logPrefix}: using instance-level proxy fallback`);
      if (typeof document !== 'undefined') {
        document.querySelectorAll(options.selector).forEach((el) => ensureInstancePatched(el as E));
      }
      patchDocumentCreateElement();
      patchFactory();
    }

    monitorMutations(ctor);
  };
}
