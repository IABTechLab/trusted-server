/**
 * Install the self-contained renderer that PUC evaluates in its hidden frame.
 *
 * This function deliberately closes over nothing: its serialized source is the
 * exact program returned in the successful outer PUC response.
 */
function installPucDynamicOwner(): void {
  const ownerWindow = window as Window & {
    render?: (data: unknown, helper: unknown, creativeWindow: Window) => Promise<void>;
  };
  const ticketPattern = /^t1_[A-Za-z0-9_-]{22}$/;
  const reservationPattern = /^r1_[A-Za-z0-9_-]{22}$/;
  const admSandbox =
    'allow-forms allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation';
  const ownerError = 'TS render owner ';
  const messageEventDataGetter = Object.getOwnPropertyDescriptor(MessageEvent.prototype, 'data')
    ?.get as ((this: MessageEvent) => unknown) | undefined;
  const messageEventPortsGetter = Object.getOwnPropertyDescriptor(MessageEvent.prototype, 'ports')
    ?.get as ((this: MessageEvent) => readonly MessagePort[]) | undefined;

  const attempt = <T>(callback: () => T): T | undefined => {
    try {
      return callback();
    } catch {
      return undefined;
    }
  };
  const closePort = (port: MessagePort | undefined): void => {
    attempt(() => port?.close());
  };

  const ownDataValue = (candidate: unknown, name: string): unknown => {
    return attempt(() => {
      if (typeof candidate !== 'object' || candidate === null) return undefined;
      const descriptor = Object.getOwnPropertyDescriptor(candidate, name);
      return descriptor && 'value' in descriptor ? descriptor.value : undefined;
    });
  };
  const eventDataValue = (event: unknown): unknown => {
    return attempt(() => {
      if (typeof event !== 'object' || event === null) return undefined;
      const descriptor = Object.getOwnPropertyDescriptor(event, 'data');
      if (descriptor) return 'value' in descriptor ? descriptor.value : undefined;
      return messageEventDataGetter ? Reflect.apply(messageEventDataGetter, event, []) : undefined;
    });
  };

  const exactRecord = (
    candidate: unknown,
    keys: readonly string[]
  ): Record<string, unknown> | undefined => {
    return attempt(() => {
      if (
        typeof candidate !== 'object' ||
        candidate === null ||
        Array.isArray(candidate) ||
        Object.getPrototypeOf(candidate) !== Object.prototype ||
        Object.getOwnPropertySymbols(candidate).length !== 0 ||
        Object.getOwnPropertyNames(candidate).length !== keys.length
      ) {
        return undefined;
      }
      for (let index = 0; index < keys.length; index += 1) {
        const descriptor = Object.getOwnPropertyDescriptor(candidate, keys[index] as string);
        if (!descriptor?.enumerable || !('value' in descriptor)) return undefined;
      }
      return candidate as Record<string, unknown>;
    });
  };
  const inspectEventPorts = (
    event: unknown
  ):
    | Readonly<{
        exactShape: boolean;
        originalCount: number;
        ports: readonly MessagePort[];
      }>
    | undefined => {
    return attempt(() => {
      if (typeof event !== 'object' || event === null) return undefined;
      const descriptor = Object.getOwnPropertyDescriptor(event, 'ports');
      const ports = descriptor
        ? 'value' in descriptor
          ? descriptor.value
          : undefined
        : messageEventPortsGetter
          ? Reflect.apply(messageEventPortsGetter, event, [])
          : undefined;
      if (!Array.isArray(ports)) return undefined;
      const length = Object.getOwnPropertyDescriptor(ports, 'length');
      if (
        !length ||
        !('value' in length) ||
        !Number.isSafeInteger(length.value) ||
        length.value < 0
      ) {
        return undefined;
      }
      let exactShape =
        Object.getPrototypeOf(ports) === Array.prototype &&
        Object.getOwnPropertySymbols(ports).length === 0 &&
        Object.getOwnPropertyNames(ports).length === length.value + 1;
      const snapshot: MessagePort[] = [];
      const seen = new Set<MessagePort>();
      for (let index = 0; index < length.value; index += 1) {
        const descriptor = Object.getOwnPropertyDescriptor(ports, String(index));
        if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) {
          exactShape = false;
          continue;
        }
        const port = descriptor.value as Partial<MessagePort> | undefined;
        const validPort =
          attempt(
            () =>
              !!port &&
              typeof Reflect.get(port, 'postMessage') === 'function' &&
              typeof Reflect.get(port, 'close') === 'function'
          ) === true;
        if (!port || !validPort) {
          exactShape = false;
          continue;
        }
        const accepted = port as MessagePort;
        if (seen.has(accepted)) {
          exactShape = false;
          continue;
        }
        seen.add(accepted);
        snapshot[snapshot.length] = accepted;
      }
      return { exactShape, originalCount: length.value, ports: snapshot };
    });
  };
  const eventPorts = (event: unknown, count: number): MessagePort[] | undefined => {
    const inspection = inspectEventPorts(event);
    return inspection?.exactShape === true &&
      inspection.originalCount === count &&
      inspection.ports.length === count
      ? (inspection.ports as MessagePort[])
      : undefined;
  };
  const closeEventPorts = (event: unknown): void => {
    const inspection = inspectEventPorts(event);
    if (!inspection) return;
    for (let index = 0; index < inspection.ports.length; index += 1) {
      closePort(inspection.ports[index]);
    }
  };
  const validDimension = (value: unknown): value is number =>
    typeof value === 'number' && Number.isInteger(value) && value >= 1 && value <= 4096;
  const validAdmSource = (value: unknown): value is Record<string, unknown> => {
    const source = exactRecord(value, ['type', 'version', 'adm', 'width', 'height']);
    if (
      !source ||
      source['type'] !== 'adm' ||
      source['version'] !== 1 ||
      typeof source['adm'] !== 'string' ||
      source['adm'].trim().length === 0 ||
      new TextEncoder().encode(source['adm']).byteLength > 512 * 1024 ||
      !validDimension(source['width']) ||
      !validDimension(source['height'])
    ) {
      return false;
    }
    return true;
  };
  ownerWindow.render = (data, helper, creativeWindow) =>
    new Promise<void>((resolve, reject) => {
      let outer: Record<string, unknown> | undefined;
      let owner: Record<string, unknown> | undefined;
      let sendMessage: unknown;
      try {
        outer = exactRecord(data, ['adId', 'message', 'renderer', 'rendererVersion', 'tsOwner']);
        owner = outer
          ? exactRecord(outer['tsOwner'], ['version', 'status', 'kind', 'lifecycleTicket'])
          : undefined;
        sendMessage =
          typeof helper === 'object' && helper !== null
            ? Reflect.get(helper, 'sendMessage')
            : undefined;
      } catch {
        outer = undefined;
      }
      const adId = outer?.['adId'];
      const lifecycleTicket = owner?.['lifecycleTicket'];
      const ownerKind = owner?.['kind'];
      if (
        !outer ||
        !owner ||
        outer['message'] !== 'Prebid Response' ||
        outer['rendererVersion'] !== '4' ||
        typeof adId !== 'string' ||
        !reservationPattern.test(adId) ||
        owner['version'] !== 1 ||
        owner['status'] !== 'ready' ||
        (owner['kind'] !== 'aps' && owner['kind'] !== 'adm') ||
        typeof lifecycleTicket !== 'string' ||
        !ticketPattern.test(lifecycleTicket) ||
        typeof sendMessage !== 'function' ||
        !creativeWindow ||
        !creativeWindow.document
      ) {
        reject(new Error(`${ownerError}input refused`));
        return;
      }

      let settled = false;
      let registrationFinished = false;
      let helperDisposer: (() => void) | undefined;
      let ownerTimer: number | undefined;
      let controlPort: MessagePort | undefined;
      let frame: HTMLIFrameElement | undefined;
      let ownerFrameCurrent: (() => boolean) | undefined;
      let frameCommitted = false;
      let started = false;

      const removeFrameHandlers = (): void => {
        if (!frame) return;
        attempt(() => (frame!.onload = null));
        attempt(() => (frame!.onerror = null));
      };
      const clearTimer = (handle: number | undefined): void => {
        if (handle !== undefined) attempt(() => creativeWindow.clearTimeout(handle));
      };
      const removeFrame = (candidate: HTMLIFrameElement | undefined): void => {
        attempt(() => candidate?.remove());
      };
      const stopHelper = (): void => {
        const dispose = helperDisposer;
        helperDisposer = undefined;
        attempt(() => dispose?.());
      };
      const finish = (accepted: boolean, reason: string): void => {
        if (settled) return;
        settled = true;
        clearTimer(registrationTimer);
        clearTimer(ownerTimer);
        stopHelper();
        removeFrameHandlers();
        if (!accepted && frame && !frameCommitted) removeFrame(frame);
        if (controlPort) {
          attempt(() => (controlPort!.onmessage = null));
          attempt(() => (controlPort!.onmessageerror = null));
        }
        closePort(controlPort);
        controlPort = undefined;
        ownerFrameCurrent = undefined;
        if (accepted) resolve();
        else reject(new Error(reason));
      };
      const fail = (reason: string): void => finish(false, `${ownerError}${reason}`);
      const postControl = (message: Record<string, unknown>): boolean => {
        try {
          if (!controlPort || settled) return false;
          controlPort.postMessage(message);
          return true;
        } catch {
          fail('control post failed');
          return false;
        }
      };
      const configureFrame = (
        source: Record<string, unknown>,
        sandbox: string
      ): HTMLIFrameElement => {
        const width = source['width'] as number;
        const height = source['height'] as number;
        const next = creativeWindow.document.createElement('iframe');
        const attributes = [
          'sandbox',
          sandbox,
          'referrerpolicy',
          'no-referrer',
          'width',
          String(width),
          'height',
          String(height),
          'scrolling',
          'no',
          'frameborder',
          '0',
          'marginwidth',
          '0',
          'marginheight',
          '0',
          'title',
          'Ad content',
          'aria-label',
          'Advertisement',
          'style',
          `border: 0; margin: 0; overflow: hidden; display: block; width: ${width}px; height: ${height}px;`,
        ];
        for (let index = 0; index < attributes.length; index += 2) {
          next.setAttribute(attributes[index]!, attributes[index + 1]!);
        }
        return next;
      };
      const prepareDocument = (): void => {
        const document = creativeWindow.document;
        document.documentElement.style.margin = '0';
        document.documentElement.style.padding = '0';
        document.documentElement.style.overflow = 'hidden';
        if (document.body) {
          document.body.style.margin = '0';
          document.body.style.padding = '0';
          document.body.style.overflow = 'hidden';
        }
      };
      const insertAdm = (source: Record<string, unknown>): void => {
        if (!validAdmSource(source) || !creativeWindow.document.body) {
          finish(false, 'TS ADM source refused');
          return;
        }
        prepareDocument();
        const next = configureFrame(source, admSandbox);
        const intendedSource = `<!doctype html><html><head><meta charset="utf-8"><meta name="referrer" content="no-referrer"><style>html,body{border:0;margin:0;padding:0;overflow:hidden}</style></head><body>${source['adm'] as string}</body></html>`;
        ownerFrameCurrent = () =>
          frame === next &&
          next.parentNode === creativeWindow.document.body &&
          next.srcdoc === intendedSource &&
          next.getAttribute('src') === null;
        next.onload = () => {
          if (!settled && ownerFrameCurrent?.() === true) {
            postControl({
              message: 'TS ADM Loaded',
              version: 1,
              lifecycleTicket,
            });
          }
        };
        next.onerror = () => {
          if (!settled && frame === next) {
            postControl({
              message: 'TS ADM Failed',
              version: 1,
              lifecycleTicket,
            });
          }
        };
        next.srcdoc = intendedSource;
        frame = next;
        creativeWindow.document.body.appendChild(next);
        postControl({
          message: 'TS Owner Inserted',
          version: 1,
          lifecycleTicket,
        });
      };
      const receiveControl = (event: MessageEvent): void => {
        if (settled) {
          closeEventPorts(event);
          return;
        }
        const ports = eventPorts(event, 0);
        const dataValue = eventDataValue(event);
        const routedMessage = ownDataValue(dataValue, 'message');
        const routedOutcome = ownDataValue(dataValue, 'outcome');
        const message = exactRecord(dataValue, [
          'message',
          'version',
          'lifecycleTicket',
          ...(routedMessage === 'TS ADM Start'
            ? ['source']
            : routedMessage === 'TS Owner Settled' && routedOutcome !== undefined
              ? ['outcome']
              : []),
        ]);
        if (
          !message ||
          !ports ||
          message['version'] !== 1 ||
          message['lifecycleTicket'] !== lifecycleTicket
        ) {
          closeEventPorts(event);
          fail('control refused');
          return;
        }
        if (message['message'] === 'TS ADM Start' && ownerKind === 'adm' && !started) {
          started = true;
          insertAdm(message['source'] as Record<string, unknown>);
          return;
        }
        if (message['message'] === 'TS APS Top Mount Started' && ownerKind === 'aps' && !started) {
          started = true;
          return;
        }
        if (message['message'] === 'TS Owner Settled') {
          if (
            message['outcome'] === 'accepted' &&
            started &&
            (ownerKind === 'aps' || ownerFrameCurrent?.() === true)
          ) {
            frameCommitted = true;
            finish(true, '');
            return;
          }
          if (message['outcome'] === 'failed' || message['outcome'] === 'cancelled') {
            finish(false, String(message['outcome']));
            return;
          }
        }
        closeEventPorts(event);
        fail('control refused');
      };
      const receiveRegistration = (event: unknown): void => {
        if (settled || registrationFinished) {
          closeEventPorts(event);
          return;
        }
        registrationFinished = true;
        stopHelper();
        clearTimer(registrationTimer);
        const ports = eventPorts(event, 1);
        const dataValue = eventDataValue(event);
        const response =
          typeof dataValue === 'string' &&
          dataValue ===
            JSON.stringify({
              message: 'TS Render Owner Registered',
              adId,
              version: 1,
              lifecycleTicket,
            });
        if (
          !ports ||
          !response ||
          typeof adId !== 'string' ||
          typeof lifecycleTicket !== 'string'
        ) {
          closeEventPorts(event);
          fail('registration refused');
          return;
        }
        const registeredPort = ports[0];
        if (!registeredPort) {
          fail('registration refused');
          return;
        }
        controlPort = registeredPort;
        ownerTimer = creativeWindow.setTimeout(() => fail('settlement timeout'), 20_000);
        registeredPort.onmessage = receiveControl;
        registeredPort.onmessageerror = () => fail('channel failed');
        try {
          registeredPort.start();
        } catch {
          fail('channel failed');
        }
      };

      const registrationTimer = creativeWindow.setTimeout(
        () => fail('registration timeout'),
        3_000
      );
      try {
        const disposer = Reflect.apply(sendMessage, helper, [
          'TS Render Owner Register',
          { version: 1, lifecycleTicket },
          receiveRegistration,
        ]) as unknown;
        if (typeof disposer !== 'function') {
          fail('registration failed');
          return;
        }
        helperDisposer = disposer as () => void;
        if (registrationFinished) stopHelper();
      } catch {
        fail('registration failed');
      }
    });
}

/** Exact checked-in program returned through PUC's dynamic renderer field. */
export const PUC_DYNAMIC_OWNER = `(${String(installPucDynamicOwner)})();`;
