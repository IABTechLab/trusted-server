import {
  createBrowserGoogletagAdapter,
  createNoopGoogletagAdapter,
  type GoogletagAdapter,
  type GoogletagGlobalTarget,
} from '../adapters/googletag';
import {
  createBrowserMessagingAdapter,
  createNoopMessagingAdapter,
  type MessageEventTarget,
  type MessagingAdapter,
} from '../adapters/messaging';
import {
  createBrowserPrebidAdapter,
  createNoopPrebidAdapter,
  type PrebidAdapter,
  type PrebidGlobalTarget,
} from '../adapters/prebid';
import type { CoreActivationContext } from '../kernel/integration_registry';
import { createRuntime, type Runtime, type RuntimeOptions } from '../kernel/runtime';

export interface BrowserAdapters {
  readonly googletag: GoogletagAdapter;
  readonly messaging: MessagingAdapter;
  readonly prebid: PrebidAdapter;
}

export interface BrowserComposition {
  readonly adapters: Readonly<BrowserAdapters>;
}

export type BrowserAdapterTarget = GoogletagGlobalTarget & PrebidGlobalTarget & MessageEventTarget;

export interface BrowserCompositionOptions {
  readonly adapters?: Partial<BrowserAdapters>;
  readonly target?: BrowserAdapterTarget;
}

export interface BrowserRuntimeComposition extends BrowserComposition {
  readonly runtime: Runtime;
}

export interface BrowserCoreActivations {
  readonly bridgeRecognizer: (
    context: CoreActivationContext,
    adapters: Readonly<BrowserAdapters>
  ) => void;
  readonly correctnessGptListeners: (
    context: CoreActivationContext,
    adapters: Readonly<BrowserAdapters>
  ) => void;
}

export interface TestBrowserRuntimeCompositionOptions extends BrowserCompositionOptions {
  readonly coreActivations: BrowserCoreActivations;
}

/**
 * Construct concrete browser dependencies in one place.
 *
 * Task 6 keeps this test-only composition disconnected from the shipped core;
 * the coordinated production switch occurs only after the runtime is complete.
 */
export function createBrowserComposition(
  options: BrowserCompositionOptions = {}
): BrowserComposition {
  const googletag =
    options.adapters?.googletag ??
    (options.target
      ? createBrowserGoogletagAdapter(options.target)
      : createBrowserGoogletagAdapter());
  const messaging =
    options.adapters?.messaging ??
    (options.target
      ? createBrowserMessagingAdapter(options.target)
      : createBrowserMessagingAdapter());
  const prebid =
    options.adapters?.prebid ??
    (options.target ? createBrowserPrebidAdapter(options.target) : createBrowserPrebidAdapter());

  return Object.freeze({
    adapters: Object.freeze({ googletag, messaging, prebid }),
  });
}

/** Construct a side-effect-free dependency set for kernel and service tests. */
export function createNoopBrowserComposition(): BrowserComposition {
  return Object.freeze({
    adapters: Object.freeze({
      googletag: createNoopGoogletagAdapter(),
      messaging: createNoopMessagingAdapter(),
      prebid: createNoopPrebidAdapter(),
    }),
  });
}

/**
 * Construct the single runtime only for coordinated-cutover tests.
 *
 * The shipped core remains on its existing bootstrap until Task 19; keeping this
 * explicit prevents an import of the composition module from claiming globals.
 */
export function createTestBrowserRuntimeComposition(
  runtimeOptions: RuntimeOptions,
  compositionOptions: TestBrowserRuntimeCompositionOptions
): BrowserRuntimeComposition {
  const composition = createBrowserComposition(compositionOptions);
  const runtime = createRuntime({
    ...runtimeOptions,
    activateCore: (context) => {
      compositionOptions.coreActivations.bridgeRecognizer(context, composition.adapters);
      compositionOptions.coreActivations.correctnessGptListeners(context, composition.adapters);
      runtimeOptions.activateCore?.(context);
    },
  });
  return Object.freeze({ adapters: composition.adapters, runtime });
}
