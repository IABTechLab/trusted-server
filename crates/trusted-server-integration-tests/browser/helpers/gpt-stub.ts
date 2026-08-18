import type { Page } from "@playwright/test";

/** Install a deterministic documented-event GPT stub before publisher scripts run. */
export async function installGptStub(page: Page): Promise<void> {
  await page.addInitScript(() => {
    type StubSlot = {
      addService(service: object): StubSlot;
      clearTargeting(key?: string): StubSlot;
      getAdUnitPath(): string;
      getSlotElementId(): string;
      getTargeting(key: string): string[];
      setTargeting(key: string, value: string | string[]): StubSlot;
    };
    type StubEvent = { slot: StubSlot } & Record<string, unknown>;
    type StubListener = (event: StubEvent) => void;

    const listeners = new Map<string, Set<StubListener>>();
    const slots = new Map<string, StubSlot>();
    const physicalSlots = new Set<StubSlot>();
    const displayCalls: StubSlot[] = [];
    const refreshCalls: StubSlot[][] = [];
    const universalCreativeLifecycle: string[] = [];
    const emissions: Array<Readonly<Record<string, unknown>>> = [];
    const pageTargeting = new Map<string, string[]>();
    let initialLoadDisabled = false;
    let requestStartOnDisplay = false;
    let nonemptyCompletionOnDisplay = false;

    const createSlot = (id: string, adUnitPath: string): StubSlot => {
      const targeting = new Map<string, string[]>();
      const slot: StubSlot = {
        addService() {
          return slot;
        },
        clearTargeting(key?: string) {
          if (key === undefined) targeting.clear();
          else targeting.delete(key);
          return slot;
        },
        getAdUnitPath: () => adUnitPath,
        getSlotElementId: () => id,
        getTargeting(key: string) {
          return [...(targeting.get(key) ?? [])];
        },
        setTargeting(key: string, value: string | string[]) {
          targeting.set(key, Array.isArray(value) ? [...value] : [value]);
          return slot;
        },
      };
      slots.set(id, slot);
      return slot;
    };

    const slotForTarget = (target: unknown): StubSlot | undefined => {
      if (typeof target === "string") return slots.get(target);
      if (typeof target !== "object" || target === null) return undefined;
      return [...physicalSlots].find((candidate) => candidate === target);
    };

    const emit = (
      name: string,
      slot: StubSlot,
      facts: Record<string, unknown> = {},
    ): void => {
      for (const listener of listeners.get(name) ?? []) {
        listener({ slot, ...facts });
      }
    };

    const pubadsService = {
      addEventListener(name: string, listener: StubListener) {
        const current = listeners.get(name) ?? new Set<StubListener>();
        current.add(listener);
        listeners.set(name, current);
      },
      removeEventListener(name: string, listener: StubListener) {
        const current = listeners.get(name);
        current?.delete(listener);
        if (current?.size === 0) listeners.delete(name);
      },
      disableInitialLoad() {
        initialLoadDisabled = true;
      },
      enableSingleRequest() {},
      getConfig() {
        return { disableInitialLoad: initialLoadDisabled };
      },
      getSlots() {
        return [...physicalSlots];
      },
      getTargeting(key: string) {
        return [...(pageTargeting.get(key) ?? [])];
      },
      setTargeting(key: string, value: string | string[]) {
        pageTargeting.set(key, Array.isArray(value) ? [...value] : [value]);
        return pubadsService;
      },
      refresh(requestedSlots?: StubSlot[]) {
        const requested = requestedSlots ?? [...physicalSlots];
        refreshCalls.push([...requested]);
      },
    };
    const commandQueue = {
      push(callback: () => void) {
        callback();
        return 1;
      },
    };
    const googletag = {
      apiReady: true,
      pubadsReady: true,
      cmd: commandQueue,
      destroySlots(requested?: StubSlot[]) {
        const candidates = requested ?? [...physicalSlots];
        for (const slot of candidates) physicalSlots.delete(slot);
        return true;
      },
      display(target: string | StubSlot) {
        const slot = slotForTarget(target);
        if (slot) {
          displayCalls.push(slot);
          if (requestStartOnDisplay) {
            emissions.push({
              name: "slotRequested",
              listeners: listeners.get("slotRequested")?.size ?? 0,
              physical: physicalSlots.has(slot),
            });
            emit("slotRequested", slot);
            if (nonemptyCompletionOnDisplay) {
              emissions.push({
                name: "slotRenderEnded",
                listeners: listeners.get("slotRenderEnded")?.size ?? 0,
                physical: physicalSlots.has(slot),
              });
              emit("slotRenderEnded", slot, {
                isEmpty: false,
                responseIdentifier: "fictional-response-1",
              });
            }
          }
        }
      },
      defineSlot(adUnitPath: string, _sizes: unknown, elementId: string) {
        const existing = slots.get(elementId);
        const slot = existing ?? createSlot(elementId, adUnitPath);
        physicalSlots.add(slot);
        return slot;
      },
      getConfig() {
        return { disableInitialLoad: initialLoadDisabled };
      },
      pubads: () => pubadsService,
      setConfig(config: { disableInitialLoad?: unknown }) {
        if (typeof config?.disableInitialLoad === "boolean") {
          initialLoadDisabled = config.disableInitialLoad;
        }
      },
    };
    const references = {
      commandPush: commandQueue.push,
      display: googletag.display,
      defineSlot: googletag.defineSlot,
      refresh: pubadsService.refresh,
      fetch: window.fetch,
      xhrOpen: window.XMLHttpRequest.prototype.open,
      pushState: window.history.pushState,
      replaceState: window.history.replaceState,
    };

    function fictionalUniversalCreative(
      adId: string,
      lifecycleIndex: number,
    ): void {
      type OwnerResponse = {
        adId: string;
        renderer: string;
      };
      type FictionalPucWindow = Window & {
        __fictionalPucOutcome?: string;
        render?: (
          data: OwnerResponse,
          helper: object,
          creativeWindow: Window,
        ) => Promise<void>;
      };

      const pucWindow = window as FictionalPucWindow;
      pucWindow.__fictionalPucOutcome = "pending";
      const recordOutcome = (outcome: string): void => {
        pucWindow.__fictionalPucOutcome = outcome;
        (
          window.parent as Window & {
            __recordFictionalPucOutcome(index: number, value: string): void;
          }
        ).__recordFictionalPucOutcome(lifecycleIndex, outcome);
      };

      const prebidMessenger = (): Promise<OwnerResponse> =>
        new Promise((resolve, reject) => {
          const channel = new MessageChannel();
          const timeout = window.setTimeout(() => {
            channel.port1.close();
            reject(new Error("fictional PUC response timeout"));
          }, 5_000);
          channel.port1.onmessage = (event) => {
            window.clearTimeout(timeout);
            channel.port1.close();
            try {
              const response = JSON.parse(String(event.data)) as OwnerResponse;
              resolve(response);
            } catch (error) {
              reject(error);
            }
          };
          channel.port1.start();
          window.parent.postMessage(
            JSON.stringify({
              message: "Prebid Request",
              adId,
              adServerDomain: window.parent.location.host,
            }),
            "*",
            [channel.port2],
          );
        });

      const runDynamicRenderer = async (
        response: OwnerResponse,
      ): Promise<void> => {
        if (typeof response.renderer !== "string") {
          throw new Error("fictional PUC response refused");
        }
        window.eval(response.renderer);
        if (typeof pucWindow.render !== "function") {
          throw new Error("fictional PUC dynamic renderer unavailable");
        }
        const helper = {
          sendMessage(
            message: string,
            payload: Record<string, unknown>,
            callback: (event: MessageEvent) => void,
          ) {
            const channel = new MessageChannel();
            let active = true;
            channel.port1.onmessage = (event) => {
              if (active) callback(event);
            };
            channel.port1.start();
            window.parent.postMessage(
              JSON.stringify({
                message,
                adId: response.adId,
                ...payload,
              }),
              "*",
              [channel.port2],
            );
            return () => {
              active = false;
              channel.port1.close();
            };
          },
        };
        await pucWindow.render(response, helper, window);
      };

      void prebidMessenger()
        .then(runDynamicRenderer)
        .then(
          () => {
            recordOutcome("accepted");
          },
          (error: unknown) => {
            const message =
              error instanceof Error ? error.message : String(error);
            recordOutcome(`failed:${message}`);
          },
        );
    }

    const browserWindow = window as unknown as {
      googletag: typeof googletag;
      __recordFictionalPucOutcome(index: number, value: string): void;
      __gptDiagnosticsStub: {
        captureReferences(): void;
        emitNonemptyCompletionOnDisplay(value?: boolean): void;
        emitRequestStartOnDisplay(value?: boolean): void;
        displayCount(): number;
        emit(
          name: string,
          slotId: string,
          facts?: Record<string, unknown>,
        ): void;
        listenerCounts(): Record<string, number>;
        referenceOwnership(): {
          diagnosticsSafe: boolean;
          pairedHistoryWrappers: boolean;
        };
        refreshCount(): number;
        renderUniversalCreative(slotId: string, adId: string): void;
        slot(id: string, adUnitPath?: string): StubSlot;
        targeting(slotId: string, key: string): readonly string[];
        universalCreativeSnapshot(): Readonly<Record<string, unknown>>;
      };
    };
    browserWindow.googletag = googletag;
    browserWindow.__recordFictionalPucOutcome = (index, value) => {
      universalCreativeLifecycle[index] = value;
    };
    browserWindow.__gptDiagnosticsStub = {
      slot(id: string, adUnitPath = `/example/site/${id}`) {
        return slots.get(id) ?? createSlot(id, adUnitPath);
      },
      emit(name: string, slotId: string, facts: Record<string, unknown> = {}) {
        const slot = this.slot(slotId);
        emissions.push({
          name,
          listeners: listeners.get(name)?.size ?? 0,
          physical: physicalSlots.has(slot),
        });
        emit(name, slot, facts);
      },
      listenerCounts() {
        return Object.fromEntries(
          [...listeners.entries()].map(([name, registered]) => [
            name,
            registered.size,
          ]),
        );
      },
      displayCount() {
        return displayCalls.length;
      },
      emitRequestStartOnDisplay(value = true) {
        requestStartOnDisplay = value;
      },
      emitNonemptyCompletionOnDisplay(value = true) {
        nonemptyCompletionOnDisplay = value;
      },
      refreshCount() {
        return refreshCalls.length;
      },
      targeting(slotId: string, key: string) {
        return slots.get(slotId)?.getTargeting(key) ?? [];
      },
      renderUniversalCreative(slotId: string, adId: string) {
        const root = document.getElementById(slotId);
        if (!root) throw new Error(`missing fictional PUC slot: ${slotId}`);
        const frame = document.createElement("iframe");
        const lifecycleIndex = universalCreativeLifecycle.length;
        universalCreativeLifecycle.push("pending");
        frame.dataset.fictionalPuc = "";
        frame.srcdoc = `<!doctype html><body><script>(${String(fictionalUniversalCreative)})(${JSON.stringify(adId)},${lifecycleIndex});<\/script></body>`;
        root.appendChild(frame);
      },
      universalCreativeSnapshot() {
        const frames = Array.from(
          document.querySelectorAll<HTMLIFrameElement>(
            "iframe[data-fictional-puc]",
          ),
        );
        return {
          lifecycle: [...universalCreativeLifecycle],
          pucFrames: frames.length,
          ownerFrames: frames.map(
            (frame) =>
              frame.contentDocument?.querySelectorAll(
                'iframe[title="Ad content"]',
              ).length ?? -1,
          ),
          ownerSources: frames.map(
            (frame) =>
              frame.contentDocument?.querySelector<HTMLIFrameElement>(
                'iframe[title="Ad content"]',
              )?.srcdoc ?? "",
          ),
          emissions: [...emissions],
          listeners: this.listenerCounts(),
          displays: displayCalls.map((slot) => slot.getSlotElementId()),
          refreshes: refreshCalls.map((slots) =>
            slots.map((slot) => slot.getSlotElementId()),
          ),
          slotChildren: Array.from(
            document.getElementById(displayCalls[0]?.getSlotElementId() ?? "")
              ?.children ?? [],
          ).map((element) => ({
            tag: element.tagName,
            fictionalPuc:
              element instanceof HTMLIFrameElement &&
              element.dataset.fictionalPuc !== undefined,
            title: element.getAttribute("title"),
            adm:
              element instanceof HTMLIFrameElement &&
              element.srcdoc.includes("fictional creative"),
          })),
        };
      },
      captureReferences() {
        references.commandPush = commandQueue.push;
        references.display = googletag.display;
        references.defineSlot = googletag.defineSlot;
        references.refresh = pubadsService.refresh;
        references.fetch = window.fetch;
        references.xhrOpen = window.XMLHttpRequest.prototype.open;
        references.pushState = window.history.pushState;
        references.replaceState = window.history.replaceState;
      },
      referenceOwnership() {
        const pushStateChanged =
          window.history.pushState !== references.pushState;
        const replaceStateChanged =
          window.history.replaceState !== references.replaceState;
        return {
          diagnosticsSafe:
          commandQueue.push === references.commandPush &&
          googletag.display === references.display &&
          googletag.defineSlot === references.defineSlot &&
          pubadsService.refresh === references.refresh &&
          window.fetch === references.fetch &&
          window.XMLHttpRequest.prototype.open === references.xhrOpen,
          pairedHistoryWrappers: pushStateChanged && replaceStateChanged,
        };
      },
    };
  });
}
