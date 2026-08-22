import { createHash } from 'node:crypto';

import { describe, expect, it } from 'vitest';

import {
  canonicalIntegrationConfigDigestV1,
  integrationConfigValueV1,
  snapshotIntegrationConfigsV1,
} from '../../src/core/contracts/integration_configs';
import { snapshotBootstrapInputV1 } from '../../src/core/contracts/boot';
import { snapshotServerBootTransportV1 } from '../../src/core/contracts/server_boot_transport';

const RELEASE = 'a'.repeat(64);

function bootInput(): Record<string, unknown> {
  return {
    target: {},
    boot: {
      abi: 1,
      releaseId: RELEASE,
      manifest: {
        version: 1,
        releaseId: RELEASE,
        firstDisplay: null,
        runtimeSrc: `/static/tsjs=tsjs-unified.min.js?v=${'b'.repeat(64)}`,
        integrations: [{ id: 'render_runtime', phase: 'takeover' }],
      },
      auctionProjection: {
        version: 1,
        auction: { version: 1, auctionId: 'initial', results: [] },
        slots: [],
        bids: [],
      },
      integrations: { version: 1, entries: [] },
      creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
      diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
    },
    outline: null,
  };
}

function carrier(config: unknown = { nested: ['value', 1, true, null] }): Record<string, unknown> {
  return {
    version: 1,
    entries: [
      { id: 'aps', config: {} },
      { id: 'prebid', config },
    ],
  };
}

function firstDisplayInput(): Record<string, unknown> {
  const input = bootInput();
  const boot = input.boot as Record<string, unknown>;
  const manifest = boot.manifest as Record<string, unknown>;
  manifest.firstDisplay = {
    src: `/static/tsjs=tsjs-first-display.min.js?m=0001&v=${'c'.repeat(64)}`,
    slices: ['first_display'],
  };
  boot.auctionProjection = {
    version: 1,
    auction: {
      version: 1,
      auctionId: 'initial',
      results: [{ slot: 'ad-1', outcome: 'no_bid' }],
    },
    slots: [
      {
        slot: 'ad-1',
        gamUnitPath: '/123/ad-1',
        divId: 'ad-1',
        formats: [[300, 250]],
        targeting: {},
      },
    ],
    bids: [],
  };
  const canonicalCarrier = JSON.stringify(boot.integrations);
  input.outline = {
    version: 1,
    releaseId: RELEASE,
    generation: 1,
    projectionDigest: 'd'.repeat(64),
    integrationConfigDigest: createHash('sha256').update(canonicalCarrier).digest('hex'),
    slices: ['first_display'],
    slotCount: 1,
    outcomeCount: 1,
    capabilities: [],
    objectKinds: [],
  };
  return input;
}

function transportInput(firstDisplay = false): Record<string, unknown> {
  const input = firstDisplay ? firstDisplayInput() : bootInput();
  const boot = input.boot as Record<string, unknown>;
  const integrationConfigDigest = createHash('sha256')
    .update(JSON.stringify(boot.integrations))
    .digest('hex');
  const projectionDigest = createHash('sha256')
    .update(JSON.stringify(boot.auctionProjection))
    .digest('hex');
  const outline = input.outline as Record<string, unknown> | null;
  if (outline) {
    outline.projectionDigest = projectionDigest;
    outline.integrationConfigDigest = integrationConfigDigest;
  }
  return {
    version: 1,
    boot,
    integrity: { version: 1, projectionDigest, integrationConfigDigest },
    outline,
  };
}

function selectFirstDisplay(
  input: Record<string, unknown>,
  mask: string,
  slices: readonly string[]
): void {
  const boot = input.boot as Record<string, unknown>;
  const manifest = boot.manifest as Record<string, unknown>;
  manifest.firstDisplay = {
    src: `/static/tsjs=tsjs-first-display.min.js?m=${mask}&v=${'c'.repeat(64)}`,
    slices: [...slices],
  };
  const outline = input.outline as Record<string, unknown>;
  outline.slices = [...slices];
}

describe('sealed production boot transport', () => {
  it.each([false, true])('parses and recursively freezes a fresh %s transport', (firstDisplay) => {
    const original = transportInput(firstDisplay);
    const accepted = snapshotServerBootTransportV1(JSON.stringify(original), RELEASE);

    expect(accepted).toBeDefined();
    expect(accepted).not.toBe(original);
    expect(Object.isFrozen(accepted)).toBe(true);
    expect(Object.isFrozen(accepted?.boot)).toBe(true);
    expect(Object.isFrozen(accepted?.boot.auctionProjection)).toBe(true);
    expect(Object.isFrozen(accepted?.integrity)).toBe(true);
    expect(accepted?.outline === null || Object.isFrozen(accepted?.outline)).toBe(true);
  });

  it('accepts strings only and requires the exact root and always-present integrity', () => {
    const valid = transportInput();
    expect(snapshotServerBootTransportV1(valid, RELEASE)).toBeUndefined();
    expect(
      snapshotServerBootTransportV1(JSON.stringify({ ...valid, extra: true }), RELEASE)
    ).toBeUndefined();
    const { integrity: _integrity, ...missingIntegrity } = valid;
    expect(
      snapshotServerBootTransportV1(JSON.stringify(missingIntegrity), RELEASE)
    ).toBeUndefined();
  });

  it('binds a non-null outline to the always-present integrity value', () => {
    const value = transportInput(true);
    (value.outline as Record<string, unknown>).projectionDigest = 'f'.repeat(64);

    expect(snapshotServerBootTransportV1(JSON.stringify(value), RELEASE)).toBeUndefined();
  });

  it('rejects a decoded transport above 10 MiB before parsing it', () => {
    const encoded = `${JSON.stringify(transportInput())}${' '.repeat(10 * 1024 * 1024)}`;
    expect(snapshotServerBootTransportV1(encoded, RELEASE)).toBeUndefined();
  });

  it('rejects more than fourteen takeover manifest entries', () => {
    const value = transportInput();
    const boot = value.boot as Record<string, unknown>;
    const manifest = boot.manifest as Record<string, unknown>;
    const integrations = Array.from({ length: 14 }, (_, index) => ({
      id: `takeover_${index}`,
      phase: 'takeover',
    }));
    manifest.integrations = integrations;

    expect(snapshotServerBootTransportV1(JSON.stringify(value), RELEASE)).toBeDefined();

    integrations.push({ id: 'takeover_14', phase: 'takeover' });

    expect(snapshotServerBootTransportV1(JSON.stringify(value), RELEASE)).toBeUndefined();
  });

  it('binds each deferred manifest source to its declared id', () => {
    const value = transportInput();
    const boot = value.boot as Record<string, unknown>;
    const manifest = boot.manifest as Record<string, unknown>;
    manifest.integrations = [
      { id: 'render_runtime', phase: 'takeover' },
      {
        id: 'gpt_later',
        phase: 'deferred',
        trigger: 'first_display_or_idle',
        src: `/static/tsjs=tsjs-gpt_later.min.js?v=${'e'.repeat(64)}`,
      },
    ];

    expect(snapshotServerBootTransportV1(JSON.stringify(value), RELEASE)).toBeDefined();

    const deferred = (manifest.integrations as Array<Record<string, unknown>>)[1]!;
    deferred.src = `/static/tsjs=tsjs-evil.min.js?v=${'e'.repeat(64)}`;
    expect(snapshotServerBootTransportV1(JSON.stringify(value), RELEASE)).toBeUndefined();
  });

  it.each([
    ['APS without the render owner', '0085', ['first_display', 'aps_initial', 'gpt_initial']],
    ['the render owner without GPT', '0003', ['first_display', 'render_owner_initial']],
  ])('rejects %s before exposing the compact boot', (_name, mask, slices) => {
    const value = transportInput(true);
    selectFirstDisplay(value, mask, slices);

    expect(snapshotServerBootTransportV1(JSON.stringify(value), RELEASE)).toBeUndefined();
  });

  it.each([
    ['0081', ['first_display', 'gpt_initial']],
    ['0083', ['first_display', 'render_owner_initial', 'gpt_initial']],
  ])('admits the closed non-APS slice relationship %s', (mask, slices) => {
    const value = transportInput(true);
    selectFirstDisplay(value, mask, slices);

    expect(snapshotServerBootTransportV1(JSON.stringify(value), RELEASE)).toBeDefined();
  });
});

describe('bootstrap integration configuration admission', () => {
  it('captures one complete immutable boot copy without retaining the server literal', () => {
    const original = bootInput();
    const originalBoot = original.boot as Record<string, unknown>;
    const accepted = snapshotBootstrapInputV1(original, RELEASE);

    expect(accepted).toBeDefined();
    expect(accepted?.boot).not.toBe(originalBoot);
    expect(Object.isFrozen(accepted?.boot)).toBe(true);
    expect(Object.isFrozen(accepted?.boot.manifest)).toBe(true);
    expect(Object.isFrozen(accepted?.boot.auctionProjection)).toBe(true);
    originalBoot.releaseId = 'c'.repeat(64);
    original.boot = {};
    expect(accepted?.boot.releaseId).toBe(RELEASE);
  });

  it('rejects manifest/config membership mismatch before capture', () => {
    const input = bootInput();
    const boot = input.boot as Record<string, unknown>;
    const manifest = boot.manifest as Record<string, unknown>;
    manifest.integrations = [
      { id: 'render_runtime', phase: 'takeover' },
      { id: 'aps', phase: 'takeover' },
    ];

    expect(snapshotBootstrapInputV1(input, RELEASE)).toBeUndefined();
  });

  it('rejects accessor-backed bootstrap fields without invoking the accessor', () => {
    let invoked = false;
    const input = bootInput();
    Object.defineProperty(input, 'boot', {
      enumerable: true,
      get: () => {
        invoked = true;
        return {};
      },
    });

    expect(snapshotBootstrapInputV1(input, RELEASE)).toBeUndefined();
    expect(invoked).toBe(false);
  });

  it('calculates the canonical carrier digest and binds a first-display outline to it', () => {
    const input = firstDisplayInput();
    const acceptedCarrier = snapshotIntegrationConfigsV1(
      (input.boot as Record<string, unknown>).integrations
    );
    const expected = createHash('sha256').update(JSON.stringify(acceptedCarrier)).digest('hex');

    expect(canonicalIntegrationConfigDigestV1(acceptedCarrier!)).toBe(expected);
    expect(snapshotBootstrapInputV1(input, RELEASE)).toBeDefined();

    const outline = input.outline as Record<string, unknown>;
    outline.integrationConfigDigest = 'e'.repeat(64);
    expect(snapshotBootstrapInputV1(input, RELEASE)).toBeUndefined();
  });

  it('rejects a self-consistent outline whose counts do not match the retained projection', () => {
    const input = firstDisplayInput();
    const outline = input.outline as Record<string, unknown>;
    outline.slotCount = 2;
    outline.outcomeCount = 2;

    expect(snapshotBootstrapInputV1(input, RELEASE)).toBeUndefined();
  });

  it.each([
    ['APS without the render owner', '0085', ['first_display', 'aps_initial', 'gpt_initial']],
    ['the render owner without GPT', '0003', ['first_display', 'render_owner_initial']],
  ])('rejects %s before retaining the full boot', (_name, mask, slices) => {
    const input = firstDisplayInput();
    selectFirstDisplay(input, mask, slices);

    expect(snapshotBootstrapInputV1(input, RELEASE)).toBeUndefined();
  });

  it('preserves poison-named config data properties and rejects them outside config values', () => {
    const config = Object.defineProperty({}, '__proto__', {
      configurable: true,
      enumerable: true,
      value: { retained: 'data' },
      writable: true,
    });
    const accepted = snapshotIntegrationConfigsV1(carrier(config));
    const copied = integrationConfigValueV1(accepted!, 'prebid')!;

    expect(Object.getPrototypeOf(copied)).toBe(Object.prototype);
    expect(Object.prototype.hasOwnProperty.call(copied, '__proto__')).toBe(true);
    expect(Object.getOwnPropertyDescriptor(copied, '__proto__')?.value).toEqual({
      retained: 'data',
    });
    expect(canonicalIntegrationConfigDigestV1(accepted!)).toBe(
      createHash('sha256').update(JSON.stringify(accepted)).digest('hex')
    );

    const input = bootInput();
    Object.defineProperty(input, '__proto__', {
      configurable: true,
      enumerable: true,
      value: {},
      writable: true,
    });
    expect(snapshotBootstrapInputV1(input, RELEASE)).toBeUndefined();
  });

  it('copies, recursively freezes, orders, and attenuates the server carrier', () => {
    const original = carrier() as {
      entries: Array<{ id: 'aps' | 'prebid'; config: Record<string, unknown> }>;
    };
    const accepted = snapshotIntegrationConfigsV1(original);

    expect(accepted).toBeDefined();
    expect(accepted).not.toBe(original);
    expect(Object.isFrozen(accepted)).toBe(true);
    expect(Object.isFrozen(accepted?.entries)).toBe(true);
    const prebid = integrationConfigValueV1(accepted!, 'prebid');
    expect(prebid).toEqual({ nested: ['value', 1, true, null] });
    expect(prebid).not.toBe(original.entries[1]?.config);
    expect(Object.isFrozen(prebid)).toBe(true);
    expect(Object.isFrozen(prebid?.nested)).toBe(true);

    original.entries[1]!.config.nested = ['mutated'];
    original.entries.length = 0;
    expect(integrationConfigValueV1(accepted!, 'prebid')).toEqual({
      nested: ['value', 1, true, null],
    });
    expect(integrationConfigValueV1(accepted!, 'aps')).toEqual({});
    expect(integrationConfigValueV1(accepted!, 'gpt')).toBeUndefined();
  });

  it.each([
    ['unknown root key', { ...carrier(), extra: true }],
    ['custom root prototype', Object.assign(Object.create(null), carrier())],
    ['symbol root key', Object.assign(carrier() as object, { [Symbol('x')]: true })],
    [
      'root accessor',
      Object.defineProperty({ version: 1 }, 'entries', { enumerable: true, get: () => [] }),
    ],
    ['unknown id', { version: 1, entries: [{ id: 'unknown', config: {} }] }],
    [
      'out-of-order ids',
      {
        version: 1,
        entries: [
          { id: 'prebid', config: {} },
          { id: 'aps', config: {} },
        ],
      },
    ],
    [
      'duplicate ids',
      {
        version: 1,
        entries: [
          { id: 'aps', config: {} },
          { id: 'aps', config: {} },
        ],
      },
    ],
    ['non-object config', { version: 1, entries: [{ id: 'aps', config: null }] }],
    ['non-finite number', carrier({ value: Number.NaN })],
    ['long key', carrier({ ['k'.repeat(4_097)]: true })],
    ['long utf8 string', carrier({ value: 'é'.repeat(2_049) })],
  ])('rejects %s', (_name, candidate) => {
    expect(snapshotIntegrationConfigsV1(candidate)).toBeUndefined();
  });

  it('rejects sparse arrays, cycles, repeated aliases, excessive depth, and throwing traps', () => {
    const sparse: unknown[] = [];
    sparse.length = 1;
    expect(snapshotIntegrationConfigsV1(carrier({ sparse }))).toBeUndefined();

    const cycle: Record<string, unknown> = {};
    cycle.self = cycle;
    expect(snapshotIntegrationConfigsV1(carrier(cycle))).toBeUndefined();

    const alias = {};
    expect(snapshotIntegrationConfigsV1(carrier({ one: alias, two: alias }))).toBeUndefined();

    let deep: Record<string, unknown> = {};
    const root = deep;
    for (let index = 0; index < 17; index += 1) {
      deep.next = {};
      deep = deep.next as Record<string, unknown>;
    }
    expect(snapshotIntegrationConfigsV1(carrier(root))).toBeUndefined();

    const throwing = new Proxy(carrier() as object, {
      ownKeys: () => {
        throw new Error('publisher trap');
      },
    });
    expect(snapshotIntegrationConfigsV1(throwing)).toBeUndefined();
  });
});
