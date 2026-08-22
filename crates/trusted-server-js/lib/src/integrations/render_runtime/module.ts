import {
  createBrowserMessagingAdapter,
  type MessagingValidationOptions,
} from '../../adapters/messaging';
import { parseTrustedServerAuctionResponseV1 } from '../../core/auction';
import {
  parseBidRenderSourceV1,
  parseBrowserAuctionProjectionV1,
} from '../../core/contracts/auction_projection';
import { validateRequestAdsOptions } from '../../core/contracts/request_ads';
import { log } from '../../core/log';
import { prepareAdmIframe } from '../../core/render';
import { createRenderTraceStore, type RenderTraceRuntimeOwner } from '../../core/trace';
import type { BootManifestV1, BrowserAuctionProjectionV1 } from '../../core/types';
import { DisposableStack } from '../../kernel/disposable';
import {
  createBrowserNavigationIdentityIssuer,
  mintBrowserLifecycleTicket,
} from '../../kernel/identity';
import {
  snapshotPersistentFirstDisplayAdoptionV1,
  type PersistentFirstDisplayAdoptionV1,
} from '../../shared/takeover';
import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  IntegrationRegistration,
} from '../../kernel/integration_registry';
import type {
  RuntimeAuctionContextContributor,
  RuntimeAuctionContextService,
  RuntimeCapabilityV1,
} from '../../kernel/runtime';
import { createDiagnosticsIngress } from '../../kernel/diagnostics';
import {
  createRuntimeSession,
  type NavigationSession,
  type RuntimeSession,
} from '../../kernel/sessions';
import {
  AdUnitRegistrationError,
  addAdUnitsResult,
  prepareProgrammaticAdUnits,
  serializeAuctionRequestBody,
} from '../../core/registry';
import { createAuctionBatchService } from '../../services/auction_batch';
import {
  createAuctionContextRegistry,
  type AuctionContextContributor,
  type AuctionContextRegistry,
  type ContextContributorOwner,
} from '../../services/context';
import {
  createPageBidsController,
  prepareInitialAuctionProjection,
  type ProjectionSlotRegistry,
} from '../../services/projections';
import { createReservationService, type ReservationService } from '../../services/reservations';
import type { PucGamAttemptInput } from '../../services/puc_bridge';
import {
  bindCommittedArtifactGuard,
  bindCommittedArtifactRetirement,
  createArtifactHostPositionLeaseRegistry,
  createCommittedArtifactStore,
  createBootstrapNonceRegistry,
  createRenderAttempt,
  createRendererNonceRegistry,
  createSlotOperation,
  renderDirectAdmAttempt,
  type ArtifactHostPositionLeaseRegistry,
  type CommittedArtifactStore,
  type CommittedRenderArtifact,
  type RenderAttempt,
} from '../../services/render';
import type { SlotRecord, SlotService } from '../../services/slots';
import { trustedDocumentHttpOrigin } from '../../shared/origin';

interface AcceptedBoot {
  readonly auctionProjection: unknown;
  readonly diagnostics: Readonly<{
    version: 1;
    renderTraceOverlay: boolean;
  }>;
  readonly manifest: Readonly<BootManifestV1>;
}

export type RegisteredRenderSource = 'aps';
export type RegisteredRenderer = (attempt: RenderAttempt, container: HTMLElement) => boolean;

function adoptionArray(candidate: unknown): readonly unknown[] | undefined {
  return Array.isArray(candidate) && Object.isFrozen(candidate) ? candidate : undefined;
}

function adoptionField(candidate: unknown, key: string): unknown {
  if (typeof candidate !== 'object' || candidate === null || Array.isArray(candidate)) {
    return undefined;
  }
  const descriptor = Object.getOwnPropertyDescriptor(candidate, key);
  return descriptor?.enumerable && 'value' in descriptor ? descriptor.value : undefined;
}

export interface AdoptedInitialRenderArtifactsV1 {
  readonly adoption: PersistentFirstDisplayAdoptionV1;
  readonly arm: () => void;
}

/** Adopt serialized replay suppression and diagnostics counters before new-epoch activity. */
export function adoptInitialRenderStateFromHandoff(
  candidate: unknown,
  navigation: Pick<NavigationSession, 'adoptFirstDisplayIdentityState' | 'generation'>,
  reservations: Pick<ReservationService, 'adoptFirstDisplayTombstones'>,
  trace: Pick<RenderTraceRuntimeOwner, 'adoptFirstDisplay'>
): PersistentFirstDisplayAdoptionV1 | undefined {
  const adoption = snapshotPersistentFirstDisplayAdoptionV1(candidate);
  if (!adoption) return undefined;
  const highWater = adoptionField(adoption.handoff, 'highWater');
  const tombstones = adoptionArray(adoptionField(adoption.handoff, 'tombstones'));
  const traceState = adoptionField(adoption.handoff, 'trace');
  const handoffSlots = adoptionArray(adoptionField(adoption.handoff, 'slots'));
  const cycles = adoptionArray(adoptionField(adoption.handoff, 'cycles'));
  const clockEpochMs = adoptionField(highWater, 'reservationClockEpochMs');
  const nextNavigationAttemptOrdinal = adoptionField(highWater, 'nextNavigationAttemptOrdinal');
  const nextAttemptOrdinal = adoptionField(highWater, 'nextAttemptOrdinal');
  const navigationAttemptPrefix = adoptionField(highWater, 'navigationAttemptPrefix');
  const nextSequence = adoptionField(traceState, 'nextSequence');
  const traceSlots = adoptionArray(adoptionField(traceState, 'slots'));
  if (
    typeof clockEpochMs !== 'number' ||
    typeof nextNavigationAttemptOrdinal !== 'number' ||
    typeof nextAttemptOrdinal !== 'number' ||
    typeof navigationAttemptPrefix !== 'string' ||
    typeof nextSequence !== 'number' ||
    !tombstones ||
    !traceSlots ||
    !handoffSlots ||
    !cycles
  ) {
    return undefined;
  }
  if (
    !navigation.adoptFirstDisplayIdentityState(
      navigationAttemptPrefix,
      Math.max(nextNavigationAttemptOrdinal, nextAttemptOrdinal)
    )
  ) {
    return undefined;
  }
  const reservationTombstones: Array<{
    expiresAtMs: number;
    reservationId: string;
  }> = [];
  for (const tombstone of tombstones) {
    const kind = adoptionField(tombstone, 'kind');
    if (kind !== 'reservation') continue;
    const reservationId = adoptionField(tombstone, 'value');
    const expiresAtMs = adoptionField(tombstone, 'expiresAtMs');
    if (typeof reservationId !== 'string' || typeof expiresAtMs !== 'number') return undefined;
    reservationTombstones.push({ expiresAtMs, reservationId });
  }
  const slots: Array<{
    bindings: readonly Readonly<{
      cycleOrdinal: number;
      historySequence: number;
      state: 'completed' | 'retired';
      token: string;
    }>[];
    impressions: number;
    records: readonly Readonly<{
      at: number;
      count: number;
      elementId: string;
      injected: true;
      path: 'ssat';
      rendered: true;
      seq: number;
      servedFrom: 'inline';
      slotId: string;
    }>[];
    slotId: string;
  }> = [];
  for (const slot of traceSlots) {
    const slotId = adoptionField(slot, 'slotId');
    const impressions = adoptionField(slot, 'impressions');
    const rawBindings = adoptionArray(adoptionField(slot, 'bindings'));
    const handoffSlot = handoffSlots.find((entry) => adoptionField(entry, 'id') === slotId);
    const cycle = cycles.find((entry) => adoptionField(entry, 'slotId') === slotId);
    const domId = adoptionField(handoffSlot, 'domId');
    if (
      typeof slotId !== 'string' ||
      typeof impressions !== 'number' ||
      !rawBindings ||
      !handoffSlot ||
      (rawBindings.length > 0 && (!cycle || typeof domId !== 'string'))
    ) {
      return undefined;
    }
    const bindings: Array<{
      cycleOrdinal: number;
      historySequence: number;
      state: 'completed' | 'retired';
      token: string;
    }> = [];
    const records: Array<{
      at: number;
      count: number;
      elementId: string;
      injected: true;
      path: 'ssat';
      rendered: true;
      seq: number;
      servedFrom: 'inline';
      slotId: string;
    }> = [];
    for (let index = 0; index < rawBindings.length; index += 1) {
      const binding = rawBindings[index];
      const atMs = adoptionField(binding, 'atMs');
      const cycleOrdinal = adoptionField(binding, 'cycleOrdinal');
      const historySequence = adoptionField(binding, 'historySequence');
      const state = adoptionField(binding, 'state');
      const token = adoptionField(binding, 'token');
      const cycleToken = adoptionField(cycle, 'token');
      const cycleRecords = adoptionArray(adoptionField(cycle, 'records'));
      if (
        typeof atMs !== 'number' ||
        typeof cycleOrdinal !== 'number' ||
        typeof historySequence !== 'number' ||
        (state !== 'completed' && state !== 'retired') ||
        typeof token !== 'string' ||
        cycleToken !== token ||
        !cycleRecords?.some(
          (record) =>
            adoptionField(record, 'ordinal') === cycleOrdinal &&
            adoptionField(record, 'state') === state
        )
      ) {
        return undefined;
      }
      bindings.push({ cycleOrdinal, historySequence, state, token });
      records.push({
        at: atMs,
        count: impressions - rawBindings.length + index + 1,
        elementId: domId as string,
        injected: true,
        path: 'ssat',
        rendered: true,
        seq: historySequence,
        servedFrom: 'inline',
        slotId,
      });
    }
    slots.push({ bindings, impressions, records, slotId });
  }
  if (
    !trace.adoptFirstDisplay({
      navigationGeneration: navigation.generation,
      nextSequence,
      slots,
    })
  ) {
    return undefined;
  }
  if (
    !reservations.adoptFirstDisplayTombstones({
      clockEpochMs,
      tombstones: reservationTombstones,
    })
  ) {
    return undefined;
  }
  return adoption;
}

/** Stage transferred frames in the persistent artifact store without arming rollback disposal. */
export function adoptInitialRenderArtifactsFromHandoff(
  candidate: unknown,
  navigation: NavigationSession,
  store: CommittedArtifactStore,
  hostPositions: ArtifactHostPositionLeaseRegistry,
  document: Document
): AdoptedInitialRenderArtifactsV1 | undefined {
  const adoption = snapshotPersistentFirstDisplayAdoptionV1(candidate);
  if (!adoption) return undefined;
  const cycles = adoptionArray(adoptionField(adoption.handoff, 'cycles'));
  const artifacts = adoptionArray(adoptionField(adoption.handoff, 'artifacts'));
  const handoffSlots = adoptionArray(adoptionField(adoption.handoff, 'slots'));
  if (
    !cycles ||
    !artifacts ||
    !handoffSlots ||
    adoption.identities.length !== cycles.length + artifacts.length ||
    new Set(adoption.identities).size !== adoption.identities.length
  ) {
    return undefined;
  }
  if (artifacts.length === 0) {
    return Object.freeze({ adoption, arm: () => undefined });
  }
  const Frame = document.defaultView?.HTMLIFrameElement;
  const Element = document.defaultView?.HTMLElement;
  const batch = navigation.createAuctionBatch('first-display-adoption');
  if (!Frame || !Element || !batch) return undefined;
  const promoted: CommittedRenderArtifact[] = [];
  let armed = false;
  try {
    for (let index = 0; index < artifacts.length; index += 1) {
      const record = artifacts[index];
      const slotId = adoptionField(record, 'slotId');
      const kind = adoptionField(record, 'kind');
      const owner = adoptionField(record, 'owner');
      const token = adoptionField(record, 'token');
      const hostPosition = adoptionField(record, 'hostPosition');
      const hostPositionPriority = adoptionField(record, 'hostPositionPriority');
      const identity = adoption.identities[cycles.length + index];
      const matchingSlots = handoffSlots.filter((entry) => adoptionField(entry, 'id') === slotId);
      const matchingCycles = cycles.filter((entry) => adoptionField(entry, 'slotId') === slotId);
      const domId = adoptionField(matchingSlots[0], 'domId');
      const host = typeof domId === 'string' ? document.getElementById(domId) : null;
      if (
        typeof slotId !== 'string' ||
        (kind !== 'aps' && kind !== 'gpt_adm') ||
        (owner !== 'trusted_server' && owner !== 'publisher') ||
        (owner === 'publisher' && kind !== 'gpt_adm') ||
        typeof token !== 'string' ||
        !/^r1_[A-Za-z0-9_-]{22}$/.test(token) ||
        (hostPosition === null && hostPositionPriority !== null) ||
        (hostPosition !== null &&
          (typeof hostPosition !== 'string' ||
            (hostPositionPriority !== '' && hostPositionPriority !== 'important'))) ||
        (kind === 'gpt_adm' && hostPosition !== null) ||
        matchingSlots.length !== 1 ||
        matchingCycles.length !== 1 ||
        !(host instanceof Element) ||
        !(identity instanceof Frame)
      ) {
        return undefined;
      }
      const previousHostPosition = hostPosition as string | null;
      const previousHostPositionPriority = hostPositionPriority as '' | 'important' | null;
      const frame = identity;
      const expectedParent = frame.parentNode;
      const expectedContentWindow = frame.contentWindow;
      const expectedSourceAttribute = frame.getAttribute('src');
      const expectedSource = frame.src;
      const expectedSourceDocumentAttribute = frame.getAttribute('srcdoc');
      const expectedSourceDocument = frame.srcdoc;
      const expectedSandbox = frame.getAttribute('sandbox');
      const expectedStyle = frame.getAttribute('style');
      if (
        !host.isConnected ||
        host.ownerDocument !== document ||
        !expectedParent ||
        (owner === 'trusted_server' && expectedParent !== host) ||
        !frame.isConnected ||
        frame.ownerDocument !== document ||
        !expectedContentWindow ||
        (kind === 'aps' &&
          hostPosition !== null &&
          (host.style.getPropertyValue('position') !== 'relative' ||
            host.style.getPropertyPriority('position') !== ''))
      ) {
        return undefined;
      }
      const attempt = batch.createRenderAttempt(slotId);
      if (!attempt.ok) return undefined;
      let disposed = false;
      const artifact: CommittedRenderArtifact = Object.freeze({
        kind: kind === 'aps' ? ('aps_mount' as const) : ('direct_iframe' as const),
        attemptId: attempt.value.id,
        slot: slotId,
        navigationGeneration: navigation.generation,
        dispose: (): void => {
          if (disposed) return;
          disposed = true;
          if (!armed || owner === 'publisher') return;
          try {
            frame.remove();
          } catch {
            // A publisher DOM replacement wins over persistent artifact retirement.
          }
          hostPositions.release(artifact);
        },
      });
      if (
        kind === 'aps' &&
        previousHostPosition !== null &&
        !hostPositions.bindOwned(
          artifact,
          host,
          previousHostPosition,
          previousHostPositionPriority ?? ''
        )
      ) {
        return undefined;
      }
      if (
        !bindCommittedArtifactGuard(artifact, () => {
          try {
            return (
              !disposed &&
              navigation.isCurrent() &&
              document.getElementById(domId as string) === host &&
              host.isConnected &&
              host.ownerDocument === document &&
              frame.parentNode === expectedParent &&
              frame.isConnected &&
              frame.ownerDocument === document &&
              frame.contentWindow === expectedContentWindow &&
              frame.getAttribute('src') === expectedSourceAttribute &&
              frame.src === expectedSource &&
              frame.getAttribute('srcdoc') === expectedSourceDocumentAttribute &&
              frame.srcdoc === expectedSourceDocument &&
              frame.getAttribute('sandbox') === expectedSandbox &&
              frame.getAttribute('style') === expectedStyle &&
              (kind !== 'aps' || previousHostPosition === null || hostPositions.current(artifact))
            );
          } catch {
            return false;
          }
        })
      ) {
        return undefined;
      }
      if (!store.promote(artifact, () => navigation.isCurrent())) return undefined;
      promoted.push(artifact);
    }
    return Object.freeze({
      adoption,
      arm: (): void => {
        armed = true;
      },
    });
  } finally {
    batch.dispose();
    if (promoted.length !== artifacts.length) {
      for (let index = promoted.length - 1; index >= 0; index -= 1) {
        const artifact = promoted[index];
        if (artifact) store.release(artifact);
      }
    }
  }
}

interface LocalSlotRecord {
  readonly directAuctionUnit?: Readonly<object>;
  readonly domAliases: readonly string[];
  readonly navigationGeneration: object;
  readonly registeredSlotId: string;
  readonly source: 'programmatic' | 'server';
}

interface RuntimeSlotBroker {
  readonly attach: (service: SlotService) => (() => void) | undefined;
  readonly register: SlotService['register'];
  readonly resolveDomAlias: SlotService['resolveDomAlias'];
  readonly resolveRegisteredSlot: SlotService['resolveRegisteredSlot'];
  readonly snapshotRegisteredSlots: SlotService['snapshotRegisteredSlots'];
  readonly dispose: () => void;
}

interface ApsMessagingValidationRegistration {
  readonly expectedPublisherOrigin: string;
  readonly expectedRendererUrl: string;
  readonly validateApsRenderer: (candidate: unknown) => boolean;
}

const APS_RENDERER_PATH = '/integrations/aps/renderer/v2';

function exactRuntimeCapability(
  interfaces: Readonly<Record<string, unknown>>
): RuntimeCapabilityV1 {
  const candidate = interfaces['runtime.v1'];
  if (
    typeof candidate !== 'object' ||
    candidate === null ||
    !Object.isFrozen(candidate) ||
    typeof (candidate as RuntimeCapabilityV1).attachAuctionContextService !== 'function' ||
    typeof (candidate as RuntimeCapabilityV1).boot !== 'function' ||
    typeof (candidate as RuntimeCapabilityV1).protectFirstDisplayAttemptBatch !== 'function' ||
    typeof (candidate as RuntimeCapabilityV1).registerAuctionContext !== 'function'
  ) {
    throw new TypeError('render_runtime requires runtime.v1');
  }
  return candidate as RuntimeCapabilityV1;
}

function acceptedBoot(runtime: RuntimeCapabilityV1): AcceptedBoot {
  const boot = runtime.boot();
  if (typeof boot !== 'object' || boot === null || !Object.isFrozen(boot)) {
    throw new TypeError('render_runtime boot is unavailable');
  }
  return boot as unknown as AcceptedBoot;
}

function slotResult(value: Record<string, unknown>): Readonly<Record<string, unknown>> {
  return Object.freeze(value);
}

function registerScopedContextContributor(
  registry: AuctionContextRegistry,
  runtimeOwner: RuntimeSession,
  integrationId: string,
  contributor: AuctionContextContributor
): (() => void) | undefined {
  let active = true;
  let releaseRegistration: (() => void) | undefined;
  const owner: ContextContributorOwner = Object.freeze({
    generation: Object.freeze({}),
    isCurrent: () => active && runtimeOwner.isCurrent(),
    onDispose: (kind: string, callback: () => void) => {
      if (kind !== 'auction-context-contributor' || !active || releaseRegistration) {
        throw new Error('Auction context contributor disposer is unavailable');
      }
      releaseRegistration = callback;
    },
  });
  if (!registry.register(integrationId, contributor, owner)) {
    active = false;
    releaseRegistration?.();
    return undefined;
  }
  return (): void => {
    if (!active) return;
    active = false;
    const release = releaseRegistration;
    releaseRegistration = undefined;
    release?.();
  };
}

function createRuntimeSlotBroker(): RuntimeSlotBroker {
  const local = new Map<string, LocalSlotRecord>();
  let localOwner: Parameters<SlotService['register']>[0] | undefined;
  let attached: SlotService | undefined;
  let disposed = false;
  const localSnapshot = (owner: Parameters<SlotService['snapshotRegisteredSlots']>[0]) =>
    !disposed && owner.isCurrent()
      ? (Object.freeze([...local.values()]) as unknown as readonly SlotRecord[])
      : undefined;
  return Object.freeze({
    attach: (service: SlotService): (() => void) | undefined => {
      if (disposed || attached || typeof service?.register !== 'function') return undefined;
      const records = [...local.values()];
      if (records.length > 0 && localOwner) {
        const transferred = service.register(
          localOwner,
          records.map((record) =>
            Object.freeze({
              ...(record.directAuctionUnit === undefined
                ? {}
                : { directAuctionUnit: record.directAuctionUnit }),
              domAliases: record.domAliases,
              registeredSlotId: record.registeredSlotId,
              source: record.source,
            })
          )
        );
        if (!transferred.ok) return undefined;
      }
      attached = service;
      local.clear();
      let released = false;
      return (): void => {
        if (released) return;
        released = true;
        if (attached !== service) return;
        const restored = localOwner
          ? (service.snapshotRegisteredSlots(localOwner) ??
            (records as unknown as readonly SlotRecord[]))
          : (records as unknown as readonly SlotRecord[]);
        attached = undefined;
        if (!disposed) {
          local.clear();
          for (const record of restored) {
            local.set(
              record.registeredSlotId,
              Object.freeze({
                ...(record.directAuctionUnit === undefined
                  ? {}
                  : { directAuctionUnit: record.directAuctionUnit }),
                domAliases: Object.freeze([...record.domAliases]),
                navigationGeneration: record.navigationGeneration,
                registeredSlotId: record.registeredSlotId,
                source: record.source,
              })
            );
          }
        }
      };
    },
    register: (
      owner: Parameters<SlotService['register']>[0],
      registrations: Parameters<SlotService['register']>[1]
    ) => {
      if (attached) {
        localOwner = owner;
        return attached.register(owner, registrations);
      }
      if (disposed || !owner.isCurrent()) {
        return Object.freeze({ ok: false as const, reason: 'stale_owner' as const });
      }
      if (localOwner && localOwner !== owner) {
        return Object.freeze({ ok: false as const, reason: 'stale_owner' as const });
      }
      if (local.size + registrations.length > 256) {
        return Object.freeze({ ok: false as const, reason: 'registry_capacity' as const });
      }
      const seen = new Set<string>();
      for (const registration of registrations) {
        if (
          typeof registration.registeredSlotId !== 'string' ||
          registration.registeredSlotId === '' ||
          local.has(registration.registeredSlotId) ||
          seen.has(registration.registeredSlotId)
        ) {
          return Object.freeze({ ok: false as const, reason: 'duplicate_slot' as const });
        }
        seen.add(registration.registeredSlotId);
      }
      const records = registrations.map((registration: (typeof registrations)[number]) =>
        Object.freeze({
          ...(registration.directAuctionUnit === undefined
            ? {}
            : { directAuctionUnit: registration.directAuctionUnit }),
          domAliases: Object.freeze([...(registration.domAliases ?? [])]),
          navigationGeneration: owner.generation,
          registeredSlotId: registration.registeredSlotId,
          source: registration.source,
        })
      );
      localOwner = owner;
      for (const record of records) local.set(record.registeredSlotId, record);
      return Object.freeze({
        ok: true as const,
        records: records as unknown as readonly SlotRecord[],
      });
    },
    resolveDomAlias: (alias: string) => {
      if (attached) return attached.resolveDomAlias(alias);
      let match: LocalSlotRecord | undefined;
      for (const record of local.values()) {
        if (!record.domAliases.includes(alias)) continue;
        if (match) return undefined;
        match = record;
      }
      return match as unknown as SlotRecord | undefined;
    },
    resolveRegisteredSlot: (id: string) =>
      attached?.resolveRegisteredSlot(id) ?? (local.get(id) as unknown as SlotRecord | undefined),
    snapshotRegisteredSlots: (owner: Parameters<SlotService['snapshotRegisteredSlots']>[0]) =>
      attached?.snapshotRegisteredSlots(owner) ?? localSnapshot(owner),
    dispose: () => {
      if (disposed) return;
      disposed = true;
      attached = undefined;
      localOwner = undefined;
      local.clear();
    },
  });
}

function exactApsMessagingValidation(
  candidate: unknown,
  publisherOrigin: string
): Readonly<ApsMessagingValidationRegistration> {
  try {
    if (
      typeof candidate !== 'object' ||
      candidate === null ||
      !Object.isFrozen(candidate) ||
      Object.getPrototypeOf(candidate) !== Object.prototype ||
      Object.getOwnPropertySymbols(candidate).length !== 0 ||
      Object.getOwnPropertyNames(candidate).sort().join(',') !==
        'expectedPublisherOrigin,expectedRendererUrl,validateApsRenderer'
    ) {
      throw new TypeError('APS message validation is malformed');
    }
    const validation = candidate as ApsMessagingValidationRegistration;
    const rendererUrl = new URL(validation.expectedRendererUrl);
    if (
      validation.expectedPublisherOrigin !== publisherOrigin ||
      rendererUrl.origin !== publisherOrigin ||
      rendererUrl.pathname !== APS_RENDERER_PATH ||
      rendererUrl.search !== '' ||
      rendererUrl.hash !== '' ||
      rendererUrl.href !== validation.expectedRendererUrl ||
      typeof validation.validateApsRenderer !== 'function'
    ) {
      throw new TypeError('APS message validation is malformed');
    }
    return Object.freeze({
      expectedPublisherOrigin: validation.expectedPublisherOrigin,
      expectedRendererUrl: validation.expectedRendererUrl,
      validateApsRenderer: validation.validateApsRenderer,
    });
  } catch (error) {
    if (error instanceof TypeError && error.message === 'APS message validation is malformed') {
      throw error;
    }
    const malformed = new TypeError('APS message validation is malformed');
    Object.defineProperty(malformed, 'cause', {
      configurable: true,
      value: error,
      writable: true,
    });
    throw malformed;
  }
}

/** Mandatory concrete provider for the shared browser runtime capabilities. */
export function createRenderRuntimeIntegrationRegistration(
  releaseId: string
): IntegrationRegistration {
  const prepare = (context: IntegrationPrepareContext) => {
    if (context.config !== undefined) {
      throw new TypeError('render_runtime does not accept integration config');
    }
    const runtime = exactRuntimeCapability(context.interfaces);
    const boot = acceptedBoot(runtime);
    const document = runtime.document;
    const view = document?.defaultView;
    if (!document || !view) throw new TypeError('render_runtime requires a browser document');
    const origin = trustedDocumentHttpOrigin(view.location.origin);
    if (!origin) throw new TypeError('render_runtime requires a trusted HTTP origin');
    const projection = prepareInitialAuctionProjection(boot.auctionProjection, (candidate) =>
      parseBrowserAuctionProjectionV1(candidate)
    ) as Readonly<BrowserAuctionProjectionV1> | undefined;
    if (!projection) throw new TypeError('render_runtime projection is invalid');

    const scope = new DisposableStack((error) => log.warn('render_runtime disposal failed', error));
    context.onDispose(() => scope.dispose());
    let active = false;
    scope.onDispose(() => {
      active = false;
    });

    const artifacts = createCommittedArtifactStore();
    scope.onDispose(() => artifacts.dispose());
    const hostPositions = createArtifactHostPositionLeaseRegistry();
    const ArtifactMutationObserver = view.MutationObserver;
    if (typeof ArtifactMutationObserver !== 'function') {
      throw new TypeError('render_runtime requires artifact DOM observation');
    }
    const artifactObserver = new ArtifactMutationObserver(() => {
      artifacts.sweep();
    });
    artifactObserver.observe(document, {
      attributeFilter: ['id', 'sandbox', 'src', 'srcdoc', 'style'],
      attributes: true,
      childList: true,
      subtree: true,
    });
    scope.onDispose(() => artifactObserver.disconnect());
    const slots = createRuntimeSlotBroker();
    scope.onDispose(() => slots.dispose());

    const renderTrace = createRenderTraceStore({
      onPresentationError: (error) => log.warn('render diagnostics presentation failed', error),
      onSubscriberError: (error) => log.warn('render diagnostics subscriber failed', error),
    });
    scope.onDispose(() => renderTrace.dispose());
    const consumeCoreObservation = (observation: Readonly<Record<string, unknown>>): void => {
      if (
        observation['kind'] === 'slotRequested' ||
        observation['kind'] === 'slotResponseReceived' ||
        observation['kind'] === 'slotRenderEnded' ||
        observation['kind'] === 'slotOnload' ||
        observation['kind'] === 'impressionViewable' ||
        observation['kind'] === 'slotVisibilityChanged'
      ) {
        renderTrace.observeGptFact(observation as never, (elementId) => {
          if (typeof elementId !== 'string' || elementId === '') return undefined;
          const slot = slots.resolveDomAlias(elementId) ?? slots.resolveRegisteredSlot(elementId);
          return slot?.traceToken
            ? Object.freeze({
                slotId: slot.registeredSlotId,
                elementId,
                navigationGeneration: slot.navigationGeneration,
                traceToken: slot.traceToken,
              })
            : undefined;
        });
        return;
      }
      if (
        observation['kind'] !== 'render_attempt' ||
        typeof observation['slotId'] !== 'string' ||
        (observation['path'] !== 'auction' && observation['path'] !== 'ssat') ||
        typeof observation['rendered'] !== 'boolean' ||
        typeof observation['injected'] !== 'boolean'
      ) {
        return;
      }
      const terminal = observation['outcome'];
      const terminalRecord =
        typeof terminal === 'object' && terminal !== null
          ? (terminal as Readonly<Record<string, unknown>>)
          : undefined;
      const attributableEmpty =
        observation['state'] === 'failed' &&
        terminalRecord?.['outcome'] === 'failed' &&
        terminalRecord['reason'] === 'gam_empty';
      if (observation['state'] !== 'accepted' && !attributableEmpty) return;
      if ((observation['state'] === 'accepted') !== observation['rendered']) return;
      const servedFrom = observation['servedFrom'];
      if (servedFrom !== undefined && servedFrom !== 'inline' && servedFrom !== 'pbs-cache') {
        return;
      }
      const slotId = observation['slotId'];
      const slot = slots.resolveRegisteredSlot(slotId);
      const optionalString = (name: 'adId' | 'bidId' | 'creativeId'): string | undefined => {
        const value = observation[name];
        return typeof value === 'string' && value !== '' ? value : undefined;
      };
      const adId = optionalString('adId');
      const bidId = optionalString('bidId');
      const creativeId = optionalString('creativeId');
      renderTrace.record({
        slotId,
        path: observation['path'],
        rendered: observation['rendered'],
        injected: observation['injected'],
        ...(slot?.domAliases[0] === undefined ? {} : { elementId: slot.domAliases[0] }),
        ...(adId === undefined ? {} : { adId }),
        ...(bidId === undefined ? {} : { bidId }),
        ...(creativeId === undefined ? {} : { creativeId }),
        ...(servedFrom === undefined ? {} : { servedFrom }),
      });
    };
    const diagnostics = createDiagnosticsIngress({
      reduce: consumeCoreObservation,
      reportError: (error) => log.warn('diagnostics reducer failed', error),
    });
    scope.onDispose(() => diagnostics.dispose());
    let apsValidation: Readonly<ApsMessagingValidationRegistration> | undefined;
    const messagingValidation: MessagingValidationOptions = Object.freeze({
      get expectedPublisherOrigin(): string {
        return apsValidation?.expectedPublisherOrigin ?? '';
      },
      get expectedRendererUrl(): string {
        return apsValidation?.expectedRendererUrl ?? '';
      },
      validateApsRenderer: (candidate: unknown): boolean =>
        apsValidation?.validateApsRenderer(candidate) === true,
    });
    const messaging = createBrowserMessagingAdapter(
      view as unknown as Parameters<typeof createBrowserMessagingAdapter>[0],
      messagingValidation
    );
    scope.onDispose(() => {
      apsValidation = undefined;
    });
    const reservations = createReservationService({
      now: () => view.performance.now(),
      prepareRenderSource: (candidate) => {
        const source = parseBidRenderSourceV1(candidate);
        return source?.type === 'pbs_cache' ? undefined : source;
      },
    });
    scope.onDispose(() => reservations.dispose());
    const bootstrapNonces = createBootstrapNonceRegistry();
    scope.onDispose(() => bootstrapNonces.dispose());
    const rendererNonces = createRendererNonceRegistry();
    scope.onDispose(() => rendererNonces.dispose());
    const session = createRuntimeSession({
      createIdentityIssuer: createBrowserNavigationIdentityIssuer,
      interfaces: Object.freeze({}),
      onNavigationDispose: (generation) => {
        artifacts.disposeNavigation(generation);
        renderTrace.pruneNavigation(generation);
        for (const slotId of Object.keys(renderTrace.diagnostics.current())) {
          renderTrace.prune(slotId);
        }
      },
    });
    scope.onDispose(() => session.dispose());
    const contextRegistry = createAuctionContextRegistry({
      manifestIntegrationIds: Object.freeze(boot.manifest.integrations.map(({ id }) => id)),
      onContributorFailure: (failure) => log.warn('auction context contributor failed', failure),
      runtimeOwner: session,
    });
    scope.onDispose(() => contextRegistry.dispose());
    const auctionContextService: RuntimeAuctionContextService = Object.freeze({
      register: (integrationId: string, contributor: RuntimeAuctionContextContributor) =>
        registerScopedContextContributor(contextRegistry, session, integrationId, contributor),
    });
    const navigationResult = session.startInitialNavigation(projection);
    if (!navigationResult.ok) throw new TypeError(navigationResult.reason);
    const navigation = navigationResult.value;
    if (
      !slots.register(
        navigation,
        projection.slots.map((placement) =>
          Object.freeze({
            domAliases: Object.freeze([placement.divId]),
            registeredSlotId: placement.slot,
            source: 'server' as const,
          })
        )
      ).ok
    ) {
      throw new TypeError('render_runtime projection slots are invalid');
    }
    const registeredRenderers = new Map<RegisteredRenderSource, RegisteredRenderer>();
    scope.onDispose(() => registeredRenderers.clear());
    let pucGamAttemptRegistrar: ((input: PucGamAttemptInput) => boolean) | undefined;
    scope.onDispose(() => {
      pucGamAttemptRegistrar = undefined;
    });
    const createAttempt = (
      owner: Parameters<typeof createRenderAttempt>[0]['owner'],
      parentAttemptId?: string
    ) =>
      createRenderAttempt({
        artifacts,
        owner,
        ...(parentAttemptId === undefined ? {} : { parentAttemptId }),
        prepareRenderSource: (candidate) => {
          const source = parseBidRenderSourceV1(candidate);
          return source?.type === 'pbs_cache' ? undefined : source;
        },
        publishDiagnostics: diagnostics.publish,
        reservations,
      });
    const resolveContainer = (slot: string): HTMLElement | undefined => {
      const record = slots.resolveRegisteredSlot(slot);
      if (!record) return undefined;
      const identifiers =
        record.source === 'programmatic'
          ? new Set([record.registeredSlotId])
          : new Set(record.domAliases);
      const matches = [...document.querySelectorAll<HTMLElement>('[id]')].filter((element) =>
        identifiers.has(element.id)
      );
      return matches.length === 1 ? matches[0] : undefined;
    };
    const renderWinner = (attempt: RenderAttempt): boolean => {
      const container = resolveContainer(attempt.slot);
      if (!container) {
        attempt.fail('slot_unresolved');
        return false;
      }
      if (attempt.renderSource?.type === 'adm') {
        return renderDirectAdmAttempt({
          attempt,
          container,
          prepareIframe: prepareAdmIframe,
          publisherOrigin: origin,
        });
      }
      if (attempt.renderSource?.type === 'aps') {
        const renderer = registeredRenderers.get('aps');
        if (renderer) return renderer(attempt, container);
      }
      attempt.fail('descriptor_invalid');
      return false;
    };
    const batches = createAuctionBatchService({
      createAttempt,
      fetcher: (input, init) => globalThis.fetch(input, init),
      parseResponse: parseTrustedServerAuctionResponseV1,
      renderWinner,
    });
    scope.onDispose(() => batches.dispose());

    const addAdUnits = (candidate: unknown): unknown => {
      if (!active || !navigation.isCurrent()) throw new AdUnitRegistrationError('slot_collision');
      const snapshot = slots.snapshotRegisteredSlots(navigation);
      if (!snapshot) throw new AdUnitRegistrationError('slot_collision');
      const prepared = prepareProgrammaticAdUnits(
        candidate,
        new Set(snapshot.map(({ registeredSlotId }) => registeredSlotId))
      );
      const registration = slots.register(
        navigation,
        prepared.map((unit) =>
          Object.freeze({
            directAuctionUnit: unit,
            registeredSlotId: unit.code,
            source: 'programmatic' as const,
          })
        )
      );
      if (!registration.ok) {
        throw new AdUnitRegistrationError(
          registration.reason === 'registry_capacity' ? 'registry_capacity' : 'slot_collision'
        );
      }
      return addAdUnitsResult(prepared);
    };

    const requestAds = async (candidate?: unknown): Promise<unknown> => {
      const validated = validateRequestAdsOptions(candidate);
      const snapshot = slots.snapshotRegisteredSlots(navigation);
      const requested = Object.freeze(
        validated.slots
          ? [...validated.slots]
          : (snapshot ?? []).map(({ registeredSlotId }) => registeredSlotId)
      );
      if (!active || !navigation.isCurrent()) {
        return Object.freeze({
          slots: Object.freeze(
            requested.map((slot) =>
              slotResult({
                slot,
                path: 'primary',
                outcome: 'cancelled',
                reason: 'navigation_disposed',
              })
            )
          ),
        });
      }
      const recordsById = new Map(
        (snapshot ?? []).map((record) => [record.registeredSlotId, record])
      );
      const admitted = requested
        .map((slot) => recordsById.get(slot))
        .filter((record): record is NonNullable<typeof record> => record !== undefined);
      if (admitted.length === 0) {
        return Object.freeze({
          slots: Object.freeze(
            requested.map((slot) =>
              slotResult({
                slot,
                path: 'primary',
                outcome: 'failed',
                reason: 'slot_unresolved',
              })
            )
          ),
        });
      }
      const adUnits = admitted.map((record) => {
        return (
          record?.directAuctionUnit ??
          Object.freeze({
            code: record.registeredSlotId,
            mediaTypes: Object.freeze({}),
            bids: Object.freeze([]),
          })
        );
      });
      const requestBody = serializeAuctionRequestBody(adUnits, contextRegistry.snapshot());
      if (!requestBody) {
        return Object.freeze({
          slots: Object.freeze(
            requested.map((slot) =>
              slotResult({ slot, path: 'primary', outcome: 'failed', reason: 'internal_error' })
            )
          ),
        });
      }
      const batch = batches.create({
        navigation,
        requestBody,
        ...(validated.signal ? { signal: validated.signal } : {}),
        slots: Object.freeze(admitted.map(({ registeredSlotId }) => registeredSlotId)),
        timeoutMs: validated.timeoutMs,
      });
      runtime.protectFirstDisplayAttemptBatch([batch.result]);
      const result = await batch.result;
      const bySlot = new Map(result.slots.map((entry) => [entry.slot, entry]));
      return Object.freeze({
        slots: Object.freeze(
          requested.map(
            (slot) =>
              bySlot.get(slot) ??
              slotResult({
                slot,
                path: 'primary',
                outcome: 'failed',
                reason: 'slot_unresolved',
              })
          )
        ),
      });
    };

    const slotsCapability = Object.freeze({
      attachPhysicalService: (candidate: unknown): (() => void) => {
        if (!active) throw new TypeError('GPT slot service provider is inactive');
        const service = candidate as SlotService;
        if (
          typeof service !== 'object' ||
          service === null ||
          !Object.isFrozen(service) ||
          typeof service.register !== 'function'
        ) {
          throw new TypeError('GPT slot service provider is invalid');
        }
        const release = slots.attach(service);
        if (!release) throw new TypeError('GPT slot service provider is duplicated');
        return release;
      },
      resolve: (id: string) => slots.resolveRegisteredSlot(id),
      snapshot: () => slots.snapshotRegisteredSlots(navigation) ?? Object.freeze([]),
    });
    const auctionCapability = Object.freeze({ batches, navigation, projection, session });
    const renderCapability = Object.freeze({
      attachPucGamAttemptRegistrar: (
        registrar: (input: PucGamAttemptInput) => boolean
      ): (() => void) => {
        if (!active || typeof registrar !== 'function' || pucGamAttemptRegistrar) {
          throw new TypeError('PUC GAM attempt registrar is unavailable or duplicated');
        }
        pucGamAttemptRegistrar = registrar;
        let released = false;
        return (): void => {
          if (released) return;
          released = true;
          if (pucGamAttemptRegistrar === registrar) pucGamAttemptRegistrar = undefined;
        };
      },
      artifacts,
      bindArtifactGuard: bindCommittedArtifactGuard,
      bindArtifactRetirement: bindCommittedArtifactRetirement,
      bootstrapNonces,
      hostPositions,
      createAttempt,
      createSlotOperation,
      commitPageBids: (
        owner: NavigationSession,
        slotRegistry: ProjectionSlotRegistry,
        candidate: unknown
      ): boolean =>
        createPageBidsController({
          navigation: owner,
          parseProjection: (value) => parseBrowserAuctionProjectionV1(value),
          slotRegistry,
        }).commit(candidate).status === 'committed',
      navigation,
      mintLifecycleTicket: mintBrowserLifecycleTicket,
      projection,
      prepareAdmIframe,
      renderWinner,
      rendererNonces,
      reservations,
      publisherOrigin: origin,
      registerRenderer: (type: RegisteredRenderSource, renderer: RegisteredRenderer) => {
        if (!active) throw new TypeError('render source provider is inactive');
        if (type !== 'aps' || typeof renderer !== 'function' || registeredRenderers.has(type)) {
          throw new TypeError('render source provider is invalid or duplicated');
        }
        registeredRenderers.set(type, renderer);
        let released = false;
        return (): void => {
          if (released) return;
          released = true;
          if (registeredRenderers.get(type) === renderer) registeredRenderers.delete(type);
        };
      },
      registerPucGamAttempt: (input: PucGamAttemptInput): boolean => {
        if (!active) return false;
        const registrar = pucGamAttemptRegistrar;
        if (!registrar) return false;
        try {
          return registrar(input) === true;
        } catch {
          return false;
        }
      },
    });
    const messagesCapability = Object.freeze({
      messaging,
      registerApsValidation: (candidate: unknown): (() => void) => {
        if (!active) throw new TypeError('APS message validation provider is inactive');
        if (apsValidation) throw new TypeError('APS message validation provider is duplicated');
        const validation = exactApsMessagingValidation(candidate, origin);
        apsValidation = validation;
        let released = false;
        return (): void => {
          if (released) return;
          released = true;
          if (apsValidation === validation) apsValidation = undefined;
        };
      },
    });
    const traceCapability = Object.freeze({
      record: renderTrace.record,
      enrich: renderTrace.enrich,
      prune: renderTrace.prune,
      diagnostics: renderTrace.diagnostics,
      observations: Object.freeze({
        publish: diagnostics.publish,
      }),
    });
    const tracePresentationCapability = Object.freeze({
      attachPresentation: renderTrace.attachPresentation,
    });
    const directCapability = Object.freeze({
      addAdUnits,
      requestAds,
      diagnostics: Object.freeze({ renderTrace: renderTrace.diagnostics }),
    });

    return Object.freeze({
      activate: (activation: IntegrationActivationContext) => {
        if (active) throw new Error('render_runtime already activated');
        const adoptedState =
          activation.adoption === undefined
            ? undefined
            : adoptInitialRenderStateFromHandoff(
                activation.adoption,
                navigation,
                reservations,
                renderTrace
              );
        const adoptedArtifacts =
          activation.adoption === undefined
            ? undefined
            : adoptInitialRenderArtifactsFromHandoff(
                activation.adoption,
                navigation,
                artifacts,
                hostPositions,
                document
              );
        if (activation.adoption !== undefined && (!adoptedState || !adoptedArtifacts)) {
          throw new TypeError('render_runtime first-display adoption is invalid');
        }
        const releaseAuctionContext = runtime.attachAuctionContextService(auctionContextService);
        if (!releaseAuctionContext) {
          throw new TypeError('render_runtime auction context service is duplicated');
        }
        active = true;
        if (adoptedArtifacts) activation.afterCommit(adoptedArtifacts.arm);
        activation.onDispose(releaseAuctionContext);
        activation.onDispose(() => {
          active = false;
          apsValidation = undefined;
          pucGamAttemptRegistrar = undefined;
          registeredRenderers.clear();
        });
      },
      interfaces: Object.freeze({
        'slots.v1': slotsCapability,
        'auction.v1': auctionCapability,
        'render.v1': renderCapability,
        'messages.v1': messagesCapability,
        'trace.v1': traceCapability,
        'trace.presentation.v1': tracePresentationCapability,
        'direct.v1': directCapability,
      }),
    });
  };
  return Object.freeze({
    abi: 1 as const,
    id: 'render_runtime',
    phase: 'takeover' as const,
    releaseId,
    prepareSync: prepare,
    prepare,
  });
}
