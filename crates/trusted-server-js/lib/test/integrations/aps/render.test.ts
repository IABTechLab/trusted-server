import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import envelope from '../../fixtures/aps-renderer-v1.json';
import type { ApsRendererV1 } from '../../../src/core/types';
import {
  APS_RENDERER_PATH,
  APS_RENDERER_SANDBOX,
  APS_UNIVERSAL_CREATIVE_RENDERER,
  APS_UNIVERSAL_CREATIVE_RENDERER_VERSION,
  apsRendererUrl,
  parseApsRendererDescriptor,
  renderApsCreative,
  validateApsRenderer,
} from '../../../src/integrations/aps/render';

function encodeEnvelope(value: unknown): string {
  const bytes = new TextEncoder().encode(JSON.stringify(value));
  let binary = '';
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
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

  it.each(['not-base64', 'e30', '====', btoa(String.fromCharCode(0xc3, 0x28)), btoa('{not json}')])(
    'rejects invalid base64, UTF-8, or JSON',
    (aaxResponse) => {
      expect(validateApsRenderer(descriptor({ aaxResponse }))).toBeUndefined();
    }
  );

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
});

describe('direct APS rendering', () => {
  beforeEach(() => {
    document.body.innerHTML = '<div id="fictional-slot"><span>existing</span></div>';
  });

  afterEach(() => {
    vi.restoreAllMocks();
    document.body.innerHTML = '';
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
