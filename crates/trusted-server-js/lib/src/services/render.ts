import type { RenderAttemptScope, WinnerContext } from '../kernel/sessions';
import { mintBrowserRendererNonce } from '../kernel/identity';
import type { IdentityGenerationResult } from '../kernel/identity';
import { isReservationService } from './reservations';
import type {
  ReservationClaimAdmission,
  ReservationClaimExpectation,
  ReservationRenderSource,
  ReservationService,
} from './reservations';

const ATTEMPT_ID = /^a1_[A-Za-z0-9_-]{22}$/;
const RENDERER_NONCE = /^n1_[A-Za-z0-9_-]{22}$/;
const MAX_RENDERER_NONCES = 256;
const MAX_RENDERER_NONCE_DRAWS = 8;
const MAX_CACHE_BODY_BYTES = 512 * 1024;
const MAX_CACHE_URL_BYTES = 4096;
const CACHE_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const reflectApplyIntrinsic = Reflect.apply;
const directAdmDocument = typeof document === 'undefined' ? undefined : document;
const directAdmOwnerDocumentGetter =
  typeof Node === 'undefined'
    ? undefined
    : Object.getOwnPropertyDescriptor(Node.prototype, 'ownerDocument')?.get;
const objectFreezeIntrinsic = Object.freeze;
const objectIsFrozenIntrinsic = Object.isFrozen;
const objectGetPrototypeOfIntrinsic = Object.getPrototypeOf;
const objectGetOwnPropertyNamesIntrinsic = Object.getOwnPropertyNames;
const objectGetOwnPropertySymbolsIntrinsic = Object.getOwnPropertySymbols;
const objectGetOwnPropertyDescriptorIntrinsic = Object.getOwnPropertyDescriptor;
const objectCreateIntrinsic = Object.create;
const objectToStringIntrinsic = Object.prototype.toString;
const objectHasOwnIntrinsic = Object.prototype.hasOwnProperty;
const arrayIsArrayIntrinsic = Array.isArray;
const arrayIncludesIntrinsic = Array.prototype.includes;
const arrayPushIntrinsic = Array.prototype.push;
const arraySliceIntrinsic = Array.prototype.slice;
const arraySortIntrinsic = Array.prototype.sort;
const arraySpliceIntrinsic = Array.prototype.splice;
const mapGetIntrinsic = Map.prototype.get;
const mapSetIntrinsic = Map.prototype.set;
const mapDeleteIntrinsic = Map.prototype.delete;
const mapClearIntrinsic = Map.prototype.clear;
const mapEntriesIntrinsic = Map.prototype.entries;
const mapValuesIntrinsic = Map.prototype.values;
const mapSizeGetter = Object.getOwnPropertyDescriptor(Map.prototype, 'size')?.get as (
  this: Map<unknown, unknown>
) => number;
const mapEntryIteratorNextIntrinsic = Object.getPrototypeOf(new Map().entries()).next as (
  this: IterableIterator<unknown>
) => IteratorResult<unknown>;
const mapValueIteratorNextIntrinsic = Object.getPrototypeOf(new Map().values()).next as (
  this: IterableIterator<unknown>
) => IteratorResult<unknown>;
const setAddIntrinsic = Set.prototype.add;
const setHasIntrinsic = Set.prototype.has;
const setDeleteIntrinsic = Set.prototype.delete;
const setClearIntrinsic = Set.prototype.clear;
const setValuesIntrinsic = Set.prototype.values;
const setSizeGetter = Object.getOwnPropertyDescriptor(Set.prototype, 'size')?.get as (
  this: Set<unknown>
) => number;
const setValueIteratorNextIntrinsic = Object.getPrototypeOf(new Set().values()).next as (
  this: IterableIterator<unknown>
) => IteratorResult<unknown>;
const weakMapGetIntrinsic = WeakMap.prototype.get;
const weakMapSetIntrinsic = WeakMap.prototype.set;
const weakMapDeleteIntrinsic = WeakMap.prototype.delete;
const weakMapHasIntrinsic = WeakMap.prototype.has;
const weakSetAddIntrinsic = WeakSet.prototype.add;
const weakSetHasIntrinsic = WeakSet.prototype.has;
const weakSetDeleteIntrinsic = WeakSet.prototype.delete;
const promiseThenIntrinsic = Promise.prototype.then;
const numberIsFiniteIntrinsic = Number.isFinite;
const numberIsIntegerIntrinsic = Number.isInteger;
const regexpTestIntrinsic = RegExp.prototype.test;
const stringCharCodeAtIntrinsic = String.prototype.charCodeAt;
const stringIndexOfIntrinsic = String.prototype.indexOf;
const stringSliceIntrinsic = String.prototype.slice;
const stringTrimIntrinsic = String.prototype.trim;
const stringIntrinsic = String;
const jsonParseIntrinsic = JSON.parse;
const urlIntrinsic = URL;
type UrlTextProperty =
  | 'href'
  | 'protocol'
  | 'hostname'
  | 'username'
  | 'password'
  | 'origin'
  | 'port'
  | 'pathname'
  | 'search'
  | 'hash';

function captureUrlGetter(name: UrlTextProperty): ((this: URL) => string) | undefined {
  let prototype: object | null = URL.prototype;
  while (prototype) {
    const getter = Object.getOwnPropertyDescriptor(prototype, name)?.get;
    if (typeof getter === 'function') return getter as (this: URL) => string;
    prototype = Object.getPrototypeOf(prototype) as object | null;
  }
  return undefined;
}

const urlGetters: Readonly<Record<UrlTextProperty, ((this: URL) => string) | undefined>> =
  Object.freeze({
    href: captureUrlGetter('href'),
    protocol: captureUrlGetter('protocol'),
    hostname: captureUrlGetter('hostname'),
    username: captureUrlGetter('username'),
    password: captureUrlGetter('password'),
    origin: captureUrlGetter('origin'),
    port: captureUrlGetter('port'),
    pathname: captureUrlGetter('pathname'),
    search: captureUrlGetter('search'),
    hash: captureUrlGetter('hash'),
  });
const encodeURIComponentIntrinsic = encodeURIComponent;
const textEncoder = new TextEncoder();
const textEncoderEncodeIntrinsic = TextEncoder.prototype.encode;
const fatalTextDecoder = new TextDecoder('utf-8', { fatal: true });
const textDecoderDecodeIntrinsic = TextDecoder.prototype.decode;
const abortControllerIntrinsic = AbortController;
const abortControllerAbortIntrinsic = AbortController.prototype.abort;
const abortControllerSignalGetter = Object.getOwnPropertyDescriptor(
  AbortController.prototype,
  'signal'
)?.get as (this: AbortController) => AbortSignal;
const abortSignalAbortedGetter = Object.getOwnPropertyDescriptor(AbortSignal.prototype, 'aborted')
  ?.get as (this: AbortSignal) => boolean;
const artifactDisposals = new WeakMap<object, boolean>();
const committedArtifactStores = new WeakSet<object>();
const renderAttempts = new WeakSet<object>();
const cacheAttemptControls = new WeakMap<
  object,
  Readonly<{ begin: () => boolean; complete: () => boolean }>
>();
const ignoreAsyncDisposal = (): void => undefined;

function frozen<const Value extends object>(value: Value): Readonly<Value> {
  return reflectApplyIntrinsic(objectFreezeIntrinsic, Object, [value]) as Readonly<Value>;
}

function arrayPush<Value>(array: Value[], value: Value): number {
  return Reflect.apply(arrayPushIntrinsic, array, [value]) as number;
}

function utf8Length(value: string): number {
  return (reflectApplyIntrinsic(textEncoderEncodeIntrinsic, textEncoder, [value]) as Uint8Array)
    .byteLength;
}

function isFiniteNumber(value: number): boolean {
  return reflectApplyIntrinsic(numberIsFiniteIntrinsic, Number, [value]) as boolean;
}

function isInteger(value: number): boolean {
  return reflectApplyIntrinsic(numberIsIntegerIntrinsic, Number, [value]) as boolean;
}

function regexpTest(pattern: RegExp, value: string): boolean {
  return reflectApplyIntrinsic(regexpTestIntrinsic, pattern, [value]) as boolean;
}

function hasOwn(value: object, name: PropertyKey): boolean {
  return reflectApplyIntrinsic(objectHasOwnIntrinsic, value, [name]) as boolean;
}

function objectIsFrozen(value: object): boolean {
  return reflectApplyIntrinsic(objectIsFrozenIntrinsic, Object, [value]) as boolean;
}

function objectPrototype(value: object): object | null {
  return reflectApplyIntrinsic(objectGetPrototypeOfIntrinsic, Object, [value]) as object | null;
}

function ownPropertyNames(value: object): string[] {
  return reflectApplyIntrinsic(objectGetOwnPropertyNamesIntrinsic, Object, [value]) as string[];
}

function ownPropertySymbols(value: object): symbol[] {
  return reflectApplyIntrinsic(objectGetOwnPropertySymbolsIntrinsic, Object, [value]) as symbol[];
}

function ownPropertyDescriptor(value: object, name: PropertyKey): PropertyDescriptor | undefined {
  return reflectApplyIntrinsic(objectGetOwnPropertyDescriptorIntrinsic, Object, [value, name]) as
    PropertyDescriptor | undefined;
}

function isArray(value: unknown): value is unknown[] {
  return reflectApplyIntrinsic(arrayIsArrayIntrinsic, Array, [value]) as boolean;
}

function sortedStrings(values: string[]): string[] {
  return reflectApplyIntrinsic(arraySortIntrinsic, values, []) as string[];
}

function urlPart(url: URL, name: UrlTextProperty): string {
  const getter = urlGetters[name];
  if (typeof getter !== 'function') throw new TypeError('missing URL accessor');
  return reflectApplyIntrinsic(getter, url, []) as string;
}

function isUint8Array(value: unknown): value is Uint8Array {
  return (
    (typeof value === 'object' || typeof value === 'function') &&
    value !== null &&
    reflectApplyIntrinsic(objectToStringIntrinsic, value, []) === '[object Uint8Array]'
  );
}

function arraySlice<Value>(array: Value[]): Value[] {
  return Reflect.apply(arraySliceIntrinsic, array, [0]) as Value[];
}

function arraySpliceAll<Value>(array: Value[]): Value[] {
  return Reflect.apply(arraySpliceIntrinsic, array, [0, array.length]) as Value[];
}

function mapGet<Key, Value>(map: Map<Key, Value>, key: Key): Value | undefined {
  return Reflect.apply(mapGetIntrinsic, map, [key]) as Value | undefined;
}

function mapSet<Key, Value>(map: Map<Key, Value>, key: Key, value: Value): void {
  Reflect.apply(mapSetIntrinsic, map, [key, value]);
}

function mapDelete<Key, Value>(map: Map<Key, Value>, key: Key): boolean {
  return Reflect.apply(mapDeleteIntrinsic, map, [key]) as boolean;
}

function mapClear<Key, Value>(map: Map<Key, Value>): void {
  Reflect.apply(mapClearIntrinsic, map, []);
}

function mapSize<Key, Value>(map: Map<Key, Value>): number {
  return Reflect.apply(mapSizeGetter, map, []) as number;
}

function mapEntrySnapshot<Key, Value>(map: Map<Key, Value>): Array<[Key, Value]> {
  const iterator = Reflect.apply(mapEntriesIntrinsic, map, []) as IterableIterator<[Key, Value]>;
  const output: Array<[Key, Value]> = [];
  while (true) {
    const step = Reflect.apply(mapEntryIteratorNextIntrinsic, iterator, []) as IteratorResult<
      [Key, Value]
    >;
    if (step.done) return output;
    output[output.length] = step.value;
  }
}

function mapValueSnapshot<Key, Value>(map: Map<Key, Value>): Value[] {
  const iterator = Reflect.apply(mapValuesIntrinsic, map, []) as IterableIterator<Value>;
  const output: Value[] = [];
  while (true) {
    const step = Reflect.apply(
      mapValueIteratorNextIntrinsic,
      iterator,
      []
    ) as IteratorResult<Value>;
    if (step.done) return output;
    output[output.length] = step.value;
  }
}

function setAdd<Value>(set: Set<Value>, value: Value): void {
  Reflect.apply(setAddIntrinsic, set, [value]);
}

function setHas<Value>(set: Set<Value>, value: Value): boolean {
  return Reflect.apply(setHasIntrinsic, set, [value]) as boolean;
}

function setDelete<Value>(set: Set<Value>, value: Value): boolean {
  return Reflect.apply(setDeleteIntrinsic, set, [value]) as boolean;
}

function setClear<Value>(set: Set<Value>): void {
  Reflect.apply(setClearIntrinsic, set, []);
}

function setSize<Value>(set: Set<Value>): number {
  return Reflect.apply(setSizeGetter, set, []) as number;
}

function setValueSnapshot<Value>(set: Set<Value>): Value[] {
  const iterator = Reflect.apply(setValuesIntrinsic, set, []) as IterableIterator<Value>;
  const output: Value[] = [];
  while (true) {
    const step = Reflect.apply(
      setValueIteratorNextIntrinsic,
      iterator,
      []
    ) as IteratorResult<Value>;
    if (step.done) return output;
    output[output.length] = step.value;
  }
}

function weakMapGet<Key extends object, Value>(
  map: WeakMap<Key, Value>,
  key: Key
): Value | undefined {
  return Reflect.apply(weakMapGetIntrinsic, map, [key]) as Value | undefined;
}

function weakMapSet<Key extends object, Value>(
  map: WeakMap<Key, Value>,
  key: Key,
  value: Value
): void {
  Reflect.apply(weakMapSetIntrinsic, map, [key, value]);
}

function weakMapDelete<Key extends object, Value>(map: WeakMap<Key, Value>, key: Key): boolean {
  return Reflect.apply(weakMapDeleteIntrinsic, map, [key]) as boolean;
}

function weakMapHas<Key extends object, Value>(map: WeakMap<Key, Value>, key: Key): boolean {
  return Reflect.apply(weakMapHasIntrinsic, map, [key]) as boolean;
}

function weakSetAdd<Value extends object>(set: WeakSet<Value>, value: Value): void {
  Reflect.apply(weakSetAddIntrinsic, set, [value]);
}

function weakSetHas<Value extends object>(set: WeakSet<Value>, value: Value): boolean {
  return Reflect.apply(weakSetHasIntrinsic, set, [value]) as boolean;
}

function weakSetDelete<Value extends object>(set: WeakSet<Value>, value: Value): boolean {
  return Reflect.apply(weakSetDeleteIntrinsic, set, [value]) as boolean;
}

export const RENDER_FAILURE_REASONS = frozen([
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
  'cache_network_error',
  'cache_http_error',
  'cache_invalid_response',
  'adm_document_no_load',
  'abi_mismatch',
  'bundle_partial',
] as const);

export type RenderFailureReason = (typeof RENDER_FAILURE_REASONS)[number];

export const RENDER_CANCELLATION_REASONS = frozen([
  'caller_aborted',
  'superseded',
  'navigation_disposed',
] as const);

export type RenderCancellationReason = (typeof RENDER_CANCELLATION_REASONS)[number];

export type RenderOutcome =
  | Readonly<{ outcome: 'accepted' }>
  | Readonly<{ outcome: 'no_bid' }>
  | Readonly<{ outcome: 'failed'; reason: RenderFailureReason }>
  | Readonly<{ outcome: 'cancelled'; reason: RenderCancellationReason }>;

export type RenderAttemptActiveState =
  | 'created'
  | 'waiting_for_gam_and_claim'
  | 'waiting_for_owner'
  | 'waiting_for_insertion'
  | 'rendering_direct'
  | 'waiting_for_document'
  | 'waiting_for_aps_completion'
  | 'waiting_for_adm';

export type RenderAttemptState =
  RenderAttemptActiveState | 'accepted' | 'no_bid' | 'failed' | 'cancelled';

export interface CommittedRenderArtifact {
  readonly kind: 'direct_iframe' | 'puc';
  readonly attemptId: string;
  readonly slot: string;
  readonly navigationGeneration: object;
  readonly dispose: () => void;
}

export interface CommittedArtifactStore {
  readonly promote: (artifact: CommittedRenderArtifact, stillCurrent?: () => boolean) => boolean;
  readonly current: (slot: string) => CommittedRenderArtifact | undefined;
  readonly release: (artifact: CommittedRenderArtifact) => boolean;
  readonly disposeNavigation: (navigationGeneration: object) => void;
  readonly dispose: () => void;
}

export interface RenderDeadline {
  readonly milliseconds: number;
  readonly reason: RenderFailureReason;
}

export interface RenderScheduler {
  readonly set: (callback: () => void, milliseconds: number) => unknown;
  readonly clear: (handle: unknown) => void;
}

/** Fixed deadlines owned by the state transition that enters each wait. */
export const RENDER_STATE_DEADLINES: Readonly<
  Partial<Record<RenderAttemptActiveState, RenderDeadline>>
> = frozen({
  waiting_for_owner: frozen({
    milliseconds: 3_000,
    reason: 'owner_registration_timeout',
  }),
  waiting_for_insertion: frozen({
    milliseconds: 1_000,
    reason: 'owner_insertion_timeout',
  }),
  waiting_for_document: frozen({
    milliseconds: 3_000,
    reason: 'renderer_document_no_load',
  }),
  waiting_for_aps_completion: frozen({ milliseconds: 10_000, reason: 'runner_failed' }),
  waiting_for_adm: frozen({ milliseconds: 5_000, reason: 'adm_document_no_load' }),
});

const CACHE_FETCH_DEADLINE = frozen({
  milliseconds: 5_000,
  reason: 'cache_network_error' as const,
});

export interface RenderAttemptOptions {
  readonly owner: RenderAttemptScope;
  readonly artifacts: CommittedArtifactStore;
  readonly prepareRenderSource: (candidate: unknown) => ReservationRenderSource | undefined;
  readonly reservations: ReservationService;
  readonly parentAttemptId?: string;
  readonly publishDiagnostics?: (observation: RenderAttemptDiagnosticsObservation) => unknown;
  readonly scheduler?: RenderScheduler;
}

export interface RenderAttemptDiagnosticsObservation extends Readonly<Record<string, unknown>> {
  readonly adId?: string;
  readonly kind: 'render_attempt';
  readonly attemptId: string;
  readonly bidId?: string;
  readonly creativeId?: string;
  readonly injected: boolean;
  readonly slotId: string;
  readonly path: 'auction' | 'ssat';
  readonly rendered: boolean;
  readonly servedFrom?: 'inline' | 'pbs-cache';
  readonly state: 'accepted' | 'no_bid' | 'failed' | 'cancelled';
  readonly outcome: RenderOutcome;
}

export type RenderAttemptCreationResult =
  | Readonly<{ ok: true; value: RenderAttempt }>
  | Readonly<{
      ok: false;
      reason: 'identity_generation_failed' | 'invalid_attempt' | 'stale_owner';
    }>;

export interface RenderAttemptSnapshot {
  readonly history: readonly RenderAttemptState[];
  readonly outcome: RenderOutcome | undefined;
  readonly state: RenderAttemptState;
}

export interface RenderAttempt {
  readonly id: string;
  readonly slot: string;
  readonly generation: object;
  readonly navigationGeneration: object;
  readonly parentAttemptId: string | undefined;
  readonly renderSource: ReservationRenderSource | undefined;
  readonly winnerContext: WinnerContext | undefined;
  readonly admitDirectWinner: (source: unknown, context: WinnerContext) => boolean;
  readonly admitClaimedWinner: (source: unknown) => boolean;
  readonly beginGamClaim: () => boolean;
  readonly ownerClaimed: () => boolean;
  readonly ownerRegistered: () => boolean;
  readonly beginDirect: () => boolean;
  readonly beginApsDocument: (artifact: CommittedRenderArtifact) => boolean;
  readonly beginAdm: (artifact: CommittedRenderArtifact) => boolean;
  readonly apsDocumentAccepted: () => boolean;
  readonly accept: () => boolean;
  readonly noBid: () => boolean;
  readonly fail: (reason: RenderFailureReason) => boolean;
  readonly cancel: (reason: RenderCancellationReason) => boolean;
  readonly onSettled: (callback: (outcome: RenderOutcome) => void) => boolean;
  readonly snapshot: () => RenderAttemptSnapshot;
}

export interface DirectAdmAttemptOptions {
  readonly attempt: RenderAttempt;
  readonly container: HTMLElement;
  readonly prepareIframe: DirectAdmIframeConstructor;
  readonly publisherOrigin: string;
}

interface CacheFetchReader {
  readonly cancel?: () => Promise<unknown> | unknown;
  readonly read: () => Promise<Readonly<{ done: boolean; value: Uint8Array | undefined }>>;
  readonly releaseLock?: () => void;
}

interface CacheFetchResponse {
  readonly body: Readonly<{ getReader: () => CacheFetchReader }> | null;
  readonly ok: boolean;
  readonly type: Response['type'];
}

type CacheFetcher = (input: string, init: RequestInit) => Promise<unknown>;

export interface CacheAdmSource {
  readonly adm: string;
  readonly height: number;
  readonly type: 'adm';
  readonly version: 1;
  readonly width: number;
}

export interface CacheFetchPolicy {
  readonly baseUrl: string;
  readonly version: 1;
}

export interface CacheAdmResolutionOptions {
  readonly attempt: RenderAttempt;
  readonly cachePolicy: Readonly<CacheFetchPolicy>;
  readonly fetcher: CacheFetcher;
  readonly onResolved: (source: Readonly<CacheAdmSource>) => boolean;
}

export interface DirectCacheAttemptOptions extends DirectAdmAttemptOptions {
  readonly cachePolicy: Readonly<CacheFetchPolicy>;
  readonly fetcher: CacheFetcher;
}

export interface DirectAdmIframeHandle {
  readonly frame: HTMLIFrameElement;
  append(): boolean;
  activate(): boolean;
  commit(): boolean;
  current(): boolean;
  dispose(): void;
}

export type DirectAdmIframeConstructor = (options: {
  readonly adm: string;
  readonly container: HTMLElement;
  readonly height: number;
  readonly onError: () => void;
  readonly onLoad: () => void;
  readonly width: number;
}) => DirectAdmIframeHandle | undefined;

/** Retained endpoint whose lifetime is owned by one renderer nonce binding. */
export interface RendererNoncePort {
  readonly close: () => void;
}

export interface RendererNonceIssueInput {
  readonly attempt: RenderAttempt;
  readonly source?: object;
  readonly port: RendererNoncePort;
}

export interface RendererNonceExpectation extends RendererNonceIssueInput {
  readonly nonce: string;
  readonly generation: object;
  readonly source: object;
}

export type RendererNonceIssueResult =
  | Readonly<{ ok: true; nonce: string }>
  | Readonly<{
      ok: false;
      reason: 'capability_registry_full' | 'identity_generation_failed' | 'invalid_attempt';
    }>;

export interface RendererNonceRegistrySnapshot {
  readonly bindings: number;
  readonly disposed: boolean;
  readonly liveNonces: number;
}

export interface RendererNonceRegistry {
  /** On failure the caller retains port ownership; success transfers it to this registry. */
  readonly issue: (input: RendererNonceIssueInput) => RendererNonceIssueResult;
  /** Bind a pre-insertion nonce exactly once to the inserted renderer's WindowProxy. */
  readonly bindSource: (expectation: RendererNonceExpectation) => boolean;
  readonly consume: (expectation: RendererNonceExpectation) => boolean;
  readonly dispose: () => void;
  readonly snapshotForTest: () => RendererNonceRegistrySnapshot;
}

export interface RendererNonceRegistryOptions {
  readonly mintNonce?: () => IdentityGenerationResult<string>;
}

export interface SlotOperationResult {
  readonly path: 'primary' | 'fallback';
  readonly outcome: RenderOutcome;
  readonly primaryAttemptId: string;
  readonly primary: RenderOutcome;
  readonly fallbackAttemptId?: string;
  readonly fallback?: RenderOutcome;
}

export interface SlotOperationSnapshot {
  readonly settled: boolean;
  readonly result?: SlotOperationResult;
}

export interface SlotOperation {
  readonly snapshot: () => SlotOperationSnapshot;
  readonly onSettled: (callback: (result: SlotOperationResult) => void) => boolean;
}

/** Result of provenance-checking a primary attempt before operation subscription. */
export type SlotOperationCreationResult =
  Readonly<{ ok: true; value: SlotOperation }> | Readonly<{ ok: false; reason: 'invalid_attempt' }>;

export interface SlotOperationOptions {
  readonly primary: RenderAttempt;
  readonly createFallback?: (parentAttemptId: string) => RenderAttemptCreationResult;
}

function validAttemptId(value: unknown): value is string {
  return typeof value === 'string' && ATTEMPT_ID.test(value);
}

function validFailureReason(value: unknown): value is RenderFailureReason {
  return (
    typeof value === 'string' &&
    (Reflect.apply(arrayIncludesIntrinsic, RENDER_FAILURE_REASONS, [value]) as boolean)
  );
}

function validCancellationReason(value: unknown): value is RenderCancellationReason {
  return (
    typeof value === 'string' &&
    (Reflect.apply(arrayIncludesIntrinsic, RENDER_CANCELLATION_REASONS, [value]) as boolean)
  );
}

function validOutcome(value: unknown): value is RenderOutcome {
  try {
    if (
      typeof value !== 'object' ||
      value === null ||
      !objectIsFrozen(value) ||
      objectPrototype(value) !== Object.prototype ||
      ownPropertySymbols(value).length !== 0
    ) {
      return false;
    }
    const names = Object.getOwnPropertyNames(value);
    const outcome = Object.getOwnPropertyDescriptor(value, 'outcome');
    if (!outcome || !('value' in outcome) || outcome.enumerable !== true) return false;
    if (outcome.value === 'accepted' || outcome.value === 'no_bid') {
      return names.length === 1;
    }
    const reason = Object.getOwnPropertyDescriptor(value, 'reason');
    if (!reason || !('value' in reason) || reason.enumerable !== true || names.length !== 2) {
      return false;
    }
    return outcome.value === 'failed'
      ? validFailureReason(reason.value)
      : outcome.value === 'cancelled' && validCancellationReason(reason.value);
  } catch {
    return false;
  }
}

function permitsPromotion(stillCurrent: (() => boolean) | undefined): boolean {
  if (!stillCurrent) return true;
  try {
    return stillCurrent() === true;
  } catch {
    return false;
  }
}

function validArtifact(
  value: unknown,
  slot?: string,
  navigationGeneration?: object,
  attemptId?: string
): value is CommittedRenderArtifact {
  try {
    if ((typeof value !== 'object' && typeof value !== 'function') || value === null) return false;
    if (!Object.isFrozen(value) || Object.getPrototypeOf(value) !== Object.prototype) return false;
    const names = sortedStrings(ownPropertyNames(value));
    if (
      names.length !== 5 ||
      names[0] !== 'attemptId' ||
      names[1] !== 'dispose' ||
      names[2] !== 'kind' ||
      names[3] !== 'navigationGeneration' ||
      names[4] !== 'slot' ||
      Object.getOwnPropertySymbols(value).length !== 0
    ) {
      return false;
    }
    for (let index = 0; index < names.length; index += 1) {
      const name = names[index];
      if (!name) return false;
      const descriptor = Object.getOwnPropertyDescriptor(value, name);
      if (!descriptor || !('value' in descriptor) || !descriptor.enumerable) return false;
    }
    const artifact = value as CommittedRenderArtifact;
    return (
      (artifact.kind === 'direct_iframe' || artifact.kind === 'puc') &&
      validAttemptId(artifact.attemptId) &&
      typeof artifact.slot === 'string' &&
      artifact.slot.length > 0 &&
      (typeof artifact.navigationGeneration === 'object' ||
        typeof artifact.navigationGeneration === 'function') &&
      artifact.navigationGeneration !== null &&
      typeof artifact.dispose === 'function' &&
      (slot === undefined || artifact.slot === slot) &&
      (navigationGeneration === undefined ||
        artifact.navigationGeneration === navigationGeneration) &&
      (attemptId === undefined || artifact.attemptId === attemptId)
    );
  } catch {
    return false;
  }
}

function disposeArtifact(artifact: CommittedRenderArtifact | undefined): boolean {
  if (!artifact) return true;
  try {
    if (weakMapHas(artifactDisposals, artifact)) {
      return weakMapGet(artifactDisposals, artifact) === true;
    }
    weakMapSet(artifactDisposals, artifact, false);
  } catch {
    return false;
  }
  try {
    const result = Reflect.apply(artifact.dispose, artifact, []) as unknown;
    if ((typeof result === 'object' || typeof result === 'function') && result !== null) {
      try {
        Reflect.apply(promiseThenIntrinsic, result, [ignoreAsyncDisposal, ignoreAsyncDisposal]);
        return false;
      } catch {
        // Non-Promise thenables are contained through their own `then` method below.
      }
      let thenMethod: unknown;
      try {
        thenMethod = Reflect.get(result, 'then');
      } catch {
        return false;
      }
      if (typeof thenMethod === 'function') {
        try {
          Reflect.apply(thenMethod, result, [ignoreAsyncDisposal, ignoreAsyncDisposal]);
        } catch {
          // A hostile thenable is still an unsupported asynchronous disposer.
        }
        return false;
      }
    }
    if (result !== undefined) return false;
    weakMapSet(artifactDisposals, artifact, true);
    return true;
  } catch {
    return false;
  }
}

function artifactDisposalStarted(artifact: CommittedRenderArtifact): boolean {
  try {
    return weakMapHas(artifactDisposals, artifact);
  } catch {
    return true;
  }
}

/** Own committed artifacts independently from terminal attempt scopes. */
export function createCommittedArtifactStore(): CommittedArtifactStore {
  const entries = new Map<string, CommittedRenderArtifact>();
  const pendingNavigationDisposals = new Set<object>();
  const disposedNavigations = new WeakSet<object>();
  let disposed = false;
  let mutating = false;
  let disposeRequested = false;

  const disposeGeneration = (navigationGeneration: object): void => {
    const snapshot = mapEntrySnapshot(entries);
    for (let index = 0; index < snapshot.length; index += 1) {
      const entry = snapshot[index];
      if (!entry) continue;
      const slot = entry[0];
      const artifact = entry[1];
      if (
        artifact.navigationGeneration === navigationGeneration &&
        mapGet(entries, slot) === artifact
      ) {
        mapDelete(entries, slot);
        disposeArtifact(artifact);
      }
    }
  };

  const drainDeferredDisposal = (): void => {
    if (mutating) return;
    if (disposeRequested) {
      disposeRequested = false;
      const snapshot = mapValueSnapshot(entries);
      mapClear(entries);
      disposed = true;
      setClear(pendingNavigationDisposals);
      for (let index = 0; index < snapshot.length; index += 1) {
        disposeArtifact(snapshot[index]);
      }
      return;
    }
    const generations = setValueSnapshot(pendingNavigationDisposals);
    setClear(pendingNavigationDisposals);
    if (generations.length === 0) return;
    mutating = true;
    try {
      for (let index = 0; index < generations.length; index += 1) {
        const generation = generations[index];
        if (generation) disposeGeneration(generation);
      }
    } finally {
      mutating = false;
      if (disposeRequested || setValueSnapshot(pendingNavigationDisposals).length > 0) {
        drainDeferredDisposal();
      }
    }
  };

  const store: CommittedArtifactStore = {
    promote(artifact, stillCurrent): boolean {
      let ownsMutation = false;
      try {
        if (
          disposed ||
          mutating ||
          !validArtifact(artifact) ||
          artifactDisposalStarted(artifact) ||
          weakSetHas(disposedNavigations, artifact.navigationGeneration) ||
          !permitsPromotion(stillCurrent)
        ) {
          return false;
        }
        const existing = mapGet(entries, artifact.slot);
        if (existing === artifact) return false;
        mutating = true;
        ownsMutation = true;
        if (
          disposed ||
          disposeRequested ||
          weakSetHas(disposedNavigations, artifact.navigationGeneration) ||
          setHas(pendingNavigationDisposals, artifact.navigationGeneration) ||
          !permitsPromotion(stillCurrent)
        ) {
          return false;
        }
        if (existing) {
          if (!disposeArtifact(existing) || mapGet(entries, artifact.slot) !== existing)
            return false;
        }
        if (
          disposed ||
          disposeRequested ||
          weakSetHas(disposedNavigations, artifact.navigationGeneration) ||
          setHas(pendingNavigationDisposals, artifact.navigationGeneration) ||
          !permitsPromotion(stillCurrent) ||
          artifactDisposalStarted(artifact)
        ) {
          if (existing && mapGet(entries, artifact.slot) === existing) {
            mapDelete(entries, artifact.slot);
          }
          return false;
        }
        if (existing && mapGet(entries, artifact.slot) === existing) {
          mapDelete(entries, artifact.slot);
        }
        mapSet(entries, artifact.slot, artifact);
        return mapGet(entries, artifact.slot) === artifact;
      } catch {
        return false;
      } finally {
        if (ownsMutation) {
          mutating = false;
          drainDeferredDisposal();
        }
      }
    },
    current(slot): CommittedRenderArtifact | undefined {
      try {
        if (disposed || typeof slot !== 'string') return undefined;
        return mapGet(entries, slot);
      } catch {
        return undefined;
      }
    },
    release(artifact): boolean {
      let ownsMutation = false;
      try {
        if (disposed || mutating || !validArtifact(artifact)) return false;
        mutating = true;
        ownsMutation = true;
        if (mapGet(entries, artifact.slot) !== artifact) return false;
        mapDelete(entries, artifact.slot);
        const cleaned = disposeArtifact(artifact);
        return cleaned && mapGet(entries, artifact.slot) !== artifact;
      } catch {
        return false;
      } finally {
        if (ownsMutation) {
          mutating = false;
          drainDeferredDisposal();
        }
      }
    },
    disposeNavigation(navigationGeneration): void {
      let ownsMutation = false;
      try {
        if (disposed) return;
        weakSetAdd(disposedNavigations, navigationGeneration);
        setAdd(pendingNavigationDisposals, navigationGeneration);
        if (mutating) return;
        mutating = true;
        ownsMutation = true;
        setDelete(pendingNavigationDisposals, navigationGeneration);
        disposeGeneration(navigationGeneration);
      } catch {
        // Disposal is best-effort and never publishes a replacement artifact.
      } finally {
        if (ownsMutation) {
          mutating = false;
          drainDeferredDisposal();
        }
      }
    },
    dispose(): void {
      let ownsMutation = false;
      try {
        if (disposed) return;
        disposeRequested = true;
        if (mutating) return;
        mutating = true;
        ownsMutation = true;
        const snapshot = mapValueSnapshot(entries);
        mapClear(entries);
        disposeRequested = false;
        disposed = true;
        setClear(pendingNavigationDisposals);
        for (let index = 0; index < snapshot.length; index += 1) {
          disposeArtifact(snapshot[index]);
        }
      } catch {
        disposeRequested = false;
        disposed = true;
        try {
          mapClear(entries);
        } catch {
          // The store remains terminal even under collection corruption.
        }
      } finally {
        if (ownsMutation) mutating = false;
      }
    },
  };
  weakSetAdd(committedArtifactStores, store);
  return frozen(store);
}

function defaultScheduler(): RenderScheduler {
  return frozen({
    set: (callback: () => void, milliseconds: number): unknown =>
      globalThis.setTimeout(callback, milliseconds),
    clear: (handle: unknown): void => {
      globalThis.clearTimeout(handle as ReturnType<typeof globalThis.setTimeout>);
    },
  });
}

function terminalState(outcome: RenderOutcome): RenderAttemptState {
  return outcome.outcome;
}

/** Construct one path-independent attempt lifecycle around an issued owner scope. */
export function createRenderAttempt(options: RenderAttemptOptions): RenderAttemptCreationResult {
  let owner: RenderAttemptScope;
  let ownerForCleanup: unknown;
  let ownerDisposeForCleanup: unknown;
  let artifacts: CommittedArtifactStore;
  let id: string;
  let slot: string;
  let generation: object;
  let navigationGeneration: object;
  let parentAttemptId: string | undefined;
  let prepareRenderSource: (candidate: unknown) => ReservationRenderSource | undefined;
  let reservations: ReservationService;
  let publishDiagnostics:
    ((observation: RenderAttemptDiagnosticsObservation) => unknown) | undefined;
  let consumeClaimMethod: ReservationService['consumeClaim'];
  let ownerIsCurrentMethod: RenderAttemptScope['isCurrent'];
  let ownerDisposeMethod: RenderAttemptScope['dispose'];
  let ownerOnDisposeMethod: RenderAttemptScope['onDispose'];
  let ownerPrepareWinnerMethod: RenderAttemptScope['prepareWinnerContext'];
  let promoteArtifactMethod: CommittedArtifactStore['promote'];
  let currentArtifactMethod: CommittedArtifactStore['current'];
  let releaseArtifactMethod: CommittedArtifactStore['release'];
  const rejectConstruction = (
    reason: 'invalid_attempt' | 'stale_owner'
  ): RenderAttemptCreationResult => {
    try {
      if (
        ((typeof ownerForCleanup === 'object' && ownerForCleanup !== null) ||
          typeof ownerForCleanup === 'function') &&
        typeof ownerDisposeForCleanup === 'function'
      ) {
        Reflect.apply(ownerDisposeForCleanup, ownerForCleanup, []);
      }
    } catch {
      // Rejection remains authoritative even when issued-owner cleanup throws.
    }
    return frozen({ ok: false, reason });
  };
  try {
    owner = options.owner;
    ownerForCleanup = owner;
    ownerDisposeForCleanup = owner.dispose;
    ownerDisposeMethod = ownerDisposeForCleanup as RenderAttemptScope['dispose'];
    artifacts = options.artifacts;
    if (!weakSetHas(committedArtifactStores, artifacts)) {
      return rejectConstruction('invalid_attempt');
    }
    id = owner.id;
    slot = owner.slot;
    generation = owner.generation;
    navigationGeneration = owner.navigationGeneration;
    parentAttemptId = options.parentAttemptId;
    prepareRenderSource = options.prepareRenderSource;
    reservations = options.reservations;
    publishDiagnostics = options.publishDiagnostics;
    if (!isReservationService(reservations)) {
      return rejectConstruction('invalid_attempt');
    }
    consumeClaimMethod = reservations.consumeClaim;
    ownerIsCurrentMethod = owner.isCurrent;
    ownerOnDisposeMethod = owner.onDispose;
    ownerPrepareWinnerMethod = owner.prepareWinnerContext;
    promoteArtifactMethod = artifacts.promote;
    currentArtifactMethod = artifacts.current;
    releaseArtifactMethod = artifacts.release;
    if (
      !validAttemptId(id) ||
      typeof slot !== 'string' ||
      slot.length === 0 ||
      (typeof generation !== 'object' && typeof generation !== 'function') ||
      generation === null ||
      (typeof navigationGeneration !== 'object' && typeof navigationGeneration !== 'function') ||
      navigationGeneration === null ||
      generation === navigationGeneration ||
      typeof promoteArtifactMethod !== 'function' ||
      typeof currentArtifactMethod !== 'function' ||
      typeof releaseArtifactMethod !== 'function' ||
      typeof ownerIsCurrentMethod !== 'function' ||
      typeof ownerDisposeMethod !== 'function' ||
      typeof ownerOnDisposeMethod !== 'function' ||
      typeof ownerPrepareWinnerMethod !== 'function' ||
      typeof prepareRenderSource !== 'function' ||
      typeof consumeClaimMethod !== 'function' ||
      (publishDiagnostics !== undefined && typeof publishDiagnostics !== 'function') ||
      (parentAttemptId !== undefined &&
        (!validAttemptId(parentAttemptId) || parentAttemptId === id))
    ) {
      return rejectConstruction('invalid_attempt');
    }
  } catch {
    return rejectConstruction('invalid_attempt');
  }
  const ownerIsCurrent = (): boolean => {
    try {
      return Reflect.apply(ownerIsCurrentMethod, owner, []) === true;
    } catch {
      return false;
    }
  };
  const ownerIdentityIsCurrent = (): boolean => {
    try {
      return (
        owner.id === id &&
        owner.slot === slot &&
        owner.generation === generation &&
        owner.navigationGeneration === navigationGeneration &&
        ownerIsCurrent()
      );
    } catch {
      return false;
    }
  };
  if (!ownerIsCurrent()) return rejectConstruction('stale_owner');

  let scheduler: RenderScheduler;
  let schedulerSetMethod: RenderScheduler['set'];
  let schedulerClearMethod: RenderScheduler['clear'];
  try {
    scheduler = options.scheduler ?? defaultScheduler();
    schedulerSetMethod = scheduler.set;
    schedulerClearMethod = scheduler.clear;
    if (typeof schedulerSetMethod !== 'function' || typeof schedulerClearMethod !== 'function') {
      return rejectConstruction('invalid_attempt');
    }
  } catch {
    return rejectConstruction('invalid_attempt');
  }
  const history: RenderAttemptState[] = ['created'];
  const observers: Array<(outcome: RenderOutcome) => void> = [];
  let state: RenderAttemptState = 'created';
  let outcome: RenderOutcome | undefined;
  let pendingArtifact: CommittedRenderArtifact | undefined;
  let admittedRenderSource: ReservationRenderSource | undefined;
  let admittedWinnerContext: WinnerContext | undefined;
  let cacheFetchStarted = false;
  let deadlineHandle: unknown;
  let deadlineState: RenderAttemptActiveState | undefined;
  let settlingInternally = false;

  const prepareSource = (candidate: unknown): ReservationRenderSource | undefined => {
    try {
      const source = prepareRenderSource(candidate);
      if (!source || !Object.isFrozen(source)) return undefined;
      const type = Object.getOwnPropertyDescriptor(source, 'type');
      const version = Object.getOwnPropertyDescriptor(source, 'version');
      if (
        !type ||
        !('value' in type) ||
        (type.value !== 'aps' && type.value !== 'adm' && type.value !== 'cache') ||
        !version ||
        !('value' in version) ||
        version.value !== 1
      ) {
        return undefined;
      }
      return source;
    } catch {
      return undefined;
    }
  };

  const claimedSource = (
    admission: unknown,
    context: WinnerContext
  ): ReservationRenderSource | undefined => {
    try {
      if (
        typeof admission !== 'object' ||
        admission === null ||
        !Object.isFrozen(admission) ||
        Object.getPrototypeOf(admission) !== Object.prototype ||
        Object.getOwnPropertySymbols(admission).length !== 0
      ) {
        return undefined;
      }
      const names = Object.getOwnPropertyNames(admission).sort();
      const renderSource = Object.getOwnPropertyDescriptor(admission, 'renderSource');
      const winnerContext = Object.getOwnPropertyDescriptor(admission, 'winnerContext');
      if (
        names.length !== 2 ||
        names[0] !== 'renderSource' ||
        names[1] !== 'winnerContext' ||
        !renderSource ||
        !('value' in renderSource) ||
        renderSource.enumerable !== true ||
        !winnerContext ||
        !('value' in winnerContext) ||
        winnerContext.enumerable !== true ||
        winnerContext.value !== context
      ) {
        return undefined;
      }
      const source = renderSource.value as ReservationRenderSource;
      if (!Object.isFrozen(source)) return undefined;
      const type = Object.getOwnPropertyDescriptor(source, 'type');
      const version = Object.getOwnPropertyDescriptor(source, 'version');
      if (
        !type ||
        !('value' in type) ||
        (type.value !== 'aps' && type.value !== 'adm' && type.value !== 'cache') ||
        !version ||
        !('value' in version) ||
        version.value !== 1
      ) {
        return undefined;
      }
      return source;
    } catch {
      return undefined;
    }
  };

  const validWinnerContext = (context: unknown): context is WinnerContext => {
    try {
      if (
        (typeof context !== 'object' && typeof context !== 'function') ||
        context === null ||
        !objectIsFrozen(context) ||
        objectPrototype(context) !== Object.prototype ||
        ownPropertyNames(context).length !== 1 ||
        ownPropertySymbols(context).length !== 0
      ) {
        return false;
      }
      const selectedCpm = ownPropertyDescriptor(context, 'selectedCpm');
      return (
        !!selectedCpm &&
        'value' in selectedCpm &&
        selectedCpm.enumerable === true &&
        typeof selectedCpm.value === 'number' &&
        isFiniteNumber(selectedCpm.value) &&
        selectedCpm.value >= 0
      );
    } catch {
      return false;
    }
  };

  const readWinnerContext = (): WinnerContext | undefined => {
    try {
      return owner.winnerContext;
    } catch {
      return undefined;
    }
  };

  const admitDirectWinner = (candidate: unknown, context: WinnerContext): boolean => {
    if (
      state !== 'created' ||
      outcome !== undefined ||
      admittedRenderSource !== undefined ||
      admittedWinnerContext !== undefined ||
      !ownerIsCurrent() ||
      !validWinnerContext(context)
    ) {
      return false;
    }
    const source = prepareSource(candidate);
    if (!source) return false;
    let admission: ReturnType<RenderAttemptScope['prepareWinnerContext']>;
    try {
      admission = Reflect.apply(ownerPrepareWinnerMethod, owner, [context]);
    } catch {
      return false;
    }
    if (!admission) return false;
    let committed: boolean;
    try {
      committed = admission.commit() === true;
    } catch {
      committed = false;
    }
    if (!committed || !ownerIsCurrent() || readWinnerContext() !== context) {
      try {
        admission.rollback();
      } catch {
        // Failed admission retains no lifecycle source/context authority.
      }
      return false;
    }
    admittedRenderSource = source;
    admittedWinnerContext = context;
    return true;
  };

  const admitClaimedWinner = (candidate: unknown): boolean => {
    if (
      state !== 'waiting_for_gam_and_claim' ||
      outcome !== undefined ||
      admittedRenderSource !== undefined ||
      admittedWinnerContext !== undefined ||
      !ownerIsCurrent()
    ) {
      return false;
    }
    const context = readWinnerContext();
    if (!validWinnerContext(context)) return false;
    let admission: ReservationClaimAdmission | undefined;
    try {
      admission = Reflect.apply(consumeClaimMethod, reservations, [
        candidate,
        frozen<ReservationClaimExpectation>({
          attempt: owner,
          attemptId: id,
          slot,
          navigationGeneration,
          winnerContext: context,
        }),
      ]) as ReservationClaimAdmission | undefined;
    } catch {
      return false;
    }
    const source = claimedSource(admission, context);
    if (!source || !ownerIsCurrent() || readWinnerContext() !== context) return false;
    admittedRenderSource = source;
    admittedWinnerContext = context;
    return true;
  };

  const clearDeadline = (): void => {
    if (deadlineState === undefined) return;
    const handle = deadlineHandle;
    deadlineHandle = undefined;
    deadlineState = undefined;
    try {
      Reflect.apply(schedulerClearMethod, scheduler, [handle]);
    } catch {
      // A cleared logical deadline remains inert even when the host clear throws.
    }
  };

  const notify = (terminal: RenderOutcome): void => {
    const snapshot = arraySpliceAll(observers);
    for (let index = 0; index < snapshot.length; index += 1) {
      const observer = snapshot[index];
      if (!observer) continue;
      try {
        observer(terminal);
      } catch {
        // Observation cannot change the terminal result.
      }
    }
  };

  const settle = (terminal: RenderOutcome, disposeOwner: boolean): boolean => {
    if (outcome !== undefined) return false;
    const terminalRenderSource = admittedRenderSource;
    outcome = terminal;
    state = terminalState(terminal);
    arrayPush(history, state);
    clearDeadline();
    admittedRenderSource = undefined;
    admittedWinnerContext = undefined;
    if (terminal.outcome !== 'accepted') {
      const uncommitted = pendingArtifact;
      pendingArtifact = undefined;
      disposeArtifact(uncommitted);
    }
    if (disposeOwner) {
      settlingInternally = true;
      try {
        Reflect.apply(ownerDisposeMethod, owner, []);
      } catch {
        // The terminal latch and owned-resource cleanup remain authoritative.
      } finally {
        settlingInternally = false;
      }
    }
    if (publishDiagnostics) {
      const accepted = terminal.outcome === 'accepted';
      const servedFrom =
        accepted && terminalRenderSource?.type === 'cache'
          ? ('pbs-cache' as const)
          : accepted
            ? ('inline' as const)
            : undefined;
      let sourceIdentity: Readonly<{ adId?: string; bidId?: string; creativeId?: string }> =
        Object.freeze({});
      if (accepted && terminalRenderSource) {
        try {
          const readString = (name: string): string | undefined => {
            const descriptor = Object.getOwnPropertyDescriptor(terminalRenderSource, name);
            return descriptor && 'value' in descriptor && typeof descriptor.value === 'string'
              ? descriptor.value
              : undefined;
          };
          const bidId = terminalRenderSource.type === 'aps' ? readString('bidId') : undefined;
          const creativeId =
            terminalRenderSource.type === 'aps' ? readString('creativeId') : undefined;
          const adId = terminalRenderSource.type === 'cache' ? readString('cacheId') : undefined;
          sourceIdentity = frozen({
            ...(adId === undefined ? {} : { adId }),
            ...(bidId === undefined ? {} : { bidId }),
            ...(creativeId === undefined ? {} : { creativeId }),
          });
        } catch {
          // Optional trace identity cannot affect the committed terminal state.
        }
      }
      const observation = frozen<RenderAttemptDiagnosticsObservation>({
        kind: 'render_attempt',
        attemptId: id,
        slotId: slot,
        path: history.includes('waiting_for_gam_and_claim') ? 'ssat' : 'auction',
        rendered: accepted,
        injected: accepted,
        ...(servedFrom === undefined ? {} : { servedFrom }),
        ...sourceIdentity,
        state: terminal.outcome,
        outcome: terminal,
      });
      try {
        Reflect.apply(publishDiagnostics, undefined, [observation]);
      } catch {
        // Diagnostics publication cannot change terminal render authority.
      }
    }
    notify(terminal);
    return true;
  };

  const fail = (reason: RenderFailureReason): boolean =>
    validFailureReason(reason) && (reason !== 'gam_empty' || state === 'waiting_for_gam_and_claim')
      ? settle(frozen({ outcome: 'failed', reason }), true)
      : false;

  const armDeadline = (
    entered: RenderAttemptActiveState,
    explicitDeadline?: RenderDeadline
  ): void => {
    const deadline = explicitDeadline ?? RENDER_STATE_DEADLINES[entered];
    if (!deadline) return;
    deadlineState = entered;
    try {
      const handle = Reflect.apply(schedulerSetMethod, scheduler, [
        () => {
          if (outcome === undefined && state === entered && deadlineState === entered) {
            fail(deadline.reason);
          }
        },
        deadline.milliseconds,
      ]);
      if (outcome === undefined && state === entered && deadlineState === entered) {
        deadlineHandle = handle;
      } else {
        try {
          Reflect.apply(schedulerClearMethod, scheduler, [handle]);
        } catch {
          // A synchronously-settled deadline remains inert through the terminal latch.
        }
      }
    } catch {
      deadlineState = undefined;
      deadlineHandle = undefined;
      fail('internal_error');
    }
  };

  const enter = (
    allowed: readonly RenderAttemptActiveState[],
    next: RenderAttemptActiveState,
    explicitDeadline?: RenderDeadline
  ): boolean => {
    if (outcome !== undefined || !ownerIsCurrent() || !allowed.includes(state as never)) {
      return false;
    }
    state = next;
    arrayPush(history, next);
    clearDeadline();
    if (outcome === undefined) armDeadline(next, explicitDeadline);
    return true;
  };

  const stageAndEnter = (
    allowed: readonly RenderAttemptActiveState[],
    next: 'waiting_for_document' | 'waiting_for_adm',
    artifact: CommittedRenderArtifact
  ): boolean => {
    const sourceType = admittedRenderSource?.type;
    const sourceMatches =
      next === 'waiting_for_document'
        ? sourceType === 'aps'
        : sourceType === 'adm' || sourceType === 'cache';
    const artifactKind = state === 'rendering_direct' ? 'direct_iframe' : 'puc';
    if (
      pendingArtifact !== undefined ||
      !validArtifact(artifact, slot, navigationGeneration, id) ||
      artifact.kind !== artifactKind ||
      !sourceMatches ||
      outcome !== undefined ||
      !ownerIsCurrent() ||
      !allowed.includes(state as never)
    ) {
      return false;
    }
    pendingArtifact = artifact;
    if (enter(allowed, next)) return true;
    pendingArtifact = undefined;
    return false;
  };

  const beginCacheFetch = (): boolean => {
    if (
      cacheFetchStarted ||
      outcome !== undefined ||
      admittedRenderSource?.type !== 'cache' ||
      admittedWinnerContext === undefined ||
      !ownerIsCurrent()
    ) {
      return false;
    }
    if (state === 'created') {
      cacheFetchStarted = true;
      return enter(['created'], 'rendering_direct', CACHE_FETCH_DEADLINE);
    }
    if (state !== 'waiting_for_insertion') return false;
    cacheFetchStarted = true;
    clearDeadline();
    if (outcome === undefined && state === 'waiting_for_insertion') {
      armDeadline('waiting_for_insertion', CACHE_FETCH_DEADLINE);
    }
    return outcome === undefined && state === 'waiting_for_insertion';
  };

  const lifecycle: RenderAttempt = {
    id,
    slot,
    generation,
    navigationGeneration,
    parentAttemptId,
    get renderSource(): ReservationRenderSource | undefined {
      return admittedRenderSource;
    },
    get winnerContext(): WinnerContext | undefined {
      return admittedWinnerContext;
    },
    admitDirectWinner,
    admitClaimedWinner,
    beginGamClaim: () =>
      admittedRenderSource === undefined && admittedWinnerContext === undefined
        ? enter(['created'], 'waiting_for_gam_and_claim')
        : false,
    ownerClaimed: () =>
      admittedRenderSource && admittedWinnerContext
        ? enter(['waiting_for_gam_and_claim'], 'waiting_for_owner')
        : false,
    ownerRegistered: () => enter(['waiting_for_owner'], 'waiting_for_insertion'),
    beginDirect: () =>
      (admittedRenderSource?.type === 'aps' || admittedRenderSource?.type === 'adm') &&
      admittedWinnerContext
        ? enter(['created'], 'rendering_direct')
        : false,
    beginApsDocument: (artifact) =>
      stageAndEnter(
        ['waiting_for_insertion', 'rendering_direct'],
        'waiting_for_document',
        artifact
      ),
    beginAdm: (artifact) =>
      stageAndEnter(['waiting_for_insertion', 'rendering_direct'], 'waiting_for_adm', artifact),
    apsDocumentAccepted: () => enter(['waiting_for_document'], 'waiting_for_aps_completion'),
    accept(): boolean {
      if (
        outcome !== undefined ||
        !ownerIsCurrent() ||
        (state !== 'waiting_for_aps_completion' && state !== 'waiting_for_adm') ||
        !pendingArtifact
      ) {
        return false;
      }
      const candidate = pendingArtifact;
      clearDeadline();
      if (
        outcome !== undefined ||
        pendingArtifact !== candidate ||
        (state !== 'waiting_for_aps_completion' && state !== 'waiting_for_adm') ||
        !ownerIsCurrent()
      ) {
        return false;
      }
      let promoted: boolean;
      let promotionAttempted = false;
      try {
        promotionAttempted = true;
        promoted = Reflect.apply(promoteArtifactMethod, artifacts, [
          candidate,
          () =>
            outcome === undefined &&
            pendingArtifact === candidate &&
            (state === 'waiting_for_aps_completion' || state === 'waiting_for_adm') &&
            ownerIsCurrent(),
        ]);
        promoted =
          promoted && Reflect.apply(currentArtifactMethod, artifacts, [slot]) === candidate;
        if (
          promoted &&
          (outcome !== undefined ||
            pendingArtifact !== candidate ||
            (state !== 'waiting_for_aps_completion' && state !== 'waiting_for_adm') ||
            !ownerIsCurrent())
        ) {
          promoted = false;
        }
      } catch {
        promoted = false;
      }
      if (!promoted) {
        if (promotionAttempted) {
          try {
            Reflect.apply(releaseArtifactMethod, artifacts, [candidate]);
          } catch {
            // The branded store contains release failure; attempt cleanup remains exact-once.
          }
        }
        fail('internal_error');
        return false;
      }
      pendingArtifact = undefined;
      return settle(frozen({ outcome: 'accepted' }), true);
    },
    noBid: () =>
      state === 'created' &&
      admittedRenderSource === undefined &&
      admittedWinnerContext === undefined &&
      ownerIsCurrent()
        ? settle(frozen({ outcome: 'no_bid' }), true)
        : false,
    fail,
    cancel: (reason) =>
      validCancellationReason(reason)
        ? settle(frozen({ outcome: 'cancelled', reason }), true)
        : false,
    onSettled(callback): boolean {
      if (typeof callback !== 'function') return false;
      if (outcome) {
        try {
          callback(outcome);
        } catch {
          // Observation cannot change the terminal result.
        }
        return true;
      }
      arrayPush(observers, callback);
      return true;
    },
    snapshot: () =>
      frozen({
        history: frozen(arraySlice(history)),
        outcome,
        state,
      }),
  };

  const disposeRejectedOwner = (): void => {
    try {
      Reflect.apply(ownerDisposeMethod, owner, []);
    } catch {
      // Owner registration failure is terminal even if host disposal throws.
    }
  };

  try {
    Reflect.apply(ownerOnDisposeMethod, owner, [
      'render-lifecycle',
      () => {
        if (!settlingInternally && outcome === undefined) {
          settle(frozen({ outcome: 'cancelled', reason: 'navigation_disposed' }), false);
        }
      },
    ]);
  } catch {
    disposeRejectedOwner();
    return frozen({ ok: false, reason: 'stale_owner' });
  }
  if (outcome !== undefined || !ownerIdentityIsCurrent()) {
    settle(frozen({ outcome: 'cancelled', reason: 'navigation_disposed' }), false);
    disposeRejectedOwner();
    return frozen({ ok: false, reason: 'stale_owner' });
  }
  const exposedLifecycle = frozen(lifecycle);
  weakSetAdd(renderAttempts, exposedLifecycle);
  weakMapSet(
    cacheAttemptControls,
    exposedLifecycle,
    frozen({
      begin: beginCacheFetch,
      complete: () => {
        const cacheState = state;
        if (
          admittedRenderSource?.type !== 'cache' ||
          !admittedWinnerContext ||
          outcome !== undefined ||
          (cacheState !== 'rendering_direct' && cacheState !== 'waiting_for_insertion') ||
          deadlineState !== cacheState ||
          !ownerIsCurrent()
        ) {
          return false;
        }
        clearDeadline();
        if (cacheState === 'waiting_for_insertion' && outcome === undefined) {
          armDeadline(cacheState);
        }
        return outcome === undefined && state === cacheState && ownerIsCurrent();
      },
    })
  );
  return frozen({ ok: true, value: exposedLifecycle });
}

type DirectAdmSource = Readonly<CacheAdmSource>;

type DirectCacheSource = Readonly<{
  cacheId: string;
  fetchUrl: string;
  height: number;
  type: 'cache';
  version: 1;
  width: number;
}>;

type CacheBodyResult =
  | Readonly<{ ok: true; text: string }>
  | Readonly<{ ok: false; reason: 'cache_network_error' | 'cache_invalid_response' }>;

function exactFrozenDataRecord(
  value: unknown,
  expectedNames: readonly string[]
): Record<string, unknown> | undefined {
  try {
    if (
      typeof value !== 'object' ||
      value === null ||
      !objectIsFrozen(value) ||
      objectPrototype(value) !== Object.prototype ||
      ownPropertySymbols(value).length !== 0
    ) {
      return undefined;
    }
    const names = sortedStrings(ownPropertyNames(value));
    if (names.length !== expectedNames.length) return undefined;
    const fields = reflectApplyIntrinsic(objectCreateIntrinsic, Object, [null]) as Record<
      string,
      unknown
    >;
    for (let index = 0; index < expectedNames.length; index += 1) {
      const name = expectedNames[index];
      if (!name || names[index] !== name) return undefined;
      const descriptor = ownPropertyDescriptor(value, name);
      if (
        !descriptor ||
        !('value' in descriptor) ||
        descriptor.enumerable !== true ||
        descriptor.configurable !== false ||
        descriptor.writable !== false
      ) {
        return undefined;
      }
      fields[name] = descriptor.value;
    }
    return fields;
  } catch {
    return undefined;
  }
}

function readCachePolicyBase(value: unknown): URL | undefined {
  try {
    const fields = exactFrozenDataRecord(value, ['baseUrl', 'version']);
    const baseUrl = fields?.['baseUrl'];
    if (
      !fields ||
      fields['version'] !== 1 ||
      typeof baseUrl !== 'string' ||
      baseUrl.length === 0 ||
      utf8Length(baseUrl) > MAX_CACHE_URL_BYTES
    ) {
      return undefined;
    }
    for (let index = 0; index < baseUrl.length; index += 1) {
      const code = reflectApplyIntrinsic(stringCharCodeAtIntrinsic, baseUrl, [index]) as number;
      if (code <= 0x1f || code === 0x7f) return undefined;
      if (code >= 0xd800 && code <= 0xdbff) {
        const next = reflectApplyIntrinsic(stringCharCodeAtIntrinsic, baseUrl, [
          index + 1,
        ]) as number;
        if (next < 0xdc00 || next > 0xdfff) return undefined;
        index += 1;
      } else if (code >= 0xdc00 && code <= 0xdfff) {
        return undefined;
      }
    }
    const base = Reflect.construct(urlIntrinsic, [baseUrl]) as URL;
    if (
      urlPart(base, 'protocol') !== 'https:' ||
      urlPart(base, 'hostname') === '' ||
      urlPart(base, 'username') !== '' ||
      urlPart(base, 'password') !== '' ||
      urlPart(base, 'search') !== '' ||
      urlPart(base, 'hash') !== '' ||
      urlPart(base, 'pathname') === '/'
    ) {
      return undefined;
    }
    return base;
  } catch {
    return undefined;
  }
}

function readDirectCacheSource(
  value: unknown,
  cachePolicy: unknown
): DirectCacheSource | undefined {
  try {
    const base = readCachePolicyBase(cachePolicy);
    if (!base) return undefined;
    const fields = exactFrozenDataRecord(value, [
      'cacheId',
      'fetchUrl',
      'height',
      'type',
      'version',
      'width',
    ]);
    if (
      !fields ||
      fields['type'] !== 'cache' ||
      fields['version'] !== 1 ||
      typeof fields['cacheId'] !== 'string' ||
      !regexpTest(CACHE_ID, fields['cacheId']) ||
      typeof fields['fetchUrl'] !== 'string' ||
      fields['fetchUrl'].length === 0 ||
      utf8Length(fields['fetchUrl']) > MAX_CACHE_URL_BYTES ||
      typeof fields['width'] !== 'number' ||
      !isInteger(fields['width']) ||
      fields['width'] < 1 ||
      fields['width'] > 4096 ||
      typeof fields['height'] !== 'number' ||
      !isInteger(fields['height']) ||
      fields['height'] < 1 ||
      fields['height'] > 4096
    ) {
      return undefined;
    }
    const sourceFetchUrl = fields['fetchUrl'] as string;
    for (let index = 0; index < sourceFetchUrl.length; index += 1) {
      const code = reflectApplyIntrinsic(stringCharCodeAtIntrinsic, sourceFetchUrl, [
        index,
      ]) as number;
      if (code <= 0x1f || code === 0x7f) return undefined;
      if (code >= 0xd800 && code <= 0xdbff) {
        const next = reflectApplyIntrinsic(stringCharCodeAtIntrinsic, sourceFetchUrl, [
          index + 1,
        ]) as number;
        if (next < 0xdc00 || next > 0xdfff) return undefined;
        index += 1;
      } else if (code >= 0xdc00 && code <= 0xdfff) {
        return undefined;
      }
    }
    const fetchUrl = Reflect.construct(urlIntrinsic, [sourceFetchUrl]) as URL;
    const canonicalSearch = `?uuid=${
      reflectApplyIntrinsic(encodeURIComponentIntrinsic, undefined, [fields['cacheId']]) as string
    }`;
    const expectedHref = `${urlPart(base, 'href')}${canonicalSearch}`;
    if (
      urlPart(fetchUrl, 'protocol') !== 'https:' ||
      urlPart(fetchUrl, 'username') !== '' ||
      urlPart(fetchUrl, 'password') !== '' ||
      urlPart(fetchUrl, 'hash') !== '' ||
      urlPart(fetchUrl, 'origin') !== urlPart(base, 'origin') ||
      urlPart(fetchUrl, 'port') !== urlPart(base, 'port') ||
      urlPart(fetchUrl, 'pathname') !== urlPart(base, 'pathname') ||
      urlPart(fetchUrl, 'search') !== canonicalSearch ||
      urlPart(fetchUrl, 'href') !== fields['fetchUrl'] ||
      urlPart(fetchUrl, 'href') !== expectedHref
    ) {
      return undefined;
    }
    return value as DirectCacheSource;
  } catch {
    return undefined;
  }
}

function readSelectedCpm(value: unknown): number | undefined {
  const fields = exactFrozenDataRecord(value, ['selectedCpm']);
  const selectedCpm = fields?.['selectedCpm'];
  return typeof selectedCpm === 'number' && isFiniteNumber(selectedCpm) && selectedCpm >= 0
    ? selectedCpm
    : undefined;
}

function expandAuctionPrice(adm: string, selectedCpm: number): string {
  const token = '${AUCTION_PRICE}';
  const replacement = stringIntrinsic(selectedCpm);
  let cursor = 0;
  let output = '';
  while (true) {
    const next = reflectApplyIntrinsic(stringIndexOfIntrinsic, adm, [token, cursor]) as number;
    if (next < 0) {
      return output + (reflectApplyIntrinsic(stringSliceIntrinsic, adm, [cursor]) as string);
    }
    output +=
      (reflectApplyIntrinsic(stringSliceIntrinsic, adm, [cursor, next]) as string) + replacement;
    cursor = next + token.length;
  }
}

function parseCacheAdm(
  text: string,
  source: DirectCacheSource,
  selectedCpm: number
): DirectAdmSource | undefined {
  try {
    const value = reflectApplyIntrinsic(jsonParseIntrinsic, JSON, [text]) as unknown;
    if (
      typeof value !== 'object' ||
      value === null ||
      isArray(value) ||
      objectPrototype(value) !== Object.prototype ||
      ownPropertySymbols(value).length !== 0
    ) {
      return undefined;
    }
    const record = value as Record<string, unknown>;
    const names = ownPropertyNames(record);
    for (let index = 0; index < names.length; index += 1) {
      const name = names[index];
      if (!name) return undefined;
      const descriptor = ownPropertyDescriptor(record, name);
      if (!descriptor || !('value' in descriptor) || descriptor.enumerable !== true) {
        return undefined;
      }
    }
    const recordHasOwn = (name: string): boolean => hasOwn(record, name);
    if (recordHasOwn('width') || recordHasOwn('height')) return undefined;
    const admDescriptor = ownPropertyDescriptor(record, 'adm');
    if (
      !admDescriptor ||
      !('value' in admDescriptor) ||
      typeof admDescriptor.value !== 'string' ||
      (reflectApplyIntrinsic(stringTrimIntrinsic, admDescriptor.value, []) as string).length ===
        0 ||
      utf8Length(admDescriptor.value) > MAX_CACHE_BODY_BYTES
    ) {
      return undefined;
    }
    const hasWidth = recordHasOwn('w');
    const hasHeight = recordHasOwn('h');
    if (hasWidth !== hasHeight) return undefined;
    if (
      hasWidth &&
      (typeof record['w'] !== 'number' ||
        !isInteger(record['w']) ||
        record['w'] < 1 ||
        record['w'] > 4096 ||
        record['w'] !== source.width ||
        typeof record['h'] !== 'number' ||
        !isInteger(record['h']) ||
        record['h'] < 1 ||
        record['h'] > 4096 ||
        record['h'] !== source.height)
    ) {
      return undefined;
    }
    if (
      recordHasOwn('price') &&
      (typeof record['price'] !== 'number' ||
        !isFiniteNumber(record['price']) ||
        record['price'] < 0)
    ) {
      return undefined;
    }
    const adm = expandAuctionPrice(admDescriptor.value, selectedCpm);
    if (utf8Length(adm) > MAX_CACHE_BODY_BYTES) return undefined;
    return frozen({
      adm,
      height: source.height,
      type: 'adm',
      version: 1,
      width: source.width,
    });
  } catch {
    return undefined;
  }
}

interface CacheBodyReadHooks {
  readonly active: () => boolean;
  readonly retain: (
    reader: CacheFetchReader,
    cancel: NonNullable<CacheFetchReader['cancel']>
  ) => boolean;
  readonly cancel: () => void;
  readonly release: (reader: CacheFetchReader) => void;
}

async function readCacheBody(
  response: CacheFetchResponse,
  hooks: CacheBodyReadHooks
): Promise<CacheBodyResult> {
  let reader: CacheFetchReader | undefined;
  let releaseLock: (() => void) | undefined;
  try {
    if (!response.ok) return frozen({ ok: false, reason: 'cache_invalid_response' });
    if (!response.body) return frozen({ ok: true, text: '' });
    reader = response.body.getReader();
    const cancel = reader.cancel;
    const read = reader.read;
    releaseLock = reader.releaseLock;
    if (
      typeof read !== 'function' ||
      typeof cancel !== 'function' ||
      !hooks.active() ||
      !hooks.retain(reader, cancel)
    ) {
      return frozen({ ok: false, reason: 'cache_network_error' });
    }
    const chunks: Uint8Array[] = [];
    let total = 0;
    while (true) {
      if (!hooks.active()) return frozen({ ok: false, reason: 'cache_network_error' });
      const step = await reflectApplyIntrinsic(read, reader, []);
      if (!hooks.active()) return frozen({ ok: false, reason: 'cache_network_error' });
      if (step.done) break;
      if (!isUint8Array(step.value)) {
        return frozen({ ok: false, reason: 'cache_network_error' });
      }
      total += step.value.byteLength;
      if (total > MAX_CACHE_BODY_BYTES) {
        hooks.cancel();
        return frozen({ ok: false, reason: 'cache_invalid_response' });
      }
      arrayPush(chunks, step.value);
    }
    const bytes = new Uint8Array(total);
    let offset = 0;
    for (let index = 0; index < chunks.length; index += 1) {
      const chunk = chunks[index];
      if (!chunk) return frozen({ ok: false, reason: 'cache_network_error' });
      bytes.set(chunk, offset);
      offset += chunk.byteLength;
    }
    try {
      return frozen({
        ok: true,
        text: reflectApplyIntrinsic(textDecoderDecodeIntrinsic, fatalTextDecoder, [
          bytes,
        ]) as string,
      });
    } catch {
      return frozen({ ok: false, reason: 'cache_invalid_response' });
    }
  } catch {
    return frozen({ ok: false, reason: 'cache_network_error' });
  } finally {
    if (reader) hooks.release(reader);
    if (reader && typeof releaseLock === 'function') {
      try {
        reflectApplyIntrinsic(releaseLock, reader, []);
      } catch {
        // The bounded result remains authoritative if stream lock release is hostile.
      }
    }
  }
}

function readDirectAdmSource(value: unknown): DirectAdmSource | undefined {
  try {
    if (
      typeof value !== 'object' ||
      value === null ||
      !Object.isFrozen(value) ||
      Object.getPrototypeOf(value) !== Object.prototype ||
      Object.getOwnPropertySymbols(value).length !== 0
    ) {
      return undefined;
    }
    const names = Object.getOwnPropertyNames(value).sort();
    const expected = ['adm', 'height', 'type', 'version', 'width'];
    if (names.length !== expected.length) return undefined;
    for (let index = 0; index < expected.length; index += 1) {
      if (names[index] !== expected[index]) return undefined;
    }
    const fields = reflectApplyIntrinsic(objectCreateIntrinsic, Object, [null]) as Record<
      string,
      unknown
    >;
    for (const name of expected) {
      const descriptor = ownPropertyDescriptor(value, name);
      if (
        !descriptor ||
        !('value' in descriptor) ||
        descriptor.enumerable !== true ||
        descriptor.configurable !== false ||
        descriptor.writable !== false
      ) {
        return undefined;
      }
      fields[name] = descriptor.value;
    }
    if (
      fields['type'] !== 'adm' ||
      fields['version'] !== 1 ||
      typeof fields['adm'] !== 'string' ||
      (reflectApplyIntrinsic(stringTrimIntrinsic, fields['adm'], []) as string).length === 0 ||
      utf8Length(fields['adm']) > MAX_CACHE_BODY_BYTES ||
      typeof fields['width'] !== 'number' ||
      !isInteger(fields['width']) ||
      fields['width'] < 1 ||
      fields['width'] > 4096 ||
      typeof fields['height'] !== 'number' ||
      !isInteger(fields['height']) ||
      fields['height'] < 1 ||
      fields['height'] > 4096
    ) {
      return undefined;
    }
    return value as DirectAdmSource;
  } catch {
    return undefined;
  }
}

function renderAdmAttempt(
  options: DirectAdmAttemptOptions,
  admittedCacheAdm?: DirectAdmSource
): boolean {
  let attempt: RenderAttempt;
  let container: HTMLElement;
  let prepareIframe: DirectAdmIframeConstructor;
  let publisherOrigin: string;
  try {
    attempt = options.attempt;
    container = options.container;
    prepareIframe = options.prepareIframe;
    publisherOrigin = options.publisherOrigin;
  } catch {
    return false;
  }
  if (!weakSetHas(renderAttempts, attempt) || typeof prepareIframe !== 'function') return false;

  let exactDocumentOrigin: boolean;
  try {
    exactDocumentOrigin =
      !!directAdmDocument &&
      typeof directAdmOwnerDocumentGetter === 'function' &&
      reflectApplyIntrinsic(directAdmOwnerDocumentGetter, container, []) === directAdmDocument &&
      directAdmDocument.defaultView?.location.origin === publisherOrigin;
  } catch {
    exactDocumentOrigin = false;
  }
  if (!exactDocumentOrigin) {
    attempt.fail('winner_not_renderable');
    return false;
  }

  const source = admittedCacheAdm ?? readDirectAdmSource(attempt.renderSource);
  if (!source) {
    attempt.fail('winner_not_renderable');
    return false;
  }
  if (admittedCacheAdm === undefined && !attempt.beginDirect()) return false;
  try {
    if (attempt.snapshot().state !== 'rendering_direct') return false;
  } catch {
    attempt.fail('internal_error');
    return false;
  }

  let activeHandle: DirectAdmIframeHandle | undefined;
  let activateHandleMethod: DirectAdmIframeHandle['activate'] | undefined;
  let appendHandleMethod: DirectAdmIframeHandle['append'] | undefined;
  let commitHandleMethod: DirectAdmIframeHandle['commit'] | undefined;
  let currentHandleMethod: DirectAdmIframeHandle['current'] | undefined;
  let disposeHandleMethod: DirectAdmIframeHandle['dispose'] | undefined;
  let handleDisposed = false;
  let artifactOwnedByAttempt = false;
  const disposeHandle = (): void => {
    if (handleDisposed) return;
    handleDisposed = true;
    if (!activeHandle || typeof disposeHandleMethod !== 'function') return;
    try {
      reflectApplyIntrinsic(disposeHandleMethod, activeHandle, []);
    } catch {
      // The attempt remains terminal even if an injected cleanup boundary is hostile.
    }
  };
  const currentHandle = (): boolean => {
    if (!activeHandle || typeof currentHandleMethod !== 'function') return false;
    try {
      return reflectApplyIntrinsic(currentHandleMethod, activeHandle, []) === true;
    } catch {
      return false;
    }
  };
  const failAttempt = (reason: RenderFailureReason): void => {
    try {
      attempt.fail(reason);
    } catch {
      if (artifactOwnedByAttempt) disposeHandle();
    }
  };
  const fail = (reason: RenderFailureReason): false => {
    if (!artifactOwnedByAttempt) disposeHandle();
    failAttempt(reason);
    return false;
  };
  try {
    activeHandle = prepareIframe({
      adm: source.adm,
      container,
      height: source.height,
      onError: () => {
        if (artifactOwnedByAttempt) failAttempt('adm_document_no_load');
      },
      onLoad: () => {
        if (!artifactOwnedByAttempt || !currentHandle()) {
          if (artifactOwnedByAttempt) failAttempt('adm_document_no_load');
          return;
        }
        let accepted = false;
        try {
          accepted = attempt.accept() === true;
        } catch {
          failAttempt('internal_error');
        }
        if (!accepted || !activeHandle || typeof commitHandleMethod !== 'function') return;
        try {
          reflectApplyIntrinsic(commitHandleMethod, activeHandle, []);
        } catch {
          // Terminal acceptance is already authoritative; cleanup cannot be replayed here.
        }
      },
      width: source.width,
    });
  } catch {
    return fail('adm_document_no_load');
  }
  const handle = activeHandle;
  if (!handle) return fail('adm_document_no_load');
  try {
    disposeHandleMethod = handle.dispose;
    appendHandleMethod = handle.append;
    currentHandleMethod = handle.current;
    activateHandleMethod = handle.activate;
    commitHandleMethod = handle.commit;
    if (
      typeof disposeHandleMethod !== 'function' ||
      typeof appendHandleMethod !== 'function' ||
      typeof currentHandleMethod !== 'function' ||
      typeof activateHandleMethod !== 'function' ||
      typeof commitHandleMethod !== 'function'
    ) {
      return fail('adm_document_no_load');
    }
  } catch {
    return fail('adm_document_no_load');
  }

  const artifact = frozen<CommittedRenderArtifact>({
    kind: 'direct_iframe',
    attemptId: attempt.id,
    slot: attempt.slot,
    navigationGeneration: attempt.navigationGeneration,
    dispose: disposeHandle,
  });
  try {
    if (reflectApplyIntrinsic(appendHandleMethod, handle, []) !== true) {
      return fail('adm_document_no_load');
    }
  } catch {
    return fail('adm_document_no_load');
  }
  try {
    if (!attempt.beginAdm(artifact)) return fail('internal_error');
  } catch {
    return fail('internal_error');
  }
  artifactOwnedByAttempt = true;
  let state: RenderAttemptState;
  try {
    state = attempt.snapshot().state;
  } catch {
    return fail('internal_error');
  }
  if (state !== 'waiting_for_adm') return false;
  if (!currentHandle()) return fail('adm_document_no_load');
  try {
    if (reflectApplyIntrinsic(activateHandleMethod, handle, []) !== true) {
      return fail('adm_document_no_load');
    }
  } catch {
    return fail('adm_document_no_load');
  }
  try {
    state = attempt.snapshot().state;
  } catch {
    return fail('internal_error');
  }
  return state === 'waiting_for_adm' || state === 'accepted';
}

/** Drive one admitted direct ADM attempt through the shared iframe constructor. */
export function renderDirectAdmAttempt(options: DirectAdmAttemptOptions): boolean {
  return renderAdmAttempt(options);
}

/** Resolve one admitted cache source without assuming direct or PUC DOM ownership. */
export function resolveCacheAdmAttempt(options: CacheAdmResolutionOptions): boolean {
  let attempt: RenderAttempt;
  let cachePolicy: CacheAdmResolutionOptions['cachePolicy'];
  let fetchCache: CacheAdmResolutionOptions['fetcher'];
  let onResolved: CacheAdmResolutionOptions['onResolved'];
  try {
    attempt = options.attempt;
    cachePolicy = options.cachePolicy;
    fetchCache = options.fetcher;
    onResolved = options.onResolved;
  } catch {
    return false;
  }
  if (
    !weakSetHas(renderAttempts, attempt) ||
    typeof fetchCache !== 'function' ||
    typeof onResolved !== 'function'
  ) {
    return false;
  }
  const cacheControls = weakMapGet(cacheAttemptControls, attempt);
  if (!cacheControls) return false;

  const source = readDirectCacheSource(attempt.renderSource, cachePolicy);
  const winnerContext = attempt.winnerContext;
  const selectedCpm = readSelectedCpm(winnerContext);
  if (!source || selectedCpm === undefined) {
    attempt.fail('descriptor_invalid');
    return false;
  }
  if (reflectApplyIntrinsic(cacheControls.begin, cacheControls, []) !== true) return false;

  let controller: AbortController;
  let signal: AbortSignal;
  try {
    controller = Reflect.construct(abortControllerIntrinsic, []) as AbortController;
    signal = reflectApplyIntrinsic(abortControllerSignalGetter, controller, []) as AbortSignal;
  } catch {
    attempt.fail('cache_network_error');
    return false;
  }

  let pending = true;
  let activeReader: CacheFetchReader | undefined;
  let activeReaderCancel: NonNullable<CacheFetchReader['cancel']> | undefined;
  let activeReaderCancelled = false;
  const retainBodyReader = (
    reader: CacheFetchReader,
    cancel: NonNullable<CacheFetchReader['cancel']>
  ): boolean => {
    if (!pending || activeReader !== undefined) return false;
    activeReader = reader;
    activeReaderCancel = cancel;
    activeReaderCancelled = false;
    return true;
  };
  const releaseBodyReader = (reader: CacheFetchReader): void => {
    if (activeReader !== reader) return;
    activeReader = undefined;
    activeReaderCancel = undefined;
    activeReaderCancelled = false;
  };
  const cancelBodyReader = (): void => {
    const reader = activeReader;
    const cancel = activeReaderCancel;
    if (!reader || !cancel || activeReaderCancelled) return;
    activeReaderCancelled = true;
    try {
      const cancellation = reflectApplyIntrinsic(cancel, reader, []) as unknown;
      if (
        (typeof cancellation === 'object' || typeof cancellation === 'function') &&
        cancellation !== null
      ) {
        try {
          reflectApplyIntrinsic(promiseThenIntrinsic, cancellation, [
            ignoreAsyncDisposal,
            ignoreAsyncDisposal,
          ]);
        } catch {
          // Cancellation authority is exact-once even for a hostile promise boundary.
        }
      }
    } catch {
      // Terminal settlement remains authoritative if reader cancellation throws.
    }
  };
  const abortFetch = (): void => {
    cancelBodyReader();
    try {
      if (reflectApplyIntrinsic(abortSignalAbortedGetter, signal, []) === true) return;
      reflectApplyIntrinsic(abortControllerAbortIntrinsic, controller, []);
    } catch {
      // Abort is best-effort after the attempt has already settled.
    }
  };
  const failCache = (reason: RenderFailureReason): void => {
    if (!pending) return;
    pending = false;
    abortFetch();
    try {
      attempt.fail(reason);
    } catch {
      // A hostile attempt boundary cannot replay the already-closed cache phase.
    }
  };
  if (
    !attempt.onSettled(() => {
      pending = false;
      abortFetch();
    })
  ) {
    failCache('cache_network_error');
    return false;
  }
  if (!pending) return false;

  let responsePromise: Promise<unknown>;
  try {
    responsePromise = reflectApplyIntrinsic(fetchCache, undefined, [
      source.fetchUrl,
      {
        credentials: 'omit',
        method: 'GET',
        mode: 'cors',
        redirect: 'error',
        referrer: '',
        referrerPolicy: 'no-referrer',
        signal,
      } satisfies RequestInit,
    ]) as Promise<unknown>;
  } catch {
    failCache('cache_network_error');
    return false;
  }

  const complete = async (): Promise<void> => {
    let fetched: unknown;
    try {
      fetched = await responsePromise;
    } catch {
      failCache('cache_network_error');
      return;
    }
    if (!pending) return;
    let response: CacheFetchResponse;
    let responseOk: boolean;
    let responseType: Response['type'];
    try {
      if ((typeof fetched !== 'object' && typeof fetched !== 'function') || fetched === null) {
        throw new TypeError('invalid cache response');
      }
      response = fetched as CacheFetchResponse;
      responseOk = response.ok;
      responseType = response.type;
      if (typeof responseOk !== 'boolean' || typeof responseType !== 'string') {
        throw new TypeError('invalid cache response metadata');
      }
    } catch {
      failCache('cache_network_error');
      return;
    }
    if (responseType !== 'cors') {
      failCache('cache_network_error');
      return;
    }
    if (!responseOk) {
      failCache('cache_http_error');
      return;
    }
    const body = await readCacheBody(response, {
      active: () => pending,
      cancel: cancelBodyReader,
      release: releaseBodyReader,
      retain: retainBodyReader,
    });
    if (!pending) return;
    if (!body.ok) {
      failCache(body.reason);
      return;
    }
    if (reflectApplyIntrinsic(cacheControls.complete, cacheControls, []) !== true) {
      failCache('cache_network_error');
      return;
    }
    const admSource = parseCacheAdm(body.text, source, selectedCpm);
    if (!admSource) {
      failCache('cache_invalid_response');
      return;
    }
    if (attempt.renderSource !== source || attempt.winnerContext !== winnerContext) {
      failCache('cache_invalid_response');
      return;
    }
    pending = false;
    let resolved: boolean;
    try {
      resolved = reflectApplyIntrinsic(onResolved, undefined, [admSource]) === true;
    } catch {
      resolved = false;
    }
    if (!resolved) {
      try {
        if (attempt.snapshot().outcome === undefined) attempt.fail('internal_error');
      } catch {
        // The terminal latch remains authoritative across a hostile consumer boundary.
      }
    }
  };
  const completion = complete();
  try {
    reflectApplyIntrinsic(promiseThenIntrinsic, completion, [
      ignoreAsyncDisposal,
      ignoreAsyncDisposal,
    ]);
  } catch {
    failCache('cache_network_error');
    return false;
  }
  return true;
}

/** Fetch and render one admitted direct cache source through the shared ADM constructor. */
export function renderDirectCacheAttempt(options: DirectCacheAttemptOptions): boolean {
  let attempt: RenderAttempt;
  let cachePolicy: DirectCacheAttemptOptions['cachePolicy'];
  let container: HTMLElement;
  let fetcher: DirectCacheAttemptOptions['fetcher'];
  let prepareIframe: DirectAdmIframeConstructor;
  let publisherOrigin: string;
  try {
    attempt = options.attempt;
    cachePolicy = options.cachePolicy;
    container = options.container;
    fetcher = options.fetcher;
    prepareIframe = options.prepareIframe;
    publisherOrigin = options.publisherOrigin;
  } catch {
    return false;
  }
  if (!weakSetHas(renderAttempts, attempt) || typeof prepareIframe !== 'function') return false;

  let created = false;
  let exactDirectContainer: boolean;
  try {
    created = attempt.snapshot().state === 'created';
    exactDirectContainer =
      created &&
      !!directAdmDocument &&
      typeof directAdmOwnerDocumentGetter === 'function' &&
      reflectApplyIntrinsic(directAdmOwnerDocumentGetter, container, []) === directAdmDocument &&
      directAdmDocument.defaultView?.location.origin === publisherOrigin;
  } catch {
    exactDirectContainer = false;
  }
  if (!created) return false;
  if (!exactDirectContainer) {
    try {
      attempt.fail('winner_not_renderable');
    } catch {
      // The exact direct-container boundary fails closed.
    }
    return false;
  }

  return resolveCacheAdmAttempt({
    attempt,
    cachePolicy,
    fetcher,
    onResolved: (source) =>
      renderAdmAttempt({ attempt, container, prepareIframe, publisherOrigin }, source),
  });
}

interface RendererNonceBinding {
  readonly nonce: string;
  readonly attempt: RenderAttempt;
  readonly attemptId: string;
  readonly generation: object;
  source: object | undefined;
  readonly port: RendererNoncePort;
  readonly closeMethod: RendererNoncePort['close'];
  consumed: boolean;
  closed: boolean;
}

function validRendererNonce(value: unknown): value is string {
  return typeof value === 'string' && RENDERER_NONCE.test(value);
}

function readMintedRendererNonce(value: unknown): string | undefined {
  try {
    if (typeof value !== 'object' || value === null || !Object.isFrozen(value)) return undefined;
    if (Object.getPrototypeOf(value) !== Object.prototype) return undefined;
    if (Object.getOwnPropertySymbols(value).length !== 0) return undefined;
    const names = Object.getOwnPropertyNames(value).sort();
    const ok = Object.getOwnPropertyDescriptor(value, 'ok');
    if (!ok || !ok.enumerable || !('value' in ok)) return undefined;
    if (ok.value === true) {
      if (names.length !== 2 || names[0] !== 'ok' || names[1] !== 'value') return undefined;
      const nonce = Object.getOwnPropertyDescriptor(value, 'value');
      return nonce && nonce.enumerable && 'value' in nonce && validRendererNonce(nonce.value)
        ? nonce.value
        : undefined;
    }
    if (ok.value === false && names.length === 2 && names[0] === 'ok' && names[1] === 'reason') {
      const reason = Object.getOwnPropertyDescriptor(value, 'reason');
      if (
        reason &&
        reason.enumerable &&
        'value' in reason &&
        reason.value === 'identity_generation_failed'
      ) {
        return undefined;
      }
    }
    return undefined;
  } catch {
    return undefined;
  }
}

function readRendererNonceExpectation(value: unknown): RendererNonceExpectation | undefined {
  try {
    if (
      typeof value !== 'object' ||
      value === null ||
      Object.getPrototypeOf(value) !== Object.prototype
    ) {
      return undefined;
    }
    if (Object.getOwnPropertySymbols(value).length !== 0) return undefined;
    const names = Object.getOwnPropertyNames(value).sort();
    const expected = ['attempt', 'generation', 'nonce', 'port', 'source'];
    if (names.length !== expected.length) return undefined;
    const fields: Record<string, unknown> = Object.create(null) as Record<string, unknown>;
    for (let index = 0; index < expected.length; index += 1) {
      const name = expected[index];
      if (!name || names[index] !== name) return undefined;
      const descriptor = Object.getOwnPropertyDescriptor(value, name);
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return undefined;
      fields[name] = descriptor.value;
    }
    return fields as unknown as RendererNonceExpectation;
  } catch {
    return undefined;
  }
}

/** Own the bounded, one-use capabilities for APS renderer-document acceptance. */
export function createRendererNonceRegistry(
  options: RendererNonceRegistryOptions = {}
): RendererNonceRegistry {
  const liveByNonce = new Map<string, RendererNonceBinding>();
  const bindingByAttempt = new WeakMap<RenderAttempt, RendererNonceBinding>();
  const bindingByGeneration = new WeakMap<object, RendererNonceBinding>();
  const bindingByPort = new WeakMap<RendererNoncePort, RendererNonceBinding>();
  const bindings = new Set<RendererNonceBinding>();
  const pendingNonces = new Set<string>();
  const pendingAttempts = new WeakSet<RenderAttempt>();
  const pendingGenerations = new WeakSet<object>();
  const pendingPorts = new WeakSet<RendererNoncePort>();
  const retiredPorts = new WeakSet<RendererNoncePort>();
  let pendingCount = 0;
  let disposed = false;
  let mintNonce: () => IdentityGenerationResult<string>;
  try {
    mintNonce = options.mintNonce ?? mintBrowserRendererNonce;
  } catch {
    mintNonce = () => frozen({ ok: false, reason: 'identity_generation_failed' });
  }

  const closeBinding = (binding: RendererNonceBinding): void => {
    if (binding.closed) return;
    binding.closed = true;
    binding.consumed = true;
    if (mapGet(liveByNonce, binding.nonce) === binding) {
      mapDelete(liveByNonce, binding.nonce);
    }
    if (weakMapGet(bindingByAttempt, binding.attempt) === binding) {
      weakMapDelete(bindingByAttempt, binding.attempt);
    }
    if (weakMapGet(bindingByGeneration, binding.generation) === binding) {
      weakMapDelete(bindingByGeneration, binding.generation);
    }
    weakSetAdd(retiredPorts, binding.port);
    if (weakMapGet(bindingByPort, binding.port) === binding) {
      weakMapDelete(bindingByPort, binding.port);
    }
    setDelete(bindings, binding);
    try {
      Reflect.apply(binding.closeMethod, binding.port, []);
    } catch {
      // Closing a retained browser port is best-effort and exact-once.
    }
  };

  const rollbackProvisionalBinding = (binding: RendererNonceBinding): void => {
    binding.closed = true;
    binding.consumed = true;
    if (mapGet(liveByNonce, binding.nonce) === binding) {
      mapDelete(liveByNonce, binding.nonce);
    }
    if (weakMapGet(bindingByAttempt, binding.attempt) === binding) {
      weakMapDelete(bindingByAttempt, binding.attempt);
    }
    if (weakMapGet(bindingByGeneration, binding.generation) === binding) {
      weakMapDelete(bindingByGeneration, binding.generation);
    }
    if (weakMapGet(bindingByPort, binding.port) === binding) {
      weakMapDelete(bindingByPort, binding.port);
    }
    setDelete(bindings, binding);
  };

  const invalidIssue = (
    reason: Exclude<RendererNonceIssueResult, { ok: true }>['reason']
  ): RendererNonceIssueResult => frozen({ ok: false, reason });

  const registry: RendererNonceRegistry = {
    issue(input): RendererNonceIssueResult {
      let attempt: RenderAttempt;
      let source: object | undefined;
      let port: RendererNoncePort;
      let closeMethod: RendererNoncePort['close'];
      let attemptId: string;
      let generation: object;
      let onSettledMethod: RenderAttempt['onSettled'];
      let snapshotMethod: RenderAttempt['snapshot'];
      try {
        attempt = input.attempt;
        source = input.source;
        port = input.port;
        closeMethod = port.close;
        attemptId = attempt.id;
        generation = attempt.generation;
        onSettledMethod = attempt.onSettled;
        snapshotMethod = attempt.snapshot;
        if (
          disposed ||
          !weakSetHas(renderAttempts, attempt) ||
          (source !== undefined &&
            ((typeof source !== 'object' && typeof source !== 'function') || source === null)) ||
          (typeof port !== 'object' && typeof port !== 'function') ||
          port === null ||
          typeof closeMethod !== 'function' ||
          !validAttemptId(attemptId) ||
          (typeof generation !== 'object' && typeof generation !== 'function') ||
          generation === null ||
          typeof onSettledMethod !== 'function' ||
          typeof snapshotMethod !== 'function' ||
          Reflect.apply(snapshotMethod, attempt, []).outcome !== undefined ||
          weakMapHas(bindingByAttempt, attempt) ||
          weakSetHas(pendingAttempts, attempt) ||
          weakMapHas(bindingByGeneration, generation) ||
          weakSetHas(pendingGenerations, generation) ||
          weakMapHas(bindingByPort, port) ||
          weakSetHas(pendingPorts, port) ||
          weakSetHas(retiredPorts, port)
        ) {
          return invalidIssue('invalid_attempt');
        }
      } catch {
        return invalidIssue('invalid_attempt');
      }
      if (setSize(bindings) + pendingCount >= MAX_RENDERER_NONCES) {
        return invalidIssue('capability_registry_full');
      }
      try {
        weakSetAdd(pendingAttempts, attempt);
        weakSetAdd(pendingGenerations, generation);
        weakSetAdd(pendingPorts, port);
        pendingCount += 1;
      } catch {
        weakSetDelete(pendingAttempts, attempt);
        weakSetDelete(pendingGenerations, generation);
        weakSetDelete(pendingPorts, port);
        return invalidIssue('invalid_attempt');
      }

      const attemptStillIssuable = (): boolean => {
        try {
          return (
            !disposed &&
            !weakMapHas(bindingByAttempt, attempt) &&
            !weakMapHas(bindingByGeneration, generation) &&
            !weakMapHas(bindingByPort, port) &&
            !weakSetHas(retiredPorts, port) &&
            attempt.id === attemptId &&
            attempt.generation === generation &&
            Reflect.apply(snapshotMethod, attempt, []).outcome === undefined
          );
        } catch {
          return false;
        }
      };

      let nonce: string | undefined;
      try {
        for (let draw = 0; draw < MAX_RENDERER_NONCE_DRAWS; draw += 1) {
          let minted: unknown;
          try {
            minted = Reflect.apply(mintNonce, undefined, []);
          } catch {
            return invalidIssue('identity_generation_failed');
          }
          const candidate = readMintedRendererNonce(minted);
          if (!attemptStillIssuable()) return invalidIssue('invalid_attempt');
          if (setSize(bindings) + pendingCount > MAX_RENDERER_NONCES) {
            return invalidIssue('capability_registry_full');
          }
          if (!candidate) return invalidIssue('identity_generation_failed');
          if (!mapGet(liveByNonce, candidate) && !setHas(pendingNonces, candidate)) {
            nonce = candidate;
            setAdd(pendingNonces, candidate);
            break;
          }
        }
        if (!nonce) return invalidIssue('identity_generation_failed');
        if (!attemptStillIssuable()) return invalidIssue('invalid_attempt');

        const binding: RendererNonceBinding = {
          nonce,
          attempt,
          attemptId,
          generation,
          source,
          port,
          closeMethod,
          consumed: false,
          closed: false,
        };
        let committed = false;
        try {
          const registered = Reflect.apply(onSettledMethod, attempt, [
            () => {
              if (committed) closeBinding(binding);
            },
          ]);
          if (
            registered !== true ||
            !attemptStillIssuable() ||
            setSize(bindings) + pendingCount > MAX_RENDERER_NONCES ||
            mapGet(liveByNonce, nonce) !== undefined ||
            !setHas(pendingNonces, nonce)
          ) {
            return invalidIssue('invalid_attempt');
          }
          mapSet(liveByNonce, nonce, binding);
          weakMapSet(bindingByAttempt, attempt, binding);
          weakMapSet(bindingByGeneration, generation, binding);
          weakMapSet(bindingByPort, port, binding);
          setAdd(bindings, binding);
          if (
            mapGet(liveByNonce, nonce) !== binding ||
            weakMapGet(bindingByAttempt, attempt) !== binding ||
            weakMapGet(bindingByGeneration, generation) !== binding ||
            weakMapGet(bindingByPort, port) !== binding ||
            !setHas(bindings, binding)
          ) {
            rollbackProvisionalBinding(binding);
            return invalidIssue('invalid_attempt');
          }
          committed = true;
        } catch {
          rollbackProvisionalBinding(binding);
          return invalidIssue('invalid_attempt');
        }
        return frozen({ ok: true, nonce });
      } finally {
        if (nonce) setDelete(pendingNonces, nonce);
        weakSetDelete(pendingAttempts, attempt);
        weakSetDelete(pendingGenerations, generation);
        weakSetDelete(pendingPorts, port);
        pendingCount -= 1;
      }
    },
    bindSource(expectation): boolean {
      try {
        const fields = readRendererNonceExpectation(expectation);
        if (
          disposed ||
          !fields ||
          !validRendererNonce(fields.nonce) ||
          (typeof fields.source !== 'object' && typeof fields.source !== 'function') ||
          fields.source === null
        ) {
          return false;
        }
        const binding = mapGet(liveByNonce, fields.nonce);
        if (
          !binding ||
          binding.closed ||
          binding.consumed ||
          binding.source !== undefined ||
          !setHas(bindings, binding) ||
          fields.attempt !== binding.attempt ||
          fields.generation !== binding.generation ||
          fields.port !== binding.port ||
          weakMapGet(bindingByPort, binding.port) !== binding ||
          binding.attempt.id !== binding.attemptId ||
          binding.attempt.generation !== binding.generation ||
          Reflect.apply(binding.attempt.snapshot, binding.attempt, []).outcome !== undefined ||
          mapGet(liveByNonce, binding.nonce) !== binding
        ) {
          return false;
        }
        binding.source = fields.source;
        return true;
      } catch {
        return false;
      }
    },
    consume(expectation): boolean {
      try {
        const fields = readRendererNonceExpectation(expectation);
        if (disposed || !fields || !validRendererNonce(fields.nonce)) return false;
        const binding = mapGet(liveByNonce, fields.nonce);
        if (
          !binding ||
          binding.closed ||
          binding.consumed ||
          binding.source === undefined ||
          !setHas(bindings, binding) ||
          fields.attempt !== binding.attempt ||
          fields.generation !== binding.generation ||
          fields.source !== binding.source ||
          fields.port !== binding.port ||
          weakMapGet(bindingByPort, binding.port) !== binding ||
          binding.attempt.id !== binding.attemptId ||
          binding.attempt.generation !== binding.generation ||
          Reflect.apply(binding.attempt.snapshot, binding.attempt, []).outcome !== undefined
        ) {
          return false;
        }
        if (
          disposed ||
          binding.closed ||
          binding.consumed ||
          mapGet(liveByNonce, binding.nonce) !== binding ||
          !mapDelete(liveByNonce, binding.nonce)
        ) {
          return false;
        }
        binding.consumed = true;
        return true;
      } catch {
        return false;
      }
    },
    dispose(): void {
      if (disposed) return;
      disposed = true;
      const snapshot = setValueSnapshot(bindings);
      mapClear(liveByNonce);
      for (let index = 0; index < snapshot.length; index += 1) {
        const binding = snapshot[index];
        if (binding) closeBinding(binding);
      }
    },
    snapshotForTest: () =>
      frozen({
        bindings: setSize(bindings),
        disposed,
        liveNonces: mapSize(liveByNonce),
      }),
  };
  return frozen(registry);
}

/** Own one public per-slot result without overwriting either child attempt result. */
export function createSlotOperation(options: SlotOperationOptions): SlotOperationCreationResult {
  const observers: Array<(result: SlotOperationResult) => void> = [];
  let primary: RenderAttempt;
  let primaryId: string;
  let primarySlot: string;
  let primaryNavigationGeneration: object;
  let primaryOnSettledMethod: RenderAttempt['onSettled'];
  let primarySnapshotMethod: RenderAttempt['snapshot'];
  let createFallback: SlotOperationOptions['createFallback'];
  try {
    const primaryDescriptor = Object.getOwnPropertyDescriptor(options, 'primary');
    const fallbackDescriptor = Object.getOwnPropertyDescriptor(options, 'createFallback');
    if (!primaryDescriptor || !('value' in primaryDescriptor)) {
      return frozen({ ok: false, reason: 'invalid_attempt' });
    }
    primary = primaryDescriptor.value as RenderAttempt;
    if (!weakSetHas(renderAttempts, primary)) {
      return frozen({ ok: false, reason: 'invalid_attempt' });
    }
    primaryId = primary.id;
    primarySlot = primary.slot;
    primaryNavigationGeneration = primary.navigationGeneration;
    primaryOnSettledMethod = primary.onSettled;
    primarySnapshotMethod = primary.snapshot;
    createFallback =
      fallbackDescriptor && 'value' in fallbackDescriptor
        ? (fallbackDescriptor.value as SlotOperationOptions['createFallback'])
        : undefined;
    if (createFallback !== undefined && typeof createFallback !== 'function') {
      return frozen({ ok: false, reason: 'invalid_attempt' });
    }
  } catch {
    return frozen({ ok: false, reason: 'invalid_attempt' });
  }
  let result: SlotOperationResult | undefined;

  const settle = (terminal: SlotOperationResult): boolean => {
    if (result) return false;
    result = frozen(terminal);
    const snapshot = arraySpliceAll(observers);
    for (let index = 0; index < snapshot.length; index += 1) {
      const observer = snapshot[index];
      if (!observer) continue;
      try {
        observer(result);
      } catch {
        // Observation cannot change the public result.
      }
    }
    return true;
  };

  const settleFallbackFailure = (primary: RenderOutcome, reason: RenderFailureReason): void => {
    settle({
      path: 'fallback',
      outcome: frozen({ outcome: 'failed', reason }),
      primaryAttemptId: primaryId,
      primary,
    });
  };

  const beginFallback = (primary: RenderOutcome): void => {
    let created: unknown;
    try {
      created =
        (createFallback ? Reflect.apply(createFallback, options, [primaryId]) : undefined) ??
        frozen({ ok: false, reason: 'stale_owner' });
    } catch {
      settleFallbackFailure(primary, 'internal_error');
      return;
    }
    let createdOk: boolean;
    let createdValue: unknown;
    let creationReason: unknown;
    try {
      if (
        (typeof created !== 'object' && typeof created !== 'function') ||
        created === null ||
        !Object.isFrozen(created) ||
        Object.getPrototypeOf(created) !== Object.prototype ||
        Object.getOwnPropertySymbols(created).length !== 0
      ) {
        throw new TypeError('invalid fallback result');
      }
      const ok = Object.getOwnPropertyDescriptor(created, 'ok');
      if (!ok || !('value' in ok) || ok.enumerable !== true || typeof ok.value !== 'boolean') {
        throw new TypeError('invalid fallback result');
      }
      createdOk = ok.value;
      const field = Object.getOwnPropertyDescriptor(created, createdOk ? 'value' : 'reason');
      if (!field || !('value' in field) || field.enumerable !== true) {
        throw new TypeError('invalid fallback result');
      }
      const names = Object.getOwnPropertyNames(created).sort();
      if (
        names.length !== 2 ||
        names[0] !== 'ok' ||
        names[1] !== (createdOk ? 'value' : 'reason')
      ) {
        throw new TypeError('invalid fallback result');
      }
      if (createdOk) createdValue = field.value;
      else creationReason = field.value;
    } catch {
      settleFallbackFailure(primary, 'internal_error');
      return;
    }
    if (!createdOk) {
      settleFallbackFailure(
        primary,
        creationReason === 'identity_generation_failed'
          ? 'identity_generation_failed'
          : 'internal_error'
      );
      return;
    }
    let child: RenderAttempt | undefined;
    let childId: string;
    let childOnSettledMethod: RenderAttempt['onSettled'];
    let childCancelMethod: RenderAttempt['cancel'];
    let cancelInvoked = false;
    const cancelChild = (): void => {
      if (cancelInvoked || !child || typeof childCancelMethod !== 'function') return;
      cancelInvoked = true;
      try {
        Reflect.apply(childCancelMethod, child, ['superseded']);
      } catch {
        // Fallback cleanup cannot change the operation result.
      }
    };
    try {
      if (
        (typeof createdValue !== 'object' && typeof createdValue !== 'function') ||
        createdValue === null
      ) {
        throw new TypeError('invalid fallback child');
      }
      child = createdValue as RenderAttempt;
      const cancel = Object.getOwnPropertyDescriptor(child, 'cancel');
      if (cancel && 'value' in cancel && typeof cancel.value === 'function') {
        childCancelMethod = cancel.value as RenderAttempt['cancel'];
      }
      if (!weakSetHas(renderAttempts, child)) throw new TypeError('invalid fallback child');
      childId = child.id;
      childOnSettledMethod = child.onSettled;
      childCancelMethod = child.cancel;
      if (
        !validAttemptId(childId) ||
        childId === primaryId ||
        child.slot !== primarySlot ||
        child.parentAttemptId !== primaryId ||
        child.navigationGeneration !== primaryNavigationGeneration ||
        typeof childOnSettledMethod !== 'function' ||
        typeof childCancelMethod !== 'function'
      ) {
        throw new TypeError('invalid fallback child');
      }
    } catch {
      cancelChild();
      settleFallbackFailure(primary, 'internal_error');
      return;
    }
    try {
      const subscribed = Reflect.apply(childOnSettledMethod, child, [
        (fallbackOutcome: RenderOutcome) => {
          if (!validOutcome(fallbackOutcome)) {
            cancelChild();
            settleFallbackFailure(primary, 'internal_error');
            return;
          }
          settle({
            path: 'fallback',
            outcome: fallbackOutcome,
            primaryAttemptId: primaryId,
            primary,
            fallbackAttemptId: childId,
            fallback: fallbackOutcome,
          });
        },
      ]);
      if (subscribed !== true && !result) {
        cancelChild();
        settleFallbackFailure(primary, 'internal_error');
      }
    } catch {
      if (!result) {
        cancelChild();
        settleFallbackFailure(primary, 'internal_error');
      }
    }
  };

  try {
    const subscribed = Reflect.apply(primaryOnSettledMethod, primary, [
      (primaryOutcome: RenderOutcome) => {
        if (!validOutcome(primaryOutcome)) {
          const internal = frozen({
            outcome: 'failed' as const,
            reason: 'internal_error' as const,
          });
          settle({
            path: 'primary',
            outcome: internal,
            primaryAttemptId: primaryId,
            primary: internal,
          });
          return;
        }
        let attributableGamEmpty = false;
        if (
          primaryOutcome.outcome === 'failed' &&
          primaryOutcome.reason === 'gam_empty' &&
          typeof createFallback === 'function'
        ) {
          try {
            const snapshot = Reflect.apply(primarySnapshotMethod, primary, []);
            attributableGamEmpty =
              snapshot.outcome === primaryOutcome &&
              snapshot.state === 'failed' &&
              snapshot.history[snapshot.history.length - 2] === 'waiting_for_gam_and_claim';
          } catch {
            attributableGamEmpty = false;
          }
        }
        if (attributableGamEmpty) {
          beginFallback(primaryOutcome);
          return;
        }
        settle({
          path: 'primary',
          outcome: primaryOutcome,
          primaryAttemptId: primaryId,
          primary: primaryOutcome,
        });
      },
    ]);
    if (subscribed !== true && !result) {
      const internal = frozen({ outcome: 'failed' as const, reason: 'internal_error' as const });
      settle({
        path: 'primary',
        outcome: internal,
        primaryAttemptId: primaryId,
        primary: internal,
      });
    }
  } catch {
    const internal = frozen({ outcome: 'failed' as const, reason: 'internal_error' as const });
    settle({
      path: 'primary',
      outcome: internal,
      primaryAttemptId: primaryId,
      primary: internal,
    });
  }

  const operation = frozen({
    snapshot: (): SlotOperationSnapshot =>
      result ? frozen({ settled: true, result }) : frozen({ settled: false }),
    onSettled(callback: (terminal: SlotOperationResult) => void): boolean {
      if (typeof callback !== 'function') return false;
      if (result) {
        try {
          callback(result);
        } catch {
          // Observation cannot change the public result.
        }
      } else {
        arrayPush(observers, callback);
      }
      return true;
    },
  });
  return frozen({ ok: true, value: operation });
}
