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
  const renderFailureReasons = new Set([
    'auction_timeout',
    'auction_disabled',
    'consent_denied',
    'slot_not_eligible',
    'provider_timeout',
    'provider_error',
    'invalid_provider_response',
    'mediation_failed',
    'winner_not_renderable',
    'internal_error',
    'network_error',
    'http_error',
    'invalid_response',
    'slot_unresolved',
    'descriptor_invalid',
    'invalid_dimensions',
    'dimensions_out_of_range',
    'no_render_source',
    'registry_full',
    'capability_registry_full',
    'external_queue_full',
    'external_ready_timeout',
    'external_artifact_incompatible',
    'prebid_admission_failed',
    'prebid_contract_violation',
    'prebid_selection_timeout',
    'reservation_collision',
    'identity_generation_failed',
    'cycle_unattributable',
    'slot_quarantined',
    'gpt_request_failed',
    'gpt_request_timeout',
    'gpt_completion_timeout',
    'reconciliation_capacity',
    'gam_empty',
    'bridge_claim_timeout',
    'bridge_id_mismatch',
    'owner_registration_timeout',
    'owner_insertion_timeout',
    'renderer_document_no_load',
    'runner_no_load',
    'runner_failed',
    'adm_document_no_load',
    'abi_mismatch',
    'bundle_partial',
  ]);
  const cancellationReasons = new Set(['caller_aborted', 'superseded', 'navigation_disposed']);
  const messageEventDataGetter = Object.getOwnPropertyDescriptor(MessageEvent.prototype, 'data')
    ?.get as ((this: MessageEvent) => unknown) | undefined;
  const messageEventPortsGetter = Object.getOwnPropertyDescriptor(MessageEvent.prototype, 'ports')
    ?.get as ((this: MessageEvent) => readonly MessagePort[]) | undefined;

  const ownDataValue = (candidate: unknown, name: string): unknown => {
    try {
      if (typeof candidate !== 'object' || candidate === null) return undefined;
      const descriptor = Object.getOwnPropertyDescriptor(candidate, name);
      return descriptor && 'value' in descriptor ? descriptor.value : undefined;
    } catch {
      return undefined;
    }
  };
  const eventDataValue = (event: unknown): unknown => {
    try {
      if (typeof event !== 'object' || event === null) return undefined;
      const descriptor = Object.getOwnPropertyDescriptor(event, 'data');
      if (descriptor) return 'value' in descriptor ? descriptor.value : undefined;
      return messageEventDataGetter ? Reflect.apply(messageEventDataGetter, event, []) : undefined;
    } catch {
      return undefined;
    }
  };

  const exactRecord = (
    candidate: unknown,
    keys: readonly string[]
  ): Record<string, unknown> | undefined => {
    if (
      typeof candidate !== 'object' ||
      candidate === null ||
      Array.isArray(candidate) ||
      Object.getPrototypeOf(candidate) !== Object.prototype ||
      Object.getOwnPropertySymbols(candidate).length !== 0
    ) {
      return undefined;
    }
    const names = Object.getOwnPropertyNames(candidate).sort();
    const expected = [...keys].sort();
    if (names.length !== expected.length) return undefined;
    for (let index = 0; index < expected.length; index += 1) {
      if (names[index] !== expected[index]) return undefined;
      const descriptor = Object.getOwnPropertyDescriptor(candidate, expected[index] as string);
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return undefined;
    }
    return candidate as Record<string, unknown>;
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
    try {
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
        let validPort = false;
        try {
          validPort =
            !!port &&
            typeof Reflect.get(port, 'postMessage') === 'function' &&
            typeof Reflect.get(port, 'close') === 'function';
        } catch {
          validPort = false;
        }
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
    } catch {
      return undefined;
    }
  };
  const eventPorts = (event: unknown, count: number): MessagePort[] | undefined => {
    const inspection = inspectEventPorts(event);
    return inspection?.exactShape === true &&
      inspection.originalCount === count &&
      inspection.ports.length === count
      ? [...inspection.ports]
      : undefined;
  };
  const closeEventPorts = (event: unknown): void => {
    const inspection = inspectEventPorts(event);
    if (!inspection) return;
    for (let index = 0; index < inspection.ports.length; index += 1) {
      try {
        inspection.ports[index]?.close();
      } catch {
        // Late or malformed endpoints are still contained independently.
      }
    }
  };
  const skipJsonWhitespace = (source: string, start: number): number => {
    let index = start;
    while (
      source[index] === ' ' ||
      source[index] === '\t' ||
      source[index] === '\n' ||
      source[index] === '\r'
    ) {
      index += 1;
    }
    return index;
  };
  const scanJsonString = (source: string, start: number): number | undefined => {
    if (source[start] !== '"') return undefined;
    let index = start + 1;
    while (index < source.length) {
      const character = source[index];
      if (character === '"') return index + 1;
      if (character === '\\') {
        index += 1;
        if (index >= source.length) return undefined;
        if (source[index] === 'u') {
          if (!/^[0-9a-fA-F]{4}$/.test(source.slice(index + 1, index + 5))) return undefined;
          index += 4;
        }
      } else if (character !== undefined && character.charCodeAt(0) < 0x20) {
        return undefined;
      }
      index += 1;
    }
    return undefined;
  };
  const scanJsonValue = (source: string, start: number): number | undefined => {
    let index = skipJsonWhitespace(source, start);
    if (source[index] === '"') return scanJsonString(source, index);
    if (source[index] === '[') {
      index = skipJsonWhitespace(source, index + 1);
      if (source[index] === ']') return index + 1;
      while (index < source.length) {
        const end = scanJsonValue(source, index);
        if (end === undefined) return undefined;
        index = skipJsonWhitespace(source, end);
        if (source[index] === ']') return index + 1;
        if (source[index] !== ',') return undefined;
        index = skipJsonWhitespace(source, index + 1);
      }
      return undefined;
    }
    if (source[index] === '{') {
      const keys = new Set<string>();
      index = skipJsonWhitespace(source, index + 1);
      if (source[index] === '}') return index + 1;
      while (index < source.length) {
        const keyEnd = scanJsonString(source, index);
        if (keyEnd === undefined) return undefined;
        let key: unknown;
        try {
          key = JSON.parse(source.slice(index, keyEnd)) as unknown;
        } catch {
          return undefined;
        }
        if (typeof key !== 'string' || keys.has(key)) return undefined;
        keys.add(key);
        index = skipJsonWhitespace(source, keyEnd);
        if (source[index] !== ':') return undefined;
        const valueEnd = scanJsonValue(source, index + 1);
        if (valueEnd === undefined) return undefined;
        index = skipJsonWhitespace(source, valueEnd);
        if (source[index] === '}') return index + 1;
        if (source[index] !== ',') return undefined;
        index = skipJsonWhitespace(source, index + 1);
      }
      return undefined;
    }
    const match = /^(?:true|false|null|-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?)/.exec(
      source.slice(index)
    );
    return match ? index + match[0].length : undefined;
  };
  const parseJsonWithoutDuplicateKeys = (source: string): unknown => {
    const end = scanJsonValue(source, 0);
    if (end === undefined || skipJsonWhitespace(source, end) !== source.length) return undefined;
    try {
      return JSON.parse(source) as unknown;
    } catch {
      return undefined;
    }
  };
  const parseRegistration = (value: unknown): Record<string, unknown> | undefined => {
    try {
      if (typeof value !== 'string' || new TextEncoder().encode(value).byteLength > 4096) {
        return undefined;
      }
      return exactRecord(parseJsonWithoutDuplicateKeys(value), [
        'message',
        'adId',
        'version',
        'lifecycleTicket',
      ]);
    } catch {
      return undefined;
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
        reject(new Error('TS render owner input refused'));
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
        try {
          frame.onload = null;
        } catch {
          // One hostile DOM setter cannot skip the remaining terminal cleanup.
        }
        try {
          frame.onerror = null;
        } catch {
          // One hostile DOM setter cannot skip the remaining terminal cleanup.
        }
      };
      const clearTimer = (handle: number | undefined): void => {
        if (handle === undefined) return;
        try {
          creativeWindow.clearTimeout(handle);
        } catch {
          // Timer cleanup cannot prevent channel cleanup or Promise settlement.
        }
      };
      const removeFrame = (candidate: HTMLIFrameElement | undefined): void => {
        try {
          candidate?.remove();
        } catch {
          // DOM cleanup is best-effort after authority is already terminal.
        }
      };
      const closePort = (port: MessagePort | undefined): void => {
        try {
          port?.close();
        } catch {
          // Endpoint cleanup remains best-effort after the owner is inert.
        }
      };
      const stopHelper = (): void => {
        const dispose = helperDisposer;
        helperDisposer = undefined;
        try {
          dispose?.();
        } catch {
          // PUC helper cleanup cannot replay owner settlement.
        }
      };
      const finish = (accepted: boolean, reason: string): void => {
        if (settled) return;
        settled = true;
        try {
          clearTimer(registrationTimer);
          clearTimer(ownerTimer);
          stopHelper();
          removeFrameHandlers();
          if (!accepted && frame && !frameCommitted) removeFrame(frame);
          if (controlPort) {
            try {
              controlPort.onmessage = null;
            } catch {
              // One hostile handler setter cannot retain the remaining authority.
            }
            try {
              controlPort.onmessageerror = null;
            } catch {
              // One hostile handler setter cannot retain the remaining authority.
            }
          }
          closePort(controlPort);
          controlPort = undefined;
          ownerFrameCurrent = undefined;
        } finally {
          if (accepted) resolve();
          else reject(new Error(reason));
        }
      };
      const postControl = (message: Record<string, unknown>): boolean => {
        try {
          if (!controlPort || settled) return false;
          controlPort.postMessage(message);
          return true;
        } catch {
          finish(false, 'TS render owner control post failed');
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
        next.setAttribute('sandbox', sandbox);
        next.setAttribute('referrerpolicy', 'no-referrer');
        next.setAttribute('width', String(width));
        next.setAttribute('height', String(height));
        next.setAttribute('scrolling', 'no');
        next.setAttribute('frameborder', '0');
        next.setAttribute('marginwidth', '0');
        next.setAttribute('marginheight', '0');
        next.setAttribute('title', 'Ad content');
        next.setAttribute('aria-label', 'Advertisement');
        next.setAttribute(
          'style',
          `border: 0; margin: 0; overflow: hidden; display: block; width: ${width}px; height: ${height}px;`
        );
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
              ? routedOutcome === 'accepted'
                ? ['outcome']
                : ['outcome', 'reason']
              : []),
        ]);
        if (
          !message ||
          !ports ||
          message['version'] !== 1 ||
          message['lifecycleTicket'] !== lifecycleTicket
        ) {
          closeEventPorts(event);
          finish(false, 'TS render owner control refused');
          return;
        }
        if (
          message['message'] === 'TS ADM Start' &&
          ownerKind === 'adm' &&
          ports.length === 0 &&
          !started
        ) {
          started = true;
          insertAdm(message['source'] as Record<string, unknown>);
          return;
        }
        if (
          message['message'] === 'TS APS Top Mount Started' &&
          ownerKind === 'aps' &&
          ports.length === 0 &&
          !started
        ) {
          started = true;
          return;
        }
        if (message['message'] === 'TS Owner Settled' && ports.length === 0) {
          if (
            message['outcome'] === 'accepted' &&
            started &&
            (ownerKind === 'aps' || ownerFrameCurrent?.() === true)
          ) {
            frameCommitted = true;
            finish(true, '');
            return;
          }
          if (
            message['outcome'] === 'failed' &&
            typeof message['reason'] === 'string' &&
            renderFailureReasons.has(message['reason'])
          ) {
            finish(false, message['reason']);
            return;
          }
          if (
            message['outcome'] === 'cancelled' &&
            typeof message['reason'] === 'string' &&
            cancellationReasons.has(message['reason'])
          ) {
            finish(false, String(message['reason']));
            return;
          }
        }
        closeEventPorts(event);
        finish(false, 'TS render owner control refused');
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
        const response = parseRegistration(dataValue);
        if (
          !ports ||
          !response ||
          response['message'] !== 'TS Render Owner Registered' ||
          response['adId'] !== adId ||
          response['version'] !== 1 ||
          response['lifecycleTicket'] !== lifecycleTicket
        ) {
          closeEventPorts(event);
          finish(false, 'TS render owner registration refused');
          return;
        }
        const registeredPort = ports[0];
        if (!registeredPort) {
          finish(false, 'TS render owner registration refused');
          return;
        }
        controlPort = registeredPort;
        ownerTimer = creativeWindow.setTimeout(
          () => finish(false, 'TS render owner settlement timeout'),
          20_000
        );
        registeredPort.onmessage = receiveControl;
        registeredPort.onmessageerror = () => finish(false, 'TS render owner channel failed');
        try {
          registeredPort.start();
        } catch {
          finish(false, 'TS render owner channel failed');
        }
      };

      const registrationTimer = creativeWindow.setTimeout(
        () => finish(false, 'TS render owner registration timeout'),
        3_000
      );
      try {
        const disposer = Reflect.apply(sendMessage, helper, [
          'TS Render Owner Register',
          { version: 1, lifecycleTicket },
          receiveRegistration,
        ]) as unknown;
        if (typeof disposer !== 'function') {
          finish(false, 'TS render owner registration failed');
          return;
        }
        helperDisposer = disposer as () => void;
        if (registrationFinished) stopHelper();
      } catch {
        finish(false, 'TS render owner registration failed');
      }
    });
}

/** Exact checked-in program returned through PUC's dynamic renderer field. */
export const PUC_DYNAMIC_OWNER = `(${String(installPucDynamicOwner)})();`;
