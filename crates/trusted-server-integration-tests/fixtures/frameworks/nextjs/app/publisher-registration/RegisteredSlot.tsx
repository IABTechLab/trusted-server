"use client";

import { useLayoutEffect, useRef, useState } from "react";
import { usePathname, useRouter } from "next/navigation";

export type Format = [number, number];
type RegistrationStatus = { status: string; reason?: string };
type Registration = { getStatus(): RegistrationStatus; cancel(): void };
type NativeSlot = {
  addService(service: Pubads): NativeSlot;
  getSlotElementId(): string;
  getAdUnitPath(): string;
  getTargeting(key: string): string[];
  setTargeting(key: string, value: string): NativeSlot;
};
type Pubads = {
  disableInitialLoad(): void;
  enableSingleRequest(): void;
  __tsRefreshWrapped?: boolean;
  refresh(slots: NativeSlot[], options?: { changeCorrelator: boolean }): void;
};
type Gpt = {
  cmd: { push(callback: () => void): unknown };
  pubads(): Pubads;
  defineSlot(path: string, formats: Format[], id: string): NativeSlot | null;
  destroySlots(slots: NativeSlot[]): boolean;
  enableServices(): void;
};
type Instance = {
  slot: NativeSlot;
  element: HTMLDivElement;
  generation: number;
  handles: Registration[];
  eligibleFormats: Format[];
};
type Lifecycle = { teardown(): Promise<void> };
export type PublisherController = {
  instances: Instance[];
  mount(id?: string): void;
  remove(): Promise<void>;
  navigate(path: string): Promise<void>;
  status(): RegistrationStatus | null;
  refresh(): void;
  cancel(): void;
};
type FixtureWindow = Window & {
  googletag: Gpt;
  fixture?: { record(event: string, details?: Record<string, unknown>): void };
  tsjs?: {
    navGeneration?: number;
    registerPublisherSlot(options: {
      slotId: string;
      gptSlot: NativeSlot;
      navigationGeneration: number;
      eligibleFormats: Format[];
    }): Registration;
  };
  publisherRegistrationFixture?: PublisherController;
};

function browser(): FixtureWindow {
  return window as unknown as FixtureWindow;
}

function formatsForBreakpoint(): Format[] {
  return window.matchMedia("(max-width: 600px)").matches
    ? [[300, 250]]
    : [
        [300, 600],
        [300, 250],
      ];
}

function sameFormats(left: Format[], right: Format[]): boolean {
  return (
    left.map(String).sort().join("|") === right.map(String).sort().join("|")
  );
}

function RegisteredSlot({
  id,
  controller,
  ready,
}: {
  id: string;
  controller: PublisherController;
  ready(lifecycle: Lifecycle): void;
}) {
  const elementRef = useRef<HTMLDivElement>(null);

  useLayoutEffect(() => {
    const current = browser();
    const element = elementRef.current!;
    // Capture before GPT work can queue. An old callback never borrows a new generation.
    const generation = current.tsjs?.navGeneration ?? 0;
    let disposed = false;
    let instance: Instance | undefined;
    const media = window.matchMedia("(max-width: 600px)");
    const record = (event: string, details: Record<string, unknown> = {}) =>
      current.fixture?.record(event, { id, generation, ...details });

    const offer = (formats: Format[]) => {
      if (!instance || disposed) return;
      const handle = current.tsjs!.registerPublisherSlot({
        slotId: "fixture-header",
        gptSlot: instance.slot,
        navigationGeneration: generation,
        eligibleFormats: formats,
      });
      instance.handles.push(handle);
      instance.eligibleFormats = formats;
      record("publisherRegistration", {
        eligibleFormats: formats,
        status: handle.getStatus(),
        handle: instance.handles.length - 1,
      });
    };

    // The publisher owns its effective size mapping. Update before foreground dispatch,
    // including when a hidden document changes breakpoint. No passive effect or TS poll.
    const updateEligibility = () => {
      if (!instance || disposed) return;
      const formats = formatsForBreakpoint();
      if (sameFormats(instance.eligibleFormats, formats)) return;
      const handle = instance.handles.at(-1)!;
      const status = handle.getStatus().status;
      if (status !== "waiting_payload" && status !== "waiting_delivery") {
        record("publisherMappingAfterDispatch", {
          status,
          eligibleFormats: formats,
        });
        return;
      }
      handle.cancel();
      record("publisherMappingCancelled", { status: handle.getStatus() });
      offer(formats);
    };

    const teardown = async () => {
      if (disposed) return;
      disposed = true;
      media.removeEventListener("change", updateEligibility);
      document.removeEventListener("visibilitychange", updateEligibility);
      instance?.handles.at(-1)?.cancel();
      record("publisherCancelBeforeDestroy", {
        connected: element.isConnected,
      });
      if (!instance) return;
      const slot = instance.slot;
      await new Promise<void>((resolve) => {
        current.googletag.cmd.push(() => {
          record("publisherDestroyBeforeRemove", {
            connected: element.isConnected,
          });
          current.googletag.destroySlots([slot]);
          resolve();
        });
      });
    };
    ready({ teardown });
    media.addEventListener("change", updateEligibility);
    document.addEventListener("visibilitychange", updateEligibility);
    current.googletag.cmd.push(() => {
      if (disposed || !element.isConnected) return;
      const service = current.googletag.pubads();
      if (!service.__tsRefreshWrapped) {
        throw new Error(
          "Fixture requires the real Prebid refresh wrapper before registration",
        );
      }
      const formats = formatsForBreakpoint();
      const slot = current.googletag
        .defineSlot("/123456789/fixture-header", formats, element.id)
        ?.addService(service);
      if (!slot) throw new Error("Fixture native slot definition failed");
      slot.setTargeting("publisher_section", "fiction");
      instance = {
        slot,
        element,
        generation,
        handles: [],
        eligibleFormats: formats,
      };
      controller.instances.push(instance);
      record("publisherNativeReady", {
        connected: element.isConnected,
        wrapperReady: true,
      });
      offer(formats);
    });
    return () => {
      // Parent-controlled teardown awaits this callback before React removes the node.
      // Layout cleanup is also safe for a deliberate navigation outside those controls.
      void teardown();
    };
  }, [controller, id, ready]);

  return (
    <div
      id={id}
      ref={elementRef}
      data-testid="final-placement"
      style={{ width: 300 }}
    />
  );
}

export default function RegisteredPublisher() {
  const router = useRouter();
  const pathname = usePathname();
  const [mounted, setMounted] = useState(false);
  const [finalId, setFinalId] = useState("publisher-final-header");
  const lifecycle = useRef<Lifecycle>();
  const controller = useRef<PublisherController>();
  const ready = useRef((value: Lifecycle) => {
    lifecycle.current = value;
  }).current;

  if (!controller.current) {
    controller.current = {
      instances: [],
      mount(id = "publisher-final-header") {
        setFinalId(id);
        setMounted(true);
      },
      async remove() {
        await lifecycle.current?.teardown();
        setMounted(false);
      },
      async navigate(path) {
        await lifecycle.current?.teardown();
        setMounted(false);
        router.push(path);
      },
      status() {
        return this.instances.at(-1)?.handles.at(-1)?.getStatus() ?? null;
      },
      refresh() {
        const slot = this.instances.at(-1)?.slot;
        if (slot)
          browser()
            .googletag.pubads()
            .refresh([slot], { changeCorrelator: false });
      },
      cancel() {
        this.instances.at(-1)?.handles.at(-1)?.cancel();
      },
    };
  }

  useLayoutEffect(() => {
    const current = browser();
    current.publisherRegistrationFixture = controller.current!;
    let unrelated: NativeSlot | undefined;
    current.googletag.cmd.push(() => {
      const service = current.googletag.pubads();
      service.disableInitialLoad();
      service.enableSingleRequest();
      unrelated = current.googletag
        .defineSlot("/123456789/unrelated", [[300, 250]], "publisher-unrelated")
        ?.addService(service);
      current.googletag.enableServices();
      current.fixture?.record("publisherServiceReady", { pathname });
    });
    return () => {
      if (unrelated) current.googletag.destroySlots([unrelated]);
      delete current.publisherRegistrationFixture;
    };
  }, []);

  return (
    <main>
      <h1>Publisher registration framework fixture</h1>
      <p data-testid="route">{pathname}</p>
      <button onClick={() => controller.current!.mount()}>
        Mount final placement
      </button>
      <button onClick={() => void controller.current!.remove()}>
        Destroy then remove
      </button>
      <button
        onClick={() =>
          void controller.current!.navigate(
            "/publisher-registration/registered-only",
          )
        }
      >
        Registered-only route
      </button>
      <button
        onClick={() =>
          void controller.current!.navigate("/publisher-registration")
        }
      >
        Mixed route
      </button>
      <button onClick={() => controller.current!.refresh()}>
        Later publisher refresh
      </button>
      <div id="fixture-header-space" data-testid="reserved-space">
        {mounted ? (
          <RegisteredSlot
            key={pathname}
            id={finalId}
            controller={controller.current}
            ready={ready}
          />
        ) : (
          <div id="fixture-header-placeholder" data-testid="ssr-placeholder">
            Publisher has not registered
          </div>
        )}
      </div>
      <div id="fixture-auto-placement" data-testid="automatic-placement" />
      <div id="publisher-unrelated" data-testid="unrelated-placement" />
    </main>
  );
}
