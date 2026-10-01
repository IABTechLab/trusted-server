import type { Page } from "@playwright/test";

/** Install a deterministic documented-event GPT stub before publisher scripts run. */
export async function installGptStub(page: Page): Promise<void> {
    await page.addInitScript(() => {
        type StubSlot = {
            getSlotElementId(): string;
            getAdUnitPath(): string;
        };
        type StubEvent = { slot: StubSlot } & Record<string, unknown>;
        type StubListener = (event: StubEvent) => void;

        const listeners = new Map<string, StubListener[]>();
        const slots = new Map<string, StubSlot>();
        const pubadsService = {
            addEventListener(name: string, listener: StubListener) {
                const current = listeners.get(name) ?? [];
                current.push(listener);
                listeners.set(name, current);
            },
            refresh() {},
        };
        const commandQueue = {
            push(callback: () => void) {
                callback();
                return 1;
            },
        };
        const googletag = {
            cmd: commandQueue,
            display() {},
            defineSlot() {},
            pubads: () => pubadsService,
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

        const browserWindow = window as unknown as {
            googletag: typeof googletag;
            __gptDiagnosticsStub: {
                slot(id: string, adUnitPath?: string): StubSlot;
                emit(
                    name: string,
                    slotId: string,
                    facts?: Record<string, unknown>,
                ): void;
                listenerCounts(): Record<string, number>;
                captureReferences(): void;
                referencesUnchanged(): boolean;
            };
        };
        browserWindow.googletag = googletag;
        browserWindow.__gptDiagnosticsStub = {
            slot(id: string, adUnitPath = `/example/site/${id}`) {
                let slot = slots.get(id);
                if (!slot) {
                    slot = {
                        getSlotElementId: () => id,
                        getAdUnitPath: () => adUnitPath,
                    };
                    slots.set(id, slot);
                }
                return slot;
            },
            emit(
                name: string,
                slotId: string,
                facts: Record<string, unknown> = {},
            ) {
                const slot = this.slot(slotId);
                for (const listener of listeners.get(name) ?? []) {
                    listener({ slot, ...facts });
                }
            },
            listenerCounts() {
                return Object.fromEntries(
                    [...listeners.entries()].map(([name, registered]) => [
                        name,
                        registered.length,
                    ]),
                );
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
            referencesUnchanged() {
                return (
                    commandQueue.push === references.commandPush &&
                    googletag.display === references.display &&
                    googletag.defineSlot === references.defineSlot &&
                    pubadsService.refresh === references.refresh &&
                    window.fetch === references.fetch &&
                    window.XMLHttpRequest.prototype.open ===
                        references.xhrOpen &&
                    window.history.pushState === references.pushState &&
                    window.history.replaceState === references.replaceState
                );
            },
        };
    });
}
