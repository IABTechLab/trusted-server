"use client";

import { useLayoutEffect, useRef, useState } from "react";
import { usePathname, useRouter } from "next/navigation";

export type RealGptInventory = {
  registered: { path: string };
  unrelated: { path: string };
};
type Slot = {
  addService(service: Service): Slot;
  setTargeting(key: string, value: string): Slot;
  getSlotElementId(): string;
  getAdUnitPath(): string;
  getTargetingKeys(): string[];
  getTargeting(key: string): string[];
};
type Service = {
  disableInitialLoad(): void;
  enableSingleRequest(): void;
  refresh(slots: Slot[]): void;
  getSlots(): Slot[];
  addEventListener(
    type: string,
    callback: (event: {
      slot: Slot;
      creativeId?: number;
      sourceAgnosticCreativeId?: number;
      isEmpty?: boolean;
      size?: number[];
    }) => void,
  ): void;
};
type Handle = {
  cancel(): void;
  getStatus(): { status: string; reason?: string };
};
type Tag = {
  cmd: { push(callback: () => void): unknown };
  pubads(): Service;
  defineSlot(path: string, sizes: number[][], id: string): Slot | null;
  destroySlots(slots: Slot[]): boolean;
  display(id: string): void;
  enableServices(): void;
};
type Current = Window & {
  googletag: Tag;
  tsjs?: {
    navGeneration?: number;
    registerPublisherSlot(options: {
      slotId: string;
      gptSlot: Slot;
      navigationGeneration: number;
      eligibleFormats: number[][];
    }): Handle;
  };
  pbjs?: {
    addAdUnits(
      units: Array<{
        code: string;
        mediaTypes: { banner: { sizes: number[][] } };
        bids: Array<{ bidder: string; params: { placement: string } }>;
      }>,
    ): void;
    removeAdUnit(code: string): void;
  };
  realGptPublisher?: Controller;
  realGptObservation?: { events: Array<Record<string, unknown>> };
};
export type Controller = {
  ready: boolean;
  events: Array<Record<string, unknown>>;
  slots: Slot[];
  handles: Handle[];
  mount(id?: string, eligible?: boolean): void;
  replacePlaceholder(): void;
  remove(): Promise<void>;
  refresh(includeUnrelated?: boolean): void;
  cancel(): void;
  navigate(): Promise<void>;
  status(): unknown;
};
const current = () => window as unknown as Current;

export default function RealGptPublisher({
  inventory,
  legacy = false,
}: {
  inventory: RealGptInventory;
  legacy?: boolean;
}) {
  const pathname = usePathname();
  const router = useRouter();
  const [placement, setPlacement] = useState<{
    id: string;
    eligible: boolean;
  }>();
  const [placeholderRevision, setPlaceholderRevision] = useState(0);
  const live = useRef<{ slot: Slot; handle?: Handle }>();
  const unrelated = useRef<Slot>();
  const authoredUnits = useRef(new Set<string>());
  const controller = useRef<Controller>();
  const record = (type: string, details: Record<string, unknown> = {}) => {
    controller.current!.events.push({
      type,
      time: performance.now(),
      ...details,
    });
  };
  if (!controller.current)
    controller.current = {
      ready: false,
      events: [],
      slots: [],
      handles: [],
      mount(id = "real-gpt-final", eligible = true) {
        setPlacement({ id, eligible });
      },
      replacePlaceholder() {
        setPlaceholderRevision((revision) => revision + 1);
      },
      async remove() {
        const instance = live.current;
        if (instance) {
          instance.handle?.cancel();
          record("cancelBeforeDestroy", {
            id: instance.slot.getSlotElementId(),
            connected: !!document.getElementById(
              instance.slot.getSlotElementId(),
            )?.isConnected,
          });
          await new Promise<void>((resolve, reject) =>
            current().googletag.cmd.push(() => {
              const destroyed = current().googletag.destroySlots([
                instance.slot,
              ]);
              const nativeStillRegistered = current()
                .googletag.pubads()
                .getSlots()
                .includes(instance.slot);
              record("destroyBeforeRemove", {
                destroyed,
                nativeStillRegistered,
                id: instance.slot.getSlotElementId(),
                connected: !!document.getElementById(
                  instance.slot.getSlotElementId(),
                )?.isConnected,
              });
              if (!destroyed || nativeStillRegistered) {
                reject(new Error("Actual GPT native slot destruction failed"));
                return;
              }
              resolve();
            }),
          );
          const code = instance.slot.getSlotElementId();
          if (authoredUnits.current.delete(code))
            current().pbjs?.removeAdUnit(code);
          live.current = undefined;
        }
        setPlacement(undefined);
      },
      refresh(includeUnrelated = false) {
        if (!live.current) throw new Error("No live registered slot");
        const targets = [
          live.current.slot,
          ...(includeUnrelated && unrelated.current ? [unrelated.current] : []),
        ];
        const pbjs = current().pbjs;
        if (!pbjs?.addAdUnits)
          throw new Error(
            "Actual Prebid library is unavailable for publisher refresh",
          );
        for (const slot of targets) {
          const code = slot.getSlotElementId();
          if (authoredUnits.current.has(code)) continue;
          pbjs.addAdUnits([
            {
              code,
              mediaTypes: { banner: { sizes: [[300, 250]] } },
              bids: [
                {
                  bidder: "fixture-bidder",
                  params: { placement: "fixture-header" },
                },
              ],
            },
          ]);
          authoredUnits.current.add(code);
          record("publisherPrebidUnit", { code, bidder: "fixture-bidder" });
        }
        current().googletag.pubads().refresh(targets);
      },
      cancel() {
        live.current?.handle?.cancel();
      },
      async navigate() {
        await this.remove();
        router.push("/publisher-registration-real-gpt/no-bid");
      },
      status() {
        return live.current?.handle?.getStatus() ?? null;
      },
    };

  useLayoutEffect(() => {
    const browser = current();
    if (browser.realGptObservation)
      controller.current!.events = browser.realGptObservation.events;
    browser.realGptPublisher = controller.current;
    browser.googletag.cmd.push(() => {
      const service = browser.googletag.pubads();
      if (!legacy) {
        service.disableInitialLoad();
        service.enableSingleRequest();
      }
      for (const type of browser.realGptObservation
        ? []
        : [
            "slotRequested",
            "slotResponseReceived",
            "slotRenderEnded",
            "slotOnload",
          ]) {
        service.addEventListener(type, (event) => {
          const slot = event.slot;
          record(type, {
            id: slot.getSlotElementId(),
            path: slot.getAdUnitPath(),
            targeting: Object.fromEntries(
              slot
                .getTargetingKeys()
                .map((key) => [key, slot.getTargeting(key)]),
            ),
            creativeId: event.creativeId ?? event.sourceAgnosticCreativeId,
            isEmpty: event.isEmpty,
            size: event.size,
          });
        });
      }
      if (!legacy) {
        unrelated.current = browser.googletag
          .defineSlot(
            inventory.unrelated.path,
            [[300, 250]],
            "real-gpt-unrelated",
          )
          ?.addService(service);
        if (!unrelated.current)
          throw new Error("Dedicated unrelated GPT slot definition failed");
      }
      browser.googletag.enableServices();
      if (!legacy) browser.googletag.display("real-gpt-unrelated");
      controller.current!.ready = true;
      record("serviceReady");
    });
    return () => {
      live.current?.handle?.cancel();
      if (live.current) browser.googletag.destroySlots([live.current.slot]);
      if (unrelated.current)
        browser.googletag.destroySlots([unrelated.current]);
      for (const code of Array.from(authoredUnits.current))
        browser.pbjs?.removeAdUnit(code);
      authoredUnits.current.clear();
      delete browser.realGptPublisher;
    };
  }, []);

  useLayoutEffect(() => {
    if (!placement) return;
    const browser = current();
    let disposed = false;
    const generation = browser.tsjs?.navGeneration ?? 0;
    browser.googletag.cmd.push(() => {
      if (disposed) return;
      if (!legacy && !browser.tsjs?.registerPublisherSlot)
        throw new Error("Actual Rust registration bootstrap is unavailable");
      const slot = browser.googletag
        .defineSlot(inventory.registered.path, [[300, 250]], placement.id)
        ?.addService(browser.googletag.pubads());
      if (!slot) throw new Error("Dedicated GPT slot definition failed");
      slot.setTargeting("publisher_section", "fiction");
      if (legacy) {
        live.current = { slot };
        controller.current!.slots.push(slot);
        record("legacyFinalNative", {
          id: placement.id,
          formats: [[300, 250]],
        });
        browser.googletag.display(placement.id);
        return;
      }
      const handle = browser.tsjs!.registerPublisherSlot({
        slotId: "fixture-header",
        gptSlot: slot,
        navigationGeneration: generation,
        eligibleFormats: placement.eligible ? [[300, 250]] : [],
      });
      live.current = { slot, handle };
      controller.current!.slots.push(slot);
      controller.current!.handles.push(handle);
      record("registered", {
        id: placement.id,
        generation,
        status: handle.getStatus(),
      });
    });
    return () => {
      disposed = true;
    };
  }, [placement]);

  return (
    <main>
      <h1>Dedicated real GPT publisher registration acceptance</h1>
      <p data-testid="real-gpt-route">{pathname}</p>
      <button onClick={() => controller.current!.mount()}>
        Mount final placement
      </button>
      <button onClick={() => void controller.current!.remove()}>
        Destroy then remove
      </button>
      <button onClick={() => controller.current!.refresh()}>
        Later publisher refresh
      </button>
      <div id="real-gpt-space">
        {placement ? (
          <div
            key={placement.id}
            id={placement.id}
            style={{ width: 300, minHeight: 250 }}
          />
        ) : (
          <div
            key={placeholderRevision}
            id={legacy ? "real-gpt-ssr" : "real-gpt-placeholder"}
            data-revision={placeholderRevision}
            style={legacy ? { width: 300, minHeight: 600 } : undefined}
          >
            Publisher has not registered
          </div>
        )}
      </div>
      <div id="real-gpt-unrelated" style={{ width: 300, minHeight: 250 }} />
    </main>
  );
}
