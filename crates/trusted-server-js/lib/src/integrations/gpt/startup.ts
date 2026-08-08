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
  readonly start: (config: unknown) => void;
}

export interface GptStartupOptions {
  readonly googletag: Pick<GoogletagAdapter, 'observePublisherCalls'>;
  readonly slots: () => GptPublisherSlotBoundary;
  readonly start?: (config: unknown) => void;
}

/** Join the sole GPT interception boundary to runtime-owned slot handoff state. */
export function createGptStartup(options: GptStartupOptions): GptStartup {
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
        refresh: (call: Readonly<GoogletagPublisherRefreshCall>) =>
          slots.preparePublisherRefresh(call),
      });
      return options.googletag.observePublisherCalls(observer);
    },
    start: (config: unknown): void => {
      options.slots().start();
      options.start?.(config);
    },
  });
}
