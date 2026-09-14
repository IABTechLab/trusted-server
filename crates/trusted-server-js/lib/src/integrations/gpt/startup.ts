import type {
  GoogletagAdapter,
  GoogletagPublisherCallObserver,
  GoogletagPublisherDefineSlotCall,
  GoogletagPublisherDestroySlotsCall,
  GoogletagPublisherDisplayCall,
  GoogletagPublisherRefreshCall,
} from '../../adapters/googletag';
import type { SlotService } from '../../services/slots';

type GptPublisherSlotBoundary = Pick<
  SlotService,
  | 'claimPublisherGptSlot'
  | 'preparePublisherDisplay'
  | 'preparePublisherRefresh'
  | 'recordPublisherDestruction'
  | 'start'
>;

export interface GptStartup {
  readonly activate: () => () => void;
  readonly installRefreshPolicy: (policy: GptRefreshPolicy) => (() => void) | undefined;
  readonly start: (config: unknown) => void;
}

/** One optional Prebid policy composed into the sole publisher refresh observer. */
export interface GptRefreshPolicy {
  readonly prepare: (
    call: Readonly<GoogletagPublisherRefreshCall>
  ) => PromiseLike<unknown> | undefined;
}

export interface GptStartupOptions {
  readonly googletag: Pick<GoogletagAdapter, 'observePublisherCalls'>;
  readonly slots: () => GptPublisherSlotBoundary;
  readonly start?: (config: unknown) => void;
}

/** Join the sole GPT interception boundary to runtime-owned slot handoff state. */
export function createGptStartup(options: GptStartupOptions): GptStartup {
  let refreshPolicy: GptRefreshPolicy | undefined;
  return Object.freeze({
    activate: (): (() => void) => {
      const slots = options.slots();
      const observer: GoogletagPublisherCallObserver = Object.freeze({
        defineSlot: (call: Readonly<GoogletagPublisherDefineSlotCall>) =>
          slots.claimPublisherGptSlot(call),
        destroySlots: ({ slots: destroyed }: Readonly<GoogletagPublisherDestroySlotsCall>) => {
          for (let index = 0; index < destroyed.length; index += 1) {
            const slot = destroyed[index];
            if (slot) slots.recordPublisherDestruction(slot);
          }
        },
        display: (call: Readonly<GoogletagPublisherDisplayCall>) =>
          slots.preparePublisherDisplay(call),
        refresh: (call: Readonly<GoogletagPublisherRefreshCall>) => {
          const decision = slots.preparePublisherRefresh(call);
          const policy = refreshPolicy;
          if (!policy || decision.action === 'suppress') return decision;
          const policySlots = decision.action === 'replace' ? decision.slots : call.slots;
          const policyCall =
            decision.action === 'replace'
              ? Object.freeze({
                  requestedSlots:
                    call.requestedSlots === undefined ? undefined : Object.freeze([...policySlots]),
                  slots: Object.freeze([...policySlots]),
                  options: call.options,
                })
              : call;
          let completion: PromiseLike<unknown> | undefined;
          try {
            completion = policy.prepare(policyCall);
          } catch {
            return decision;
          }
          if (!completion) return decision;
          return Object.freeze({
            action: 'defer' as const,
            ...(decision.admission ? { admission: decision.admission } : {}),
            completion,
            slots: Object.freeze([...policySlots]),
          });
        },
      });
      return options.googletag.observePublisherCalls(observer);
    },
    installRefreshPolicy: (policy: GptRefreshPolicy): (() => void) | undefined => {
      if (!policy || typeof policy.prepare !== 'function' || refreshPolicy) return undefined;
      refreshPolicy = policy;
      let active = true;
      return (): void => {
        if (!active) return;
        active = false;
        if (refreshPolicy === policy) refreshPolicy = undefined;
      };
    },
    start: (config: unknown): void => {
      options.slots().start();
      options.start?.(config);
    },
  });
}
