import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import envelope from '../../fixtures/aps-renderer-v1.json';
import type { ApsRendererV1 } from '../../../src/core/types';
import { log } from '../../../src/core/log';
import {
  APS_NATIVE_RENDERER_TIMEOUT_MS,
  APS_PREBID_CREATIVE_RUNNER_URL,
  APS_RENDERER_PATH,
  APS_RENDERER_SANDBOX,
  APS_RENDERING_MODE_ATTRIBUTE_NAME,
  APS_UNIVERSAL_CREATIVE_RENDERER,
  APS_UNIVERSAL_CREATIVE_RENDERER_VERSION,
  apsRendererUrl,
  dispatchApsRendering as dispatchDefaultApsRendering,
  getApsPrebidRenderer,
  parseApsRendererDescriptor,
  registerApsPrebidRenderer,
  renderApsCreative,
  validateApsRenderer,
} from '../../../src/integrations/aps/render';

function nativeRunnerState(frame: HTMLIFrameElement): {
  runner: HTMLScriptElement;
  event: CustomEvent<{ aaxResponse: string; seatBidId: string }>;
} {
  const runner = frame.contentDocument?.querySelector<HTMLScriptElement>('script');
  const frameWindow = frame.contentWindow as unknown as {
    _aps: Map<string, { queue: Array<CustomEvent<{ aaxResponse: string; seatBidId: string }>> }>;
  };
  const account = frameWindow._aps.get('example-account-id');
  expect(runner).not.toBeNull();
  expect(account?.queue).toHaveLength(1);
  return { runner: runner!, event: account!.queue[0] };
}

function encodeBytes(bytes: Uint8Array): string {
  let binary = '';
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

function encodeEnvelope(value: unknown): string {
  return encodeBytes(new TextEncoder().encode(JSON.stringify(value)));
}

function encodeEnvelopeAtSize(size: number): string {
  const serialized = JSON.stringify(envelope);
  const padding = size - new TextEncoder().encode(serialized).length;
  if (padding < 0) throw new Error('requested envelope size is too small');
  return encodeBytes(new TextEncoder().encode(`${serialized}${' '.repeat(padding)}`));
}

function descriptor(overrides: Partial<ApsRendererV1> = {}): ApsRendererV1 {
  const bid = envelope.seatbid[0].bid[0];
  return {
    type: 'aps',
    version: 1,
    accountId: 'example-account-id',
    bidId: bid.id,
    creativeId: 'fictional-creative-id',
    tagType: bid.ext.tagtype as 'iframe',
    creativeUrl: bid.ext.creativeurl,
    aaxResponse: encodeEnvelope(envelope),
    width: bid.w,
    height: bid.h,
    ...overrides,
  };
}

describe('APS renderer validation', () => {
  it('consumes the shared fictional golden envelope and supports an omitted creative ID', () => {
    const withCreativeId = descriptor();
    const withoutCreativeId = descriptor();
    delete withoutCreativeId.creativeId;

    expect(validateApsRenderer(withCreativeId)).toEqual(withCreativeId);
    expect(validateApsRenderer(withoutCreativeId)).toEqual(withoutCreativeId);
  });

  it('caches an immutable validated descriptor for the same publisher origin', () => {
    const input = descriptor();
    const atobSpy = vi.spyOn(window, 'atob');

    const first = validateApsRenderer(input, 'https://publisher.example');
    input.width = 728;
    const second = validateApsRenderer(first, 'https://publisher.example');

    expect(first?.width).toBe(300);
    expect(first).toBe(second);
    expect(Object.isFrozen(first)).toBe(true);
    expect(atobSpy).toHaveBeenCalledTimes(1);
    expect(() => {
      first!.width = 728;
    }).toThrow(TypeError);

    expect(validateApsRenderer(first, 'https://other-publisher.example')).toBeDefined();
    expect(atobSpy).toHaveBeenCalledTimes(2);
    atobSpy.mockRestore();
  });

  it('keeps auction parsing structural and leaves complete trust validation to render time', () => {
    const renderer = descriptor({ aaxResponse: 'not-base64' });

    expect(parseApsRendererDescriptor(renderer)).toEqual(renderer);
    expect(validateApsRenderer(renderer)).toBeUndefined();
  });

  it.each([
    ['unknown root field', { ...envelope, id: 'forbidden' }],
    ['sibling seat', { seatbid: [...envelope.seatbid, envelope.seatbid[0]] }],
    [
      'sibling bid',
      { seatbid: [{ bid: [...envelope.seatbid[0].bid, envelope.seatbid[0].bid[0]] }] },
    ],
    [
      'markup',
      {
        seatbid: [
          { bid: [{ ...envelope.seatbid[0].bid[0], adm: '<script>forbidden()</script>' }] },
        ],
      },
    ],
    [
      'notification',
      { seatbid: [{ bid: [{ ...envelope.seatbid[0].bid[0], nurl: 'https://notify.example' }] }] },
    ],
    [
      'unknown extension',
      {
        seatbid: [
          {
            bid: [
              {
                ...envelope.seatbid[0].bid[0],
                ext: { ...envelope.seatbid[0].bid[0].ext, userSyncs: [] },
              },
            ],
          },
        ],
      },
    ],
  ])('rejects an envelope containing %s', (_name, invalidEnvelope) => {
    expect(
      validateApsRenderer(descriptor({ aaxResponse: encodeEnvelope(invalidEnvelope) }))
    ).toBeUndefined();
  });

  it.each([
    ['bid ID', { bidId: 'another-bid' }],
    ['width', { width: 728 }],
    ['height', { height: 90 }],
    ['creative URL', { creativeUrl: 'https://other.example/render' }],
    ['tag type', { tagType: 'script' as const }],
  ])('rejects a descriptor/envelope %s mismatch', (_name, override) => {
    expect(validateApsRenderer(descriptor(override))).toBeUndefined();
  });

  it.each([
    'not-base64',
    'e30',
    '====',
    'Zh==',
    btoa(String.fromCharCode(0xc3, 0x28)),
    btoa('{not json}'),
  ])('rejects invalid base64, UTF-8, JSON, or non-canonical padding', (aaxResponse) => {
    expect(validateApsRenderer(descriptor({ aaxResponse }))).toBeUndefined();
  });

  it('rejects non-canonical trailing bits that decode to the valid envelope', () => {
    const canonical = encodeBytes(new TextEncoder().encode(`${JSON.stringify(envelope)} `));
    const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
    const finalDataIndex = canonical.length - 3;
    const canonicalIndex = alphabet.indexOf(canonical[finalDataIndex]);
    const nonCanonical = `${canonical.slice(0, finalDataIndex)}${alphabet[canonicalIndex + 1]}==`;

    expect(atob(nonCanonical)).toBe(atob(canonical));
    expect(validateApsRenderer(descriptor({ aaxResponse: canonical }))).toBeDefined();
    expect(validateApsRenderer(descriptor({ aaxResponse: nonCanonical }))).toBeUndefined();
  });

  it.each([
    'http://creative.example/render',
    'https://user:password@creative.example/render',
    `${window.location.origin}/creative`,
  ])('rejects an unsafe creative URL', (creativeUrl) => {
    const invalidEnvelope = structuredClone(envelope);
    invalidEnvelope.seatbid[0].bid[0].ext.creativeurl = creativeUrl;
    expect(
      validateApsRenderer(descriptor({ creativeUrl, aaxResponse: encodeEnvelope(invalidEnvelope) }))
    ).toBeUndefined();
  });

  it('rejects unknown descriptor fields and version mismatches', () => {
    expect(
      parseApsRendererDescriptor({ ...descriptor(), adm: '<div>forbidden</div>' })
    ).toBeUndefined();
    expect(parseApsRendererDescriptor({ ...descriptor(), version: 2 })).toBeUndefined();
  });

  it('enforces account and creative ID UTF-8 byte limits', () => {
    expect(validateApsRenderer(descriptor({ accountId: 'é'.repeat(512) }))).toBeDefined();
    expect(validateApsRenderer(descriptor({ accountId: `${'é'.repeat(512)}x` }))).toBeUndefined();
    expect(validateApsRenderer(descriptor({ creativeId: 'é'.repeat(512) }))).toBeDefined();
    expect(validateApsRenderer(descriptor({ creativeId: `${'é'.repeat(512)}x` }))).toBeUndefined();
  });

  it('enforces the creative URL UTF-8 byte limit', () => {
    const prefix = 'https://creative.example/';
    const atLimit = `${prefix}${'a'.repeat(4096 - prefix.length)}`;
    const overLimit = `${atLimit}x`;
    const atLimitEnvelope = structuredClone(envelope);
    atLimitEnvelope.seatbid[0].bid[0].ext.creativeurl = atLimit;
    const overLimitEnvelope = structuredClone(envelope);
    overLimitEnvelope.seatbid[0].bid[0].ext.creativeurl = overLimit;

    expect(
      validateApsRenderer(
        descriptor({ creativeUrl: atLimit, aaxResponse: encodeEnvelope(atLimitEnvelope) })
      )
    ).toBeDefined();
    expect(
      validateApsRenderer(
        descriptor({ creativeUrl: overLimit, aaxResponse: encodeEnvelope(overLimitEnvelope) })
      )
    ).toBeUndefined();
  });

  it('accepts the maximum decoded envelope and rejects one byte over', () => {
    const atLimit = encodeEnvelopeAtSize(256 * 1024);
    const overLimit = encodeEnvelopeAtSize(256 * 1024 + 1);

    expect(atLimit).toHaveLength(349528);
    expect(overLimit).toHaveLength(349528);
    expect(validateApsRenderer(descriptor({ aaxResponse: atLimit }))).toBeDefined();
    expect(validateApsRenderer(descriptor({ aaxResponse: overLimit }))).toBeUndefined();
    expect(
      parseApsRendererDescriptor(descriptor({ aaxResponse: `${atLimit}AAAA` }))
    ).toBeUndefined();
  });
});

describe('Prebid APS renderer registry', () => {
  afterEach(() => {
    delete window.tsjs;
  });

  it('bounds entries and evicts the oldest capability', () => {
    for (let index = 0; index <= 256; index += 1) {
      expect(
        registerApsPrebidRenderer(`prebid-${index}`, 'fictional-slot', descriptor(), 300, {
          markUsed: vi.fn(),
        })
      ).toBe(true);
    }

    expect(Object.keys(window.tsjs?.apsPrebidRenderers ?? {})).toHaveLength(256);
    expect(getApsPrebidRenderer('prebid-0')).toBeUndefined();
    expect(getApsPrebidRenderer('prebid-256')).toEqual(
      expect.objectContaining({ adUnitCode: 'fictional-slot', renderer: descriptor() })
    );
  });

  it('rejects unsafe Prebid IDs and invalid descriptors', () => {
    const lifecycle = { markUsed: vi.fn() };
    expect(
      registerApsPrebidRenderer('__proto__', 'fictional-slot', descriptor(), 300, lifecycle)
    ).toBe(false);
    expect(
      registerApsPrebidRenderer(
        'safe-prebid-id',
        'fictional-slot',
        descriptor({ aaxResponse: 'invalid' }),
        300,
        lifecycle
      )
    ).toBe(false);
    expect(window.tsjs?.apsPrebidRenderers).toBeUndefined();
  });
});

describe('APS rendering-mode authorization', () => {
  it('ignores mode markers and duplicate script tags injected after module initialization', () => {
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    document.head.insertAdjacentHTML(
      'beforeend',
      '<meta name="trusted-server-aps-rendering-mode" content="publisher_native">' +
        '<script data-ts-aps-rendering-mode="publisher_native"></script>'
    );
    const trustedServer = vi.fn(() => true);

    expect(
      dispatchDefaultApsRendering({
        slotId: 'fictional-slot',
        renderer: descriptor(),
        trustedServer,
      })
    ).toBe(true);
    expect(trustedServer).toHaveBeenCalledOnce();
    expect(document.querySelector('#fictional-slot iframe')).toBeNull();

    document.head
      .querySelectorAll(
        'meta[name="trusted-server-aps-rendering-mode"], script[data-ts-aps-rendering-mode]'
      )
      .forEach((element) => element.remove());
    document.body.innerHTML = '';
  });
});

describe('publisher-native APS runner contract tests', () => {
  let dispatchApsRendering: typeof dispatchDefaultApsRendering;

  beforeEach(async () => {
    vi.resetModules();
    document.body.innerHTML = '<div id="fictional-slot"><span>existing</span></div>';
    const publisherScript = document.createElement('script');
    publisherScript.setAttribute(APS_RENDERING_MODE_ATTRIBUTE_NAME, 'publisher_native');
    const currentScriptSpy = vi
      .spyOn(document, 'currentScript', 'get')
      .mockReturnValue(publisherScript);
    ({ dispatchApsRendering } = await import('../../../src/integrations/aps/render'));
    currentScriptSpy.mockRestore();
  });

  afterEach(() => {
    delete window.tsjs;
    vi.restoreAllMocks();
    document.body.innerHTML = '';
  });

  it('queues the exact selected response for the fixed APS runner and commits on load', async () => {
    const trustedServer = vi.fn(() => true);
    const accepted = dispatchApsRendering({
      slotId: 'fictional-slot',
      renderer: descriptor(),
      trustedServer,
    });
    const slot = document.getElementById('fictional-slot')!;
    const frame = slot.querySelector('iframe')!;
    const { runner, event } = nativeRunnerState(frame);

    expect(frame.getAttribute('sandbox')).toBeNull();
    expect(frame.style.display).toBe('none');
    expect(runner.src).toBe(APS_PREBID_CREATIVE_RUNNER_URL);
    expect(event.type).toBe('prebid/creative/render');
    expect(event.detail).toEqual({
      aaxResponse: descriptor().aaxResponse,
      seatBidId: descriptor().bidId,
    });
    expect(slot.querySelector('span')).not.toBeNull();
    expect(trustedServer).not.toHaveBeenCalled();

    const runnerDocument = frame.contentDocument!;
    expect(runnerDocument.querySelector('meta[name="referrer"]')?.getAttribute('content')).toBe(
      'no-referrer'
    );
    expect(runnerDocument.documentElement.style.margin).toBe('0px');
    expect(runnerDocument.documentElement.style.padding).toBe('0px');
    expect(runnerDocument.body.style.margin).toBe('0px');
    expect(runnerDocument.body.style.padding).toBe('0px');
    const creativeFrame = runnerDocument.createElement('iframe');
    runnerDocument.body.appendChild(creativeFrame);
    await vi.waitFor(() => expect(creativeFrame.style.display).toBe('block'));

    runner.dispatchEvent(new Event('load'));
    await expect(accepted).resolves.toBe(true);
    expect(slot.querySelector('span')).toBeNull();
    expect(frame.style.display).toBe('');
  });

  it('fails closed when the runner fails without clearing publisher content', async () => {
    const trustedServer = vi.fn(() => true);
    const accepted = dispatchApsRendering({
      slotId: 'fictional-slot',
      renderer: descriptor(),
      trustedServer,
    });
    const frame = document.querySelector<HTMLIFrameElement>('#fictional-slot iframe')!;
    const { runner } = nativeRunnerState(frame);

    runner.dispatchEvent(new Event('error'));

    await expect(accepted).resolves.toBe(false);
    expect(trustedServer).not.toHaveBeenCalled();
    expect(document.querySelector('#fictional-slot iframe')).toBeNull();
    expect(document.querySelector('#fictional-slot span')).not.toBeNull();
  });

  it('cancels a pending runner when a newer dispatch replaces it', async () => {
    const first = dispatchApsRendering({
      slotId: 'fictional-slot',
      renderer: descriptor(),
      trustedServer: () => true,
    });
    const firstFrame = document.querySelector<HTMLIFrameElement>('#fictional-slot iframe')!;

    const second = dispatchApsRendering({
      slotId: 'fictional-slot',
      renderer: descriptor(),
      trustedServer: () => true,
    });
    const secondFrame = document.querySelector<HTMLIFrameElement>('#fictional-slot iframe')!;

    expect(firstFrame.isConnected).toBe(false);
    expect(secondFrame).not.toBe(firstFrame);
    await expect(first).resolves.toBe(false);
    nativeRunnerState(secondFrame).runner.dispatchEvent(new Event('load'));
    await expect(second).resolves.toBe(true);
  });

  it('lets an invalid replacement cancel an older pending runner', async () => {
    const first = dispatchApsRendering({
      slotId: 'fictional-slot',
      renderer: descriptor(),
      trustedServer: () => true,
    });
    const second = dispatchApsRendering({
      slotId: 'fictional-slot',
      renderer: descriptor({ aaxResponse: 'invalid' }),
      trustedServer: () => true,
    });

    expect(second).toBe(false);
    await expect(first).resolves.toBe(false);
    expect(document.querySelector('#fictional-slot iframe')).toBeNull();
    expect(document.querySelector('#fictional-slot span')).not.toBeNull();
  });

  it('resolves a logical GPT slot through the injected div mapping', async () => {
    document.body.innerHTML = '<div id="div-header"><span>existing</span></div>';
    window.tsjs = { divToSlotId: { 'div-header': 'homepage_header' } } as typeof window.tsjs;

    const accepted = dispatchApsRendering({
      slotId: 'homepage_header',
      renderer: descriptor(),
      trustedServer: () => true,
    });
    const frame = document.querySelector<HTMLIFrameElement>('#div-header iframe')!;
    nativeRunnerState(frame).runner.dispatchEvent(new Event('load'));

    await expect(accepted).resolves.toBe(true);
    expect(document.querySelector('#div-header span')).toBeNull();
  });

  it('renders inside the inner slot when Prebid uses its container ID', async () => {
    document.body.innerHTML =
      '<div id="div-header-container"><div id="div-header"><iframe></iframe></div></div>';
    const source = document.querySelector<HTMLIFrameElement>('#div-header > iframe')!.contentWindow;

    const accepted = dispatchApsRendering({
      slotId: 'div-header-container',
      renderer: descriptor(),
      source,
      trustedServer: () => true,
    });
    const frame = Array.from(
      document.querySelectorAll<HTMLIFrameElement>('#div-header > iframe')
    ).find((candidate) => candidate.title === 'Ad content')!;
    nativeRunnerState(frame).runner.dispatchEvent(new Event('load'));

    await expect(accepted).resolves.toBe(true);
    expect(document.getElementById('div-header-container')).not.toBeNull();
    expect(document.getElementById('div-header')).not.toBeNull();
    expect(document.querySelectorAll('#div-header > iframe')).toHaveLength(1);
  });

  it('uses the requesting frame to resolve a dynamic slot prefix', async () => {
    document.body.innerHTML =
      '<div id="div-header-first"><iframe></iframe></div>' +
      '<div id="div-header-second"><iframe></iframe></div>';
    const source = document.querySelector<HTMLIFrameElement>(
      '#div-header-second > iframe'
    )!.contentWindow;

    const accepted = dispatchApsRendering({
      slotId: 'div-header-',
      renderer: descriptor(),
      source,
      trustedServer: () => true,
    });
    const frame = Array.from(
      document.querySelectorAll<HTMLIFrameElement>('#div-header-second > iframe')
    ).find((candidate) => candidate.title === 'Ad content')!;
    nativeRunnerState(frame).runner.dispatchEvent(new Event('load'));

    await expect(accepted).resolves.toBe(true);
    expect(document.querySelector('#div-header-first > iframe')).not.toBeNull();
    expect(document.querySelectorAll('#div-header-second > iframe')).toHaveLength(1);
  });

  it('contains throwing publisher slot mappings without falling back', async () => {
    const tsjs = {} as NonNullable<typeof window.tsjs>;
    Object.defineProperty(tsjs, 'divToSlotId', {
      get: () => {
        throw new Error('fictional mapping lookup failure');
      },
    });
    window.tsjs = tsjs;
    const trustedServer = vi.fn(() => true);

    await expect(
      dispatchApsRendering({
        slotId: 'logical-slot',
        renderer: descriptor(),
        trustedServer,
      })
    ).resolves.toBe(false);
    expect(trustedServer).not.toHaveBeenCalled();
    expect(document.querySelector('iframe')).toBeNull();
  });

  it('times out an unacknowledged runner without clearing publisher content', async () => {
    vi.useFakeTimers();
    try {
      const result = dispatchApsRendering({
        slotId: 'fictional-slot',
        renderer: descriptor(),
        trustedServer: () => true,
      });
      await vi.advanceTimersByTimeAsync(APS_NATIVE_RENDERER_TIMEOUT_MS);

      await expect(result).resolves.toBe(false);
      expect(vi.getTimerCount()).toBe(0);
      expect(document.querySelector('#fictional-slot iframe')).toBeNull();
      expect(document.querySelector('#fictional-slot span')).not.toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });
});

describe('direct APS rendering', () => {
  beforeEach(() => {
    document.body.innerHTML = '<div id="fictional-slot"><span>existing</span></div>';
  });

  afterEach(() => {
    vi.restoreAllMocks();
    document.body.innerHTML = '';
  });

  it('keeps a valid default frame when an invalid replacement is rejected', () => {
    const trustedServer = (renderer: ApsRendererV1): boolean =>
      renderApsCreative({ slotId: 'fictional-slot', renderer });
    expect(
      dispatchDefaultApsRendering({
        slotId: 'fictional-slot',
        renderer: descriptor(),
        trustedServer,
      })
    ).toBe(true);

    const slot = document.getElementById('fictional-slot')!;
    const iframe = slot.querySelector('iframe')!;
    const postMessage = vi.spyOn(iframe.contentWindow!, 'postMessage');
    iframe.dispatchEvent(new Event('load'));
    const sent = postMessage.mock.calls[0][0] as { nonce: string };

    expect(
      dispatchDefaultApsRendering({
        slotId: 'fictional-slot',
        renderer: descriptor({ aaxResponse: 'invalid' }),
        trustedServer,
      })
    ).toBe(false);
    expect(iframe.isConnected).toBe(true);

    window.dispatchEvent(
      new MessageEvent('message', {
        data: { message: 'trusted-server/aps/renderer-ready', nonce: sent.nonce },
        source: iframe.contentWindow,
      })
    );
    expect(slot.querySelector('span')).toBeNull();
    expect(iframe.style.display).toBe('');
  });

  it('loads the static route with a fragment-bound 128-bit nonce and opaque sandbox', () => {
    expect(renderApsCreative({ slotId: 'fictional-slot', renderer: descriptor() })).toBe(true);

    const slot = document.getElementById('fictional-slot')!;
    const iframe = slot.querySelector('iframe')!;
    const existing = slot.querySelector('span');
    expect(existing).not.toBeNull();
    expect(iframe.src).toMatch(/\/integrations\/aps\/renderer#tsaps=[A-Za-z0-9_-]{22}$/);
    expect(iframe.getAttribute('sandbox')).toBe(APS_RENDERER_SANDBOX);
    expect(iframe.getAttribute('sandbox')).not.toContain('allow-same-origin');
    expect(iframe.srcdoc).toBe('');

    const postMessage = vi.spyOn(iframe.contentWindow!, 'postMessage');
    iframe.dispatchEvent(new Event('load'));

    expect(slot.querySelector('span')).not.toBeNull();
    expect(iframe.style.display).toBe('none');
    expect(postMessage).toHaveBeenCalledTimes(1);
    expect(postMessage).toHaveBeenCalledWith(
      {
        nonce: expect.stringMatching(/^[A-Za-z0-9_-]{22}$/),
        renderer: descriptor(),
      },
      '*'
    );

    const message = postMessage.mock.calls[0][0] as { nonce: string };
    window.dispatchEvent(
      new MessageEvent('message', {
        data: {
          message: 'trusted-server/aps/renderer-ready',
          nonce: `wrong-${message.nonce}`,
        },
        source: iframe.contentWindow,
      })
    );
    expect(slot.querySelector('span')).not.toBeNull();

    window.dispatchEvent(
      new MessageEvent('message', {
        data: { message: 'trusted-server/aps/renderer-ready', nonce: message.nonce },
        source: iframe.contentWindow,
      })
    );
    expect(slot.querySelector('span')).toBeNull();
    expect(iframe.style.display).toBe('');
  });

  it('rejects a ready message with the correct nonce from a foreign window', () => {
    expect(renderApsCreative({ slotId: 'fictional-slot', renderer: descriptor() })).toBe(true);

    const slot = document.getElementById('fictional-slot')!;
    const rendererFrame = slot.querySelector('iframe')!;
    const postMessage = vi.spyOn(rendererFrame.contentWindow!, 'postMessage');
    rendererFrame.dispatchEvent(new Event('load'));
    const sent = postMessage.mock.calls[0][0] as { nonce: string };
    const foreignFrame = document.createElement('iframe');
    document.body.appendChild(foreignFrame);

    window.dispatchEvent(
      new MessageEvent('message', {
        data: { message: 'trusted-server/aps/renderer-ready', nonce: sent.nonce },
        source: foreignFrame.contentWindow,
      })
    );

    expect(slot.querySelector('span')).not.toBeNull();
    expect(rendererFrame.style.display).toBe('none');

    window.dispatchEvent(
      new MessageEvent('message', {
        data: { message: 'trusted-server/aps/renderer-ready', nonce: sent.nonce },
        source: rendererFrame.contentWindow,
      })
    );

    expect(slot.querySelector('span')).toBeNull();
    expect(rendererFrame.style.display).toBe('');
  });

  it('leaves existing slot content intact when validation or loading fails', () => {
    expect(
      renderApsCreative({
        slotId: 'fictional-slot',
        renderer: descriptor({ aaxResponse: 'invalid' }),
      })
    ).toBe(false);
    expect(document.querySelector('#fictional-slot span')).not.toBeNull();
    expect(document.querySelector('#fictional-slot iframe')).toBeNull();

    expect(renderApsCreative({ slotId: 'fictional-slot', renderer: descriptor() })).toBe(true);
    const iframe = document.querySelector('#fictional-slot iframe')!;
    iframe.dispatchEvent(new Event('error'));
    expect(document.querySelector('#fictional-slot span')).not.toBeNull();
    expect(document.querySelector('#fictional-slot iframe')).toBeNull();
  });

  it('removes an unacknowledged frame without clearing publisher content', () => {
    vi.useFakeTimers();
    try {
      expect(renderApsCreative({ slotId: 'fictional-slot', renderer: descriptor() })).toBe(true);
      const iframe = document.querySelector('#fictional-slot iframe')!;
      iframe.dispatchEvent(new Event('load'));

      vi.advanceTimersByTime(10_000);

      expect(document.querySelector('#fictional-slot span')).not.toBeNull();
      expect(document.querySelector('#fictional-slot iframe')).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it('immediately cancels a superseded pending frame and its timeout', () => {
    vi.useFakeTimers();
    const warnSpy = vi.spyOn(log, 'warn').mockImplementation(() => {});
    try {
      const baselineTimers = vi.getTimerCount();
      expect(renderApsCreative({ slotId: 'fictional-slot', renderer: descriptor() })).toBe(true);
      const firstFrame = document.querySelector('#fictional-slot iframe')!;
      const firstPostMessage = vi.spyOn(firstFrame.contentWindow!, 'postMessage');
      firstFrame.dispatchEvent(new Event('load'));
      const firstSent = firstPostMessage.mock.calls[0][0] as { nonce: string };
      const timersAfterFirst = vi.getTimerCount();
      expect(timersAfterFirst).toBeGreaterThan(baselineTimers);

      expect(renderApsCreative({ slotId: 'fictional-slot', renderer: descriptor() })).toBe(true);
      const secondFrame = document.querySelector('#fictional-slot iframe')!;
      expect(firstFrame.isConnected).toBe(false);
      expect(vi.getTimerCount()).toBe(timersAfterFirst);
      const postMessage = vi.spyOn(secondFrame.contentWindow!, 'postMessage');
      secondFrame.dispatchEvent(new Event('load'));
      const sent = postMessage.mock.calls[0][0] as { nonce: string };

      window.dispatchEvent(
        new MessageEvent('message', {
          data: { message: 'trusted-server/aps/renderer-ready', nonce: firstSent.nonce },
          source: firstFrame.contentWindow,
        })
      );
      expect(document.querySelector('#fictional-slot span')).not.toBeNull();
      expect((secondFrame as HTMLIFrameElement).style.display).toBe('none');

      window.dispatchEvent(
        new MessageEvent('message', {
          data: { message: 'trusted-server/aps/renderer-ready', nonce: sent.nonce },
          source: secondFrame.contentWindow,
        })
      );

      vi.advanceTimersByTime(10_000);
      expect(warnSpy).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });
});

describe('Universal Creative APS source', () => {
  it('uses the deployed dynamic renderer protocol and only creates the opaque route frame', () => {
    expect(APS_UNIVERSAL_CREATIVE_RENDERER_VERSION).toBeGreaterThanOrEqual(4);
    expect(APS_UNIVERSAL_CREATIVE_RENDERER).toContain('window.render=function');
    expect(APS_UNIVERSAL_CREATIVE_RENDERER).toContain('d&&d.apsRenderer');
    expect(APS_UNIVERSAL_CREATIVE_RENDERER).toContain('d&&d.rendererUrl');
    expect(APS_UNIVERSAL_CREATIVE_RENDERER).toContain(APS_RENDERER_PATH);
    expect(APS_UNIVERSAL_CREATIVE_RENDERER).toContain(APS_RENDERER_SANDBOX);
    expect(APS_UNIVERSAL_CREATIVE_RENDERER).not.toContain('allow-same-origin');
    expect(APS_UNIVERSAL_CREATIVE_RENDERER).not.toContain('srcdoc');
    expect(APS_UNIVERSAL_CREATIVE_RENDERER).not.toContain('document.write');
    expect(APS_UNIVERSAL_CREATIVE_RENDERER).not.toContain('creativeUrl');
    expect(APS_UNIVERSAL_CREATIVE_RENDERER).not.toContain('aaxResponse');
    expect(APS_UNIVERSAL_CREATIVE_RENDERER).not.toContain('example-account-id');
  });

  it('computes an absolute renderer URL from the publisher origin', () => {
    expect(apsRendererUrl()).toBe(new URL(APS_RENDERER_PATH, window.location.origin).href);
    expect(apsRendererUrl('http://publisher.example')).toBe(
      `http://publisher.example${APS_RENDERER_PATH}`
    );
    expect(apsRendererUrl('not an origin')).toBeUndefined();
  });

  it('creates the opaque route frame and resolves only after the bound acknowledgement', async () => {
    const dynamicWindow = window as unknown as {
      render?: (data: Record<string, unknown>, helper: unknown, target: Window) => Promise<void>;
    };
    window.eval(APS_UNIVERSAL_CREATIVE_RENDERER);

    try {
      const renderer = descriptor();
      const rendered = dynamicWindow.render!(
        {
          apsRenderer: renderer,
          rendererUrl: apsRendererUrl(),
        },
        undefined,
        window
      );
      const iframe = document.body.querySelector('iframe')!;
      expect(iframe.src).toMatch(/\/integrations\/aps\/renderer#tsaps=[A-Za-z0-9_-]{22}$/);
      expect(iframe.getAttribute('sandbox')).toBe(APS_RENDERER_SANDBOX);
      expect(iframe.getAttribute('sandbox')).not.toContain('allow-same-origin');

      const postMessage = vi.spyOn(iframe.contentWindow!, 'postMessage');
      iframe.dispatchEvent(new Event('load'));
      const sent = postMessage.mock.calls[0][0] as { nonce: string; renderer: ApsRendererV1 };
      expect(sent.renderer).toEqual(renderer);

      let settled = false;
      void rendered.then(() => {
        settled = true;
      });
      await Promise.resolve();
      expect(settled).toBe(false);

      window.dispatchEvent(
        new MessageEvent('message', {
          data: { message: 'trusted-server/aps/renderer-ready', nonce: sent.nonce },
          source: iframe.contentWindow,
        })
      );
      await expect(rendered).resolves.toBeUndefined();
    } finally {
      delete dynamicWindow.render;
      document.body.innerHTML = '';
    }
  });
});
